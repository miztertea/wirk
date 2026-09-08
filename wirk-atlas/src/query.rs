use crate::admission::{AdmissionSummary, QueryScope, admit};
use crate::domain::actual_line_bounds;
use crate::retrieval::{RankingMode, SemanticApplication, SemanticPlan, SemanticQueryConfig};
use crate::semantic::QueryProducerPin;
use crate::{
    AtlasError, AtlasStore, ContentFamily, CoverageDisposition, EditionId, ExactCoordinate,
    GenerationId, MembershipId,
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
    /// Every admitted source that contributed a generation to this answer
    /// also contributed its verified semantic rows, and the ranking is
    /// the native implementation's over exactly those rows.
    Applied,
    /// Semantic ranking ran, but not over everything this scope admits.
    /// Deliberately its own state: calling it `Applied` would assert a
    /// coverage the answer does not have, and calling it `Unavailable`
    /// would deny a ranking that actually happened.
    Partial(String),
    Unavailable(String),
    Disabled,
}

impl SemanticStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Partial(_) => "partial",
            Self::Unavailable(_) => "unavailable",
            Self::Disabled => "disabled",
        }
    }

    /// The sentence a human is owed. `W4-PUBLIC-RETRIEVAL-BUILD.md` and
    /// `SEMANTIC-LIFECYCLE-LIMITS.md`: plain search must say *why*
    /// semantic use is unavailable or degraded and identify the lexical
    /// fallback, not print a bare token.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Applied | Self::Disabled => None,
            Self::Partial(reason) | Self::Unavailable(reason) => Some(reason),
        }
    }
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
    /// A continuation whose captured semantic editions can no longer be
    /// ranked through. The answer refuses explicitly rather than silently
    /// restarting at page 1 or quietly switching this page to a different
    /// corpus or ranking mode (`W4-PUBLIC-RETRIEVAL-BUILD.md`: "use
    /// retained verified state where available or explicitly refuse
    /// unrecoverable continuation").
    pub continuation_unrecoverable: bool,
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
            || self.spent
            || self.continuation_unrecoverable)
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
    /// The semantic editions this answer actually ranked through, if any.
    /// A continuation captures these exactly as it captures generations:
    /// the next page must read the same bytes, not whatever is selected
    /// by then.
    pub editions: Vec<(MembershipId, EditionId)>,
    /// How this answer was ranked. Pinned into a continuation so a later
    /// page cannot change it.
    pub mode: RankingMode,
    /// What the native implementation reported about a ranking that
    /// actually happened. `None` for a lexical answer.
    pub application: Option<SemanticApplication>,
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
    /// The configured semantic query backend. Absent means the caller did
    /// not configure one, which is a truthful `unavailable` reason and not
    /// an error: the product ships no backend, no model and no host path.
    pub semantic_query: Option<SemanticQueryConfig>,
    /// A continuation's captured semantic editions, per membership.
    pub pinned_editions: Option<BTreeMap<MembershipId, EditionId>>,
    /// A continuation's captured ranking mode. When present it is
    /// authoritative: a page that began lexical stays lexical even if an
    /// edition became available in between, and a page that began
    /// semantic is refused rather than silently downgraded.
    pub pinned_mode: Option<RankingMode>,
    /// A semantic continuation's captured query producer identity: the
    /// implementation that ranked its first page. When it no longer
    /// matches, the page is refused outright rather than re-ranked — a
    /// backend path and an argv are the *spelling* of an implementation,
    /// not its bytes, and a file edited in place at that same path is a
    /// different ranker wearing the first page's receipt.
    ///
    /// Absence is not "no check", and the *kind* of absence is not one
    /// thing: see `PinnedProducer`.
    pub pinned_producer: PinnedProducer,
}

