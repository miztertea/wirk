//! P3 W4 A (`W4-PUBLIC-LIFECYCLE-BUILD.md`): explicit, immutable semantic
//! *editions* over an already-published source generation.
//!
//! Nothing here ranks, queries or embeds anything itself. The product owns
//! the identity contract — which exact committed bytes were embedded, by
//! which model, through which backend, producing which vector bytes — and
//! delegates the embedding itself to an explicitly configured executable
//! at an argv boundary (`SemanticBuildConfig`). That boundary is the whole
//! portability rule: no model name, cache path, interpreter or host
//! directory is a constant of this crate. R5 (use the installed facility
//! the way it does) is honoured by *configuring* it, never by cloning a
//! loader or a ranker into the product.
//!
//! Three separations matter, and each is a rejected shortcut from
//! `W4-PREPARATION-ADJUDICATION.md` item 2:
//!
//! * A semantic edition is **not** a source generation. A generation is
//!   keyed by source bytes alone; several editions — different models,
//!   different backends, different chunker editions — can and do exist
//!   for one unchanged generation. Nothing here mutates a generation or
//!   encodes vector availability into an extractor suffix.
//! * **Recipe equality is not output identity** (`0078`). An edition's id
//!   covers the digests of the vector bytes and the mapping bytes actually
//!   written to disk, not merely the configuration that claims to have
//!   produced them.
//! * **Building is not selecting.** A build stages an immutable directory
//!   that no reader consults; a separate, atomic `select` names it in the
//!   catalog. A failed build or a refused selection leaves the previously
//!   selected edition exactly as it was.
//!
//! This increment deliberately does not retrieve. `SemanticStatus` never
//! reports `Applied` because no ranking reads these vectors yet; W4 B owns
//! that, and claiming it here would be the "asserting search happened"
//! failure the brief names.

use crate::domain::now_unix_millis;
use crate::{
    AtlasError, EstateScope, GenerationId, Membership, MembershipId, SourceGeneration, SourceId,
    UnitId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The first stdin/stdout contract this crate spoke to a configured
/// backend. Retained as a public constant because every edition built
/// before native chunking records it as the protocol its vectors were
/// produced under, and those records stay readable and stay labelled with
/// the protocol they actually used. Nothing writes it any more.
pub const EMBED_PROTOCOL: &str = "wirk-embed/v1";

/// The only vector encoding this increment writes or verifies: little
/// endian IEEE-754 binary32, row-major, `rows * dimensions` values with no
/// header or padding. Named in the edition record so a later format is a
/// visibly different identity rather than a silent reinterpretation of the
/// same bytes.
pub const VECTOR_FORMAT: &str = "f32le-row-major/v1";

pub const EDITION_RECORD: &str = "edition.json";
pub const MAPPING_FILE: &str = "mapping.ndjson";
pub const VECTORS_FILE: &str = "vectors.bin";

/// The build protocol that also *chunks*. `wirk-embed/v1` sends text and
/// receives vectors; a native chunker's boundaries are not the product's
/// to compute, so `v2` sends the committed blob bytes of each admitted
/// resource and receives, per produced row, the byte range **in those
/// original bytes**. `v1` remains exactly what it was: every edition
/// already built binds the bytes of a backend that speaks it.
pub const EMBED_PROTOCOL_V2: &str = "wirk-embed/v2";

/// The query protocol. The product composes the admitted ranking view —
/// which rows, in which order, with which vectors — and the backend ranks
/// it with the installed native implementation. No index is persisted and
/// nothing but the query itself is ever embedded.
///
/// `v2` carries one field `v1` did not: every row's membership scope,
/// beside a ranking path that is now the source-relative path alone
/// (`RANKING_PATH_CONVENTION`). The version moves because a `v1` backend
/// handed a `v2` row would rank the right text under the wrong identity —
/// it would collapse two memberships' copies of one relative path into a
/// single document — and because a `v2` backend handed a `v1` row would
/// tokenise an alias. Neither is a thing to discover from the scores.
pub const QUERY_PROTOCOL: &str = "wirk-query/v2";

/// How a row's ranking text relates to the committed bytes it addresses.
/// `identity` means the two are the same bytes; the other value names the
/// transformation the native reader applies
/// (`native-chunk-boundary-use/HANDOFF.md` N1), so a row whose digests
/// differ says *why* rather than reading as corrupt.
pub const TEXT_IDENTITY: &str = "identity";
pub const TEXT_NORMALIZED: &str = "universal-newline+utf8-replace/v1";

/// The frozen path string convention fed to the native ranker, disclosed
/// in every edition that uses it.
///
/// It is not cosmetic and it is not recoverable afterwards, because the
/// native ranker reads one string for two different jobs. As *text* the
/// path is ranking evidence: `enrich_for_bm25` appends the file stem twice
/// and the last three directory components to every BM25 document,
/// `_boost_stem_matches` reads the stem and the parent directory name, and
/// `rerank_topk`'s `_file_path_penalty` matches test, compat/legacy and
/// example directory and file patterns anywhere in it. As *identity* the
/// same string is the BM25 document key (`make_chunk_id`) and the key
/// `boost_multi_chunk_files` and the file-saturation decay group by, so
/// two memberships publishing the same source-relative path must stay
/// distinct here, one layer below any adapter
/// (`W4-CONTROL-ADJUDICATION.md`).
///
/// `v1` served both jobs with `{alias}/{source-relative path}`, which put
/// the operator's own name for the source in the first path component —
/// exactly where those priors read. A membership called `tests` or
/// `legacy` therefore had the test/compat penalty applied to every one of
/// its rows, cancelling the prior's *discrimination* inside that source
/// and losing about three quarters of its share of the head against a
/// neighbour holding identical bytes (`native-ranking-gap-review/REVIEW.md`
/// F2, measured through the real engine).
///
/// `v2` separates the two jobs rather than spelling the alias differently
/// (0167: no denylist, no escape chosen to miss today's regexes). The
/// ranking path is the source-relative path verbatim — what the ranker's
/// own contract expects, "already repo-relative … so machine-specific
/// directory components are never indexed" — and the membership travels
/// beside it as an opaque scope (`RankingScope`) that is a document key
/// and a grouping key and is never tokenised, never enriched into a
/// document, and never matched by a path prior. Neither an operator's
/// alias nor any estate or host path can become relevance text under it.
///
/// The convention is disclosed in `RetrievalIdentity`, so it is bound into
/// every edition id and every continuation: a `v1` edition is refused
/// rather than reinterpreted (`retrieval.rs`), and its recorded bytes are
/// left exactly as they were built.
pub const RANKING_PATH_CONVENTION: &str = "source-relative-path+membership-scope/v2";

/// The retrieval identity scheme this product writes.
pub const RETRIEVAL_SCHEME: &str = "wirk-retrieval/v1";

/// How a query's result capacity is decided (ruling 0171). An edition
/// records the *policy*, never one query's capacity: baking each `K` into
/// immutable edition data would make every capacity its own edition
/// identity, which is worse than the universal depth it replaces.
///
/// The mechanism this policy exists for, measured rather than assumed
/// (`native-ranking-gap-review/REVIEW.md` F1): the native ranker truncates
/// *each* modality's list to `top_k * 5` before fusing them. A row's own
/// semantic score and its rank within each modality do not move with the
/// depth; what moves is whether its **second** modality's rank fell inside
/// that cut, and so whether its fused score carries that modality's
/// reciprocal-rank term at all. `boost_multi_chunk_files` then normalises
/// by the largest file sum in the candidate pool, so a larger pool also
/// changes the boost every file's top chunk receives, and
/// `apply_query_boost` inherits the changed maximum. Asking for a
/// different `top_k` is therefore not guaranteed to return a prefix of the
/// larger answer even though nothing was re-ranked.
///
/// The previous policy answered that by fixing one universal depth of 200
/// for every query, which made a five-result request a slice of a
/// two-hundred-result ranking and not the ranking the caller asked for
/// (measured: `query-capacity-build/BUILT.md` red). This policy separates
/// the two quantities the old one fused
/// (`native-ranking-contract-review/CORRECTIONS.md` C5):
///
/// * **result capacity `K`** — how many ranked rows this query's answer
///   consists of, i.e. the `top_k` handed to `semble.search.search`. It is
///   the caller's, it defaults to the initially requested result limit, it
///   is frozen at the first page and bound into the continuation, and
///   asking for a deeper one is a *new query*, never a continuation of an
///   old walk.
/// * **display page size** — how many of those rows one page shows. It
///   moves nothing about the ranking: every page of a walk re-runs the
///   same `search(top_k = K)` and slices its output.
///
/// `K` is a budget, not a promise: fewer than `K` rows means this ranker's
/// bounded result set was exhausted, never that the admitted view holds no
/// other relevant information.
pub const CAPACITY_POLICY: &str = "query-bound-capacity/v1";

/// The largest result capacity this increment will run, retained at the
/// previous universal depth (ruling 0171: "Keep existing operational upper
/// bound 200 for this bounded increment … not as a guaranteed count").
/// Larger estate budgets are separate work. An explicitly requested
/// capacity above it is refused rather than quietly clamped; a capacity
/// *derived* from a larger requested limit is bounded here and the answer
/// says so.
pub const CAPACITY_MAX: u64 = 200;

/// The capacity every edition built under the previous policy recorded, so
/// such an edition can be named in the sentence that refuses it rather
/// than merely failing a digest comparison.
pub const LEGACY_CANDIDATE_LIMIT: u64 = 200;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EditionId(pub String);

/// A path the caller configured, as configured *and* as actually used.
/// Both are kept: the raw string is what a reader must be told to
/// reproduce the build, the canonical string is what was really opened.
/// `digest` is over the bytes actually read through that canonical path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfiguredPath {
    pub configured: String,
    pub canonical: String,
    pub digest: String,
    pub byte_len: u64,
    /// Files contributing to `digest`; 1 for a regular file, the whole
    /// recursive count for a model directory.
    pub file_count: u64,
}

/// The model an edition's vectors were actually produced by.
///
/// `consumed` is the product's own canonicalization and digest of the
/// configured directory. `reported_*` is what the backend independently
/// says it loaded, recomputed on its side. A build refuses unless the two
/// agree — the 0088 defect in prototype form was exactly a consumed string
/// that nothing compared against the object really loaded, so the product
/// asks the loader itself rather than trusting its own argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub consumed: ConfiguredPath,
    pub reported_path: String,
    pub reported_digest: String,
}

/// One argv token, as executed, in its executed position.
///
/// W4 A's first record kept only the arguments that happened to name a
/// file and discarded every other token, so two builds whose backends
/// were handed genuinely different options were indistinguishable in the
/// record whenever their outputs happened to agree
/// (`W4-LIFECYCLE-CORRECTION.md` item 4, watched failing in
/// `raw/11-red-argv.log`). Producer *configuration* is part of producer
/// identity, so the whole ordered token list is recorded and absorbed —
/// each token separately, so `["--precision", "high"]` can never digest
/// as `["--precisionhigh"]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BackendArgument {
    /// A token that named no existing absolute file: an option, a flag,
    /// a value. Recorded verbatim; it has no bytes of its own to digest.
    Literal { value: String },
    /// A token that named an existing absolute file, whose *content* is
    /// therefore also an input. `value` is still the token as executed;
    /// `file` is the separate content binding.
    File {
        value: String,
        file: Box<ConfiguredPath>,
    },
}

impl BackendArgument {
    pub fn value(&self) -> &str {
        match self {
            Self::Literal { value } => value,
            Self::File { value, .. } => value,
        }
    }

    pub fn file(&self) -> Option<&ConfiguredPath> {
        match self {
            Self::Literal { .. } => None,
            Self::File { file, .. } => Some(file),
        }
    }
}

/// One installed distribution that was actually imported into the
/// backend's process, identified through the installer's own metadata.
///
/// `record_digest` and `metadata_digest` are the *product's* reading of
/// `RECORD` and `METADATA` at `metadata_path`, not the backend's claim
/// about them; a build refuses when the two readings disagree, exactly as
/// it does for the model directory. `RECORD` carries the installer's own
/// sha256 for every file of the distribution, so digesting it is a
/// bounded content address for the whole installed distribution;
/// `files_mismatched`/`files_missing` are the backend's count of files
/// whose bytes on disk no longer match that record, which is what makes
/// `RECORD` a measurement rather than only a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionIdentity {
    pub name: String,
    pub version: String,
    pub metadata_path: String,
    pub record_digest: String,
    pub metadata_digest: String,
    pub declared_files: u64,
    pub declared_byte_len: u64,
    pub files_checked: u64,
    pub files_missing: u64,
    pub files_mismatched: u64,
}

/// Where a module the backend actually loaded came from, and whether the
/// distribution it is attributed to actually declares that file.
///
/// This is the `W4-PRODUCER-PROVENANCE-CORRECTION.md` item 1/item 3
/// correction in one struct. `DistributionIdentity` above is the
/// *installer's* account of a distribution; this is the *interpreter's*
/// account of what ran. They are not the same claim, and the two ways
/// they come apart are both ordinary:
///
///  * a package earlier on `sys.path` than the installed one executes,
///    while `importlib.metadata` still attributes the name to the
///    installed distribution — whose `RECORD` then verifies clean, and
///    positively asserts `files_mismatched: 0` for code that did not run;
///  * two different edits to two different files of one distribution both
///    read `files_mismatched: 1`. A count is not the identity of the
///    changed bytes.
///
/// `digest` and `byte_len` are the *product's* reading of the file at
/// `path`, not the backend's claim about it; a disagreement refuses the
/// build, exactly as it does for `RECORD` and for the model directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleIdentity {
    /// The dotted module name as the interpreter held it.
    pub name: String,
    /// The origin the interpreter reported, verbatim.
    pub origin: String,
    /// That origin resolved by the product, which is the file the product
    /// opened and digested.
    pub path: String,
    pub digest: String,
    pub byte_len: u64,
    pub attribution: ModuleAttribution,
}

/// Whether the loaded file is one the distribution claiming it actually
/// declares.
///
/// Decided by the product, from the `RECORD` bytes it read and digested
/// itself — never from the backend's say-so, and never from a top-level
/// name. `Undeclared` is not an accusation and not an error: an editable
/// install, a `.pth` overlay and a deliberate local override all land
/// there. It is the record declining to assert a membership it did not
/// verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum ModuleAttribution {
    /// The loaded file is declared by this distribution's `RECORD`.
    Declared(String),
    /// No reported distribution's `RECORD` declares this file. The detail
    /// names the distributions that claimed the module by name, so the
    /// discrepancy is inspectable without a second scan.
    Undeclared(String),
}

/// Something the backend imported and could not describe, named with the
/// reason it could not.
///
/// `W4-PRODUCER-PROVENANCE-CORRECTION.md` item 2: a thing that was skipped
/// has to stay countable and, as far as a local boundary allows, nameable.
/// A bare count would say something was missing without saying which
/// dependency is unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableEntry {
    pub name: String,
    pub reason: String,
}

/// How much of what ran this record actually measured.
///
/// Deliberately three states, not a boolean. `Unmeasured` is what every
/// record written before this correction gets, and it must never read as
/// `Complete`: those builds measured installer metadata and never looked
/// at a loaded module, so claiming otherwise would mint a coverage they
/// never had (`W4-PRODUCER-PROVENANCE-CORRECTION.md`: do not retroactively
/// manufacture loaded-byte coverage).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum EnvironmentCoverage {
    /// No loaded module was measured: the backend reported no module
    /// list, reported an empty one, or reported one whose every entry it
    /// could not read. All three are the same epistemic state — the
    /// implementation bytes that ran are not part of this record — and
    /// the count is what decides it, never the presence of the key
    /// (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(a)).
    #[default]
    Unmeasured,
    /// Every distribution the run imported was described, every module
    /// attributed to one was read and digested, and every one of those
    /// files is declared by the `RECORD` of the distribution claiming it.
    Complete,
    /// Something in that chain is missing, and this says which.
    Partial(String),
}

/// The exact scope of what an environment record measures, written into
/// the record itself so a reader years later does not have to infer it
/// from the fields that happen to be present.
pub const ENVIRONMENT_SCOPE_V2: &str = "measured: the RECORD and METADATA bytes at each reported      .dist-info, re-read by the product; every module in the backend's process attributed by name      to one of those distributions, read and digested by the product at the origin the interpreter      reported, and checked for membership against the declaring RECORD. not measured: modules      belonging to no reported distribution (the standard library among them), files no RECORD      declares, and whether the backend's process is the one that produced these vectors — that      last is execution attestation, which no local argv boundary provides.";

