//! The stage projection: the bounded, immutable, inspectable context one
//! orienting Waypoint was actually delivered (P3 W-C1,
//! `knowledge/work/p3-world-loop/loop-c-build-correct/BUILD.md` §2, §3,
//! §5.1, as bounded by ruling 0124).
//!
//! Three separations carry the whole design and each is testable:
//!
//! 1. **Content identity is not the observation receipt** (BUILD.md C3).
//!    `ProjectionId` covers `ProjectionContent` and nothing else, so two
//!    assemblies that delivered the same context at different instants
//!    share one id. The receipt — when it was observed, over how long,
//!    how many observation laps it cost — is delivered in the same file
//!    and returned by `wirk world show`, but it is provenance, not
//!    content.
//!
//!    Separate is not unchecked. The reference the journal carries
//!    holds a `receipt` digest over **every** delivered receipt byte, so
//!    a receipt that does not match the journal reads unavailable rather
//!    than delivering substituted provenance (ruling 0126, correcting
//!    the first candidate: `observed_at`, `observation_window_ms` and
//!    `laps` were covered by nothing at all and could be edited on disk
//!    without any signal). What stays separate is *identity*: the digest
//!    is not part of `ProjectionId` and not part of `WorldHash`, so
//!    re-observing the identical context still yields the same
//!    projection id and does not move a stage's resume key.
//!
//! 2. **Delivery order is part of the fingerprint.** `bound`,
//!    `unknowns`, `assumptions` and `omitted` are hashed in the order
//!    they were delivered, because a different presentation order is a
//!    different delivered context (`BUILD-AMENDMENTS.md`). Nothing here
//!    sorts a list on the way into the hash.
//!
//! 3. **The file is written once and never rewritten.** Its name is the
//!    observation id, so a retry, or a second Work assembling identical
//!    content, writes its own file; "immutable" holds literally rather
//!    than by convention. Reading recomputes `ProjectionId` from the
//!    parsed content and compares: a mismatch is an explicit
//!    unavailability, never a silent empty projection and never a
//!    re-assembly against today's estate.
//!
//! 4. **A revision is added, never edited** (W-C3). Expansion does not
//!    reopen the delivered file and does not touch the reserved World:
//!    it writes a *new* file, at `revision + 1`, naming its parent by
//!    content id and observation, and the journal carries the new
//!    reference on its own event. Every earlier revision stays
//!    retrievable byte-identical, and the initial World's hash — the
//!    stage's resume key — never moves because a stage asked a second
//!    question.
//!
//! Scope, honestly: W-C1 assembles `bound` from literal references
//! resolved out of the authored question and intent; W-C2 adds
//! governance edges, prior-stage artifacts, ranked `referenced`,
//! `reachable` handles and the retrieval note; W-C3 adds expansion.
//! Consulted findings and the index-health note have no producer here,
//! so they have no field here either (ruling 0124: "no unreachable event
//! kinds or empty future schema pretending implemented semantics").

use crate::WaypointId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The one content format this wave writes and reads. A wave that adds a
/// *required* field to `ProjectionContent` changes the canonical bytes of
/// everything it writes, so it advances this tag and keeps a decoder for
/// the older one; a reader that meets a tag it does not know says
/// `ProjectionUnavailable::UnknownFormat` rather than guessing.
///
/// W-C3 adds `expansion` and deliberately does **not** advance the tag,
/// because it is an optional field that is *absent from the document*
/// unless there is one: a revision-0 projection written by this binary
/// serializes to the same bytes a C2 binary wrote, and re-hashes to the
/// `ProjectionId` its journal already recorded. The rule is the field's
/// shape, not the wave number — an added field advances the tag exactly
/// when it moves the bytes of a document that could already exist.
pub const PROJECTION_FORMAT: &str = "wirk.projection/v2";

/// W-C1's format, still read. `ProjectionContentV1` below is frozen: it
/// is the exact field set C1 wrote, so a file written then still
/// re-hashes to the `ProjectionId` its journal recorded. A v2 field is
/// never given a `serde(default)` on the v1 struct — that would silently
/// change the canonical bytes of an already-delivered projection, which
/// is the one thing an immutable delivered context may not do.
pub const PROJECTION_FORMAT_V1: &str = "wirk.projection/v1";

/// The named selector set the assembler ran (BUILD.md §4.3). Carried as
/// content so a delivered projection says which policy produced it,
/// rather than leaving a reader to infer it from which fields are
/// populated.
pub const ASSEMBLY_POLICY: &str = "wirk.assembly/v2";