/// What a continuation token says about the implementation that ranked
/// its first page.
///
/// Three states rather than an `Option`, because the two absences are
/// different facts and telling a caller the wrong one is exactly the
/// defect `public-retrieval-identity-verify/VERDICT.md` V2 found: a token
/// this build issued seconds ago was refused with "issued before the
/// query producer identity was recorded". A refusal may be right while
/// the cause it states is false, and a product whose doctrine is "Known
/// is a trail" does not get to call its own fresh token legacy history.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PinnedProducer {
    /// No producer fields at all. On a fresh request that is simply the
    /// normal state; on a semantic continuation it is genuine
    /// pre-correction history, whose producer is unknown and unverifiable.
    #[default]
    Unrecorded,
    /// Producer fields are present but do not form a pin — a digest or the
    /// basis is missing. That is a malformed token, not history, and it
    /// says so.
    Incomplete,
    Recorded(QueryProducerPin),
}

struct Candidate {
    coordinate: ExactCoordinate,
    terms: BTreeMap<String, usize>,
    length: usize,
    snippet: String,
    generation_identity: HitGenerationIdentity,
}

/// The sentence a caller gets when semantic ranking did not happen, or
/// did not happen everywhere.
///
/// Every branch names a condition this estate is actually in and says
/// what the returned hits therefore are. It never asserts a state that
/// was not evaluated, and under a denial it says nothing about editions
/// at all.
fn fallback_reason(detail: &str) -> String {
    format!("semantic ranking was requested but did not run, so these hits are lexical: {detail}")
}