/// The interpreter/runtime environment a backend says it embedded in,
/// bounded to what it actually imported.
///
/// This is *not* an execution attestation. A backend that lies about
/// which environment it ran in can produce a record that verifies; what
/// this closes is the far more ordinary case the original record could
/// not tell apart at all — two environments with different `model2vec`
/// builds reporting the same version string
/// (`W4-LIFECYCLE-CORRECTION.md` item 3). Every path in here is
/// re-measured by the product before it is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentity {
    /// The backend's own name for the scheme it reported under, e.g.
    /// `python-distributions/v1`. A scheme this product does not know is
    /// still recorded verbatim; nothing here is interpreted beyond the
    /// paths and digests the product re-reads itself.
    pub kind: String,
    /// The environment root the backend ran in (a virtual environment
    /// prefix, typically) — the thing that actually supplies the
    /// implementation, and which canonicalizing the interpreter loses.
    pub root: String,
    /// The runtime as it described itself: implementation and version.
    pub runtime: String,
    /// The executable the runtime reports for itself, which is the
    /// *configured* path rather than the canonical one whenever the
    /// configured path is what carries the environment.
    pub executable: String,
    pub distributions: Vec<DistributionIdentity>,
    /// Distributions the run imported and the backend could not describe,
    /// each named with its reason. Empty is a claim: nothing was skipped.
    #[serde(default)]
    pub undescribed_distributions: Vec<UnavailableEntry>,
    /// Every module actually loaded from one of the distributions above,
    /// with the product's own digest of the file the interpreter loaded.
    #[serde(default)]
    pub modules: Vec<ModuleIdentity>,
    /// Modules the interpreter could name no readable file for — builtin,
    /// frozen, namespace or archive-loaded. Named rather than dropped.
    #[serde(default)]
    pub unmeasured_modules: Vec<UnavailableEntry>,
    /// What this record measures and what it does not, in words, written
    /// into the record. Empty for records written before the scope was
    /// stated, which is itself the honest answer for those.
    #[serde(default)]
    pub scope: String,
    /// Derived from the four lists above, and never latched: a record with
    /// no module list is `Unmeasured`, not `Complete`.
    #[serde(default)]
    pub coverage: EnvironmentCoverage,
    /// One digest over every field above, length-prefixed. It is this
    /// value, not the list, that the edition id absorbs.
    pub digest: String,
}

/// Provenance for the backend *implementation* bytes.
///
/// Deliberately an enum rather than an optional field: honest absence has
/// to stay distinguishable from verified presence, and it has to stay
/// distinguishable in a record read years later
/// (`W4-LIFECYCLE-CORRECTION.md` item 3). A conforming backend that
/// cannot enumerate its own environment — a compiled binary, say — is
/// still a legal backend at this argv boundary; its editions simply say
/// so instead of implying provenance they do not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum BackendEnvironment {
    /// The backend reported nothing. Everything below the interpreter is
    /// unmeasured, and this record says exactly that.
    #[default]
    Unreported,
    Reported(Box<EnvironmentIdentity>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendIdentity {
    pub protocol: String,
    /// The executable, plus every configured argument that named an
    /// existing file (an adapter script, typically). Each is digested:
    /// "which backend built this" must survive that file being edited.
    pub program: ConfiguredPath,
    pub arguments: Vec<ConfiguredPath>,
    /// Every argv token, in order, as executed — including the ones that
    /// name no file. `arguments` above remains the separate file-content
    /// binding; this is the configuration itself.
    #[serde(default)]
    pub argv: Vec<BackendArgument>,
    /// Free-form identity the backend states for itself, e.g. its library
    /// name and version. Recorded as the backend's own claim, never
    /// validated by the product — `environment` is the measured part.
    pub reported: String,
    #[serde(default)]
    pub environment: BackendEnvironment,
}

/// Where the embedded text came from, in the product's own terms: the
/// extraction edition and unitizer the *generation* already committed to.
/// The product never re-chunks; a different chunking is a different
/// generation, which is a different edition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkerIdentity {
    pub extractor_set: String,
    pub unitizer: String,
    /// The chunker that actually produced this edition's rows, and the
    /// silent inputs that determine its output.
    ///
    /// `extractor_set`/`unitizer` above describe the *generation*: what
    /// W3 committed. They do not determine a semantic row, because a
    /// semantic edition may group those units differently. Absent on
    /// every edition built before native chunking existed, which is
    /// exactly what `default` means here — such a record says "the rows
    /// are the generation's units" and stays readable as that.
    #[serde(default)]
    pub chunks: Option<NativeChunkerIdentity>,
}

/// The installed chunker as it actually is, not as a name.
///
/// `native-chunk-boundary-use/HANDOFF.md` §6.3: an identity that records
/// only a name and a version does not determine the output. Chunk size is
/// a module constant carrying an upstream `# TODO: make this
/// configurable`; boundaries come from parse trees, so a grammar bump
/// moves them; and the resolved language — derived from the path suffix,
/// not from the blob — changes both the count and the boundaries. Every
/// one of those is recorded, the implementation files by digest. The
/// resolved language is per resource and lives on the row, because it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeChunkerIdentity {
    /// The implementation and version, e.g. `semble/0.5.2`.
    pub implementation: String,
    /// The public entry point actually called.
    pub entry_point: String,
    /// The module constants that decide boundary size, verbatim.
    pub constants: String,
    /// The parser generation the boundaries came out of.
    pub parsers: String,
    /// The implementation files themselves, digested by the product.
    pub files: Vec<ConfiguredPath>,
    /// The parser shared libraries that actually produced the parse
    /// trees, by their bytes — or the honest statement that none is
    /// covered. `parsers` above is a version string, and a version does
    /// not pin the extracted library it names
    /// (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` O1). Absent on every
    /// edition built before this was measured, which is what `default`
    /// means here: unreported, never "none loaded".
    #[serde(default)]
    pub grammars: GrammarCoverage,
}

/// What an edition records about the parser shared libraries that decided
/// its boundaries.
///
/// Four states, and the empty case is deliberately *not* one of the
/// measured ones: D1(a)'s lesson applies here too, so "the loader had
/// loaded nothing" is its own state and can never be read as coverage of
/// something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum GrammarCoverage {
    /// The backend said nothing about grammar libraries. Every edition
    /// built before O1 reads as this, and nothing is minted for them.
    #[default]
    Unreported,
    /// The backend reported that no parser shared library was loaded, so
    /// every boundary came from the line chunker. Zero libraries, and
    /// zero claimed.
    NoneLoaded(String),
    /// The backend reported that its parsers come from a provider whose
    /// loaded files it cannot enumerate. Missing coverage, named.
    Unavailable(String),
    /// One or more libraries were reported and re-read by the product.
    Measured(Box<GrammarLibraries>),
}

/// The parser shared libraries a build actually loaded, as the product
/// re-measured them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrammarLibraries {
    /// The module that supplied the parsers, e.g.
    /// `semble_grammars.loader`.
    pub provider: String,
    /// The cache directory the loader extracted them into — the
    /// overridable location whose bytes no version pins.
    pub cache_root: String,
    /// What this record measures and what it does not, in words.
    pub scope: String,
    pub libraries: Vec<GrammarLibrary>,
    /// Libraries the backend loaded and could not read, and any reason
    /// the bundled archive's own declarations were unavailable. Empty is
    /// a claim: nothing was skipped.
    pub uncovered: Vec<UnavailableEntry>,
}

/// One loaded grammar library, digested by the product at the path the
/// loader loaded it from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrammarLibrary {
    /// The languages the bundled manifest maps onto this file.
    pub languages: Vec<String>,
    /// The file itself, canonicalized and digested by the product.
    pub file: ConfiguredPath,
    /// Whether the bundled archive manifest declares these exact bytes.
    ///
    /// Never assumed from the file merely existing: upstream's
    /// `extract_atomic` returns early without re-checking the sha256 of a
    /// destination that already exists, so a cached library can differ
    /// from what the archive declares and still be loaded.
    pub declaration: ModuleAttribution,
}

/// The scope of a grammar-library record, written into it.
pub const GRAMMAR_SCOPE_V1: &str = "measured: every parser shared library the backend's loader reports having loaded in this \
     build, re-read and digested by the product at the path it was loaded from, with the sha256 \
     the provider's bundled archive manifest declares for that file recorded beside it. not \
     measured: libraries no parser was asked for in this build, the archive the loader extracted \
     from, and whether the process that parsed is the one this record describes.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorManifest {
    pub format: String,
    pub file: String,
    pub rows: u64,
    pub dimensions: u64,
    pub byte_len: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingManifest {
    pub file: String,
    pub rows: u64,
    pub byte_len: u64,
    pub digest: String,
}

/// Who built this, when, and under what configuration — recorded because
/// 0089 forbids minting producer proof for vectors whose real producer is
/// unknown. A product build always knows its own; nothing here reconstructs
/// a producer for an artifact it did not create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerIdentity {
    pub producer: String,
    pub built_at_unix_millis: u128,
}

/// One row of `mapping.ndjson`: vector row *n* embedded exactly these
/// committed bytes.
///
/// Deliberately self-identifying down to the estate. 0078 rejected
/// file/line pointers as chunk identity, and the control adjudication
/// rejected keying anything by path alone across sources; a row that
/// carries its own estate/membership/source/generation/object cannot be
/// silently reinterpreted against a different source that happens to hold
/// the same path. `content_digest` is over the exact byte range that was
/// handed to the backend, so altering the text alters the mapping digest
/// and therefore the edition id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingRow {
    pub row: u64,
    pub estate: EstateScope,
    pub membership: MembershipId,
    pub source: SourceId,
    pub generation: GenerationId,
    /// The *first* generation unit this row's byte range touches. Under
    /// the one-unit-per-row editions this field was the only unit, and a
    /// record written then still reads back meaning exactly that.
    pub unit: UnitId,
    /// The last unit the range touches; absent means the range lies
    /// inside `unit` alone. Two ids, never a list: `validate_generation`
    /// pins the generation's units as a contiguous gapless partition of
    /// the blob, so the interior of the run is derivable and storing it
    /// would be storing a duplicate.
    ///
    /// The run is a *covering superset*, never an equality claim: 80 % of
    /// native chunks begin or end mid-line
    /// (`native-chunk-boundary-use/HANDOFF.md` N2), so evidence recovery
    /// reads `byte_start`/`byte_end` and never the unit run.
    #[serde(default)]
    pub unit_last: Option<UnitId>,
    pub path: Vec<u8>,
    pub object_id: String,
    pub byte_start: u64,
    pub byte_end: u64,
    /// Display coordinates, computed from the *original* blob bytes.
    /// Adjacent rows may overlap on a line and a row may begin in the
    /// middle of one; they are never a way to reconstruct the text.
    pub line_start: u64,
    pub line_end: u64,
    pub byte_len: u64,
    /// Digest of the committed bytes at `[byte_start, byte_end)`. This is
    /// what the 0078 re-derivation control reads, and its meaning is
    /// unchanged from the editions that only ever had this field.
    pub content_digest: String,
    /// Digest of the text that was actually embedded and is actually
    /// ranked. Equal to `content_digest` whenever `text_normalization` is
    /// `identity`; different, legitimately, when the native reader's
    /// universal-newline translation or lossy decode stands between the
    /// committed bytes and the ranked string. Keeping one digest for both
    /// would either forge the evidence digest or report every CRLF row as
    /// corrupt.
    #[serde(default)]
    pub text_digest: Option<String>,
    #[serde(default)]
    pub text_normalization: Option<String>,
    /// The language the chunker resolved for this resource, from the
    /// ranking path's suffix — an input to the boundaries, so an input to
    /// the identity. `None` is the honest answer for a suffix the
    /// implementation maps to nothing.
    #[serde(default)]
    pub language: Option<String>,
    /// The exact path string handed to the native ranker, under
    /// `RANKING_PATH_CONVENTION`. Recorded per row because it is indexed
    /// text, not decoration.
    #[serde(default)]
    pub ranking_path: Option<String>,
    /// This row's slot within its own resource, which is what the native
    /// document key is built from.
    #[serde(default)]
    pub slot: Option<u64>,
    /// The content family of the resource this row came from, so family
    /// admission can be applied *before* any ranking statistic is
    /// computed rather than by filtering a finished ranking.
    #[serde(default)]
    pub family: Option<crate::ContentFamily>,
}

impl MappingRow {
    /// The digest of the committed bytes this row addresses.
    pub fn source_digest(&self) -> &str {
        &self.content_digest
    }

    /// The digest of the text that was embedded. Falls back to the
    /// evidence digest for the editions where the two were the same
    /// thing by construction.
    pub fn ranking_text_digest(&self) -> &str {
        self.text_digest.as_deref().unwrap_or(&self.content_digest)
    }

    pub fn normalization(&self) -> &str {
        self.text_normalization.as_deref().unwrap_or(TEXT_IDENTITY)
    }
}

/// The identity scheme this product writes. `v1` is W4 A's first
/// record, which bound neither the complete argv nor any backend
/// environment; a `v1` record stays readable and stays honestly labelled
/// as `v1` rather than being recomputed into a scheme it was never built
/// under (`W4-LIFECYCLE-CORRECTION.md`: historical readability with no
/// invented producer proof).
///
/// `v3` is this correction. It differs from `v2` in one place only — the
/// environment digest it absorbs is `wirk-backend-environment/v2`, which
/// covers the modules that actually loaded and the coverage of that
/// measurement. The scheme name is bumped so that a reader can tell which
/// question a record answered, and so that a `v2` record cannot be read as
/// a weaker claim of the same kind: `v2` measured installer metadata and
/// never looked at a loaded module, and stays labelled as exactly that.
/// Nothing recomputes: an existing `v1` or `v2` record verifies against
/// the environment digest it already stores, which is why a model or
/// scheme change never rewrites a prior edition's bytes.
/// What an edition's rows *are*, and therefore what a retrieval over them
/// ranks. Explicit, because the two are genuinely different artifacts and
/// a reader must be able to tell which one they have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SemanticChunking {
    /// One row per generation unit: the W3 one-line extraction, which is
    /// what every edition before native chunking contains.
    #[default]
    Units,
    /// One row per native chunk: meaningful multi-line spans produced by
    /// the installed chunker over the same unchanged generation.
    Native,
}

impl SemanticChunking {
    pub fn label(self) -> &'static str {
        match self {
            Self::Units => "units",
            Self::Native => "native",
        }
    }
}

/// How this edition's rows are ranked, fixed at build time and disclosed.
///
/// `VectorManifest` binds the vector *file*; that is not a retrieval
/// representation. The same vectors ranked under a different sparse text
/// rule, a different path convention or a different fusion are a different
/// retrieval, and a continuation that silently crossed between them would
/// be handing a caller a second answer while calling it the first one's
/// next page. Every field here is read from the installed implementation
/// rather than asserted about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalIdentity {
    pub scheme: String,
    /// Which rows this edition holds.
    pub chunking: SemanticChunking,
    /// The native implementation that will rank them.
    pub native: String,
    /// Dense side: metric, normalisation, and which text was embedded.
    pub dense: String,
    /// Sparse side: the synthesised document text and its tokenizer.
    /// This is not the evidence text — it appends path components that
    /// exist in no source file.
    pub sparse: String,
    /// The frozen ranking path convention.
    pub path_convention: String,
    /// Fusion and re-ranking, named as the installed implementation does.
    pub fusion: String,
    /// How this edition's rows have their result capacity decided
    /// (`CAPACITY_POLICY`), and the operational bound that policy runs
    /// under. The *policy* is edition data; one query's `K` is not, and
    /// travels on the answer and the continuation instead.
    #[serde(default)]
    pub capacity_policy: String,
    #[serde(default)]
    pub capacity_max: u64,
    /// Historical only, never written now: an edition built under the
    /// previous universal-depth policy recorded its one frozen candidate
    /// depth here. Read so that such an edition's own bytes stay readable
    /// and its declaration can be stated back to a caller when it is
    /// refused — never to reinterpret it under this policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_limit: Option<u64>,
    /// One digest over every field above, length-prefixed. It is this
    /// value that an edition id absorbs and a continuation pins.
    pub digest: String,
}

