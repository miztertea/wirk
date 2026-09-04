//! wirk-herdr: the Herdr-pane executor (0001 D2; 0022 D78). Depends only
//! on wirk-core among internal crates (D7/D71's boundary test enforces
//! the edge). Types and the client trait per
//! `knowledge/work/p1-executor-design/orient/herdr.md` §1, built (W3,
//! `BRIEF.md` "Part B") against a synchronous `HerdrClient` — a live
//! socket implementation is item 4's, out of scope here.
//!
//! Contents: `Bearing`/`PaneBinding` (D51's terminal_id-keyed binding);
//! one request struct per used operation-map row (`GitWorktreeAdd` is
//! not a Herdr request — the executor's own wirk-side git call, D77);
//! `EventSubscription`/`HerdrEvent` (dotted vs underscored, D51's
//! matching pair); the info structs `PaneInfo`/`WorkspaceInfo`/
//! `WorktreeInfo`/`TabInfo` and `AgentStatus`, fields verbatim from
//! `herdr api schema --json` (protocol 20); `HerdrError`; the
//! `HerdrClient` trait; `HerdrExecutor`, implementing
//! `wirk_core::Executor`;
//! `PromptGate` (item 4's per-pane serialisation, D56, named here only).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

pub mod fake;
pub mod git;
pub mod run_loop;
pub mod socket;

pub use run_loop::{RunLoop, WirkdApi};
pub use socket::SocketClient;

// ---- Bearing / PaneBinding -------------------------------------------------
//
// herdr.md §1: terminal_id survives a pane move; pane_id does not
// (0017 D51). J3 on D51; R2, reused inside this crate's own
// Snapshot/Event types.

/// Where a pane currently lives. `pane_id` is Herdr's per-move identity
/// (0017 D51); `terminal_id` is the stable key a `PaneBinding` is kept
/// by across a `PaneMoved` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bearing {
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub terminal_id: String,
}

/// A wirk-side binding of a Run's pane, keyed by `terminal_id` so it
/// survives a Herdr-side move (`rebind`, below). "Vanished" means
/// `terminal_id` is absent from a fresh `session.snapshot` (D9 #5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBinding {
    pub terminal_id: String,
    pub bearing: Bearing,
}

// ---- Requests, one struct per used operation-map row -----------------------
//
// J3 (0017 decisions, per-row in orient/executor-herdr.md); R6/R7 per
// struct. Field names verbatim from `herdr api schema --json`'s
// `*Params` defs, per herdr.md §1.

/// Row 1, "Create a worktree": not a Herdr request — the executor's own
/// wirk-side `git worktree add` call (0018 D60, 0022 D77), kept as a
/// plain struct so `HerdrExecutor::launch` has one place to carry the
/// path/branch/base it will pass to that call (item 5's territory to
/// implement; D9 #6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeAdd {
    pub path: PathBuf,
    pub branch: String,
    pub base_sha: String,
}

/// Row 2, `worktree.open`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWorktree {
    pub path: PathBuf,
    pub workspace_id: Option<String>,
}

/// Row 3, `worktree.remove`; D54/D61 order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveWorktree {
    pub workspace_id: String,
    pub force: Option<bool>,
}

/// Row 4, `workspace.create`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkspace {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub label: Option<String>,
}

/// Row 5, `pane.split` — the execution triple is injected here (0001
/// D3, 0022 D73), in `env`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPane {
    pub workspace_id: Option<String>,
    pub target_pane_id: Option<String>,
    pub direction: SplitDirection,
    pub cwd: PathBuf,
    /// `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID` go here (0022
    /// D73; D9 #4's round-trip half).
    pub env: BTreeMap<String, String>,
}

/// Split direction for `SplitPane`. Wire values are `"right"`/`"down"`
/// — verbatim from the vendored schema's `event.$defs.SplitDirection`
/// (also `request`/`success_response`'s copies of the same def; all
/// three agree), *not* `"horizontal"`/`"vertical"`, which the live
/// server rejects outright (0028 tried step 2's live finding,
/// `knowledge/work/p1-herdr-executor/tried/RESULT.md`: every
/// `pane.split` call failed `invalid_request`, "unknown variant
/// `horizontal`, expected `right` or `down`"). `Right`/`Down` read as
/// well as the old `Horizontal`/`Vertical` names and need no explicit
/// `#[serde(rename)]` — `snake_case` already gives them the schema's
/// own spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

