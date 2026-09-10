//! wirk-core: the crate wirk-herdr, wirk-atlas, and wirk depend on and
//! that depends on nothing internal (0001 D7). Its manifest carries a
//! deny-list of external dependencies (0022 D71: no Herdr/socket/RPC
//! shaped names), not zero-deps — `wirk/tests/boundary.rs` enforces the
//! narrowed reading.
//!
//! W1 (`knowledge/work/p1-executor-design/orient/build-brief.md` §3 W1):
//! identity newtypes, `Work`/`WorkState`, `Route`/`Waypoint` definition
//! types, and the `World` a Waypoint receives. W2 (§3 W2, this addition):
//! `Run`/`RunState`/`FailureCause`, `Claim`/`ExecutionTriple` (moved into
//! W1)/`ClaimVerdict`/`ClaimRefusal`, `Event`/`EventKind`, `WorldHash::of`'s
//! SHA-256 hashing, the `Run`-level reducer `Run::apply`, the `Executor`
//! trait, and the D9 contract tests. `fold` and `validate_claim` were
//! stubs here — item 2's journal store and item 3's claim validation
//! (this file, W1) landed their real bodies (build-brief.md §2). W3
//! (build-brief.md §3 W3, ruling
//! 0026): `ClaimRefusal::OutOfBoundary`, `Work.repositories` as
//! `Vec<RepositoryBinding>`, `Claim.kind`/`ClaimKind`,
//! `FailureCause.detail` — type-level answers to inherited defects 280,
//! 288, 283, 275; validator bodies stay item 3's.

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// P3, ruling 0145: Work-owned declared outputs — the durable, daemon-
/// derived area a Run's actor writes a declared output into when that
/// output cannot live in the Run's own checkout (a `Read` execution
/// binding). Its own module for the same reason `projection` is one: a
/// self-contained storage contract (name rules, derived addresses,
/// containment, write-once snapshot) rather than another face of
/// `Work`/`Run`/`Event`.
pub mod outputs;
pub use outputs::ArtifactStore;

/// P3 W-C1: the stage projection — the delivered, immutable, inspectable
/// context an orienting Waypoint received. Its own module because it is
/// a self-contained content contract (types, canonical bytes, identity,
/// write-once file) rather than another face of `Work`/`Run`/`Event`.
mod projection;
pub use projection::{
    ASSEMBLY_POLICY, ASSEMBLY_POLICY_V1, ASSEMBLY_POLICY_V2, ConsultedEvidence, ConsultedFinding,
    ConsultedOrigin, ConsultedStatus, Contradiction, CoverageReason, DeliveredContent,
    EvidenceCoverage, EvidenceItem, EvidenceProjectionRef, ExpansionBasis, ExpansionRecord,
    ExpansionRequest, FindingsIndexNote, FindingsIndexState, GenerationRelation, ItemIdentity,
    Lifetime, ObservationId, ObservationReceipt, Omission, OrientationRequest, PROJECTION_FORMAT,
    PROJECTION_FORMAT_V1, PROJECTION_FORMAT_V2, PresentationBudget, ProjectionContent,
    ProjectionContentV1, ProjectionContentV2, ProjectionFile, ProjectionId, ProjectionUnavailable,
    ProjectionWriteError, REACHABLE_DEFAULT, REFERENCED_DEFAULT, ReachableEntry,
    RetrievalCapacityNote, RetrievalNote, SemanticQueryRequest, ShownEvidence, Statement,
    StatementOrigin, UnavailableReason, projection_path, projections_dir,
};

// ---- Identity ----------------------------------------------------------
// Newtypes so a WorkId can't be handed where a RunId is expected
// (core.md §1). R2: shape reused verbatim from orient/core.md.

/// ULID; adopted from sergeant's `Work.id` (domain/work.rs:321-322);
/// minted by wirkd at submit (orient/core.md line 16).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkId(pub String);

/// A Route's own name (definition, not attempt); sergeant resolved
/// `WorkflowDefinition` by name (domain/workflow.rs:2679-2691;
/// orient/core.md line 18).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RouteId(pub String);

/// Derived `{route_id}/{key}`, stable across Run/replay; unlike
/// sergeant's `StageBinding{stage_id, index}` (domain/workflow.rs:460-467),
/// wirk drops the index (orient/core.md line 20).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WaypointId(pub String);

/// ULID per attempt; adopted from sergeant's `ExecutionRecord.execution_id`
/// (domain/execution.rs:37-38); `native_id` (execution.rs:41-43) dropped,
/// Herdr's pane binding is wirk-herdr's (D51, 0022 D71; orient/core.md
/// line 22).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunId(pub String);

/// ULID. New: sergeant completed via lifecycle event
/// (`KIND_WORK_COMPLETED`, api.rs:47-51); wirk moves completion to an
/// explicit verb (0001 D3; orient/core.md line 24).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClaimId(pub String);

/// Event identity newtype (orient/core.md line 26).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventId(pub String);

/// Hash of a Waypoint's World inputs; incident file's resume-by-key,
/// replaces sergeant's `KIND_CONTEXT_COMPILED` event (engine.rs:53)
/// with a pure key (orient/core.md line 28).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldHash(pub String);

impl WorldHash {
    /// SHA-256 (R5: installed dependency, cache-resolvable offline; W2
    /// build brief §2, correcting core.md's stdlib-hash recommendation —
    /// `std::hash::Hasher`'s default is unspecified across Rust releases,
    /// unfit for a persisted key) over the **covered** fields only
    /// (orient/world.md §2), each followed by a `0x1f` separator, behind a
    /// leading tag byte selecting the `World` variant so an `Actor` world
    /// and a `Deterministic` world with coincidentally equal field bytes
    /// never collide.
    ///
    /// Covered, `World::Actor`: `repository`, `branch`, `base_sha`,
    /// `intent`, each `output_contract` artifact spec's `name` and
    /// `required` flag, each `boundary` glob. Covered, `World::Deterministic`:
    /// each `command` word, `base_sha` (item 5, issue 285: the code
    /// state the command runs against is content, same principle as
    /// `Actor`'s own `base_sha`), each `expected_artifacts` spec's
    /// `name` and `required` flag. Excluded (world.md §2):
    /// `worktree_path`, `estate_root`, `cwd`, `env`, `triple`, every id.
    ///
    /// Hex-encoded lowercase.
    pub fn of(world: &World) -> WorldHash {
        // `legacy` is the compatibility encoding for Worlds that predate
        // `source_basis` — and, equally, predate frozen review targets.
        // The independent review's executed D1: an Actor World whose
        // basis is `Unknown` (the bare public Actor submit, which the
        // CLI accepts) took this fallback, and `legacy` hashes
        // repository, branch, `base_sha`, intent, output contract and
        // boundary and nothing else. The targets really were frozen, the
        // review really settled, and recomputing `legacy` *without*
        // feeding the targets in reproduced the journaled hash exactly
        // (`loop-b-legacy-target-binding/raw/00-red-d1-legacy-basis.txt`)
        // — so the value the operator admitted did not bind the reviewed
        // target, which is the one thing the target-binding requirement
        // exists to make it do.
        //
        // A World that carries frozen review targets is therefore never
        // a pre-v2 World, whatever its source basis says, and takes the
        // v2 encoding — which already covers the targets, in one place,
        // length-prefixed and unambiguous. No divergent second list, and
        // no historical hash moves: no World written before this wave
        // carries a frozen target, so every one of them still takes the
        // fallback and hashes exactly as it always did.
        // W-C1: a World that carries a stage projection is likewise
        // never a pre-v2 World — nothing written before this wave carries
        // one, so the fallback still covers every historical World exactly
        // as it always did, and the predicate only ever narrows.
        if world.source_basis() == &SourceBasis::Unknown
            && !world.carries_review_targets()
            && !world.carries_evidence()
        {
            return Self::legacy(world);
        }

        let mut hasher = Sha256::new();
        hasher.update(b"wirk.world-hash/v2\0");
        match world {
            World::Actor(actor) => {
                hasher.update([0u8]);
                hash_string(&mut hasher, &actor.repository);
                hash_string(&mut hasher, &actor.branch);
                hash_string(&mut hasher, &actor.base_sha);
                hash_source_basis(&mut hasher, &actor.source_basis);
                hash_string(&mut hasher, &actor.intent);
                hash_len(&mut hasher, actor.output_contract.0.len());
                for spec in &actor.output_contract.0 {
                    hash_string(&mut hasher, &spec.name);
                    hasher.update([spec.required as u8]);
                }
                hash_len(&mut hasher, actor.boundary.0.len());
                for glob in &actor.boundary.0 {
                    hash_string(&mut hasher, glob);
                }
                // W-B target binding: the frozen review targets are part
                // of this World's own content — they are what the review
                // was actually pointed at. Hashed only when present, so
                // every Actor World that declares no review keeps the
                // hash it has always had (there is no unconditional byte
                // here to shift them).
                if !actor.review_targets.is_empty() {
                    hasher.update([0x02]);
                    hash_len(&mut hasher, actor.review_targets.len());
                    for target in &actor.review_targets {
                        hash_string(&mut hasher, &target.source);
                        hash_string(&mut hasher, &target.path);
                        hash_string(&mut hasher, &target.estate);
                        hash_string(&mut hasher, &target.membership);
                        hash_string(&mut hasher, &target.source_id);
                        hash_string(&mut hasher, &target.generation);
                        hash_string(&mut hasher, &target.object_id);
                    }
                }
                // W-C1 (BUILD.md §3.2): the delivered projection is part
                // of this World's own content — it is what the stage was
                // actually given. Appended under the existing `v2` tag,
                // presence-gated, length-framed and behind its own marker
                // byte, exactly the precedent `review_targets` set above,
                // so no World written without a projection moves. The
                // observation id is deliberately **not** covered:
                // re-observing the identical delivered context must not
                // change a stage's resume key.
                if let Some(evidence) = &actor.evidence {
                    hasher.update([0x03]);
                    hash_string(&mut hasher, &evidence.projection.0);
                    hasher.update(evidence.revision.to_be_bytes());
                    hash_string(&mut hasher, &evidence.format);
                }
            }
            World::Deterministic(det) => {
                hasher.update([1u8]);
                hash_len(&mut hasher, det.command.len());
                for word in &det.command {
                    hash_string(&mut hasher, word);
                }
                hash_string(&mut hasher, &det.base_sha);
                hash_source_basis(&mut hasher, &det.source_basis);
                hash_len(&mut hasher, det.expected_artifacts.0.len());
                for spec in &det.expected_artifacts.0 {
                    hash_string(&mut hasher, &spec.name);
                    hasher.update([spec.required as u8]);
                }
            }
        }
        let digest = hasher.finalize();
        WorldHash(hex_lower(&digest))
    }

    /// The pre-v2 encoding, exposed for the one test that has to assert
    /// a historical hash is *unchanged* — which cannot be shown by
    /// calling `of` alone, since `of` is exactly what decides whether the
    /// fallback still applies.
    #[doc(hidden)]
    pub fn legacy_for_tests(world: &World) -> WorldHash {
        Self::legacy(world)
    }

    fn legacy(world: &World) -> WorldHash {
        let mut hasher = Sha256::new();
        match world {
            World::Actor(actor) => {
                hasher.update([0u8]);
                for field in [
                    &actor.repository,
                    &actor.branch,
                    &actor.base_sha,
                    &actor.intent,
                ] {
                    hasher.update(field.as_bytes());
                    hasher.update([0x1f]);
                }
                for spec in &actor.output_contract.0 {
                    hasher.update(spec.name.as_bytes());
                    hasher.update([0x1f, spec.required as u8, 0x1f]);
                }
                for glob in &actor.boundary.0 {
                    hasher.update(glob.as_bytes());
                    hasher.update([0x1f]);
                }
            }
            World::Deterministic(det) => {
                hasher.update([1u8]);
                for word in &det.command {
                    hasher.update(word.as_bytes());
                    hasher.update([0x1f]);
                }
                hasher.update(det.base_sha.as_bytes());
                hasher.update([0x1f]);
                for spec in &det.expected_artifacts.0 {
                    hasher.update(spec.name.as_bytes());
                    hasher.update([0x1f, spec.required as u8, 0x1f]);
                }
            }
        }
        WorldHash(hex_lower(&hasher.finalize()))
    }
}

fn hash_len(hasher: &mut Sha256, len: usize) {
    hasher.update((len as u64).to_be_bytes());
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hash_len(hasher, value.len());
    hasher.update(value.as_bytes());
}

fn hash_source_basis(hasher: &mut Sha256, basis: &SourceBasis) {
    match basis {
        SourceBasis::Unknown => hasher.update([0]),
        SourceBasis::Git { base } => {
            hasher.update([1]);
            hash_string(hasher, base);
        }
        SourceBasis::OutputOnly { reference } => {
            hasher.update([2]);
            hash_string(hasher, reference);
        }
    }
}

/// Lowercase hex encoding of a byte slice (stdlib `format!`, R3 — no hex
/// crate needed for this one call site).
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String never fails");
    }
    out
}

// ---- Work ---------------------------------------------------------------

/// Durable unit of intent (0001 D5). D9#1: reconstructible from Events
/// alone (orient/core.md line 30-38).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Work {
    pub id: WorkId,
    pub intent: String,
    pub route: RouteId,
    pub repositories: Vec<RepositoryBinding>,
    pub state: WorkState,
    /// The Waypoint most recently reserved (issue 286; fold.md §4): set
    /// by `WaypointReserved`, left unchanged by a `ClaimRecorded` that
    /// advances state without a new reservation (fold.md §1).
    pub current_waypoint: Option<WaypointId>,
    /// Wall-clock time of the last event folded for this Work (issue
    /// 286; fold.md §4), written on every folded event so "still
    /// working" and "wedged" are distinguishable without transcript
    /// reading.
    pub last_activity: Timestamp,
    /// P2.3 W1 (0033 D102; 0044): why the Work is (or last was)
    /// `NeedsInput` — a failed Run, a vanished Run, a stuck actor
    /// (`RunFailed` with `cause.status == Some("stuck")`), or a
    /// validated Question claim. Set by `fold` on the transition, left
    /// as history once the Work moves on (a retry verb clearing it is
    /// P2.3 W2's decision, out of scope here). `#[serde(default)]`: a
    /// `Work` is never itself journaled, only rebuilt fresh by `fold`
    /// on every read (no cached projection), so this is a pure
    /// in-memory addition — the default only matters if `Work` is ever
    /// round-tripped, which it is not today.
    #[serde(default)]
    pub needs_input: Option<NeedsInputCause>,
    /// W-A (§3.3): set when this Work was submitted as a child, from
    /// `WorkSubmitted.parent`. `#[serde(default)]`: `Work` is never
    /// itself journaled (`needs_input`'s own doc), so this only matters
    /// on the in-memory value, which never predates this field.
    #[serde(default)]
    pub parent: Option<ParentBinding>,
    /// W-A (§3.2): why the Work is (or last was) `Waiting` — the
    /// container `StageHeld` named and what it is missing. Cleared by a
    /// later `StageClosed` on the same container (fold's own rule,
    /// mirroring `needs_input`'s "set on the transition, left as
    /// history once the Work moves on").
    #[serde(default)]
    pub held: Option<HeldInfo>,
    /// W-A correction (F1/F2): every container's current activation, in
    /// first-activation order — folded from `ContainerActivated` so a
    /// reader (and every closure decision) can tell one generation of a
    /// container from the next without re-deriving it.
    #[serde(default)]
    pub activations: Vec<ContainerActivation>,
    /// P3 W3 (ruling 0090): folded from `WorkSubmitted.execution_repo`
    /// — which named `repositories` entry is this Work's actual
    /// execution checkout. `#[serde(default)]`: `Work` is never itself
    /// journaled (only rebuilt fresh by `fold`), so this only matters
    /// on the in-memory value, which never predates this field.
    #[serde(default)]
    pub execution_repo: Option<String>,
    /// Folded from `WorkSubmitted.execution_identity` — wirkd's own
    /// verified canonical identity of that checkout.
    #[serde(default)]
    pub execution_identity: Option<String>,
    /// W-B: every Finding raised in this Work's own journal, by id —
    /// `FindingRaised`/`FindingSettled`/`FindingAsserted`/`FindingApplied`
    /// all fold onto the same record (`FindingRecord`'s own doc).
    /// `#[serde(default)]`: `Work` is never itself journaled (only
    /// rebuilt fresh by `fold`), so this only matters on the in-memory
    /// value, which never predates this field.
    #[serde(default)]
    pub findings: BTreeMap<FindingId, FindingRecord>,
    /// W-B (§6 correction): settlement candidates derivable from this
    /// Work's own journal alone (`DeterministicVerified`,
    /// `SupersededInOrigin`) — computed fresh on every fold, order
    /// independent of when the qualifying event and the `FindingRaised`
    /// that names it appear in the journal (the terminal design's own
    /// bug: computing readiness only "at the moment the qualifying event
    /// folds" made `DeterministicVerified` unreachable, since that
    /// class's own evidence always names an *earlier* Claim). Only ever
    /// contains one entry per still-`Proposed` finding (a settled
    /// finding is dropped). `ChildInvestigationConfirmed` is not derivable
    /// here at all — it needs a second Work's journal, which the core
    /// fold must never read (construction review: "pure core fold must
    /// not read other journals or Atlas"); wirkd's own `settle_ready`
    /// computes that one from the daemon's cross-journal view.
    #[serde(default)]
    pub settlement_ready: Vec<ReadySettlement>,
}

impl Work {
    /// The current activation of `waypoint`, or the implicit first
    /// generation for a container no `ContainerActivated` named (a
    /// pre-correction journal).
    pub fn activation(&self, waypoint: &WaypointId) -> u32 {
        self.activations
            .iter()
            .find(|entry| &entry.waypoint == waypoint)
            .map(|entry| entry.attempt)
            .unwrap_or(1)
    }
}

/// W-A (§3.2): a held container's own reason, surfaced by `wirk work
/// status` (module doc on `Work.held`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldInfo {
    pub waypoint: WaypointId,
    /// W-A correction (F1/F2): the container *activation* this hold
    /// belongs to. A hold from a superseded generation is history, not
    /// the current requirement — `fold` only clears `held` on a
    /// `StageClosed` for this same attempt.
    #[serde(default = "first_attempt")]
    pub attempt: u32,
    pub missing: Vec<String>,
}

/// The attempt every pre-correction record implies (W-A correction,
/// F1/F2): journals written before container activations carried an
/// identity name exactly one generation.
pub fn first_attempt() -> u32 {
    1
}

/// W-A correction (F1/F2): one container's current activation, folded
/// from `ContainerActivated` — the generation every closure receipt,
/// hold and served-child binding is scoped to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerActivation {
    pub waypoint: WaypointId,
    pub attempt: u32,
}

/// W-A (§3.3): names the parent Work/container/Run/role a child Work
/// was submitted under. Carried on `WorkSubmitted.parent` and echoed
/// onto `Work.parent`; grants (repository bindings) are checked at
/// submit time against the parent's own — never re-derived from this
/// binding after the fact (§3.3: "the child's own `WorkSubmitted.parent`
/// names this parent Work, this waypoint, that same Run and role").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentBinding {
    pub work: WorkId,
    pub waypoint: WaypointId,
    /// W-A correction (F4): the container activation this child serves.
    /// A client may leave it `None` ("whichever generation is current"),
    /// which wirkd resolves to the concrete attempt before journaling —
    /// every *recorded* binding names one exact generation, so a child
    /// that served a superseded activation can never credit the next
    /// one. `None` on a replayed record is a pre-correction journal and
    /// reads as the first generation (`attempt_or_first`).
    #[serde(default)]
    pub attempt: Option<u32>,
    pub run: RunId,
    pub role: String,
}

impl ParentBinding {
    /// The activation this binding names, with a pre-correction record's
    /// implicit first generation.
    pub fn attempt_or_first(&self) -> u32 {
        self.attempt.unwrap_or(1)
    }
}

