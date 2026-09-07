//! Exact, Git-object backed source generations.  This crate deliberately owns
//! no checkout scanner and no retrieval policy; callers explicitly acquire,
//! publish, and resolve immutable generations.

mod admission;
mod domain;
mod extract;
mod git;
mod query;
mod relationship;
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
pub use store::{AcquireOutcome, AtlasStore};
