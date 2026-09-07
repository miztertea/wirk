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

/// The stdin/stdout contract this crate speaks to a configured backend.
/// Versioned because it is a compatibility surface with something the
/// product does not compile: a backend that answers with a different
/// protocol string is refused rather than guessed at.
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
    /// The backend enumerated distributions but no loaded modules. The
    /// implementation bytes that ran are simply not part of this record.
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
}

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
    pub unit: UnitId,
    pub path: Vec<u8>,
    pub object_id: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub byte_len: u64,
    pub content_digest: String,
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
pub const IDENTITY_V1: &str = "wirk-semantic-edition/v1";
pub const IDENTITY_V2: &str = "wirk-semantic-edition/v2";
pub const IDENTITY_V3: &str = "wirk-semantic-edition/v3";

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
}

#[derive(Debug, Clone)]
pub struct SemanticBuildConfig {
    pub backend: PathBuf,
    pub backend_args: Vec<String>,
    pub model: PathBuf,
    pub producer: String,
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
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

fn configured_file(path: &Path, what: &str) -> Result<ConfiguredPath, String> {
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
fn measure_environment(reported: &ReportedEnvironment) -> Result<EnvironmentIdentity, String> {
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
    let coverage = if reported.modules.is_none() {
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

#[derive(Serialize)]
struct EmbedHeader<'a> {
    protocol: &'a str,
    model_path: &'a str,
    rows: u64,
    output: &'a str,
    vector_format: &'a str,
}

#[derive(Serialize)]
struct EmbedRow<'a> {
    row: u64,
    text: &'a str,
}

#[derive(Deserialize)]
struct EmbedReply {
    protocol: String,
    backend: String,
    model_path: String,
    model_digest: String,
    rows: u64,
    dimensions: u64,
    /// Optional at the protocol level: a conforming backend that cannot
    /// enumerate its own environment is still a legal backend, and its
    /// editions record `Unreported` rather than an implied provenance.
    #[serde(default)]
    environment: Option<ReportedEnvironment>,
}

/// What a backend says about the environment it embedded in. Every field
/// is re-measured by `measure_environment` before anything is recorded.
#[derive(Deserialize)]
struct ReportedEnvironment {
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

/// Run the configured backend over `texts`, writing binary32 vectors to
/// `output`. The child gets an explicitly constructed environment: nothing
/// inherited, offline flags on. A backend that wants to reach the network
/// has to be configured to, and this product never configures it.
fn run_backend(
    program: &Path,
    args: &[String],
    model: &str,
    texts: &[String],
    output: &Path,
) -> Result<EmbedReply, String> {
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
    let header = serde_json::to_vec(&EmbedHeader {
        protocol: EMBED_PROTOCOL,
        model_path: model,
        rows: texts.len() as u64,
        output: &output.display().to_string(),
        vector_format: VECTOR_FORMAT,
    })
    .map_err(|error| format!("backend request could not be encoded: {error}"))?;
    let write = (|| -> std::io::Result<()> {
        stdin.write_all(&header)?;
        stdin.write_all(b"\n")?;
        for (row, text) in texts.iter().enumerate() {
            stdin.write_all(&serde_json::to_vec(&EmbedRow {
                row: row as u64,
                text,
            })?)?;
            stdin.write_all(b"\n")?;
        }
        stdin.flush()
    })();
    drop(stdin);
    let finished = child
        .wait_with_output()
        .map_err(|error| format!("backend {} failed: {error}", program.display()))?;
    // A backend that exits before reading every row makes the write above
    // fail with EPIPE; its own stderr is the useful diagnostic, so report
    // the exit rather than the broken pipe.
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
    let reply: EmbedReply = serde_json::from_str(line)
        .map_err(|error| format!("backend reply is not a {EMBED_PROTOCOL} record: {error}"))?;
    if reply.protocol != EMBED_PROTOCOL {
        return Err(format!(
            "backend speaks protocol {} but this product speaks {EMBED_PROTOCOL}",
            reply.protocol
        ));
    }
    Ok(reply)
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

        let (rows, texts, unitizer) = match self.collect_units(membership, &generation)? {
            Ok(collected) => collected,
            Err(reason) => return Ok(SemanticBuildOutcome::Refused(reason)),
        };
        if rows.is_empty() {
            return Ok(SemanticBuildOutcome::Refused(format!(
                "generation {} has no indexed retrieval unit to embed",
                generation.id.0
            )));
        }

        let staging = self
            .semantic_root()
            .join(format!(".tmp-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&staging)?;
        let vectors_path = staging.join(VECTORS_FILE);
        let outcome = self.finish_build(
            membership,
            &generation,
            config,
            model,
            program,
            arguments,
            argv,
            rows,
            texts,
            unitizer,
            &staging,
            &vectors_path,
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
                    crate::store::checkpoint_public("semantic-edition-written");
                    std::fs::rename(&staging, &destination)?;
                    std::fs::File::open(self.semantic_root())?.sync_all()?;
                }
                crate::store::checkpoint_public("semantic-edition-staged");
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
        rows: Vec<MappingRow>,
        texts: Vec<String>,
        unitizer: String,
        staging: &Path,
        vectors_path: &Path,
    ) -> Result<SemanticBuildOutcome, AtlasError> {
        // Deliberately the *configured* path, not the canonical one.
        // Canonicalizing an interpreter is not a no-op: a virtual
        // environment's `bin/python` is a symlink to a base interpreter,
        // and executing the resolved target silently loses the
        // environment that made the backend's libraries importable (this
        // is not hypothetical — it is the first thing that happened when
        // this build ran against a real venv). What is executed is what
        // the caller configured; what is *recorded* is both that string
        // and the canonical path and digest of the bytes it resolves to,
        // which is what the kernel actually runs.
        let reply = match run_backend(
            Path::new(&config.backend),
            &config.backend_args,
            &model.canonical,
            &texts,
            vectors_path,
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
        if reply.rows != rows.len() as u64 {
            return Ok(SemanticBuildOutcome::Refused(format!(
                "backend embedded {} rows but {} were sent",
                reply.rows,
                rows.len()
            )));
        }
        if reply.dimensions == 0 {
            return Ok(SemanticBuildOutcome::Refused(
                "backend reports zero-dimensional vectors".into(),
            ));
        }
        let vector_bytes = match std::fs::read(vectors_path) {
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

        let mut mapping_bytes = Vec::new();
        for row in &rows {
            mapping_bytes.extend_from_slice(&serde_json::to_vec(row)?);
            mapping_bytes.push(b'\n');
        }
        write_sync(&staging.join(MAPPING_FILE), &mapping_bytes)?;

        let mut edition = SemanticEdition {
            id: EditionId(String::new()),
            identity: IDENTITY_V3.into(),
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
            },
            model: ModelIdentity {
                consumed: model,
                reported_path: reply.model_path,
                reported_digest: reply.model_digest,
            },
            backend: BackendIdentity {
                protocol: EMBED_PROTOCOL.into(),
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
                rows: rows.len() as u64,
                byte_len: mapping_bytes.len() as u64,
                digest: digest_bytes(&mapping_bytes),
            },
            producer: ProducerIdentity {
                producer: config.producer.clone(),
                built_at_unix_millis: now_unix_millis(),
            },
        };
        edition.id = EditionId::compute(&edition);
        write_sync(
            &staging.join(EDITION_RECORD),
            &serde_json::to_vec_pretty(&edition)?,
        )?;
        std::fs::File::open(staging)?.sync_all()?;
        Ok(SemanticBuildOutcome::Staged(Box::new(edition)))
    }

    /// Every indexed unit of `generation`, in one deterministic order,
    /// with the exact committed bytes behind each. A unit whose blob or
    /// bounds cannot be read is a refusal for the whole build: a partial
    /// edition that silently omits rows would be exactly the "publish only
    /// a fully verified edition" failure.
    #[allow(clippy::type_complexity)]
    fn collect_units(
        &self,
        membership: &Membership,
        generation: &SourceGeneration,
    ) -> Result<Result<(Vec<MappingRow>, Vec<String>, String), String>, AtlasError> {
        let mut rows = Vec::new();
        let mut texts = Vec::new();
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
                let (start, end) = (unit.byte_start as usize, unit.byte_end as usize);
                if end > bytes.len() || start > end {
                    return Ok(Err(format!(
                        "unit {} addresses bytes outside its own blob",
                        unit.id.0
                    )));
                }
                let slice = &bytes[start..end];
                let Ok(text) = std::str::from_utf8(slice) else {
                    return Ok(Err(format!(
                        "unit {} is not valid UTF-8; refusing rather than substituting bytes",
                        unit.id.0
                    )));
                };
                rows.push(MappingRow {
                    row: rows.len() as u64,
                    estate: membership.estate.clone(),
                    membership: membership.id.clone(),
                    source: membership.source.clone(),
                    generation: generation.id.clone(),
                    unit: unit.id.clone(),
                    path: resource.path.clone(),
                    object_id: object_id.clone(),
                    byte_start: unit.byte_start,
                    byte_end: unit.byte_end,
                    line_start: unit.line_start,
                    line_end: unit.line_end,
                    byte_len: slice.len() as u64,
                    content_digest: digest_bytes(slice),
                });
                texts.push(text.to_owned());
            }
        }
        let unitizer = unitizer.unwrap_or_default();
        Ok(Ok((rows, texts, unitizer)))
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
        crate::store::checkpoint_public("semantic-selection-verified");
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