/// W-A (§3.2-3.3): one piece of exact evidence a container's closure
/// relied on — never a bare file path or a `WorkState` alone.
/// `Container` (this crate's own addition, beyond BUILD-BRIEF's
/// one-level shape): a nested sub-container's own `StageClosed`
/// receipts, so an outer container's declared_outputs can be satisfied
/// by a leaf several levels down without re-deriving it from raw Claims
/// each time (W-A-BUILD.md: "Required latest receipts aggregate
/// recursively, never by last flattened leaf alone").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeReceipt {
    Leaf {
        waypoint: WaypointId,
        run: RunId,
        claim: ClaimId,
        /// W-A correction (F3): the exact artifacts that validated,
        /// each with the content identity recorded at validation —
        /// never a re-derivation of the Route's declared names against
        /// a mutable path.
        artifacts: Vec<ArtifactReceipt>,
    },
    Child {
        role: String,
        child: WorkId,
        parent_run: RunId,
        claim: ClaimId,
        world_hash: WorldHash,
    },
    Container {
        waypoint: WaypointId,
        receipts: Vec<OutcomeReceipt>,
    },
}

/// P2.3 W1 (states.md §1): one three-field struct rather than a
/// `reason` variant per underlying cause string (R6) — a caller
/// distinguishing an exit failure from a stuck actor reads
/// `cause.status` off the named Run (already surfaced by
/// `handle_status`'s `failure_status`), not a second field here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeedsInputCause {
    pub run: RunId,
    /// `"run_failed"` | `"run_vanished"` | `"question"` | `"blocked"`
    /// (ruling 0052 D156, P2.6 W2) — a stuck actor is a `RunFailed` too
    /// (states.md §1), distinguished by `cause.status ==
    /// Some("stuck")` on the Run, not a fifth string here. `"blocked"`
    /// is the one reason `fold` also clears on its own (a later
    /// `LifecycleObserved{Working}`), never through a human verb.
    pub reason: String,
    pub detail: String,
}

/// A repository this Work may touch, with its declared access mode
/// (W3, ruling 0026, issue 288: sergeant's `--group`/`--repo` selection
/// carried no read/write tag, so a Claim validator had nothing but
/// mutation-surface prose to check a write against; `Work.repositories`
/// was the identical flat `Vec<String>` shape here before this change).
/// A Claim whose evidence shows a write to a repository declared
/// `Access::Read` is refused (`ClaimRefusal::OutOfBoundary`), not
/// accepted on trust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepositoryBinding {
    pub name: String,
    pub access: Access,
}

/// Per `RepositoryBinding.access` above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Read,
    Write,
}

/// Adopted verbatim from sergeant's `WorkState` (domain/work.rs:192-207);
/// reshaped transition discipline only — incident file's "a failed Run
/// is not a failed Work", no mechanical Run->Work fold exists
/// (orient/core.md line 39-46).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Pending,
    Active,
    Waiting,
    NeedsInput,
    Blocked,
    Completed,
    Failed,
    Canceled,
}

impl WorkState {
    /// `Completed`/`Failed`/`Canceled` are terminal (fold.md §1: each
    /// new terminal event's row reads "any non-terminal -> ..."); used
    /// by `fold` to stop advancing a Work that has already ended.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkState::Completed | WorkState::Failed | WorkState::Canceled
        )
    }
}

// ---- Route / Waypoint ----------------------------------------------------

/// Sergeant's workflow. `retry_policy` (incident file item 4's "a retry
/// policy is a Route concern") is gone (ruling 0044 D134: no count or
/// time governs how wirk treats an agent or a Run; `RetryPolicy`/
/// `BackoffPolicy` were unread by anything — R1). P2.3 decides retry
/// from the failure observed, not a policy value carried here.
/// `deny_unknown_fields` (p2-route-files, format.md §1, R6): a Route
/// file authored by hand cannot carry a field this type doesn't know —
/// D134's "no count or duration field" is enforced by the loader
/// refusing it (`RouteError::Malformed`), not by silently dropping it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub id: RouteId,
    pub waypoints: Vec<WaypointDefinition>,
}

/// W-B obligation proof (`W-B-OBLIGATION-BUILD.md`; construction
/// review "policy proves the named obligation"; authority adjudication
/// "bind the admitted policy/check identity and exact evidence basis").
/// The *name* one verification obligation goes by: a check identity and
/// the edition of that check. Named by the Route on the Waypoint that
/// discharges it, named again by the Finding that claims to discharge
/// it, and admitted — by name **and** by content basis — in the estate's
/// own settlement policy. Naming alone is never authority: `id` and
/// `edition` only select which admitted entry must match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationRef {
    pub id: String,
    pub edition: String,
}

/// The Route-authored verification obligation a Waypoint discharges:
/// what check this is (`id`/`edition`), the exact, limited statement a
/// passing run of it proves (`proves` — never the Finding's own free
/// sentence), and the named outcomes whose receipts constitute the
/// discharge (`outputs`, matched against the Claim's own
/// `ArtifactReceipt` names).
///
/// This type is authored content, not authority. A proposer may write
/// any obligation it likes into its own Route; `obligation_basis` below
/// content-addresses every field of it together with the immutable
/// execution basis, and only a basis the estate's own policy file
/// already admits can settle anything (`try_mint_settlement`). That is
/// what stops "an actor names a check and thereby makes it
/// authoritative".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationObligation {
    pub id: String,
    pub edition: String,
    /// The exact sentence a discharge of this obligation proves — and
    /// nothing wider. Rendered as `settled.proves.statement`; the
    /// Finding's own `claim` stays a recorded, unverified sentence
    /// beside it.
    pub proves: String,
    /// The **outcome** half, kept separate from the mechanism below:
    /// declared artifact names (Deterministic) or obligated child roles
    /// (Container) whose receipts discharge this obligation. Every name
    /// here must be present in the discharging receipt — for the
    /// Container class, every obligated role must have closed, in this
    /// container's current activation, or nothing discharges (the
    /// independent review's executed C2: `outputs: ["auditor"]`
    /// discharged through role `scribe` while `auditor` never existed).
    ///
    /// Empty is legal for a `Deterministic` Waypoint (the command's own
    /// validated completion is the whole receipt) and refused for a
    /// `Container` (a container obligation naming no obligated role
    /// obliges nothing, so there is nothing for a child to discharge).
    #[serde(default)]
    pub outputs: Vec<String>,
    /// The **mechanism** half, `Container` only: the verification
    /// obligation each obligated role's child Work must itself have
    /// *settled* for this container obligation to be discharged.
    ///
    /// This is what makes a container obligation bind a real execution.
    /// A container has no World of its own, so hashing its own shape
    /// content-addresses prose and an outcome contract and nothing else
    /// — the independent review's executed C1: two unrelated Routes,
    /// different repository, different waypoint id, different leaf
    /// command, collided to the identical admitted basis, and a child
    /// whose whole "investigation" was one Finding reading *"I did not
    /// investigate anything"* discharged it. Naming `requires` moves the
    /// container's own basis, and discharging it forces each obligated
    /// role's child to have really run — and really settled — a
    /// `Deterministic` obligation whose own basis content-addresses its
    /// command, source basis and expected artifacts. The estate admits
    /// that mechanism basis too (`policy/settlement.json`'s own
    /// `mechanisms`), so a changed repository generation, a changed
    /// child verification command or a changed evidence target cannot
    /// silently reuse the authority already granted.
    ///
    /// `None` on a `Container` means the obligation declares no
    /// mechanism and can therefore discharge nothing.
    #[serde(default)]
    pub requires: Option<ObligationRef>,
    /// The **agentic** mechanism, `Actor` only: the bounded review
    /// contract this Waypoint's own reviewer must satisfy
    /// (`ReviewContract`). An `Actor` Waypoint declaring an obligation
    /// without one discharges nothing — the same fail-closed rule a
    /// `Container` without `requires` follows.
    #[serde(default)]
    pub review: Option<ReviewContract>,
}

/// `Container` and `Actor` obligations both need a mechanism; this is the
/// **agentic** one. A Route Waypoint of kind `Actor` that declares a
/// `review` contract is a bounded independent review: a named recipe, the
/// exact resource paths it must have looked at, and the closed set of
/// structured decisions it may return.
///
/// What this makes provable and what it deliberately does not:
///
/// - Provable, because every part is an immutable journal fact: *this*
///   World (repository, branch, `base_sha`, source basis, **intent**,
///   output contract, boundary — all already covered by `WorldHash::of`'s
///   own `Actor` arm) ran, produced *this* report artifact with *this*
///   content digest, applied to *these* exact admitted source coordinates,
///   and recorded *this* decision from the declared set.
/// - **Not** provable, and never claimed: that the review's English
///   conclusion is true. A settled agentic review says an admitted review
///   was performed under an admitted recipe over admitted targets and
///   recorded a declared decision. The judgement itself stays judgement.
///
/// This is why `decisions` is a closed set of `FindingKind` rather than
/// free text: the *structured outcome* is checkable, the prose is not.
/// What a Route author can name before the review runs: an admitted
/// source alias and a resource path inside it. Resolvable, not yet
/// resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSelector {
    /// The Atlas source alias, which must be one of the reviewing Work's
    /// own `repositories` bindings.
    pub source: String,
    /// The resource path within that source.
    pub path: String,
}

/// One review target as it was **frozen at reservation**: the selector
/// the Route asked for, and the complete immutable identity wirkd
/// resolved it to against that source's own currently published
/// generation, admitted under the reviewing Work's own bindings.
///
/// This is where the exact binding becomes fixed. It is carried in the
/// reserved `ActorWorld`, so `WorldHash::of` covers it, so
/// `obligation_basis`'s `Actor` arm binds it, so the estate admits *this*
/// target and not merely *a path*. A different repository, a different
/// source, a different generation or a different object is a different
/// World hash, a different basis, and a fresh admission.
///
/// Every field is an Atlas identity carried as an opaque `String`, the
/// same way `EvidenceRef::Source` carries an already-encoded coordinate:
/// `wirk-core` does not depend on `wirk-atlas` (0022 D71), and only
/// `wirk/src/wirkd/server.rs` resolves or compares them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewTarget {
    /// The declared selector, echoed so the record shows what was asked
    /// for beside what it resolved to.
    pub source: String,
    pub path: String,
    /// The resolved identity.
    pub estate: String,
    pub membership: String,
    pub source_id: String,
    pub generation: String,
    pub object_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewContract {
    /// The bounded verification procedure and its edition, as the Route
    /// author states it. Hashed into `obligation_basis`, so changing the
    /// recipe changes what the estate admitted.
    pub recipe: String,
    /// What this review must have applied to, as a **selector** the Route
    /// author can write before any generation exists: an admitted source
    /// alias and a resource path within it.
    ///
    /// A selector is not the binding. Before the review executes, wirkd
    /// resolves every selector against that source's own currently
    /// published generation, admitted under the reviewing Work's own
    /// bindings, and **freezes** the resulting exact identity into the
    /// reserved World (`ActorWorld.review_targets`) — where
    /// `WorldHash::of` covers it and `obligation_basis` therefore binds
    /// it. The selector itself is hashed too, so changing what the Route
    /// asks for is also a fresh admission.
    ///
    /// The independent re-review's executed C1 is why this is a selector
    /// and not a path: when the declared target was a bare string,
    /// `socket.rs` in a *different admitted repository*, and `socket.rs`
    /// at an *earlier generation the reviewing World never opened
    /// against*, both discharged an admitted review of `demo`'s current
    /// `socket.rs` — and the settled record could not tell the three
    /// apart.
    pub targets: Vec<ReviewSelector>,
    /// The closed set of structured outcomes this review may return. The
    /// reviewer's Finding kind must be one of them; a decision outside
    /// the declared set discharges nothing.
    pub decisions: Vec<FindingKind>,
}

/// The immutable content address of one obligation *as it will be
/// discharged*: the authored obligation (id, edition, statement,
/// outputs) inseparably bound to the execution basis that discharges it
/// — for a `Deterministic` Waypoint, its World hash (which already
/// content-addresses `command` + `base_sha` + `expected_artifacts`,
/// `WorldHash::of`); for a `Container`, its own declared outputs and
/// required child roles, the whole of its outcome contract.
///
/// The estate's settlement policy admits obligations by this basis, so
/// nothing a proposer can author — a different command, a different
/// source basis, a widened `proves` sentence, a dropped required output
/// — leaves the admitted basis unchanged.
///
/// `None` for a Waypoint that declares no obligation, and for an `Actor`
/// obligation carrying no `review` contract — the one arm below that
/// refuses on a missing mechanism (`obligation.review.as_ref()?`).
///
/// A `Container` obligation carrying no `requires` is **not** one of
/// them: the `Container` arm hashes a `0u8` discriminator for the
/// absent mechanism and returns a basis, so "no `requires`" changes the
/// value rather than withholding it. Its fail-closed is real and lives
/// somewhere else — a container obligation with no `requires` is
/// skipped as a settlement candidate before its basis is ever asked for
/// (`wirkd::server`'s readiness walk: `let Some(requires) =
/// obligation.requires.as_ref() else { continue; }`). Executed both
/// ways in `wirk-core/tests/findings.rs`. An earlier revision of this
/// doc said `Container` refused here; it does not, and no arm below
/// changes to make it. What is **not** true either, and what a still
/// earlier
/// revision of this doc said, is that an `Actor` never has a basis at
/// all "because no deterministic check exists there to be discharged".
/// An `Actor` carrying a `ReviewContract` has had a real basis since
/// W-B-AGENTIC-PROOF.md — the `Actor` arm below hashes its World hash,
/// which already covers repository, branch, `base_sha`, source basis,
/// intent, output contract and boundary — and `wirk/tests/findings.rs`
/// settles against exactly that value on a real estate. Ruling 0135
/// records the old wording, and the "prior design limitation" it
/// described, as stale. This is a correction to the description only;
/// the three arms below are unchanged, and an `Actor` with a review
/// contract has never been, and does not become, a way around a
/// `Container`'s own explicit nested-mechanism requirement.
pub fn obligation_basis(
    def: &WaypointDefinition,
    world_hash: Option<&WorldHash>,
) -> Option<String> {
    let obligation = def.verifies.as_ref()?;
    let mut hasher = Sha256::new();
    hasher.update(b"wirk.obligation-basis/v1\0");
    hash_string(&mut hasher, &obligation.id);
    hash_string(&mut hasher, &obligation.edition);
    hash_string(&mut hasher, &obligation.proves);
    hash_len(&mut hasher, obligation.outputs.len());
    for output in &obligation.outputs {
        hash_string(&mut hasher, output);
    }
    // The declared agentic review contract is authored content like any
    // other, and it is admitted like any other: changing the recipe, a
    // reviewed target, or the set of decisions the review may return
    // changes the basis the estate has to admit.
    match &obligation.review {
        None => hasher.update([0u8]),
        Some(review) => {
            hasher.update([1u8]);
            hash_string(&mut hasher, &review.recipe);
            hash_len(&mut hasher, review.targets.len());
            for target in &review.targets {
                hash_string(&mut hasher, &target.source);
                hash_string(&mut hasher, &target.path);
            }
            hash_len(&mut hasher, review.decisions.len());
            for decision in &review.decisions {
                hash_string(&mut hasher, finding_kind_name(*decision));
            }
        }
    }
    match def.kind {
        WaypointKind::Deterministic => {
            hasher.update([1u8]);
            hash_string(&mut hasher, &world_hash?.0);
        }
        WaypointKind::Container => {
            hasher.update([2u8]);
            // The declared mechanism is part of what the operator
            // admits: changing which child obligation this container
            // requires changes its own basis, so an admitted container
            // obligation can never be re-pointed at a different
            // verification without a fresh admission.
            match &obligation.requires {
                None => hasher.update([0u8]),
                Some(required) => {
                    hasher.update([1u8]);
                    hash_string(&mut hasher, &required.id);
                    hash_string(&mut hasher, &required.edition);
                }
            }
            hash_len(&mut hasher, def.declared_outputs.len());
            for spec in &def.declared_outputs {
                hash_string(&mut hasher, &spec.name);
                hasher.update([spec.required as u8]);
            }
            hash_len(&mut hasher, def.required_child_outcomes.len());
            for spec in &def.required_child_outcomes {
                hash_string(&mut hasher, &spec.role);
                hasher.update([spec.required as u8]);
            }
        }
        // W-B-AGENTIC-PROOF.md: an `Actor` Waypoint has a real content
        // identity — `WorldHash::of`'s own `Actor` arm already covers
        // repository, branch, `base_sha`, source basis, **intent**,
        // output contract and boundary. The previous revision returned
        // `None` here and so left the operator with no value to admit at
        // all; that conflated "its reasoning is not deterministic" with
        // "its execution inputs have no identity", and it is corrected.
        // A changed review intent is a changed World is a changed basis.
        WaypointKind::Actor => {
            // An Actor obligation with no review contract declares no
            // mechanism, and this is the only arm that refuses on that
            // ground: the `Container` arm above hashes the absence of
            // `requires` and returns a basis, and the estate refuses a
            // mechanism-less container later, at readiness.
            obligation.review.as_ref()?;
            hasher.update([3u8]);
            hash_string(&mut hasher, &world_hash?.0);
        }
    }
    Some(hex_lower(&hasher.finalize()))
}

/// The wire name of a `FindingKind`, used where the kind has to be hashed
/// or compared as text rather than matched (`obligation_basis`). Kept
/// beside the enum so the two never drift.
pub fn finding_kind_name(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::Gap => "gap",
        FindingKind::ContradictedAssumption => "contradicted_assumption",
        FindingKind::Relationship => "relationship",
        FindingKind::VerifiedOutcome => "verified_outcome",
    }
}

/// Sergeant's stage, from `StageDefinition`/`StageBinding`
/// (domain/workflow.rs:460-475); `harness`/`route_source`/`profile`
/// (backend-selection) dropped, Herdr-adjacent (0022 D71).
/// `declared_outputs` kept: D9#3 checks a Claim against it
/// (orient/core.md line 57-62).
///
/// p2-route-files (format.md §1, build-brief.md §2 Disagreement 1,
/// R6): `intent`/`command` added as an `Option` pair, not an
/// enum-with-payload — the type itself does not bar an Actor Waypoint
/// from carrying a `command` (or vice versa); `load_route`'s refusals
/// (`ActorWithCommand`/`DeterministicMissingCommand`) catch it instead.
/// `boundary` defaults to an empty `Boundary` (`#[serde(default)]`,
/// R6) — refusal 9: an empty boundary is authoring, not enforcement
/// (P2.4's). `deny_unknown_fields` per `Route`'s own doc above.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaypointDefinition {
    pub id: WaypointId,
    pub kind: WaypointKind,
    pub declared_outputs: Vec<ArtifactSpec>,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub boundary: Boundary,
    /// `Container` only (W-A, BUILD-BRIEF.md §3.1): the nested mechanism,
    /// in execution order. A leaf here may itself be a `Container`
    /// (BUILD-AMENDMENTS.md: recursive, not bounded to one level).
    /// Empty for an Actor/Deterministic Waypoint (`load_route` refuses
    /// the reverse).
    #[serde(default)]
    pub leaves: Vec<WaypointDefinition>,
    /// `Container` only: the child-Work roles this container's closure
    /// requires a valid receipt for (§3.3). Empty for an
    /// Actor/Deterministic Waypoint.
    #[serde(default)]
    pub required_child_outcomes: Vec<ChildOutcomeSpec>,
    /// P3 native launch selection (BUILD-BRIEF.md, PREPARATION-ADJUDICATION.md
    /// point 1): the workflow-authored default harness/model/effort/raw-args
    /// for an Actor Waypoint's Run, layered under `wirk run`'s own explicit
    /// `--actor-kind`/`--actor-model`/`--actor-effort` overrides (documented
    /// precedence: CLI-explicit > this authored default > the harness's own
    /// native default). `Actor` only — mechanism, not content, so it never
    /// touches `ActorWorld`/`WorldHash::of` (same separation `Run.kind`
    /// already draws, orient/actor.md §2). A `Deterministic`/`Container`
    /// Waypoint carries no Run of its own to launch, so a `selection` there
    /// would be authored and silently unused; `validate_tree` refuses it
    /// (`RouteError::ActorSelectionOnNonActor`) rather than accept dead
    /// configuration. No inheritance: a `Container`'s `leaves` each carry
    /// their own independent `selection`, never their parent's or a
    /// sibling's — nothing here reads up or across the tree.
    #[serde(default)]
    pub selection: Option<AuthoredSelection>,
    /// W-B obligation proof: the verification obligation this Waypoint
    /// discharges, if any (`VerificationObligation` above). Additive and
    /// `#[serde(default)]`, so every Route file and every
    /// `WorkSubmitted.waypoint_defs` written before this wave still
    /// parses and still folds — reading, correctly, as a Waypoint that
    /// discharges no obligation and can therefore settle nothing.
    #[serde(default)]
    pub verifies: Option<VerificationObligation>,
    /// W-C1: the orientation this Waypoint asks for. `None` — the
    /// default, and every Route written before this wave — means the
    /// reservation does exactly what it always did: no Atlas work, no
    /// projection, a byte-identical World and World hash.
    ///
    /// `Actor` only. A `Deterministic` or `Container` Waypoint opens no
    /// actor Run to hand a projection to, so an `orient` block there
    /// would be authored and silently unused; `validate_tree` refuses it
    /// (`RouteError::OrientationOnNonActor`), the same posture
    /// `ActorSelectionOnNonActor` already takes.
    ///
    /// `skip_serializing_if` so a Route without orientation journals its
    /// `waypoint_defs` byte-for-byte as before — which is also what keeps
    /// `route_edition_of` stable for every Route written before this wave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orient: Option<OrientationRequest>,
}

