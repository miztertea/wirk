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
//! `reachable` handles and the retrieval note; W-C3 adds expansion;
//! W-C4 adds `consulted` — this Work's own recorded findings and the
//! estate's genuinely settled EstateLocal publications — and
//! `findings_index`, the actual scoped health of the index those
//! publications were read from, frozen into the same document.
//!
//! Both are *required* fields: unlike `expansion`, which is legitimately
//! absent from a revision that expands nothing, every assembly from here
//! on either consults or says why it could not, so there is no honest
//! document in which they are missing. That is why the format tag
//! advances (`PROJECTION_FORMAT`) and `ProjectionContentV2` below is
//! frozen exactly as C3 wrote it (ruling 0124: "no unreachable event
//! kinds or empty future schema pretending implemented semantics" — this
//! schema is implemented, and nothing here is a slot for a later wave).

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
/// unless there is one: a revision-0 projection written by that binary
/// serializes to the same bytes a C2 binary wrote, and re-hashes to the
/// `ProjectionId` its journal already recorded. The rule is the field's
/// shape, not the wave number — an added field advances the tag exactly
/// when it moves the bytes of a document that could already exist.
///
/// W-C4 adds `consulted` and `findings_index`, which are *required* on
/// every document this binary writes, so it does advance the tag, by
/// exactly the same rule.
pub const PROJECTION_FORMAT: &str = "wirk.projection/v3";

/// W-C2/C3's format, still read. `ProjectionContentV2` below is frozen
/// for the same reason `ProjectionContentV1` is: its canonical bytes are
/// the bytes whose sha256 a C2- or C3-era journal recorded.
pub const PROJECTION_FORMAT_V2: &str = "wirk.projection/v2";

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
pub const ASSEMBLY_POLICY: &str = "wirk.assembly/v3";

/// W-C2/C3's selector set, still named by every projection they wrote.
pub const ASSEMBLY_POLICY_V2: &str = "wirk.assembly/v2";

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
    /// This stage's ranked-retrieval result capacity (ruling 0171), held
    /// apart from `budget`, which is only how much of that result this
    /// stage wants *rendered*. `None` is not "no capacity" — it is "this
    /// Route did not author one", and the assembler derives the World's
    /// documented default from it, the same way `wirk atlas search`
    /// derives one from an unset `--capacity`.
    ///
    /// Additive and skip-serialized, the same shape `semantic` above and
    /// `ProjectionContentV2::expansion` already take (R2): a Route that
    /// authors no capacity serializes to exactly the bytes it serialized
    /// to before this field existed, so `route_edition` never moves for
    /// an existing Route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u64>,
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

/// Where inside the delivered resource the summary was actually taken
/// from, when it was taken from a place a match located rather than from
/// the head (ruling 0142).
///
/// The same block `atlas search` delivers beside a hit as `evidence`,
/// and it means the same thing here: `coordinate` is a supported exact
/// coordinate `wirk atlas resolve` returns those same committed bytes
/// for, so the summary — a *presentation* string, newlines flattened —
/// has an exact citation that resolves the source it was made from. The
/// item's own `coordinate` above is untouched and still names the whole
/// resource or ranked unit the item was delivered as.
///
/// `matched_terms` are terms that really are inside the shown bytes. An
/// item nothing lexical located anything in carries no `ShownEvidence`
/// at all rather than an empty one, so a semantically ranked row is
/// never dressed up as a term match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShownEvidence {
    pub coordinate: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
    pub matched_terms: Vec<String>,
    /// False only where no bounded summary could carry the whole match:
    /// the matched token is itself at least the summary budget.
    pub whole_match_shown: bool,
}

