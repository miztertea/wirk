//! Exact, Git-object backed source generations.  This crate deliberately owns
//! no checkout scanner and no retrieval policy; callers explicitly acquire,
//! publish, and resolve immutable generations.

mod admission;
mod doctree;
mod document;
mod domain;
mod extract;
mod findings;
mod git;
mod http_source;
mod hydrate;
mod preview;
mod query;
mod relationship;
mod retrieval;
mod semantic;
mod store;

pub use admission::{AdmissionSummary, AdmittedSource, QueryScope};
/// The document-tree acquisition policy label, for a caller (`wirkd`)
/// deciding which `AtlasStore` method a registered source's
/// `Membership::policy` calls for.
pub use doctree::ACQUISITION_POLICY as DOCUMENT_TREE_POLICY;
/// The only `--revision`/`requested_ref` spelling a document-tree
/// source honours. Such a source observes its own current state and
/// nothing else, so a caller defaults to this rather than inventing or
/// requiring a Git-shaped ref a document collection could mean nothing
/// by.
pub use doctree::CURRENT_OBSERVATION as DOCUMENT_TREE_CURRENT_OBSERVATION;
/// The named window between classifying a document-collection entry and
/// opening it, for a verifier arming [`BARRIER_RELEASE_SOCKET`]'s gate
/// at the one real instant a replacement can happen. Exported for the
/// same reason the gate itself is: the window has to be nameable from
/// outside the crate that holds it, or the only way to test the race is
/// to sleep and hope.
pub use doctree::OPEN_WINDOW as DOCTREE_OPEN_WINDOW;
/// The structured document reader: what one source actually holds, and
/// the descriptor a caller selects one of its embedded assets by.
pub use document::{
    DocumentAsset, DocumentHeading, DocumentOutline, DocumentReading, DocumentTable, ResolvedAsset,
};
pub use domain::*;
pub use extract::ExtractorPolicy;
pub use findings::{
    AtlasDirectoryListing, FINDINGS_INDEX_FILE, FindingIndexAppend, FindingIndexRead,
    FindingIndexUnwritten, FindingRow, FindingRowId, FindingRowKind, IndexBacking,
    MalformedFindingLine, Origin as FindingOrigin, PRESERVED_INDEX_PREFIX,
    PreservedIndexRetirementFailed, RETIRED_INDEX_PREFIX, SalvagedFindingIndex,
    UnaccountedFindingRow, atlas_directory_listing, backing_after_failed_index_write,
    unaccounted_finding_rows,
};
/// The HTTP(S) source acquisition policy label, parallel to
/// `DOCUMENT_TREE_POLICY`.
pub use http_source::ACQUISITION_POLICY as HTTP_SOURCE_POLICY;
/// The only `--revision`/`requested_ref` spelling an HTTP source
/// honours, parallel to `DOCUMENT_TREE_CURRENT_OBSERVATION`.
pub use http_source::CURRENT_OBSERVATION as HTTP_SOURCE_CURRENT_OBSERVATION;
/// `atlas acquire --dry-run`'s own answer shape — see the module doc
/// for what each field means and does not promise.
pub use preview::{PreviewBucket, PreviewReport};
pub use query::{
    AnswerBudget, AnswerCoverage, EvidenceHit, HitGenerationIdentity, PathLookupOutcome,
    PathLookupRequest, PinnedProducer, SearchAnswer, SearchRequest, SemanticRequest,
    SemanticStatus, TermMatch, resolve_capacity, resolve_path, search,
};
pub use relationship::{
    FrontierRelationships, RelationshipError, RelationshipView, ResourceKey, admit_relationship,
    relationships_for, relationships_from_resources,
};
pub use retrieval::{
    CapacitySource, RankingMode, ResolvedCapacity, SemanticApplication, SemanticQueryConfig,
    query_index_cache_capacity, query_index_cache_root,
};
pub use semantic::{
    BackendArgument, BackendEnvironment, BackendIdentity, CAPACITY_MAX, CAPACITY_POLICY,
    ChunkerIdentity, ConfiguredPath, DistributionIdentity, EDITION_RECORD, EMBED_PROTOCOL,
    EMBED_PROTOCOL_V2, EMBEDDING_BATCH_POLICY, ENVIRONMENT_SCOPE_V2, EditionCoverage, EditionId,
    EditionState, EnvironmentCoverage, EnvironmentIdentity, GRAMMAR_SCOPE_V1, GrammarCoverage,
    GrammarLibraries, GrammarLibrary, IDENTITY_V1, IDENTITY_V2, IDENTITY_V3, IDENTITY_V4,
    IDENTITY_V5, IDENTITY_V6, LEGACY_CANDIDATE_LIMIT, MAPPING_FILE, MappingManifest, MappingRow,
    ModelIdentity, ModuleAttribution, ModuleIdentity, NativeChunkerIdentity, ProducerIdentity,
    QUERY_HASH_SEED, QUERY_ORDERING_POLICY, QUERY_PRODUCER_BASIS_MEASURED,
    QUERY_PRODUCER_BASIS_MISSING, QUERY_PRODUCER_SCHEME, QUERY_PRODUCER_SCOPE, QUERY_PROTOCOL,
    QueryProducerBasis, QueryProducerPin, RANKING_PATH_CONVENTION, RETRIEVAL_SCHEME,
    RetrievalIdentity, SemanticAvailability, SemanticBuildConfig, SemanticBuildOutcome,
    SemanticChunking, SemanticEdition, SemanticVerification, TEXT_IDENTITY, TEXT_NORMALIZED,
    UnavailableEntry, VECTOR_FORMAT, VECTORS_FILE, VectorManifest,
};
pub use store::{
    AcquireOutcome, AtlasLayout, AtlasStore, BARRIER_RELEASE_SOCKET, RemovalOutcome, atlas_layout,
    checkpoint,
};