/// The workflow-authored half of P3 native launch selection (`WaypointDefinition.selection`).
/// `harness` is the Route's own default `ActorKind` (`wirk run`'s
/// `--actor-kind` still overrides it, per the documented precedence);
/// `model`/`effort` are translated into the harness's own real CLI
/// controls at launch (`wirk-herdr`'s `build_selection_args`, verified
/// against the installed `claude`/`opencode`/`codex` binaries, never a
/// headless-only guess); `args` is raw, verbatim pass-through for
/// anything neither convenience field covers (R1: no product tries to
/// name every harness flag — an author who needs one types it here).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredSelection {
    #[serde(default)]
    pub harness: Option<ActorKind>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

/// The resolved half of P3 native launch selection, carried on `Run`
/// exactly like `Run.kind` (mechanism, never `World` content): the
/// model/effort/raw-args this Run's launch was actually requested with,
/// after CLI/Route-authored precedence was applied and before Herdr was
/// ever called. Distinct from `Run.launch_argv` (`RunLaunched.launch_argv`):
/// this is *what wirk asked for*, that is *what Herdr says it submitted*
/// — Herdr's own reply proves what reached the shell, never that a
/// provider actually served the requested model (PREPARATION-ADJUDICATION.md
/// point 3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorSelection {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Who holds a Run's launch *attempt* — P3 native launch attempt
/// admission. Minted by `wirkd` itself from the connected client's
/// kernel-reported credentials (`SO_PEERCRED`), never from anything a
/// client says about itself, and never a claim of authenticity: any
/// process that can write this estate's journal could write any event,
/// so this is exclusion between cooperating invocations, not a
/// security boundary.
///
/// `start_token` is the holder process's own start time as the kernel
/// reports it (`/proc/<pid>/stat` field 22), read by `wirkd` at the
/// moment it admits the attempt. Without it a recycled pid would read
/// as the original holder still being alive; with it, "this pid is
/// live *and* is the same process" is one comparison. `None` means
/// `wirkd` could not read it, which is deliberately *not* the same as
/// zero (`holder_state`'s own doc, `server.rs`).
///
/// The token answers *which* process, never *whether it is alive*: a
/// dead but unreaped holder keeps both its pid and its start token, so
/// `wirkd` reads the process state alongside it and treats a corpse as
/// gone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptHolder {
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub start_token: Option<String>,
}

/// One admitted launch attempt: who holds it and where that holder
/// will call Herdr. Folded onto `Run.launch_attempt` — only the latest
/// admitted attempt is kept, because `wirkd` refuses a new one while
/// the previous holder is still alive (`handle_record`), so the latest
/// is the only one that can be current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchAttempt {
    pub holder: AttemptHolder,
    /// The Herdr destination this Run's launch is bound to — the
    /// client's own canonicalized Herdr socket path. Client-declared
    /// (wirkd has no way to observe which Herdr a client dialed) and
    /// bound at this Run's first admitted attempt: every later attempt
    /// must present the same one or is refused. That is what stops an
    /// uncertain launch in one Herdr session from being "recovered"
    /// into a second, real launch in another, where the first session's
    /// agent is neither visible nor name-colliding.
    pub destination: String,
}

/// One child-Work role a container's outcome contract requires (§3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildOutcomeSpec {
    pub role: String,
    pub required: bool,
}

/// From sergeant's `StageKind` (Actor). `Container` (backend concept)
/// becomes `Deterministic`, naming wirk's own executor split (0001 D4)
/// instead of sergeant's backend taxonomy — which `Executor` impl runs
/// a Waypoint is a bin-crate binding decision (0022 D78), not a field
/// (orient/core.md line 63-68).
///
/// `Container` (W-A, p3-world-loop BUILD-BRIEF.md §3.1): a nested-stage
/// node. A container carries no intent, no command, no boundary and no
/// Run of its own — its `leaves` are the mechanism, its
/// `declared_outputs`/`required_child_outcomes` are the outcome
/// contract (`load_route`'s own refusals enforce the split). Leaves may
/// themselves be containers: BUILD-AMENDMENTS.md supersedes the
/// original draft's one-level bound — "preserve the recursive schema
/// and implement/test at least a grandchild nested stage."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaypointKind {
    Actor,
    Deterministic,
    Container,
}

/// Per orient/core.md line 70.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSpec {
    pub name: String,
    pub required: bool,
}

/// A Waypoint's declared required artifacts, authored on the Route
/// (build-brief.md §2 "OutputContract/Boundary ... decided now ...
/// Route-authored fields"; minimal wrapper, R6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputContract(pub Vec<ArtifactSpec>);

/// The declared mutation surface and authority envelope for a Waypoint
/// (vocabulary.md "Boundary": "declared mutation surface... per-Route";
/// 0001 D5), as path globs the Waypoint may mutate. Minimal, R6.
///
/// `Default` (p2-route-files, format.md §1, R6): an empty boundary
/// (`Boundary(Vec::new())`) so `WaypointDefinition.boundary`'s
/// `#[serde(default)]` has a value to default to, and a Route file
/// omitting `"boundary"` parses as authoring nothing yet, not a
/// refusal (refusal 9, out of scope: enforcement is P2.4's).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Boundary(pub Vec<String>);

// ---- Route file loader ------------------------------------------------

/// Every way `load_route` refuses a Route file (p2-route-files,
/// format.md §3, build-brief.md §2/§7). A refusal never touches the
/// journal (`wirkd`'s own `handle_submit` reads the file before
/// minting any id) — this type only carries the reason.
#[derive(Debug, Error)]
pub enum RouteError {
    #[error("route file not found: {path}")]
    NotFound { path: PathBuf },
    /// Unparseable JSON, an unknown field (`deny_unknown_fields`, D134
    /// row 10), a `kind` string that is neither `Actor` nor
    /// `Deterministic` (folds into this row, format.md refusal 5 — a
    /// closed two-variant enum, serde already refuses an unknown tag),
    /// or an Actor Waypoint with no `intent` (build-brief.md §2
    /// Disagreement 3's resolution: "row 2, same as Actor-with-command"
    /// — a post-parse structural check reported the same way).
    #[error("route file malformed at {path}: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("route has no waypoints")]
    NoWaypoints,
    #[error("duplicate waypoint id: {}", id.0)]
    DuplicateWaypoint { id: WaypointId },
    #[error("actor waypoint {} carries a command", id.0)]
    ActorWithCommand { id: WaypointId },
    #[error("deterministic waypoint {} has no command", id.0)]
    DeterministicMissingCommand { id: WaypointId },
    #[error("waypoint {} declares an output with an empty name", waypoint.0)]
    EmptyArtifactName { waypoint: WaypointId },
    /// W-A (§3.1): a `Container` carries no intent, command, or
    /// boundary of its own — those belong to its `leaves`.
    #[error("container waypoint {} carries an intent, command, or boundary", id.0)]
    ContainerWithMechanism { id: WaypointId },
    /// W-A (§3.1): a `Container` with no `leaves` has no mechanism at
    /// all — refused rather than silently vacuous.
    #[error("container waypoint {} has no leaves", id.0)]
    ContainerWithoutLeaves { id: WaypointId },
    /// P3 native launch selection (BUILD-BRIEF.md item 1): a
    /// `Deterministic`/`Container` Waypoint has no Run of its own to
    /// launch, so an authored `selection` there would be silently
    /// unused — refused at load, the same posture `ActorWithCommand`
    /// already takes for the reverse mismatch.
    #[error("non-actor waypoint {} carries an actor-only selection", id.0)]
    ActorSelectionOnNonActor { id: WaypointId },
    /// W-C1: a `Deterministic`/`Container` Waypoint hands its World to no
    /// actor, so an authored `orient` block there would assemble a
    /// projection nothing ever reads — refused at load rather than
    /// silently ignored.
    #[error("non-actor waypoint {} carries an actor-only orientation request", id.0)]
    OrientationOnNonActor { id: WaypointId },
}

/// Reads and validates a Route file (format.md §3, R2 co-located with
/// `Route`): a file path in, a `Route` or a named `RouteError` out.
/// Read once, at submit (build-brief.md §7.1) — `wirkd` never calls
/// this again for the same Work; auto-advance reads the journaled
/// `WaypointDefinition`s instead (`WorkSubmitted.waypoint_defs`).
pub fn load_route(path: &Path) -> Result<Route, RouteError> {
    let text = std::fs::read_to_string(path).map_err(|_| RouteError::NotFound {
        path: path.to_path_buf(),
    })?;
    let route: Route = serde_json::from_str(&text).map_err(|source| RouteError::Malformed {
        path: path.to_path_buf(),
        reason: source.to_string(),
    })?;

    if route.waypoints.is_empty() {
        return Err(RouteError::NoWaypoints);
    }

    let mut seen: std::collections::HashSet<WaypointId> = std::collections::HashSet::new();
    validate_tree(&route.waypoints, path, &mut seen)?;

    Ok(route)
}

/// Recursive validation over a (possibly nested) `WaypointDefinition`
/// tree (W-A, §3.1): `DuplicateWaypoint`/`EmptyArtifactName` are
/// checked across the *whole* tree, not just one level, and a
/// `Container` node is checked against the outcome/mechanism split
/// while every other node keeps the original flat checks. Recursion has
/// no depth bound (BUILD-AMENDMENTS.md: nesting is not limited to one
/// level).
fn validate_tree(
    nodes: &[WaypointDefinition],
    path: &Path,
    seen: &mut std::collections::HashSet<WaypointId>,
) -> Result<(), RouteError> {
    for waypoint in nodes {
        if !seen.insert(waypoint.id.clone()) {
            return Err(RouteError::DuplicateWaypoint {
                id: waypoint.id.clone(),
            });
        }
        match waypoint.kind {
            WaypointKind::Actor => {
                if waypoint.command.is_some() {
                    return Err(RouteError::ActorWithCommand {
                        id: waypoint.id.clone(),
                    });
                }
                if waypoint.intent.is_none() {
                    return Err(RouteError::Malformed {
                        path: path.to_path_buf(),
                        reason: format!(
                            "waypoint {} is an actor Waypoint with no intent",
                            waypoint.id.0
                        ),
                    });
                }
                if !waypoint.leaves.is_empty() || !waypoint.required_child_outcomes.is_empty() {
                    return Err(RouteError::Malformed {
                        path: path.to_path_buf(),
                        reason: format!(
                            "waypoint {} is not a container but declares leaves or required_child_outcomes",
                            waypoint.id.0
                        ),
                    });
                }
            }
            WaypointKind::Deterministic => {
                if waypoint.command.is_none() {
                    return Err(RouteError::DeterministicMissingCommand {
                        id: waypoint.id.clone(),
                    });
                }
                if !waypoint.leaves.is_empty() || !waypoint.required_child_outcomes.is_empty() {
                    return Err(RouteError::Malformed {
                        path: path.to_path_buf(),
                        reason: format!(
                            "waypoint {} is not a container but declares leaves or required_child_outcomes",
                            waypoint.id.0
                        ),
                    });
                }
                if waypoint.selection.is_some() {
                    return Err(RouteError::ActorSelectionOnNonActor {
                        id: waypoint.id.clone(),
                    });
                }
                if waypoint.orient.is_some() {
                    return Err(RouteError::OrientationOnNonActor {
                        id: waypoint.id.clone(),
                    });
                }
            }
            WaypointKind::Container => {
                if waypoint.intent.is_some()
                    || waypoint.command.is_some()
                    || !waypoint.boundary.0.is_empty()
                {
                    return Err(RouteError::ContainerWithMechanism {
                        id: waypoint.id.clone(),
                    });
                }
                if waypoint.leaves.is_empty() {
                    return Err(RouteError::ContainerWithoutLeaves {
                        id: waypoint.id.clone(),
                    });
                }
                if waypoint.selection.is_some() {
                    return Err(RouteError::ActorSelectionOnNonActor {
                        id: waypoint.id.clone(),
                    });
                }
                if waypoint.orient.is_some() {
                    return Err(RouteError::OrientationOnNonActor {
                        id: waypoint.id.clone(),
                    });
                }
                validate_tree(&waypoint.leaves, path, seen)?;
            }
        }
        for output in &waypoint.declared_outputs {
            if output.name.is_empty() {
                return Err(RouteError::EmptyArtifactName {
                    waypoint: waypoint.id.clone(),
                });
            }
        }
        // Refusal 9 (format.md §3): an empty `boundary` parses and
        // loads clean — authoring is this item's, enforcement is
        // P2.4's.
    }
    Ok(())
}

// ---- Nested-stage tree helpers (W-A, §3.1-3.2) -------------------------
//
// Pure functions over a journaled `waypoint_defs` tree, shared by
// `fold` (this crate) and `wirkd::server` (closure evaluation,
// container-activation bookkeeping) so both read the same recursive
// structure the same way (R2).

/// Finds a `WaypointDefinition` by id anywhere in `tree`, at any depth.
pub fn find_definition<'a>(
    tree: &'a [WaypointDefinition],
    id: &WaypointId,
) -> Option<&'a WaypointDefinition> {
    for def in tree {
        if &def.id == id {
            return Some(def);
        }
        if let Some(found) = find_definition(&def.leaves, id) {
            return Some(found);
        }
    }
    None
}

/// The ancestor container ids of `id` in `tree`, immediate parent
/// first. Empty when `id` is a top-level entry or is not found.
pub fn ancestor_chain(tree: &[WaypointDefinition], id: &WaypointId) -> Vec<WaypointId> {
    fn walk(nodes: &[WaypointDefinition], id: &WaypointId, path: &mut Vec<WaypointId>) -> bool {
        for def in nodes {
            if &def.id == id {
                return true;
            }
            path.push(def.id.clone());
            if walk(&def.leaves, id, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    walk(tree, id, &mut path);
    path.reverse();
    path
}

/// `true` when `id` names `container`'s last direct child, by
/// declaration order (the DFS-order sibling boundary closure evaluation
/// walks outward from).
pub fn is_last_direct_child(container: &WaypointDefinition, id: &WaypointId) -> bool {
    container.leaves.last().map(|d| &d.id) == Some(id)
}

/// The first executable (non-`Container`) leaf reached by always
/// descending into a node's own first child — the DFS-first leaf of
/// `def`'s subtree. `None` for an empty `leaves` (refused at load) or a
/// non-container `def`.
pub fn first_dfs_leaf(def: &WaypointDefinition) -> Option<&WaypointId> {
    let first = def.leaves.first()?;
    match first.kind {
        WaypointKind::Container => first_dfs_leaf(first),
        _ => Some(&first.id),
    }
}

/// Every executable (`Actor`/`Deterministic`) leaf under `tree`, in DFS
/// order — the flattened execution sequence `WorkSubmitted.waypoints`
/// carries, unchanged in shape whether or not any `Container` nodes are
/// present (fold.md's own "old journal with no containers folds
/// exactly as today").
pub fn flatten_leaves(tree: &[WaypointDefinition]) -> Vec<WaypointId> {
    let mut out = Vec::new();
    fn walk(nodes: &[WaypointDefinition], out: &mut Vec<WaypointId>) {
        for def in nodes {
            match def.kind {
                WaypointKind::Container => walk(&def.leaves, out),
                _ => out.push(def.id.clone()),
            }
        }
    }
    walk(tree, &mut out);
    out
}

// ---- ExecutionTriple ------------------------------------------------------

/// 0022 D73: names stand. Adopted from sergeant's causation triple,
/// cited in `wirk/src/main.rs` ("sergeant-rs v0.3.0, W1 hierarchical
/// execution contract §6"); reshaped to carry `RunId` at the third slot
/// per wirk's Run/Waypoint split (orient/core.md line 98-103). Moved
/// into W1 because `ActorWorld` carries it as `triple: ExecutionTriple`
/// (build-brief.md §2 amendment), not three separate strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionTriple {
    pub estate_root: String,
    pub work_id: WorkId,
    pub run_id: RunId,
}

/// The explicit source inspection contract for a reserved World. Missing
/// fields in historical journals are unknown; they are never guessed from a
/// path or a SHA-shaped string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceBasis {
    #[default]
    Unknown,
    Git {
        base: String,
    },
    OutputOnly {
        reference: String,
    },
}

// ---- World ----------------------------------------------------------------
//
// world.md §1's `HerdrWorld`/`DeterministicWorld` shapes, adopted
// verbatim but renamed to wirk-core's own vocabulary (build-brief.md
// §2: core.md's single `World` struct is rejected, replaced by
// world.md's two-struct enum; J3 + R6 — the enum is the one-line glue
// the shared `Executor` trait signature needs, W2). Named `ActorWorld`
// / `World::Actor` to match `WaypointKind::Actor` (R2, reused verbatim
// from this file) rather than `Herdr`: wirk-core's own vocabulary names
// no Herdr-shaped type (0022 D71; w1/VERIFY.md Finding 3 — the brief's
// `HerdrWorld` name conflicted with the deny-list's own vocabulary, and
// the deny-list wins, J3 on 0001 D7).
// world.md §3: R1, no Atlas field or import anywhere below.

/// World handed to an actor (Herdr-pane / Claude) Waypoint at launch.
/// Assembled once, at reservation, from Work + Route + git
/// (orient/world.md §1, §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorWorld {
    /// Work: which repo this Work targets (0001 D6 carve list names "repo").
    pub repository: String,
    /// git: created wirk-side by `git worktree add` (operation-map row
    /// "Create a worktree": "none (wirk-side git)"; 0018 D60).
    pub worktree_path: PathBuf,
    /// git: the work branch cut for this Waypoint (0001 D9 evidence 6,
    /// "base SHA").
    pub branch: String,
    /// git: exact commit the worktree was cut from, pinned at creation
    /// (0001 D9 evidence 6).
    pub base_sha: String,
    #[serde(default)]
    pub source_basis: SourceBasis,
    /// env: `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`, the injected
    /// execution triple (claim-contract.md; 0001 D3).
    pub triple: ExecutionTriple,
    /// Work: the intent text this Waypoint executes (0001 D1: "wirk
    /// executes it").
    pub intent: String,
    /// Route: the Waypoint's declared required artifacts (0001 D9
    /// evidence 3; claim-contract.md "What wirkd validates").
    pub output_contract: OutputContract,
    /// Route: declared mutation surface and authority envelope for this
    /// Waypoint (vocabulary.md "Boundary"; 0001 D5).
    pub boundary: Boundary,
    /// W-B target binding: when this Waypoint declares a
    /// `ReviewContract`, the exact identity each declared selector
    /// resolved to at reservation — **the point where the review's
    /// target becomes fixed**, before the review executes and before the
    /// operator admits anything.
    ///
    /// Empty for every Waypoint that declares no review, which is every
    /// Waypoint outside the agentic class. `#[serde(default)]`, and
    /// `WorldHash::of` hashes it **only when non-empty**, so a World
    /// without frozen targets hashes byte-identically to the way it
    /// always did: no landed or candidate-era Actor World hash moves
    /// because this field exists.
    #[serde(default)]
    pub review_targets: Vec<ReviewTarget>,
    /// W-C1: when this Waypoint declares an `orient` block, the stage
    /// projection actually assembled and durably written for this
    /// reservation — the reference, never the content. The content lives
    /// in `works/<work>/projections/<observation>.json`, written and
    /// fsynced *before* this event is appended, so a reference always
    /// names a file that was durable first.
    ///
    /// `None` for every Waypoint that declares no orientation request,
    /// which is every Waypoint written before this wave.
    /// `#[serde(default)]`, and `WorldHash::of` hashes it **only when
    /// present**, so no historical Actor World hash moves because this
    /// field exists.
    ///
    /// Only an `ActorWorld` carries one: a projection is context for an
    /// actor to read, and `validate_tree` refuses an `orient` block on a
    /// Waypoint that opens no actor Run rather than accepting authored
    /// configuration nothing consumes.
    ///
    /// `skip_serializing_if` so a World without a projection serializes
    /// to exactly the bytes it always did — a journal line, and not only
    /// a hash, is unchanged by this field existing. `Box`ed so the field
    /// costs one pointer rather than eighty bytes on every `World` ever
    /// moved: `World` is an enum whose two variants must not drift far
    /// apart in size (`clippy::large_enum_variant`), and a `Box`
    /// serializes transparently, so nothing on the wire or in a journal
    /// sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Box<EvidenceProjectionRef>>,
}