/// One piece of evidence as delivered: the opaque coordinate the actor
/// can resolve, a bounded summary, how long it stays true, and — never
/// optional — which assembly step put it here.
///
/// `summary` is a presentation string and always was: it is bounded and
/// its newlines are flattened, so it is not the resource's exact bytes.
/// What ruling 0142 adds is that when it is taken from a place a match
/// located, `shown` names those bytes exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceItem {
    pub coordinate: String,
    pub summary: String,
    pub lifetime: Lifetime,
    pub reason: String,
    pub identity: ItemIdentity,
    /// Ruling 0142: the exact source span the summary was taken from,
    /// when it was chosen around a match rather than read off the head.
    ///
    /// Additive-and-skipped rather than a new format tag, exactly the
    /// shape `ProjectionContent::expansion` already takes (R2): an item
    /// with no located match serializes to byte-identical canonical
    /// bytes with this field present in the struct and absent from the
    /// document, so every projection already written — v1, v2 and v3
    /// alike — still re-hashes to the `ProjectionId` its journal
    /// recorded. A tag advance would have moved all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<ShownEvidence>,
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
    /// The estate's findings index could not be read at all at this
    /// assembly, so the consulted set is whatever this Work's own journal
    /// holds and nothing else. Named as the one coordinate an actor can
    /// act on — never a filesystem path, and never the error's own text,
    /// which carries a path no scope admitted (BUILD.md §9).
    FindingsIndexUnreadable,
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
    /// `count` resource(s) of the generations this assembly captured
    /// record a genuine extraction failure: the extractor was asked for
    /// retrieval units and could not produce them, so those bytes are in
    /// the source and in no index. Part of the corpus this projection
    /// describes was never searchable at the generation it names.
    ///
    /// A count, and only a count, for the reason `Inadmissible` is one:
    /// the failing resource is not something this projection delivered,
    /// and naming it would hand over a path the assembly never bound.
    /// The extractor's own diagnostic never travels either — it carries
    /// a budget and a size that say more about the estate than the
    /// requester asked for. `wirk atlas status` is where a requester
    /// already admitted to the source reads the detail.
    ///
    /// Deliberately **not** every resource a generation did not index:
    /// a family no edition claims and a path the extractor deliberately
    /// refuses are the declared shape of the corpus, and counting them
    /// here "would obscure the map" (ruling 0135, qualification to
    /// R12).
    SourceExtractionIncomplete {
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
    /// The findings index this assembly's consulted set was read from is
    /// not a projection this estate can attest is complete: it has never
    /// been reconciled in this daemon, its rows' directory entry is not
    /// confirmed on disk, it is missing rows the journals hold, or it
    /// could not be read at all. A parsable index is not proof of
    /// synchronization (ruling 0124), and a missing one is not an empty
    /// estate (ruling 0137) — so the projection says its consulted set
    /// may be short, rather than presenting it as everything the estate
    /// holds.
    IndexCannotAttestCompleteness,
    /// The findings index could not be read at all at this assembly, so
    /// the consulted set is whatever this Work's own journal holds and
    /// nothing else. A closed reason: the read error's own text carries
    /// a filesystem path no scope admitted and never travels (BUILD.md
    /// §9).
    FindingsIndexUnreadable,
    /// A generation in the captured vector records a resource the
    /// extractor could not turn into retrieval units at all, so part of
    /// the corpus this projection describes was never searchable at the
    /// generation it names. A fact about the captured vector, which an
    /// expansion inherits wholesale — so, like every other source
    /// reason, it stays true of every later revision of the chain and
    /// only a newly captured vector can clear it (ruling 0135 C4-R12).
    SourceExtractionIncomplete,
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
///
/// `total_candidates` is the ranked query's own result-set size at its
/// capacity, never the rendering budget's — a presentation cut is
/// `Omission::OverBudget { of: "referenced" }` on the delivered list, not
/// a smaller `total_candidates` (ruling 0172). `capacity`, when present,
/// says what that result-set size was bounded *by*, so a reader is never
/// left to infer the difference between "this ranker's bounded result set
/// is exhausted at this capacity" and "a rendering budget cut the list".
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
    /// The result capacity this assembly's ranked query ran at (ruling
    /// 0171, ruling 0172), or absent for an answer no capacity bounded —
    /// a lexical one, a query that could not run, or one produced before
    /// this field existed. A missing field is unspecified historical
    /// data, never an invented current default.
    ///
    /// Additive and skip-serialized, the shape `expansion` and
    /// `OrientationRequest::semantic` already use (R2): a document
    /// without it serializes to exactly the bytes it did before this
    /// field existed, so no recorded `ProjectionId` moves for a
    /// projection that never carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<RetrievalCapacityNote>,
}

/// What a ranked query's own result capacity was, copied from the answer
/// that ran it — the disclosure ruling 0171 requires on every real World
/// consumer, not only on the public search answer and the CLI (ruling
/// 0172).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalCapacityNote {
    /// The `top_k` this query actually ranked at.
    pub capacity: u64,
    /// Where that capacity came from: `requested-limit`,
    /// `requested-limit-bounded`, or `explicit` — `CapacitySource::label`
    /// verbatim.
    pub source: String,
    /// The policy this capacity was decided under, restated from the
    /// edition this answer ranked through.
    pub policy: String,
    /// That policy's operational bound.
    pub max: u64,
    /// The result set filled the capacity: relevant rows may exist beyond
    /// this query's budget, and a deeper answer is a *new query* at a
    /// larger capacity — never something a smaller rendering budget can
    /// reach.
    pub reached: bool,
    /// The native ranker returned fewer rows than the capacity allowed,
    /// so this query's bounded result set is exhausted. A fact about
    /// *this ranker at this capacity over the admitted view*, never a
    /// claim that the estate holds no other relevant information.
    pub resultset_exhausted: bool,
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