/// W-C1's selector set, still named by every projection C1 wrote.
pub const ASSEMBLY_POLICY_V1: &str = "wirk.assembly/v1";

/// How many ranked `referenced` entries and how many `reachable` handles
/// one projection *renders* when the Route names no budget of its own.
///
/// Presentation, and only presentation (ruling 0124, ruling 0126 F2): a
/// cut is a `truncated` flag plus an `Omission::OverBudget` carrying the
/// real total, it never changes `coverage`, it never changes
/// `next_action`, and it can never touch `bound` — whose members come
/// from the authored references, the governance edges over them and this
/// Work's own prior receipts, all of which the stage requires.
pub const REFERENCED_DEFAULT: usize = 8;
pub const REACHABLE_DEFAULT: usize = 8;

/// The authored orientation request on a Waypoint: what this stage is
/// being oriented *for*, and which of the Work's own source bindings the
/// assembler may look in.
///
/// `sources` is a filter, never a grant: it is intersected with the
/// Work's own journaled `repositories`, and an alias the Work never
/// bound contributes an `Omission::Inadmissible` count and no lookup at
/// all (BUILD.md §4.1). Empty means "every source this Work is bound
/// to".
///
/// No role, no vocabulary, no taxonomy: a `question` and a source
/// filter, both authored (ruling 0124: "No role taxonomy or mandatory
/// orientation agent").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrientationRequest {
    pub question: String,
    #[serde(default)]
    pub sources: Vec<String>,
    /// How much of the ranked and discoverable material this stage wants
    /// *rendered*. Additive and defaulted, so every C1-era Route keeps
    /// its exact meaning.
    #[serde(default)]
    pub budget: PresentationBudget,
    /// The semantic query backend this Route explicitly configures for
    /// its ranked retrieval, or `None` for "this Route named none".
    ///
    /// The same explicit configuration `wirk atlas search` takes on the
    /// command line (ruling 0109), carried on the recorded orientation
    /// input instead — because a Route's own authored text is the only
    /// place a stage's retrieval can be configured, and until this field
    /// existed the one surface whose job is to hand a stage its estate
    /// evidence was the only retrieval surface that could not ask for
    /// the estate's own semantic editions.
    ///
    /// `skip_serializing_if` is not cosmetic: `route_edition` is a
    /// sha256 over the journaled `waypoint_defs` re-serialized, so a
    /// Route that names no backend must serialize to exactly the bytes
    /// it serialized to before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticQueryRequest>,
}

/// An explicitly configured semantic query backend, as a Route authors
/// it.
///
/// Deliberately the same two-part explicit configuration the public
/// `wirk atlas search --semantic-backend/--semantic-model` takes, and
/// deliberately not defaulted: there is no installed executable, no
/// model name, no environment variable and no user setting that this
/// product reads to fill either half in. A backend without a model (or
/// the reverse) names no runnable configuration, which is why both are
/// required fields rather than two independent options — a Route that
/// names one and not the other is refused when the Route is read, not
/// silently degraded at assembly time.
///
/// Everything a configured backend must satisfy — absolute paths, a file
/// that resolves, the producer configuration digest, the identity and
/// basis checks a continuation is held to — belongs to
/// `wirk_atlas::SemanticQueryConfig` and is applied there, identically
/// for this caller and for the public search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticQueryRequest {
    pub backend: String,
    #[serde(default)]
    pub backend_args: Vec<String>,
    pub model: String,
}

/// The Route's own presentation budget. `0` means "this Route named no
/// number", which reads as the default — disclosed either way, because a
/// projection that was cut says so with the real total beside it.
///
/// There is deliberately no `bound_max`. `bound` is what the stage
/// requires and is bounded by the Route rather than by the estate;
/// cutting it would let a rendering number decide what a stage was given
/// (BUILD.md §4.7, ruling 0126 F2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationBudget {
    #[serde(default)]
    pub referenced_max: usize,
    #[serde(default)]
    pub reachable_max: usize,
}

impl PresentationBudget {
    pub fn referenced(&self) -> usize {
        if self.referenced_max == 0 {
            REFERENCED_DEFAULT
        } else {
            self.referenced_max
        }
    }

    pub fn reachable(&self) -> usize {
        if self.reachable_max == 0 {
            REACHABLE_DEFAULT
        } else {
            self.reachable_max
        }
    }
}

