use crate::admission::{AdmissionSummary, QueryScope, admit};
use crate::domain::actual_line_bounds;
use crate::{
    AtlasError, AtlasStore, ContentFamily, CoverageDisposition, ExactCoordinate, GenerationId,
    MembershipId,
};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticRequest {
    Requested,
    Disabled,
}

/// Required so an empty semantic result list is never mistaken for "no
/// backend exists": `Requested` without an applied backend must say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticStatus {
    Applied,
    Unavailable(String),
    Disabled,
}

/// Independent dimensions rather than one mutually-exclusive label: more
/// than one can be true of the same answer (e.g. truncated *and* missing a
/// generation), and collapsing them would hide which one actually applies.
///
/// `denied` and `no_sources` (ruling 0093, W3-CORRECTION.md item 3) are
/// deliberately distinct from `no_match`: a request the caller's own scope
/// refused, or an estate with no registered membership at all, has not
/// been searched — reporting either as `no_match` would positively assert
/// an absence that was never actually checked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnswerCoverage {
    pub no_match: bool,
    pub partial: bool,
    pub source_unavailable: bool,
    pub generation_unavailable: bool,
    pub unsupported_family: bool,
    /// The caller's scope admitted none of the sources it named (or the
    /// whole scope), so nothing was searched — distinct from a genuine
    /// zero-hit search of admitted content.
    pub denied: bool,
    /// The addressed estate (or the one named `requested_source`) has no
    /// registered Atlas membership at all — a fresh estate, not a denial.
    pub no_sources: bool,
    /// A continuation whose window starts at or past the end of its own
    /// ranked candidate list: every candidate this answer's pinned
    /// generations hold has already been handed to the caller (ruling
    /// 0095; W3-SECOND-CORRECTION.md item 2). Truthful end-of-results —
    /// deliberately *not* `no_match`, which would assert the corpus held
    /// nothing while `budget.total_candidates` in the very same answer
    /// says how many it held.
    pub spent: bool,
}
impl AnswerCoverage {
    pub fn is_complete(&self) -> bool {
        !(self.no_match
            || self.partial
            || self.source_unavailable
            || self.generation_unavailable
            || self.unsupported_family
            || self.denied
            || self.no_sources
            || self.spent)
    }
}

/// The exact identity of the generation an `EvidenceHit` was drawn from —
/// BUILD-BRIEF.md's decisive assertion ("every hit names estate, source,
/// generation, revision/content/extractor identities"; W3-CORRECTION.md
/// item 4). Carried on the hit itself so a caller never has to join
/// against a separate `status` call to learn what generation an answer
/// actually rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitGenerationIdentity {
    pub revision: String,
    pub content: String,
    pub extractor_set: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceHit {
    pub coordinate: ExactCoordinate,
    pub score: f64,
    pub snippet: String,
    pub generation_identity: HitGenerationIdentity,
}

/// The evidence budget an answer actually used (W3-CORRECTION.md item 4):
/// how many admitted, family-filtered candidates scored above zero in
/// total, versus how many this answer actually returned starting at
/// `offset` — distinct from the bare `truncated` boolean, which says only
/// that more exists, not how much.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnswerBudget {
    pub limit: usize,
    pub offset: usize,
    pub total_candidates: usize,
    pub returned: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchAnswer {
    pub publication_revision: u64,
    pub generations: Vec<(MembershipId, GenerationId)>,
    pub admission: AdmissionSummary,
    pub hits: Vec<EvidenceHit>,
    pub semantic: SemanticStatus,
    pub coverage: AnswerCoverage,
    pub truncated: bool,
    pub budget: AnswerBudget,
}

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub scope: QueryScope,
    pub requested_source: Option<String>,
    pub query: String,
    pub families: Vec<ContentFamily>,
    pub semantic: SemanticRequest,
    pub limit: usize,
    /// Continuation (ruling 0093, W3-CORRECTION.md item 1): when `Some`,
    /// pins the exact generation an admitted membership is read from —
    /// the vector a prior answer already captured — instead of that
    /// membership's *current* published generation. A membership this map
    /// does not name (the continuation was captured before the caller's
    /// scope ever admitted it) is treated as `generation_unavailable`,
    /// never silently upgraded to "current". Absent for a fresh request.
    pub pinned: Option<BTreeMap<MembershipId, GenerationId>>,
    /// How many already-ranked candidates to skip before taking `limit` —
    /// continuation's own page cursor. Zero for a fresh request.
    pub offset: usize,
}

struct Candidate {
    coordinate: ExactCoordinate,
    terms: BTreeMap<String, usize>,
    length: usize,
    snippet: String,
    generation_identity: HitGenerationIdentity,
}

