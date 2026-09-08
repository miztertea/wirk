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
pub use findings::{FindingRow, FindingRowId, FindingRowKind, Origin as FindingOrigin};
pub use query::{
    AnswerBudget, AnswerCoverage, EvidenceHit, HitGenerationIdentity, PathLookupOutcome,
    PathLookupRequest, PinnedProducer, SearchAnswer, SearchRequest, SemanticRequest,
    SemanticStatus, resolve_path, search,
};
pub use relationship::{
    RelationshipError, RelationshipView, admit_relationship, relationships_for,
};
pub use retrieval::{RankingMode, SemanticApplication, SemanticQueryConfig};
pub use semantic::{
    BackendArgument, BackendEnvironment, BackendIdentity, CANDIDATE_LIMIT, ChunkerIdentity,
    ConfiguredPath, DistributionIdentity, EDITION_RECORD, EMBED_PROTOCOL, EMBED_PROTOCOL_V2,
    ENVIRONMENT_SCOPE_V2, EditionCoverage, EditionId, EditionState, EnvironmentCoverage,
    EnvironmentIdentity, GRAMMAR_SCOPE_V1, GrammarCoverage, GrammarLibraries, GrammarLibrary,
    IDENTITY_V1, IDENTITY_V2, IDENTITY_V3, IDENTITY_V4, IDENTITY_V5, MAPPING_FILE, MappingManifest,
    MappingRow, ModelIdentity, ModuleAttribution, ModuleIdentity, NativeChunkerIdentity,
    ProducerIdentity, QUERY_PRODUCER_BASIS_MEASURED, QUERY_PRODUCER_BASIS_MISSING,
    QUERY_PRODUCER_SCHEME, QUERY_PRODUCER_SCOPE, QUERY_PROTOCOL, QueryProducerBasis,
    QueryProducerPin, RANKING_PATH_CONVENTION, RETRIEVAL_SCHEME, RetrievalIdentity,
    SemanticAvailability, SemanticBuildConfig, SemanticBuildOutcome, SemanticChunking,
    SemanticEdition, SemanticVerification, TEXT_IDENTITY, TEXT_NORMALIZED, UnavailableEntry,
    VECTOR_FORMAT, VECTORS_FILE, VectorManifest,
};
pub use store::{AcquireOutcome, AtlasStore};