/// Row 7, `agent.start`; D52 surface-and-wait on blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartAgent {
    pub pane_id: String,
    pub kind: String,
    pub name: String,
    pub args: Vec<String>,
    pub timeout_ms: Option<u64>,
}

/// Row 8, `agent.prompt`; D56 per-pane serialisation (`PromptGate`,
/// below).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAgent {
    pub target: String,
    pub text: String,
}

/// Row 13, `agent.send_keys`, trust-block nudge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKeys {
    pub target: String,
    pub keys: Vec<String>,
}

/// Row 15, `release`/`clear_agent_authority`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAgent {
    pub pane_id: String,
    pub agent: String,
    pub source: Option<String>,
}

/// Row 17, `workspace.close`; D53 marks every pane gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseWorkspace {
    pub workspace_id: String,
}

/// Row 19, `session.snapshot`; rebuilds the `PaneBinding` set (D51).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub workspaces: Vec<Bearing>,
}

/// Row 20, `events.subscribe` — dotted names, D51's matching pair to
/// `HerdrEvent` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventSubscription {
    PaneAgentStatusChanged {
        pane_id: String,
    },
    /// Item 4, W2: the inactivity signal (loop.md §3) — a revision bump
    /// with no status change is still activity (output scrolling).
    /// Mirrors `PaneAgentStatusChanged`'s shape; `HerdrEvent::PaneUpdated`
    /// already existed with nothing able to subscribe to it.
    PaneUpdated {
        pane_id: String,
    },
    PaneOutputMatched {
        pane_id: String,
    },
    PaneScrollChanged {
        pane_id: String,
    },
    WorkspaceClosed,
    WorktreeRemoved,
    PaneCreated,
    PaneClosed,
    PaneExited,
    PaneFocused,
    PaneMoved,
    TabCreated,
    WorkspaceMetadataUpdated,
}

impl EventSubscription {
    /// The dotted wire form (e.g. `"pane.agent_status_changed"`), D51's
    /// matching pair to `HerdrEvent`'s underscored variant names.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventSubscription::PaneAgentStatusChanged { .. } => "pane.agent_status_changed",
            EventSubscription::PaneUpdated { .. } => "pane.updated",
            EventSubscription::PaneOutputMatched { .. } => "pane.output_matched",
            EventSubscription::PaneScrollChanged { .. } => "pane.scroll_changed",
            EventSubscription::WorkspaceClosed => "workspace.closed",
            EventSubscription::WorktreeRemoved => "worktree.removed",
            EventSubscription::PaneCreated => "pane.created",
            EventSubscription::PaneClosed => "pane.closed",
            EventSubscription::PaneExited => "pane.exited",
            EventSubscription::PaneFocused => "pane.focused",
            EventSubscription::PaneMoved => "pane.moved",
            EventSubscription::TabCreated => "tab.created",
            EventSubscription::WorkspaceMetadataUpdated => "workspace.metadata_updated",
        }
    }
}

impl std::fmt::Display for EventSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Row 22, `pane.report_agent_session`; D55 official pair, Herdr's copy
/// is cross-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportAgentSession {
    pub pane_id: String,
    pub source: String,
    pub agent: String,
    pub agent_session_id: Option<String>,
    pub session_start_source: Option<String>,
    pub seq: Option<u64>,
}

/// Row 24, `pane.report_agent`, hook path for non-Claude kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportAgent {
    pub pane_id: String,
    pub source: String,
    pub agent: String,
    pub state: String,
    pub seq: Option<u64>,
}

/// Row 25, `workspace`/`pane.report_metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportMetadata {
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub source: String,
    pub tokens: Option<serde_json::Value>,
    pub title: Option<String>,
}

/// Row 26, `notification.show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notify {
    pub title: String,
    pub body: String,
}

/// Row 27, `pane.focus`/`agent.focus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusPane {
    pub pane_id: String,
}

