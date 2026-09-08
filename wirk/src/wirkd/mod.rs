//! wirkd wire protocol: the envelope and verb types (W2, `orient/
//! transport.md` §2, §4; `orient/build-brief.md` W2), plus the server
//! loop (W3, `server` submodule) and the client (`client` submodule)
//! that both serialize against them. One NDJSON-framed JSON object per
//! request and per reply, the Journal's own line-delimited convention
//! reused (R2).
//!
//! Seven verbs (transport.md §2; `record` and `fail` both new since W2):
//! `ping`, `submit`, `claim`, `status`, `record`, `stop`, `fail`. `ping`
//! and `stop` carry no payload fields; `submit`, `claim`, `status`,
//! `record`, `fail` each have a typed payload struct so a caller does
//! not hand-build JSON, but `Request.payload` itself stays a
//! `serde_json::Value` — the one shape both a typed payload (via
//! `Request::submit`/`claim`/`status`/`record`/`fail`) and a scripted
//! fake server's literal JSON (the W2 test) can produce identically.
//!
//! `record` (item 4 W3, `orient/build-brief.md`): appends one
//! `EventKind` to a Work's journal through the same single write path
//! `submit`/`claim` already use, for the journal writes `RunLoop` needs
//! (`RunLaunched`, `RunFailed`, `RunVanished`, `LifecycleObserved`,
//! `WorktreeCreated`, and a re-emitted `WaypointReserved` carrying the
//! worktree path once it exists — R1: no new event type for "the
//! World's worktree_path changed", the existing `WaypointReserved` is
//! re-recorded with the field filled in, `server.rs` reading the *last*
//! one). `ClaimFiled`/`ClaimRecorded` are refused through this verb:
//! those two are written only by `claim`'s own validated path, never by
//! a caller naming them directly.
//!
//! `fail` (item 5 W3, `orient/build-brief.md` §3 W3; `orient/child.md`
//! §7 item 2): the only way `wirk run-deterministic` — a separate
//! `wirk` invocation from the wirkd it talks to, never a library call
//! into it — can turn a local executor `launch` error or a
//! `RunObservation::Failed` into a journaled `RunFailed`; the Journal
//! itself lives behind wirkd's own socket, not a handle that process
//! holds.

// `wirk/tests/wirkd_client.rs` compiles this module into its own crate
// root via `#[path]` (not through `main.rs`) to unit-test the wire
// types and the client directly; that test binary never calls into
// `server`'s items, so its own dead-code analysis would otherwise flag
// them there even though `main.rs` uses every one of them for real.
// Allowed at the module level, not scattered per item (`#![...]`
// cascades to `client`/`server` as this module's own descendants).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use wirk_core::{
    ClaimKind, EventKind, ExecutionTriple, ParentBinding, RepositoryBinding, RunId, WorkId,
};

// P2.3 W2 (decide.md §1): `Verb::Retry`/`Verb::WorkFail` and their
// payloads land in this module alongside every other verb's — `Fail`
// (item 5, `run-deterministic`'s own verb) already names the Run-level
// failure; `Retry`/`WorkFail` are the human's decision verbs, named
// distinctly so `wirk work retry`/`wirk work fail` read as one thing
// each, not a second reading of `fail`.

/// The boundary glob matcher (P2.4 W1): `server`'s own submodule, not
/// `pub` — nothing outside `wirkd` reaches a Waypoint's boundary
/// directly.
mod boundary;
pub mod client;
pub mod server;

// ---- Request ---------------------------------------------------------

