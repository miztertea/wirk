//! Exact, Git-object backed source generations.  This crate deliberately owns
//! no checkout scanner and no retrieval policy; callers explicitly acquire,
//! publish, and resolve immutable generations.

mod admission;
mod domain;
mod extract;
mod git;
mod query;
mod relationship;
mod semantic;
mod store;

pub use admission::{AdmissionSummary, AdmittedSource, QueryScope};
pub use domain::*;
pub use extract::ExtractorPolicy;
pub use query::{
    AnswerBudget, AnswerCoverage, EvidenceHit, HitGenerationIdentity, PathLookupOutcome,
    PathLookupRequest, SearchAnswer, SearchRequest, SemanticRequest, SemanticStatus, resolve_path,
    search,
};
pub use relationship::{
    RelationshipError, RelationshipView, admit_relationship, relationships_for,
};
pub use semantic::{
    BackendArgument, BackendEnvironment, BackendIdentity, ChunkerIdentity, ConfiguredPath,
    DistributionIdentity, EDITION_RECORD, EMBED_PROTOCOL, ENVIRONMENT_SCOPE_V2, EditionId,
    EditionState, EnvironmentCoverage, EnvironmentIdentity, IDENTITY_V1, IDENTITY_V2, IDENTITY_V3,
    MAPPING_FILE, MappingManifest, MappingRow, ModelIdentity, ModuleAttribution, ModuleIdentity,
    ProducerIdentity, SemanticAvailability, SemanticBuildConfig, SemanticBuildOutcome,
    SemanticEdition, SemanticVerification, UnavailableEntry, VECTOR_FORMAT, VECTORS_FILE,
    VectorManifest,
};
pub use store::{AcquireOutcome, AtlasStore};
