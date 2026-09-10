//! P3 W4 B (`W4-PUBLIC-RETRIEVAL-BUILD.md`): ranking through verified
//! semantic editions, over exactly the rows the caller's own scope admits.
//!
//! The division of labour is the same one the build side already follows,
//! and it is the whole of R5 here. **Wirk owns identity and admission**:
//! which memberships this scope may read, which immutable generation each
//! is pinned to, which edition's verified bytes describe it, which rows
//! survive the family filter, what a returned coordinate means, and what
//! a caller is told about all of it. **The installed native
//! implementation owns ranking**: its chunk type, its BM25, its dense
//! backend, its fusion, its boosts and its penalties, called across an
//! argv boundary at a configured executable. No rank function is copied,
//! retuned or patched, and no ranking heuristic is re-implemented here.
//!
//! Three things this module deliberately does *not* do.
//!
//! * It does not select on a wider index. `W4-PUBLIC-RETRIEVAL-BUILD.md`
//!   is explicit that a selector over a full-corpus BM25 index is not an
//!   admitted view: excluded rows must not reach corpus statistics. The
//!   view handed to the ranker therefore *contains* only admitted rows —
//!   document frequencies, the average document length, the fused ranks
//!   and the boosts are all computed over that set and no other.
//! * It does not persist anything. No index is built on disk, no cache is
//!   written, no vector is created. The only bytes a query writes are the
//!   admitted view's own vectors, into a private temporary directory that
//!   is removed before the answer returns; every durable byte of the
//!   estate is identical before and after.
//! * It does not embed the corpus. The only thing embedded during a query
//!   is the query string, by the model the edition already names.

use crate::domain::{EstateScope, GenerationId, MembershipId};
use crate::semantic::{
    BackendArgument, BackendEnvironment, BackendIdentity, CAPACITY_POLICY, EMBEDDING_BATCH_POLICY,
    EditionId, MappingRow, QUERY_HASH_SEED, QUERY_ORDERING_POLICY, QUERY_PRODUCER_BASIS_MISSING,
    QUERY_PROTOCOL, QueryProducerBasis, QueryProducerPin, RANKING_PATH_CONVENTION,
    ReportedEnvironment, SemanticEdition, configured_file, digest_bytes, measure_environment,
    normalize_ranking_text, producer_basis, query_producer_configuration_digest,
    query_producer_identity_digest, ranking_scope,
};
use crate::{AtlasError, AtlasStore, ContentFamily, Membership, SemanticAvailability};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// How an answer was actually ranked. Bound into a continuation, because
/// a page that changed ranking mode half way through would be a different
/// answer wearing the first one's receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankingMode {
    Lexical,
    Semantic,
}

impl RankingMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "lexical" => Some(Self::Lexical),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

/// The query-time half of the portability boundary. Exactly like
/// `SemanticBuildConfig`: the product ships no model name, no interpreter,
/// no cache directory and no host path, and records what it was handed.
#[derive(Debug, Clone)]
pub struct SemanticQueryConfig {
    pub backend: PathBuf,
    pub backend_args: Vec<String>,
    pub model: PathBuf,
}

/// One admitted source's contribution to the ranking view.
pub(crate) struct AdmittedEdition {
    pub membership: MembershipId,
    pub locator: String,
    pub edition: SemanticEdition,
    pub rows: Vec<MappingRow>,
}

/// What a semantic request resolved to before a single row was read.
pub(crate) enum SemanticPlan {
    /// No source could contribute; the answer falls back to lexical and
    /// says exactly why.
    Unavailable(String),
    Ready {
        editions: Vec<AdmittedEdition>,
        /// Present when some admitted source could *not* contribute, with
        /// the reason. An answer carrying this is explicitly partial
        /// semantics; it is never reported as fully applied.
        partial: Option<String>,
    },
}