/// The six verbs a request names (transport.md §2; `Fail` new this
/// wave). Serialized as its lowercase name (`"ping"`, `"submit"`, ...),
/// matching the envelope's `{"verb": "<name>", ...}` shape verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verb {
    Ping,
    Submit,
    Claim,
    Status,
    Record,
    Stop,
    Fail,
    /// P2.3 W2 (decide.md §1): opens a fresh Run on the same reserved
    /// World for a `NeedsInput` Work — the human's "try again" verb.
    Retry,
    /// P2.3 W2 (decide.md §1): terminal `WorkFailed{cause}` with the
    /// human's reason — 0033 D102's event, never inferred from a
    /// `RunFailed`.
    WorkFail,
    /// Item B, ruling 0044: a long-lived connection, not the usual
    /// one-request-one-reply shape — `server::handle_connection`
    /// special-cases it before the normal `dispatch`/single-`Reply`
    /// path, and `client::watch` reads a blocking line iterator instead
    /// of one `Reply`.
    Watch,
    /// W-A (§3.4): the operator's cancel verb. `--cascade` cancels every
    /// open (non-terminal) descendant child Work first, attributed via
    /// `caused_by`, before canceling the named Work itself; without it,
    /// an open child refuses the whole verb (`OpenChild`).
    Cancel,
    /// P3 W3: registers a source (on first use) and stages an
    /// exact-generation acquisition (`wirk-atlas`'s own domain) from a
    /// real Git repository — a producer operation, never implicit from
    /// a query.
    AtlasAcquire,
    /// P3 W3: re-acquires a registered source's current ref, staging a
    /// candidate generation without publishing it.
    AtlasRefresh,
    /// P3 W3: atomically advances a registered source's published
    /// generation to an already-staged one.
    AtlasPublish,
    /// P3 W4 A: builds one immutable semantic edition over an
    /// already-staged generation with an explicitly configured backend
    /// and model, and stages it. No query ever triggers this.
    AtlasSemanticBuild,
    /// P3 W4 A: atomically selects an already-built, fully verified
    /// semantic edition as a source's published one — the separate,
    /// explicit publication step for vectors.
    AtlasSemanticSelect,
    /// P3 W3: reports registered sources, their published generation
    /// and coverage summary, and recent acquisition attempts.
    AtlasStatus,
    /// P3 W3: lexical (optionally semantic-requested) ranked search
    /// over admitted, published generations.
    AtlasSearch,
    /// P3 W3: exact evidence-coordinate resolution.
    AtlasResolve,
    /// P3 W3: admits one evidenced `GovernedBy` relationship.
    AtlasRelate,
    /// W-B (§5.2): an actor's own bounded, evidence-backed claim.
    /// Triple-checked like `claim` — `wirkd` refuses a Run that is not
    /// current for its Waypoint.
    FindingRaise,
    /// W-B (§2.5): records an honestly unverified human/client
    /// assertion. No execution triple — an operator verb, never a
    /// settlement (construction review: "do not make a command named
    /// `settle` return success while only appending an assertion").
    FindingAssert,
    /// W-B (§2.4): requests wirkd evaluate the named finding's
    /// settlement readiness right now and returns the real outcome —
    /// settled, still pending (naming why), or refused. Never a wire
    /// field for a decision: settling is minting a derived fact, not
    /// recording an asserted one.
    FindingSettle,
    /// W-B (§4): records the owning source's mechanical before/after
    /// change plus the attributed judgement that it implements the
    /// finding — never a bare `applied: true`.
    FindingApplied,
    /// W-B (§9): every finding this estate (or one Work) knows about,
    /// with its settlement/assertions/application.
    FindingList,
    /// W-B (§7): the estate's derived Findings index — list, or rebuild
    /// it from journals alone.
    AtlasFindings,
    /// W-B basis access (`loop-b-basis-access`, the integrated review's
    /// §5.1): read-only inspection of the verification obligations a
    /// Work's own Route declares, each with the canonical
    /// `wirk_core::obligation_basis` its currently reserved World
    /// produces and the admission state of this estate's own settlement
    /// policy against it. Mints nothing, admits nothing, appends
    /// nothing — the value an operator needs to write
    /// `policy/settlement.json` by hand, which before this verb existed
    /// only inside an already-settled record.
    WorkObligations,
}

/// One NDJSON-framed request line: `{"verb": "<name>", "payload": {...}}`
/// (transport.md §2). `payload` is untyped on the wire so `ping`/`stop`
/// (no fields) and `submit`/`claim`/`status` (typed below) share one
/// envelope shape; the `Request::*` constructors are the typed door in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub verb: Verb,
    pub payload: Value,
}

impl Request {
    /// `{"verb":"ping","payload":{}}` — no fields (transport.md §2).
    pub fn ping() -> Self {
        Request {
            verb: Verb::Ping,
            payload: Value::Object(serde_json::Map::new()),
        }
    }

    /// `{"verb":"stop","payload":{}}` — no fields (transport.md §2).
    pub fn stop() -> Self {
        Request {
            verb: Verb::Stop,
            payload: Value::Object(serde_json::Map::new()),
        }
    }

    /// Serializes `payload` into the envelope's untyped `payload` field.
    /// Panics only if `SubmitPayload`/`ClaimPayload`/`StatusPayload`'s
    /// own `Serialize` impl fails, which none of the three can (plain
    /// data, no maps with non-string keys, no floats) — `expect` names
    /// that invariant rather than threading an infallible `Result`.
    pub fn submit(payload: SubmitPayload) -> Self {
        Request {
            verb: Verb::Submit,
            payload: serde_json::to_value(payload).expect("SubmitPayload always serializes"),
        }
    }

    pub fn claim(payload: ClaimPayload) -> Self {
        Request {
            verb: Verb::Claim,
            payload: serde_json::to_value(payload).expect("ClaimPayload always serializes"),
        }
    }

    pub fn status(payload: StatusPayload) -> Self {
        Request {
            verb: Verb::Status,
            payload: serde_json::to_value(payload).expect("StatusPayload always serializes"),
        }
    }

    /// W3: `RunLoop`'s journal writes and `wirk run`'s own worktree/
    /// World-update writes, both routed through wirkd's single write
    /// path rather than opening the Work's journal file directly (item
    /// 3's design: "the Journal the only writer").
    pub fn record(payload: RecordPayload) -> Self {
        Request {
            verb: Verb::Record,
            payload: serde_json::to_value(payload).expect("RecordPayload always serializes"),
        }
    }