impl RetrievalIdentity {
    pub(crate) fn new(chunking: SemanticChunking, native: &str) -> Self {
        let mut identity = Self {
            scheme: RETRIEVAL_SCHEME.into(),
            chunking,
            native: native.to_owned(),
            dense: "cosine over unnormalised stored vectors with the query side normalised \
                    (vicinity CosineBasicBackend via semble.index.dense.SelectableBasicBackend); \
                    embedded text is the row's ranking text verbatim"
                .into(),
            sparse: "semble.index.sparse.enrich_for_bm25: \
                     `<text> <stem> <stem> <last three directory components>` of the \
                     source-relative ranking path, tokenised by semble.tokens.tokenize, indexed \
                     by semble.index.bm25.BM25 under the document key \
                     semble.index.types.make_chunk_id(<membership scope> NUL <ranking path>, \
                     <slot>), which is an identity and is never tokenised"
                .into(),
            path_convention: RANKING_PATH_CONVENTION.into(),
            fusion: "semble.search.search: reciprocal rank fusion k=60 with alpha from \
                     semble.ranking.resolve_alpha, then boost_multi_chunk_files, \
                     apply_query_boost and rerank_topk path penalties"
                .into(),
            capacity_policy: CAPACITY_POLICY.into(),
            capacity_max: CAPACITY_MAX,
            candidate_limit: None,
            digest: String::new(),
        };
        let mut hasher = Sha256::new();
        absorb(&mut hasher, b"wirk-retrieval/v1");
        for part in [
            identity.scheme.as_bytes(),
            identity.chunking.label().as_bytes(),
            identity.native.as_bytes(),
            identity.dense.as_bytes(),
            identity.sparse.as_bytes(),
            identity.path_convention.as_bytes(),
            identity.fusion.as_bytes(),
            identity.capacity_policy.as_bytes(),
        ] {
            absorb(&mut hasher, part);
        }
        absorb(&mut hasher, &identity.capacity_max.to_be_bytes());
        identity.digest = hex(&hasher.finalize());
        identity
    }
}

/// What an edition's rows actually cover of the generation they describe,
/// and what they honestly do not.
///
/// Native chunks are not a partition: inter-node whitespace falls between
/// them, and a whitespace-only resource yields no chunk at all. An
/// edition that claimed to tile every blob would be asserting something
/// the installed chunker does not do
/// (`native-chunk-boundary-use/HANDOFF.md` N3/N5), so the covered byte
/// count is recorded beside the indexed byte count instead, and the
/// resources that produced nothing are named rather than counted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EditionCoverage {
    pub resources_indexed: u64,
    pub resources_with_rows: u64,
    pub indexed_bytes: u64,
    pub covered_bytes: u64,
    /// Indexed resources the chunker returned no row for, each with the
    /// reason. Not an error and not a missing resource.
    #[serde(default)]
    pub resources_without_rows: Vec<UnavailableEntry>,
    /// Indexed resources whose committed bytes could not be mapped
    /// exactly to a ranking text, named with why. This is the "name the
    /// exact unsupported coverage honestly" slot; it is deliberately not
    /// a silent omission and deliberately not a build failure, and it is
    /// frozen at build time rather than chosen later.
    #[serde(default)]
    pub resources_unmapped: Vec<UnavailableEntry>,
}

pub const IDENTITY_V1: &str = "wirk-semantic-edition/v1";
pub const IDENTITY_V2: &str = "wirk-semantic-edition/v2";
pub const IDENTITY_V3: &str = "wirk-semantic-edition/v3";
/// `v4` adds the three things a retrieval needs and `v3` never bound: the
/// chunker that produced the rows, the ranking representation those rows
/// are ranked under, and the honest coverage of what the rows do and do
/// not address. As with every previous bump nothing is recomputed — a
/// `v1`, `v2` or `v3` record verifies against the scheme it was written
/// under and stays labelled as that.
pub const IDENTITY_V4: &str = "wirk-semantic-edition/v4";
/// `v5` binds the parser shared libraries that actually produced the
/// parse trees. `v4` bound `parsers` — a provider and a version string —
/// and a version does not pin the library the provider extracted into a
/// cache and loaded: those bytes can change under an unchanged version
/// and move every boundary while the recorded identity stands still
/// (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` O1). As with every previous
/// bump nothing is recomputed: a `v4` record verifies under `v4` and
/// stays labelled as that, and no historical edition gains coverage it
/// never had.
pub const IDENTITY_V5: &str = "wirk-semantic-edition/v5";

fn identity_v1() -> String {
    IDENTITY_V1.to_owned()
}

/// The immutable public identity of one semantic edition. Its `id` is a
/// digest over every other field, so two editions agree only when their
/// actual outputs, actual inputs and actual producer configuration all do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticEdition {
    pub id: EditionId,
    /// Which identity scheme `id` was computed under. Absent in a `v1`
    /// record, which is exactly what `default` means here.
    #[serde(default = "identity_v1")]
    pub identity: String,
    pub estate: EstateScope,
    pub membership: MembershipId,
    pub source: SourceId,
    pub generation: GenerationId,
    pub generation_revision: String,
    pub generation_content: String,
    pub acquisition_policy: String,
    pub chunker: ChunkerIdentity,
    pub model: ModelIdentity,
    pub backend: BackendIdentity,
    pub vectors: VectorManifest,
    pub mapping: MappingManifest,
    pub producer: ProducerIdentity,
    /// How these rows are ranked. Absent on every edition built before
    /// retrieval existed; such an edition binds no ranking representation
    /// at all, and a query refuses to invent one for it rather than
    /// ranking it under whatever this build happens to do today.
    #[serde(default)]
    pub retrieval: Option<RetrievalIdentity>,
    #[serde(default)]
    pub coverage: EditionCoverage,
}

#[derive(Debug, Clone)]
pub struct SemanticBuildConfig {
    pub backend: PathBuf,
    pub backend_args: Vec<String>,
    pub model: PathBuf,
    pub producer: String,
    /// What a row is. The caller's explicit choice, never inferred from
    /// the corpus: a native-chunk edition and a unit edition over the
    /// same generation are different artifacts with different identities,
    /// and which one an estate wants is not something a build may decide
    /// on its behalf.
    pub chunking: SemanticChunking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticBuildOutcome {
    Staged(Box<SemanticEdition>),
    /// A truthful refusal of a configuration or backend fact — absent
    /// model, absent backend, a backend that loaded something else, a
    /// generation with nothing admissible to embed. Distinct from
    /// `AtlasError`, which is this store failing at its own job.
    Refused(String),
}

/// What is actually true of an edition's bytes on disk *right now* —
/// recomputed, never remembered. `Verified` means every digest in the
/// record was reproduced from the bytes present; it does not mean anything
/// was searched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum SemanticVerification {
    Verified,
    /// A file the record names is not present at all.
    Missing(String),
    /// Present, but its bytes are not the bytes the record commits to, or
    /// the record itself is internally inconsistent.
    Corrupt(String),
    /// Present and self-consistent, but something it depends on cannot be
    /// read right now (the source repository, for instance).
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionState {
    pub edition: SemanticEdition,
    pub selected: bool,
    pub verification: SemanticVerification,
    /// Whether this edition describes the generation its source
    /// *currently* publishes. An edition over a superseded generation is
    /// retained historical evidence and stays exactly where it is; it is
    /// simply no longer a description of the public source record.
    pub current: bool,
}

/// Whether the *selected* edition can honestly be advertised as this
/// source's current semantic artifact.
///
/// `select_semantic` enforces "the public semantic record must describe
/// the public source record" at the moment of selection, and W4 A's first
/// record then let that lapse: republishing the source left a selection
/// publicly reported as `available true`, over bytes no reader could
/// reach (`W4-LIFECYCLE-CORRECTION.md` item 1, watched failing in
/// `raw/10-red.log`). Availability is therefore *derived* on every read
/// and never latched. Two consequences the correction brief names
/// specifically: nothing is erased to hide staleness — the selection, the
/// edition directory and every retained edition survive untouched — and
/// publishing back to the earlier generation makes the same selection
/// available again, because the answer was never stored in the first
/// place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum SemanticAvailability {
    /// This source has never selected an edition.
    None,
    /// A selection is recorded, but its edition record cannot be read.
    Unreadable(String),
    /// The selected edition's own bytes do not verify right now.
    Unusable(String),
    /// The selected edition verifies, but describes a source generation
    /// this membership does not currently publish. Retained evidence,
    /// not a current artifact.
    Superseded(String),
    /// Selected, verified, and over the currently published generation.
    Available,
}

impl SemanticAvailability {
    /// The single question every public surface must answer the same way.
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    /// `None` when nothing is selected at all — the distinction between
    /// "no selection" and "a selection that is not usable" is not one a
    /// bare boolean can carry.
    pub fn selected_available(&self) -> Option<bool> {
        match self {
            Self::None => None,
            other => Some(other.is_available()),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unreadable(_) => "unreadable",
            Self::Unusable(_) => "unusable",
            Self::Superseded(_) => "superseded",
            Self::Available => "available",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::None | Self::Available => None,
            Self::Unreadable(detail) | Self::Unusable(detail) | Self::Superseded(detail) => {
                Some(detail)
            }
        }
    }
}

// ---- identity ------------------------------------------------------------

fn absorb(hasher: &mut Sha256, part: &[u8]) {
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}