/// The sentence a caller gets when a *semantic continuation* cannot be
/// reproduced.
///
/// Deliberately not `fallback_reason`: an unrecoverable continuation
/// returns no hit at all, so telling the reader that "these hits are
/// lexical" describes hits that do not exist
/// (`public-retrieval-verify/VERDICT.md` D5). The plain surface's
/// adjacent line and `coverage.continuation_unrecoverable` already say a
/// page was refused; this says why, in the same words on both surfaces.
fn unrecoverable_reason(detail: &str) -> String {
    format!(
        "this continuation's own ranking cannot be reproduced, so no page was returned and \
         nothing was restarted: {detail}"
    )
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
        return Ok(empty_answer(
            store,
            request,
            AdmissionSummary::default(),
            AnswerCoverage {
                no_sources: true,
                ..AnswerCoverage::default()
            },
            match request.semantic {
                SemanticRequest::Disabled => SemanticStatus::Disabled,
                // Scoped to the request, not to the estate
                // (`W4-LIFECYCLE-CORRECTION.md` item 2): this branch is
                // reached both by a fresh estate and by a `--source` no
                // membership answers to, and asserting the second case is
                // the first would be a plain falsehood on a surface whose
                // whole value is that it says nothing untrue.
                SemanticRequest::Requested => SemanticStatus::Unavailable(fallback_reason(
                    "no registered source matched this request, so no semantic edition can be \
                     selected for one",
                )),
            },
        ));
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
        return Ok(empty_answer(
            store,
            request,
            admission,
            AnswerCoverage {
                denied: true,
                ..AnswerCoverage::default()
            },
            match request.semantic {
                SemanticRequest::Disabled => SemanticStatus::Disabled,
                // Deliberately says nothing about which editions exist,
                // which backend is configured, or what is on disk: this
                // scope admitted nothing, so any of that would leak past
                // the denial.
                SemanticRequest::Requested => SemanticStatus::Unavailable(fallback_reason(
                    "this scope admitted no source to search",
                )),
            },
        ));
    }
    let publication_revision = store.publication_revision();
    let mut coverage = AnswerCoverage::default();

    // ---- the immutable generation vector this answer reads ---------------
    //
    // Resolved before anything is read, and before the ranking mode is
    // decided, because both the lexical and the semantic path are defined
    // over exactly this vector and nothing else.
    let mut resolved: Vec<(crate::AdmittedSource, crate::SourceGeneration)> = Vec::new();
    let mut generations: Vec<(MembershipId, GenerationId)> = Vec::new();
    for source in &admitted {
        let generation = match &request.pinned {
            Some(pinned) => match pinned.get(&source.membership.id) {
                Some(generation_id) => match store.generation(generation_id) {
                    Ok(generation) => {
                        // Ruling 0095 / W3-SECOND-CORRECTION.md item 1:
                        // `store.generation` is a *global* lookup by id —
                        // it says nothing about which source the manifest
                        // belongs to. Bind the pinned generation to the
                        // membership's own source before any manifest,
                        // blob or snippet is touched.
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
        generations.push((source.membership.id.clone(), generation.id.clone()));
        resolved.push((source.clone(), generation));
    }
    generations.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    let saw_any_generation = !resolved.is_empty();

    // ---- ranking mode ----------------------------------------------------
    let mut editions_used: Vec<(MembershipId, EditionId)> = Vec::new();
    let mut application: Option<SemanticApplication> = None;
    let mut semantic = SemanticStatus::Disabled;
    let mut mode = RankingMode::Lexical;
    let mut hits: Vec<EvidenceHit> = Vec::new();
    let mut total_candidates = 0usize;
    let mut ranked_lexically = true;

    if request.semantic == SemanticRequest::Requested
        && request.pinned_mode != Some(RankingMode::Lexical)
    {
        let generation_of: BTreeMap<MembershipId, GenerationId> = resolved
            .iter()
            .map(|(source, generation)| (source.membership.id.clone(), generation.id.clone()))
            .collect();
        let memberships: Vec<crate::Membership> = resolved
            .iter()
            .map(|(source, _)| source.membership.clone())
            .collect();
        let outcome =
            semantic_attempt(store, request, &memberships, &generation_of, &mut coverage)?;
        match outcome {
            Ok((ranked, applied, status)) => {
                hits = ranked;
                total_candidates = hits.len();
                editions_used = applied.0;
                application = Some(applied.1);
                semantic = status;
                mode = RankingMode::Semantic;
                ranked_lexically = false;
            }
            Err(detail) => {
                let unrecoverable = request.pinned_mode == Some(RankingMode::Semantic);
                semantic = SemanticStatus::Unavailable(if unrecoverable {
                    unrecoverable_reason(&detail)
                } else {
                    fallback_reason(&detail)
                });
                if unrecoverable {
                    // The continuation's own ranking cannot be
                    // reproduced. Falling back to lexical here would hand
                    // the caller a different corpus under the first
                    // page's receipt; restarting would hide it entirely.
                    coverage.continuation_unrecoverable = true;
                    return Ok(SearchAnswer {
                        publication_revision,
                        generations,
                        editions: Vec::new(),
                        mode: RankingMode::Semantic,
                        application: None,
                        admission,
                        hits: Vec::new(),
                        semantic,
                        coverage,
                        truncated: false,
                        budget: AnswerBudget {
                            limit: request.limit,
                            offset: request.offset,
                            ..AnswerBudget::default()
                        },
                    });
                }
            }
        }
    } else if request.semantic == SemanticRequest::Requested {
        semantic = SemanticStatus::Unavailable(fallback_reason(
            "this continuation's first page was ranked lexically, and a continuation keeps the \
             ranking mode it was issued under even when a semantic edition has become available \
             since",
        ));
    }

    if ranked_lexically {
        let (lexical, candidates_total) = lexical_hits(store, request, &resolved, &mut coverage)?;
        hits = lexical;
        total_candidates = candidates_total;
        if !request.families.is_empty() && total_candidates == 0 && saw_any_generation {
            coverage.unsupported_family = true;
        }
    }

    // ---- one deterministic ranked list, paged ----------------------------
    let page: Vec<EvidenceHit> = hits
        .into_iter()
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
    // 0095).
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
    coverage.partial = coverage.partial
        || truncated
        || coverage.source_unavailable
        || coverage.generation_unavailable;

    Ok(SearchAnswer {
        publication_revision,
        generations,
        editions: editions_used,
        mode,
        application,
        admission,
        hits,
        semantic,
        coverage,
        truncated,
        budget,
    })
}

fn empty_answer(
    store: &AtlasStore,
    request: &SearchRequest,
    admission: AdmissionSummary,
    coverage: AnswerCoverage,
    semantic: SemanticStatus,
) -> SearchAnswer {
    SearchAnswer {
        publication_revision: store.publication_revision(),
        generations: Vec::new(),
        editions: Vec::new(),
        mode: RankingMode::Lexical,
        application: None,
        admission,
        hits: Vec::new(),
        semantic,
        coverage,
        truncated: false,
        budget: AnswerBudget {
            limit: request.limit,
            offset: request.offset,
            ..AnswerBudget::default()
        },
    }
}

/// The `Err` side is the raw *detail*, not a finished `SemanticStatus`:
/// the same failure is told two different ways depending on what the
/// answer then does with it — a fresh request falls back to lexical hits
/// and says so, an unrecoverable continuation returns no hit at all and
/// must not claim any (`unrecoverable_reason`, VERDICT.md D5).
type SemanticOutcome = Result<
    (
        Vec<EvidenceHit>,
        (Vec<(MembershipId, EditionId)>, SemanticApplication),
        SemanticStatus,
    ),
    String,
>;

/// Try to rank this answer semantically, and say truthfully what happened.
///
/// The whole admission decision has already been made by the caller: this
/// sees only the memberships that survived it and only the immutable
/// generation each is pinned to. What it adds is the second admission the
/// retrieval side owns — which of those sources has a verified, current,
/// compatible edition — and it applies the family filter to *rows* before
/// the view exists, so an excluded row never reaches a corpus statistic.
fn semantic_attempt(
    store: &AtlasStore,
    request: &SearchRequest,
    memberships: &[crate::Membership],
    generations: &BTreeMap<MembershipId, GenerationId>,
    coverage: &mut AnswerCoverage,
) -> Result<SemanticOutcome, AtlasError> {
    let Some(config) = &request.semantic_query else {
        return Ok(Err(
            "no semantic query backend is configured for this request; a backend \
             executable and an offline model directory are the caller's explicit configuration, \
             and this request named neither. That is a fact about this request only: it says \
             nothing about whether this estate holds semantic editions, and nothing about what \
             this product can run"
                .to_owned(),
        ));
    };
    // A semantic continuation whose token predates the query producer
    // identity carries no pin. That is honest history, not a positive
    // statement that nothing moved, and it is the one thing this check
    // cannot verify — so the page is refused with that as the reason
    // rather than served on an assumption (`W4-PRODUCER-PROVENANCE-
    // CORRECTION.md`: historical unmeasured identities stay unknown).
    let pinned_producer = if request.pinned_mode == Some(RankingMode::Semantic)
        && request.pinned_editions.is_some()
    {
        match &request.pinned_producer {
            PinnedProducer::Unrecorded => {
                return Ok(Err(
                    "this continuation was issued before the query producer identity was \
                     recorded on an answer, so there is nothing to check the implementation that \
                     would rank this page against; re-run the query to start a continuation that \
                     carries one"
                        .to_owned(),
                ));
            }
            PinnedProducer::Incomplete => {
                return Ok(Err(
                    "this continuation carries an incomplete query producer pin: some of its \
                     producer fields are present and some are missing, so it is a malformed \
                     token rather than a record of an implementation, and there is nothing \
                     complete to check this ranking against; re-run the query"
                        .to_owned(),
                ));
            }
            PinnedProducer::Recorded(pin) => Some(pin),
        }
    } else {
        None
    };
    let plan = crate::retrieval::plan_semantic(
        store,
        memberships,
        generations,
        request.pinned_editions.as_ref(),
        &request.families,
    )?;
    let (editions, partial) = match plan {
        SemanticPlan::Unavailable(detail) => return Ok(Err(detail)),
        SemanticPlan::Ready { editions, partial } => (editions, partial),
    };
    // A continuation may not quietly widen or narrow its own corpus: the
    // editions it captured are the editions it ranks, exactly.
    if let Some(pinned) = &request.pinned_editions {
        let ranked: std::collections::BTreeSet<&MembershipId> =
            editions.iter().map(|edition| &edition.membership).collect();
        if pinned.len() != ranked.len() || !pinned.keys().all(|key| ranked.contains(key)) {
            return Ok(Err(format!(
                "this continuation was issued over {} semantic edition(s) and only {} can be \
                 ranked through now; the page it asks for cannot be reproduced{}",
                pinned.len(),
                ranked.len(),
                partial
                    .as_ref()
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default()
            )));
        }
    }
    let used: Vec<(MembershipId, EditionId)> = editions
        .iter()
        .map(|edition| (edition.membership.clone(), edition.edition.id.clone()))
        .collect();
    let (view, ranked, applied) =
        match crate::retrieval::rank(config, &editions, store, &request.query, pinned_producer)? {
            Ok(result) => result,
            Err(detail) => return Ok(Err(detail)),
        };
    let identities: BTreeMap<MembershipId, HitGenerationIdentity> = editions
        .iter()
        .map(|edition| {
            (
                edition.membership.clone(),
                HitGenerationIdentity {
                    revision: edition.edition.generation_revision.clone(),
                    content: edition.edition.generation_content.clone(),
                    extractor_set: edition.edition.chunker.extractor_set.clone(),
                },
            )
        })
        .collect();
    let mut hits = Vec::with_capacity(ranked.len());
    for result in &ranked {
        let row = &view[result.row];
        let Some(identity) = identities.get(&row.membership) else {
            return Ok(Err(
                "a ranked row named a membership this answer did not admit".to_owned(),
            ));
        };
        hits.push(EvidenceHit {
            coordinate: ExactCoordinate {
                estate: row.estate.clone(),
                membership: row.membership.clone(),
                source: row.source.clone(),
                generation: row.generation.clone(),
                path: row.path.clone(),
                object_id: row.object_id.clone(),
                byte_start: row.byte_start,
                byte_end: row.byte_end,
                line_start: row.line_start,
                line_end: row.line_end,
            },
            score: result.score,
            // The *evidence* bytes, not the ranking text: what a reader is
            // shown is what the repository holds at this coordinate.
            snippet: String::from_utf8_lossy(&row.bytes).into_owned(),
            generation_identity: identity.clone(),
        });
    }
    // The candidate pool is frozen so paging is a slice of one list. When
    // the native ranker fills it, more candidates may exist beyond it and
    // the answer says so rather than implying completeness.
    if applied.saturated {
        coverage.partial = true;
    }
    let status = match &partial {
        None => SemanticStatus::Applied,
        Some(detail) => SemanticStatus::Partial(format!(
            "semantic ranking ran over {} of {} admitted source(s); the rest were not searched \
             semantically and contribute no hit to this answer: {detail}",
            editions.len(),
            memberships.len(),
            detail = detail
        )),
    };
    Ok(Ok((hits, (used, applied), status)))
}

/// The lexical path, unchanged in behaviour: a deterministic small BM25
/// over the admitted, family-filtered generation units.
fn lexical_hits(
    store: &AtlasStore,
    request: &SearchRequest,
    resolved: &[(crate::AdmittedSource, crate::SourceGeneration)],
    coverage: &mut AnswerCoverage,
) -> Result<(Vec<EvidenceHit>, usize), AtlasError> {
    let mut blob_cache: BTreeMap<(String, String), Vec<u8>> = BTreeMap::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let _ = store;
    for (source, generation) in resolved {
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
    let ranked = score(&request.query, candidates);
    let total = ranked.len();
    Ok((ranked, total))
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