/// World handed to a deterministic (child/docker) Waypoint. Same
/// compilation source; no pane, no Herdr binding (orient/world.md §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicWorld {
    /// Route: the Waypoint's own command definition (0001 D4:
    /// "wirk-owned executors").
    pub command: Vec<String>,
    /// git: exact commit the child/docker executor's cwd is checked out
    /// at, explicit and validated, never read back from the checkout
    /// itself (issue 285; item 5 orient/child.md §7 item 1). A
    /// `ChildExecutor`/`DockerExecutor` refuses to launch a World whose
    /// `base_sha` is empty. Covered by `WorldHash::of`'s `Deterministic`
    /// arm below (J3 on 0029 D95's principle: the code state a
    /// deterministic command runs against is content, not location).
    pub base_sha: String,
    #[serde(default)]
    pub source_basis: SourceBasis,
    /// git: same `worktree_path` as `ActorWorld` (0018 D60).
    pub cwd: PathBuf,
    /// env: execution triple (claim-contract.md) plus any Route-declared
    /// vars.
    pub env: BTreeMap<String, String>,
    /// Route: same output-contract mechanism as `ActorWorld` (0001 D9
    /// evidence 3).
    pub expected_artifacts: OutputContract,
}

/// The bounded context one Waypoint receives (vocabulary.md), one
/// `Executor` trait (W2) implemented by both an actor (Herdr-pane)
/// executor and a deterministic one (0001 D2, D4). No Atlas variant:
/// orient/world.md §3's R1 answer for P1 is "not now" (0023 D81).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum World {
    Actor(ActorWorld),
    Deterministic(DeterministicWorld),
}

impl World {
    /// Whether this World carries frozen review targets — the fact that
    /// disqualifies it from the pre-v2 `WorldHash::legacy` encoding
    /// (`WorldHash::of`). Only an `ActorWorld` can carry them.
    pub fn carries_review_targets(&self) -> bool {
        match self {
            World::Actor(actor) => !actor.review_targets.is_empty(),
            World::Deterministic(_) => false,
        }
    }

    /// Whether this World carries a stage projection — the second fact
    /// that disqualifies it from the pre-v2 `WorldHash::legacy` encoding.
    pub fn carries_evidence(&self) -> bool {
        match self {
            World::Actor(actor) => actor.evidence.is_some(),
            World::Deterministic(_) => false,
        }
    }

    /// The projection this World was reserved with, if any.
    pub fn evidence(&self) -> Option<&EvidenceProjectionRef> {
        match self {
            World::Actor(actor) => actor.evidence.as_deref(),
            World::Deterministic(_) => None,
        }
    }

    pub fn source_basis(&self) -> &SourceBasis {
        match self {
            World::Actor(world) => &world.source_basis,
            World::Deterministic(world) => &world.source_basis,
        }
    }
}

// ---- Run ------------------------------------------------------------------

/// Which program drives this Run's actor pane (0041 D129, superseded by
/// 0056 D164). Not content the actor must produce, only which executor
/// runs the intent — the same distinction `ActorWorld.triple`/`env`
/// already draw between content and execution mechanism (`WorldHash::of`
/// excludes both), so `kind` lives on `Run`, never `World`, and is never
/// hashed (orient/actor.md §2).
///
/// 0056 D164 ("wirk accepts every agent kind Herdr accepts... wirk adds
/// no list of its own"): the closed two-variant enum of 0041 D129 is
/// superseded as a gate. R6 on the type: a newtype over `String` is the
/// smallest change that lets `ActorKind` carry any kind string Herdr
/// names — a plain field, no match arm to extend, no list anywhere —
/// while `claude()`/`opencode()` keep the two kinds Herdr and wirk both
/// already know about nameable by call site instead of by string
/// literal. `Default` is `claude()`: every journal on disk before this
/// field existed only ever ran Claude.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActorKind(pub String);

impl<'de> Deserialize<'de> for ActorKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let kind = String::deserialize(deserializer)?;
        Ok(ActorKind(match kind.as_str() {
            // Before 76cc10d these were enum variant names on disk.
            "Claude" => "claude".to_string(),
            "Opencode" => "opencode".to_string(),
            _ => kind,
        }))
    }
}

impl ActorKind {
    pub fn claude() -> Self {
        ActorKind("claude".to_string())
    }

    pub fn opencode() -> Self {
        ActorKind("opencode".to_string())
    }
}

impl Default for ActorKind {
    fn default() -> Self {
        ActorKind::claude()
    }
}

impl std::fmt::Display for ActorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Sergeant's execution attempt. `attempt: u32` adopted
/// (domain/execution.rs:36-52); `backend`/`native_id`/`stop_requested`
/// dropped (Herdr-shaped, 0022 D71) — `ExecutionHandle`
/// (backend/mod.rs:685-699) is the clearest instance of the dropped shape
/// (orient/core.md line 79-85).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub waypoint: WaypointId,
    pub attempt: u32,
    pub world_hash: WorldHash,
    pub state: RunState,
    /// W1 (0041 D129): additive, `#[serde(default)]` so a `Run`
    /// reconstructed from a journal line written before this field
    /// existed still folds, defaulting to `Claude` (the only kind that
    /// ever ran before). Seeded at `RunOpened` (before `--actor-kind`
    /// is known — submit precedes `wirk run`) and moved to the actual
    /// launched kind when `RunLaunched` folds (`Run::apply`).
    #[serde(default)]
    pub kind: ActorKind,
    /// P3 native launch selection: the resolved model/effort/raw-args
    /// this Run's launch was requested with, moved off `ActorSelection::default()`
    /// only when `RunLaunched` folds (`Run::apply`) — same seed/move
    /// pattern as `kind` above, and additive for the same reason
    /// (`#[serde(default)]`: a journal written before this field existed
    /// folds to the empty selection, which records that this journal
    /// carries no selection — not that the Run launched bare; see
    /// `EventKind::RunLaunched::selection`).
    #[serde(default)]
    pub selection: ActorSelection,
    /// `true` once this Run's own `RunLaunched` has folded — the durable
    /// marker `wirk run` reads to refuse silently altering an
    /// already-fixed launch on a repeated invocation
    /// (PREPARATION-ADJUDICATION.md point 3: "recovery/reinvocation
    /// cannot silently change an already fixed launch request"). Neither
    /// `kind`'s nor `selection`'s own default value can stand in for
    /// this: both are indistinguishable from "never launched" when the
    /// resolved request itself was the harness default.
    #[serde(default)]
    pub launched: bool,
    /// `true` once this Run's own `RunLaunchRequested` has folded — the
    /// durable binding of the resolved request, written **before** the
    /// irreversible Herdr `agent.start` call rather than after it
    /// (`RunLaunchRequested`'s own doc). `launch_requested && !launched`
    /// is the honest "a launch for this exact request was admitted, and
    /// what Herdr did with it is not known here" state: it is what a
    /// daemon loss or a lost reply in the launch window leaves behind,
    /// and it is deliberately *not* collapsed into either "never
    /// launched" or "launched".
    #[serde(default)]
    pub launch_requested: bool,
    /// Herdr's own `agent_started.argv` for this Run's launch — what
    /// Herdr says it submitted to the shell, evidence distinct from
    /// `selection` (what wirk asked for) and never proof a provider
    /// actually served the requested model (PREPARATION-ADJUDICATION.md
    /// point 3). Empty for a journal written before this field existed,
    /// or a Run that failed before Herdr ever replied.
    #[serde(default)]
    pub launch_argv: Vec<String>,
    /// The latest launch attempt `wirkd` admitted for this Run
    /// (`RunLaunchAttempted`) — who owns launching and driving it, and
    /// which Herdr destination its launch is bound to. `None` for a Run
    /// nobody has attempted yet and for every journal written before
    /// this field existed: an old journal folds with the fact absent,
    /// and absent is read as "no attempt admission was ever taken",
    /// never as "the attempt is free" for the purposes of claiming an
    /// already-launched Run.
    #[serde(default)]
    pub launch_attempt: Option<LaunchAttempt>,
    /// W-C3: the projection revisions this Run's own actor expanded its
    /// delivered context into, oldest first, folded from this Run's own
    /// `ProjectionExpanded` events.
    ///
    /// Revision 0 is **not** here: it lives in the reserved World, which
    /// expansion never touches. This is the tail of the chain whose head
    /// is `World::evidence()`, and it is per-Run by construction —
    /// `apply` ignores any event whose `run` is not this Run's id — so a
    /// retry's new Run starts at revision 0 with an empty tail and the
    /// superseded Run keeps every revision it was actually delivered.
    ///
    /// `#[serde(default)]`: a `Run` reconstructed from a journal written
    /// before this field existed folds with an empty tail, which is the
    /// literal truth for every one of them.
    #[serde(default)]
    pub expansions: Vec<EvidenceProjectionRef>,
}

/// Reshaped hard from sergeant's `StageStatus` (domain/workflow.rs:561-578,
/// lifecycle-driven per projection.rs:1093-1101): no lifecycle-derived
/// success variant remains (0001 D3; 0017 D56) — only `Claimed(ClaimId)`
/// (orient/core.md line 86-90).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunState {
    Open,
    Failed(FailureCause),
    Vanished,
    Claimed(ClaimId),
}

/// Incident file item 3, verbatim (orient/core.md line 92).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureCause {
    pub status: Option<String>,
    pub request_id: Option<String>,
    pub at: Timestamp,
    /// Bounded diagnostic text from the launch or transport (W3, issue
    /// 275: sergeant's actor-spawn failures landed a Work `blocked`
    /// with nothing but a daemon-side log line naming the cause; a
    /// journaled `FailureCause` now carries it, when the failure has
    /// one to give — HTTP-shaped failures may have only `status`).
    pub detail: Option<String>,
}

/// Unix ms; adopted from sergeant's ms fields (telemetry.rs:676-681)
/// (orient/core.md line 94).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

impl Run {
    /// The Run-level state machine (D9#2, D9#3). `LifecycleObserved` never
    /// advances or otherwise changes state (0001 D9 #2; 0017 D56): sergeant's
    /// stage-lifecycle events were authoritative
    /// (projection.rs:1093-1101), wirk's is deliberately inert.
    /// `ClaimRecorded{Refused}` leaves the Run open — a refused Claim is not
    /// a state transition (D9#3). `ClaimFiled` and `WorktreeCreated` change
    /// nothing here: filing is not deciding, and worktree creation is not a
    /// Run-state fact. An event whose `run` is not this Run's id is ignored
    /// (R6: the minimum a shared journal stream requires).
    pub fn apply(&mut self, event: &Event) {
        if event.run.as_ref() != Some(&self.id) {
            return;
        }
        match &event.kind {
            EventKind::LifecycleObserved { .. } => {}
            EventKind::RunFailed { cause } => {
                self.state = RunState::Failed(cause.clone());
            }
            EventKind::RunVanished => {
                self.state = RunState::Vanished;
            }
            EventKind::ClaimFiled { .. } => {}
            EventKind::ClaimRecorded {
                claim,
                claim_kind,
                verdict,
                ..
            } => match (verdict, claim_kind) {
                // A validated Done claim is the sole completion path
                // (0001 D3). A late but valid claim is still honored,
                // including one arriving after RunVanished (D9#5's
                // test): the Run only ever learns of Vanished-ness from
                // the executor's poll, not from the claim path, so a
                // claim that shows up afterward is real evidence, not
                // stale. J1: local, reversible, no contract crossed by
                // moving Vanished -> Claimed.
                (ClaimVerdict::Validated, ClaimKind::Done) => {
                    self.state = RunState::Claimed(claim.clone());
                }
                // A validated Question claim is not completion (W3,
                // issue 283): the Run stays Open. The Work moving to
                // WorkState::NeedsInput is item 2's fold over Work, a
                // separate reducer this Run-level `apply` does not
                // touch.
                (ClaimVerdict::Validated, ClaimKind::Question(_)) => {}
                (ClaimVerdict::Refused(_), _) => {}
            },
            EventKind::WorktreeCreated { .. } => {}
            // W-C3: the delivered context grew a revision. Appended,
            // never replaced — the chain is what the actor was
            // successively given, and a lost intermediate revision would
            // make a later one's `parent_projection` name nothing.
            EventKind::ProjectionExpanded { reference, .. } => {
                self.expansions.push(reference.as_ref().clone());
            }
            // W1 (0041 D129): the one place a Run's `kind` moves after
            // being seeded (at `RunOpened`, before `--actor-kind` is
            // known) to the kind `wirk run` actually launched —
            // `run_launched_with_opencode_kind_updates_run` pins it.
            //
            // P3 native launch selection: `selection`/`launch_argv` move
            // the same way, and `launched` becomes `true` — the durable
            // marker a repeated `wirk run` invocation reads to refuse
            // silently altering an already-fixed launch.
            EventKind::RunLaunched {
                actor_kind,
                selection,
                launch_argv,
                ..
            } => {
                self.kind = actor_kind.clone();
                self.selection = selection.clone();
                self.launch_argv = launch_argv.clone();
                self.launched = true;
            }
            // The pre-launch half of the same move: the request is bound
            // here, before Herdr is called at all, so a launch that
            // really happened can never be an unrecorded model choice.
            // `RunLaunched` above then re-states the same
            // `actor_kind`/`selection` (wirkd refuses a mismatch) and
            // adds Herdr's own argv.
            EventKind::RunLaunchRequested {
                actor_kind,
                selection,
                ..
            } => {
                self.kind = actor_kind.clone();
                self.selection = selection.clone();
                self.launch_requested = true;
            }
            // The attempt admission (N1's repair): the latest admitted
            // holder/destination replaces the previous one, which
            // `wirkd` only ever admits once the previous holder is
            // gone from the kernel's own process table.
            EventKind::RunLaunchAttempted {
                destination,
                holder,
                ..
            } => {
                self.launch_attempt = Some(LaunchAttempt {
                    holder: holder.clone(),
                    destination: destination.clone(),
                });
            }
            // W2 (p1-journal): RunOpened is Run-scoped bookkeeping the
            // Work-level `fold` owns (fold.md §1);
            // WorkSubmitted/WaypointReserved/WorkFailed/WorkCanceled
            // carry no `run` and never reach this match (the guard
            // above returns first) — arms kept only for exhaustiveness.
            EventKind::RunOpened { .. }
            | EventKind::WorkSubmitted { .. }
            | EventKind::WaypointReserved { .. }
            | EventKind::WorkFailed { .. }
            | EventKind::WorkCanceled { .. }
            | EventKind::ContainerActivated { .. }
            | EventKind::StageHeld { .. }
            | EventKind::StageClosed { .. }
            | EventKind::ChildWorkSpawned { .. }
            | EventKind::FindingRaised { .. }
            | EventKind::FindingSettled { .. }
            | EventKind::FindingAsserted { .. }
            | EventKind::FindingApplied { .. } => {}
        }
    }
}

// ---- Claim ------------------------------------------------------------------

/// 0001 D3: sole completion path; no sergeant equivalent
/// (orient/core.md line 97).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub run: RunId,
    pub triple: ExecutionTriple,
    pub artifacts: Vec<ArtifactRef>,
    /// The completion or question verb (0001 D5; W3, issue 283). See
    /// `ClaimKind`.
    pub kind: ClaimKind,
}

/// One artifact a Claim points at (orient/core.md line 78, reused for
/// `Claim.artifacts`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    /// The path the actor named, for a `Worktree` artifact. Empty for a
    /// `WorkOutputs` one, which carries no caller path at all: the
    /// daemon derives its address from the bound Work, the bound Run and
    /// this `name` (ruling 0145, `outputs::staged_path`).
    pub path: String,
    /// Which of the two places this Claim says the artifact is
    /// (ruling 0145). `#[serde(default)]`: every pre-0145 Claim named a
    /// checkout artifact and said nothing, and reads back as exactly
    /// that.
    #[serde(default)]
    pub store: ArtifactStore,
}

impl ArtifactRef {
    /// The historical shape: a caller-supplied path in the Run's own
    /// checkout.
    pub fn worktree(name: String, path: String) -> Self {
        ArtifactRef {
            name,
            path,
            store: ArtifactStore::Worktree,
        }
    }

    /// A declared output in this Work's managed output area, addressed
    /// by name alone.
    pub fn managed(name: String) -> Self {
        ArtifactRef {
            name,
            path: String::new(),
            store: ArtifactStore::WorkOutputs,
        }
    }
}

/// W-A correction (F3): one artifact as it actually validated —
/// the declared name, the path resolved against the Run's own checkout
/// at validation time, and the sha256 of the bytes that were inspected.
/// Journaled on `ClaimRecorded` and carried verbatim into
/// `OutcomeReceipt::Leaf`, so a later rewrite of that path can be
/// *detected* (the digest no longer matches) instead of being silently
/// attributed to the earlier Claim (BUILD-AMENDMENTS.md: "never
/// silently attribute later file bytes to an earlier Claim").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactReceipt {
    pub name: String,
    /// Worktree-relative where the join resolved inside the Run's
    /// checkout, otherwise the claimed path verbatim — for a `Worktree`
    /// receipt. For a `WorkOutputs` one (ruling 0145) it is
    /// `claims/<claim>/<name>` relative to `works/<work>/outputs/`,
    /// which is the daemon's own derived address and never a caller
    /// string.
    pub path: String,
    /// Lowercase hex sha256 of the file's bytes at validation.
    pub digest: String,
    /// Which root `path` is relative to (ruling 0145). Explicit, because
    /// a permissive `String` does not make a consumer support a second
    /// namespace: every consumer that resolves a receipt reads this to
    /// decide which root to join against, and one that does not
    /// understand the answer refuses rather than resolving against the
    /// wrong one.
    pub store: ArtifactStore,
}

/// A pre-correction record carried the artifact *name* alone
/// (`OutcomeReceipt::Leaf.artifacts` was a `Vec<String>`). Those
/// journals must stay replayable and inspectable — the W-A wave that
/// wrote them is not landed, but a reviewer's own probe estates carry
/// them, and refusing the whole journal over one field shape would
/// destroy exactly the historical inspectability this correction is
/// supposed to protect. A bare string reads as a receipt with no
/// recorded content identity, which every reader then reports as
/// unavailable (`"unrecorded"`) rather than as evidence that still
/// holds.
impl<'de> Deserialize<'de> for ArtifactReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Recorded {
                name: String,
                path: String,
                digest: String,
                /// Ruling 0145. Absent on every pre-0145 record, which
                /// had exactly one root and named none — so the default
                /// is `Worktree` and a historical journal resolves at
                /// the same path, with the same digest and the same
                /// availability as before this field existed.
                #[serde(default)]
                store: ArtifactStore,
            },
            NameOnly(String),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Recorded {
                name,
                path,
                digest,
                store,
            } => ArtifactReceipt {
                name,
                path,
                digest,
                store,
            },
            Wire::NameOnly(name) => ArtifactReceipt {
                name,
                path: String::new(),
                digest: String::new(),
                store: ArtifactStore::Worktree,
            },
        })
    }
}

impl ArtifactReceipt {
    /// The historical shape: an artifact validated in the Run's own
    /// checkout. Every call site that predates ruling 0145 means this.
    pub fn worktree(name: String, path: String, digest: String) -> Self {
        ArtifactReceipt {
            name,
            path,
            digest,
            store: ArtifactStore::Worktree,
        }
    }