/// Choose, for every admitted source, the edition this query will rank
/// through — or say why there is none.
///
/// `pinned` is a continuation's own captured selection: when present it is
/// authoritative and a source whose pinned edition is no longer usable is
/// an explicit refusal rather than a silent substitution of whatever is
/// selected now. A same-generation edition switch therefore cannot change
/// an open continuation's corpus.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_semantic(
    store: &AtlasStore,
    memberships: &[Membership],
    generations: &BTreeMap<MembershipId, GenerationId>,
    pinned: Option<&BTreeMap<MembershipId, EditionId>>,
    families: &[ContentFamily],
) -> Result<SemanticPlan, AtlasError> {
    let mut ready: Vec<AdmittedEdition> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    for membership in memberships {
        let Some(generation) = generations.get(&membership.id) else {
            // This source contributed no generation to the answer at all;
            // it is already reported through `generation_unavailable`.
            continue;
        };
        let chosen = match pinned {
            Some(pinned) => match pinned.get(&membership.id) {
                Some(id) => Some(id.clone()),
                None => {
                    excluded.push(format!(
                        "{}: this continuation captured no semantic edition for it",
                        membership.alias
                    ));
                    continue;
                }
            },
            None => match store.semantic_availability(membership)? {
                SemanticAvailability::Available => store.selected_semantic(membership),
                other => {
                    excluded.push(format!(
                        "{}: its selected edition is {}{}",
                        membership.alias,
                        other.label(),
                        other
                            .detail()
                            .map(|detail| format!(" — {detail}"))
                            .unwrap_or_default()
                    ));
                    continue;
                }
            },
        };
        let Some(id) = chosen else {
            excluded.push(format!("{}: no edition is selected", membership.alias));
            continue;
        };
        let edition = match store.read_edition(&id) {
            Ok(edition) => edition,
            Err(error) => {
                excluded.push(format!("{}: {error}", membership.alias));
                continue;
            }
        };
        // A pinned edition is verified here rather than inherited: a
        // continuation must not rank through bytes that have rotted since
        // the first page.
        if !matches!(
            store.verify_edition(&edition),
            crate::SemanticVerification::Verified
        ) {
            excluded.push(format!(
                "{}: edition {} no longer verifies",
                membership.alias, edition.id.0
            ));
            continue;
        }
        if &edition.generation != generation {
            excluded.push(format!(
                "{}: edition {} describes generation {}, and this answer reads {}",
                membership.alias, edition.id.0, edition.generation.0, generation.0
            ));
            continue;
        }
        let Some(retrieval) = edition.retrieval.clone() else {
            excluded.push(format!(
                "{}: edition {} was built under {} and binds no ranking representation; rebuild \
                 it before it can be ranked through",
                membership.alias, edition.id.0, edition.identity
            ));
            continue;
        };
        // A ranking representation is not a detail an answer may absorb
        // silently. An edition built under an older ranking path
        // convention holds paths that mean something else to the ranker —
        // v1 put the membership alias in the first path component, where
        // the native path priors read it (0167) — so it is refused, named,
        // and left exactly as it was built. The recovery is the ordinary
        // one: rebuild its semantic edition and select the rebuilt one.
        if retrieval.path_convention != RANKING_PATH_CONVENTION {
            excluded.push(format!(
                "{}: edition {} ranks paths under {} and this product ranks under {}; rebuild its \
                 semantic edition and select the rebuilt one before it can be ranked through",
                membership.alias, edition.id.0, retrieval.path_convention, RANKING_PATH_CONVENTION
            ));
            continue;
        }
        // The same rule, for the other half of the ranking contract. An
        // edition built under the previous universal-depth policy declares
        // one frozen candidate depth for every query ever asked of it
        // (ruling 0171). Its rows are perfectly good bytes and are left
        // exactly as they were built; what cannot happen is ranking them
        // under a policy they never declared, because a `K` this product
        // now takes from the caller would be silently reinterpreting that
        // edition's own statement about itself. The recovery is the
        // ordinary one, the same as the `v1` path convention's: rebuild
        // the semantic edition and select the rebuilt one.
        if retrieval.capacity_policy != CAPACITY_POLICY {
            let declared = match retrieval.candidate_limit {
                Some(limit) if retrieval.capacity_policy.is_empty() => format!(
                    "declares no result-capacity policy and a fixed universal candidate depth \
                     of {limit}"
                ),
                _ if retrieval.capacity_policy.is_empty() => {
                    "declares no result-capacity policy".to_owned()
                }
                _ => format!(
                    "decides result capacity under {}",
                    retrieval.capacity_policy
                ),
            };
            excluded.push(format!(
                "{}: edition {} {declared} and this product decides it under {}; rebuild its \
                 semantic edition and select the rebuilt one before it can be ranked through",
                membership.alias, edition.id.0, CAPACITY_POLICY
            ));
            continue;
        }
        // A native edition's stored vectors are a function of how its
        // chunks were batched into the embedding model's `encode` calls
        // (ruling 0175, D4): the installed tokenizer pads every text in
        // one call to that call's own longest member. An edition built
        // before this correction batched the whole request together and
        // its vectors are perfectly good bytes, left exactly as built;
        // what cannot happen is ranking them as though they were embedded
        // under today's per-resource policy. The recovery is the same as
        // the other two: rebuild the semantic edition and select the
        // rebuilt one.
        if retrieval.chunking == crate::semantic::SemanticChunking::Native
            && retrieval.batch_policy != EMBEDDING_BATCH_POLICY
        {
            let declared = if retrieval.batch_policy.is_empty() {
                "declares no embedding-batch policy".to_owned()
            } else {
                format!("embeds native chunks under {}", retrieval.batch_policy)
            };
            excluded.push(format!(
                "{}: edition {} {declared} and this product embeds native chunks under {}; \
                 rebuild its semantic edition and select the rebuilt one before it can be ranked \
                 through",
                membership.alias, edition.id.0, EMBEDDING_BATCH_POLICY
            ));
            continue;
        }
        if let Some(first) = ready.first() {
            let reference = first
                .edition
                .retrieval
                .as_ref()
                .expect("a planned edition always carries a retrieval identity");
            if reference.digest != retrieval.digest {
                excluded.push(format!(
                    "{}: edition {} ranks under retrieval identity {} while this answer ranks \
                     under {}; two ranking representations cannot be one ranked list",
                    membership.alias, edition.id.0, retrieval.digest, reference.digest
                ));
                continue;
            }
            if first.edition.model.consumed.digest != edition.model.consumed.digest {
                excluded.push(format!(
                    "{}: edition {} was embedded by model {} while this answer ranks vectors from \
                     {}; scores from two models are not comparable",
                    membership.alias,
                    edition.id.0,
                    edition.model.consumed.digest,
                    first.edition.model.consumed.digest
                ));
                continue;
            }
        }
        let rows = match store.read_mapping(&edition) {
            Ok(rows) => rows,
            Err(error) => {
                excluded.push(format!("{}: {error}", membership.alias));
                continue;
            }
        };
        let rows: Vec<MappingRow> = rows
            .into_iter()
            .filter(|row| {
                families.is_empty() || row.family.is_some_and(|family| families.contains(&family))
            })
            .collect();
        if rows.is_empty() {
            excluded.push(format!(
                "{}: edition {} holds no row in the requested content families",
                membership.alias, edition.id.0
            ));
            continue;
        }
        ready.push(AdmittedEdition {
            membership: membership.id.clone(),
            locator: membership.locator.clone(),
            edition,
            rows,
        });
    }
    if ready.is_empty() {
        let detail = if excluded.is_empty() {
            "no admitted source contributed a generation to rank".to_owned()
        } else {
            excluded.join("; ")
        };
        return Ok(SemanticPlan::Unavailable(detail));
    }
    let partial = (!excluded.is_empty()).then(|| excluded.join("; "));
    Ok(SemanticPlan::Ready {
        editions: ready,
        partial,
    })
}