    /// `fail`'s request: `run-deterministic`'s own way of journaling a
    /// local executor failure (module doc) — never invented by wirkd
    /// itself.
    pub fn fail(payload: FailPayload) -> Self {
        Request {
            verb: Verb::Fail,
            payload: serde_json::to_value(payload).expect("FailPayload always serializes"),
        }
    }

    /// `retry`'s request (P2.3 W2, decide.md §1): the failed Run's own
    /// triple names the Work and which Run to reopen against.
    pub fn retry(payload: RetryPayload) -> Self {
        Request {
            verb: Verb::Retry,
            payload: serde_json::to_value(payload).expect("RetryPayload always serializes"),
        }
    }

    /// `workfail`'s request (P2.3 W2, decide.md §1): the Work to fail
    /// and the human's reason, verbatim into `WorkFailed.cause.detail`.
    pub fn workfail(payload: WorkFailPayload) -> Self {
        Request {
            verb: Verb::WorkFail,
            payload: serde_json::to_value(payload).expect("WorkFailPayload always serializes"),
        }
    }

    /// `watch`'s request (item B): the Work whose journal appends the
    /// caller wants streamed, starting with what is already there.
    pub fn watch(payload: WatchPayload) -> Self {
        Request {
            verb: Verb::Watch,
            payload: serde_json::to_value(payload).expect("WatchPayload always serializes"),
        }
    }

    /// `cancel`'s request (W-A, §3.4).
    pub fn cancel(payload: CancelPayload) -> Self {
        Request {
            verb: Verb::Cancel,
            payload: serde_json::to_value(payload).expect("CancelPayload always serializes"),
        }
    }

    pub fn atlas_acquire(payload: AtlasAcquirePayload) -> Self {
        Request {
            verb: Verb::AtlasAcquire,
            payload: serde_json::to_value(payload).expect("AtlasAcquirePayload always serializes"),
        }
    }

    pub fn atlas_refresh(payload: AtlasRefreshPayload) -> Self {
        Request {
            verb: Verb::AtlasRefresh,
            payload: serde_json::to_value(payload).expect("AtlasRefreshPayload always serializes"),
        }
    }

    pub fn atlas_publish(payload: AtlasPublishPayload) -> Self {
        Request {
            verb: Verb::AtlasPublish,
            payload: serde_json::to_value(payload).expect("AtlasPublishPayload always serializes"),
        }
    }

    pub fn atlas_semantic_build(payload: AtlasSemanticBuildPayload) -> Self {
        Request {
            verb: Verb::AtlasSemanticBuild,
            payload: serde_json::to_value(payload)
                .expect("AtlasSemanticBuildPayload always serializes"),
        }
    }

    pub fn atlas_semantic_select(payload: AtlasSemanticSelectPayload) -> Self {
        Request {
            verb: Verb::AtlasSemanticSelect,
            payload: serde_json::to_value(payload)
                .expect("AtlasSemanticSelectPayload always serializes"),
        }
    }

    pub fn atlas_status(payload: AtlasStatusPayload) -> Self {
        Request {
            verb: Verb::AtlasStatus,
            payload: serde_json::to_value(payload).expect("AtlasStatusPayload always serializes"),
        }
    }

    pub fn atlas_search(payload: AtlasSearchPayload) -> Self {
        Request {
            verb: Verb::AtlasSearch,
            payload: serde_json::to_value(payload).expect("AtlasSearchPayload always serializes"),
        }
    }

    pub fn atlas_resolve(payload: AtlasResolvePayload) -> Self {
        Request {
            verb: Verb::AtlasResolve,
            payload: serde_json::to_value(payload).expect("AtlasResolvePayload always serializes"),
        }
    }

    pub fn atlas_relate(payload: AtlasRelatePayload) -> Self {
        Request {
            verb: Verb::AtlasRelate,
            payload: serde_json::to_value(payload).expect("AtlasRelatePayload always serializes"),
        }
    }

    pub fn finding_raise(payload: FindingRaisePayload) -> Self {
        Request {
            verb: Verb::FindingRaise,
            payload: serde_json::to_value(payload).expect("FindingRaisePayload always serializes"),
        }
    }

    pub fn finding_assert(payload: FindingAssertPayload) -> Self {
        Request {
            verb: Verb::FindingAssert,
            payload: serde_json::to_value(payload).expect("FindingAssertPayload always serializes"),
        }
    }

    pub fn finding_settle(payload: FindingSettlePayload) -> Self {
        Request {
            verb: Verb::FindingSettle,
            payload: serde_json::to_value(payload).expect("FindingSettlePayload always serializes"),
        }
    }

    pub fn finding_applied(payload: FindingAppliedPayload) -> Self {
        Request {
            verb: Verb::FindingApplied,
            payload: serde_json::to_value(payload)
                .expect("FindingAppliedPayload always serializes"),
        }
    }

    pub fn finding_list(payload: FindingListPayload) -> Self {
        Request {
            verb: Verb::FindingList,
            payload: serde_json::to_value(payload).expect("FindingListPayload always serializes"),
        }
    }