    /// The sha256 of `path`'s bytes, or `None` when the file cannot be
    /// read at all (removed, replaced by a directory, unreadable) —
    /// which is the "explicit unavailable" answer, never a silent pass.
    /// R3: `sha2` is already this crate's hash dependency (`WorldHash`).
    pub fn digest_of(path: &std::path::Path) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Some(hex_lower(&hasher.finalize()))
    }
}

/// D9#3, D9#4 (orient/core.md line 105).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClaimVerdict {
    Validated,
    Refused(ClaimRefusal),
}

/// Per orient/core.md line 107. `OutOfBoundary` is new (W3, ruling 0026,
/// issues 280/288: Claude actors are unsandboxed and per-repo scope is
/// declared-not-enforced — a write outside the Waypoint's `Boundary`
/// globs or a repository this Work did not declare `Access::Write` for
/// is refused at Claim validation, not silently accepted on trust). The
/// `String` names the offending path or repository; the validator body
/// (item 3) decides which.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClaimRefusal {
    MissingArtifact(String),
    TripleMismatch,
    OutOfBoundary(String),
    /// The declared inspection contract could not be observed. This is
    /// neither success nor evidence of a boundary violation.
    ValidationUnavailable(String),
    /// A Claim filed against a `Run` already `RunState::Claimed` (build
    /// brief amendment 1, this item; d9_5's precedent: a valid Claim on
    /// a `Failed` or `Vanished` Run is still honored — late evidence,
    /// not stale — so only `Claimed` refuses here). J1, recorded in the
    /// closing ruling.
    AlreadyClaimed,
}

/// A Claim's completion verb (0001 D5: "the completion/question verb"),
/// carried on the Claim and echoed onto `EventKind::ClaimRecorded` so
/// `Run::apply` can see it without looking anywhere else (W3, issue
/// 283: sergeant had no structural needs-input signal, only
/// phrase-matched prose). `Done` is the ordinary completion path;
/// `Question(String)` is an actor's deliberate, typed escalation
/// carrying the question text. A `Question` claim, once `Validated`,
/// leaves the `Run` `Open` (`Run::apply` below) — the `Work` moving to
/// `WorkState::NeedsInput` is item 2's fold over `Work`, not a `Run`
/// concern; the `Executor` trait is unchanged, since a Question is
/// still filed via the same `wirk claim` path (0001 D3), not through
/// `poll`/`RunObservation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClaimKind {
    Done,
    Question(String),
}

// ---- Findings, Settlement, Assertion, Application (W-B) --------------------
//
// P3 W-B (`knowledge/work/p3-world-loop/W-B-BUILD.md`, corrected by
// `loop-b-prepare-correct/HANDOFF.md` and `W-B-CONSTRUCTION-REVIEW.md`).
// Five things kept distinct throughout: Evidence -> Finding (a bounded
// claim, raised) -> Settlement (a decision by an admitted authority,
// with its own check named) -> Application (the owning source actually
// changed) -> the estate index (derived, rebuildable, never the only
// copy). `wirk-core` gains no dependency on `wirk-atlas` (0022 D71's own
// crate-boundary discipline continued): a source coordinate travels as
// `EvidenceRef::Source`'s already-encoded, opaque `String` — only
// `wirk/src/wirkd/server.rs` (which depends on both crates) decodes and
// resolves it, the same split `encode_coordinate`/`decode_coordinate`
// already draw for Atlas's own wire shapes.

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FindingId(pub String);

/// One coordinate a Finding's evidence, contradiction, or `applies_to`
/// entry names — never a free string (construction review: "well-formed
/// strings plus alias membership are not admission"). `Source` is
/// resolved and admitted by `wirk/src/wirkd/server.rs` against the
/// raising Work's own scope before a `Finding` is ever minted; `Journal`
/// is checked against the raising Work's own parent/child lineage there
/// too. Both are frozen at raise time (`AdmittedEvidence`) and never
/// re-resolved to a different outcome later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceRef {
    /// The already-encoded `wirk_atlas::ExactCoordinate` string, exactly
    /// `wirk/src/wirkd/server.rs::encode_coordinate`'s own shape —
    /// opaque here, decoded only where the Atlas dependency lives.
    Source(String),
    /// One event in a Work's own journal, admitted only when that Work
    /// is the raising Work itself or lies on its own parent/child chain
    /// (construction review: "journal kinship is not universal evidence
    /// access" — every other Work in the estate is refused).
    Journal { work: WorkId, event: EventId },
    /// One *finding* in a Work's own journal, named as
    /// `work/<work-id>/finding/<finding-id>` — the spelling
    /// `--confirmed-by` already uses, one noun over from `Journal`.
    ///
    /// This is the reference a later independent Work needs to say what
    /// it thinks of an earlier record: put in `contradicts` it claims
    /// disagreement, in `evidence` it claims support. It is admitted by
    /// two routes and no others (`wirk/src/wirkd/server.rs::
    /// finding_reference_admitted`): journal kinship, exactly as
    /// `Journal` is, or — for a Work with no kinship at all — the same
    /// settled estate publication route the findings index already
    /// publishes by, so a reference reaches exactly the records its
    /// author could already discover and no more.
    ///
    /// Naming a target is a claim about it and nothing else. It does not
    /// settle, supersede, publish or validate the named record, it
    /// appends nothing to the journal that holds it, and it is not the
    /// `confirmed_by` child-proof obligation, which `ChildInvestigationConfirmed`
    /// re-derives for itself.
    Finding { work: WorkId, finding: FindingId },
}

/// The admission outcome for one `EvidenceRef`, decided once at raise
/// time and never promoted later (§3's own rule: "unavailable evidence
/// is never promoted to admitted on reread").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceOutcome {
    /// `generation`/`object_id` name the exact Atlas generation and blob
    /// a `Source` reference resolved against, or (for a `Journal`
    /// reference) the naming Work and Event id — an honest identity
    /// either way, never a bare boolean.
    Admitted {
        generation: String,
        object_id: String,
    },
    Unavailable {
        reason: String,
    },
    /// What a `Finding` reference resolved to, frozen at raise time like
    /// every other outcome. It keeps the four things a reader of a
    /// recorded relation has to be able to tell apart: the *claimed*
    /// relation is the list this entry sits in, the *resolved exact
    /// target* is `work`/`origin_event`, the *actual admission* is
    /// `route`, and the target's own *settlement standing at the moment
    /// of admission* is `standing` — which is a fact about the named
    /// record, never about the claim that names it.
    Relation {
        work: WorkId,
        origin_event: EventId,
        route: RelationRoute,
        standing: RelationStanding,
    },
}

/// Which of the two admission routes actually admitted a `Finding`
/// reference. Recorded rather than recomputed: the requesting Work's
/// lineage and the estate's publications both move on, and this says
/// what was true when the relation was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationRoute {
    /// The named record is in the referencing Work's own journal.
    OwnJournal,
    /// The named record is on the referencing Work's own parent/child
    /// lineage — the same kinship a `Journal` reference needs.
    Lineage,
    /// No kinship at all: the named record is a settled EstateLocal
    /// publication this Work's own bindings already entitle it to
    /// discover through the estate findings index.
    SettledEstatePublication,
}

/// The named record's own state when the relation was admitted. A
/// disagreement with a settled record and a disagreement with an
/// unsettled one are different things, and a reader must not have to
/// guess which it is holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationStanding {
    Settled,
    Unsettled,
}

/// One piece of evidence, frozen at raise time: the reference the
/// caller named and the outcome admission actually reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedEvidence {
    pub reference: EvidenceRef,
    pub outcome: EvidenceOutcome,
}

/// The four kinds a Finding names (accepted proposal §5; no `Shared`
/// scope variant exists at any layer of this type — estate isolation is
/// total, per §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Gap,
    ContradictedAssumption,
    Relationship,
    VerifiedOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingScope {
    WorkLocal,
    EstateLocal,
}

/// An actor's own bounded claim (§5.2). Evidence-backed by construction:
/// `evidence`/`contradicts`/`applies_to` are all `AdmittedEvidence`,
/// never a free string a proposer could mint authority from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub id: FindingId,
    pub work: WorkId,
    pub run: RunId,
    pub waypoint: WaypointId,
    pub kind: FindingKind,
    pub scope: FindingScope,
    pub claim: String,
    pub evidence: Vec<AdmittedEvidence>,
    pub contradicts: Vec<AdmittedEvidence>,
    pub applies_to: Vec<AdmittedEvidence>,
    /// A Work may replace its own provisional Finding with a traceable
    /// newer one (construction review: "naming another Finding does not
    /// grant authority over it") — checked at raise time against this
    /// same Work's own journal only; superseding an already-settled
    /// estate record needs its own settlement rules (`SupersededInOrigin`,
    /// below), never mere same-origin authorship.
    pub supersedes: Option<FindingId>,
    pub proposed_change: Option<String>,
    /// W-B obligation proof: which verification obligation this Finding
    /// claims to discharge (`--obligation <id>@<edition>`). A
    /// `VerifiedOutcome` Finding that names none is never settleable —
    /// naming an obligation is *necessary*, never sufficient: the
    /// Waypoint whose Claim is cited must itself declare exactly this
    /// obligation, and the estate policy must admit that obligation's
    /// own content basis.
    #[serde(default)]
    pub obligation: Option<ObligationRef>,
    /// W-B obligation proof: the child Finding this Finding names as its
    /// independent confirmation (`--confirmed-by work/<id>/finding/<id>`),
    /// for the `ChildInvestigationConfirmed` class. Explicit, never
    /// inferred from matching prose.
    #[serde(default)]
    pub confirmed_by: Option<ConfirmedBy>,
}

/// One named child Finding a parent Finding cites as its confirmation.
/// Both coordinates are required: the child Work and the Finding id
/// inside it. wirkd re-derives everything about it from the child's own
/// journal and the parent's own `StageClosed` receipt — this is a
/// pointer, never a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedBy {
    pub work: WorkId,
    pub finding: FindingId,
}

/// The construction review's corrected shape: a `Settlement` never
/// carries a wire-supplied "decision" — settling *is* the decision
/// (some admitted, derived fact held). Accept/reject/defer/supersede
/// judgements a human states remain real, but only ever as an honestly
/// unverified `Assertion` (§2.5) that never becomes a `Settlement`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Accepted,
    PartiallyAccepted,
    Rejected { reason: String },
    Deferred,
    Superseded(FindingId),
}

/// `UnixStream::peer_cred()` (R3, stdlib): attribution, never
/// authentication (§2.3's own boundary — the same OS uid runs both an
/// honest human terminal and an actor's shell, and this daemon's socket
/// admits both identically). Recorded on every `Assertion` so a reader
/// can see who *claims* to have spoken, never a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerIdentity {
    pub uid: u32,
    pub gid: u32,
}

/// §2.5: the complete, usable human/client path — recorded, never
/// authority. `by` is a caller-supplied label, never verified;
/// `wirk finding list` renders it "recorded name: `<by>`, unverified".
/// Never sets a Finding `Settled` and never suppresses it from later
/// consultation (an asserted `Rejected`/`Deferred` still surfaces,
/// captioned with the assertion attached).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assertion {
    pub decision: Decision,
    pub by: String,
    pub reason: Option<String>,
    pub peer: PeerIdentity,
    pub at: Timestamp,
    /// Who wirkd itself admitted as the author of this sentence, at the
    /// moment it was written (`ASSERTION-AUTHOR-ADJUDICATION.md`). An
    /// assertion is written into the *target* Finding's own journal, and
    /// `finding assert` admits any requester on that Finding's lineage —
    /// so the Work whose journal holds an assertion is routinely not the
    /// Work that wrote it, and only this field says which one did.
    ///
    /// `None` on every assertion journaled before this field existed:
    /// unknown, and it stays unknown. `by` is a caller-supplied label
    /// and `peer` is an OS credential the daemon explicitly refuses to
    /// treat as identity — neither may be promoted into an authorship
    /// claim after the fact.
    #[serde(default)]
    pub author: Option<AssertingAuthor>,
}

/// The authorship half of an `Assertion`: server-admitted at write time,
/// never client-supplied.
///
/// `Administrator` is the explicitly unscoped `--admin` path, which
/// names no requesting Work and therefore no source breadth — the
/// sentence it wrote could quote anything in the estate. It is recorded
/// honestly rather than attributed to the journal that holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssertingAuthor {
    Work(WorkId),
    Administrator,
}

/// The closed set of settlement mechanisms this increment compiles in
/// (§2.4: "closed — a client cannot name a class that is not compiled
/// in, and the policy file cannot introduce one"). `SupersededInOrigin`
/// is the one derived path for `Decision::Superseded` that settles
/// rather than merely asserting: a *later* Finding in the *same* Work
/// naming an earlier one via `supersedes` is a journal fact the Work
/// owns on both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementClass {
    DeterministicVerified,
    /// W-B-AGENTIC-PROOF.md: the agentic sibling of
    /// `DeterministicVerified`. A bounded independent Actor review,
    /// performed under a policy-admitted recipe against admitted targets,
    /// discharging its Waypoint's own declared review obligation. It is a
    /// *distinct* mechanism with its own standing, never a deterministic
    /// check in disguise and never a rubber stamp for one: see
    /// `ActorReviewProof` for exactly what it proves and what it leaves
    /// as judgement.
    ActorReviewed,
    ChildInvestigationConfirmed,
    SupersededInOrigin,
}

/// What a `DeterministicVerified` settlement proves, and the immutable
/// receipt that discharged it. Every field is re-derived by wirk from
/// the journal at settlement time, never taken from a proposer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicProof {
    pub obligation: ObligationRef,
    /// `obligation_basis` for the discharging Waypoint — the value the
    /// estate's settlement policy must have admitted.
    pub basis: String,
    /// The Route-authored, limited statement the discharge proves.
    pub proves: String,
    pub waypoint: WaypointId,
    pub attempt: u32,
    pub world_hash: WorldHash,
    pub artifacts: Vec<ArtifactReceipt>,
}

/// What an `ActorReviewed` settlement proves.
///
/// **Proved**, every field re-derived from the journal at settlement
/// time: this exact `world_hash` (which content-addresses the reviewing
/// World's repository, branch, `base_sha`, source basis, `intent`,
/// output contract and boundary) ran as `attempt` of `waypoint`, on the
/// Waypoint's current activation and current reservation; it produced the
/// obligated `report` artifacts with their recorded content digests; its
/// reviewer's Finding applied to every declared target at the exact
/// `generation`/`object_id` this daemon admitted; and it recorded
/// `decision`, which is one of the closed set the Route declared.
///
/// **Not proved, and never claimed**: that the review's conclusion is
/// true. `proves` is the Route-authored statement about the *review
/// having been performed under this recipe*, not about the world. The
/// reviewer's own English sentence stays a recorded, unverified claim
/// beside it, exactly as it does for every other class.
///
/// This variant did not exist before the agentic wave, so unlike
/// `DeterministicProof`/`ChildProof` it is not optional: no journal can
/// contain an `ActorReview` check written without one, and pretending a
/// historical form exists would be a fabrication. Future fields go inside
/// this struct with `#[serde(default)]`, which is the lesson the
/// historical-readability correction already paid for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorReviewProof {
    pub obligation: ObligationRef,
    pub basis: String,
    pub proves: String,
    pub waypoint: WaypointId,
    pub attempt: u32,
    pub world_hash: WorldHash,
    /// The reviewing World's own `intent`, as reserved — the actual
    /// instruction the review was carried out under.
    pub intent: String,
    /// The Route-authored verification recipe and edition.
    pub recipe: String,
    /// The complete checked identity of every declared target: the
    /// selector the Route asked for and the exact membership, source,
    /// generation and object the reviewing World was frozen against and
    /// the reviewer's own admitted evidence matched.
    pub targets: Vec<ReviewTarget>,
    /// The structured outcome the reviewer recorded, from the declared
    /// closed set. This, not the prose, is the checkable decision.
    pub decision: FindingKind,
    /// The obligated report artifacts, as the Claim validated them.
    pub report: Vec<ArtifactReceipt>,
}

/// One obligated container role, and the child settlement that actually
/// discharged it. `mechanism`/`mechanism_basis` are the container
/// obligation's own `requires` and the exact basis that child's own
/// settlement discharged — the immutable verification execution behind
/// the parent's claim, which the estate policy must also have admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DischargedRole {
    pub role: String,
    pub child: WorkId,
    /// The child's own closing `ClaimId`, from the parent's receipt.
    pub claim: ClaimId,
    /// The child's own settled Finding that discharged `mechanism`.
    pub finding: FindingId,
    pub mechanism: ObligationRef,
    pub mechanism_basis: String,
    /// The child's own `FindingSettled` event.
    pub settled_event: EventId,
}

/// What a `ChildInvestigationConfirmed` settlement proves. `roles`
/// carries one entry per **obligated** role (the container obligation's
/// own `outputs`), each independently verified against the container's
/// current activation — partial completion is never full proof.
/// `confirmed_by` is the child Finding the parent named explicitly, and
/// is always one of `roles`' own findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildProof {
    pub obligation: ObligationRef,
    pub basis: String,
    pub proves: String,
    pub confirmed_by: FindingId,
    pub requires: ObligationRef,
    pub roles: Vec<DischargedRole>,
}

/// Fields a record carries that this revision does not interpret.
///
/// The independent re-review's executed C2: a settlement written by the
/// intermediate revision carried its obligation, basis, World hash,
/// proven statement and artifact receipts as ten *flat* fields, before
/// they moved inside `proof`. The reader could not match that shape, and
/// said so as a claim about the past — "the obligation it discharged was
/// never recorded", "not reconstructible" — while the fields sat unread
/// in the journal line. That is a fact about this revision stated as a
/// fact about history.
///
/// Capturing them changes what can honestly be said: the record can now
/// show exactly what it holds and name it as unread, and the bytes
/// survive a read-and-rewrite. It deliberately does **not** promote them
/// to a proof: the basis rule those values were computed under is not
/// this revision's rule, so presenting them as a current
/// `DeterministicProof` would manufacture a currency they do not have.
/// They are disclosed, never relied on — `obligation_admission` treats a
/// record with no readable `proof` as unadmittable, whatever it carries
/// here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnreadFields(pub std::collections::BTreeMap<String, serde_json::Value>);

impl UnreadFields {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The journal fact each `SettlementClass` binds to — never a bare id
/// comparison, never an unrelated successful command. Every field here
/// is something wirkd itself re-derives from a journal at settlement
/// time, never trusted from a proposer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementCheck {
    /// The journal facts a `DeterministicVerified` settlement rests on.
    /// `work`/`claim`/`claim_event` are the three the very first W-B
    /// revision recorded and are required; `proof` is everything the
    /// obligation-proof revision added.
    ///
    /// **`proof: None` is a historical record, never a default.**
    /// `#[serde(default)]` is what lets a `FindingSettled` journaled by
    /// an earlier revision (`0634657` and before) still deserialize —
    /// the independent review's executed C3, where the added fields
    /// were required and a base-era Work journal and the estate
    /// findings index both went malformed rather than fail-closed.
    /// Absence is rendered as exactly what it is: this settlement
    /// predates the obligation-proof contract and what it proved was
    /// not recorded. It is never filled with zero values and never
    /// presented as newly verified — and nothing in this crate can
    /// *mint* a `None`: `deterministic_verified_readiness` always
    /// constructs `Some`.
    ValidatedClaim {
        work: WorkId,
        claim: ClaimId,
        claim_event: EventId,
        #[serde(default)]
        proof: Option<DeterministicProof>,
        /// Whatever else the record carried that this revision does not
        /// interpret (`UnreadFields`). Captured so the reader can say
        /// what is present instead of asserting what the past did not
        /// record, and re-emitted verbatim so reading a journal never
        /// loses its bytes.
        #[serde(flatten, default, skip_serializing_if = "UnreadFields::is_empty")]
        unread: UnreadFields,
    },
    /// The journal facts a `ChildInvestigationConfirmed` settlement
    /// rests on. Every field except `proof` existed before the
    /// obligation-proof revision and stays required; `proof` carries
    /// what that revision added, and `None` means the same historical
    /// thing it means above.
    /// W-B-AGENTIC-PROOF.md. The journal facts an `ActorReviewed`
    /// settlement rests on: the reviewing Work, its Validated `Done`
    /// Claim and that Claim's own event, plus the review proof itself.
    ActorReview {
        work: WorkId,
        claim: ClaimId,
        claim_event: EventId,
        proof: ActorReviewProof,
    },
    ChildReceipt {
        parent: WorkId,
        waypoint: WaypointId,
        attempt: u32,
        child: WorkId,
        role: String,
        claim: ClaimId,
        closed_event: EventId,
        child_raise_event: EventId,
        #[serde(default)]
        proof: Option<ChildProof>,
        #[serde(flatten, default, skip_serializing_if = "UnreadFields::is_empty")]
        unread: UnreadFields,
    },
    SupersededBy {
        work: WorkId,
        finding: FindingId,
        raise_event: EventId,
    },
}