// ---- Info structs -----------------------------------------------------------
//
// Fields per `herdr api schema --json` (protocol 20)'s
// `event.$defs.{PaneInfo,WorkspaceInfo,WorktreeInfo,TabInfo}`: every
// field the schema's own `required` array does not name is `Option`;
// nested object fields whose own type is out of this item's scope
// (`AgentSessionInfo`, `PaneScrollInfo`, `WorkspaceWorktreeInfo` — no
// P1 caller needs them typed, R1) are carried as `Option<serde_json::Value>`
// rather than modeled fully.

/// `event.$defs.PaneInfo`. Required: `pane_id`, `terminal_id`,
/// `workspace_id`, `tab_id`, `focused`, `agent_status`, `revision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    pub agent_status: AgentStatus,
    pub revision: u64,
    pub agent: Option<String>,
    pub agent_session: Option<serde_json::Value>,
    pub cwd: Option<String>,
    pub display_agent: Option<String>,
    pub foreground_cwd: Option<String>,
    pub label: Option<String>,
    pub scroll: Option<serde_json::Value>,
    pub state_labels: Option<BTreeMap<String, String>>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub title: Option<String>,
    pub tokens: Option<BTreeMap<String, String>>,
}

/// `event.$defs.WorkspaceInfo`. Required: `workspace_id`, `number`,
/// `label`, `focused`, `pane_count`, `tab_count`, `active_tab_id`,
/// `agent_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub pane_count: u32,
    pub tab_count: u32,
    pub active_tab_id: String,
    pub agent_status: AgentStatus,
    pub tokens: Option<BTreeMap<String, String>>,
    pub worktree: Option<serde_json::Value>,
}

/// `event.$defs.WorktreeInfo`. Required: `path`, `is_bare`,
/// `is_detached`, `is_prunable`, `is_linked_worktree`, `label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_prunable: bool,
    pub is_linked_worktree: bool,
    pub label: String,
    pub branch: Option<String>,
    pub open_workspace_id: Option<String>,
}

/// `event.$defs.TabInfo`. Required: `tab_id`, `workspace_id`, `number`,
/// `label`, `focused`, `pane_count`, `agent_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub pane_count: u32,
    pub agent_status: AgentStatus,
}

/// `event.$defs.AgentStatus`: `["idle", "working", "blocked", "done",
/// "unknown"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

// ---- Events the executor reacts to ------------------------------------------
//
// Variant names are `EventData.type` in underscored form (schema
// `event.$defs.EventData.oneOf`); `EventSubscription` above uses dotted
// form — this pairing IS D51's dotted/underscore match. This enum is a
// subset of the schema's `EventData.oneOf` variants — only the ones the
// executor reacts to (herdr.md §1); the schema also defines
// `workspace_updated`, `workspace_renamed`, `workspace_moved`,
// `workspace_reordered`, `worktree_created`, `tab_closed`,
// `tab_renamed`, `tab_moved`, `pane_output_changed`, and
// `layout_updated`, none modelled here (R1). Field sets and
// required/optional split verified field-for-field against
// `tests/fixtures/herdr-schema-0.8.2-p20.json` by
// `tests/schema.rs` — read that fixture, not this comment, for the
// authority (0017 D51; issue 223). `#[serde(tag = "type", rename_all =
// "snake_case")]` gives each variant a self-describing JSON shape,
// which `event_identity` below hashes for the non-pane variants.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HerdrEvent {
    WorkspaceCreated {
        workspace: WorkspaceInfo,
    },
    WorkspaceClosed {
        workspace_id: String,
        workspace: Option<WorkspaceInfo>,
    },
    WorkspaceMetadataUpdated {
        workspace: WorkspaceInfo,
    },
    WorkspaceFocused {
        workspace_id: String,
    },
    WorktreeOpened {
        already_open: bool,
        workspace: WorkspaceInfo,
        worktree: WorktreeInfo,
    },
    WorktreeRemoved {
        forced: bool,
        workspace_id: String,
        workspace: Option<WorkspaceInfo>,
        worktree: WorktreeInfo,
    },
    TabCreated {
        tab: TabInfo,
    },
    TabFocused {
        tab_id: String,
        workspace_id: String,
    },
    PaneCreated {
        pane: PaneInfo,
    },
    PaneUpdated {
        pane: PaneInfo,
    },
    PaneClosed {
        pane_id: String,
        workspace_id: String,
    },
    PaneFocused {
        pane_id: String,
        workspace_id: String,
    },
    PaneMoved {
        pane: Box<PaneInfo>,
        previous_pane_id: String,
        previous_workspace_id: String,
        previous_tab_id: String,
        closed_tab_id: Option<String>,
        closed_workspace_id: Option<String>,
        created_tab: Option<TabInfo>,
        created_workspace: Option<WorkspaceInfo>,
    },
    PaneExited {
        pane_id: String,
        workspace_id: String,
    },
    PaneAgentDetected {
        pane_id: String,
        workspace_id: String,
        agent: Option<String>,
        final_status: Option<AgentStatus>,
        released: Option<bool>,
    },
    PaneAgentStatusChanged {
        pane_id: String,
        workspace_id: String,
        agent: Option<String>,
        agent_status: AgentStatus,
        display_agent: Option<String>,
        state_labels: Option<BTreeMap<String, String>>,
        title: Option<String>,
    },
}

