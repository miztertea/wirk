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
        if world.source_basis() == &SourceBasis::Unknown {
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
fn hex_lower(bytes: &[u8]) -> String {
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
            // W1 (0041 D129): the one place a Run's `kind` moves after
            // being seeded (at `RunOpened`, before `--actor-kind` is
            // known) to the kind `wirk run` actually launched —
            // `run_launched_with_opencode_kind_updates_run` pins it.
            EventKind::RunLaunched { actor_kind, .. } => {
                self.kind = actor_kind.clone();
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
            | EventKind::ChildWorkSpawned { .. } => {}
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
    pub path: String,
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
    /// checkout, otherwise the claimed path verbatim.
    pub path: String,
    /// Lowercase hex sha256 of the file's bytes at validation.
    pub digest: String,
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
            },
            NameOnly(String),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Recorded { name, path, digest } => ArtifactReceipt { name, path, digest },
            Wire::NameOnly(name) => ArtifactReceipt {
                name,
                path: String::new(),
                digest: String::new(),
            },
        })
    }
}

impl ArtifactReceipt {
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
    RunLaunched {
        run: RunId,
        #[serde(default)]
        actor_kind: ActorKind,
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
            EventKind::RunLaunched { .. } => {}
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
                "Working"
                    if w.state == WorkState::NeedsInput
                        && w.needs_input
                            .as_ref()
                            .is_some_and(|cause| cause.reason == "blocked") =>
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

    work.expect("fold called with no WorkSubmitted event in the slice: no Work to build")
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
        let mut reader = self.file.try_clone()?;
        reader.seek(SeekFrom::Start(0))?;
        Ok(EnvelopeIter {
            lines: BufReader::new(reader).lines(),
            next_seq: 1,
            line_no: 0,
            done: false,
        })
    }
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