/// Where the projection file lives and what it is, carried in the
/// reserved `World`.
///
/// `observation` names the file — provenance, deliberately **not**
/// hashed into the World, so re-observing the identical context does not
/// change the stage's resume key. `projection`, `revision` and `format`
/// are what was delivered, and are hashed (see `WorldHash::of`).
///
/// `receipt` is the sha256 over the delivered `ObservationReceipt`'s
/// canonical bytes. It is journal-attributed — it arrives only inside
/// the reserving event's own `World` — and `read_referenced` checks it,
/// so no receipt field can be changed on disk without the read failing
/// (ruling 0126). It is deliberately **not** in `WorldHash::of`: a
/// resume key must not move because the same context was observed twice,
/// and journal attribution is what the integrity property needs (J2, the
/// call ruling 0126 left to this stage).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceProjectionRef {
    pub observation: ObservationId,
    pub projection: ProjectionId,
    pub revision: u64,
    pub format: String,
    pub receipt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObservationId(pub String);

impl ObservationId {
    /// An observation id becomes a filename. Nothing that could leave
    /// `works/<work>/projections/` is one — checked here, at the type,
    /// rather than at each of the two call sites that join it onto a
    /// path.
    pub fn is_well_formed(&self) -> bool {
        !self.0.is_empty()
            && self.0.len() <= 128
            && self
                .0
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }
}

/// How long an item stays true, and nothing about who may read it
/// (BUILD.md §4.1: "authority never arrives through lifetime").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    Standing,
    Working,
}

/// The content identity of one delivered item. W-C1 resolves literal
/// references against a captured generation vector, so every item it
/// produces names the exact generation and Git object it was read at;
/// prior-stage artifact digests arrive with step 5 in W-C2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemIdentity {
    Generation {
        generation: String,
        object_id: String,
    },
    /// A prior stage of this same Work: the Claim whose validation read
    /// the bytes, and the sha256 it recorded then. The digest — not the
    /// path, and not the `ClaimId` beside it — is what makes this
    /// historical evidence identity: an artifact whose bytes have since
    /// been rewritten does not bind at all, it is an explicit
    /// unavailability (BUILD-AMENDMENTS.md, ruling 0124).
    ArtifactDigest { claim: String, digest: String },
}

/// One piece of evidence as delivered: the opaque coordinate the actor
/// can resolve, a bounded summary, how long it stays true, and — never
/// optional — which assembly step put it here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceItem {
    pub coordinate: String,
    pub summary: String,
    pub lifetime: Lifetime,
    pub reason: String,
    pub identity: ItemIdentity,
}

/// Every uncertainty label says whose it is. `Assembly` is the
/// assembler's own statement about how it ran; `Intent` is a reference
/// the authored question or intent made that the assembler could not
/// resolve. Neither is a judgement about the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementOrigin {
    Assembly,
    Intent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub text: String,
    pub attributed_to: StatementOrigin,
}

/// Why a coordinate that was asked for is not delivered. A closed enum,
/// never raw error text: the `Display` of an Atlas or filesystem error
/// carries host paths no scope admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// The captured vector names no published generation for this source.
    GenerationUnavailable,
    /// The generation records the resource, but not as indexed content.
    ResourceExcluded,
    ResourceUnsupported,
    /// The generation records it and the bytes could not be read back at
    /// the generation it was recorded against.
    ResourceUnavailable,
    /// A prior stage's artifact is still readable and its bytes no longer
    /// hash to the digest that Claim validated. The later bytes are never
    /// attributed to the earlier Claim (BUILD-AMENDMENTS.md).
    ArtifactBytesChanged,
    /// A prior stage's artifact cannot be read at all now.
    ArtifactUnreadable,
    /// A prior stage's receipt carries no content identity — a
    /// pre-correction, name-only receipt. A path plus a `ClaimId` is not
    /// historical evidence identity, so it is reported rather than bound.
    ArtifactUnrecorded,
    /// A `GovernedBy` edge touching a bound resource exists and one of
    /// its endpoints or evidence coordinates no longer resolves at the
    /// generation the edge was admitted against.
    GoverningRecordUnresolvable,
}