    pub fn atlas_findings(payload: AtlasFindingsPayload) -> Self {
        Request {
            verb: Verb::AtlasFindings,
            payload: serde_json::to_value(payload).expect("AtlasFindingsPayload always serializes"),
        }
    }

    pub fn work_obligations(payload: WorkObligationsPayload) -> Self {
        Request {
            verb: Verb::WorkObligations,
            payload: serde_json::to_value(payload)
                .expect("WorkObligationsPayload always serializes"),
        }
    }
}

/// `submit`'s payload (transport.md §2): the repository bindings the
/// Work declares and the base ref its worktree is cut from. `intent`
/// is carried for wire-shape compatibility only (p2-route-files W2,
/// J1) — `--intent` is removed from `wirk work submit`, so every real
/// caller now sends an empty string; a Waypoint's own intent is
/// authored in its Route file (`WaypointDefinition.intent`).
///
/// `kind`/`command`/`repo_path` are additive across both items (item 4's
/// `--kind actor --repo-path <path>` and item 5's `--kind deterministic
/// --command <argv...>`, `orient/build-brief.md` "Outcome" for each):
/// `#[serde(default)]` on all three keeps every existing caller that
/// never sets them parsing exactly as before. `kind: Some("actor")`
/// with a `repo_path` resolves `base_ref` to a commit SHA with git at
/// submit time (issue 285: an unresolved ref left the worktree's pin
/// meaningless) and reserves an `ActorWorld` with an empty
/// `worktree_path` — `wirk run` fills it in once the worktree exists
/// (`RecordPayload`, below). `kind: Some("deterministic")` with
/// `command` non-empty reserves a `World::Deterministic` instead, the
/// one submit shape that carries no Route at all (build-brief.md
/// §7.3) — every other submit resolves `route` (below) to a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitPayload {
    pub intent: String,
    pub repositories: Vec<RepositoryBinding>,
    pub base_ref: String,
    /// Explicit inspection contract for deterministic execution. Actor
    /// submissions are always verified Git bindings. Missing on legacy
    /// clients preserves the established ad-hoc output-only meaning of
    /// `base_ref`; persisted Worlds never default this field implicitly.
    #[serde(default)]
    pub source_basis: Option<wirk_core::SourceBasis>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub repo_path: Option<String>,
    /// p2-route-files W2 (build-brief.md §7.3): a bare name or a path
    /// naming the Route file to load (`server.rs::resolve_route_path`,
    /// `wirk_core::load_route`) — required for every submit except the
    /// ad hoc `--kind deterministic --command` shape above.
    #[serde(default)]
    pub route: Option<String>,
    /// W-A (§3.3): present only for a child Work submission, naming the
    /// parent Work/container/Run/role it is submitted under.
    /// `handle_submit` checks this against the parent's own journal
    /// before minting anything (`ChildParentMismatch`/
    /// `ChildExceedsParentBinding`).
    #[serde(default)]
    pub parent: Option<ParentBinding>,
    /// P3 W3 (ruling 0090): names which entry of `repositories` is this
    /// Work's actual execution/write checkout, when more than one
    /// binding is declared (`handle_submit` refuses an ambiguous or
    /// unknown designation rather than guessing the first element).
    /// Optional when zero or one binding is declared, preserving the
    /// legacy single-repository submit line unchanged. Never itself
    /// trusted as the verified identity — `wirkd` resolves the real
    /// canonical repository this names from `repo_path` itself.
    #[serde(default)]
    pub execution_repo: Option<String>,
}

/// `claim`'s payload (transport.md §2): the injected
/// `ExecutionTriple`, the `ClaimKind` (`Done` or `Question`, 0027 D87),
/// and the artifacts by name-to-path pair — `BTreeMap` for a
/// deterministic wire order, matching `ArtifactRef.name` ->
/// `ArtifactRef.path` (`wirk-core::ArtifactRef`) one pair at a time.
/// `Claim.id` is minted server-side (transport.md §2: "the client sends
/// an empty id"), so no id field travels here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimPayload {
    pub triple: ExecutionTriple,
    pub kind: ClaimKind,
    pub artifacts: BTreeMap<String, String>,
}

/// `status`'s payload (transport.md §2): the `Work` to report on.
///
/// W-B launch disclosure integration (the launch review's F-C):
/// `requester`/`admin` is the identical exclusive pair
/// `FindingListPayload` already carries, with **no silent unscoped
/// default**. `status` returns compiled World content, a Run's resolved
/// launch selection, Herdr's own `launch_argv`, the attempt's
/// destination and every validated Claim's artifact paths and digests —
/// the same record parts `event_source_disclosure` classifies as
/// checkout-derived. It previously took no requester at all, so any
/// caller that could reach the socket read all of them for any Work id.
/// It now answers one of two named ways: scoped to a `requester` Work's
/// own lineage and bindings, or explicitly `admin`.
///
/// `admin` is the *named* operator surface, exactly as it is on
/// `finding list` and `atlas findings`, and exactly as honestly
/// unauthenticated: the same OS uid runs an operator's terminal and an
/// actor's shell, so naming it proves nothing and claims nothing. What
/// it buys is that a scoped consultation can no longer *silently* fall
/// through to the unscoped answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub work_id: WorkId,
    /// The caller's own Work: it sees this Work only if it is in that
    /// Work's own lineage, and sees the checkout-derived halves only if
    /// its own bindings cover the target Work's whole binding set.
    #[serde(default)]
    pub requester: Option<WorkId>,
    #[serde(default)]
    pub admin: bool,
}