/// §2.4: pre-admission is `<estate>/policy/settlement.json`, read by
/// wirkd, never written by it or by any Work. `policy_digest` is bound
/// into the settlement at the moment it is minted and never recomputed
/// (§6: "an already-journaled settlement is never recomputed, re-minted
/// or rewritten, whatever the file now says").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementAuthority {
    pub class: SettlementClass,
    pub policy_version: u32,
    pub policy_digest: String,
}

/// wirkd's own sole producer (`FindingSettled`'s own doc). `settled_by`
/// names the qualifying event `check` relied on; `minted_at_startup`
/// distinguishes a startup repair mint from an inline one — attribution
/// only, both are equally settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settlement {
    pub authority: SettlementAuthority,
    pub check: SettlementCheck,
    pub settled_by: EventId,
    pub at: Timestamp,
    #[serde(default)]
    pub minted_at_startup: bool,
}

/// One exact generation/resource identity of a source membership (§4):
/// `generation` is the Atlas `GenerationId`, `object_id` the Git blob at
/// the finding's own coordinate — both re-derived by wirkd, never taken
/// from a caller's claim.
/// W-B-CORRECT.md defect 3 ("preserve ... explicit deletion absence"):
/// `object_id: None` is a real, distinct fact — the resource is absent
/// at this generation — never a fabricated empty string standing in for
/// "no object id" (the authority review's own executed counterexample:
/// `after_object_id.unwrap_or_default()` rendered `"object_id": ""`, a
/// value indistinguishable from a real empty-blob object id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationPoint {
    pub generation: String,
    pub object_id: Option<String>,
}

/// W-B-CORRECT.md defect 3 ("require current valid producing Work/Run/
/// World for actor-attributed assertions"): the real, checked identity
/// that made the call — never a caller-supplied name alone. `wirkd`
/// derives this from the caller's own injected triple (`TripleMismatch`/
/// `WorkTerminal`/current-run, the identical three checks
/// `handle_finding_raise` already performs), the same way `raise` never
/// trusts a bare string for *its* own Run identity either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationProducer {
    pub work: WorkId,
    pub run: RunId,
    pub world_hash: WorldHash,
}

/// §4's "owning-source authority": a `Claim` attribution is derived from
/// a real Validated Claim whose Work is bound `Write` to the changed
/// membership; `Asserted` is the same unverified standing as §2.5's
/// human path (ruling 0077's separation: recording a durable, evidenced
/// assertion in Atlas state is distinct from source mutation authority)
/// — unverified means the *judgement*, never the caller's own identity:
/// `producer` is real and checked either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attribution {
    Claim {
        work: WorkId,
        run: RunId,
        claim: ClaimId,
        claim_event: EventId,
    },
    Asserted {
        by: String,
        peer: PeerIdentity,
        producer: ApplicationProducer,
    },
}

/// §4.5: whether the recorded byte change *implements* the finding is
/// never mechanical in P3 and is never pretended to be — always a named,
/// attributed judgement, distinct from the mechanical proof above it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertedJudgement {
    pub by: String,
    pub peer: PeerIdentity,
    pub at: Timestamp,
}

/// §4: the owning source's exact before/after change, split from the
/// judgement that it implements the finding. `revision` is `after`'s own
/// generation's revision, re-derived by wirkd, never the caller's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationRef {
    pub source: String,
    pub before: GenerationPoint,
    pub after: GenerationPoint,
    pub revision: String,
    pub attribution: Attribution,
    pub implements_finding: AssertedJudgement,
}

/// A Finding's own terminal-or-not state, folded from
/// `FindingSettled` alone — an `Assertion` never appears here (§2.5: it
/// never sets `Settled`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingState {
    Proposed,
    // Boxed: `Settlement` is large enough that an unboxed variant here
    // would make every `FindingState` (most of which are `Proposed`)
    // pay its size (clippy::large_enum_variant).
    Settled(Box<Settlement>),
}

/// One Finding's complete in-Work record: the bounded claim itself, its
/// settlement state, every assertion ever recorded against it (kept even
/// once settled — an assertion is never erased by a later settlement),
/// and its Application, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRecord {
    pub finding: Finding,
    pub state: FindingState,
    pub assertions: Vec<Assertion>,
    /// W-B-CORRECT.md defect 3 ("historical Application is not erased
    /// merely because a newer generation is published"): every real
    /// `FindingApplied`, appended, newest last — never a single slot a
    /// later Application silently overwrites.
    pub applied: Vec<ApplicationRef>,
}

/// One settlement candidate `fold` derived purely from this Work's own
/// journal (`Work.settlement_ready`'s own doc) — wirkd checks it against
/// the admitted policy before minting anything; `fold` itself never
/// mints, never reads a file, never reads another journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadySettlement {
    pub finding: FindingId,
    pub class: SettlementClass,
    pub check: SettlementCheck,
}

// ---- Event ------------------------------------------------------------------

/// Same as sergeant's Event (0001 D5). Reshaped from string-tagged `KIND_*`
/// constants (projection.rs:27-31, api.rs:47-58) into a closed
/// serde-tagged enum (R3) — a malformed kind fails to deserialize instead
/// of hitting a wildcard match (orient/core.md line 109-114).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub work: WorkId,
    pub run: Option<RunId>,
    pub at: Timestamp,
    pub kind: EventKind,
}

/// Internally tagged (`#[serde(tag = "kind")]`, R3: serde already allowed)
/// on the `kind` field's own name (orient/core.md line 115-124).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum EventKind {
    /// D9#2: folding this NEVER changes RunState (`Run::apply`'s own
    /// `{ .. }` arm stays inert); sergeant's stage-lifecycle events WERE
    /// authoritative (projection.rs:1093-1101), wirk's is inert there.
    ///
    /// Ruling 0052 D156 (P2.6 W2): at the *Work* level this is no
    /// longer wholly inert — `fold`'s own arm now acts on
    /// `status == "Blocked"`/`"Working"` (below), reusing this event
    /// kind rather than adding a new one (R2: the loop already journals
    /// `LifecycleObserved` for every changed status, Blocked included;
    /// the only gap was `fold` ignoring it). `detail` carries the
    /// pane's last screen lines for a `Blocked` observation (`None` for
    /// every other status this loop journals) — `#[serde(default)]` so
    /// a journal written before this wave (no `detail` field at all)
    /// still deserializes.
    LifecycleObserved {
        status: String,
        #[serde(default)]
        detail: Option<String>,
    },
    RunFailed {
        cause: FailureCause,
    },
    RunVanished,
    ClaimFiled {
        claim: ClaimId,
    },
    /// `claim_kind` is carried on the event, not looked up from the
    /// Claim itself (W3, issue 283): `Run::apply` sees only the event,
    /// so a `Question` claim's `ClaimKind` has to travel with the
    /// `ClaimRecorded` fact to let the Run-level reducer tell a
    /// completion from a question without a side lookup. Named
    /// `claim_kind`, not `kind`: `EventKind`'s own internal tag field is
    /// already named `kind` (`#[serde(tag = "kind")]` above), and serde
    /// refuses a variant field with the same name as its enum's tag.
    ClaimRecorded {
        claim: ClaimId,
        claim_kind: ClaimKind,
        verdict: ClaimVerdict,
        /// W-A correction (F3): the exact artifacts this Claim was
        /// validated against, each with the content identity read at
        /// validation. Empty for a refused Claim, a Question, or a
        /// pre-correction journal (`serde(default)`), which is why the
        /// Claim's own `artifacts` are the receipt's only source — a
        /// closure never re-derives them from the Route's declared
        /// names and a mutable path.
        #[serde(default)]
        artifacts: Vec<ArtifactReceipt>,
    },
    /// D9#6.
    WorktreeCreated {
        repo: String,
        base_sha: String,
    },
    /// Work doesn't exist until submitted; `waypoints` is the Route's
    /// ordered plan at submission time — the only way `fold`'s
    /// single-argument signature can tell "last waypoint" without a
    /// `Route` parameter (fold.md §2, R6; BRIEF.md Intent).
    WorkSubmitted {
        route: RouteId,
        repositories: Vec<RepositoryBinding>,
        intent: String,
        waypoints: Vec<WaypointId>,
        /// p2-route-files W2 (build-brief.md §2 Disagreement 2, J4): the
        /// submitted `--command` argv W3's scaffolding once carried here
        /// for auto-advance's own fallback is removed, dropped with no
        /// compatibility field — no production journal predates this
        /// wave to protect, and the Route file's own Deterministic
        /// Waypoint now carries its command on `waypoint_defs` below
        /// unconditionally.
        /// p2-route-files (build-brief.md §7.1, format.md §4, J3 on
        /// 0029 D95): the full Route content as authored in the loaded
        /// file — every `WaypointDefinition`, in Route order — read
        /// once here at submit and never re-read from the file again.
        /// Auto-advance (`handle_claim`) and every later reader take a
        /// Waypoint's intent/command/outputs from here, never from
        /// `Route::load`, so a file edited after submit cannot change
        /// what a Work already reserved. Additive, `#[serde(default)]`
        /// so a `WorkSubmitted` written before this field existed still
        /// folds (`old_worksubmitted_without_waypoint_defs_still_folds`
        /// pins it); empty for a submit that named no Route file (a
        /// bare-name submit still taking the hardcoded Route this
        /// wave).
        #[serde(default)]
        waypoint_defs: Vec<WaypointDefinition>,
        /// W-A (§3.3): present only for a child Work, naming the parent
        /// Work/container/Run/role it was submitted under.
        /// `#[serde(default)]` so a `WorkSubmitted` written before this
        /// field existed still folds.
        #[serde(default)]
        parent: Option<ParentBinding>,
        /// P3 W3 (ruling 0090; the narrow core allowance W3-BUILD.md
        /// grants for "the already-required W3 authority binding"):
        /// which named entry of `repositories` is this Work's actual
        /// execution/write checkout, distinct from a merely readable
        /// evidence source — `wirkd` resolves this itself (refusing an
        /// ambiguous or unknown designation, `handle_submit`), never
        /// trusting a bare position in the list. `#[serde(default)]` so
        /// a `WorkSubmitted` written before this field existed still
        /// folds, reading as the legacy "first binding is execution"
        /// interpretation wherever a reader still needs one.
        #[serde(default)]
        execution_repo: Option<String>,
        /// The canonical repository identity (`git rev-parse
        /// --path-format=absolute --git-common-dir`) `wirkd` itself
        /// verified for `execution_repo`'s real checkout at submit
        /// time — never accepted from a client's own claim. `None`
        /// when this Work has no checkout yet to verify (a bare Actor
        /// submission materialized later by `wirk run`) or predates
        /// this field.
        #[serde(default)]
        execution_identity: Option<String>,
    },
    /// Journals the compiled World at reservation (BRIEF.md Intent;
    /// evidence/work/p1-executor-design/orient/world.md §5) so a
    /// resumed/retried Run relaunches without recompiling (fold.md §2,
    /// R6).
    WaypointReserved {
        waypoint: WaypointId,
        world_hash: WorldHash,
        world: World,
    },
    /// W-C3: this Run's actor expanded its delivered context, and the
    /// projection revision it was given is durable.
    ///
    /// The reserved World is untouched — expansion adds a revision, it
    /// does not edit one — so `world_hash` never moves and the stage's
    /// resume key is unchanged by a stage having asked a second
    /// question. The file this reference names is fsynced and renamed
    /// before this event is appended, exactly as the initial
    /// reservation's is.
    ///
    /// `parent` is the observation of the revision this one expands, so
    /// the chain is verifiable from the journal alone: the first
    /// expansion's parent is the World's own reference, and each later
    /// one's parent is its predecessor. wirkd appends this event under
    /// the same journal guard the parent was read under, so two
    /// concurrent expansions produce two ordered revisions or an
    /// explicit `Conflict`, never a lost update.
    ///
    /// There is no client-callable producer: `record` refuses it, and
    /// `world expand` is the only verb that mints one.
    ProjectionExpanded {
        waypoint: WaypointId,
        parent: ObservationId,
        /// `Box`ed for the same reason `ActorWorld::evidence` is: an
        /// `EventKind` variant must not drag every other variant's size
        /// up with it (`clippy::large_enum_variant`).
        reference: Box<EvidenceProjectionRef>,
    },
    /// Creates the Run's own record; distinct from `WaypointReserved`
    /// because one reservation (one World, one hash) can back several
    /// attempts (fold.md §2, R6; ruling 0044: no policy governs how
    /// many).
    RunOpened {
        run: RunId,
        waypoint: WaypointId,
        attempt: u32,
        world_hash: WorldHash,
    },
    /// "Run's actor launched" (BRIEF.md Intent); an activity signal
    /// issue 286 needs, distinct from `RunOpened` so a stalled launch is
    /// visible before any lifecycle event arrives (fold.md §2, R6).
    ///
    /// `actor_kind` (W1, 0041 D129): additive, `#[serde(default)]` so a
    /// `RunLaunched` written before this field existed still folds —
    /// `run_launched_without_kind_field_still_folds` pins it. Carries
    /// `--actor-kind` (chosen at `wirk run` time, after `RunOpened` has
    /// already been journaled at submit) onto the Run via `Run::apply`.
    /// Named `actor_kind`, not `kind`: `EventKind`'s own internal tag
    /// field is already named `kind` (`#[serde(tag = "kind")]` above),
    /// same reason `ClaimRecorded.claim_kind` isn't `kind` either.
    /// P3 native launch selection, D1 (BUILD-BRIEF.md "resolve and
    /// durably bind the requested launch **before** starting it"): the
    /// resolved launch request, journaled *before* `agent.start` — the
    /// one irreversible call in the launch — rather than after it.
    ///
    /// wirkd accepts at most one of these per Run and refuses every
    /// later one (`handle_record`), which is what makes admission
    /// atomic against the Run's actual journal under the daemon's own
    /// authority: two concurrent `wirk run` invocations for one Run
    /// serialize here, exactly one is admitted, and the other never
    /// reaches Herdr at all. Nothing about it depends on a git worktree
    /// lock or on Herdr's agent-name uniqueness, both of which are
    /// incidental guards this contract must not lean on.
    ///
    /// It is deliberately *not* proof that anything launched: it is
    /// admission of the request. `RunLaunched` — carrying the same
    /// `actor_kind`/`selection` plus Herdr's own argv — is the
    /// separate record of the launch actually returning. A Run with
    /// this event and no `RunLaunched` is a launch whose outcome this
    /// estate does not know (`Run.launch_requested`).
    RunLaunchRequested {
        run: RunId,
        #[serde(default)]
        actor_kind: ActorKind,
        #[serde(default)]
        selection: ActorSelection,
    },
    /// P3 native launch *attempt* admission (the independent review's
    /// N1): admission of the request is not admission of the attempt.
    /// Once `RunLaunchRequested` is bound, every later invocation —
    /// a plain duplicate, or a recovery after a daemon or client loss
    /// — used to walk straight into `agent.start` with nothing but
    /// Herdr's own agent-name uniqueness between them. This event is
    /// the second admission, taken under the same journal lock and the
    /// same daemon authority as the first: one invocation at a time
    /// owns the launch *and* the drive loop of a Run.
    ///
    /// `holder` is server-minted, exactly as `RunFailed.cause.at` is;
    /// whatever a client sends is discarded. `destination` is the
    /// client's own Herdr socket, bound at the first attempt and
    /// required to match on every later one.
    ///
    /// The attempt is released by nothing: it *expires* when its
    /// holder process is no longer alive, which `wirkd` checks against
    /// the kernel when a new attempt asks. There is deliberately no
    /// marker to leave behind and no lease to forget to release, so a
    /// crashed holder can never trap a valid Run, and a live one can
    /// never be silently replaced.
    RunLaunchAttempted {
        run: RunId,
        #[serde(default)]
        destination: String,
        #[serde(default)]
        holder: AttemptHolder,
    },
    RunLaunched {
        run: RunId,
        #[serde(default)]
        actor_kind: ActorKind,
        /// P3 native launch selection (BUILD-BRIEF.md item 3): the
        /// resolved request — CLI-explicit, Route-authored, or the
        /// harness's own native default, precedence applied before this
        /// event was ever built. `#[serde(default)]` so a `RunLaunched`
        /// written before this field existed still folds, to the empty
        /// selection — which means *this record carries no selection*,
        /// never "this Run launched bare". The estate's own pre-field
        /// launch path passed `--model sonnet` plus a `--settings
        /// <estate root>/…` pair for every claude Run and
        /// `--model hecate/…` for opencode; those launches had
        /// arguments and the journal never recorded them. Absent is
        /// unrecorded (the W-B launch review's F-D).
        #[serde(default)]
        selection: ActorSelection,
        /// Herdr's own `agent_started.argv` for this launch — submission
        /// evidence, never provider attestation (PREPARATION-ADJUDICATION.md
        /// point 3). `#[serde(default)]`, same reason as `selection`.
        #[serde(default)]
        launch_argv: Vec<String>,
    },
    /// Terminal Work failure is never inferred from `RunFailed`
    /// (incident file); an explicit event keeps `fold` retry-policy-
    /// agnostic — the component that decides a Run's fate (items 4, 5)
    /// fires this (fold.md §2, §8; 0027 D92). J1: no ruling pins
    /// Work-level failure firing yet.
    WorkFailed {
        cause: FailureCause,
    },
    /// Owner/operator cancellation; no existing event carries this verb
    /// (fold.md §2, R6).
    ///
    /// `caused_by` (W-A, §3.4): `Some(parent)` when this cancellation is
    /// a cascade step fired by canceling `parent` with `--cascade`;
    /// `None` for the explicitly named target of the verb itself.
    /// `#[serde(default)]` so a `WorkCanceled` written before this field
    /// existed still folds.
    WorkCanceled {
        reason: Option<String>,
        #[serde(default)]
        caused_by: Option<WorkId>,
    },
    /// W-A (§3.1): explicit journaled identity for one container
    /// occurrence, even though a container has no execution Run
    /// (BUILD-AMENDMENTS.md: "name it and journal it, rather than
    /// relying on the phrase 'current attempt set' without a
    /// reducer"). `attempt` starts at 1; W-A correction (F1/F2):
    /// reopening a previously closed nested container during
    /// held-work recovery journals a new attempt here, while a merely
    /// held ancestor keeps its current activation unchanged. Retrying
    /// while the Work is `Active` on a later waypoint is refused
    /// today.
    ContainerActivated {
        waypoint: WaypointId,
        attempt: u32,
    },
    /// W-A (§3.2): a container's outcome contract is not yet satisfied
    /// — `missing` names each unmet declared-output name or
    /// `"child role <role>"`/`"container <id> not closed"` entry.
    /// Server-minted only (`handle_record` refuses it like `RunOpened`).
    StageHeld {
        waypoint: WaypointId,
        /// The container activation this hold belongs to (W-A
        /// correction, F1/F2).
        #[serde(default = "first_attempt")]
        attempt: u32,
        missing: Vec<String>,
    },
    /// W-A (§3.2): a container's outcome contract is satisfied, with
    /// the exact receipts that satisfied it. Server-minted only.
    StageClosed {
        waypoint: WaypointId,
        /// The container activation these receipts close (W-A
        /// correction, F1/F2): a `StageClosed` from a superseded
        /// generation never credits the current one.
        #[serde(default = "first_attempt")]
        attempt: u32,
        receipts: Vec<OutcomeReceipt>,
    },
    /// W-A (§3.3): journaled on the **parent's** journal when it admits
    /// a child Work submission for one of its containers' declared
    /// roles. Server-minted only, written before the child's own
    /// journal is created (§3.3: "the reverse order was rejected
    /// because it could produce a creditable child the parent never
    /// recorded").
    ChildWorkSpawned {
        role: String,
        child: WorkId,
        waypoint: WaypointId,
        /// The container activation the child serves (W-A correction,
        /// F4) — the parent's half of the two-sided binding, checked
        /// against the child's own recorded `parent` at credit time.
        #[serde(default = "first_attempt")]
        attempt: u32,
        run: RunId,
    },
    /// W-B (§5.2): an actor's own bounded claim, with its evidence
    /// admitted and frozen at raise time (`AdmittedEvidence`, never
    /// re-resolved later). Its own verb (`wirk finding raise`),
    /// triple-checked like `claim` — `Event.run` names the raising Run.
    /// Also joins `handle_record`'s Forbidden arm so a raw `record` can
    /// never mint one directly, the same defense-in-depth every other
    /// server-owned transition already has.
    FindingRaised {
        finding: Finding,
    },
    /// W-B (§2.4): wirkd's own sole producer, minted only when an
    /// admitted policy class's check holds against a journal fact this
    /// daemon derived itself — never a wire field, never client-minted.
    /// `Event.run = None`.
    FindingSettled {
        finding: FindingId,
        settlement: Settlement,
    },
    /// W-B (§2.5): an honestly unverified human/client assertion —
    /// never sets a finding `Settled`, never suppresses it from later
    /// consultation. `Event.run = None`; the operator verb (`wirk
    /// finding assert`) carries no execution triple.
    FindingAsserted {
        finding: FindingId,
        assertion: Assertion,
    },
    /// W-B (§4): the owning source's mechanical before/after change,
    /// with the judgement that it implements the finding kept separate
    /// and always asserted, never derived. `Event.run = None`.
    FindingApplied {
        finding: FindingId,
        application: ApplicationRef,
    },
}