/// Where one consulted record came from, and it is the whole set of
/// routes: this Work's own journal, or a genuinely settled EstateLocal
/// publication reached through the estate publication route. There is no
/// third variant and no cross-estate one — estate isolation is total and
/// `Shared` is not a variant at any layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsultedOrigin {
    OwnWork,
    EstatePublication,
}

/// The named record's own settlement standing, as the route that
/// delivered it reported it. A *receipt fact*, deliberately separate
/// from the generation relation below and from whether the record's own
/// evidence still resolves: three different facts, and collapsing any
/// two of them is how a projection starts deciding what P3 refuses to
/// decide (ruling 0135).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConsultedStatus {
    /// Raised and not settled. The honest state of most of what a Work
    /// records about itself, and never a defect.
    Provisional,
    /// Settled, by the named compiled settlement class.
    Settled { class: String },
    /// A later record in the same origin supersedes it. It is still
    /// delivered: a superseded record is history, not an error.
    Superseded { by: String },
}

/// How the generations this record was raised against compare with the
/// vector this assembly captured. **It states a generation relation and
/// never a truth value** (BUILD.md §6): a changed generation does not
/// mean the claim became false, and an unchanged one is not a proof that
/// it was ever true. P3 records the pair and refuses to collapse it;
/// deciding is P4 invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationRelation {
    /// Every generation this record names is one the captured vector
    /// still names for the same membership.
    RecordedStillPublished,
    /// At least one membership this record names is published here at a
    /// different generation than the one it was recorded against.
    RecordedSuperseded,
    /// The record names no generation this assembly captured a
    /// membership for — including the ordinary case of a record whose
    /// evidence is journal-side and names no source at all.
    Unknown,
}

/// One recorded evidence coordinate of a consulted finding, delivered as
/// **identity only**: the opaque coordinate an actor can resolve for
/// themselves, and the exact generation and object it was recorded
/// against. No bytes are read here and no summary is derived — consulting
/// a record is not a read-through onto its evidence (BUILD.md §4.1).
///
/// An entry reaches this list only if the *current* requester's own
/// disclosure view admits it and this assembly's own admission step
/// captured its membership at exactly this generation. Raise-time
/// admission is frozen provenance and is explicitly not transferable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsultedEvidence {
    pub coordinate: String,
    pub generation: String,
    pub object_id: String,
}

/// A typed disagreement a consulted record recorded against something
/// **this projection actually delivered**.
///
/// No prose is read, matched or compared: the only thing that makes this
/// a contradiction is that the record's own `contradicts` list names a
/// coordinate that is in this projection's `bound` list. It is attributed
/// to the finding by being carried on it, and it endorses nothing: the
/// record is not made true by disagreeing, and the bound item is not made
/// false by being disagreed with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contradiction {
    /// The delivered coordinate the record names.
    pub coordinate: String,
    pub text: String,
}

/// One record this stage was handed, from one of the two routes in
/// `ConsultedOrigin`.
///
/// `claim` is the *captioned* sentence the daemon already renders
/// everywhere else — `recorded claim: <text>, unverified` — and
/// `claim_verified` is the `false` beside it. The projection carries what
/// was recorded and never asserts it (BUILD.md §6, acceptance 17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsultedFinding {
    pub id: String,
    pub origin: ConsultedOrigin,
    /// The Work whose journal holds the record. For an own record this
    /// Work; for a publication, the producing Work — which the
    /// publication route has already admitted to this requester whole,
    /// or the row would not be here at all.
    pub work: String,
    pub kind: String,
    pub claim: String,
    /// Always `false`. Carried rather than omitted, because a reader
    /// that has to *infer* that a recorded sentence is unverified is a
    /// reader that will eventually forget to.
    pub claim_verified: bool,
    pub status: ConsultedStatus,
    /// `(membership id, generation id)` for every source generation this
    /// record was recorded against, in recorded order.
    pub recorded_generations: Vec<(String, String)>,
    /// The generation this assembly captured for each of those same
    /// memberships. Present so the pair is inspectable rather than
    /// summarized away by the relation below.
    pub current_generations: Vec<(String, String)>,
    pub generation_relation: GenerationRelation,
    pub evidence: Vec<ConsultedEvidence>,
    /// Recorded entries the *current* requester's disclosure view
    /// refuses. A count, and only a count: no id, alias, path,
    /// generation or coordinate travels in it.
    pub evidence_withheld: usize,
    /// Recorded entries this assembly does not deliver as a coordinate —
    /// a journal or relation reference, an entry recorded unavailable at
    /// raise time, or one recorded against a membership or generation
    /// this assembly did not capture. Also a count, for the same reason.
    pub evidence_not_delivered: usize,
    pub contradictions: Vec<Contradiction>,
    pub reason: String,
}