impl StatusPayload {
    /// The explicitly administrative read: every field, unscoped, for
    /// any Work id. The human `wirk wirkd status`/`wirk work status`
    /// verbs and estate tooling.
    pub fn admin(work_id: WorkId) -> Self {
        Self {
            work_id,
            requester: None,
            admin: true,
        }
    }

    /// The scoped consultation: `requester` is the caller's own Work.
    /// A Work reading its *own* status is always fully admitted (its
    /// bindings trivially cover its own), which is what keeps `wirk
    /// run`'s setup read and `RunLoop`'s ongoing progress poll working
    /// unchanged while never handing either an estate-wide surface.
    pub fn scoped(work_id: WorkId, requester: WorkId) -> Self {
        Self {
            work_id,
            requester: Some(requester),
            admin: false,
        }
    }
}

/// `record`'s payload (item 4 W3): the `EventKind` to append, the `Work`
/// it belongs to, and the `Run` it is scoped to when the event kind
/// carries one (`WaypointReserved`, like `WorkSubmitted`, names no
/// `Run` — `Event.run` is `None` for both, `wirk-core`'s own
/// convention, `main.rs`'s `demo_events`). `wirkd` refuses
/// `ClaimFiled`/`ClaimRecorded` here (`server.rs::handle_record`):
/// those travel only through `claim`'s own validated path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordPayload {
    pub work_id: WorkId,
    #[serde(default)]
    pub run: Option<RunId>,
    pub kind: EventKind,
}

/// `fail`'s payload (item 5 W3, `run-deterministic`'s own verb, module
/// doc): the triple names which Work's journal and which Run; `status`/
/// `detail` become `FailureCause.status`/`.detail` verbatim (wirkd
/// stamps `at`, same as every other server-minted event timestamp) —
/// wirkd refuses (`TripleMismatch`) a `run_id` naming no `RunOpened` in
/// this Work's journal, the same D9#4 check `claim` makes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailPayload {
    pub triple: ExecutionTriple,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// `retry`'s payload (P2.3 W2, decide.md §1): the triple names the
/// Work and the failed Run — `handle_retry` refuses `NotNeedsInput`
/// unless the folded Work is `NeedsInput`, then `TripleMismatch` if
/// `run_id` names no `RunOpened` (same D9#4 shape `claim`/`fail` use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPayload {
    pub triple: ExecutionTriple,
}

/// `workfail`'s payload (P2.3 W2, decide.md §1): the Work to fail and
/// the human's reason, carried verbatim onto `WorkFailed.cause.detail`
/// — 0033 D102's explicit event, never inferred from a `RunFailed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkFailPayload {
    pub work_id: WorkId,
    pub reason: String,
}

/// `watch`'s payload (item B): the Work whose journal to stream.
///
/// W-B launch disclosure integration (the launch review's F-C, applied
/// to `status`'s own sibling): this streams the Work's **raw journal
/// events**, launch metadata and captured pane details included, so it
/// reaches strictly more than `status` does. It takes the identical
/// exclusive `requester`/`admin` pair, with no silent unscoped default.
///
/// A stream is admitted or refused whole — there is no withheld marker
/// here, because a partially redacted `Event` is not an `Event` and
/// every consumer of this stream folds it. So a `requester` gets the
/// stream when the target Work is on its own lineage *and* its own
/// bindings cover that Work's whole binding set, and `InadmissibleEvidence`
/// otherwise. `RunLoop`'s drive stream is a Work watching itself, which
/// is always both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchPayload {
    pub work_id: WorkId,
    #[serde(default)]
    pub requester: Option<WorkId>,
    #[serde(default)]
    pub admin: bool,
}

impl WatchPayload {
    /// The explicitly administrative stream: any Work's raw journal.
    pub fn admin(work_id: WorkId) -> Self {
        Self {
            work_id,
            requester: None,
            admin: true,
        }
    }

    /// The scoped stream: `requester` is the caller's own Work.
    pub fn scoped(work_id: WorkId, requester: WorkId) -> Self {
        Self {
            work_id,
            requester: Some(requester),
            admin: false,
        }
    }
}

/// `cancel`'s payload (W-A, §3.4): the Work to cancel, whether to
/// cascade into open children, and an optional human-readable reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelPayload {
    pub work_id: WorkId,
    #[serde(default)]
    pub cascade: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

// ---- Atlas (P3 W3) -------------------------------------------------------

