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

use serde::{Deserialize, Serialize};
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
    /// each `command` word, each `expected_artifacts` spec's `name` and
    /// `required` flag. Excluded (world.md §2): `worktree_path`,
    /// `estate_root`, `cwd`, `env`, `triple`, every id.
    ///
    /// Hex-encoded lowercase.
    pub fn of(world: &World) -> WorldHash {
        let mut hasher = Sha256::new();
        match world {
            World::Actor(actor) => {
                hasher.update([0u8]);
                hasher.update(actor.repository.as_bytes());
                hasher.update([0x1f]);
                hasher.update(actor.branch.as_bytes());
                hasher.update([0x1f]);
                hasher.update(actor.base_sha.as_bytes());
                hasher.update([0x1f]);
                hasher.update(actor.intent.as_bytes());
                hasher.update([0x1f]);
                for spec in &actor.output_contract.0 {
                    hasher.update(spec.name.as_bytes());
                    hasher.update([0x1f]);
                    hasher.update([spec.required as u8]);
                    hasher.update([0x1f]);
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
                for spec in &det.expected_artifacts.0 {
                    hasher.update(spec.name.as_bytes());
                    hasher.update([0x1f]);
                    hasher.update([spec.required as u8]);
                    hasher.update([0x1f]);
                }
            }
        }
        let digest = hasher.finalize();
        WorldHash(hex_lower(&digest))
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

/// Sergeant's workflow. `retry_policy` is new (incident file item 4: "a
/// retry policy is a Route concern... not a hidden default in the
/// executor" — no sergeant field carries this; orient/core.md line
/// 48-52).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub id: RouteId,
    pub waypoints: Vec<WaypointDefinition>,
    pub retry_policy: RetryPolicy,
}

/// Per orient/core.md line 54.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffPolicy,
}

/// Per orient/core.md line 56.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackoffPolicy {
    None,
    FixedSeconds(u32),
}

/// Sergeant's stage, from `StageDefinition`/`StageBinding`
/// (domain/workflow.rs:460-475); `harness`/`route_source`/`profile`
/// (backend-selection) dropped, Herdr-adjacent (0022 D71).
/// `declared_outputs` kept: D9#3 checks a Claim against it
/// (orient/core.md line 57-62).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaypointDefinition {
    pub id: WaypointId,
    pub kind: WaypointKind,
    pub declared_outputs: Vec<ArtifactSpec>,
}

/// From sergeant's `StageKind` (Actor). `Container` (backend concept)
/// becomes `Deterministic`, naming wirk's own executor split (0001 D4)
/// instead of sergeant's backend taxonomy — which `Executor` impl runs
/// a Waypoint is a bin-crate binding decision (0022 D78), not a field
/// (orient/core.md line 63-68).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaypointKind {
    Actor,
    Deterministic,
}

/// Per orient/core.md line 70.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSpec {
    pub name: String,
    pub required: bool,
}

/// A Waypoint's declared required artifacts, authored on the Route
/// (build-brief.md §2 "OutputContract/Boundary ... decided now ...
/// Route-authored fields"; minimal wrapper, R6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputContract(pub Vec<ArtifactSpec>);

/// The declared mutation surface and authority envelope for a Waypoint
/// (vocabulary.md "Boundary": "declared mutation surface... per-Route";
/// 0001 D5), as path globs the Waypoint may mutate. Minimal, R6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Boundary(pub Vec<String>);

// ---- ExecutionTriple ------------------------------------------------------

