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
    BackendArgument, BackendEnvironment, BackendIdentity, CANDIDATE_LIMIT, EditionId, MappingRow,
    QUERY_PRODUCER_BASIS_MISSING, QUERY_PROTOCOL, QueryProducerBasis, QueryProducerPin,
    ReportedEnvironment, SemanticEdition, configured_file, digest_bytes, measure_environment,
    normalize_ranking_text, producer_basis, query_producer_configuration_digest,
    query_producer_identity_digest,
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
    pub slot: u64,
    pub text: String,
    pub language: Option<String>,
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
}

#[derive(Serialize)]
struct QueryRow<'a> {
    row: u64,
    ranking_path: &'a str,
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

/// What a completed native ranking says about itself, recorded on the
/// answer so a caller never has to take "semantic" on the product's word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticApplication {
    pub native: String,
    pub model_digest: String,
    pub retrieval_digest: String,
    pub rows_ranked: u64,
    pub candidate_limit: u64,
    pub saturated: bool,
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
                "this continuation was ranked by query producer configuration {} and the backend \
                 configured now is {}; the executable or an argument at the same configured path \
                 is not the bytes that produced the first page, so the page it asks for cannot be \
                 reproduced",
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
        let mut cache: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for row in &admitted.rows {
            let bytes = match cache.get(&row.object_id) {
                Some(bytes) => bytes.clone(),
                None => {
                    let bytes = match crate::git::blob(Path::new(&admitted.locator), &row.object_id)
                    {
                        Ok(bytes) => bytes,
                        Err(AtlasError::GitUnavailable(detail)) => {
                            return Ok(Err(format!(
                                "the committed bytes edition {} ranks over are unavailable: \
                                 {detail}",
                                admitted.edition.id.0
                            )));
                        }
                        Err(error) => return Err(error),
                    };
                    cache.insert(row.object_id.clone(), bytes.clone());
                    bytes
                }
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
            if digest_bytes(text.as_bytes()) != row.ranking_text_digest() {
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
                slot: row.slot.unwrap_or(0),
                text,
                language: row.language.clone(),
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
        candidate_limit: CANDIDATE_LIMIT,
        saturated: ranked.len() as u64 >= CANDIDATE_LIMIT,
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
        top_k: CANDIDATE_LIMIT,
    })
    .map_err(|error| format!("query request could not be encoded: {error}"))?;
    let write = (|| -> std::io::Result<()> {
        stdin.write_all(&header)?;
        stdin.write_all(b"\n")?;
        for (index, row) in view.iter().enumerate() {
            stdin.write_all(&serde_json::to_vec(&QueryRow {
                row: index as u64,
                ranking_path: &row.ranking_path,
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