// ---- Errors -------------------------------------------------------------

/// `NotFound` covers `pane_not_found`/`agent_not_found`/
/// `workspace_not_found`; `Blocked` covers `agent_not_ready` (D52);
/// `Invalid` is every other well-formed `{"error":{code,message}}`
/// business reply (`invalid_request` foremost — a schema-rejected
/// request, per the tried step's live finding) — a business error the
/// server *did* parse and reply to, distinct from `Transport`, which
/// is reserved for socket/io/framing failures where no reply (or no
/// parseable one) came back at all (fix 2, 0028 tried step 2's second
/// finding: an `Invalid` reply was previously misreported as a
/// `Transport` id mismatch, because `SocketClient::call` checked the
/// reply id before the `error` field — see `socket.rs::call`).
#[derive(Debug, Clone, Error)]
pub enum HerdrError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("blocked: {0}")]
    Blocked(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("transport: {0}")]
    Transport(String),
}

// ---- HerdrClient trait --------------------------------------------------
//
// Sync, per R5's reference on the box: sergeant's `Backend` trait
// stayed sync because "the only M3 implementation is in-process, and a
// dyn-compatible async trait would need a boxing dependency for no
// measured benefit" (refs/sergeant-rs/src/backend/mod.rs:869-873);
// reused, its five verbs and PTY hosting dropped (0023 D83).