/// `atlas acquire`'s payload: registers `source` (on first use, against
/// `repository`) in this daemon's one canonical estate and stages an
/// exact-generation acquisition at `revision`. An existing `source`
/// resolves through its membership and refuses a conflicting
/// `repository` (`wirk_atlas::AtlasStore::register_git`'s own check).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasAcquirePayload {
    pub source: String,
    pub repository: String,
    pub revision: String,
}

/// `atlas refresh`'s payload: reuses `source`'s existing registration
/// and membership; stages a candidate generation at `revision` without
/// publishing it (`AtlasStore::refresh`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasRefreshPayload {
    pub source: String,
    pub revision: String,
}

/// `atlas publish`'s payload: atomically advances `source`'s published
/// generation to the already-staged `generation` (its encoded
/// `GenerationId`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasPublishPayload {
    pub source: String,
    pub generation: String,
}

/// `atlas semantic build`'s payload (P3 W4 A). Backend and model are
/// caller configuration travelling as data on an argv boundary — the
/// product holds no model name, cache path or interpreter of its own,
/// and records what it was actually given as build provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSemanticBuildPayload {
    pub source: String,
    pub generation: String,
    pub backend: String,
    #[serde(default)]
    pub backend_args: Vec<String>,
    pub model: String,
    /// `"units"` or `"native"` (default `"units"`). What a row of the
    /// edition *is*, and the caller's explicit choice: a native-chunk
    /// edition and a unit edition over the same generation are different
    /// artifacts with different identities.
    #[serde(default)]
    pub chunker: Option<String>,
}

/// `atlas semantic select`'s payload (P3 W4 A).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSemanticSelectPayload {
    pub source: String,
    pub edition: String,
}

/// `atlas status`'s payload: every registered source, or one named
/// `source`, in this daemon's canonical estate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasStatusPayload {
    #[serde(default)]
    pub source: Option<String>,
    /// P3 W3 correction (ruling 0093, W3-CORRECTION.md item 3): when
    /// present, scopes disclosure to only the sources this Work's own
    /// journaled `repositories` admit — a denied source's
    /// locator/generation/revision is never disclosed through this
    /// verb (the exact leak W3-REVIEW-OBSERVATIONS bullet 4 and
    /// VERDICT.md L4 found: status has no `--work` scope at all).
    /// Absent, this is explicit estate-wide catalog administration —
    /// a distinct, intentionally broader capability from Work-scoped
    /// retrieval, never silently generalized into a source-grant of
    /// its own.
    #[serde(default)]
    pub work: Option<WorkId>,
}

/// `atlas search`'s payload. `work` is optional: present, admission is
/// derived *only* from that Work's own journaled `repositories`
/// (`handle_atlas_search` opens the Work itself — this wire shape
/// cannot carry a replacement grant set from the client); absent, the
/// estate-wide orientation scope applies (status/orientation tooling
/// only, `wirk_atlas::admission::QueryScope::EstateOrientation`).
/// `semantic`: `"requested"` or `"disabled"` (default `"disabled"`).
/// `continuation` (ruling 0093, W3-CORRECTION.md item 1): when present,
/// an opaque token a prior `search` answer's own `continuation` field
/// returned — pins that answer's exact generation vector and resumes
/// it at its own next offset, regardless of any `publish` since. The
/// decoded token's own `query`/`source`/`families`/`semantic`/`limit`/
/// `work` must match this request's exactly, or the request is refused
/// rather than silently continuing a different query under someone
/// else's captured generations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSearchPayload {
    #[serde(default)]
    pub work: Option<WorkId>,
    pub query: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub semantic: Option<String>,
    #[serde(default)]
    pub families: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub continuation: Option<String>,
    /// The semantic query backend, exactly as `semantic build`'s is:
    /// caller configuration travelling as data over an argv boundary.
    /// Absent means no semantic ranking can run, which is a truthful
    /// reason on the answer rather than an error.
    #[serde(default)]
    pub semantic_backend: Option<String>,
    #[serde(default)]
    pub semantic_backend_args: Vec<String>,
    #[serde(default)]
    pub semantic_model: Option<String>,
}

/// `atlas resolve`'s payload: `coordinate` is a hex encoding of one
/// JSON-serialized `wirk_atlas::ExactCoordinate` (`server.rs`'s own
/// `encode_coordinate`/`decode_coordinate` — Git pathnames are raw
/// bytes, never line-oriented text, so this travels as one opaque
/// argv-safe token rather than a delimited string). `work` is optional,
/// exactly as `AtlasSearchPayload`'s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasResolvePayload {
    #[serde(default)]
    pub work: Option<WorkId>,
    pub coordinate: String,
}