/// D9#1: replay rebuilds Work state, no in-memory objects. The
/// Work-level reducer over `fold.md` §1's transition table: strictly
/// slice order, single-writer/append-only so slice order *is* journal
/// order by construction (fold.md §3) — `fold` consults nothing but
/// `events` (BRIEF.md's decisive check).
///
/// A `Work` exists only from its `WorkSubmitted` event onward; nothing
/// before it is folded (no oracle for a Work that hasn't been
/// submitted). `WaypointReserved`'s `world`/`world_hash` are not kept
/// on `Work` today — folded for their state/`current_waypoint` effect
/// only (fold.md §1's `WaypointReserved` row); a resumed Run's use of
/// the journaled World is item 4's, not this reducer's.
///
/// Unknown Run: any event naming a `run` this Work has no `RunOpened`
/// for is ignored entirely — never panics, never changes state — same
/// policy as `Run::apply` (`lib.rs` above, line ~404-407; fold.md §3,
/// §7, R6).
pub fn fold(events: &[Event]) -> Work {
    let mut work: Option<Work> = None;
    let mut route_waypoints: Vec<WaypointId> = Vec::new();
    let mut waypoint_defs: Vec<WaypointDefinition> = Vec::new();
    let mut run_waypoints: BTreeMap<RunId, WaypointId> = BTreeMap::new();

    for event in events {
        let is_work_submitted = matches!(event.kind, EventKind::WorkSubmitted { .. });
        let is_run_opened = matches!(event.kind, EventKind::RunOpened { .. });
        if !is_work_submitted
            && let Some(run) = &event.run
            && !is_run_opened
            && !run_waypoints.contains_key(run)
        {
            // Unknown Run: the whole event is ignored (fold.md §3).
            continue;
        }

        let Some(w) = work.as_mut() else {
            if let EventKind::WorkSubmitted {
                route,
                repositories,
                intent,
                waypoints,
                waypoint_defs: defs,
                parent,
                execution_repo,
                execution_identity,
            } = &event.kind
            {
                route_waypoints = waypoints.clone();
                waypoint_defs = defs.clone();
                work = Some(Work {
                    id: event.work.clone(),
                    intent: intent.clone(),
                    route: route.clone(),
                    repositories: repositories.clone(),
                    state: WorkState::Pending,
                    current_waypoint: None,
                    last_activity: event.at,
                    needs_input: None,
                    parent: parent.clone(),
                    held: None,
                    activations: Vec::new(),
                    execution_repo: execution_repo.clone(),
                    execution_identity: execution_identity.clone(),
                    findings: BTreeMap::new(),
                    settlement_ready: Vec::new(),
                });
            }
            // No `Work` exists yet and this isn't `WorkSubmitted`: there
            // is nothing to fold onto (fold.md's table has no row for
            // "before submission"; R1).
            continue;
        };

        w.last_activity = event.at;

        match &event.kind {
            EventKind::WorkSubmitted { .. } => {
                // Precondition "no prior event for this work" (fold.md
                // §1): a second WorkSubmitted for an already-existing
                // Work does not reset it.
            }
            EventKind::WaypointReserved { waypoint, .. } => {
                if !w.state.is_terminal() {
                    w.current_waypoint = Some(waypoint.clone());
                    if matches!(w.state, WorkState::Pending | WorkState::Waiting) {
                        w.state = WorkState::Active;
                        // W-A (§3.2 amendment): a fresh reservation for
                        // a leaf under a held container (the usable
                        // retry path) clears the stale hold — the
                        // leaf's own later Claim re-evaluates the
                        // container from scratch.
                        w.held = None;
                    }
                }
            }
            EventKind::RunOpened { run, waypoint, .. } => {
                run_waypoints.insert(run.clone(), waypoint.clone());
                // P2.3 W2 (decide.md §1, build-brief.md §7): a retry's
                // own `RunOpened` clears `NeedsInput` back to `Active`
                // on the same reserved World — the human's decision is
                // this one event, no count invented (0044 D134).
                // Auto-advance's own `RunOpened` (handle_claim) only
                // ever fires from a Validated Done claim, which never
                // reaches this arm while `w.state == NeedsInput` (a
                // Question/RunFailed/RunVanished already moved the Work
                // there and a Done claim on that Run cannot validate
                // afterward), so this guard is a no-op on that path,
                // never a special case for it.
                if w.state == WorkState::NeedsInput {
                    w.state = WorkState::Active;
                    w.needs_input = None;
                }
                // W-A (§3.2 amendment): a retry on a leaf under a held
                // container always writes a fresh `WaypointReserved`
                // before this `RunOpened` (`handle_retry`, mirroring
                // every other writer), so that event's own arm above
                // already cleared `Waiting`/`held` — nothing left to do
                // here for that case.
            }
            // Neither the request nor the launch moves Work-level
            // state: both are Run-scoped facts `Run::apply` folds.
            EventKind::RunLaunched { .. }
            | EventKind::RunLaunchRequested { .. }
            | EventKind::RunLaunchAttempted { .. } => {}
            // D9#2: inert at Run level (0001 D9 #2; 0017 D56) and, for
            // every status but the two named below, equally inert here
            // — a lifecycle event alone does not otherwise advance or
            // change WorkState (fold.md §6, rejecting sergeant's
            // KIND_WORK_COMPLETED jump table).
            //
            // Ruling 0052 D156 (P2.6 W2): `Blocked` is an actor waiting
            // on a human, not a failed Run — the Work surfaces the same
            // way `RunFailed`/`RunVanished` do (guarded by
            // `is_terminal()`, same as every other non-terminal arm),
            // reason `"blocked"`, `detail` the observation the loop
            // journaled (the pane and its last screen lines). `Working`
            // clears it back to `Active` — but *only* when the Work is
            // `NeedsInput` for `"blocked"` specifically: a `Working`
            // observed while `NeedsInput` for a different reason (a
            // filed Question, a different Run's failure) must not
            // clobber that human decision with an unrelated pane's
            // lifecycle event.
            EventKind::LifecycleObserved { status, detail } => match status.as_str() {
                "Blocked" => {
                    if !w.state.is_terminal() {
                        w.state = WorkState::NeedsInput;
                        w.needs_input = Some(NeedsInputCause {
                            run: event
                                .run
                                .clone()
                                .expect("LifecycleObserved always names a run"),
                            reason: "blocked".into(),
                            detail: detail.clone().unwrap_or_default(),
                        });
                    }
                }
                // Ruling 0113 (P3 native usability, the operator's own
                // work-18d32a2752f0ca8a-0): the pane whose permission
                // prompt a human has just answered is `Idle`, not
                // `Working` — the actor answered and is waiting to be
                // told what to do, and nothing will make it `Working`
                // again until its driver prompts it. Requiring
                // `Working` to clear a block therefore waited for a
                // transition that could not happen until the very
                // continuation the block was holding up. `Done` is the
                // same turn end under Herdr's own name for a pane
                // nothing has viewed since (`turn_ended`,
                // `wirk-herdr/src/run_loop.rs`), which is every pane
                // `wirk run` drives. `Unknown` is *not* here: it is
                // Herdr declining to say what the pane is doing, which
                // is the absence of an observation, never a resolution.
                //
                // ...and only the blocked Run's own lifecycle clears
                // it (`cause.run`). An older Run's pane, still alive in
                // the session and still reporting, says nothing about
                // the pane a human is actually looking at; before this
                // wave any Run's `Working` cleared any other Run's
                // block. The `reason == "blocked"` guard is unchanged:
                // a filed Question or a Run failure is a human decision
                // no lifecycle event may clobber.
                "Working" | "Idle" | "Done"
                    if w.state == WorkState::NeedsInput
                        && w.needs_input.as_ref().is_some_and(|cause| {
                            cause.reason == "blocked" && Some(&cause.run) == event.run.as_ref()
                        }) =>
                {
                    w.state = WorkState::Active;
                    w.needs_input = None;
                }
                _ => {}
            },
            EventKind::ClaimFiled { .. } => {}
            EventKind::ClaimRecorded {
                claim_kind,
                verdict,
                ..
            } => {
                let claimed_waypoint = event.run.as_ref().and_then(|run| run_waypoints.get(run));
                match (verdict, claim_kind) {
                    (ClaimVerdict::Validated, ClaimKind::Done) => {
                        let is_last_waypoint =
                            claimed_waypoint.is_some_and(|wp| route_waypoints.last() == Some(wp));
                        // W-A (§3.2): a leaf nested under a container
                        // never completes the Work by itself, however
                        // last it is in the flattened sequence — the
                        // Work waits for that container's own
                        // `StageClosed` (below), which is either
                        // journaled in the same server call (closure
                        // succeeded) or preceded by a `StageHeld`
                        // (closure did not). A top-level leaf (no
                        // container ancestor — every leaf on an old flat
                        // Route) keeps the original rule exactly
                        // (`old_flat_route_journal_folds_identically`).
                        let has_container_ancestor = claimed_waypoint
                            .is_some_and(|wp| !ancestor_chain(&waypoint_defs, wp).is_empty());
                        if is_last_waypoint && !has_container_ancestor {
                            w.state = WorkState::Completed;
                        } else if !w.state.is_terminal() {
                            // current_waypoint unchanged until the next
                            // WaypointReserved (fold.md §1).
                            w.state = WorkState::Active;
                        }
                    }
                    (ClaimVerdict::Validated, ClaimKind::Question(reason)) => {
                        if !w.state.is_terminal() {
                            w.state = WorkState::NeedsInput;
                            w.needs_input = Some(NeedsInputCause {
                                run: event
                                    .run
                                    .clone()
                                    .expect("a validated Claim always names a run"),
                                reason: "question".into(),
                                detail: reason.clone(),
                            });
                        }
                    }
                    // P2.4 W2 (build-brief.md §3 W2; refuse.md §1):
                    // `OutOfBoundary` is the one refusal a Work cannot
                    // route around by re-filing the same Claim
                    // correctly — the actor touched the wrong path, a
                    // human decides what happens next. Same shape as
                    // `RunFailed`/`RunVanished` above, guarded the same
                    // way so an already-terminal Work is left alone.
                    (ClaimVerdict::Refused(ClaimRefusal::OutOfBoundary(what)), _) => {
                        if !w.state.is_terminal() {
                            w.state = WorkState::NeedsInput;
                            w.needs_input = Some(NeedsInputCause {
                                run: event
                                    .run
                                    .clone()
                                    .expect("a refused Claim always names a run"),
                                reason: "out_of_boundary".into(),
                                detail: what.clone(),
                            });
                        }
                    }
                    // Every other refusal kind is not a Work fact,
                    // actor-fixable by re-filing the same Claim
                    // correctly, mirrors Run::apply line ~443 (fold.md
                    // §1).
                    (ClaimVerdict::Refused(_), _) => {}
                }
            }
            // P2.3 W1 (0033 D102; 0044; states.md §1): superseded — a
            // failed Run now surfaces the Work as `NeedsInput` with its
            // cause, guarded like every other non-terminal transition
            // so a Work already `Failed`/`Completed`/`Canceled` is left
            // alone by a later `RunFailed`/`RunVanished`.
            EventKind::RunFailed { cause } => {
                if !w.state.is_terminal() {
                    w.state = WorkState::NeedsInput;
                    w.needs_input = Some(NeedsInputCause {
                        run: event.run.clone().expect("RunFailed always names a run"),
                        reason: "run_failed".into(),
                        detail: cause.detail.clone().unwrap_or_default(),
                    });
                }
            }
            EventKind::RunVanished => {
                if !w.state.is_terminal() {
                    w.state = WorkState::NeedsInput;
                    w.needs_input = Some(NeedsInputCause {
                        run: event.run.clone().expect("RunVanished always names a run"),
                        reason: "run_vanished".into(),
                        detail: "the actor's pane ended (Herdr stream EOF)".into(),
                    });
                }
            }
            EventKind::WorktreeCreated { .. } => {}
            EventKind::WorkFailed { .. } => {
                w.state = WorkState::Failed;
            }
            EventKind::WorkCanceled { .. } => {
                w.state = WorkState::Canceled;
            }
            // W-A (§3.1): no *state* effect, but the activation itself
            // is folded (W-A correction, F1/F2): `activations` carries
            // each container's current generation, and a container
            // reopened while it was held drops the stale hold — the
            // requirement belongs to the superseded generation, and the
            // reservation that follows is what moves the Work on.
            EventKind::ContainerActivated { waypoint, attempt } => {
                match w
                    .activations
                    .iter_mut()
                    .find(|entry| &entry.waypoint == waypoint)
                {
                    Some(entry) => entry.attempt = *attempt,
                    None => w.activations.push(ContainerActivation {
                        waypoint: waypoint.clone(),
                        attempt: *attempt,
                    }),
                }
                if w.held
                    .as_ref()
                    .is_some_and(|held| &held.waypoint == waypoint && held.attempt < *attempt)
                {
                    w.held = None;
                }
            }
            EventKind::StageHeld {
                waypoint,
                attempt,
                missing,
            } => {
                if !w.state.is_terminal() {
                    w.state = WorkState::Waiting;
                    w.current_waypoint = Some(waypoint.clone());
                    w.held = Some(HeldInfo {
                        waypoint: waypoint.clone(),
                        attempt: *attempt,
                        missing: missing.clone(),
                    });
                }
            }
            EventKind::StageClosed {
                waypoint, attempt, ..
            } => {
                if !w.state.is_terminal() {
                    // W-A correction (F1/F2): a `StageClosed` clears the
                    // hold only for the generation it closed — a
                    // replayed close from a superseded attempt leaves a
                    // newer hold standing.
                    if w.held
                        .as_ref()
                        .is_some_and(|held| &held.waypoint == waypoint && held.attempt == *attempt)
                    {
                        w.held = None;
                    }
                    // A closing container completes the Work only when
                    // it is itself a top-level Route element (no
                    // container ancestor of its own) and the last one —
                    // exactly the condition a top-level leaf's own
                    // "is_last_waypoint" check answers above; a nested
                    // sub-container's own `StageClosed` (a grandchild
                    // case) only clears `held` here, the cascade to its
                    // outer container is the server's own next journal
                    // line in the same call.
                    let is_top_level_last = ancestor_chain(&waypoint_defs, waypoint).is_empty()
                        && waypoint_defs.last().map(|d| &d.id) == Some(waypoint);
                    if is_top_level_last {
                        w.state = WorkState::Completed;
                    } else {
                        w.state = WorkState::Active;
                    }
                }
            }
            EventKind::ChildWorkSpawned { .. } => {}
            // W-B (§5.2): a Finding is visible to this Work's own later
            // Worlds the instant it is raised — no settlement required
            // for WorkLocal use (fold.md's own "informed immediately"
            // rule, carried here rather than re-derived by every
            // reader). `settlement_ready` is recomputed fresh against
            // the *whole* `events` slice already in hand (order
            // independent of where the qualifying event sits — the
            // terminal design's own ordering bug, fixed by never relying
            // on iteration order to "arrive after" the trigger).
            EventKind::FindingRaised { finding } => {
                w.findings.insert(
                    finding.id.clone(),
                    FindingRecord {
                        finding: finding.clone(),
                        state: FindingState::Proposed,
                        assertions: Vec::new(),
                        applied: Vec::new(),
                    },
                );
                if let Some(ready) =
                    deterministic_verified_readiness(events, &waypoint_defs, finding)
                {
                    w.settlement_ready.push(ready);
                }
                if let Some(superseded) = &finding.supersedes {
                    w.settlement_ready.push(ReadySettlement {
                        finding: superseded.clone(),
                        class: SettlementClass::SupersededInOrigin,
                        check: SettlementCheck::SupersededBy {
                            work: finding.work.clone(),
                            finding: finding.id.clone(),
                            raise_event: event.id.clone(),
                        },
                    });
                }
            }
            // W-B (§2.4, §6): wirkd's own sole producer; folded as the
            // record's terminal state. A `FindingSettled` naming a
            // finding this journal never raised is ignored (fail closed,
            // R6: the same "no oracle for a fact this Work never
            // recorded" rule `ClaimRecorded`'s own `claimed_waypoint`
            // lookup already applies).
            EventKind::FindingSettled {
                finding,
                settlement,
            } => {
                if let Some(record) = w.findings.get_mut(finding) {
                    record.state = FindingState::Settled(Box::new(settlement.clone()));
                }
            }
            // W-B (§2.5): never sets `Settled`, never suppresses —
            // appended to the record's own assertion history.
            EventKind::FindingAsserted { finding, assertion } => {
                if let Some(record) = w.findings.get_mut(finding) {
                    record.assertions.push(assertion.clone());
                }
            }
            // W-B (§4): the exact before/after record; `Settled` is
            // never implied and never rewritten by a later Application.
            // W-B-CORRECT.md defect 3: appended, never replaced — a
            // historical Application survives a later generation's own
            // Application of the same Finding.
            EventKind::FindingApplied {
                finding,
                application,
            } => {
                if let Some(record) = w.findings.get_mut(finding) {
                    record.applied.push(application.clone());
                }
            }
            // W-C3: a projection revision is a fact about one Run's
            // delivered context, not about the Work's state. It folds on
            // `Run` (`Run::apply`) and is deliberately inert here: no
            // Work state, no current Waypoint, no leaf decision moves
            // because a stage asked its own context a second question.
            EventKind::ProjectionExpanded { .. } => {}
        }

        // Current-vs-historical contract (loop-a-reverify
        // 22-rvF1-legitimate-completion.log, and a second, independently
        // reproduced `out_of_boundary`-then-`Canceled` report): every arm
        // above that sets `w.needs_input` also sets `w.state` to
        // `NeedsInput` in the same branch, so `state != NeedsInput` is
        // never true immediately after one of those arms runs. Any other
        // arm moving `state` away from `NeedsInput` — a late Claim's
        // completion, a container's `StageClosed`, an explicit
        // `WorkFailed`/`WorkCanceled` — therefore always means the old
        // cause is resolved, not current, and clearing it here can never
        // erase a genuinely still-open one.
        if w.state != WorkState::NeedsInput {
            w.needs_input = None;
        }
    }

    let mut work =
        work.expect("fold called with no WorkSubmitted event in the slice: no Work to build");
    // W-B (§6): only one still-`Proposed` finding's readiness survives —
    // a finding `FindingSettled` already resolved (folded above, in
    // either order) is done, and a readiness fact for it is stale, not
    // a second candidate for wirkd to re-mint against.
    let mut seen: std::collections::HashSet<FindingId> = std::collections::HashSet::new();
    work.settlement_ready.retain(|ready| {
        matches!(
            work.findings
                .get(&ready.finding)
                .map(|record| &record.state),
            Some(FindingState::Proposed)
        ) && seen.insert(ready.finding.clone())
    });
    work
}