pub trait HerdrClient: Send + Sync {
    fn create_workspace(&self, req: CreateWorkspace) -> Result<WorkspaceInfo, HerdrError>;
    fn split_pane(&self, req: SplitPane) -> Result<PaneInfo, HerdrError>;
    fn open_worktree(&self, req: OpenWorktree) -> Result<WorktreeInfo, HerdrError>;
    fn remove_worktree(&self, req: RemoveWorktree) -> Result<(), HerdrError>;
    fn send_input(&self, pane_id: &str, text: &str) -> Result<(), HerdrError>;
    fn start_agent(&self, req: StartAgent) -> Result<(), HerdrError>;
    fn prompt_agent(&self, req: PromptAgent) -> Result<(), HerdrError>;
    /// `timeout_ms` (here and on `StartAgent`) is a transport bound
    /// only: never treated as completion (0017 D56) and never treated
    /// as blocked detection (issue 274, item 4's brief — inactivity,
    /// not wall-clock, is item 4's design). Used only to wait for
    /// "working" before the next prompt (D56).
    fn wait_agent(
        &self,
        target: &str,
        until: AgentStatus,
        timeout_ms: u64,
    ) -> Result<AgentStatus, HerdrError>;
    fn get_pane(&self, pane_id: &str) -> Result<PaneInfo, HerdrError>;
    fn get_agent(&self, target: &str) -> Result<PaneInfo, HerdrError>;
    fn list_agents(&self) -> Result<Vec<PaneInfo>, HerdrError>;
    fn send_keys(&self, req: SendKeys) -> Result<(), HerdrError>;
    fn release_agent(&self, req: ReleaseAgent) -> Result<(), HerdrError>;
    fn close_pane(&self, pane_id: &str) -> Result<(), HerdrError>;
    fn close_workspace(&self, req: CloseWorkspace) -> Result<(), HerdrError>;
    fn snapshot(&self) -> Result<Snapshot, HerdrError>;
    fn report_agent_session(&self, req: ReportAgentSession) -> Result<(), HerdrError>;
    fn report_agent(&self, req: ReportAgent) -> Result<(), HerdrError>;
    fn report_metadata(&self, req: ReportMetadata) -> Result<(), HerdrError>;
    fn notify(&self, req: Notify) -> Result<(), HerdrError>;
    fn focus_pane(&self, req: FocusPane) -> Result<(), HerdrError>;
    /// Row 20: subscribe, hand back raw events; dedup-by-identity lives
    /// above this trait in `Reconciler`, not inside the client — a fake
    /// can replay a fixed `Vec<HerdrEvent>` with no dedup of its own.
    fn subscribe(
        &self,
        subs: Vec<EventSubscription>,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>, HerdrError>;
}

/// So a test can hold an `Arc<FakeHerdrClient>` (mutating its recorded
/// responses concurrently with a `RunLoop` driving on another thread)
/// and still satisfy `RunLoop`'s `C: HerdrClient` bound directly — the
/// same move `run_loop.rs` already makes for `Arc<T: WirkdApi>`.
impl<T: HerdrClient + ?Sized> HerdrClient for std::sync::Arc<T> {
    fn create_workspace(&self, req: CreateWorkspace) -> Result<WorkspaceInfo, HerdrError> {
        (**self).create_workspace(req)
    }
    fn split_pane(&self, req: SplitPane) -> Result<PaneInfo, HerdrError> {
        (**self).split_pane(req)
    }
    fn open_worktree(&self, req: OpenWorktree) -> Result<WorktreeInfo, HerdrError> {
        (**self).open_worktree(req)
    }
    fn remove_worktree(&self, req: RemoveWorktree) -> Result<(), HerdrError> {
        (**self).remove_worktree(req)
    }
    fn send_input(&self, pane_id: &str, text: &str) -> Result<(), HerdrError> {
        (**self).send_input(pane_id, text)
    }
    fn start_agent(&self, req: StartAgent) -> Result<(), HerdrError> {
        (**self).start_agent(req)
    }
    fn prompt_agent(&self, req: PromptAgent) -> Result<(), HerdrError> {
        (**self).prompt_agent(req)
    }
    fn wait_agent(
        &self,
        target: &str,
        until: AgentStatus,
        timeout_ms: u64,
    ) -> Result<AgentStatus, HerdrError> {
        (**self).wait_agent(target, until, timeout_ms)
    }
    fn get_pane(&self, pane_id: &str) -> Result<PaneInfo, HerdrError> {
        (**self).get_pane(pane_id)
    }
    fn get_agent(&self, target: &str) -> Result<PaneInfo, HerdrError> {
        (**self).get_agent(target)
    }
    fn list_agents(&self) -> Result<Vec<PaneInfo>, HerdrError> {
        (**self).list_agents()
    }
    fn send_keys(&self, req: SendKeys) -> Result<(), HerdrError> {
        (**self).send_keys(req)
    }
    fn release_agent(&self, req: ReleaseAgent) -> Result<(), HerdrError> {
        (**self).release_agent(req)
    }
    fn close_pane(&self, pane_id: &str) -> Result<(), HerdrError> {
        (**self).close_pane(pane_id)
    }
    fn close_workspace(&self, req: CloseWorkspace) -> Result<(), HerdrError> {
        (**self).close_workspace(req)
    }
    fn snapshot(&self) -> Result<Snapshot, HerdrError> {
        (**self).snapshot()
    }
    fn report_agent_session(&self, req: ReportAgentSession) -> Result<(), HerdrError> {
        (**self).report_agent_session(req)
    }
    fn report_agent(&self, req: ReportAgent) -> Result<(), HerdrError> {
        (**self).report_agent(req)
    }
    fn report_metadata(&self, req: ReportMetadata) -> Result<(), HerdrError> {
        (**self).report_metadata(req)
    }
    fn notify(&self, req: Notify) -> Result<(), HerdrError> {
        (**self).notify(req)
    }
    fn focus_pane(&self, req: FocusPane) -> Result<(), HerdrError> {
        (**self).focus_pane(req)
    }
    fn subscribe(
        &self,
        subs: Vec<EventSubscription>,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>, HerdrError> {
        (**self).subscribe(subs)
    }
}

// ---- PromptGate -----------------------------------------------------------

/// Per-pane prompt serialisation (0017 D56: one prompt in flight per
/// pane at a time). Named here per W3's scope; the gating logic itself
/// — waiting on `busy` before sending the next `PromptAgent` — is item
/// 4's, once a live client exists to wait against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptGate {
    pub busy: bool,
}

impl PromptGate {
    /// Attempts to acquire the gate for sending a prompt now (0017 D56:
    /// one prompt in flight per pane at a time — concurrent prompts
    /// concatenate into one input line). Returns `false` and leaves the
    /// gate untouched when already busy; a caller that gets `false`
    /// must not send.
    pub fn try_acquire(&mut self) -> bool {
        if self.busy {
            false
        } else {
            self.busy = true;
            true
        }
    }