/// Absorb a grammar record, states distinguished by their own domain
/// strings so "nothing was loaded", "nothing can be enumerated" and
/// "these exact libraries" can never digest alike.
fn absorb_grammars(hasher: &mut Sha256, grammars: &GrammarCoverage) {
    match grammars {
        GrammarCoverage::Unreported => absorb(hasher, b"grammars-unreported"),
        GrammarCoverage::NoneLoaded(reason) => {
            absorb(hasher, b"grammars-none-loaded");
            absorb(hasher, reason.as_bytes());
        }
        GrammarCoverage::Unavailable(reason) => {
            absorb(hasher, b"grammars-unavailable");
            absorb(hasher, reason.as_bytes());
        }
        GrammarCoverage::Measured(measured) => {
            absorb(hasher, b"grammars-measured");
            absorb(hasher, measured.provider.as_bytes());
            absorb(hasher, measured.scope.as_bytes());
            absorb(hasher, &(measured.libraries.len() as u64).to_be_bytes());
            for library in &measured.libraries {
                absorb(hasher, library.file.canonical.as_bytes());
                absorb(hasher, library.file.digest.as_bytes());
                match &library.declaration {
                    ModuleAttribution::Declared(detail) => {
                        absorb(hasher, b"declared");
                        absorb(hasher, detail.as_bytes());
                    }
                    ModuleAttribution::Undeclared(detail) => {
                        absorb(hasher, b"undeclared");
                        absorb(hasher, detail.as_bytes());
                    }
                }
            }
            absorb(hasher, &(measured.uncovered.len() as u64).to_be_bytes());
            for entry in &measured.uncovered {
                absorb(hasher, entry.name.as_bytes());
                absorb(hasher, entry.reason.as_bytes());
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// The scheme a *query-time* producer identity is digested under.
///
/// Deliberately its own scheme rather than the edition's: an edition id
/// is the identity of stored bytes and must never move, while this is the
/// identity of the implementation that ranked one answer, computed fresh
/// on every query and pinned into that answer's own continuation.
///
/// `v2` because the identity now covers `QUERY_ORDERING_POLICY` as well
/// as the backend's own bytes (0163). The scheme is versioned rather than
/// left alone so that a digest computed here can never be read as if it
/// had been computed under the older, narrower rule.
pub const QUERY_PRODUCER_SCHEME: &str = "wirk-query-producer/v2";

/// The ordering and runtime policy this build *actually applies* to a
/// semantic answer, in the words the digest absorbs and a refusal prints.
///
/// Two halves, and both of them are the product's own decision rather
/// than the backend's:
///
/// * `selection` — the child is started with a fixed `PYTHONHASHSEED`, so
///   the native ranker's own candidate set iterates in one order and the
///   rows it selects at its `top_k` cut are the same rows in every
///   process. Without it the cut is drawn through whatever order that
///   process's randomly seeded hashing produced, and two pages of one
///   walk are slices of two different candidate pools
///   (`knowledge/rulings/0163`, measured at a real equal-score boundary).
/// * `order` — the total order this crate then imposes on the pool before
///   any page is cut from it (`query::order_ranked`).
///
/// Neither is visible in the backend's bytes: the same executable, the
/// same arguments and the same loaded modules rank differently under a
/// different policy. So a continuation whose first page was produced
/// under another policy cannot be reproduced under this one, and the
/// producer *configuration* digest — the half that is checked before a
/// child is started — absorbs this string to say so.
pub const QUERY_ORDERING_POLICY: &str = "selection:PYTHONHASHSEED=0 \
     order:score-desc,membership,path,byte-start";

/// The value `QUERY_ORDERING_POLICY`'s `selection` half names, in the
/// form the child's environment takes it. Kept beside the policy string
/// and pinned to it by a test, so the environment a query actually runs
/// under and the policy its continuation is digested under cannot drift
/// apart.
pub const QUERY_HASH_SEED: &str = "0";

/// What a query producer identity measures, and what it does not, written
/// into the answer so a reader does not have to infer it from the fields
/// that happen to be present.
///
/// The build side's `ENVIRONMENT_SCOPE_V2` states the same bound for the
/// environment half; this states the bound of the whole record. It is the
/// honest edge of "ranked by semble/0.5.2": the product measures the file
/// it executed, the tokens it executed it with, and — when the backend
/// reports them — the modules that loaded in the process that answered.
/// It does not, and at a local argv boundary cannot, attest that the
/// process which returned these scores is the one it described.
pub const QUERY_PRODUCER_SCOPE: &str = "measured: the executable this query ran, canonicalized and \
     digested by the product; every argv token in its executed position, with the bytes of each \
     token that names an existing absolute file; and, when the backend reports one, the \
     environment record measured under the build side's own rules. not measured: anything the \
     backend does not report, and whether the process that returned these scores is the one this \
     record describes — that is execution attestation, which no local argv boundary provides.";

/// How much of the ranking implementation a producer record actually
/// binds, which is not the same question as whether its coverage is
/// honest.
///
/// `public-retrieval-identity-verify/VERDICT.md` V1, executed: a backend
/// that reports no environment is a legal, useful backend, and
/// `environment: unreported` is a truthful thing for its answer to say.
/// What that answer's *continuation* cannot do is tell the second page
/// that the ranker changed underneath it: the executable at the
/// configured path and every argv token are byte-identical, and the
/// change lives in a module that loaded inside the process. So the two
/// digests still match and a re-ranked page is served under the first
/// page's token.
///
/// The distinction is therefore recorded rather than inferred, published
/// on the answer, and pinned into the token — because it is a property of
/// the page-1 measurement, and only page 1 can state it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryProducerBasis {
    /// The record covers the bytes of at least one module file the
    /// backend reported loading in the process that ranked, each read and
    /// digested by the product. A same-path change to a *reported* module
    /// moves `identity`. It is a statement about the reported scope and
    /// never about the whole process: a module the backend did not report
    /// is outside the record, and so is anything that is not a module at
    /// all — a native shared library among them
    /// (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(b), the 0104 limit
    /// restated rather than repaired).
    ImplementationMeasured,
    /// The record covers the configured executable and its argv, and
    /// nothing below them: the backend reported no environment at all, or
    /// reported one that measured no module — no list, an empty list, or
    /// a list whose entries could none of them be read. Honest, usable,
    /// and not a basis a continuation can be checked against.
    ConfigurationOnly,
}

impl QueryProducerBasis {
    pub fn label(self) -> &'static str {
        match self {
            Self::ImplementationMeasured => "implementation_measured",
            Self::ConfigurationOnly => "configuration_only",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "implementation_measured" => Some(Self::ImplementationMeasured),
            "configuration_only" => Some(Self::ConfigurationOnly),
            _ => None,
        }
    }
}

/// What is missing when the basis is `ConfigurationOnly`, in the words a
/// refused continuation gives its caller. Deliberately names the actual
/// missing basis rather than the state name alone.
pub const QUERY_PRODUCER_BASIS_MISSING: &str = "the first page's query producer record measured the configured executable and its argv \
     only: the backend reported no environment, or reported one that measured no module at all \
     (no list, an empty list, or a list whose entries could not be read), so nothing in the pin \
     covers the implementation bytes that ran inside it. A module changed at the same path \
     inside the same process would leave both digests identical, so this continuation cannot be \
     checked against the ranking that produced its first page";

/// What an `ImplementationMeasured` basis actually claims, in the words
/// both public surfaces print.
///
/// `EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(b): the sentence this
/// replaces said "the loaded-module bytes of the process that ranked this
/// answer were measured", which claims the process, not the report. A
/// backend that honestly narrows its list to modules it did measure —
/// omitting the very file that ranks — publishes a true list and a false
/// sentence, and its continuation cannot see a change in what it omitted.
/// That limit is 0104's reported-scope bound; it is stated here, not
/// repaired by prose, and no whitelist, execution attestation or
/// complete-dependency claim follows from it.
pub const QUERY_PRODUCER_BASIS_MEASURED: &str = "the pin covers the module files this backend reported: the product read and digested each \
     one, so a change to any of them is detected before a continuation is served. It does not \
     cover the rest of the process — a module the backend did not report, and anything that is \
     not a module at all, such as a native shared library — and a change to one of those cannot \
     be detected here";

/// The two digests a semantic answer publishes and its continuation pins,
/// and the basis they were measured on.
///
/// Two digests, not one, and the split is the whole point: `configuration`
/// is the half the product can measure *before* it starts a child, so a
/// continuation whose backend file has changed underneath it is refused
/// without ranking anything at all; `identity` additionally covers what
/// the backend reported about its own loaded modules, which only exists
/// once the child has answered. A page is served only when both match —
/// and only when `basis` says the second digest had something below the
/// argv line to cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryProducerPin {
    pub configuration: String,
    pub identity: String,
    /// Derived from the same `environment` the `identity` digest already
    /// absorbs, so it adds no new measurement and is not absorbed again;
    /// it is carried because a continuation has only the token, and the
    /// token has to be able to say what its digests are worth.
    pub basis: QueryProducerBasis,
}

/// The basis a measured environment supplies.
///
/// `Unreported` and a reported record whose coverage is `Unmeasured` are
/// different honest statements about the same gap — no loaded-module
/// bytes were measured — and both yield `ConfigurationOnly`. `Partial` is
/// *not* in that class: its modules were read and digested by the product,
/// and only their distribution attribution is incomplete
/// (`QUERY-IDENTITY-REVIEW-ADJUDICATION.md`: partial attribution is not
/// unmeasured implementation).
pub(crate) fn producer_basis(environment: &BackendEnvironment) -> QueryProducerBasis {
    match environment {
        BackendEnvironment::Unreported => QueryProducerBasis::ConfigurationOnly,
        // The count is asked first, and asked of the list itself rather
        // than of the coverage state derived from it. A record that
        // measured no module bytes cannot be a basis whatever it is
        // labelled — including a historical record whose stored coverage
        // says `Complete` over an empty list, which stays exactly as it
        // was written and simply stops minting an assurance here
        // (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(a)).
        BackendEnvironment::Reported(identity) if identity.modules.is_empty() => {
            QueryProducerBasis::ConfigurationOnly
        }
        BackendEnvironment::Reported(identity) => match identity.coverage {
            EnvironmentCoverage::Unmeasured => QueryProducerBasis::ConfigurationOnly,
            EnvironmentCoverage::Complete | EnvironmentCoverage::Partial(_) => {
                QueryProducerBasis::ImplementationMeasured
            }
        },
    }
}

/// The half of a query producer identity that is measurable before the
/// child runs: the executable's own bytes and every argv token, each
/// absorbed separately so `["--alpha", "0.2"]` can never digest as
/// `["--alpha0.2"]`.
///
/// R2: this is `EditionId::compute`'s argv absorption, over the query
/// side's own configuration, under its own scheme name.
pub(crate) fn query_producer_configuration_digest(
    program: &ConfiguredPath,
    argv: &[BackendArgument],
) -> String {
    let mut hasher = Sha256::new();
    absorb(&mut hasher, QUERY_PRODUCER_SCHEME.as_bytes());
    absorb(&mut hasher, b"configuration");
    absorb(&mut hasher, QUERY_PROTOCOL.as_bytes());
    // The effective policy, absorbed beside the bytes it governs: the
    // backend can be byte-identical and still rank a different list.
    absorb(&mut hasher, QUERY_ORDERING_POLICY.as_bytes());
    absorb(&mut hasher, program.configured.as_bytes());
    absorb(&mut hasher, program.canonical.as_bytes());
    absorb(&mut hasher, program.digest.as_bytes());
    absorb(&mut hasher, &program.byte_len.to_be_bytes());
    absorb(&mut hasher, &(argv.len() as u64).to_be_bytes());
    for argument in argv {
        match argument {
            BackendArgument::Literal { value } => {
                absorb(&mut hasher, b"literal");
                absorb(&mut hasher, value.as_bytes());
            }
            BackendArgument::File { value, file } => {
                absorb(&mut hasher, b"file");
                absorb(&mut hasher, value.as_bytes());
                absorb(&mut hasher, file.canonical.as_bytes());
                absorb(&mut hasher, file.digest.as_bytes());
                absorb(&mut hasher, &file.byte_len.to_be_bytes());
            }
        }
    }
    hex(&hasher.finalize())
}

/// The whole query producer identity: the configuration above, the
/// backend's own claim about itself, and the measured environment record
/// with its coverage.
///
/// The environment enters by its digest, which `measure_environment`
/// already computed over every module path, byte digest, attribution and
/// coverage state it recorded — so a run whose loaded module bytes differ
/// digests differently even when every version string agrees, and a run
/// that reported no environment at all is `unreported` rather than
/// silently equal to a measured one.
pub(crate) fn query_producer_identity_digest(backend: &BackendIdentity) -> String {
    let mut hasher = Sha256::new();
    absorb(&mut hasher, QUERY_PRODUCER_SCHEME.as_bytes());
    absorb(&mut hasher, b"identity");
    absorb(
        &mut hasher,
        query_producer_configuration_digest(&backend.program, &backend.argv).as_bytes(),
    );
    absorb(&mut hasher, backend.protocol.as_bytes());
    absorb(&mut hasher, backend.reported.as_bytes());
    match &backend.environment {
        BackendEnvironment::Unreported => absorb(&mut hasher, b"environment-unreported"),
        BackendEnvironment::Reported(environment) => {
            absorb(&mut hasher, b"environment-reported");
            absorb(&mut hasher, environment.digest.as_bytes());
        }
    }
    hex(&hasher.finalize())
}

impl EditionId {
    /// Length-prefixed over every identity-bearing field, so no two
    /// different editions can collide by concatenation. Deliberately
    /// includes the *output* digests: `W4-PREPARATION-ADJUDICATION.md`
    /// and 0078 both refuse recipe equality as output identity.
    ///
    /// Computed under the scheme the record itself names. A `v1` record
    /// keeps the `v1` computation, so an edition built before the
    /// correction still reads back with its own id intact rather than
    /// being declared forged by a scheme it predates. This product only
    /// ever *writes* `v2`.
    fn compute(edition: &SemanticEdition) -> Self {
        let mut hasher = Sha256::new();
        absorb(&mut hasher, edition.identity.as_bytes());
        for part in [
            edition.estate.0.as_bytes(),
            edition.membership.0.as_bytes(),
            edition.source.0.as_bytes(),
            edition.generation.0.as_bytes(),
            edition.generation_revision.as_bytes(),
            edition.generation_content.as_bytes(),
            edition.acquisition_policy.as_bytes(),
            edition.chunker.extractor_set.as_bytes(),
            edition.chunker.unitizer.as_bytes(),
            edition.model.consumed.canonical.as_bytes(),
            edition.model.consumed.digest.as_bytes(),
            edition.model.reported_path.as_bytes(),
            edition.model.reported_digest.as_bytes(),
            edition.backend.protocol.as_bytes(),
            edition.backend.program.canonical.as_bytes(),
            edition.backend.program.digest.as_bytes(),
            edition.backend.reported.as_bytes(),
            edition.vectors.format.as_bytes(),
            edition.vectors.digest.as_bytes(),
            edition.mapping.digest.as_bytes(),
            edition.producer.producer.as_bytes(),
        ] {
            absorb(&mut hasher, part);
        }
        absorb(
            &mut hasher,
            &(edition.backend.arguments.len() as u64).to_be_bytes(),
        );
        for argument in &edition.backend.arguments {
            absorb(&mut hasher, argument.canonical.as_bytes());
            absorb(&mut hasher, argument.digest.as_bytes());
        }
        if edition.identity != IDENTITY_V1 {
            // The correction's own additions, absorbed only under the
            // scheme that declares them. Each argv token is absorbed
            // separately, and its kind with it, so a token boundary is
            // part of the identity rather than an artefact of joining.
            absorb(&mut hasher, edition.backend.program.configured.as_bytes());
            absorb(
                &mut hasher,
                &(edition.backend.argv.len() as u64).to_be_bytes(),
            );
            for argument in &edition.backend.argv {
                match argument {
                    BackendArgument::Literal { value } => {
                        absorb(&mut hasher, b"literal");
                        absorb(&mut hasher, value.as_bytes());
                    }
                    BackendArgument::File { value, file } => {
                        absorb(&mut hasher, b"file");
                        absorb(&mut hasher, value.as_bytes());
                        absorb(&mut hasher, file.canonical.as_bytes());
                        absorb(&mut hasher, file.digest.as_bytes());
                    }
                }
            }
            match &edition.backend.environment {
                BackendEnvironment::Unreported => absorb(&mut hasher, b"environment-unreported"),
                BackendEnvironment::Reported(environment) => {
                    absorb(&mut hasher, b"environment-reported");
                    absorb(&mut hasher, environment.digest.as_bytes());
                }
            }
        }
        if edition.identity == IDENTITY_V4 || edition.identity == IDENTITY_V5 {
            // The retrieval representation, the chunker that produced the
            // rows and the coverage they honestly claim. Absorbed only
            // under the scheme that declares them, so no earlier record's
            // id moves.
            match &edition.retrieval {
                None => absorb(&mut hasher, b"retrieval-unbound"),
                Some(retrieval) => {
                    absorb(&mut hasher, b"retrieval-bound");
                    absorb(&mut hasher, retrieval.digest.as_bytes());
                }
            }
            match &edition.chunker.chunks {
                None => absorb(&mut hasher, b"chunker-generation-units"),
                Some(chunks) => {
                    absorb(&mut hasher, b"chunker-native");
                    for part in [
                        chunks.implementation.as_bytes(),
                        chunks.entry_point.as_bytes(),
                        chunks.constants.as_bytes(),
                        chunks.parsers.as_bytes(),
                    ] {
                        absorb(&mut hasher, part);
                    }
                    absorb(&mut hasher, &(chunks.files.len() as u64).to_be_bytes());
                    for file in &chunks.files {
                        absorb(&mut hasher, file.canonical.as_bytes());
                        absorb(&mut hasher, file.digest.as_bytes());
                    }
                    if edition.identity == IDENTITY_V5 {
                        // Only under the scheme that declares it, so no
                        // `v4` id moves and no earlier edition is
                        // retroactively said to have covered a grammar.
                        absorb_grammars(&mut hasher, &chunks.grammars);
                    }
                }
            }
            let coverage = &edition.coverage;
            for number in [
                coverage.resources_indexed,
                coverage.resources_with_rows,
                coverage.indexed_bytes,
                coverage.covered_bytes,
            ] {
                absorb(&mut hasher, &number.to_be_bytes());
            }
            for list in [
                &coverage.resources_without_rows,
                &coverage.resources_unmapped,
            ] {
                absorb(&mut hasher, &(list.len() as u64).to_be_bytes());
                for entry in list {
                    absorb(&mut hasher, entry.name.as_bytes());
                    absorb(&mut hasher, entry.reason.as_bytes());
                }
            }
        }
        for number in [
            edition.vectors.rows,
            edition.vectors.dimensions,
            edition.vectors.byte_len,
            edition.mapping.rows,
            edition.mapping.byte_len,
        ] {
            absorb(&mut hasher, &number.to_be_bytes());
        }
        // Deliberately *not* the build's wall clock. An edition is a
        // content address over its actual inputs, actual outputs and
        // actual producer configuration; two builds that consumed the
        // same bytes with the same configuration and produced the same
        // bytes are the same edition, and rebuilding one must find the
        // immutable directory already there rather than mint a second
        // identity for identical content. The timestamp is recorded in
        // the record for provenance, outside the identity.
        Self(format!("e-{}", hex(&hasher.finalize())))
    }
}

pub(crate) fn valid_edition_id(id: &str) -> bool {
    id.len() == 66
        && id.starts_with("e-")
        && id[2..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

// ---- configured path canonicalization ------------------------------------

/// C1/C2/C3 of the accepted 0089 canonicalization rules, applied by the
/// product to its own configuration: a model or backend must be named by
/// an absolute path that resolves to something that exists. A bare model
/// name is refused explicitly — it resolves through a shared mutable cache
/// and therefore names no fixed bytes — rather than being handed onward to
/// a backend that would happily download it.
fn canonicalize(path: &Path, what: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{what} {} is not an absolute path: a relative path or a bare model name resolves \
             through a shared mutable cache and names no fixed bytes",
            path.display()
        ));
    }
    path.canonicalize()
        .map_err(|error| format!("{what} {} does not resolve: {error}", path.display()))
}

/// The query side's own canonicalization of the model it was configured
/// with, under exactly the rule the build side used (0089 C1/C2/C3).
pub(crate) fn configured_model(path: &Path) -> Result<ConfiguredPath, String> {
    configured_directory(path, "model")
}

pub(crate) fn configured_file(path: &Path, what: &str) -> Result<ConfiguredPath, String> {
    let canonical = canonicalize(path, what)?;
    let bytes = std::fs::read(&canonical)
        .map_err(|error| format!("{what} {} is unreadable: {error}", canonical.display()))?;
    Ok(ConfiguredPath {
        configured: path.display().to_string(),
        canonical: canonical.display().to_string(),
        digest: digest_bytes(&bytes),
        byte_len: bytes.len() as u64,
        file_count: 1,
    })
}

/// A directory digest over the bytes a loader would actually read:
/// every regular file reachable below `path`, keyed by its relative path,
/// in sorted order, length-prefixed. Symlinks are followed deliberately —
/// a Hugging Face snapshot directory is entirely symlinks into a blob
/// store, and digesting the links rather than the blobs would digest
/// nothing that any model loader consumes.
fn configured_directory(path: &Path, what: &str) -> Result<ConfiguredPath, String> {
    let canonical = canonicalize(path, what)?;
    if !canonical.is_dir() {
        return Err(format!("{what} {} is not a directory", canonical.display()));
    }
    let mut files: Vec<(Vec<u8>, PathBuf)> = Vec::new();
    collect_files(&canonical, &canonical, &mut files)
        .map_err(|error| format!("{what} {} is unreadable: {error}", canonical.display()))?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    absorb(&mut hasher, b"wirk-model-directory/v1");
    absorb(&mut hasher, &(files.len() as u64).to_be_bytes());
    let mut byte_len = 0u64;
    for (relative, absolute) in &files {
        let bytes = std::fs::read(absolute)
            .map_err(|error| format!("{what} {} is unreadable: {error}", absolute.display()))?;
        absorb(&mut hasher, relative);
        absorb(&mut hasher, &bytes);
        byte_len += bytes.len() as u64;
    }
    Ok(ConfiguredPath {
        configured: path.display().to_string(),
        canonical: canonical.display().to_string(),
        digest: hex(&hasher.finalize()),
        byte_len,
        file_count: files.len() as u64,
    })
}

fn collect_files(
    root: &Path,
    directory: &Path,
    out: &mut Vec<(Vec<u8>, PathBuf)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        // `metadata` follows symlinks; `file_type` would not.
        let metadata = std::fs::metadata(&path)?;
        if metadata.is_dir() {
            collect_files(root, &path, out)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string()
                .into_bytes();
            out.push((relative, path));
        }
    }
    Ok(())
}

/// The most distributions a backend may report. Bounded on purpose: this
/// is a provenance record over what the backend actually imported, not an
/// inventory of an installation, and an unbounded list from the child
/// would be an unbounded read on this side.
const MAX_REPORTED_DISTRIBUTIONS: usize = 256;

/// The most loaded modules a backend may report, and the most entries any
/// one `RECORD` may declare. Both bounded for the same reason as the
/// distributions: this is a record of one run's import closure, not a
/// filesystem scan, and an unbounded list from the child is an unbounded
/// read on this side.
/// Re-measure the parser shared libraries the backend reported loading.
///
/// The discipline is the chunker modules' discipline (R2): the product
/// reads the file at the path the backend named and refuses if the bytes
/// disagree with what was reported. What it does *not* do is refuse a
/// library whose bytes differ from the archive manifest's declared
/// sha256 — that difference is exactly the fact O1 is about, and an
/// edition that records it honestly is worth more than a build that
/// declines to exist. Nothing here verifies a cache by its existence.
fn measure_grammars(reported: Option<&ReportedGrammars>) -> Result<GrammarCoverage, String> {
    let Some(reported) = reported else {
        return Ok(GrammarCoverage::Unreported);
    };
    match reported.state.as_str() {
        "none_loaded" => Ok(GrammarCoverage::NoneLoaded(reported.reason.clone())),
        "unavailable" => Ok(GrammarCoverage::Unavailable(reported.reason.clone())),
        "measured" => {
            if reported.libraries.len() > MAX_REPORTED_MODULES {
                return Err(format!(
                    "backend reported {} grammar libraries; at most {MAX_REPORTED_MODULES} are \
                     recorded",
                    reported.libraries.len()
                ));
            }
            let mut libraries = Vec::new();
            for library in &reported.libraries {
                let file = configured_file(
                    Path::new(&library.path),
                    &format!("grammar library {}", library.path),
                )?;
                if file.digest != library.digest || file.byte_len != library.byte_len {
                    return Err(format!(
                        "backend reports grammar library {} as {} but its bytes digest to {}",
                        library.path, library.digest, file.digest
                    ));
                }
                let declaration = match &library.declared_digest {
                    Some(declared) if *declared == file.digest => ModuleAttribution::Declared(
                        format!("the provider's bundled archive manifest declares {declared}"),
                    ),
                    Some(declared) => ModuleAttribution::Undeclared(format!(
                        "the provider's bundled archive manifest declares {declared} for this \
                         file, but the library actually loaded digests to {}; the loader does \
                         not re-check a cached file that already exists",
                        file.digest
                    )),
                    None => ModuleAttribution::Undeclared(
                        "no entry in the provider's bundled archive manifest names this file, so \
                         nothing declares its bytes"
                            .into(),
                    ),
                };
                libraries.push(GrammarLibrary {
                    languages: {
                        let mut languages = library.languages.clone();
                        languages.sort();
                        languages
                    },
                    file,
                    declaration,
                });
            }
            libraries.sort_by(|a, b| a.file.canonical.cmp(&b.file.canonical));
            if libraries
                .windows(2)
                .any(|pair| pair[0].file.canonical == pair[1].file.canonical)
            {
                return Err("backend reported the same grammar library twice".into());
            }
            let mut uncovered: Vec<UnavailableEntry> = reported
                .uncovered
                .iter()
                .map(|entry| UnavailableEntry {
                    name: entry.name.clone(),
                    reason: entry.reason.clone(),
                })
                .collect();
            uncovered.sort_by(|a, b| (&a.name, &a.reason).cmp(&(&b.name, &b.reason)));
            if libraries.is_empty() && uncovered.is_empty() {
                return Err(
                    "backend reported measured grammar libraries but listed none; an empty list \
                     measures nothing and must be reported as such"
                        .into(),
                );
            }
            Ok(GrammarCoverage::Measured(Box::new(GrammarLibraries {
                provider: reported.provider.clone(),
                cache_root: reported.cache_root.clone(),
                scope: GRAMMAR_SCOPE_V1.to_owned(),
                libraries,
                uncovered,
            })))
        }
        other => Err(format!(
            "backend reported grammar coverage state {other:?}, which this product does not know"
        )),
    }
}