/// One row of the view actually handed to the native ranker.
pub(crate) struct ViewRow {
    pub membership: MembershipId,
    pub estate: EstateScope,
    pub source: crate::SourceId,
    pub generation: GenerationId,
    pub path: Vec<u8>,
    pub object_id: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub bytes: Vec<u8>,
    pub ranking_path: String,
    /// The membership this row belongs to, as the native ranker receives
    /// it: an identity beside the path, never part of it
    /// (`RANKING_PATH_CONVENTION`).
    pub ranking_scope: String,
    pub slot: u64,
    pub text: String,
    pub language: Option<String>,
    /// The digest this row's ranking text was just re-verified against.
    /// Carried rather than recomputed: it is the exact per-row identity a
    /// built index is a function of, and it has already been checked
    /// against the committed bytes on the way in.
    pub ranking_text_digest: String,
}

/// One completed native ranking: the view that was handed over, the order
/// it came back in, and what the implementation said about doing it.
pub(crate) type RankedView = (Vec<ViewRow>, Vec<RankedRow>, SemanticApplication);

/// One ranked result, as the native implementation returned it.
pub(crate) struct RankedRow {
    pub row: usize,
    pub score: f64,
}

#[derive(Serialize)]
struct QueryHeader<'a> {
    protocol: &'a str,
    model_path: &'a str,
    vectors: &'a str,
    rows: u64,
    dimensions: u64,
    query: &'a str,
    top_k: u64,
    /// Where this exact view's built index may be kept, and the identity
    /// it is kept under. Both absent means "build it and keep nothing",
    /// which is what every backend older than this field already does
    /// with a header field it does not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    index_cache: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_key: Option<&'a str>,
}

#[derive(Serialize)]
struct QueryRow<'a> {
    row: u64,
    ranking_path: &'a str,
    ranking_scope: &'a str,
    slot: u64,
    text: &'a str,
    start_line: u64,
    end_line: u64,
    language: Option<&'a str>,
}

#[derive(Deserialize)]
struct QueryReply {
    protocol: String,
    #[allow(dead_code)]
    backend: String,
    native: String,
    model_path: String,
    model_digest: String,
    returned: u64,
    /// What the ranking process says about the modules that actually
    /// loaded in it. Absent is a legal answer — a compiled backend or a
    /// fresh wrapper has nothing to enumerate — and is recorded as
    /// `Unreported` rather than as an absence of provenance nobody
    /// noticed. Present, it is re-measured by the product under exactly
    /// the build side's rules before anything is recorded.
    #[serde(default)]
    environment: Option<ReportedEnvironment>,
}

#[derive(Deserialize)]
struct QueryResultRow {
    row: u64,
    score: f64,
}

/// Where a query's result capacity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CapacitySource {
    /// No capacity was named, so this query's capacity is the result limit
    /// it initially requested — the documented default (ruling 0171:
    /// "Existing callers omitting it get documented initial-limit-derived
    /// capacity").
    #[default]
    RequestedLimit,
    /// No capacity was named and the initially requested limit is above
    /// `CAPACITY_MAX`, so the derived capacity is the bound. The answer
    /// publishes both numbers rather than narrowing in silence.
    RequestedLimitBounded,
    /// The caller named this capacity. A normal bounded search request,
    /// not a tuning surface: it is the ordinary semantics of asking a
    /// search engine for `k` results, held apart from how many of them one
    /// page shows.
    Explicit,
}

impl CapacitySource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::RequestedLimit => "requested-limit",
            Self::RequestedLimitBounded => "requested-limit-bounded",
            Self::Explicit => "explicit",
        }
    }
}

/// One query's frozen result capacity: the `top_k` the native ranker is
/// asked for, and where that number came from.
///
/// Resolved once, before anything is ranked, from the request alone — so
/// the surface that issues a continuation can compute exactly the same
/// value it will later have to compare a restated request against, without
/// running a search to learn it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCapacity {
    pub value: u64,
    pub source: CapacitySource,
    /// The limit the capacity was derived from, when it was derived. Kept
    /// so a bounded derivation can say what it bounded.
    pub requested_limit: u64,
}