/// 0022 D73: names stand. Adopted from sergeant's causation triple,
/// cited in `wirk/src/main.rs` ("sergeant-rs v0.3.0, W1 hierarchical
/// execution contract §6"); reshaped to carry `RunId` at the third slot
/// per wirk's Run/Waypoint split (orient/core.md line 98-103). Moved
/// into W1 because `ActorWorld` carries it as `triple: ExecutionTriple`
/// (build-brief.md §2 amendment), not three separate strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTriple {
    pub estate_root: String,
    pub work_id: WorkId,
    pub run_id: RunId,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicWorld {
    /// Route: the Waypoint's own command definition (0001 D4:
    /// "wirk-owned executors").
    pub command: Vec<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum World {
    Actor(ActorWorld),
    Deterministic(DeterministicWorld),
}

// ---- Run ------------------------------------------------------------------

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
            // W2 (p1-journal): RunOpened/RunLaunched are Run-scoped
            // bookkeeping the Work-level `fold` owns (fold.md §1);
            // WorkSubmitted/WaypointReserved/WorkFailed/WorkCanceled
            // carry no `run` and never reach this match (the guard
            // above returns first) — arms kept only for exhaustiveness.
            EventKind::RunOpened { .. }
            | EventKind::RunLaunched { .. }
            | EventKind::WorkSubmitted { .. }
            | EventKind::WaypointReserved { .. }
            | EventKind::WorkFailed { .. }
            | EventKind::WorkCanceled { .. } => {}
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
    /// D9#2: folding this NEVER changes RunState; sergeant's
    /// stage-lifecycle events WERE authoritative (projection.rs:1093-1101),
    /// wirk's is inert.
    LifecycleObserved {
        status: String,
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
    /// attempts under `RetryPolicy` (fold.md §2, R6).
    RunOpened {
        run: RunId,
        waypoint: WaypointId,
        attempt: u32,
        world_hash: WorldHash,
    },
    /// "Run's actor launched" (BRIEF.md Intent); an activity signal
    /// issue 286 needs, distinct from `RunOpened` so a stalled launch is
    /// visible before any lifecycle event arrives (fold.md §2, R6).
    RunLaunched {
        run: RunId,
    },
    /// Terminal Work failure is never inferred from `RunFailed`
    /// (incident file); an explicit event keeps `fold` retry-policy-
    /// agnostic — the component that exhausts `RetryPolicy` (items 4, 5)
    /// fires this (fold.md §2, §8; 0027 D92). J1: no ruling pins
    /// Work-level failure firing yet.
    WorkFailed {
        cause: FailureCause,
    },
    /// Owner/operator cancellation; no existing event carries this verb
    /// (fold.md §2, R6).
    WorkCanceled {
        reason: Option<String>,
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
            } = &event.kind
            {
                route_waypoints = waypoints.clone();
                work = Some(Work {
                    id: event.work.clone(),
                    intent: intent.clone(),
                    route: route.clone(),
                    repositories: repositories.clone(),
                    state: WorkState::Pending,
                    current_waypoint: None,
                    last_activity: event.at,
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
                    }
                }
            }
            EventKind::RunOpened { run, waypoint, .. } => {
                run_waypoints.insert(run.clone(), waypoint.clone());
            }
            EventKind::RunLaunched { .. } => {}
            // D9#2: inert at Run level (0001 D9 #2; 0017 D56); equally
            // inert here — a lifecycle event alone never advances or
            // otherwise changes WorkState (fold.md §6, rejecting
            // sergeant's KIND_WORK_COMPLETED jump table).
            EventKind::LifecycleObserved { .. } => {}
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
                        if is_last_waypoint {
                            w.state = WorkState::Completed;
                        } else if !w.state.is_terminal() {
                            // current_waypoint unchanged until the next
                            // WaypointReserved (fold.md §1).
                            w.state = WorkState::Active;
                        }
                    }
                    (ClaimVerdict::Validated, ClaimKind::Question(_)) => {
                        if !w.state.is_terminal() {
                            w.state = WorkState::NeedsInput;
                        }
                    }
                    // A refusal is not a Work fact, mirrors Run::apply
                    // line ~443 (fold.md §1).
                    (ClaimVerdict::Refused(_), _) => {}
                }
            }
            // "A failed Run is not a failed Work" (incident file;
            // fold.md §1) — activity only.
            EventKind::RunFailed { .. } => {}
            EventKind::RunVanished => {}
            EventKind::WorktreeCreated { .. } => {}
            EventKind::WorkFailed { .. } => {
                w.state = WorkState::Failed;
            }
            EventKind::WorkCanceled { .. } => {
                w.state = WorkState::Canceled;
            }
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
    pub fn append(&mut self, event: &Event) -> Result<(), JournalError> {
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
        Ok(())
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
    if matches!(run.state, RunState::Claimed(_)) {
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