    /// A `working` status observed on the gated pane releases the gate
    /// for the next send (D56: "waits for `working` before sending
    /// another"). Any other status leaves `busy` as it is — `blocked`,
    /// `idle`, and `done` are not "the prompt was received and the
    /// agent has moved on", only `working` is.
    pub fn release_on_working(&mut self, status: AgentStatus) {
        if matches!(status, AgentStatus::Working) {
            self.busy = false;
        }
    }
}

// ---- HerdrExecutor --------------------------------------------------------

/// Implements `wirk_core::Executor` against a `HerdrClient` (0001 D2,
/// D4; 0022 D78). `launch` creates the workspace (no existing pane) or
/// splits a pane (an existing one) with the triple injected in env from
/// `run` and `world`, subscribes to `pane.agent_status_changed` for
/// that pane before `start_agent`, then calls `start_agent`. `poll`
/// reads `get_pane` and maps `NotFound` to `Vanished`; a blocked status
/// is still `Running` (D52: surface and wait, no completion signal
/// through this trait).
pub struct HerdrExecutor<C: HerdrClient> {
    client: C,
}

/// What `HerdrExecutor::launch_actor` hands back: the actor's pane, and
/// the **one** subscription opened for it — opened before `agent.start`
/// (0017 D51/D52: no early transition is missed) and handed to the
/// caller that will drain it, rather than opened and dropped.
///
/// Fix 3 (0028 tried step 3): `launch` used to open a subscription it
/// immediately discarded, and `RunLoop::drive` then opened a second one
/// of its own — two connections for one pane, the second built from
/// `run.id` rather than the pane id it never saw. Handing the live
/// subscription out is what makes one subscription enough.
pub struct LaunchedRun {
    pub pane: PaneInfo,
    pub events: Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>,
}

impl std::fmt::Debug for LaunchedRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchedRun")
            .field("pane", &self.pane)
            .finish_non_exhaustive()
    }
}