/// What the estate's findings index was, at this assembly, in the terms
/// a scoped reader is entitled to.
///
/// A 1:1 map of the projection state the daemon's own `IndexHealth`
/// records and its scoped `atlas findings` surface already renders, plus
/// the two states that surface cannot have: `Unobserved`, for an
/// assembly that never got as far as looking, and `Unreadable`, for a
/// read that failed. **No administrative count, detail or path** —
/// scoped readers get the projection's state and nothing about the
/// estate's contents (ruling 0135: "do not expose admin data"; ruling
/// 0124: "scoped projections must not gain administrative pending row
/// counts").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingsIndexState {
    /// This assembly did not observe the index at all — the degraded
    /// shape, where the estate moved under the assembler on every lap
    /// and nothing was read. Never "there is nothing there".
    Unobserved,
    /// No reconciliation has run in this daemon yet. An index nobody has
    /// checked is not an index known to be complete.
    Unreconciled,
    /// Every row this estate's journals support is in the file.
    Synchronized,
    /// The rows are visible to a fresh reader and their directory entry
    /// is not confirmed on disk.
    DurabilityUnconfirmed,
    /// The index is missing rows the journals hold, or its completeness
    /// cannot be established — including the file that a health record
    /// was formed over having since gone away (ruling 0137).
    Behind,
    /// The index could not be read at all at this assembly.
    Unreadable,
}

/// The index note, frozen into this document at assembly and never
/// retro-corrected: a later query does not change what a delivered
/// projection said, and an expansion re-observes and records its own note
/// in its own revision, so a health change is visible as a difference
/// between two frozen records rather than a silent edit (BUILD.md §9).
///
/// Its observation instant is `ObservationReceipt.observed_at` — the one
/// frozen window this document already carries. There is deliberately no
/// second timestamp here (ruling 0120).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingsIndexNote {
    pub state: FindingsIndexState,
    /// Whether the consulted set below may be read as a complete
    /// projection of the estate's journals. True for `Synchronized` and
    /// for nothing else — the same single field, with the same meaning,
    /// that `wirk atlas findings` puts in front of a scoped reader.
    pub complete: bool,
}

impl FindingsIndexNote {
    pub fn unobserved() -> Self {
        Self {
            state: FindingsIndexState::Unobserved,
            complete: false,
        }
    }
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

/// W-C2/C3's delivered context, frozen.
///
/// Not a compatibility shim: it is the literal definition of what a
/// `wirk.projection/v2` file means, and its canonical bytes are the
/// bytes whose sha256 a C2- or C3-era journal recorded. A v3 field goes
/// in `ProjectionContent` below and nowhere else, for exactly the reason
/// `ProjectionContentV1` says.
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
pub struct ProjectionContentV2 {
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

impl ProjectionContentV2 {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ProjectionContentV2 always serializes")
    }

    pub fn projection_id(&self) -> ProjectionId {
        let mut hasher = Sha256::new();
        hasher.update(b"wirk.projection/v2\0");
        hasher.update(self.canonical_bytes());
        ProjectionId(crate::hex_lower(&hasher.finalize()))
    }