/// `atlas relate`'s payload: `work` is required (never optional) —
/// admitting an evidenced relationship needs a real, accountable
/// producer identity, which `handle_atlas_relate` derives itself from
/// the journaled `work` (never a client-supplied producer string, per
/// `wirk-atlas`'s own `admit_relationship` doc: "W3 must bind
/// `producer` to an admitted coordinator identity at the public
/// boundary"). `kind` is `"governed_by"` today (the only
/// `RelationshipKind` this increment admits). `from`/`to`/each
/// `evidence` entry are encoded coordinates, same shape as
/// `AtlasResolvePayload::coordinate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasRelatePayload {
    pub work: WorkId,
    pub kind: String,
    pub from: String,
    pub to: String,
    pub evidence: Vec<String>,
    /// P3 W3 second correction (ruling 0095): the producing action the
    /// caller believes it is asserting under. Optional — `wirkd` derives
    /// the Work's current action itself either way — and never believed:
    /// a stated value that is not the current action is refused rather
    /// than adopted, so an actor can state its own receipt and find out
    /// it is stale instead of silently asserting under a different one.
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub world: Option<String>,
}

// ---- Findings (W-B) -----------------------------------------------------

/// `finding raise`'s payload (§5.2, §9): the injected triple names the
/// raising Run, `wirkd` refuses one that is not current for its
/// Waypoint (`TripleMismatch`, the same rule `finding_raise` reuses from
/// `claim`). `kind`/`scope` are closed strings
/// (`"gap"|"contradicted_assumption"|"relationship"|"verified_outcome"`,
/// `"work_local"|"estate_local"`) — parsed by `server.rs`, an unknown
/// value refuses `BadRequest` rather than defaulting. Each
/// `evidence`/`contradicts`/`applies_to` entry is either an
/// already-hex-encoded `wirk_atlas::ExactCoordinate` (§3's `Source`
/// form, `server.rs::encode_coordinate`'s own shape) or the literal
/// `work/<id>/event/<id>` string (§3's `Journal` form) — `server.rs`
/// tells the two apart by the `work/` prefix, since a hex string never
/// contains `/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingRaisePayload {
    pub triple: ExecutionTriple,
    pub kind: String,
    pub scope: String,
    pub claim: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub contradicts: Vec<String>,
    #[serde(default)]
    pub applies_to: Vec<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub proposed_change: Option<String>,
    /// W-B obligation proof: `--obligation <id>@<edition>`, the
    /// verification obligation this Finding claims to discharge. A
    /// pointer wirkd re-checks against the Route's own Waypoint
    /// definition and the estate's own admitted policy — never a grant.
    #[serde(default)]
    pub obligation: Option<String>,
    /// W-B obligation proof: `--confirmed-by work/<id>/finding/<id>`,
    /// the child Finding this Finding names as its independent
    /// confirmation. Explicit, never inferred from matching prose.
    #[serde(default)]
    pub confirmed_by: Option<String>,
}

/// `finding assert`'s payload (§2.5): no execution triple at all — this
/// is the operator/client path, and `wirkd` records `by` as an honestly
/// unverified label, never authority. `decision` is one of
/// `"accepted"|"partially_accepted"|"rejected"|"deferred"|"superseded"`;
/// `"rejected"` reads `reason` (defaults to empty), `"superseded"` reads
/// `superseded_by` (a `FindingId`, required for that decision only).
///
/// W-B disclosure response repair
/// (`W-B-DISCLOSURE-RESPONSE-REPAIR.md`): `requester`/`admin` is the
/// same exclusive pair `FindingListPayload` already carries, no default.
/// `assert` additionally *writes* a `FindingAsserted` event onto the
/// target finding's own Work, so a non-admin requester off that Work's
/// lineage is refused before anything is appended — scope admission is
/// a read boundary, never itself settlement or write permission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingAssertPayload {
    pub finding: String,
    pub decision: String,
    pub by: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub requester: Option<WorkId>,
    #[serde(default)]
    pub admin: bool,
}

/// `finding settle`'s payload (§2.4, construction review): no decision
/// field exists on the wire at all — `handle_finding_settle` requests a
/// real evaluation of the named finding's admitted policy check and
/// reports the actual outcome, never a client-supplied verdict.
///
/// W-B disclosure response repair: `requester`/`admin` is the same
/// exclusive pair `FindingListPayload` already carries, no default. This
/// only bounds what the *reply* discloses; the settlement evaluation
/// itself is triggered and decided identically regardless of who asks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingSettlePayload {
    pub finding: String,
    #[serde(default)]
    pub requester: Option<WorkId>,
    #[serde(default)]
    pub admin: bool,
}

/// `work obligations`'s payload. `requester`/`admin` is the same
/// exclusive, always-named pair `status`, `finding list` and `finding
/// settle` carry — there is no silent unscoped default. `waypoint`
/// narrows the answer to one Waypoint of that Work's own Route and
/// refuses a name the Route does not carry, so a typo can never read as
/// "this Work declares no obligation".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkObligationsPayload {
    pub work_id: WorkId,
    #[serde(default)]
    pub waypoint: Option<String>,
    #[serde(default)]
    pub requester: Option<WorkId>,
    #[serde(default)]
    pub admin: bool,
}