/// What `--semantic requested` can honestly be told, given what this
/// estate has actually built and selected.
///
/// It is never `Applied` in this increment, and that is a contract, not
/// an omission: W4 A builds and selects semantic editions; W4 B is the
/// increment that ranks through them. Reporting `Applied` because a
/// verified edition exists would assert that a search consulted vectors
/// it did not consult — the exact "asserting search happened" failure
/// `W4-PUBLIC-LIFECYCLE-BUILD.md` forbids. The reason text distinguishes
/// the two genuinely different states so a caller can tell "nothing is
/// built" from "something is built and this answer still did not use it".
fn semantic_status(
    store: &AtlasStore,
    request: &SearchRequest,
    admitted: &[crate::AdmittedSource],
) -> SemanticStatus {
    if request.semantic == SemanticRequest::Disabled {
        return SemanticStatus::Disabled;
    }
    // The same derived answer `atlas status` gives, over the same
    // selected editions (`W4-LIFECYCLE-CORRECTION.md` item 1: the human
    // status, the JSON status and this fallback must agree). A selection
    // that is superseded, corrupt or unreadable is counted as what it
    // is, never as a usable edition. Only admitted sources are consulted,
    // so nothing here reaches past a denial.
    let mut usable = 0usize;
    let mut unusable = 0usize;
    // The reason classes actually present, kept apart rather than summed.
    // `W4-PRODUCER-PROVENANCE-CORRECTION.md` item 4: a selection whose own
    // record has gone is not a selection whose source generation moved on,
    // and this sentence used to assert the second for both.
    let mut superseded = 0usize;
    let mut unverified = 0usize;
    let mut unreadable = 0usize;
    for source in admitted {
        match store.semantic_availability(&source.membership) {
            Ok(crate::SemanticAvailability::None) => {}
            Ok(availability) if availability.is_available() => usable += 1,
            // A selection this product cannot presently evaluate is
            // reported with the ones it evaluated and rejected, never
            // silently as usable.
            Ok(other) => {
                unusable += 1;
                match other {
                    crate::SemanticAvailability::Superseded(_) => superseded += 1,
                    crate::SemanticAvailability::Unusable(_) => unverified += 1,
                    _ => unreadable += 1,
                }
            }
            Err(_) => {
                unusable += 1;
                unreadable += 1;
            }
        }
    }
    // Only the classes that actually occurred, so the sentence never
    // names a condition this estate is not in.
    let mut classes: Vec<String> = Vec::new();
    if superseded > 0 {
        classes.push(format!(
            "{superseded} built over a source generation this source no longer publishes"
        ));
    }
    if unverified > 0 {
        classes.push(format!("{unverified} whose bytes no longer verify"));
    }
    if unreadable > 0 {
        classes.push(format!(
            "{unreadable} whose own edition record is absent or unreadable"
        ));
    }
    let classes = classes.join(", ");
    let total = admitted.len();
    if usable == 0 && unusable == 0 {
        return SemanticStatus::Unavailable(
            "no admitted source has a selected semantic edition; \
             build and select one explicitly before requesting semantic retrieval"
                .into(),
        );
    }
    if usable == 0 {
        return SemanticStatus::Unavailable(format!(
            "no admitted source has a usable selected semantic edition: {unusable} of {total} \
             have a selection that is not currently usable — {classes}. `atlas status` names the \
             condition per source."
        ));
    }
    let mut reason = format!(
        "{usable} of {total} admitted sources have a usable selected semantic edition, but \
         semantic retrieval is not implemented in this increment: these hits are lexical"
    );
    if unusable > 0 {
        reason.push_str(&format!(
            "; a further {unusable} of {total} have a selection that is not currently usable — \
             {classes}; see `atlas status`"
        ));
    }
    SemanticStatus::Unavailable(reason)
}