/// The Waypoint `run` was opened against, from `events` alone — a
/// whole-slice lookup (W-B §6, order independence), not the incremental
/// `run_waypoints` accumulator `fold`'s own loop builds only up to its
/// current position. Returns the whole opening fact (waypoint, attempt,
/// the World hash the attempt was opened against), because the
/// obligation proof needs all three and there is no honest way to ask
/// for one of them alone.
fn run_opening(events: &[Event], run: &RunId) -> Option<(WaypointId, u32, WorldHash)> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::RunOpened {
            run: id,
            waypoint,
            attempt,
            world_hash,
        } if id == run => Some((waypoint.clone(), *attempt, world_hash.clone())),
        _ => None,
    })
}

/// The World hash currently reserved for `waypoint` — the last
/// `WaypointReserved` naming it. A Run opened against an older
/// reservation (a re-reserved Waypoint whose World changed) is a
/// superseded activation: its Claim proves what it proved then, never
/// what the Waypoint's current activation obliges now (construction
/// review: "do not settle a superseded activation").
fn latest_reserved_world_hash<'a>(
    events: &'a [Event],
    waypoint: &WaypointId,
) -> Option<&'a WorldHash> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::WaypointReserved {
            waypoint: id,
            world_hash,
            ..
        } if id == waypoint => Some(world_hash),
        _ => None,
    })
}

/// The most recently opened Run for `waypoint` — the last `RunOpened`
/// naming it, walked in reverse so a retried Waypoint's latest attempt
/// wins. The pure-`wirk-core` mirror of `wirk/src/wirkd/server.rs::
/// latest_run_for_waypoint` (that helper lives in `wirkd` because it also
/// returns `attempt`/`world_hash` wirkd's own status replies need; this
/// one only ever answers "is `run` still current," which `fold`'s own
/// pure readiness checks can ask without crossing the crate boundary).
fn latest_run_opened_for_waypoint<'a>(
    events: &'a [Event],
    waypoint: &WaypointId,
) -> Option<&'a RunId> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::RunOpened {
            run, waypoint: w, ..
        } if w == waypoint => Some(run),
        _ => None,
    })
}

/// W-B (§5.3, `deterministic-verified`), rebuilt on the obligation
/// contract `W-B-OBLIGATION-BUILD.md` requires.
///
/// The defect this replaces, reproduced through the real service on
/// `0634657` before a line changed here
/// (`loop-b-obligation-build/raw/00-counterexample-frozen-base.txt`): a
/// Finding whose sentence was *"wirk has no remote code execution
/// vulnerability and its full security audit passed with zero findings"*
/// settled `deterministic_verified` — and reached the estate index — on
/// the strength of an earlier leaf running `sh -c "echo one > out1.md"`.
/// Every guard the prior correction added was satisfied: the cited Claim
/// was real, Validated, `Done`, on a `Deterministic` leaf, at a Route
/// position the Finding's own Waypoint had already passed, on that
/// Waypoint's current Run. **None of that is a proof of the sentence**,
/// and Route position is not a proof of anything: it says which
/// obligations *could* have been discharged, never which one *was*.
///
/// What is bound instead, all of it re-derived here from the journal:
///
/// 1. The Finding **names** an obligation (`Finding.obligation`). A
///    `VerifiedOutcome` Finding naming none is never ready. Naming is
///    necessary and never sufficient — the remaining five are why.
/// 2. The Waypoint whose Claim is cited **declares that same
///    obligation** on its own Route definition (`WaypointDefinition.
///    verifies`), by id *and* edition. A Claim of a Waypoint that
///    declares a different check, a different edition, or no obligation
///    at all proves nothing here, however successful it was.
/// 3. That Waypoint is `Deterministic` and its Run is the **current**
///    one for it, opened against the **currently reserved World** — a
///    superseded attempt, or an attempt opened against a World the
///    Waypoint has since re-reserved, is a stale activation.
/// 4. The cited event is that Run's own Validated `Done` `ClaimRecorded`
///    — the receipt, not a neighbouring success.
/// 5. Every `outputs` name the obligation declares appears in that
///    Claim's own `ArtifactReceipt` set **with a recorded digest**: the
///    obligated outcome was actually produced and its content identity
///    was actually read (an unrecorded, pre-correction receipt is not
///    an evidence basis).
/// 6. The whole authored obligation plus that exact execution basis
///    content-address to `basis` (`obligation_basis`) — which
///    `wirk/src/wirkd/server.rs::try_mint_settlement` then requires the
///    estate's own settlement policy to have admitted. That is the step
///    a proposer cannot forge by authoring: changing the command, the
///    source basis, the expected artifacts, the proven statement or the
///    obligated outputs all change `basis`, and an unadmitted basis
///    settles nothing.
///
/// The Route-position rule the prior correction introduced is kept as a
/// further narrowing (a Finding still cannot cite a Waypoint its own
/// Route has not reached), not as the proof: it is now one of six
/// necessary conditions rather than the whole binding.
///
/// Whole-slice lookup by `EventId` (§6): the referenced Claim may sit
/// anywhere in `events` relative to this `FindingRaised`, before or
/// after, so this never depends on fold's own iteration order.
fn deterministic_verified_readiness(
    events: &[Event],
    waypoint_defs: &[WaypointDefinition],
    finding: &Finding,
) -> Option<ReadySettlement> {
    if finding.kind != FindingKind::VerifiedOutcome {
        return None;
    }
    // (1) The Finding must name the obligation it claims to discharge.
    let named = finding.obligation.as_ref()?;
    let sequence = flatten_leaves(waypoint_defs);
    let finding_position = sequence.iter().position(|id| id == &finding.waypoint)?;
    for item in &finding.evidence {
        let EvidenceRef::Journal {
            work,
            event: event_id,
        } = &item.reference
        else {
            continue;
        };
        if work != &finding.work {
            continue;
        }
        let Some(claim_event) = events.iter().find(|event| &event.id == event_id) else {
            continue;
        };
        // (4) The cited event is a Validated `Done` Claim receipt.
        let EventKind::ClaimRecorded {
            claim,
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
            artifacts,
        } = &claim_event.kind
        else {
            continue;
        };
        let Some(run_id) = &claim_event.run else {
            continue;
        };
        let Some((waypoint, attempt, world_hash)) = run_opening(events, run_id) else {
            continue;
        };
        let Some(claim_position) = sequence.iter().position(|id| id == &waypoint) else {
            continue;
        };
        if claim_position > finding_position {
            continue;
        }
        // (3) Current activation, on both axes: the current Run for the
        // Waypoint, opened against the currently reserved World.
        if latest_run_opened_for_waypoint(events, &waypoint) != Some(run_id) {
            continue;
        }
        if latest_reserved_world_hash(events, &waypoint)
            .is_some_and(|current| current != &world_hash)
        {
            continue;
        }
        let Some(def) = find_definition(waypoint_defs, &waypoint) else {
            continue;
        };
        if def.kind != WaypointKind::Deterministic {
            continue;
        }
        // (2) That Waypoint declares exactly the named obligation.
        let Some(obligation) = def.verifies.as_ref() else {
            continue;
        };
        if obligation.id != named.id || obligation.edition != named.edition {
            continue;
        }
        // (5) The obligated outputs are in the receipt, with real
        // recorded content identity.
        if !obligation.outputs.iter().all(|name| {
            artifacts
                .iter()
                .any(|receipt| &receipt.name == name && !receipt.digest.is_empty())
        }) {
            continue;
        }
        // (6) The content basis the estate policy must have admitted.
        let Some(basis) = obligation_basis(def, Some(&world_hash)) else {
            continue;
        };
        return Some(ReadySettlement {
            finding: finding.id.clone(),
            class: SettlementClass::DeterministicVerified,
            check: SettlementCheck::ValidatedClaim {
                work: work.clone(),
                claim: claim.clone(),
                claim_event: event_id.clone(),
                proof: Some(DeterministicProof {
                    obligation: ObligationRef {
                        id: obligation.id.clone(),
                        edition: obligation.edition.clone(),
                    },
                    basis,
                    proves: obligation.proves.clone(),
                    waypoint,
                    attempt,
                    world_hash,
                    artifacts: artifacts.clone(),
                }),
                unread: UnreadFields::default(),
            },
        });
    }
    None
}

// ---- Journal ------------------------------------------------------------
//
// Item 2's store (orient/store.md). Append-only NDJSON, one
// `{"seq": N, "event": {...}}` envelope per line, `serde_json` (§1, R5).
// `Journal`/`JournalError` live beside `Event`/`fold`: pure file/format
// logic, no Herdr or process concept, same crate and deny-list boundary
// (0022 D71; store.md §2, §6).

/// On-disk envelope: the sequence number is a store-only field, never on
/// `Event` itself — `fold` consults nothing but the events it is handed
/// (store.md §2, "sequence is not added to `Event`").
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    seq: u64,
    event: Event,
}

/// Owns the one write path for a Work's journal (Intent, BRIEF.md:17-18:
/// "Every append goes through one write path that keeps any derived
/// state contiguous with the journal"). No other way to add a line to
/// the file this owns (store.md §2).
pub struct Journal {
    file: File,
    next_seq: u64,
}

impl Journal {
    /// Opens the journal inside `dir`, creating `dir` and
    /// `dir/journal.ndjson` if either is absent (Outcome: "the directory
    /// created on open"). Scans any existing lines once to recover
    /// `next_seq` for the next `append` — fails closed on the same
    /// malformed-line/seq-gap rule `replay`/`iter` apply (§5): a
    /// corrupted journal never opens silently as if it were empty.
    pub fn open(dir: impl AsRef<Path>) -> Result<Journal, JournalError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let path = dir.join("journal.ndjson");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let mut journal = Journal { file, next_seq: 1 };
        for envelope in journal.envelopes()? {
            let (seq, _event) = envelope?;
            journal.next_seq = seq + 1;
        }
        Ok(journal)
    }

    /// Appends one `Event`: builds the `{"seq", "event"}` envelope,
    /// mints a ULID into `event.id` when the caller left it empty
    /// (store.md §2, §3 — `EventId` is minted here, the convention
    /// `WorkId`/`RunId`/`ClaimId` already document), serializes it to a
    /// single line, one `write_all`, then `fsync`s before returning —
    /// every acknowledged append is durable (R1: no batching for P1).
    pub fn append(&mut self, event: &Event) -> Result<Event, JournalError> {
        let mut event = event.clone();
        if event.id.0.is_empty() {
            event.id = EventId(ulid::Ulid::generate().to_string());
        }
        let seq = self.next_seq;
        let envelope = Envelope { seq, event };
        let mut line = serde_json::to_vec(&envelope).map_err(|source| JournalError::Serialize {
            index: seq as usize,
            source,
        })?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_all()?;
        self.next_seq = seq + 1;
        Ok(envelope.event)
    }

    /// Rebuilds `Vec<Event>` from seq 1; `fold(&journal.replay()?)` is
    /// the only derived-Work path P1 has (store.md §4: no cache).
    pub fn replay(&self) -> Result<Vec<Event>, JournalError> {
        self.iter()?.collect()
    }

    /// Streaming form; fails closed the same way as `replay` — a bad
    /// line stops the iterator with `Err`, never silently skipped (§5).
    pub fn iter(&self) -> Result<JournalIter, JournalError> {
        Ok(JournalIter {
            inner: self.envelopes()?,
        })
    }

    /// Reads the file from its start through a fresh seek on a cloned
    /// handle (`append`'s handle keeps writing at EOF regardless of the
    /// read cursor, per `O_APPEND` semantics) — one on-disk file, one
    /// write path, read from wherever this call needs.
    fn envelopes(&self) -> Result<EnvelopeIter, JournalError> {
        envelopes(&self.file)
    }
}

/// The read-only half of the same journal, for a caller that only ever
/// reads one: it opens `dir/journal.ndjson` for reading and **nothing
/// else** — no `create_dir_all`, no `create(true)`, no append mode.
///
/// `Journal::open` is the estate's one *write* path and creates whatever
/// is missing on the way in, which is right for the path that is about
/// to append and wrong for a pure scan. A scanner routed through it had
/// two executed consequences (`index-health-reverify/VERDICT.md`,
/// "`Journal::open` on the read path"): a `works/` entry with no journal
/// got a **zero-byte `journal.ndjson` invented by the scan itself**, and
/// a journal that was perfectly readable but not writable (`0444`: a
/// restored backup, an archived tree, a `chmod -R a-w` snapshot) failed
/// the scan with `Permission denied`, so such an estate could never
/// report a healthy derived index and could never be rebuilt.
///
/// This is the same on-disk format and the same replay, not a second
/// one: `replay`/`iter` share `EnvelopeIter` with `Journal` verbatim and
/// fail closed on the identical malformed-line/seq-gap rule (§5), so a
/// torn tail is still an error here and never a short, silent row set.
/// It cannot append, which is the point — the write path stays exactly
/// one.
pub struct JournalReader {
    file: File,
}

impl JournalReader {
    /// Opens `dir/journal.ndjson` read-only. A missing directory or a
    /// missing journal is `io::ErrorKind::NotFound` for the caller to
    /// interpret against the estate's own layout; nothing is created
    /// either way.
    pub fn open(dir: impl AsRef<Path>) -> Result<JournalReader, JournalError> {
        let file = OpenOptions::new()
            .read(true)
            .open(dir.as_ref().join("journal.ndjson"))?;
        Ok(JournalReader { file })
    }

    /// `Journal::replay`, from a handle that cannot write.
    pub fn replay(&self) -> Result<Vec<Event>, JournalError> {
        self.iter()?.collect()
    }

    /// `Journal::iter`, from a handle that cannot write.
    pub fn iter(&self) -> Result<JournalIter, JournalError> {
        Ok(JournalIter {
            inner: envelopes(&self.file)?,
        })
    }
}

/// One reader over one journal file, from its start, on a cloned handle.
/// Shared by `Journal` and `JournalReader` so there is exactly one
/// parser for the format however the file was opened.
fn envelopes(file: &File) -> Result<EnvelopeIter, JournalError> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    Ok(EnvelopeIter {
        lines: BufReader::new(reader).lines(),
        next_seq: 1,
        line_no: 0,
        done: false,
    })
}

/// `Iterator<Item = Result<Event, JournalError>>` over a buffered reader
/// from the file's start (store.md §2). Wraps `EnvelopeIter`, dropping
/// the store-only `seq`.
pub struct JournalIter {
    inner: EnvelopeIter,
}

impl Iterator for JournalIter {
    type Item = Result<Event, JournalError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|item| item.map(|(_seq, event)| event))
    }
}

/// Private: parses one envelope per line, checking `seq` against the
/// expected next value. Stops and yields one final `Err` on the first
/// line that fails to parse or whose `seq` is not `expected` — never
/// silently skipped (§5); nothing is yielded after that `Err`.
struct EnvelopeIter {
    lines: std::io::Lines<BufReader<File>>,
    next_seq: u64,
    line_no: usize,
    done: bool,
}

impl Iterator for EnvelopeIter {
    type Item = Result<(u64, Event), JournalError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let line = match self.lines.next()? {
            Ok(line) => line,
            Err(source) => {
                self.done = true;
                return Some(Err(JournalError::Io(source)));
            }
        };
        self.line_no += 1;
        let envelope: Envelope = match serde_json::from_str(&line) {
            Ok(envelope) => envelope,
            Err(source) => {
                self.done = true;
                return Some(Err(JournalError::Malformed {
                    line: self.line_no,
                    source,
                }));
            }
        };
        if envelope.seq != self.next_seq {
            self.done = true;
            return Some(Err(JournalError::SeqDiscontinuity {
                line: self.line_no,
                expected: self.next_seq,
                found: envelope.seq,
            }));
        }
        self.next_seq += 1;
        Some(Ok((envelope.seq, envelope.event)))
    }
}

/// store.md §2. `Malformed`/`SeqDiscontinuity` are reported, never
/// silently skipped (§5) — the decisive check's own words.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal io error")]
    Io(#[from] std::io::Error),
    #[error("event {index} failed to serialize")]
    Serialize {
        index: usize,
        source: serde_json::Error,
    },
    #[error("malformed line {line}: {source}")]
    Malformed {
        line: usize,
        source: serde_json::Error,
    },
    #[error("seq discontinuity at line {line}: expected {expected}, found {found}")]
    SeqDiscontinuity {
        line: usize,
        expected: u64,
        found: u64,
    },
}

/// D9#3: a Claim missing a required artifact is refused, the Run stays
/// open. D9#4: a fabricated triple is recorded, not honored. Types and the
/// refusal enum are final in this item; the validator body is item 3's
/// "claim validation and wirkd" (0023 D81; build-brief.md §2, J5 over R7).
pub fn validate_claim(waypoint: &WaypointDefinition, run: &Run, claim: &Claim) -> ClaimVerdict {
    // 1. Triple match (0001 D9#4): the Claim must name the Run it is
    // filed against. `work_id` is not checked here — that needs the
    // `Work`, which this signature does not carry; wirkd checks it
    // (item 3's process, W3).
    if claim.triple.run_id != run.id {
        return ClaimVerdict::Refused(ClaimRefusal::TripleMismatch);
    }

    // 2. Run state (build brief amendment 1, this item's J1): a Claim
    // against an already-`Claimed` Run is refused; `Open`, `Failed`,
    // and `Vanished` all proceed (d9_5's precedent — a late claim is
    // evidence the work completed, not stale).
    //
    // W4 (P2.6 run 3, rerun3's own second-Claim finding): a Run marked
    // `Failed{status: "retried"}` is not an ordinary Failed run — it was
    // explicitly superseded by `handle_retry`'s own new Run for the same
    // Waypoint (`server.rs::handle_retry`'s own `"superseded by retry
    // <id>"` detail), so d9_5's "late claim is evidence the work
    // completed" precedent does not apply to it: its Waypoint's
    // completion path already moved to the retry, and any further Claim
    // against the superseded Run is stale, not late — refused the same
    // way an already-`Claimed` Run's second Claim is, so it can never
    // re-trigger auto-advance a second time for the Waypoint the retry
    // already carried forward. An ordinary `Failed` (no retry, e.g. a
    // crashed child process or a `RunFailed` filed by `run-deterministic`
    // itself) and `Vanished` are unchanged: still honored.
    if matches!(run.state, RunState::Claimed(_)) {
        return ClaimVerdict::Refused(ClaimRefusal::AlreadyClaimed);
    }
    if let RunState::Failed(cause) = &run.state
        && cause.status.as_deref() == Some("retried")
    {
        return ClaimVerdict::Refused(ClaimRefusal::AlreadyClaimed);
    }

    // 3. Artifacts by name (0001 D9#3), skipped for a Question claim
    // (0027 D87): every declared, required output must appear in
    // `claim.artifacts` by name; the first missing one refuses.
    if matches!(claim.kind, ClaimKind::Done) {
        for output in &waypoint.declared_outputs {
            if !output.required {
                continue;
            }
            let present = claim.artifacts.iter().any(|a| a.name == output.name);
            if !present {
                return ClaimVerdict::Refused(ClaimRefusal::MissingArtifact(output.name.clone()));
            }
        }
    }

    ClaimVerdict::Validated
}

// ---- Executor trait --------------------------------------------------------

/// 0001 D2-D4; 0017 D53, D56. One trait a Herdr pane executor (wirk-herdr)
/// and a deterministic child/docker executor (`wirk` bin, 0022 D78) both
/// implement. Never reports completion: `launch`/`poll` only produce
/// `RunObservation` (no `Completed` variant) — completion is only a
/// validated Claim, filed via a separate path (`wirk claim` -> wirkd),
/// never through this trait (0017 D56).
pub trait Executor {
    type Error: std::error::Error;
    fn launch(&self, run: &Run, world: &World) -> Result<(), Self::Error>;
    fn poll(&self, run: &Run) -> Result<RunObservation, Self::Error>;
}

/// Per orient/core.md line 141.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunObservation {
    Running,
    Failed(FailureCause),
    Vanished,
}