/// `finding applied`'s payload (§4): `source`/`revision` name the
/// published generation `wirkd` re-derives and checks against, `by` is
/// the same honestly-unverified label `finding assert` carries (an
/// `Attribution::Claim` is only ever derived from a real Validated
/// Claim, never from this field).
///
/// W-B-CORRECT.md defect 3 ("require current valid producing Work/Run/
/// World for actor-attributed assertions"): `triple` is the caller's own
/// injected pane triple, exactly like `FindingRaisePayload` — `wirkd`
/// checks its currency (`TripleMismatch`/`WorkTerminal`/current-run)
/// before recording *any* attribution, `Asserted` included. This closes
/// the authority review's own executed counterexample: a bare `--by`
/// string, with no Work, Run, or checkout at all, previously wrote a
/// durable `FindingApplied` into another Work's journal. `claim`
/// requests the exact, checked `Attribution::Claim` path over the
/// caller's own current Run — a real Validated Done `ClaimRecorded` on
/// it, that Work's own `Write` binding on the named `source`, and that
/// Claim's own artifact receipt matching this Finding's exact path with
/// a digest equal to the after-generation's actual bytes — instead of
/// the default `Attribution::Asserted`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingAppliedPayload {
    pub triple: ExecutionTriple,
    pub finding: String,
    pub source: String,
    pub revision: String,
    pub by: String,
    /// W-B Application repair: the *cited historical* Claim, named
    /// separately from `triple` above. `triple` is always the caller —
    /// the current admitted producing action authoring the judgement
    /// (ruling 0095) — while these two name a Run whose Validated Done
    /// Claim is offered as causal evidence, and which is therefore
    /// allowed to be spent, closed and its Work terminal. The frozen
    /// candidate conflated the two into one `claim: bool` over the
    /// caller's own triple, which is why a terminal producer had to be
    /// admitted for the ordinary closing-Claim shape to work at all.
    /// `claim_work` defaults to the caller's own Work.
    #[serde(default)]
    pub claim_run: Option<String>,
    #[serde(default)]
    pub claim_work: Option<String>,
}

/// `finding list`'s payload (§9): every finding in the estate, or one
/// Work's own.
///
/// W-B-CORRECT.md defect 2 ("journal disclosure"): naming a `work` here
/// is a *selection*, never itself an evidence grant ("selecting an
/// origin Work id is not a general evidence grant"). A non-`admin` call
/// must carry the caller's own `requester` Work id, and the daemon
/// applies that Work's own effective admission (its own lineage) before
/// returning anything — the same rule `admit_evidence`'s Journal branch
/// already applies at raise time. `admin` is the one, explicit,
/// separately-named administrative path that bypasses this and sees the
/// whole estate or any named Work unscoped ("keep explicit
/// administrative inspection separate").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingListPayload {
    #[serde(default)]
    pub work: Option<WorkId>,
    #[serde(default)]
    pub requester: Option<WorkId>,
    #[serde(default)]
    pub admin: bool,
}

/// `atlas findings`'s payload (§7): `rebuild` recreates the index from
/// every eligible journal; otherwise this just lists the current index.
///
/// W-B disclosure repair: the estate index is a **derived disclosure
/// surface**, not a neutral listing — a settled row carries its proof
/// targets' exact source coordinates and an applied row carries the
/// changed source's alias, both generation points and the published
/// revision. It previously took no requester at all, so any caller that
/// reached the estate root read every one of them. It now answers one of
/// two named ways, exactly as `finding list` already did: scoped to a
/// `requester`'s own lineage and source grants, or explicitly `admin`.
/// `rebuild` mutates the index from every journal and is administrative
/// on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasFindingsPayload {
    #[serde(default)]
    pub rebuild: bool,
    #[serde(default)]
    pub requester: Option<WorkId>,
    #[serde(default)]
    pub admin: bool,
}

// ---- Reply -------------------------------------------------------------

/// One NDJSON-framed reply line: `{"ok": true, "result": {...}}` or
/// `{"ok": false, "error": {...}}` (transport.md §2). `untagged`: serde
/// tries `Ok` first (needs a `result` field), then `Err` (needs
/// `error`) — the two shapes are disjoint on the wire, so this never
/// picks the wrong one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reply {
    Ok { ok: bool, result: Value },
    Err { ok: bool, error: ErrorDetail },
}

impl Reply {
    /// `true` for `Reply::Ok`, `false` for `Reply::Err` — matches the
    /// envelope's own `ok` field, exposed so a caller need not match on
    /// the enum just to check success.
    pub fn is_ok(&self) -> bool {
        matches!(self, Reply::Ok { .. })
    }
}

/// The error reply's `error` object (transport.md §2): `code` names the
/// `ClaimRefusal` variant or another short string, `message` is a
/// human-readable line, `detail` is the bounded diagnostic text 0027
/// D92's `FailureCause.detail` also carries (issue 279) — optional,
/// omitted from the wire when absent rather than serialized as `null`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---- Pointer file (0022 D79) --------------------------------------------

/// `<estate_root>/.wirk/wirkd.json`, written atomically once the
/// listener is bound (transport.md §3) — read-only from this module's
/// side; W3 writes it, `client::locate` reads it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirkdPointer {
    pub schema: String,
    pub socket: PathBuf,
    pub pid: u32,
    pub protocol_version: u32,
}