/// What was left out, and why. Never a coordinate the requester's scope
/// did not already admit: an inadmissible source is a *count*, so a
/// caller can tell "there is nothing here" from "there is something here
/// you may not see" without learning what (the shape
/// `wirk_atlas::AdmissionSummary` and `DisclosureView::withheld` already
/// use).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Omission {
    OverBudget {
        of: String,
        shown: usize,
        total: usize,
    },
    Inadmissible {
        count: usize,
    },
    Unavailable {
        coordinate: String,
        reason: UnavailableReason,
    },
    /// This estate has admitted `count` relationship(s) naming a resource
    /// this projection bound — same source, same path — at a generation
    /// this assembly did not capture, so they were not followed here.
    ///
    /// A count, and only a count, for the same reason `Inadmissible` is
    /// one: it says "there is something here you were not shown" without
    /// naming a coordinate, an id or a foreign identity. It is
    /// deliberately *not* a statement that such a relationship stopped
    /// being true, and deliberately not an invitation to read today's
    /// bytes under a historical coordinate. It exists because the
    /// alternative — saying nothing at all — is read as "this estate
    /// governs nothing here", which is a different fact entirely
    /// (`loop-c2-verify/VERDICT.md` F1, ruling 0128).
    AdmittedAtAnotherEdition {
        count: usize,
    },
}

/// Why this projection's factual coverage is less than complete.
/// Deliberately not a presentation fact: a cut list is `truncated` plus
/// an `Omission::OverBudget`, never a coverage state (ruling 0044, and
/// BUILD.md §4.7 — a rendering budget must not become a completion
/// oracle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageReason {
    /// The authored question or intent named references that resolved
    /// nowhere in the admitted, captured sources.
    UnresolvedReferences,
    /// The request named a source this Work is not bound to.
    InadmissibleSources,
    /// Something the captured vector records could not be read back.
    EvidenceUnavailable,
    /// The estate published under the assembler often enough that it
    /// gave up re-observing; the stage still runs, and is told so.
    ConcurrentPublication,
    /// A relationship this estate admitted names a resource bound here at
    /// an edition outside the captured generation vector. A governing
    /// record that exists and was not delivered is a completeness fact
    /// about this projection, so it moves coverage exactly as the mirror
    /// case — a governing endpoint that no longer resolves — already
    /// does. It is not a judgement about whether the relationship holds
    /// (ruling 0128 F1).
    GovernanceOutsideCapturedEditions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EvidenceCoverage {
    Complete,
    Partial { reason: CoverageReason },
    Degraded { reason: CoverageReason },
}

/// What ranked retrieval actually did, so an empty `referenced` list is
/// never read as "the estate holds nothing" (BUILD.md §4.5).
///
/// Every field is copied from the answer the Atlas query returned. There
/// is no place here to write a mode that was not used or a status that
/// was not reported: `mode` and `semantic` are the labels the query's own
/// `RankingMode` and `SemanticStatus` produce, `semantic_reason` is
/// `SemanticStatus::reason()` verbatim, `editions` are the semantic
/// editions the answer says it ranked through, and `degraded` names the
/// `AnswerCoverage` dimensions that were true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalNote {
    pub mode: String,
    pub semantic: String,
    pub semantic_reason: Option<String>,
    pub editions: Vec<(String, String)>,
    pub degraded: Vec<String>,
    /// How many candidates the ranked query found in total, before any
    /// presentation cut — the honest denominator behind every
    /// `Omission::OverBudget { of: "referenced" }`.
    pub total_candidates: usize,
    pub returned: usize,
}

/// One admitted place this stage may go looking that nothing in the
/// authored text named — a *discovery handle*, not a decorative string.
///
/// `handle` is the token an actor pastes; `source` and `family` are what
/// it addresses; `resources` is how many indexed resources of that family
/// the captured generation actually holds; `fetch` is the exact public
/// command that turns the handle into evidence, which `wirk world show`
/// prints and which really runs (`wirk atlas search` falls back to the
/// injected triple exactly as `wirk atlas resolve` does). Binding one of
/// these into the delivered context is W-C3's `world expand`; being able
/// to *use* it is this wave's, and is tested through the public verb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReachableEntry {
    pub handle: String,
    pub source: String,
    pub family: String,
    pub resources: usize,
    pub fetch: String,
}

/// What one expansion was asked for (W-C3). Authored by the actor of
/// the expanding Run, and carried as content so a revision says what
/// produced it rather than leaving a reader to infer it.
///
/// A `reference` is a `reachable` handle, and only one this Run's own
/// projection chain actually delivered: it is revalidated against the
/// chain before anything is read, so a handle copied out of another
/// Work's context, or invented, addresses nothing (BUILD.md §3.4 — a
/// coordinate confers no authority, and neither does a handle).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpansionRequest {
    /// The terms this expansion ranked for. Present whether the actor
    /// authored them or the stage's own question stood in, and
    /// `authored` says which — a projection never presents a question
    /// the actor did not write as one they did.
    pub question: String,
    pub authored_question: bool,
    /// The `reachable` handle this expansion narrowed to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The actor's own sentence about why. Never fabricated: absent
    /// means the actor wrote none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Which source bytes an expansion was resolved against.