impl<C: HerdrClient> HerdrExecutor<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &C {
        &self.client
    }

    /// The full actor launch: the pane (created or split), then **one**
    /// `events.subscribe` for that pane's `pane_id`, then
    /// `agent.start` — in that order, so D51's subscribe-before-start
    /// holds with exactly one subscription, which is returned live for
    /// the loop to drain (`LaunchedRun`).
    ///
    /// This, not `Executor::launch`, is the path `RunLoop` takes. The
    /// trait row cannot return the subscription (its signature is
    /// `Result<(), Self::Error>`), and a subscription opened only to be
    /// dropped catches nothing while costing a connection — so the
    /// trait row does not open one at all.
    pub fn launch_actor(
        &self,
        run: &wirk_core::Run,
        world: &wirk_core::World,
    ) -> Result<LaunchedRun, HerdrExecutorError> {
        let pane = self.actor_pane(run, world)?;

        // Subscribe to this pane's status changes and revision bumps
        // before starting the agent, so no early transition is missed
        // (D51/D52) and the inactivity signal (loop.md §3) is live from
        // the start. `pane.pane_id` is Herdr's own pane id, the only
        // thing `pane.agent_status_changed`/`pane.updated` accept: the
        // server probes it with an internal `pane.get` when it builds
        // the subscription (`refs/herdr` `0f8ad12`
        // `src/api/subscriptions.rs:207`), and `pane.get` parses a
        // structured pane id and nothing else
        // (`src/app/api/panes.rs:159-168`, `parse_pane_id`) — an agent
        // name such as `run.id` fails it `pane_not_found`.
        let events = self.client.subscribe(vec![
            EventSubscription::PaneAgentStatusChanged {
                pane_id: pane.pane_id.clone(),
            },
            EventSubscription::PaneUpdated {
                pane_id: pane.pane_id.clone(),
            },
        ])?;

        self.start_actor_agent(run, &pane.pane_id)?;

        Ok(LaunchedRun { pane, events })
    }

    /// The actor's pane: reuse-and-split when one exists for this Run,
    /// create a workspace and split otherwise. No subscription, no
    /// agent — shared by `launch_actor` and the `Executor::launch`
    /// trait row.
    fn actor_pane(
        &self,
        run: &wirk_core::Run,
        world: &wirk_core::World,
    ) -> Result<PaneInfo, HerdrExecutorError> {
        let actor = match world {
            wirk_core::World::Actor(actor) => actor,
            wirk_core::World::Deterministic(_) => {
                return Err(HerdrExecutorError::NotDeterministicKind);
            }
        };

        let mut env = BTreeMap::new();
        env.insert(
            "WIRK_ESTATE_ROOT".to_string(),
            actor.triple.estate_root.clone(),
        );
        env.insert("WIRK_WORK_ID".to_string(), actor.triple.work_id.0.clone());
        env.insert("WIRK_RUN_ID".to_string(), actor.triple.run_id.0.clone());

        // Workspace-vs-pane branching (item 4, W2; loop.md §2, build
        // brief §2.2 row 4: "CreateWorkspace{cwd,env} (no open
        // workspace) or SplitPane{...} (one exists)"). `ActorWorld`
        // itself carries no workspace identity (it is compiled once at
        // reservation, before any Herdr call is made, world.md §1), so
        // "does a workspace already exist for this Run" is answered the
        // same way `poll` answers "is this Run's pane still there": by
        // asking Herdr for the pane `start_agent` would have named
        // `run.id.0` (this executor's own convention, matching `poll`
        // below). Found -> reuse that pane's workspace, splitting a
        // fresh pane inside it. Not found (a first launch, or Herdr's
        // own state was lost) -> create a workspace explicitly, so the
        // triple lands in workspace-level env too ("workspace env
        // reaches the first pane", 0017 spike r2 `21-workspace-
        // create.log`); if that explicit call itself fails (offline
        // fakes with nothing configured; a real Herdr that rejects it
        // for a reason `split_pane`'s own auto-create tolerates), fall
        // through to `split_pane(workspace_id: None)` — Herdr already
        // creates a workspace as a side effect of that call today (this
        // file's prior behavior; 0017 spike: "Connecting the CLI with
        // `--cwd` creates a workspace before any explicit call").
        let existing = self.client.get_pane(&run.id.0).ok();
        let pane = match existing {
            Some(pane) => self.client.split_pane(SplitPane {
                workspace_id: Some(pane.workspace_id),
                target_pane_id: Some(pane.pane_id),
                // `Down`: the actor's pane appears below the existing
                // one, matching the old hardcoded `Vertical`'s intent
                // (a vertical stack) now expressed in the schema's own
                // `right`/`down` vocabulary — which of the two is a
                // design call the tried step's RESULT.md parked, not
                // resolved elsewhere; kept as one hardcoded value here,
                // same as before (J1, local/reversible).
                direction: SplitDirection::Down,
                cwd: actor.worktree_path.clone(),
                env: env.clone(),
            })?,
            None => {
                let workspace_id = self
                    .client
                    .create_workspace(CreateWorkspace {
                        cwd: actor.worktree_path.clone(),
                        env: env.clone(),
                        label: None,
                    })
                    .ok()
                    .map(|w| w.workspace_id);
                self.client.split_pane(SplitPane {
                    workspace_id,
                    target_pane_id: None,
                    // Same `Down` as the branch above.
                    direction: SplitDirection::Down,
                    cwd: actor.worktree_path.clone(),
                    env,
                })?
            }
        };
        Ok(pane)
    }

    /// `agent.start` on the actor's pane, named by `run.id` — the name
    /// every later `agent.*` call targets (`agent.prompt`,
    /// `agent.send_keys`: confirmed live, `tried/RESULT.md` run 3,
    /// 04-blocked).
    fn start_actor_agent(
        &self,
        run: &wirk_core::Run,
        pane_id: &str,
    ) -> Result<(), HerdrExecutorError> {
        // W1 (0041 D129): `run.kind`, not a hardcoded `"claude"` — the
        // opencode row starts with its own configured default model
        // (`hecate/qwen3.8-27b-udiq3s-mtp`, orient/actor.md §5, passed
        // explicitly the first live run rather than relying on
        // opencode's own bare-args default).
        let (kind_str, args) = match run.kind {
            wirk_core::ActorKind::Claude => {
                ("claude", vec!["--model".to_string(), "sonnet".to_string()])
            }
            wirk_core::ActorKind::Opencode => (
                "opencode",
                vec![
                    "--model".to_string(),
                    "hecate/qwen3.8-27b-udiq3s-mtp".to_string(),
                ],
            ),
        };
        self.client.start_agent(StartAgent {
            pane_id: pane_id.to_string(),
            kind: kind_str.to_string(),
            name: run.id.0.clone(),
            args,
            timeout_ms: None,
        })?;
        Ok(())
    }

    /// `Executor::poll`'s body against an explicit pane id. `pane.get`
    /// takes a structured pane id and nothing else
    /// (`refs/herdr` `0f8ad12` `src/app/api/panes.rs:159-168`), so a
    /// caller holding the pane `launch_actor` returned
    /// (`RunLoop::poll_vanished`) asks by that, not by the agent name
    /// the trait row has to fall back on.
    pub fn poll_pane(
        &self,
        pane_id: &str,
    ) -> Result<wirk_core::RunObservation, HerdrExecutorError> {
        match self.client.get_pane(pane_id) {
            Ok(pane) => match pane.agent_status {
                // A blocked status is still Running (D52: surface and
                // wait; no completion signal through this trait).
                AgentStatus::Idle
                | AgentStatus::Working
                | AgentStatus::Blocked
                | AgentStatus::Done
                | AgentStatus::Unknown => Ok(wirk_core::RunObservation::Running),
            },
            Err(HerdrError::NotFound(_)) => Ok(wirk_core::RunObservation::Vanished),
            Err(other) => Err(other.into()),
        }
    }
}