const MAX_REPORTED_MODULES: usize = 4096;
const MAX_RECORD_ENTRIES: usize = 65_536;

/// The first field of one `RECORD` line, which is the installed path the
/// entry declares, relative to the directory holding the `.dist-info`.
///
/// `RECORD` is RFC4180 CSV, and a path containing a comma is quoted, so
/// this cannot be a `split(',')`. Only the first field is needed here —
/// the hash and length are the backend's business, and the product uses
/// this solely to answer "does this distribution declare that file".
fn record_first_field(line: &str) -> Option<String> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(character) = chars.next() {
            if character != '"' {
                out.push(character);
            } else if chars.clone().next() == Some('"') {
                chars.next();
                out.push('"');
            } else {
                break;
            }
        }
        return (!out.is_empty()).then_some(out);
    }
    let field = line.split(',').next().unwrap_or_default();
    (!field.is_empty()).then(|| field.to_owned())
}

/// Every path a `RECORD` declares, resolved against the directory that
/// holds the distribution's `.dist-info` and normalised lexically.
///
/// Lexical, not `canonicalize`: this is a membership question over up to
/// a few thousand declared entries, most of which are never loaded, and
/// opening every one of them to answer it would turn a bounded record into
/// a filesystem walk. The loaded files themselves *are* opened, digested
/// and compared — that is the measurement; this is only the set they are
/// looked up in.
fn declared_paths(root: &Path, record: &str) -> BTreeSet<PathBuf> {
    let mut declared = BTreeSet::new();
    for line in record.lines().take(MAX_RECORD_ENTRIES) {
        let Some(relative) = record_first_field(line) else {
            continue;
        };
        let mut path = root.to_path_buf();
        for component in Path::new(&relative).components() {
            match component {
                std::path::Component::ParentDir => {
                    path.pop();
                }
                std::path::Component::CurDir => {}
                other => path.push(other.as_os_str()),
            }
        }
        declared.insert(path);
    }
    declared
}

/// Re-measure, on the product's own side, everything a backend said about
/// the environment it embedded in.
///
/// The pattern is the model's, deliberately: the backend names paths and
/// digests, the product opens the same paths and digests them itself, and
/// a disagreement refuses the build. What is *recorded* is the product's
/// reading. What this cannot do — and what the record must therefore not
/// imply — is prove the backend actually imported what it named; that is
/// execution attestation, which no local argv boundary provides.
pub(crate) fn measure_environment(
    reported: &ReportedEnvironment,
) -> Result<EnvironmentIdentity, String> {
    if reported.distributions.len() > MAX_REPORTED_DISTRIBUTIONS {
        return Err(format!(
            "backend reported {} distributions; at most {MAX_REPORTED_DISTRIBUTIONS} are recorded",
            reported.distributions.len()
        ));
    }
    let mut measured = Vec::with_capacity(reported.distributions.len());
    // Built from the same `RECORD` bytes the product just re-read and
    // digested, so module membership is decided against a verified
    // reading rather than against the backend's word for it.
    let mut declared_by: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    for distribution in &reported.distributions {
        let directory = canonicalize(
            Path::new(&distribution.metadata_path),
            &format!("backend distribution {}", distribution.name),
        )?;
        if !directory.is_dir() {
            return Err(format!(
                "backend distribution {} names {} which is not a directory",
                distribution.name,
                directory.display()
            ));
        }
        let mut digests = Vec::new();
        let mut record_text = String::new();
        for (file, claimed) in [
            ("RECORD", &distribution.record_digest),
            ("METADATA", &distribution.metadata_digest),
        ] {
            let path = directory.join(file);
            let bytes = std::fs::read(&path).map_err(|error| {
                format!(
                    "backend distribution {} names {} which is unreadable: {error}",
                    distribution.name,
                    path.display()
                )
            })?;
            let digest = digest_bytes(&bytes);
            if &digest != claimed {
                return Err(format!(
                    "backend reports {file} digest {claimed} for distribution {} but its bytes \
                     digest to {digest}",
                    distribution.name
                ));
            }
            if file == "RECORD" {
                record_text = String::from_utf8_lossy(&bytes).into_owned();
            }
            digests.push(digest);
        }
        // A wheel's `RECORD` paths are relative to the directory that
        // holds the `.dist-info`, which is the site-packages root.
        let root = directory
            .parent()
            .ok_or_else(|| {
                format!(
                    "backend distribution {} names a .dist-info with no parent directory",
                    distribution.name
                )
            })?
            .to_path_buf();
        declared_by.insert(
            distribution.name.clone(),
            declared_paths(&root, &record_text),
        );
        measured.push(DistributionIdentity {
            name: distribution.name.clone(),
            version: distribution.version.clone(),
            metadata_path: directory.display().to_string(),
            record_digest: digests[0].clone(),
            metadata_digest: digests[1].clone(),
            declared_files: distribution.declared_files,
            declared_byte_len: distribution.declared_byte_len,
            files_checked: distribution.files_checked,
            files_missing: distribution.files_missing,
            files_mismatched: distribution.files_mismatched,
        });
    }
    // Sorted and de-duplicated so the same environment digests the same
    // way whatever order the backend happened to walk it in.
    measured.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    if measured.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err("backend reported the same distribution twice".into());
    }

    // ---- the modules that actually loaded --------------------------------
    //
    // The distribution list above is the installer's account of what is
    // installed. This is the interpreter's account of what ran, and the
    // product measures it the same way it measures everything else it
    // records: it opens the file itself, digests it, and refuses the build
    // if its reading and the backend's disagree
    // (`W4-PRODUCER-PROVENANCE-CORRECTION.md` items 1 and 3).
    let mut modules = Vec::new();
    let mut undeclared = 0usize;
    if let Some(reported_modules) = &reported.modules {
        if reported_modules.len() > MAX_REPORTED_MODULES {
            return Err(format!(
                "backend reported {} loaded modules; at most {MAX_REPORTED_MODULES} are recorded",
                reported_modules.len()
            ));
        }
        for module in reported_modules {
            let path = canonicalize(
                Path::new(&module.path),
                &format!("backend module {}", module.name),
            )?;
            let bytes = std::fs::read(&path).map_err(|error| {
                format!(
                    "backend module {} names {} which is unreadable: {error}",
                    module.name,
                    path.display()
                )
            })?;
            let digest = digest_bytes(&bytes);
            if digest != module.digest {
                return Err(format!(
                    "backend reports digest {} for module {} loaded from {} but its bytes digest \
                     to {digest}",
                    module.digest,
                    module.name,
                    path.display()
                ));
            }
            if bytes.len() as u64 != module.byte_len {
                return Err(format!(
                    "backend reports {} bytes for module {} loaded from {} but it is {} bytes",
                    module.byte_len,
                    module.name,
                    path.display(),
                    bytes.len()
                ));
            }
            // Membership, verified — never assumed from the top-level
            // name. `claims` is what `importlib.metadata` said from the
            // name alone; a distribution earns the attribution only if its
            // own RECORD declares the file that actually loaded.
            let origin = Path::new(&module.origin);
            let attribution = module
                .claims
                .iter()
                .find(|name| {
                    declared_by.get(*name).is_some_and(|declared| {
                        declared.contains(&path) || declared.contains(origin)
                    })
                })
                .map(|name| ModuleAttribution::Declared(name.clone()))
                .unwrap_or_else(|| {
                    undeclared += 1;
                    ModuleAttribution::Undeclared(if module.claims.is_empty() {
                        format!(
                            "loaded from {}, which no reported distribution claims and no \
                             reported RECORD declares",
                            path.display()
                        )
                    } else {
                        format!(
                            "loaded from {}, which the RECORD of the claiming distribution{} {} \
                             does not declare",
                            path.display(),
                            if module.claims.len() == 1 { "" } else { "s" },
                            module.claims.join(", ")
                        )
                    })
                });
            modules.push(ModuleIdentity {
                name: module.name.clone(),
                origin: module.origin.clone(),
                path: path.display().to_string(),
                digest,
                byte_len: bytes.len() as u64,
                attribution,
            });
        }
        modules.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
        if modules.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err("backend reported the same module twice".into());
        }
    }

    let entries = |source: &[ReportedUnavailable]| {
        let mut out: Vec<UnavailableEntry> = source
            .iter()
            .map(|entry| UnavailableEntry {
                name: entry.name.clone(),
                reason: entry.reason.clone(),
            })
            .collect();
        out.sort_by(|a, b| (&a.name, &a.reason).cmp(&(&b.name, &b.reason)));
        out
    };
    let undescribed = entries(&reported.undescribed_distributions);
    let unmeasured = entries(&reported.unmeasured_modules);
    if undescribed.len() + unmeasured.len() > MAX_REPORTED_MODULES {
        return Err("backend reported more unavailable entries than are recorded".into());
    }

    // Coverage is derived here and stored, because it is a statement about
    // this measurement and cannot be recomputed later from a record that
    // did not carry the lists. It is never latched into `Complete` by
    // absence: a backend that reported no module list at all gets
    // `Unmeasured`, which is what every pre-correction record reads as.
    let coverage = if modules.is_empty() {
        // Absent, empty, and "present but nothing in it could be read"
        // are three ways of reporting the same measurement: none. A list
        // that names no module has measured no implementation byte, and
        // an unreadable-module list does not rescue it — the entries in
        // it are precisely what was *not* measured
        // (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(a)).
        EnvironmentCoverage::Unmeasured
    } else if undescribed.is_empty() && unmeasured.is_empty() && undeclared == 0 {
        EnvironmentCoverage::Complete
    } else {
        let mut parts = Vec::new();
        if !undescribed.is_empty() {
            parts.push(format!(
                "{} imported distribution{} could not be described ({})",
                undescribed.len(),
                if undescribed.len() == 1 { "" } else { "s" },
                undescribed
                    .iter()
                    .map(|entry| format!("{}: {}", entry.name, entry.reason))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if !unmeasured.is_empty() {
            parts.push(format!(
                "{} loaded module{} could not be read ({})",
                unmeasured.len(),
                if unmeasured.len() == 1 { "" } else { "s" },
                unmeasured
                    .iter()
                    .map(|entry| format!("{}: {}", entry.name, entry.reason))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if undeclared > 0 {
            // Grouped by top-level name: which dependency is unidentified
            // is the question, and 11 lines of one package is one answer.
            let mut by_package: BTreeMap<&str, usize> = BTreeMap::new();
            for module in &modules {
                if matches!(module.attribution, ModuleAttribution::Undeclared(_)) {
                    *by_package
                        .entry(module.name.split('.').next().unwrap_or(&module.name))
                        .or_default() += 1;
                }
            }
            parts.push(format!(
                "{undeclared} of {} loaded module{} declared by no reported RECORD ({})",
                modules.len(),
                if undeclared == 1 { " is" } else { "s are" },
                by_package
                    .iter()
                    .map(|(package, count)| format!("{package}: {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        EnvironmentCoverage::Partial(parts.join("; "))
    };

    // `wirk-backend-environment/v2`: the module list, the unavailable
    // entries, the scope and the coverage all enter the digest, so an
    // implementation byte that changed under an unchanged `RECORD` changes
    // the identity by *what changed* and not by a count of how many things
    // did. The domain string is bumped rather than extended, so a v1
    // digest can never be confused with a v2 one over the same fields.
    let scope = if reported.modules.is_some() {
        ENVIRONMENT_SCOPE_V2.to_owned()
    } else {
        String::new()
    };
    let mut hasher = Sha256::new();
    absorb(&mut hasher, b"wirk-backend-environment/v2");
    for part in [
        reported.kind.as_bytes(),
        reported.root.as_bytes(),
        reported.runtime.as_bytes(),
        reported.executable.as_bytes(),
        scope.as_bytes(),
    ] {
        absorb(&mut hasher, part);
    }
    absorb(&mut hasher, &(measured.len() as u64).to_be_bytes());
    for distribution in &measured {
        for part in [
            distribution.name.as_bytes(),
            distribution.version.as_bytes(),
            distribution.metadata_path.as_bytes(),
            distribution.record_digest.as_bytes(),
            distribution.metadata_digest.as_bytes(),
        ] {
            absorb(&mut hasher, part);
        }
        for number in [
            distribution.declared_files,
            distribution.declared_byte_len,
            distribution.files_checked,
            distribution.files_missing,
            distribution.files_mismatched,
        ] {
            absorb(&mut hasher, &number.to_be_bytes());
        }
    }
    absorb(&mut hasher, &(modules.len() as u64).to_be_bytes());
    for module in &modules {
        for part in [
            module.name.as_bytes(),
            module.origin.as_bytes(),
            module.path.as_bytes(),
            module.digest.as_bytes(),
        ] {
            absorb(&mut hasher, part);
        }
        absorb(&mut hasher, &module.byte_len.to_be_bytes());
        match &module.attribution {
            ModuleAttribution::Declared(name) => {
                absorb(&mut hasher, b"declared");
                absorb(&mut hasher, name.as_bytes());
            }
            ModuleAttribution::Undeclared(detail) => {
                absorb(&mut hasher, b"undeclared");
                absorb(&mut hasher, detail.as_bytes());
            }
        }
    }
    for list in [&undescribed, &unmeasured] {
        absorb(&mut hasher, &(list.len() as u64).to_be_bytes());
        for entry in list {
            absorb(&mut hasher, entry.name.as_bytes());
            absorb(&mut hasher, entry.reason.as_bytes());
        }
    }
    match &coverage {
        EnvironmentCoverage::Unmeasured => absorb(&mut hasher, b"coverage-unmeasured"),
        EnvironmentCoverage::Complete => absorb(&mut hasher, b"coverage-complete"),
        EnvironmentCoverage::Partial(detail) => {
            absorb(&mut hasher, b"coverage-partial");
            absorb(&mut hasher, detail.as_bytes());
        }
    }
    Ok(EnvironmentIdentity {
        kind: reported.kind.clone(),
        root: reported.root.clone(),
        runtime: reported.runtime.clone(),
        executable: reported.executable.clone(),
        distributions: measured,
        undescribed_distributions: undescribed,
        modules,
        unmeasured_modules: unmeasured,
        scope,
        coverage,
        digest: hex(&hasher.finalize()),
    })
}

// ---- the backend boundary ------------------------------------------------

/// What a backend says about the environment it embedded in. Every field
/// is re-measured by `measure_environment` before anything is recorded.
#[derive(Deserialize)]
pub(crate) struct ReportedEnvironment {
    kind: String,
    root: String,
    runtime: String,
    executable: String,
    distributions: Vec<ReportedDistribution>,
    #[serde(default)]
    undescribed_distributions: Vec<ReportedUnavailable>,
    /// `Option`, not a defaulted `Vec`: a backend that reported no module
    /// list at all is a different thing from one that reported an empty
    /// one, and only the first may record `Unmeasured` coverage. Keyed on
    /// the field's presence rather than on the scheme name, so a backend
    /// this product has never heard of is classified by what it actually
    /// sent.
    #[serde(default)]
    modules: Option<Vec<ReportedModule>>,
    #[serde(default)]
    unmeasured_modules: Vec<ReportedUnavailable>,
}

#[derive(Deserialize)]
struct ReportedUnavailable {
    name: String,
    reason: String,
}

#[derive(Deserialize)]
struct ReportedModule {
    name: String,
    origin: String,
    path: String,
    digest: String,
    byte_len: u64,
    claims: Vec<String>,
}

#[derive(Deserialize)]
struct ReportedDistribution {
    name: String,
    version: String,
    metadata_path: String,
    record_digest: String,
    metadata_digest: String,
    declared_files: u64,
    declared_byte_len: u64,
    files_checked: u64,
    files_missing: u64,
    files_mismatched: u64,
}

/// The ranking text of a committed byte range, re-derived by the product
/// itself.
///
/// This is the product's own implementation of what the native reader
/// does to a file before anything chunks or embeds it: UTF-8 with lossy
/// replacement, then universal-newline translation
/// (`native-chunk-boundary-use/HANDOFF.md` N1). It exists so that the
/// backend's claim about a row's ranking text is *checked* rather than
/// believed — the build re-derives every row here from the committed
/// bytes and refuses if the digests disagree — and so that a query can
/// rebuild the ranking view from the blobs without storing a second copy
/// of the corpus.
pub(crate) fn normalize_ranking_text(bytes: &[u8]) -> String {
    let decoded = String::from_utf8_lossy(bytes);
    if !decoded.contains('\r') {
        return decoded.into_owned();
    }
    let mut out = String::with_capacity(decoded.len());
    let mut characters = decoded.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            out.push('\n');
        } else {
            out.push(character);
        }
    }
    out
}

/// The frozen ranking path for one resource: the source-relative path
/// itself, under `RANKING_PATH_CONVENTION` v2.
///
/// Everything the native ranker reads out of this string is ranking
/// evidence — the stem and the last three directory components go into
/// every BM25 document, and the whole string is matched against the
/// test/compat/example path priors. So it holds what a repository
/// actually contains and nothing an operator or a host contributed: the
/// membership travels separately, as `ranking_scope`.
pub(crate) fn ranking_path(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

/// The identity of the membership a ranked row belongs to, as the native
/// ranker receives it.
///
/// The membership id, which is a digest over the estate, the alias and
/// the source id — opaque, stable across generations and renames of
/// nothing the operator typed. It separates two memberships that publish
/// the same source-relative path into two documents, two BM25 keys and
/// two groups, and it is never tokenised into any document: identity, not
/// text (0167).
pub(crate) fn ranking_scope(membership: &crate::MembershipId) -> String {
    membership.0.clone()
}

// ---- the v2 (chunking) backend boundary ----------------------------------

#[derive(Serialize)]
struct EmbedV2Header<'a> {
    protocol: &'a str,
    mode: &'a str,
    model_path: &'a str,
    output: &'a str,
    chunks: &'a str,
    scratch: &'a str,
    vector_format: &'a str,
    inputs: u64,
    path_convention: &'a str,
}

#[derive(Serialize)]
struct EmbedV2Input<'a> {
    input: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    ranking_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
}

#[derive(Deserialize)]
struct EmbedV2Reply {
    protocol: String,
    backend: String,
    model_path: String,
    model_digest: String,
    rows: u64,
    dimensions: u64,
    #[serde(default)]
    unmapped: Vec<ReportedUnmapped>,
    #[serde(default)]
    chunker: Option<ReportedChunker>,
    #[serde(default)]
    environment: Option<ReportedEnvironment>,
}

#[derive(Deserialize)]
struct ReportedUnmapped {
    path: String,
    reason: String,
}

#[derive(Deserialize)]
struct ReportedChunker {
    implementation: String,
    entry_point: String,
    constants: String,
    parsers: String,
    files: BTreeMap<String, ReportedChunkerFile>,
    #[serde(default)]
    grammars: Option<ReportedGrammars>,
}

/// What the backend says about the parser shared libraries it loaded.
/// Every path in here is re-read by the product before it is recorded.
#[derive(Deserialize)]
struct ReportedGrammars {
    state: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    cache_root: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    libraries: Vec<ReportedGrammarLibrary>,
    #[serde(default)]
    uncovered: Vec<ReportedUnavailable>,
}

#[derive(Deserialize)]
struct ReportedGrammarLibrary {
    path: String,
    digest: String,
    byte_len: u64,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    declared_digest: Option<String>,
}

#[derive(Deserialize)]
struct ReportedChunkerFile {
    path: String,
    digest: String,
    byte_len: u64,
}

/// One row the native chunker produced, as the backend reports it. Every
/// field is re-derived and checked on this side before it becomes a
/// `MappingRow`.
#[derive(Deserialize)]
struct ProducedChunk {
    input: u64,
    slot: u64,
    byte_start: u64,
    byte_end: u64,
    #[serde(default)]
    language: Option<String>,
    text_digest: String,
    text_normalization: String,
}

/// Run the configured backend under `wirk-embed/v2`.
#[allow(clippy::too_many_arguments)]
///
/// Same child-environment rule as `run_backend`: nothing inherited,
/// offline flags on. The difference is the payload — `chunk-embed` hands
/// over committed blob bytes by path and receives boundaries back,
/// `embed` hands over text the product already fixed.
fn run_backend_v2(
    program: &Path,
    args: &[String],
    model: &str,
    mode: &str,
    inputs: &[EmbedV2Input<'_>],
    output: &Path,
    chunks: &Path,
    scratch: &Path,
) -> Result<EmbedV2Reply, String> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command
        .args(args)
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
            "backend {} could not be started: {error}",
            program.display()
        )
    })?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let header = serde_json::to_vec(&EmbedV2Header {
        protocol: EMBED_PROTOCOL_V2,
        mode,
        model_path: model,
        output: &output.display().to_string(),
        chunks: &chunks.display().to_string(),
        scratch: &scratch.display().to_string(),
        vector_format: VECTOR_FORMAT,
        inputs: inputs.len() as u64,
        path_convention: RANKING_PATH_CONVENTION,
    })
    .map_err(|error| format!("backend request could not be encoded: {error}"))?;
    let write = (|| -> std::io::Result<()> {
        stdin.write_all(&header)?;
        stdin.write_all(b"\n")?;
        for input in inputs {
            stdin.write_all(&serde_json::to_vec(input)?)?;
            stdin.write_all(b"\n")?;
        }
        stdin.flush()
    })();
    drop(stdin);
    let finished = child
        .wait_with_output()
        .map_err(|error| format!("backend {} failed: {error}", program.display()))?;
    if !finished.status.success() {
        return Err(format!(
            "backend {} exited {} : {}",
            program.display(),
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
            "backend {} did not consume the request: {error}",
            program.display()
        ));
    }
    let stdout = String::from_utf8_lossy(&finished.stdout);
    let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) else {
        return Err(format!(
            "backend {} produced no reply line",
            program.display()
        ));
    };
    let reply: EmbedV2Reply = serde_json::from_str(line)
        .map_err(|error| format!("backend reply is not a {EMBED_PROTOCOL_V2} record: {error}"))?;
    if reply.protocol != EMBED_PROTOCOL_V2 {
        return Err(format!(
            "backend speaks protocol {} but this request is {EMBED_PROTOCOL_V2}",
            reply.protocol
        ));
    }
    Ok(reply)
}

/// One indexed resource, with the committed bytes and the generation
/// units that already partition them.
struct BuildInput {
    ranking_path: String,
    path: Vec<u8>,
    object_id: String,
    family: crate::ContentFamily,
    /// Resolved only when a chunker resolved one; a units edition has no
    /// language because nothing detected one.
    language: Option<String>,
    bytes: Vec<u8>,
    units: Vec<crate::TextUnit>,
}

fn read_produced_chunks(path: &Path) -> Result<Vec<ProducedChunk>, String> {
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "backend wrote no chunk output at {}: {error}",
            path.display()
        )
    })?;
    let text =
        String::from_utf8(bytes).map_err(|_| "backend chunk output is not UTF-8".to_owned())?;
    let mut produced = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        produced.push(
            serde_json::from_str(line).map_err(|error| {
                format!("backend chunk line {} is malformed: {error}", number + 1)
            })?,
        );
    }
    Ok(produced)
}