///
/// One variant, and it is the whole W-C3 answer rather than a slot for a
/// second one later: an expansion **preserves the captured generation
/// vector of the revision it expands**. The initial World is immutable
/// and was pinned to one vector; every coordinate in every revision of
/// its chain therefore resolves at exactly that vector, and no revision
/// ever reads today's bytes under the identity of a generation the stage
/// was pinned to (ruling 0126's substituted-provenance refusal, ruling
/// 0128 F1's refusal to attribute later editions to an earlier
/// admission).
///
/// The honest cost is stated on the delivered revision rather than
/// hidden: an expansion sees the estate as it was captured, not as it is
/// now, and a generation that can no longer be read back is an explicit
/// `Omission::Unavailable`, never a silent re-observation at HEAD. A
/// verb that observes a *new* vector would be a different verb
/// delivering a different stage, and it does not exist here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpansionBasis {
    /// Every read pinned to the parent revision's own
    /// `generations`/`publication_revision`, re-admitted under this
    /// Work's bindings as they stand now.
    PreservedCapturedVector,
}

/// A revision that is not an initial assembly: which revision it
/// expands, and what was asked for.
///
/// `parent_projection` is a content id and `parent_observation` names
/// the parent's own file, so the chain is followable in both directions
/// without a join through anything but this Work's own journal. Absent
/// on revision 0 — and absent, not `null`, so an initial projection
/// serializes to exactly the bytes it serialized to before this field
/// existed and keeps the `ProjectionId` its journal already recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpansionRecord {
    pub parent_projection: ProjectionId,
    pub parent_observation: ObservationId,
    /// The Run whose actor asked. Always this projection chain's own
    /// Run: `world expand` reads the injected triple and there is no
    /// argument on it that names another.
    pub expanded_by: String,
    pub request: ExpansionRequest,
    pub basis: ExpansionBasis,
    /// How many items this revision added, and how many candidates it
    /// found that the revision it expands had already bound at the same
    /// coordinate.
    ///
    /// Content, not presentation: "the handle you asked for holds two
    /// resources and this context already had both" is a fact about the
    /// delivered evidence, and a stage that cannot see it reads a new
    /// revision number as though it were new material. `0` added with a
    /// non-zero `already_bound` is an honest and useful answer, and it
    /// is never a reason to refuse the request.
    pub delivered: usize,
    pub already_bound: usize,
}

/// W-C1's delivered context, frozen.
///
/// Not a compatibility shim to be tidied away later: it is the literal
/// definition of what a `wirk.projection/v1` file means, and its
/// canonical bytes are the bytes whose sha256 a C1-era journal recorded.
/// Adding a field here — even a defaulted one — would move the
/// `ProjectionId` of every projection already delivered, which is the
/// one thing an immutable delivered context may never do. A v2 field
/// goes in `ProjectionContent` below and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionContentV1 {
    pub format: String,
    pub compilation_policy: String,
    pub route_edition: String,
    pub waypoint: WaypointId,
    pub revision: u64,
    pub question: String,
    pub generations: Vec<(String, String)>,
    pub publication_revision: u64,
    pub bound: Vec<EvidenceItem>,
    pub assumptions: Vec<Statement>,
    pub unknowns: Vec<Statement>,
    pub omitted: Vec<Omission>,
    pub coverage: EvidenceCoverage,
}

impl ProjectionContentV1 {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ProjectionContentV1 always serializes")
    }

    pub fn projection_id(&self) -> ProjectionId {
        let mut hasher = Sha256::new();
        hasher.update(b"wirk.projection/v1\0");
        hasher.update(self.canonical_bytes());
        ProjectionId(crate::hex_lower(&hasher.finalize()))
    }
}

