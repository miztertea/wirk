//! Exact, Git-object backed source generations.  This crate deliberately owns
//! no checkout scanner and no retrieval policy; callers explicitly acquire,
//! publish, and resolve immutable generations.

mod admission;
mod domain;
mod extract;
mod findings;
mod git;
mod query;
mod relationship;
mod retrieval;
mod semantic;
mod store;

pub use admission::{AdmissionSummary, AdmittedSource, QueryScope};
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
};
pub use semantic::{
    BackendArgument, BackendEnvironment, BackendIdentity, CAPACITY_MAX, CAPACITY_POLICY,
    ChunkerIdentity, ConfiguredPath, DistributionIdentity, EDITION_RECORD, EMBED_PROTOCOL,
    EMBED_PROTOCOL_V2, ENVIRONMENT_SCOPE_V2, EditionCoverage, EditionId, EditionState,
    EnvironmentCoverage, EnvironmentIdentity, GRAMMAR_SCOPE_V1, GrammarCoverage, GrammarLibraries,
    GrammarLibrary, IDENTITY_V1, IDENTITY_V2, IDENTITY_V3, IDENTITY_V4, IDENTITY_V5,
    LEGACY_CANDIDATE_LIMIT, MAPPING_FILE, MappingManifest, MappingRow, ModelIdentity,
    ModuleAttribution, ModuleIdentity, NativeChunkerIdentity, ProducerIdentity, QUERY_HASH_SEED,
    QUERY_ORDERING_POLICY, QUERY_PRODUCER_BASIS_MEASURED, QUERY_PRODUCER_BASIS_MISSING,
    QUERY_PRODUCER_SCHEME, QUERY_PRODUCER_SCOPE, QUERY_PROTOCOL, QueryProducerBasis,
    QueryProducerPin, RANKING_PATH_CONVENTION, RETRIEVAL_SCHEME, RetrievalIdentity,
    SemanticAvailability, SemanticBuildConfig, SemanticBuildOutcome, SemanticChunking,
    SemanticEdition, SemanticVerification, TEXT_IDENTITY, TEXT_NORMALIZED, UnavailableEntry,
    VECTOR_FORMAT, VECTORS_FILE, VectorManifest,
};
pub use store::{AcquireOutcome, AtlasStore, BARRIER_RELEASE_SOCKET, checkpoint};