/// The contiguous run of generation units a byte range lies inside.
///
/// `validate_generation` pins the units of a resource as an ordered,
/// gapless partition of its blob, so the run is found by two boundary
/// searches and the interior never has to be stored. This returns `None`
/// when the range is not covered at all, which is a refusal rather than a
/// row with an invented index.
fn covering_units(units: &[crate::TextUnit], start: u64, end: u64) -> Option<(UnitId, UnitId)> {
    let first = units.partition_point(|unit| unit.byte_end <= start);
    let last = units
        .partition_point(|unit| unit.byte_start < end)
        .checked_sub(1)?;
    let (first, last) = (units.get(first)?, units.get(last)?);
    (first.byte_start <= start && end <= last.byte_end).then(|| (first.id.clone(), last.id.clone()))
}

/// Turn what the backend reported into rows, re-deriving every claim from
/// the committed bytes on this side first.
///
/// Nothing here trusts the backend about a boundary. The byte range must
/// lie inside the blob, rows must be ordered and non-overlapping within a
/// resource, the ranking text is re-derived here and must digest to what
/// the backend said, and the covering unit run is computed here from the
/// generation. Display line numbers come from the *original* bytes, so a
/// CRLF resource reports the lines a reader would count in the file
/// rather than the lines the chunker counted in its normalised copy.
fn build_native_rows(
    membership: &Membership,
    generation: &SourceGeneration,
    inputs: &[BuildInput],
    produced: &[ProducedChunk],
    coverage: &mut EditionCoverage,
) -> Result<Vec<MappingRow>, String> {
    let mut rows = Vec::with_capacity(produced.len());
    let mut previous: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    let mut with_rows: BTreeSet<u64> = BTreeSet::new();
    for chunk in produced {
        let Some(input) = inputs.get(chunk.input as usize) else {
            return Err(format!(
                "backend returned a chunk for input {} which was never sent",
                chunk.input
            ));
        };
        let (start, end) = (chunk.byte_start, chunk.byte_end);
        if start >= end || end > input.bytes.len() as u64 {
            return Err(format!(
                "chunk {} of {} addresses bytes [{start},{end}) outside its own {}-byte blob",
                chunk.slot,
                input.ranking_path,
                input.bytes.len()
            ));
        }
        match previous.get(&chunk.input) {
            Some((previous_slot, previous_end)) => {
                if chunk.slot != previous_slot + 1 {
                    return Err(format!(
                        "chunk slots of {} are not consecutive: {} follows {previous_slot}",
                        input.ranking_path, chunk.slot
                    ));
                }
                if start < *previous_end {
                    return Err(format!(
                        "chunk {} of {} starts at {start}, inside the previous chunk which ends \
                         at {previous_end}",
                        chunk.slot, input.ranking_path
                    ));
                }
            }
            None => {
                if chunk.slot != 0 {
                    return Err(format!(
                        "the first chunk of {} is slot {} rather than 0",
                        input.ranking_path, chunk.slot
                    ));
                }
            }
        }
        previous.insert(chunk.input, (chunk.slot, end));
        with_rows.insert(chunk.input);

        let slice = &input.bytes[start as usize..end as usize];
        // The independent re-derivation. The backend derived this text
        // through the native reader; the product derives it again here
        // from the committed bytes and refuses if they are not the same
        // string. Nothing about the mapping is taken on the backend's
        // word.
        let text = normalize_ranking_text(slice);
        let text_digest = digest_bytes(text.as_bytes());
        if text_digest != chunk.text_digest {
            return Err(format!(
                "chunk {} of {} reports ranking text {} but the committed bytes [{start},{end}) \
                 normalise to {text_digest}; this build refuses rather than record a mapping it \
                 could not reproduce",
                chunk.slot, input.ranking_path, chunk.text_digest
            ));
        }
        let normalization = if text.as_bytes() == slice {
            TEXT_IDENTITY
        } else {
            TEXT_NORMALIZED
        };
        if normalization != chunk.text_normalization {
            return Err(format!(
                "chunk {} of {} reports normalization {} but its bytes are {normalization}",
                chunk.slot, input.ranking_path, chunk.text_normalization
            ));
        }
        let Some((unit_first, unit_last)) = covering_units(&input.units, start, end) else {
            return Err(format!(
                "chunk {} of {} spans bytes [{start},{end}) which no run of this generation's \
                 units covers",
                chunk.slot, input.ranking_path
            ));
        };
        let Some((line_start, line_end)) =
            crate::domain::actual_line_bounds(&input.bytes, start, end)
        else {
            return Err(format!(
                "chunk {} of {} does not lie on character boundaries of its own blob",
                chunk.slot, input.ranking_path
            ));
        };
        rows.push(MappingRow {
            row: rows.len() as u64,
            estate: membership.estate.clone(),
            membership: membership.id.clone(),
            source: membership.source.clone(),
            generation: generation.id.clone(),
            unit: unit_first,
            unit_last: Some(unit_last),
            path: input.path.clone(),
            object_id: input.object_id.clone(),
            byte_start: start,
            byte_end: end,
            line_start,
            line_end,
            byte_len: slice.len() as u64,
            content_digest: digest_bytes(slice),
            text_digest: Some(text_digest),
            text_normalization: Some(normalization.to_owned()),
            language: chunk.language.clone(),
            ranking_path: Some(input.ranking_path.clone()),
            slot: Some(chunk.slot),
            family: Some(input.family),
        });
    }
    // A resource the chunker returned nothing for is named, not counted
    // and not treated as missing: `chunk_source` returns an empty list for
    // whitespace-only input, and "every resource contributes a row" is not
    // a safe invariant over the installed implementation.
    for (index, input) in inputs.iter().enumerate() {
        if !with_rows.contains(&(index as u64)) {
            coverage.resources_without_rows.push(UnavailableEntry {
                name: input.ranking_path.clone(),
                reason: format!(
                    "the native chunker produced no chunk for these {} committed bytes",
                    input.bytes.len()
                ),
            });
        }
    }
    coverage
        .resources_without_rows
        .sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

// ---- store operations ----------------------------------------------------

impl crate::AtlasStore {
    fn semantic_root(&self) -> PathBuf {
        self.root().join("semantic")
    }

    pub(crate) fn edition_dir(&self, id: &EditionId) -> Result<PathBuf, AtlasError> {
        if !valid_edition_id(&id.0) {
            return Err(AtlasError::Generation("invalid edition identifier".into()));
        }
        Ok(self.semantic_root().join(&id.0))
    }

    /// Build one immutable semantic edition for an already-staged
    /// generation of `membership`, leaving it *staged*: no reader consults
    /// it until `select_semantic` names it.
    ///
    /// `config.chunking` decides what a row *is*, and it is the caller's
    /// explicit choice. `Units` keeps one row per generation unit.
    /// `Native` asks the configured backend to run the installed chunker
    /// over the same unchanged committed bytes and returns meaningful
    /// multi-line spans — **without** restaging, re-extracting or
    /// otherwise disturbing the source generation, whose units remain the
    /// covering index every chunk is bound to.
    pub fn build_semantic(
        &mut self,
        membership: &Membership,
        generation_id: &GenerationId,
        config: &SemanticBuildConfig,
    ) -> Result<SemanticBuildOutcome, AtlasError> {
        self.check_membership_public(membership)?;
        let generation = self.generation(generation_id)?;
        // A generation id is a global key; binding it to this membership's
        // own source before a single blob is read is the same rule
        // continuation pinning already follows (ruling 0095).
        if generation.source != membership.source {
            return Err(AtlasError::InvalidCoordinate(
                "generation does not belong to this membership's source".into(),
            ));
        }

        let model = match configured_directory(&config.model, "model") {
            Ok(model) => model,
            Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
        };
        let program = match configured_file(&config.backend, "backend") {
            Ok(program) => program,
            Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
        };
        // Every token is recorded in its executed position; the ones that
        // name an existing file are additionally digested, because their
        // *bytes* are an input as well as their spelling. Dropping the
        // rest is what W4-LIFECYCLE-CORRECTION.md item 4 found: two
        // genuinely different producer configurations with the same
        // output were the same edition.
        let mut arguments = Vec::new();
        let mut argv = Vec::new();
        for argument in &config.backend_args {
            let path = Path::new(argument);
            if path.is_absolute() && path.is_file() {
                match configured_file(path, "backend argument") {
                    Ok(recorded) => {
                        arguments.push(recorded.clone());
                        argv.push(BackendArgument::File {
                            value: argument.clone(),
                            file: Box::new(recorded),
                        });
                    }
                    Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
                }
            } else {
                argv.push(BackendArgument::Literal {
                    value: argument.clone(),
                });
            }
        }

        let (inputs, unitizer) = match self.collect_inputs(membership, &generation)? {
            Ok(collected) => collected,
            Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
        };
        if inputs.is_empty() {
            return Ok(SemanticBuildOutcome::Refused(format!(
                "generation {} has no indexed resource to embed",
                generation.id.0
            )));
        }

        let staging = self
            .semantic_root()
            .join(format!(".tmp-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&staging)?;
        let outcome = self.finish_build(
            membership,
            &generation,
            config,
            model,
            program,
            arguments,
            argv,
            inputs,
            unitizer,
            &staging,
        );
        match outcome {
            Ok(SemanticBuildOutcome::Staged(edition)) => {
                let destination = self.edition_dir(&edition.id)?;
                if destination.exists() {
                    // Identical inputs and identical outputs, already
                    // staged: the immutable directory is the same one.
                    // Never rewritten — an edition's bytes are its
                    // identity — and the record returned is the one
                    // already on disk, so a caller is never handed a
                    // fresher build time than the bytes actually have.
                    std::fs::remove_dir_all(&staging)?;
                    let existing = self.read_edition(&edition.id)?;
                    return Ok(SemanticBuildOutcome::Staged(Box::new(existing)));
                } else {
                    // The crash window a verifier can actually exercise:
                    // everything is written and synced, nothing is
                    // visible. `AtlasStore::open` cleans the abandoned
                    // private temporary, and no edition exists.
                    crate::store::checkpoint("semantic-edition-written");
                    std::fs::rename(&staging, &destination)?;
                    std::fs::File::open(self.semantic_root())?.sync_all()?;
                }
                crate::store::checkpoint("semantic-edition-staged");
                Ok(SemanticBuildOutcome::Staged(edition))
            }
            other => {
                let _ = std::fs::remove_dir_all(&staging);
                other
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_build(
        &self,
        membership: &Membership,
        generation: &SourceGeneration,
        config: &SemanticBuildConfig,
        model: ConfiguredPath,
        program: ConfiguredPath,
        arguments: Vec<ConfiguredPath>,
        argv: Vec<BackendArgument>,
        inputs: Vec<BuildInput>,
        unitizer: String,
        staging: &Path,
    ) -> Result<SemanticBuildOutcome, AtlasError> {
        let vectors_path = staging.join(VECTORS_FILE);
        let chunks_path = staging.join("chunks.ndjson");
        let scratch = staging.join("inputs");
        std::fs::create_dir_all(&scratch)?;

        // What is sent depends only on who owns the boundaries.
        let mut texts: Vec<String> = Vec::new();
        let mut prepared: Vec<MappingRow> = Vec::new();
        let mut coverage = EditionCoverage {
            resources_indexed: inputs.len() as u64,
            indexed_bytes: inputs.iter().map(|input| input.bytes.len() as u64).sum(),
            ..EditionCoverage::default()
        };
        let mode = match config.chunking {
            SemanticChunking::Units => "embed",
            SemanticChunking::Native => "chunk-embed",
        };
        let mut payload: Vec<EmbedV2Input<'_>> = Vec::new();
        let mut blob_files: Vec<String> = Vec::new();
        if config.chunking == SemanticChunking::Native {
            for (index, input) in inputs.iter().enumerate() {
                let file = scratch.join(format!("{index}.bin"));
                write_sync(&file, &input.bytes)?;
                blob_files.push(file.display().to_string());
            }
            for (index, input) in inputs.iter().enumerate() {
                payload.push(EmbedV2Input {
                    input: index as u64,
                    ranking_path: Some(&input.ranking_path),
                    bytes_file: Some(blob_files[index].clone()),
                    text: None,
                });
            }
        } else {
            // The product already owns every boundary: one row per unit,
            // built and digested here, with only the text crossing the
            // boundary.
            for input in &inputs {
                for (slot, unit) in input.units.iter().enumerate() {
                    let (start, end) = (unit.byte_start as usize, unit.byte_end as usize);
                    if end > input.bytes.len() || start > end {
                        return Ok(SemanticBuildOutcome::Refused(format!(
                            "unit {} addresses bytes outside its own blob",
                            unit.id.0
                        )));
                    }
                    let slice = &input.bytes[start..end];
                    if std::str::from_utf8(slice).is_err() {
                        return Ok(SemanticBuildOutcome::Refused(format!(
                            "unit {} is not valid UTF-8; refusing rather than substituting bytes",
                            unit.id.0
                        )));
                    }
                    let text = normalize_ranking_text(slice);
                    let (line_start, line_end) = crate::domain::actual_line_bounds(
                        &input.bytes,
                        unit.byte_start,
                        unit.byte_end,
                    )
                    .unwrap_or((unit.line_start, unit.line_end));
                    prepared.push(MappingRow {
                        row: prepared.len() as u64,
                        estate: membership.estate.clone(),
                        membership: membership.id.clone(),
                        source: membership.source.clone(),
                        generation: generation.id.clone(),
                        unit: unit.id.clone(),
                        unit_last: None,
                        path: input.path.clone(),
                        object_id: input.object_id.clone(),
                        byte_start: unit.byte_start,
                        byte_end: unit.byte_end,
                        line_start,
                        line_end,
                        byte_len: slice.len() as u64,
                        content_digest: digest_bytes(slice),
                        text_digest: Some(digest_bytes(text.as_bytes())),
                        text_normalization: Some(
                            if text.as_bytes() == slice {
                                TEXT_IDENTITY
                            } else {
                                TEXT_NORMALIZED
                            }
                            .to_owned(),
                        ),
                        language: input.language.clone(),
                        ranking_path: Some(input.ranking_path.clone()),
                        slot: Some(slot as u64),
                        family: Some(input.family),
                    });
                    texts.push(text);
                }
            }
            if prepared.is_empty() {
                return Ok(SemanticBuildOutcome::Refused(format!(
                    "generation {} has no indexed retrieval unit to embed",
                    generation.id.0
                )));
            }
            for (index, text) in texts.iter().enumerate() {
                payload.push(EmbedV2Input {
                    input: index as u64,
                    ranking_path: None,
                    bytes_file: None,
                    text: Some(text),
                });
            }
        }

        // Deliberately the *configured* path, not the canonical one.
        // Canonicalizing an interpreter is not a no-op: a virtual
        // environment's `bin/python` is a symlink to a base interpreter,
        // and executing the resolved target silently loses the
        // environment that made the backend's libraries importable.
        let reply = match run_backend_v2(
            Path::new(&config.backend),
            &config.backend_args,
            &model.canonical,
            mode,
            &payload,
            &vectors_path,
            &chunks_path,
            &scratch,
        ) {
            Ok(reply) => reply,
            Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
        };
        // The backend's own account of what it loaded, checked against the
        // product's own canonicalization and digest of the same directory.
        // Ruling 0088's defect was precisely a consumed identity nothing
        // compared against the loaded object.
        if reply.model_path != model.canonical {
            return Ok(SemanticBuildOutcome::Refused(format!(
                "backend loaded model {} but this build consumed {}",
                reply.model_path, model.canonical
            )));
        }
        if reply.model_digest != model.digest {
            return Ok(SemanticBuildOutcome::Refused(format!(
                "backend reports model digest {} for {} but its bytes digest to {}",
                reply.model_digest, model.canonical, model.digest
            )));
        }

        let mut chunker_identity = None;
        if config.chunking == SemanticChunking::Native {
            // Everything the backend says about a boundary is re-derived
            // from the committed bytes here before it becomes a row.
            let Some(reported) = &reply.chunker else {
                return Ok(SemanticBuildOutcome::Refused(
                    "backend produced native chunks but described no chunker; a boundary whose \
                     producer is unrecorded cannot be an edition input"
                        .into(),
                ));
            };
            let mut files = Vec::new();
            for name in reported.files.keys() {
                let file = &reported.files[name];
                match configured_file(Path::new(&file.path), &format!("chunker module {name}")) {
                    Ok(measured) => {
                        if measured.digest != file.digest || measured.byte_len != file.byte_len {
                            return Ok(SemanticBuildOutcome::Refused(format!(
                                "backend reports chunker module {name} as {} but its bytes digest \
                                 to {}",
                                file.digest, measured.digest
                            )));
                        }
                        files.push(measured);
                    }
                    Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
                }
            }
            let produced = match read_produced_chunks(&chunks_path) {
                Ok(produced) => produced,
                Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
            };
            match build_native_rows(membership, generation, &inputs, &produced, &mut coverage) {
                Ok(rows) => prepared = rows,
                Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
            }
            for entry in &reply.unmapped {
                coverage.resources_unmapped.push(UnavailableEntry {
                    name: entry.path.clone(),
                    reason: entry.reason.clone(),
                });
            }
            coverage
                .resources_unmapped
                .sort_by(|a, b| a.name.cmp(&b.name));
            let grammars = match measure_grammars(reported.grammars.as_ref()) {
                Ok(grammars) => grammars,
                Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
            };
            chunker_identity = Some(NativeChunkerIdentity {
                implementation: reported.implementation.clone(),
                entry_point: reported.entry_point.clone(),
                constants: reported.constants.clone(),
                parsers: reported.parsers.clone(),
                files,
                grammars,
            });
            if prepared.is_empty() {
                return Ok(SemanticBuildOutcome::Refused(format!(
                    "the native chunker produced no row for any indexed resource of generation {}",
                    generation.id.0
                )));
            }
        }

        if reply.rows != prepared.len() as u64 {
            return Ok(SemanticBuildOutcome::Refused(format!(
                "backend embedded {} rows but {} were derived",
                reply.rows,
                prepared.len()
            )));
        }
        if reply.dimensions == 0 {
            return Ok(SemanticBuildOutcome::Refused(
                "backend reports zero-dimensional vectors".into(),
            ));
        }
        let vector_bytes = match std::fs::read(&vectors_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(SemanticBuildOutcome::Refused(format!(
                    "backend wrote no vector output at {}: {error}",
                    vectors_path.display()
                )));
            }
        };
        let expected = reply.rows * reply.dimensions * 4;
        if vector_bytes.len() as u64 != expected {
            return Ok(SemanticBuildOutcome::Refused(format!(
                "backend wrote {} vector bytes; {VECTOR_FORMAT} for {} rows of {} dimensions is {expected}",
                vector_bytes.len(),
                reply.rows,
                reply.dimensions
            )));
        }

        // The backend's account of its own implementation bytes,
        // re-measured here from the same filesystem before it is believed.
        let environment = match &reply.environment {
            None => BackendEnvironment::Unreported,
            Some(reported) => match measure_environment(reported) {
                Ok(measured) => BackendEnvironment::Reported(Box::new(measured)),
                Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
            },
        };

        // The build's own temporary inputs are not part of the edition:
        // they are the committed blobs, which the repository already holds.
        std::fs::remove_dir_all(&scratch)?;
        let _ = std::fs::remove_file(&chunks_path);

        coverage.resources_with_rows = {
            let mut seen: BTreeSet<&[u8]> = BTreeSet::new();
            for row in &prepared {
                seen.insert(row.path.as_slice());
            }
            seen.len() as u64
        };
        coverage.covered_bytes = prepared.iter().map(|row| row.byte_len).sum();

        let mut mapping_bytes = Vec::new();
        for row in &prepared {
            mapping_bytes.extend_from_slice(&serde_json::to_vec(row)?);
            mapping_bytes.push(b'\n');
        }
        write_sync(&staging.join(MAPPING_FILE), &mapping_bytes)?;

        let mut edition = SemanticEdition {
            id: EditionId(String::new()),
            identity: IDENTITY_V5.into(),
            estate: membership.estate.clone(),
            membership: membership.id.clone(),
            source: membership.source.clone(),
            generation: generation.id.clone(),
            generation_revision: generation.revision.clone(),
            generation_content: generation.content.clone(),
            acquisition_policy: generation.acquisition_policy.clone(),
            chunker: ChunkerIdentity {
                extractor_set: generation.extractor_set.clone(),
                unitizer,
                chunks: chunker_identity.clone(),
            },
            model: ModelIdentity {
                consumed: model,
                reported_path: reply.model_path,
                reported_digest: reply.model_digest,
            },
            backend: BackendIdentity {
                protocol: EMBED_PROTOCOL_V2.into(),
                program,
                arguments,
                argv,
                reported: reply.backend,
                environment,
            },
            vectors: VectorManifest {
                format: VECTOR_FORMAT.into(),
                file: VECTORS_FILE.into(),
                rows: reply.rows,
                dimensions: reply.dimensions,
                byte_len: vector_bytes.len() as u64,
                // The digest is over the bytes read back off disk, not
                // over anything held in memory before the write.
                digest: digest_bytes(&vector_bytes),
            },
            mapping: MappingManifest {
                file: MAPPING_FILE.into(),
                rows: prepared.len() as u64,
                byte_len: mapping_bytes.len() as u64,
                digest: digest_bytes(&mapping_bytes),
            },
            producer: ProducerIdentity {
                producer: config.producer.clone(),
                built_at_unix_millis: now_unix_millis(),
            },
            retrieval: Some(RetrievalIdentity::new(
                config.chunking,
                chunker_identity
                    .as_ref()
                    .map(|identity| identity.implementation.as_str())
                    .unwrap_or("wirk/generation-units"),
            )),
            coverage,
        };
        edition.id = EditionId::compute(&edition);
        write_sync(
            &staging.join(EDITION_RECORD),
            &serde_json::to_vec_pretty(&edition)?,
        )?;
        std::fs::File::open(staging)?.sync_all()?;
        Ok(SemanticBuildOutcome::Staged(Box::new(edition)))
    }

    /// Every indexed resource of `generation`, in one deterministic order,
    /// with the exact committed bytes behind each. A resource whose blob
    /// cannot be read is a refusal for the whole build: a partial edition
    /// that silently omitted content would be exactly the "publish only a
    /// fully verified edition" failure.
    #[allow(clippy::type_complexity)]
    fn collect_inputs(
        &self,
        membership: &Membership,
        generation: &SourceGeneration,
    ) -> Result<Result<(Vec<BuildInput>, String), String>, AtlasError> {
        let mut inputs = Vec::new();
        let mut unitizer: Option<String> = None;
        let mut blob_cache: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for resource in &generation.resources {
            if resource.disposition != crate::CoverageDisposition::Indexed {
                continue;
            }
            let Some(object_id) = resource.object_id.clone() else {
                return Ok(Err(format!(
                    "indexed resource {} carries no blob identity",
                    String::from_utf8_lossy(&resource.path)
                )));
            };
            let bytes = match blob_cache.get(&object_id) {
                Some(bytes) => bytes.clone(),
                None => match crate::git::blob(Path::new(&membership.locator), &object_id) {
                    Ok(bytes) => {
                        blob_cache.insert(object_id.clone(), bytes.clone());
                        bytes
                    }
                    Err(AtlasError::GitUnavailable(detail)) => {
                        return Ok(Err(format!(
                            "committed bytes for {} are unavailable: {detail}",
                            String::from_utf8_lossy(&resource.path)
                        )));
                    }
                    Err(error) => return Err(error),
                },
            };
            for unit in &resource.units {
                match unitizer.as_deref() {
                    None => unitizer = Some(unit.unitizer.clone()),
                    Some(seen) if seen == unit.unitizer => {}
                    Some(seen) => {
                        return Ok(Err(format!(
                            "generation mixes unitizers {seen} and {}",
                            unit.unitizer
                        )));
                    }
                }
            }
            let Some(family) = resource.units.first().map(|unit| unit.family) else {
                continue;
            };
            inputs.push(BuildInput {
                ranking_path: ranking_path(&resource.path),
                path: resource.path.clone(),
                object_id,
                family,
                language: None,
                bytes,
                units: resource.units.clone(),
            });
        }
        let unitizer = unitizer.unwrap_or_default();
        Ok(Ok((inputs, unitizer)))
    }

    /// The bytes of one file of an edition's immutable directory.
    pub(crate) fn edition_file(&self, id: &EditionId, file: &str) -> std::io::Result<Vec<u8>> {
        let directory = self
            .edition_dir(id)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        std::fs::read(directory.join(file))
    }

    /// One edition's mapping rows, in the order they were written, which
    /// is the vector row order.
    pub(crate) fn read_mapping(
        &self,
        edition: &SemanticEdition,
    ) -> Result<Vec<MappingRow>, String> {
        let bytes = self
            .edition_file(&edition.id, &edition.mapping.file)
            .map_err(|error| format!("edition {} mapping is unreadable: {error}", edition.id.0))?;
        if digest_bytes(&bytes) != edition.mapping.digest {
            return Err(format!(
                "edition {} mapping bytes are not the bytes its record commits to",
                edition.id.0
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("edition {} mapping is not UTF-8", edition.id.0))?;
        let mut rows = Vec::new();
        for (number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row: MappingRow = serde_json::from_str(line).map_err(|error| {
                format!(
                    "edition {} mapping line {} is malformed: {error}",
                    edition.id.0,
                    number + 1
                )
            })?;
            if row.row != rows.len() as u64 {
                return Err(format!(
                    "edition {} mapping rows are out of order at {}",
                    edition.id.0, row.row
                ));
            }
            rows.push(row);
        }
        if rows.len() as u64 != edition.mapping.rows {
            return Err(format!(
                "edition {} record commits to {} mapping rows and holds {}",
                edition.id.0,
                edition.mapping.rows,
                rows.len()
            ));
        }
        Ok(rows)
    }

    pub fn read_edition(&self, id: &EditionId) -> Result<SemanticEdition, AtlasError> {
        let path = self.edition_dir(id)?.join(EDITION_RECORD);
        if !path.exists() {
            return Err(AtlasError::Edition(format!(
                "{EDITION_RECORD} is absent from the directory of edition {}; the edition's own \
                 record, not this source's generation, is what is missing",
                id.0
            )));
        }
        let edition: SemanticEdition = serde_json::from_slice(&std::fs::read(path)?)?;
        if edition.id != *id || EditionId::compute(&edition) != edition.id {
            return Err(AtlasError::Edition(format!(
                "the record in the directory of edition {} does not compute to that identity",
                id.0
            )));
        }
        Ok(edition)
    }

    /// Recompute, from the bytes on disk, everything the record claims.
    /// Nothing here is cached and nothing is repaired.
    pub fn verify_edition(&self, edition: &SemanticEdition) -> SemanticVerification {
        let directory = match self.edition_dir(&edition.id) {
            Ok(directory) => directory,
            Err(error) => return SemanticVerification::Corrupt(error.to_string()),
        };
        let mapping_path = directory.join(&edition.mapping.file);
        let vectors_path = directory.join(&edition.vectors.file);
        for path in [&mapping_path, &vectors_path] {
            if !path.exists() {
                return SemanticVerification::Missing(format!(
                    "{} is absent",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        let mapping_bytes = match std::fs::read(&mapping_path) {
            Ok(bytes) => bytes,
            Err(error) => return SemanticVerification::Unavailable(error.to_string()),
        };
        let vector_bytes = match std::fs::read(&vectors_path) {
            Ok(bytes) => bytes,
            Err(error) => return SemanticVerification::Unavailable(error.to_string()),
        };
        if mapping_bytes.len() as u64 != edition.mapping.byte_len
            || digest_bytes(&mapping_bytes) != edition.mapping.digest
        {
            return SemanticVerification::Corrupt(format!(
                "{} does not match the digest this edition commits to",
                edition.mapping.file
            ));
        }
        // Length is checked as well as digest so a same-length substitution
        // and a truncation report the same way: neither is these bytes.
        if vector_bytes.len() as u64 != edition.vectors.byte_len
            || digest_bytes(&vector_bytes) != edition.vectors.digest
        {
            return SemanticVerification::Corrupt(format!(
                "{} does not match the digest this edition commits to",
                edition.vectors.file
            ));
        }
        if edition.vectors.byte_len != edition.vectors.rows * edition.vectors.dimensions * 4
            || edition.vectors.rows != edition.mapping.rows
        {
            return SemanticVerification::Corrupt(
                "vector rows, dimensions and mapping rows are not mutually consistent".into(),
            );
        }
        let rows: Result<Vec<MappingRow>, _> = mapping_bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<MappingRow>)
            .collect();
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                return SemanticVerification::Corrupt(format!("mapping is malformed: {error}"));
            }
        };
        if rows.len() as u64 != edition.mapping.rows {
            return SemanticVerification::Corrupt(
                "mapping row count does not match the manifest".into(),
            );
        }
        for (index, row) in rows.iter().enumerate() {
            if row.row != index as u64
                || row.estate != edition.estate
                || row.membership != edition.membership
                || row.source != edition.source
                || row.generation != edition.generation
            {
                return SemanticVerification::Corrupt(format!(
                    "mapping row {index} does not belong to this edition"
                ));
            }
        }
        SemanticVerification::Verified
    }

    /// Re-read every mapping row's committed bytes from Git and confirm
    /// each one still digests to what the edition recorded. This is the
    /// exact-coordinate control at the product boundary: the check the
    /// last workspace report omitted, run against the real repository
    /// rather than against the artifact's own copy of the answer.
    pub fn verify_edition_coordinates(
        &self,
        membership: &Membership,
        edition: &SemanticEdition,
    ) -> Result<SemanticVerification, AtlasError> {
        self.check_membership_public(membership)?;
        if edition.membership != membership.id || edition.estate != membership.estate {
            return Ok(SemanticVerification::Corrupt(
                "edition does not belong to this membership".into(),
            ));
        }
        let directory = self.edition_dir(&edition.id)?;
        let mapping_bytes = match std::fs::read(directory.join(&edition.mapping.file)) {
            Ok(bytes) => bytes,
            Err(error) => return Ok(SemanticVerification::Missing(error.to_string())),
        };
        let mut blob_cache: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for line in mapping_bytes.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let row: MappingRow = match serde_json::from_slice(line) {
                Ok(row) => row,
                Err(error) => {
                    return Ok(SemanticVerification::Corrupt(format!(
                        "mapping is malformed: {error}"
                    )));
                }
            };
            let bytes = match blob_cache.get(&row.object_id) {
                Some(bytes) => bytes.clone(),
                None => match crate::git::blob(Path::new(&membership.locator), &row.object_id) {
                    Ok(bytes) => {
                        blob_cache.insert(row.object_id.clone(), bytes.clone());
                        bytes
                    }
                    Err(AtlasError::GitUnavailable(detail)) => {
                        return Ok(SemanticVerification::Unavailable(detail));
                    }
                    Err(error) => return Err(error),
                },
            };
            let (start, end) = (row.byte_start as usize, row.byte_end as usize);
            if end > bytes.len() || start > end {
                return Ok(SemanticVerification::Corrupt(format!(
                    "mapping row {} addresses bytes outside its own blob",
                    row.row
                )));
            }
            if digest_bytes(&bytes[start..end]) != row.content_digest {
                return Ok(SemanticVerification::Corrupt(format!(
                    "mapping row {} no longer digests to the bytes it recorded",
                    row.row
                )));
            }
        }
        Ok(SemanticVerification::Verified)
    }

    /// Publish one staged edition as this membership's selected semantic
    /// artifact. Atomic, and refused outright unless the edition verifies
    /// completely first: a failed selection leaves the previous one exactly
    /// as it was.
    pub fn select_semantic(
        &mut self,
        membership: &Membership,
        id: &EditionId,
    ) -> Result<Result<SemanticEdition, String>, AtlasError> {
        self.check_membership_public(membership)?;
        let edition = match self.read_edition(id) {
            Ok(edition) => edition,
            Err(error) => return Ok(Err(error.to_string())),
        };
        // Cross-estate, cross-membership and cross-source substitution are
        // all refused by the same rule, before any catalog write.
        if edition.estate != membership.estate
            || edition.membership != membership.id
            || edition.source != membership.source
        {
            return Ok(Err(
                "edition belongs to a different estate, membership or source".into(),
            ));
        }
        // The public semantic record must describe the public source
        // record: selecting an edition of a generation this membership
        // does not currently publish would announce vectors over bytes no
        // reader can reach.
        match self.current(membership)? {
            Some(current) if current.id == edition.generation => {}
            Some(current) => {
                return Ok(Err(format!(
                    "edition is built over generation {} but {} currently publishes {}",
                    edition.generation.0, membership.alias, current.id.0
                )));
            }
            None => {
                return Ok(Err(format!(
                    "{} publishes no source generation yet",
                    membership.alias
                )));
            }
        }
        match self.verify_edition(&edition) {
            SemanticVerification::Verified => {}
            other => return Ok(Err(format!("edition does not verify: {other:?}"))),
        }
        match self.verify_edition_coordinates(membership, &edition)? {
            SemanticVerification::Verified => {}
            other => {
                return Ok(Err(format!(
                    "edition's exact coordinates do not verify: {other:?}"
                )));
            }
        }
        crate::store::checkpoint("semantic-selection-verified");
        self.commit_semantic_selection(membership, id.clone())?;
        Ok(Ok(edition))
    }

    /// Every edition on disk for this membership, newest identity order
    /// irrelevant — sorted by id so two runs report the same list.
    pub fn semantic_editions(
        &self,
        membership: &Membership,
    ) -> Result<Vec<EditionState>, AtlasError> {
        self.check_membership_public(membership)?;
        let root = self.semantic_root();
        if !root.exists() {
            return Ok(Vec::new());
        }
        let selected = self.selected_semantic(membership);
        let published = self.current(membership)?.map(|generation| generation.id);
        let mut states = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&root)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !valid_edition_id(&name) {
                continue;
            }
            let id = EditionId(name);
            let edition = match self.read_edition(&id) {
                Ok(edition) => edition,
                // A directory whose record cannot be read at all is not
                // this membership's business to report; it is reported
                // only when it is the *selected* one, below.
                Err(_) => continue,
            };
            if edition.membership != membership.id {
                continue;
            }
            let verification = self.verify_edition(&edition);
            states.push(EditionState {
                selected: selected.as_ref() == Some(&edition.id),
                verification,
                current: published.as_ref() == Some(&edition.generation),
                edition,
            });
        }
        Ok(states)
    }
}

impl crate::AtlasStore {
    /// Whether this membership's selected edition can be advertised as
    /// its current semantic artifact — the one question `atlas status`,
    /// its `--json` form and `--semantic requested` must all answer the
    /// same way (`W4-LIFECYCLE-CORRECTION.md` item 1).
    ///
    /// Derived on every call and never stored, so a republish makes a
    /// selection stale and a republish back makes it current again,
    /// without anything being erased in either direction. Deep
    /// verification is over the *selected* edition alone; `status` walks
    /// every edition separately.
    pub fn semantic_availability(
        &self,
        membership: &Membership,
    ) -> Result<SemanticAvailability, AtlasError> {
        self.check_membership_public(membership)?;
        let Some(id) = self.selected_semantic(membership) else {
            return Ok(SemanticAvailability::None);
        };
        let edition = match self.read_edition(&id) {
            Ok(edition) => edition,
            Err(error) => return Ok(SemanticAvailability::Unreadable(error.to_string())),
        };
        if edition.membership != membership.id || edition.estate != membership.estate {
            return Ok(SemanticAvailability::Unreadable(
                "the selected edition does not belong to this membership".into(),
            ));
        }
        match self.verify_edition(&edition) {
            SemanticVerification::Verified => {}
            other => {
                return Ok(SemanticAvailability::Unusable(match other {
                    SemanticVerification::Missing(detail) => {
                        format!("the selected edition is incomplete: {detail}")
                    }
                    SemanticVerification::Corrupt(detail) => {
                        format!("the selected edition's bytes no longer verify: {detail}")
                    }
                    SemanticVerification::Unavailable(detail) => {
                        format!("the selected edition cannot be read right now: {detail}")
                    }
                    SemanticVerification::Verified => unreachable!(),
                }));
            }
        }
        // Retained, intact, and no longer a description of what this
        // source publishes. Explicitly not a reason to clear the
        // selection: the edition remains historical evidence and a future
        // continuation pin, and erasing it would only hide the staleness.
        match self.current(membership)? {
            Some(current) if current.id == edition.generation => {
                Ok(SemanticAvailability::Available)
            }
            Some(current) => Ok(SemanticAvailability::Superseded(format!(
                "the selected edition is built over generation {}, which this source no longer \
                 publishes; it now publishes {}. The edition is retained unchanged; build and \
                 select one over the published generation, or publish that generation again.",
                edition.generation.0, current.id.0
            ))),
            None => Ok(SemanticAvailability::Superseded(format!(
                "the selected edition is built over generation {}, and this source publishes no \
                 generation at all right now. The edition is retained unchanged.",
                edition.generation.0
            ))),
        }
    }
}

pub(crate) fn write_sync(path: &Path, bytes: &[u8]) -> Result<(), AtlasError> {
    use std::fs::OpenOptions;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(a), at the second of the
    /// two checks that close it. The load-bearing one is the derivation:
    /// a measurement of zero modules is `Unmeasured` when it is written,
    /// which `q11`/`q12` execute end to end through real child processes.
    /// This one is the check that outlives it — a record whose *stored*
    /// coverage says `Complete` over an empty module list (a pre-
    /// correction record, or one written by some other build) mints no
    /// assurance when it is read back. The record is not rewritten; it
    /// simply stops being a basis.
    #[test]
    fn a_stored_complete_coverage_over_no_modules_is_still_not_a_basis() {
        let empty = EnvironmentIdentity {
            kind: "python-distributions/v1".into(),
            root: "/env".into(),
            runtime: "test/1.0".into(),
            executable: "/env/bin/python".into(),
            distributions: Vec::new(),
            undescribed_distributions: Vec::new(),
            modules: Vec::new(),
            unmeasured_modules: Vec::new(),
            scope: ENVIRONMENT_SCOPE_V2.into(),
            coverage: EnvironmentCoverage::Complete,
            digest: "0".repeat(64),
        };
        assert_eq!(
            producer_basis(&BackendEnvironment::Reported(Box::new(empty.clone()))),
            QueryProducerBasis::ConfigurationOnly,
            "zero measured module bytes cannot be an implementation basis, whatever the \
             record calls its coverage"
        );

        // And the control that keeps this from being a ban: one module
        // actually read and digested is a basis, partial attribution and
        // all.
        let measured = EnvironmentIdentity {
            modules: vec![ModuleIdentity {
                name: "ranker".into(),
                origin: "/env/ranker.py".into(),
                path: "/env/ranker.py".into(),
                digest: "1".repeat(64),
                byte_len: 12,
                attribution: ModuleAttribution::Undeclared("no RECORD declares it".into()),
            }],
            coverage: EnvironmentCoverage::Partial("one undeclared module".into()),
            ..empty
        };
        assert_eq!(
            producer_basis(&BackendEnvironment::Reported(Box::new(measured))),
            QueryProducerBasis::ImplementationMeasured
        );
    }

    /// Ruling 0163: the producer *configuration* digest is what a continuation
    /// is
    /// refused on before any child is started, and until now it covered
    /// only the backend's bytes and its argv. The ordering and runtime
    /// policy this build applies is not in either of those: the same
    /// executable, the same arguments and the same loaded modules select
    /// and order a different list under a different policy, and a token
    /// that crossed that boundary silently was resumed against a list it
    /// was never cut from.
    ///
    /// Red before the correction: the digest equalled the same computation
    /// with the policy left out.
    #[test]
    fn b_the_producer_configuration_digest_binds_the_effective_ordering_policy() {
        let program = ConfiguredPath {
            configured: "/env/bin/python3".into(),
            canonical: "/env/bin/python3".into(),
            digest: "a".repeat(64),
            byte_len: 4096,
            file_count: 1,
        };
        let argv = vec![BackendArgument::Literal {
            value: "--backend".into(),
        }];

        // The same absorption, with the policy left out: what the digest
        // was before this correction, recomputed here rather than quoted
        // as a frozen hex string so it stays honest if the surrounding
        // scheme moves.
        let without_policy = {
            let mut hasher = Sha256::new();
            absorb(&mut hasher, QUERY_PRODUCER_SCHEME.as_bytes());
            absorb(&mut hasher, b"configuration");
            absorb(&mut hasher, QUERY_PROTOCOL.as_bytes());
            absorb(&mut hasher, program.configured.as_bytes());
            absorb(&mut hasher, program.canonical.as_bytes());
            absorb(&mut hasher, program.digest.as_bytes());
            absorb(&mut hasher, &program.byte_len.to_be_bytes());
            absorb(&mut hasher, &(argv.len() as u64).to_be_bytes());
            for argument in &argv {
                match argument {
                    BackendArgument::Literal { value } => {
                        absorb(&mut hasher, b"literal");
                        absorb(&mut hasher, value.as_bytes());
                    }
                    BackendArgument::File { value, file } => {
                        absorb(&mut hasher, b"file");
                        absorb(&mut hasher, value.as_bytes());
                        absorb(&mut hasher, file.canonical.as_bytes());
                        absorb(&mut hasher, file.digest.as_bytes());
                        absorb(&mut hasher, &file.byte_len.to_be_bytes());
                    }
                }
            }
            hex(&hasher.finalize())
        };

        assert_ne!(
            query_producer_configuration_digest(&program, &argv),
            without_policy,
            "a configuration digest that does not absorb the effective ordering policy cannot \
             refuse a token that crossed one"
        );
    }

    /// The environment a query child actually runs under and the policy
    /// its continuation is digested under are two places one value is
    /// written. This is the pin that keeps them one value.
    #[test]
    fn c_the_declared_policy_names_the_seed_the_child_is_given() {
        assert!(
            QUERY_ORDERING_POLICY.contains(&format!("PYTHONHASHSEED={QUERY_HASH_SEED}")),
            "the declared policy must name the seed the query child is actually given; \
             policy is {QUERY_ORDERING_POLICY:?} and the seed is {QUERY_HASH_SEED:?}"
        );
    }
}