/// The delivered context. This — and only this — is what `ProjectionId`
/// covers.
///
/// W-C2 adds the rest of the selector set to it: governance and
/// prior-stage items inside `bound`, the ranked `referenced` list, the
/// `reachable` discovery handles, the `retrieval` note, the
/// state-describing `next_action` and the `truncated` presentation flag.
/// That changes the canonical bytes of everything written from here on,
/// so the format tag advances to `wirk.projection/v2` and
/// `ProjectionContentV1` keeps decoding what came before, rather than
/// this struct quietly re-hashing older files under a newer shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionContent {
    pub format: String,
    pub compilation_policy: String,
    /// sha256 over the journaled `waypoint_defs` this Work reserves
    /// against, so a projection says which Route edition produced it.
    pub route_edition: String,
    pub waypoint: WaypointId,
    /// 0 for an initial assembly. Expansion (a new revision on the same
    /// Run) is W-C3; nothing in this wave produces a non-zero value, and
    /// the field exists because the World hash covers it and a later
    /// revision must not collide with revision 0.
    pub revision: u64,
    pub question: String,
    /// The captured admitted generation vector, in delivery order:
    /// `(membership id, generation id)`. Every coordinate in `bound`
    /// resolves at exactly one of these.
    pub generations: Vec<(String, String)>,
    pub publication_revision: u64,
    pub retrieval: RetrievalNote,
    pub bound: Vec<EvidenceItem>,
    /// Ranked hits for the authored question that no authored reference
    /// named. Presentation-budgeted; every cut is disclosed with the real
    /// total.
    pub referenced: Vec<EvidenceItem>,
    pub reachable: Vec<ReachableEntry>,
    pub assumptions: Vec<Statement>,
    pub unknowns: Vec<Statement>,
    pub omitted: Vec<Omission>,
    /// A sentence about the **state of the delivered evidence** —
    /// chosen only by `coverage` and whether `unknowns` is empty, so it
    /// is byte-identical under any budget. It never names a role, never
    /// says the stage is finished, and never says the stage is not
    /// (ruling 0124: "No role taxonomy"; BUILD.md §4.7: a rendering
    /// budget must not become a completion oracle).
    pub next_action: String,
    pub coverage: EvidenceCoverage,
    /// Some presentation list was cut. Always accompanied by an
    /// `Omission::OverBudget` carrying the real total, and never a
    /// coverage fact.
    pub truncated: bool,
    /// W-C3: what this revision expands, and why. `None` — and, on the
    /// wire, *absent* — for every initial assembly.
    ///
    /// Additive-and-skipped rather than a new format tag, the same shape
    /// `ActorWorld::evidence` and `OrientationRequest::semantic` already
    /// take (R2): a revision-0 projection serializes to byte-identical
    /// canonical bytes with this field present in the struct and absent
    /// from the document, so every `ProjectionId` a journal has already
    /// recorded still re-hashes. A tag advance would have moved all of
    /// them, which is the one thing an immutable delivered context may
    /// not do — so the tag stays `wirk.projection/v2` and the *document*
    /// carries the field only when there is one to carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion: Option<ExpansionRecord>,
}

/// What a projection file was found to contain, at the format it
/// declares. A reader that meets a tag it does not know says
/// `ProjectionUnavailable::UnknownFormat` rather than guessing; a reader
/// that meets one it does know decodes it under *that* format's own
/// frozen shape and re-hashes it under that format's own domain tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeliveredContent {
    /// Tried first: a v2 file has fields `ProjectionContentV1` denies,
    /// and a v1 file lacks fields `ProjectionContent` requires, so the
    /// two shapes are mutually exclusive and the order only decides
    /// which error a malformed file reports.
    V2(Box<ProjectionContent>),
    V1(Box<ProjectionContentV1>),
}

impl DeliveredContent {
    pub fn format(&self) -> &str {
        match self {
            Self::V2(content) => content.format.as_str(),
            Self::V1(content) => content.format.as_str(),
        }
    }

    pub fn revision(&self) -> u64 {
        match self {
            Self::V2(content) => content.revision,
            Self::V1(content) => content.revision,
        }
    }

    pub fn waypoint(&self) -> &WaypointId {
        match self {
            Self::V2(content) => &content.waypoint,
            Self::V1(content) => &content.waypoint,
        }
    }

    pub fn projection_id(&self) -> ProjectionId {
        match self {
            Self::V2(content) => content.projection_id(),
            Self::V1(content) => content.projection_id(),
        }
    }

    /// The format tag this variant is the definition of. Checked against
    /// the file's own `format` string, so a document carrying v2 fields
    /// under a v1 tag (or the reverse) is refused rather than read
    /// through under whichever shape happened to parse.
    pub fn declared_format(&self) -> &'static str {
        match self {
            Self::V2(_) => PROJECTION_FORMAT,
            Self::V1(_) => PROJECTION_FORMAT_V1,
        }
    }

    pub fn v2(&self) -> Option<&ProjectionContent> {
        match self {
            Self::V2(content) => Some(content),
            Self::V1(_) => None,
        }
    }
}