/// `HerdrExecutor::launch`/`poll` error surface. `NotDeterministicKind`
/// is this executor's own boundary check: a `World::Deterministic`
/// names a different `WaypointKind`, run by the `wirk` bin's own
/// deterministic executor (0022 D78), not this one.
#[derive(Debug, Error)]
pub enum HerdrExecutorError {
    #[error(transparent)]
    Herdr(#[from] HerdrError),
    #[error(
        "HerdrExecutor cannot launch a Deterministic world: not this executor's kind (0022 D78)"
    )]
    NotDeterministicKind,
}

impl<C: HerdrClient> wirk_core::Executor for HerdrExecutor<C> {
    type Error = HerdrExecutorError;

    /// The generic `Executor` row: the actor's pane, then
    /// `agent.start`. It opens **no** subscription — the row cannot
    /// hand one back, and a subscription opened only to be dropped
    /// catches nothing (fix 3; before it, `launch` opened exactly such
    /// a throwaway and `RunLoop::drive` opened a second, differently
    /// addressed one). Any caller that needs the pane's events calls
    /// `HerdrExecutor::launch_actor`, which opens one subscription
    /// before `agent.start` per D51 and returns it.
    fn launch(&self, run: &wirk_core::Run, world: &wirk_core::World) -> Result<(), Self::Error> {
        let pane = self.actor_pane(run, world)?;
        self.start_actor_agent(run, &pane.pane_id)
    }

    fn poll(&self, run: &wirk_core::Run) -> Result<wirk_core::RunObservation, Self::Error> {
        // `run.waypoint`/`run.id` do not directly carry a pane_id in
        // this item's scope (the binding lives in a `Reconciler` the
        // caller owns, per D51), so this row asks by the name
        // `StartAgent.name` was given. `pane.get` resolves structured
        // pane ids only (`refs/herdr` `0f8ad12`
        // `src/app/api/panes.rs:159-168`), so a caller that holds the
        // real pane id should use `poll_pane` instead — `RunLoop` does,
        // once `launch_actor` has given it one.
        self.poll_pane(&run.id.0)
    }
}