    /// The same delivered context in the current shape, for the one
    /// caller that needs it: `world expand` reads its parent revision
    /// through this so a chain begun before W-C4 stays expandable. The
    /// two fields it cannot have are exactly what this revision never
    /// observed, said as that — the expansion re-observes both for
    /// itself and overwrites them on the revision it writes.
    pub fn lift(&self) -> ProjectionContent {
        ProjectionContent {
            format: self.format.clone(),
            compilation_policy: self.compilation_policy.clone(),
            route_edition: self.route_edition.clone(),
            waypoint: self.waypoint.clone(),
            revision: self.revision,
            question: self.question.clone(),
            generations: self.generations.clone(),
            publication_revision: self.publication_revision,
            retrieval: self.retrieval.clone(),
            bound: self.bound.clone(),
            referenced: self.referenced.clone(),
            reachable: self.reachable.clone(),
            assumptions: self.assumptions.clone(),
            unknowns: self.unknowns.clone(),
            omitted: self.omitted.clone(),
            next_action: self.next_action.clone(),
            coverage: self.coverage,
            truncated: self.truncated,
            expansion: self.expansion.clone(),
            consulted: Vec::new(),
            findings_index: FindingsIndexNote::unobserved(),
        }
    }
}

/// The delivered context. This — and only this — is what `ProjectionId`
/// covers.
///
/// W-C4 adds `consulted` and `findings_index` to it: what recorded
/// learning this stage was handed, and what the index those publications
/// were read from actually was at that instant. Both are required on
/// every document written from here on, which changes the canonical
/// bytes of all of them — so the format tag advances to
/// `wirk.projection/v3` and `ProjectionContentV2`/`ProjectionContentV1`
/// keep decoding what came before, rather than this struct quietly
/// re-hashing older files under a newer shape.
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
    /// W-C4: the recorded learning this stage was handed — this Work's
    /// own findings and the estate's settled EstateLocal publications
    /// this requester independently admits, in that order, each with the
    /// route that delivered it.
    ///
    /// Required, and empty is a real answer: an empty list beside a
    /// `findings_index` that says `Synchronized` means this estate holds
    /// nothing for this requester, and beside anything else it means the
    /// index could not attest that. The two are read together, which is
    /// why they are one document and one frozen observation.
    pub consulted: Vec<ConsultedFinding>,
    /// W-C4: what the findings index was when the list above was read.
    pub findings_index: FindingsIndexNote,
}

/// What a projection file was found to contain, at the format it
/// declares. A reader that meets a tag it does not know says
/// `ProjectionUnavailable::UnknownFormat` rather than guessing; a reader
/// that meets one it does know decodes it under *that* format's own
/// frozen shape and re-hashes it under that format's own domain tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeliveredContent {
    /// Tried in order, and the shapes are mutually exclusive rather than
    /// merely ordered: every one of them is `deny_unknown_fields`, so a
    /// v3 document carries two fields `ProjectionContentV2` refuses and a
    /// v2 document lacks two `ProjectionContent` requires — and the same
    /// argument one version down. The order only decides which error a
    /// malformed file reports.
    V3(Box<ProjectionContent>),
    V2(Box<ProjectionContentV2>),
    V1(Box<ProjectionContentV1>),
}

impl DeliveredContent {
    pub fn format(&self) -> &str {
        match self {
            Self::V3(content) => content.format.as_str(),
            Self::V2(content) => content.format.as_str(),
            Self::V1(content) => content.format.as_str(),
        }
    }

    pub fn revision(&self) -> u64 {
        match self {
            Self::V3(content) => content.revision,
            Self::V2(content) => content.revision,
            Self::V1(content) => content.revision,
        }
    }

    pub fn waypoint(&self) -> &WaypointId {
        match self {
            Self::V3(content) => &content.waypoint,
            Self::V2(content) => &content.waypoint,
            Self::V1(content) => &content.waypoint,
        }
    }

    pub fn projection_id(&self) -> ProjectionId {
        match self {
            Self::V3(content) => content.projection_id(),
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
            Self::V3(_) => PROJECTION_FORMAT,
            Self::V2(_) => PROJECTION_FORMAT_V2,
            Self::V1(_) => PROJECTION_FORMAT_V1,
        }
    }

    /// The content in this binary's current shape, if this document is
    /// one it wrote.
    pub fn current(&self) -> Option<&ProjectionContent> {
        match self {
            Self::V3(content) => Some(content),
            Self::V2(_) | Self::V1(_) => None,
        }
    }

    /// The content in the current shape, **lifting** an older document
    /// that carries an expansion chain rather than refusing it: a Run
    /// whose revision 0 was written before W-C4 stays expandable, and
    /// the revision the expansion writes re-observes for itself. A v1
    /// document has no chain at all and is `None`, exactly as before.
    pub fn expandable(&self) -> Option<ProjectionContent> {
        match self {
            Self::V3(content) => Some((**content).clone()),
            Self::V2(content) => Some(content.lift()),
            Self::V1(_) => None,
        }
    }

    /// The discovery handles this revision delivered, whichever format
    /// it was written in — a handle offered by a pre-W-C4 revision of a
    /// chain is still one this Run's own context delivered.
    pub fn reachable(&self) -> &[ReachableEntry] {
        match self {
            Self::V3(content) => &content.reachable,
            Self::V2(content) => &content.reachable,
            Self::V1(_) => &[],
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
        hasher.update(b"wirk.projection/v3\0");
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