/// Delivered beside the content, hashed into nothing (BUILD.md §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationReceipt {
    pub observation: ObservationId,
    /// Unix milliseconds. One window for the whole assembly: ruling
    /// 0120's frozen-as-observed qualification is inspectable only if
    /// the instant is delivered.
    pub observed_at: u64,
    pub observation_window_ms: u64,
    /// How many observe/re-check laps this assembly cost.
    pub laps: u32,
}

impl ObservationReceipt {
    /// The canonical bytes: `serde_json` over this struct's own fixed
    /// field order, exactly as `ProjectionContent::canonical_bytes`
    /// does. `deny_unknown_fields` on the struct is what makes the
    /// digest total — a file carrying a field this binary does not know
    /// fails to parse rather than round-tripping past the check.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ObservationReceipt always serializes")
    }

    /// The digest the journal reference carries. Domain-separated from
    /// `ProjectionId`'s tag so a content digest can never be presented
    /// as a receipt digest.
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"wirk.projection.receipt/v1\0");
        hasher.update(self.canonical_bytes());
        crate::hex_lower(&hasher.finalize())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionFile {
    pub content: DeliveredContent,
    pub receipt: ObservationReceipt,
}

impl ProjectionContent {
    /// The canonical bytes: `serde_json` over this struct's own fixed
    /// field order, no whitespace, UTF-8. Every map inside is a `Vec` of
    /// pairs in delivery order or a `BTreeMap`, so there is no
    /// unspecified iteration order anywhere in the encoding.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ProjectionContent always serializes")
    }

    pub fn projection_id(&self) -> ProjectionId {
        let mut hasher = Sha256::new();
        hasher.update(b"wirk.projection/v2\0");
        hasher.update(self.canonical_bytes());
        ProjectionId(crate::hex_lower(&hasher.finalize()))
    }
}

/// Why a referenced projection cannot be delivered. Closed, and never a
/// silent empty projection: a corrupt or missing file is an explicit
/// unavailability, and is never regenerated against today's estate
/// (ruling 0124).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionUnavailable {
    /// The reference names an observation id that is not a filename.
    MalformedReference,
    /// The referencing event exists; the file it names does not.
    FileMissing,
    /// The file exists and is not a readable projection file.
    FileUnreadable,
    /// The file parses and its content does not re-hash to the id the
    /// journal recorded.
    ContentMismatch,
    /// The file parses and names a content format this binary does not
    /// decode.
    UnknownFormat,
    /// The file parses, its content is the content the journal names,
    /// and its **receipt** is not the receipt the journal names: some
    /// provenance byte — the instant, the window, the lap count — was
    /// changed after delivery.
    ReceiptMismatch,
    /// The reference disagrees with the content it names about what was
    /// delivered: the journaled `format` or `revision` is not the one in
    /// the file. The reference is what `WorldHash` covers, so a
    /// reference that does not describe its own content is refused
    /// rather than read through.
    ReferenceMismatch,
}

impl ProjectionUnavailable {
    pub fn reason(self) -> &'static str {
        match self {
            Self::MalformedReference => "malformed-reference",
            Self::FileMissing => "file-missing",
            Self::FileUnreadable => "file-unreadable",
            Self::ContentMismatch => "content-mismatch",
            Self::UnknownFormat => "unknown-format",
            Self::ReceiptMismatch => "receipt-mismatch",
            Self::ReferenceMismatch => "reference-mismatch",
        }
    }
}

#[derive(Debug, Error)]
pub enum ProjectionWriteError {
    #[error("projection io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is not a usable observation id")]
    MalformedObservation(String),
    #[error("a projection is already written at observation {0}")]
    AlreadyWritten(String),
    /// The rename already made the file visible; only the containing
    /// directory's own sync failed. Reported as visible-but-unconfirmed,
    /// exactly as `AtlasStore::append_relationship` reports the identical
    /// window — never as a bare I/O error indistinguishable from "never
    /// wrote".
    #[error("projection {0} is visible; directory sync failed: {1}")]
    DurabilityUncertain(String, String),
}

/// `works/<work_id>/projections/`.
pub fn projections_dir(estate_root: &Path, work_id: &crate::WorkId) -> PathBuf {
    estate_root
        .join("works")
        .join(&work_id.0)
        .join("projections")
}