/// Coherence does not come from the `&AtlasStore` borrow (source-verify-w2
/// `probe_b1`: a *second*, independent `AtlasStore` handle on the same
/// estate root is outside this borrow entirely, and can `acquire`/`publish`
/// while this call runs). It comes from each `AtlasStore` instance holding
/// its own private in-memory `catalog` snapshot — only `&mut self` calls on
/// *this same instance* ever change it — together with generation
/// manifests being immutable and content-addressed once staged. Every
/// `store.current(...)` call below reads that one already-resident
/// snapshot, so the vector this function captures is coherent for its
/// whole duration regardless of what other handles do concurrently.
pub fn search(store: &AtlasStore, request: &SearchRequest) -> Result<SearchAnswer, AtlasError> {
    // W3-CORRECTION.md item 3: a fresh estate (or a specifically
    // requested-but-unregistered source) has never had anything to admit
    // or deny — reported distinctly from both `denied` and `no_match`.
    let registered_total = match &request.requested_source {
        Some(wanted) => store
            .memberships()
            .filter(|membership| &membership.alias == wanted)
            .count(),
        None => store.memberships().count(),
    };
    if registered_total == 0 {
        return Ok(SearchAnswer {
            publication_revision: store.publication_revision(),
            generations: Vec::new(),
            admission: AdmissionSummary::default(),
            hits: Vec::new(),
            semantic: match request.semantic {
                SemanticRequest::Disabled => SemanticStatus::Disabled,
                // Scoped to the request, not to the estate
                // (`W4-LIFECYCLE-CORRECTION.md` item 2): this branch is
                // reached both by a fresh estate and by a `--source` no
                // membership answers to, and asserting the second case is
                // the first would be a plain falsehood on a surface whose
                // whole value is that it says nothing untrue. It still
                // discloses no more than the old sentence did.
                SemanticRequest::Requested => SemanticStatus::Unavailable(
                    "no registered source matched this request, so no semantic edition can be \
                     selected for one"
                        .into(),
                ),
            },
            coverage: AnswerCoverage {
                no_sources: true,
                ..AnswerCoverage::default()
            },
            truncated: false,
            budget: AnswerBudget {
                limit: request.limit,
                offset: request.offset,
                ..AnswerBudget::default()
            },
        });
    }

    let (admitted, admission) = admit(
        store.memberships(),
        &request.scope,
        request.requested_source.as_deref(),
    );
    // W3-CORRECTION.md item 3: the scope admitted nothing to search at
    // all — a denial (or a requested source this scope was never granted),
    // never a searched-and-empty `no_match`.
    if admitted.is_empty() {
        return Ok(SearchAnswer {
            publication_revision: store.publication_revision(),
            generations: Vec::new(),
            admission,
            hits: Vec::new(),
            semantic: match request.semantic {
                SemanticRequest::Disabled => SemanticStatus::Disabled,
                // Deliberately says nothing about which editions exist:
                // this scope admitted nothing, so disclosing the estate's
                // semantic state here would leak past the denial.
                SemanticRequest::Requested => {
                    SemanticStatus::Unavailable("this scope admitted no source to search".into())
                }
            },
            coverage: AnswerCoverage {
                denied: true,
                ..AnswerCoverage::default()
            },
            truncated: false,
            budget: AnswerBudget {
                limit: request.limit,
                offset: request.offset,
                ..AnswerBudget::default()
            },
        });
    }
    let publication_revision = store.publication_revision();
    let mut generations = Vec::new();
    let mut coverage = AnswerCoverage::default();
    let mut blob_cache: BTreeMap<(String, String), Vec<u8>> = BTreeMap::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut saw_any_generation = false;

    for source in &admitted {
        let generation = match &request.pinned {
            Some(pinned) => match pinned.get(&source.membership.id) {
                Some(generation_id) => match store.generation(generation_id) {
                    Ok(generation) => {
                        // Ruling 0095 / W3-SECOND-CORRECTION.md item 1:
                        // `store.generation` is a *global* lookup by id —
                        // it says nothing about which source the manifest
                        // belongs to. Without this check a pin can name
                        // any generation in the estate, including one
                        // acquired under an alias this scope denies, and
                        // the loop below would then read that generation's
                        // blobs through the *admitted* membership's
                        // locator: two aliases over one repository is
                        // exactly the case where that succeeds. Bind the
                        // pinned generation to the membership's own source
                        // before any manifest, blob or snippet is touched.
                        if generation.source != source.membership.source {
                            return Err(AtlasError::InvalidCoordinate(
                                "pinned generation does not belong to this membership's source"
                                    .into(),
                            ));
                        }
                        generation
                    }
                    Err(_) => {
                        coverage.generation_unavailable = true;
                        continue;
                    }
                },
                None => {
                    coverage.generation_unavailable = true;
                    continue;
                }
            },
            None => {
                let Some(generation) = store.current(&source.membership)? else {
                    coverage.generation_unavailable = true;
                    continue;
                };
                generation
            }
        };
        saw_any_generation = true;
        generations.push((source.membership.id.clone(), generation.id.clone()));
        let generation_identity = HitGenerationIdentity {
            revision: generation.revision.clone(),
            content: generation.content.clone(),
            extractor_set: generation.extractor_set.clone(),
        };
        for resource in &generation.resources {
            if resource.disposition != CoverageDisposition::Indexed {
                continue;
            }
            let Some(family) = resource.units.first().map(|unit| unit.family) else {
                continue;
            };
            if !request.families.is_empty() && !request.families.contains(&family) {
                continue;
            }
            let object_id = resource.object_id.clone().unwrap_or_default();
            let key = (generation.id.0.clone(), object_id.clone());
            let bytes = if let Some(bytes) = blob_cache.get(&key) {
                bytes.clone()
            } else {
                match crate::git::blob(Path::new(&source.membership.locator), &object_id) {
                    Ok(bytes) => {
                        blob_cache.insert(key, bytes.clone());
                        bytes
                    }
                    Err(AtlasError::GitUnavailable(_)) => {
                        coverage.source_unavailable = true;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
            for unit in &resource.units {
                let start = unit.byte_start as usize;
                let end = unit.byte_end as usize;
                if end > bytes.len() || start > end {
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&bytes[start..end]) else {
                    continue;
                };
                let mut terms = BTreeMap::new();
                let mut length = 0usize;
                for token in tokenize(text) {
                    *terms.entry(token).or_insert(0) += 1;
                    length += 1;
                }
                candidates.push(Candidate {
                    coordinate: ExactCoordinate {
                        estate: source.membership.estate.clone(),
                        membership: source.membership.id.clone(),
                        source: source.membership.source.clone(),
                        generation: generation.id.clone(),
                        path: resource.path.clone(),
                        object_id: object_id.clone(),
                        byte_start: unit.byte_start,
                        byte_end: unit.byte_end,
                        line_start: unit.line_start,
                        line_end: unit.line_end,
                    },
                    terms,
                    length,
                    snippet: text.to_owned(),
                    generation_identity: generation_identity.clone(),
                });
            }
        }
    }
    generations.sort_by(|a, b| a.0.0.cmp(&b.0.0));

    if !request.families.is_empty() && candidates.is_empty() && saw_any_generation {
        coverage.unsupported_family = true;
    }

    let mut ranked = score(&request.query, candidates);
    let total_candidates = ranked.len();
    // Continuation's own page cursor (W3-CORRECTION.md item 1): skip
    // already-returned candidates before taking this page's `limit`, over
    // the same deterministic ranked order a fresh, unpaged request would
    // see (the ranking itself never depends on `offset`).
    let page: Vec<EvidenceHit> = ranked
        .drain(..)
        .skip(request.offset)
        .take(request.limit)
        .collect();
    let truncated = request.offset + page.len() < total_candidates;
    let budget = AnswerBudget {
        limit: request.limit,
        offset: request.offset,
        total_candidates,
        returned: page.len(),
    };
    let hits = page;
    // A continuation window that starts at or past the end of its own
    // ranked list has run out of *page*, not out of *corpus* (ruling
    // 0095). `offset > 0` keeps a genuine fresh zero-hit query
    // (`offset 0`, `total_candidates 0`) reporting `no_match` as before.
    coverage.spent = request.offset > 0 && request.offset >= total_candidates;
    // `no_match` must mean the admitted, family-filtered corpus was fully
    // searched and genuinely produced nothing (source-verify-w2 Failure 1):
    // an empty presentation window (`truncated`/`spent`) or an unread
    // source (`source_unavailable`/`generation_unavailable`) is missing
    // evidence, not proven absence, and must never present as one.
    if hits.is_empty()
        && !coverage.unsupported_family
        && !truncated
        && !coverage.spent
        && !coverage.source_unavailable
        && !coverage.generation_unavailable
    {
        coverage.no_match = true;
    }
    coverage.partial = truncated || coverage.source_unavailable || coverage.generation_unavailable;

    let semantic = semantic_status(store, request, &admitted);

    Ok(SearchAnswer {
        publication_revision,
        generations,
        admission,
        hits,
        semantic,
        coverage,
        truncated,
        budget,
    })
}

/// Deterministic small BM25 over the admitted, family-filtered candidate
/// units only: inadmissible sources never entered `candidates`, so they
/// cannot outrank a permitted match here regardless of their content.
fn score(query: &str, candidates: Vec<Candidate>) -> Vec<EvidenceHit> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let query_terms = dedupe(tokenize(query));
    let n = candidates.len() as f64;
    let avgdl = if candidates.is_empty() {
        0.0
    } else {
        candidates.iter().map(|c| c.length as f64).sum::<f64>() / n
    };
    let mut df: BTreeMap<&str, f64> = BTreeMap::new();
    for term in &query_terms {
        let count = candidates
            .iter()
            .filter(|candidate| candidate.terms.contains_key(term))
            .count();
        df.insert(term, count as f64);
    }
    let mut hits: Vec<EvidenceHit> = candidates
        .into_iter()
        .map(|candidate| {
            let mut total = 0.0;
            for term in &query_terms {
                let tf = *candidate.terms.get(term).unwrap_or(&0) as f64;
                if tf == 0.0 {
                    continue;
                }
                let df_t = *df.get(term.as_str()).unwrap_or(&0.0);
                let idf = ((n - df_t + 0.5) / (df_t + 0.5) + 1.0).ln();
                let denom = tf + K1 * (1.0 - B + B * candidate.length as f64 / avgdl.max(1.0));
                total += idf * (tf * (K1 + 1.0)) / denom;
            }
            EvidenceHit {
                coordinate: candidate.coordinate,
                score: total,
                snippet: candidate.snippet,
                generation_identity: candidate.generation_identity,
            }
        })
        .filter(|hit| hit.score > 0.0)
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.coordinate.membership.0.cmp(&b.coordinate.membership.0))
            .then_with(|| a.coordinate.path.cmp(&b.coordinate.path))
            .then_with(|| a.coordinate.byte_start.cmp(&b.coordinate.byte_start))
    });
    hits
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