/// What a completed native ranking says about itself, recorded on the
/// answer so a caller never has to take "semantic" on the product's word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticApplication {
    pub native: String,
    pub model_digest: String,
    pub retrieval_digest: String,
    pub rows_ranked: u64,
    /// The result capacity this query ran at: the `top_k` actually handed
    /// to the native ranker, frozen for the whole continuation.
    pub capacity: u64,
    /// Where that capacity came from — the caller's explicit request, or
    /// the initially requested result limit — so a reader never has to
    /// guess whether a number was asked for or derived.
    pub capacity_source: CapacitySource,
    /// The policy the capacity was decided under, and its operational
    /// bound, restated from the editions this answer ranked through.
    pub capacity_policy: String,
    pub capacity_max: u64,
    /// How many rows the native ranker actually returned for this query.
    /// The size of this query's whole result set, not of one page.
    pub result_rows: u64,
    /// The result set filled the capacity: relevant rows may exist beyond
    /// this query's budget, and a deeper answer is a *new query* at a
    /// larger capacity.
    pub capacity_reached: bool,
    /// The native ranker returned fewer rows than the capacity allowed, so
    /// this query's bounded result set is exhausted. That is a fact about
    /// *this ranker at this capacity over the admitted view* and never a
    /// claim that the estate holds no other relevant information
    /// (`native-ranking-contract-review/ROOT-ADJUDICATION.md`).
    pub resultset_exhausted: bool,
    /// The implementation that actually ranked this answer, in exactly
    /// the terms the build side records its own producer: the executable
    /// the product opened and digested, every argv token in its executed
    /// position, the backend's own claim about itself, and the measured
    /// environment record with its honest coverage.
    ///
    /// `native` above is still the child's self-report and is still
    /// printed as such; this is the measured half beside it. Neither is
    /// execution attestation (`QUERY_PRODUCER_SCOPE`).
    pub producer: BackendIdentity,
    /// The producer digests this answer publishes and its continuation
    /// pins. `configuration` is checked before a child is started;
    /// `identity` once it has answered.
    pub producer_pin: QueryProducerPin,
}

/// The scheme the reusable-index identity below is digested under. Its own
/// label, so a digest computed for this purpose can never be mistaken for
/// an edition id, a producer identity or a retrieval digest.
///
/// `v2` absorbs one field more than `v1`: the membership scope each row
/// is ranked under. It moved with the ranking path convention — under v1
/// the scope was inside the path this already digested, and an index
/// built for a v1 key indexes different documents under different keys.
const QUERY_INDEX_IDENTITY: &str = "wirk-query-index/v2";

/// How many built query indexes are kept. The product owns this
/// directory's growth, not the backend: a backend that is handed a path
/// writes one index there and nothing else, and this is the only place
/// that decides how many such paths survive.
const QUERY_INDEX_CACHE_ENTRIES: usize = 8;

/// The exact identity of the admitted view a backend's built index is a
/// function of.
///
/// Everything the ranking representation of these rows depends on, and
/// nothing else: the producer configuration (the scheme, the protocol,
/// the ordering policy, and the backend executable and argv digested from
/// the bytes at the configured path — *not* the installed `semble` whose
/// tokenizer and enrichment those bytes call, which is outside that
/// digest and is guarded instead by the backend refusing a stored index
/// that does not carry the running `semble` version, `QUERY_PRODUCER_SCOPE`),
/// the retrieval identity every planned edition agrees on, and, per row in
/// view order, the coordinate the native ranker keys on plus the digest of
/// the ranking text — the same digest this query has just
/// re-derived from the committed bytes and checked. Two views with this
/// digest cannot differ in a way any index over them could see; a view
/// that differs anywhere gets a different digest and therefore a
/// different, empty directory.
fn view_index_identity(configuration: &str, retrieval_digest: &str, view: &[ViewRow]) -> String {
    let mut identity = Vec::with_capacity(view.len() * 128);
    for field in [
        QUERY_INDEX_IDENTITY,
        QUERY_PROTOCOL,
        configuration,
        retrieval_digest,
    ] {
        identity.extend_from_slice(field.as_bytes());
        identity.push(0);
    }
    for row in view {
        identity.extend_from_slice(row.ranking_path.as_bytes());
        identity.push(0);
        identity.extend_from_slice(row.ranking_scope.as_bytes());
        identity.push(0);
        identity.extend_from_slice(row.slot.to_string().as_bytes());
        identity.push(0);
        identity.extend_from_slice(row.line_start.to_string().as_bytes());
        identity.push(0);
        identity.extend_from_slice(row.line_end.to_string().as_bytes());
        identity.push(0);
        identity.extend_from_slice(row.language.as_deref().unwrap_or("").as_bytes());
        identity.push(0);
        identity.extend_from_slice(row.ranking_text_digest.as_bytes());
        identity.push(b'\n');
    }
    digest_bytes(&identity)
}

/// The directory this view's index may be reused from, or `None` — in
/// which case the backend builds one and the query costs exactly what it
/// cost before.
///
/// A stable path in a shared temporary directory is pre-creatable by
/// anyone who can write there, and an index is bytes a ranking is read
/// from, so the root is private (`0700`) and is used only if what is
/// actually on disk is a directory this user owns, at those permissions,
/// reached without following a symlink. Anything else and this returns
/// `None`: no reuse is a slower query, a poisoned index would be a
/// different answer.
fn query_index_cache(key: &str) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    // SAFETY: `getuid` takes no arguments, cannot fail, and touches no
    // memory this process owns — the same call, made the same way, as
    // `wirk/src/executors/docker.rs`.
    let own = unsafe { libc::getuid() };
    let root = std::env::temp_dir().join(format!("wirk-atlas-query-index-{own}"));
    if !root.exists() {
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&root);
    }
    let found = std::fs::symlink_metadata(&root).ok()?;
    if !found.is_dir() || found.uid() != own || found.permissions().mode() & 0o777 != 0o700 {
        return None;
    }
    prune_query_index_cache(&root, key, own);
    let entry = root.join(key);
    if !entry.exists() {
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&entry);
    }
    let entry_found = std::fs::symlink_metadata(&entry).ok()?;
    if !entry_found.is_dir() || entry_found.uid() != own {
        return None;
    }
    Some(entry)
}