pub fn projection_path(
    estate_root: &Path,
    work_id: &crate::WorkId,
    observation: &ObservationId,
) -> Option<PathBuf> {
    observation
        .is_well_formed()
        .then(|| projections_dir(estate_root, work_id).join(format!("{}.json", observation.0)))
}

impl ProjectionFile {
    /// Writes the file, durably, **before** anything references it, with
    /// the same temp-file/fsync/rename/directory-fsync discipline
    /// `AtlasStore` already uses for every durable artifact it owns (R2,
    /// `wirk-atlas/src/store.rs`) — not a second durability protocol,
    /// and no new dependency.
    ///
    /// `create_new` on the temp file and on the rename target: the file
    /// name is the observation id, one observation writes one file, and
    /// an id that already exists is a bug rather than an overwrite.
    pub fn write_new(
        &self,
        estate_root: &Path,
        work_id: &crate::WorkId,
    ) -> Result<PathBuf, ProjectionWriteError> {
        let observation = &self.receipt.observation;
        let Some(final_path) = projection_path(estate_root, work_id, observation) else {
            return Err(ProjectionWriteError::MalformedObservation(
                observation.0.clone(),
            ));
        };
        let dir = projections_dir(estate_root, work_id);
        fs::create_dir_all(&dir)?;
        // `fs::rename` replaces its destination, so `create_new` on the
        // temp file alone does not make the *final* path write-once —
        // watched fail as `a_projection_file_is_never_rewritten`. The
        // daemon mints one observation id per assembly and names the file
        // after it, so a name that already exists is a minting bug, not a
        // rewrite to perform.
        if final_path.try_exists().unwrap_or(true) {
            return Err(ProjectionWriteError::AlreadyWritten(observation.0.clone()));
        }
        let bytes = serde_json::to_vec(self).expect("ProjectionFile always serializes");
        let temp = dir.join(format!(".tmp-{}", observation.0));
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&temp, &final_path)?;
        File::open(&dir)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                ProjectionWriteError::DurabilityUncertain(observation.0.clone(), error.to_string())
            })?;
        Ok(final_path)
    }

    /// Reads the file the reference names and proves it is the delivered
    /// content: the parsed `content` must re-hash to `reference.projection`.
    ///
    /// Reading is the only way a projection is ever obtained. There is no
    /// path from a `ProjectionId` to a file: the file is found under
    /// `works/<work_id>/`, from a reference this Work's own journal
    /// carries, so a copied id reads nothing (BUILD.md §3.4).
    pub fn read_referenced(
        estate_root: &Path,
        work_id: &crate::WorkId,
        reference: &EvidenceProjectionRef,
    ) -> Result<ProjectionFile, ProjectionUnavailable> {
        let Some(path) = projection_path(estate_root, work_id, &reference.observation) else {
            return Err(ProjectionUnavailable::MalformedReference);
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProjectionUnavailable::FileMissing);
            }
            Err(_) => return Err(ProjectionUnavailable::FileUnreadable),
        };
        let file: ProjectionFile =
            serde_json::from_slice(&bytes).map_err(|_| ProjectionUnavailable::FileUnreadable)?;
        // The declared tag must be the tag of the shape that decoded.
        // A document carrying v2 fields under a v1 tag parses as
        // `V2` — and is refused here, rather than being re-hashed under
        // a domain tag its journal never recorded.
        if file.content.format() != file.content.declared_format() {
            return Err(ProjectionUnavailable::UnknownFormat);
        }
        if file.content.projection_id() != reference.projection {
            return Err(ProjectionUnavailable::ContentMismatch);
        }
        // The reference must describe the content it names. `format` and
        // `revision` are duplicated into the reference because
        // `WorldHash::of` covers them there; a duplicate that disagrees
        // with the file is a reference that does not describe its own
        // delivery, and is refused rather than read through (ruling
        // 0126: the reference's format and revision must agree with the
        // actual content, not merely with this binary's current
        // constant).
        if file.content.format() != reference.format
            || file.content.revision() != reference.revision
        {
            return Err(ProjectionUnavailable::ReferenceMismatch);
        }
        if file.receipt.observation != reference.observation {
            return Err(ProjectionUnavailable::ContentMismatch);
        }
        // Every remaining receipt byte — the instant, the window, the
        // laps — at once. The observation id above is checked separately
        // only because a wrong id means the wrong *file*; this is the
        // check that makes the receipt unforgeable-without-the-journal
        // as a whole.
        if file.receipt.digest() != reference.receipt {
            return Err(ProjectionUnavailable::ReceiptMismatch);
        }
        Ok(file)
    }
}