fn dedupe(tokens: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    tokens
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathLookupOutcome {
    Denied,
    GenerationUnavailable,
    Absent,
    Excluded(String),
    Unsupported(String),
    Unavailable(String),
    Resolved {
        coordinate: ExactCoordinate,
        bytes: Vec<u8>,
        total_bytes: u64,
        truncated: bool,
    },
}

#[derive(Debug, Clone)]
pub struct PathLookupRequest {
    pub scope: QueryScope,
    pub source: String,
    pub path: Vec<u8>,
    pub budget_bytes: u64,
}

/// Exact path lookup independent of the extractor's unit chunking: it
/// reads the whole admitted blob once (bounded by `budget_bytes`) rather
/// than stitching together `TextUnit`s.
pub fn resolve_path(
    store: &AtlasStore,
    request: &PathLookupRequest,
) -> Result<PathLookupOutcome, AtlasError> {
    let (admitted, _) = admit(store.memberships(), &request.scope, Some(&request.source));
    let Some(source) = admitted.first() else {
        return Ok(PathLookupOutcome::Denied);
    };
    let Some(generation) = store.current(&source.membership)? else {
        return Ok(PathLookupOutcome::GenerationUnavailable);
    };
    let Some(record) = generation
        .resources
        .iter()
        .find(|resource| resource.path == request.path)
    else {
        return Ok(PathLookupOutcome::Absent);
    };
    match record.disposition {
        CoverageDisposition::Excluded => {
            return Ok(PathLookupOutcome::Excluded(detail_of(record)));
        }
        CoverageDisposition::Unsupported => {
            return Ok(PathLookupOutcome::Unsupported(detail_of(record)));
        }
        CoverageDisposition::Unavailable | CoverageDisposition::Error => {
            return Ok(PathLookupOutcome::Unavailable(detail_of(record)));
        }
        CoverageDisposition::Indexed => {}
    }
    let object_id = record.object_id.clone().unwrap_or_default();
    let bytes = match crate::git::blob(Path::new(&source.membership.locator), &object_id) {
        Ok(bytes) => bytes,
        Err(AtlasError::GitUnavailable(detail)) => {
            return Ok(PathLookupOutcome::Unavailable(detail));
        }
        Err(error) => return Err(error),
    };
    let total = bytes.len() as u64;
    let mut cap = request.budget_bytes.min(total) as usize;
    while cap > 0 && std::str::from_utf8(&bytes[..cap]).is_err() {
        cap -= 1;
    }
    let truncated = (cap as u64) < total;
    let Some((line_start, line_end)) = actual_line_bounds(&bytes, 0, cap as u64) else {
        return Err(AtlasError::InvalidCoordinate(
            "committed bytes are not valid UTF-8 at the requested budget bound".into(),
        ));
    };
    Ok(PathLookupOutcome::Resolved {
        coordinate: ExactCoordinate {
            estate: source.membership.estate.clone(),
            membership: source.membership.id.clone(),
            source: source.membership.source.clone(),
            generation: generation.id.clone(),
            path: request.path.clone(),
            object_id,
            byte_start: 0,
            byte_end: cap as u64,
            line_start,
            line_end,
        },
        bytes: bytes[..cap].to_vec(),
        total_bytes: total,
        truncated,
    })
}

fn detail_of(record: &crate::ResourceRecord) -> String {
    record
        .detail
        .clone()
        .unwrap_or_else(|| "resource unavailable".into())
}