/// Keep the newest `QUERY_INDEX_CACHE_ENTRIES` entries, always including
/// the one this query is about to use. Every entry is reproducible from
/// its own view, so removing one costs a rebuild and nothing else.
fn prune_query_index_cache(root: &Path, keep: &str, own: u32) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy() != keep)
        .filter_map(|entry| {
            let found = std::fs::symlink_metadata(entry.path()).ok()?;
            if !found.is_dir() || found.uid() != own {
                // Not ours and not a directory we made: left exactly
                // where it is, and never reused.
                return None;
            }
            Some((found.modified().ok()?, entry.path()))
        })
        .collect();
    if found.len() < QUERY_INDEX_CACHE_ENTRIES {
        return;
    }
    found.sort_by_key(|found| std::cmp::Reverse(found.0));
    for (_, path) in found.into_iter().skip(QUERY_INDEX_CACHE_ENTRIES - 1) {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Build the exact admitted view and rank it.
///
/// Every row's ranking text is re-derived here from the committed bytes
/// and checked against the digest its edition recorded, so a query ranks
/// the text the build actually embedded or it ranks nothing.
pub(crate) fn rank(
    config: &SemanticQueryConfig,
    editions: &[AdmittedEdition],
    store: &AtlasStore,
    query: &str,
    capacity: ResolvedCapacity,
    pinned_producer: Option<&QueryProducerPin>,
) -> Result<Result<RankedView, String>, AtlasError> {
    // The producer's *configuration* is measured before anything else
    // happens, because it is the half a continuation can be refused on
    // without ranking a single row. `configured_file` is the build side's
    // own reading of a configured executable, applied here to the query
    // side's (0089 C1/C2/C3: an absolute path that resolves, digested
    // through the path that was actually opened — so a symlink retargeted
    // between two pages resolves and digests differently even though the
    // configured string never moved).
    let program = match configured_file(&config.backend, "query backend") {
        Ok(program) => program,
        Err(reason) => return Ok(Err(reason)),
    };
    let mut arguments = Vec::new();
    let mut argv = Vec::new();
    for argument in &config.backend_args {
        let path = Path::new(argument);
        if path.is_absolute() && path.is_file() {
            match configured_file(path, "query backend argument") {
                Ok(recorded) => {
                    arguments.push(recorded.clone());
                    argv.push(BackendArgument::File {
                        value: argument.clone(),
                        file: Box::new(recorded),
                    });
                }
                Err(reason) => return Ok(Err(reason)),
            }
        } else {
            argv.push(BackendArgument::Literal {
                value: argument.clone(),
            });
        }
    }
    let configuration = query_producer_configuration_digest(&program, &argv);
    if let Some(pinned) = pinned_producer {
        if pinned.configuration != configuration {
            return Ok(Err(format!(
                "this continuation was ranked by query producer configuration {} and the \
                 configuration in force now is {}; either the executable or an argument at the \
                 same configured path is not the bytes that produced the first page, or this \
                 build's effective ordering and runtime policy ({QUERY_ORDERING_POLICY}) is not \
                 the one that ranked it, so the page it asks for cannot be reproduced",
                pinned.configuration, configuration
            )));
        }
        // The basis, checked before a child is started, because it is a
        // property of the *first* page and no amount of ranking now can
        // supply it. `VERDICT.md` V1: a configuration-only pin cannot
        // distinguish the ranker that produced page 1 from a different
        // one at the same path, so serving page 2 under it would be
        // exactly the defect this correction exists to close — with the
        // digests agreeing and telling the caller nothing.
        //
        // The product retains no ranked page, so the original contract's
        // first branch ("retain the original verified result state") is
        // not available and its second is what happens: an explicit
        // refusal naming the exact missing basis. The token itself is
        // preserved, so a caller whose backend later reports an
        // environment resumes rather than being handed a dead receipt.
        if pinned.basis == QueryProducerBasis::ConfigurationOnly {
            return Ok(Err(format!(
                "this continuation cannot be reproduced: {QUERY_PRODUCER_BASIS_MISSING}"
            )));
        }
    }
    let mut view: Vec<ViewRow> = Vec::new();
    let mut vectors: Vec<u8> = Vec::new();
    let mut dimensions = 0u64;
    let model = match crate::semantic::configured_model(&config.model) {
        Ok(model) => model,
        Err(reason) => return Ok(Err(reason)),
    };
    for admitted in editions {
        if admitted.edition.model.consumed.digest != model.digest {
            return Ok(Err(format!(
                "the configured query model digests to {} but edition {} was embedded by {}; a \
                 query vector from a different model is not comparable with these rows",
                model.digest, admitted.edition.id.0, admitted.edition.model.consumed.digest
            )));
        }
        let raw = match store.edition_file(&admitted.edition.id, &admitted.edition.vectors.file) {
            Ok(raw) => raw,
            Err(error) => {
                return Ok(Err(format!(
                    "edition {} vectors are unreadable: {error}",
                    admitted.edition.id.0
                )));
            }
        };
        let width = admitted.edition.vectors.dimensions;
        if dimensions == 0 {
            dimensions = width;
        } else if dimensions != width {
            return Ok(Err(format!(
                "edition {} holds {width}-dimensional vectors and this view is already {dimensions}",
                admitted.edition.id.0
            )));
        }
        let stride = (width * 4) as usize;
        if raw.len() != admitted.edition.vectors.rows as usize * stride {
            return Ok(Err(format!(
                "edition {} vector file is {} bytes, not the {} its record commits to",
                admitted.edition.id.0,
                raw.len(),
                admitted.edition.vectors.rows as usize * stride
            )));
        }
        // One `git cat-file --batch-command` session reads every unique
        // object this edition's rows address, instead of one `git`
        // process spawn per unique object (the per-row lazy fetch this
        // replaced). Same bytes, same per-object error surfaced the same
        // way; only how many processes are started to get them changes.
        let mut unique_oids: Vec<String> = Vec::new();
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for row in &admitted.rows {
            if seen.insert(row.object_id.as_str()) {
                unique_oids.push(row.object_id.clone());
            }
        }
        let cache: BTreeMap<String, Vec<u8>> =
            match crate::git::blobs(Path::new(&admitted.locator), &unique_oids) {
                Ok(cache) => cache,
                Err(AtlasError::GitUnavailable(detail)) => {
                    return Ok(Err(format!(
                        "the committed bytes edition {} ranks over are unavailable: {detail}",
                        admitted.edition.id.0
                    )));
                }
                Err(error) => return Err(error),
            };
        for row in &admitted.rows {
            // Borrowed, not cloned: one blob backs every row that
            // addresses a range inside it, and cloning it per row copied
            // the whole object once per chunk of it.
            let Some(bytes) = cache.get(&row.object_id) else {
                // What is actually known: the batched read returned no
                // bytes for this object. Naming a `git` message this
                // process never received would put an invented diagnosis
                // in an answer whose whole job is to say what it read.
                return Ok(Err(format!(
                    "the committed bytes edition {} ranks over are unavailable: object {} was not \
                     returned by the batched read of {}",
                    admitted.edition.id.0, row.object_id, admitted.locator
                )));
            };
            let (start, end) = (row.byte_start as usize, row.byte_end as usize);
            if end > bytes.len() || start > end {
                return Ok(Err(format!(
                    "edition {} row {} addresses bytes outside its own blob",
                    admitted.edition.id.0, row.row
                )));
            }
            let slice = &bytes[start..end];
            let text = normalize_ranking_text(slice);
            let text_digest = digest_bytes(text.as_bytes());
            if text_digest != row.ranking_text_digest() {
                return Ok(Err(format!(
                    "edition {} row {} no longer re-derives to the text it recorded; the committed \
                     bytes behind it are not the bytes it was built from",
                    admitted.edition.id.0, row.row
                )));
            }
            let Some(ranking_path) = row.ranking_path.clone() else {
                return Ok(Err(format!(
                    "edition {} row {} carries no ranking path",
                    admitted.edition.id.0, row.row
                )));
            };
            let offset = row.row as usize * stride;
            vectors.extend_from_slice(&raw[offset..offset + stride]);
            view.push(ViewRow {
                membership: admitted.membership.clone(),
                estate: row.estate.clone(),
                source: row.source.clone(),
                generation: row.generation.clone(),
                path: row.path.clone(),
                object_id: row.object_id.clone(),
                byte_start: row.byte_start,
                byte_end: row.byte_end,
                line_start: row.line_start,
                line_end: row.line_end,
                bytes: slice.to_vec(),
                ranking_path,
                ranking_scope: ranking_scope(&admitted.membership),
                slot: row.slot.unwrap_or(0),
                text,
                language: row.language.clone(),
                ranking_text_digest: text_digest,
            });
        }
    }
    if view.is_empty() {
        return Ok(Err("the admitted view holds no row".into()));
    }

    // The one place a query writes bytes: a private temporary directory
    // holding this view's own vectors, removed before the answer returns.
    // Nothing under the estate is touched.
    let scratch = std::env::temp_dir().join(format!("wirk-atlas-query-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&scratch)?;
    // What this view is, exactly, so a backend can recognise an index it
    // has already built over these same rows instead of building the
    // same one again. The identity is the product's to compute — it is
    // the one side that knows what was admitted and has just verified
    // every row's bytes — and the directory is the product's to bound.
    let planned_retrieval = editions[0]
        .edition
        .retrieval
        .as_ref()
        .expect("a planned edition always carries a retrieval identity");
    let index_key = view_index_identity(&configuration, &planned_retrieval.digest, &view);
    let index_cache = query_index_cache(&index_key);
    let outcome = (|| -> Result<Result<(Vec<RankedRow>, QueryReply), String>, AtlasError> {
        let vectors_path = scratch.join("view.bin");
        std::fs::write(&vectors_path, &vectors)?;
        Ok(run_query_backend(
            config,
            &model.canonical,
            &vectors_path,
            view.len() as u64,
            dimensions,
            query,
            &view,
            capacity.value,
            index_cache.as_deref(),
            &index_key,
        ))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let (ranked, reply) = match outcome? {
        Ok(result) => result,
        Err(reason) => return Ok(Err(reason)),
    };
    if reply.model_path != model.canonical || reply.model_digest != model.digest {
        return Ok(Err(format!(
            "the query backend ranked with model {} ({}) but this query consumed {} ({})",
            reply.model_path, reply.model_digest, model.canonical, model.digest
        )));
    }
    for result in &ranked {
        if result.row >= view.len() {
            return Ok(Err(
                "the query backend returned a row this view never sent".into()
            ));
        }
    }
    let retrieval = editions[0]
        .edition
        .retrieval
        .as_ref()
        .expect("a planned edition always carries a retrieval identity");
    // What the process that answered says about its own loaded modules,
    // re-measured here by the product itself under the build side's rules
    // — the same `measure_environment` that refuses a build whose backend
    // and product disagree about a file's bytes.
    let environment = match &reply.environment {
        None => BackendEnvironment::Unreported,
        Some(reported) => match measure_environment(reported) {
            Ok(measured) => BackendEnvironment::Reported(Box::new(measured)),
            Err(reason) => {
                return Ok(Err(format!(
                    "the query backend's environment report does not hold up: {reason}"
                )));
            }
        },
    };
    let producer = BackendIdentity {
        protocol: QUERY_PROTOCOL.to_owned(),
        program,
        arguments,
        argv,
        reported: reply.native.clone(),
        environment,
    };
    let producer_pin = QueryProducerPin {
        configuration,
        identity: query_producer_identity_digest(&producer),
        basis: producer_basis(&producer.environment),
    };
    // The same question asked of *this* ranking. A pin measured on
    // implementation bytes whose backend now reports nothing would already
    // fail the identity check below — the environment enters that digest —
    // but it would fail with the wrong sentence, naming two digests when
    // the actual answer is that there is no longer anything under them.
    if pinned_producer.is_some() && producer_pin.basis == QueryProducerBasis::ConfigurationOnly {
        return Ok(Err(
            "this continuation cannot be reproduced: the ranking configured now reports no \
             loaded-module basis (its environment is unreported, or carries no module list), so \
             it cannot be compared with the implementation that produced the first page"
                .to_owned(),
        ));
    }
    // The second half of the same check. The configuration matched, so the
    // file at the configured path is byte-for-byte the one that produced
    // the first page; this catches what that cannot see — the modules that
    // loaded inside it. A ranking that has already happened is discarded
    // rather than served: no page is better than a reranked page under the
    // first page's receipt.
    if let Some(pinned) = pinned_producer
        && pinned.identity != producer_pin.identity
    {
        return Ok(Err(format!(
            "this continuation was ranked by query producer identity {} and this ranking is {}; \
             the implementation that answered is not the one that produced the first page, so \
             the page it asks for cannot be reproduced",
            pinned.identity, producer_pin.identity
        )));
    }
    let application = SemanticApplication {
        native: reply.native.clone(),
        model_digest: model.digest.clone(),
        retrieval_digest: retrieval.digest.clone(),
        rows_ranked: view.len() as u64,
        capacity: capacity.value,
        capacity_source: capacity.source,
        capacity_policy: retrieval.capacity_policy.clone(),
        capacity_max: retrieval.capacity_max,
        result_rows: ranked.len() as u64,
        capacity_reached: ranked.len() as u64 >= capacity.value,
        resultset_exhausted: (ranked.len() as u64) < capacity.value,
        producer,
        producer_pin,
    };
    Ok(Ok((view, ranked, application)))
}

#[allow(clippy::too_many_arguments)]
fn run_query_backend(
    config: &SemanticQueryConfig,
    model: &str,
    vectors: &Path,
    rows: u64,
    dimensions: u64,
    query: &str,
    view: &[ViewRow],
    capacity: u64,
    index_cache: Option<&Path>,
    index_key: &str,
) -> Result<(Vec<RankedRow>, QueryReply), String> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(&config.backend);
    command
        .args(&config.backend_args)
        .env_clear()
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("PYTHONNOUSERSITE", "1")
        // The selection half of `QUERY_ORDERING_POLICY`, applied where it
        // has to be applied: on the child, before it ranks anything.
        //
        // R5, the installed runtime's own control rather than a change to
        // the ranker. `semble` 0.5.6 unions its two candidate lists into a
        // `set` and sorts that set on `start_line` alone; rows sharing a
        // start line — every whole-file row does — keep the set's own
        // iteration order, which CPython derives from a per-process random
        // hash seed. That order survives every later stable sort, so it
        // decides which rows `rerank_topk` keeps at its `top_k` cut when
        // penalised scores tie there. Each page of a walk is its own
        // process, so without this the pages are slices of two different
        // candidate pools (0163; measured at a real equal-score boundary).
        //
        // It fixes *which rows are selected*, not what order they are
        // served in: `query::order_ranked` still imposes this crate's own
        // canonical total order on the pool. No score is touched, nothing
        // is re-ranked here, and the installed `semble` is not patched.
        .env("PYTHONHASHSEED", QUERY_HASH_SEED)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        format!(
            "query backend {} could not be started: {error}",
            config.backend.display()
        )
    })?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let header = serde_json::to_vec(&QueryHeader {
        protocol: QUERY_PROTOCOL,
        model_path: model,
        vectors: &vectors.display().to_string(),
        rows,
        dimensions,
        query,
        top_k: capacity,
        index_cache: index_cache.map(|path| path.to_str()).unwrap_or(None),
        index_key: index_cache.and(Some(index_key)),
    })
    .map_err(|error| format!("query request could not be encoded: {error}"))?;
    let write = (|| -> std::io::Result<()> {
        stdin.write_all(&header)?;
        stdin.write_all(b"\n")?;
        for (index, row) in view.iter().enumerate() {
            stdin.write_all(&serde_json::to_vec(&QueryRow {
                row: index as u64,
                ranking_path: &row.ranking_path,
                ranking_scope: &row.ranking_scope,
                slot: row.slot,
                text: &row.text,
                start_line: row.line_start,
                end_line: row.line_end,
                language: row.language.as_deref(),
            })?)?;
            stdin.write_all(b"\n")?;
        }
        stdin.flush()
    })();
    drop(stdin);
    let finished = child
        .wait_with_output()
        .map_err(|error| format!("query backend {} failed: {error}", config.backend.display()))?;
    if !finished.status.success() {
        return Err(format!(
            "query backend {} exited {} : {}",
            config.backend.display(),
            finished
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "by signal".into()),
            String::from_utf8_lossy(&finished.stderr).trim()
        ));
    }
    if let Err(error) = write {
        return Err(format!(
            "query backend {} did not consume the view: {error}",
            config.backend.display()
        ));
    }
    let stdout = String::from_utf8_lossy(&finished.stdout);
    let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
    let Some(head) = lines.next() else {
        return Err(format!(
            "query backend {} produced no reply line",
            config.backend.display()
        ));
    };
    let reply: QueryReply = serde_json::from_str(head)
        .map_err(|error| format!("query reply is not a {QUERY_PROTOCOL} record: {error}"))?;
    if reply.protocol != QUERY_PROTOCOL {
        return Err(format!(
            "query backend speaks protocol {} but this product speaks {QUERY_PROTOCOL}",
            reply.protocol
        ));
    }
    let mut ranked = Vec::new();
    for line in lines {
        let result: QueryResultRow = serde_json::from_str(line)
            .map_err(|error| format!("query result line is malformed: {error}"))?;
        ranked.push(RankedRow {
            row: result.row as usize,
            score: result.score,
        });
    }
    if ranked.len() as u64 != reply.returned {
        return Err(format!(
            "query backend announced {} results and sent {}",
            reply.returned,
            ranked.len()
        ));
    }
    Ok((ranked, reply))
}

#[cfg(test)]
mod query_index_identity_tests {
    use super::*;
    use crate::{EstateScope, GenerationId, MembershipId, SourceId};

    fn row(path: &str, slot: u64, text: &str) -> ViewRow {
        ViewRow {
            membership: MembershipId("m-x".into()),
            estate: EstateScope("/estate".into()),
            source: SourceId("s-x".into()),
            generation: GenerationId("g-x".into()),
            path: path.as_bytes().to_vec(),
            object_id: "0".repeat(40),
            byte_start: 0,
            byte_end: text.len() as u64,
            line_start: 1,
            line_end: 2,
            bytes: text.as_bytes().to_vec(),
            ranking_path: path.to_owned(),
            ranking_scope: "m-x".to_owned(),
            slot,
            text: text.to_owned(),
            language: Some("rust".into()),
            ranking_text_digest: crate::semantic::digest_bytes(text.as_bytes()),
        }
    }

    /// The identity is a function of the view, and of everything in the
    /// view an index over it could see. Same rows, same key; any change
    /// to the ranked text, the coordinate it is ranked under, or the
    /// implementation that would tokenise it, and the key moves — which
    /// is the whole invalidation rule: a moved key names a directory that
    /// holds nothing.
    /// Two memberships publishing the same relative path are two
    /// different views, and an index built over one cannot be reused for
    /// the other: under this convention the scope, not the path, is what
    /// separates their documents.
    #[test]
    fn the_membership_scope_is_part_of_the_index_identity() {
        let base = vec![row("a/one.rs", 0, "alpha")];
        let mut moved = vec![row("a/one.rs", 0, "alpha")];
        moved[0].ranking_scope = "m-y".into();
        assert_ne!(
            view_index_identity("configuration-1", "retrieval-1", &base),
            view_index_identity("configuration-1", "retrieval-1", &moved),
        );
    }

    #[test]
    fn a_view_that_differs_anywhere_gets_a_different_index_identity() {
        let base = vec![row("a/one.rs", 0, "alpha"), row("a/two.rs", 0, "beta")];
        let key = view_index_identity("configuration-1", "retrieval-1", &base);
        assert_eq!(
            key,
            view_index_identity("configuration-1", "retrieval-1", &base),
            "the same view digests to the same identity"
        );
        assert_eq!(key.len(), 64, "an identity is a sha256 in hex");

        for (what, moved) in [
            (
                "the ranking text",
                vec![row("a/one.rs", 0, "ALPHA"), row("a/two.rs", 0, "beta")],
            ),
            (
                "the ranking path",
                vec![row("a/renamed.rs", 0, "alpha"), row("a/two.rs", 0, "beta")],
            ),
            (
                "the slot",
                vec![row("a/one.rs", 1, "alpha"), row("a/two.rs", 0, "beta")],
            ),
            (
                "the row order",
                vec![row("a/two.rs", 0, "beta"), row("a/one.rs", 0, "alpha")],
            ),
            ("a dropped row", vec![row("a/one.rs", 0, "alpha")]),
        ] {
            assert_ne!(
                key,
                view_index_identity("configuration-1", "retrieval-1", &moved),
                "{what} changed and the index identity did not"
            );
        }
        assert_ne!(
            key,
            view_index_identity("configuration-2", "retrieval-1", &base),
            "the producer configuration changed and the index identity did not"
        );
        assert_ne!(
            key,
            view_index_identity("configuration-1", "retrieval-2", &base),
            "the retrieval identity changed and the index identity did not"
        );
    }

    /// Two rows that differ only in a field the identity does not read
    /// would be a hole in it. This pins the two that are deliberately not
    /// read — where the bytes live — because the ranking never sees them.
    #[test]
    fn the_identity_reads_what_a_ranking_reads_and_not_where_it_came_from() {
        let mut moved = row("a/one.rs", 0, "alpha");
        let base = vec![row("a/one.rs", 0, "alpha")];
        moved.object_id = "1".repeat(40);
        moved.byte_start = 4096;
        moved.byte_end = 4096 + 5;
        assert_eq!(
            view_index_identity("c", "r", &base),
            view_index_identity("c", "r", &[moved]),
            "the same ranked text at the same coordinate is the same document to rank"
        );
    }
}
