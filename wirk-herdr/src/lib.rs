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
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod claim_hook;
pub mod fake;
pub mod git;
pub mod run_loop;
pub mod socket;
pub mod worker_contract;

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
///
/// `panes` (P4.5 first increment, ruling 0203): the same reply's own
/// `panes` list, kept in full rather than reduced to `Bearing`. The
/// wire reply already names one `PaneInfo` per pane in the session —
/// every pane, not only one Herdr also tracks as an agent (`agent:
/// None` for a plain shell) — and `PaneInfo` already carries `cwd`/
/// `foreground_cwd`. Discarding those two fields down to `Bearing`
/// (identity alone) is what left `wirk work clean`'s ownership check
/// unable to see a plain shell `cd`'d into a Work's checkout at all
/// (QUALIFIED.md "Unresolved limits"): `agent.list` only ever lists a
/// pane with a registered agent, so a plain shell was invisible to
/// every ownership check this client could make. No new wire method —
/// `session.snapshot` already answers this; only the Rust type was
/// throwing the answer away.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub workspaces: Vec<Bearing>,
    pub panes: Vec<PaneInfo>,
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
    /// `AgentInfo`'s own field (`agent.list`/`agent.get`'s reply type,
    /// same required-field set as `PaneInfo` but not the same optional
    /// fields — confirmed against both the vendored p20 fixture and a
    /// live protocol-22 schema): the name `agent.start{name}` gave this
    /// pane's agent, i.e. the Run id (`start_actor_agent`'s own doc:
    /// "the live agent's name is the Run id"). `None` for an ordinary
    /// `pane.get`/`session.snapshot` `PaneInfo` reply, which never
    /// carries this property at all — reused here rather than a
    /// separate `AgentInfo` type (R1/R2: identical required-field set,
    /// one struct, one `agent`-vs-`name` distinction to remember, not
    /// two near-duplicate types). P4.5 first increment (ruling 0203):
    /// this is the field `wirk work clean`'s own ownership check
    /// actually needs to match a live agent to its Run — `agent` alone
    /// (the harness kind, e.g. `"opencode"`) never identifies *which*
    /// Run's agent a pane holds.
    pub name: Option<String>,
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

/// `success_response.$defs.PaneProcessInfo` (`pane.process_info`,
/// protocol 22): the box's own actual process table for one pane, not
/// the terminal's cached idea of its `cwd` (`PaneInfo.cwd`/
/// `foreground_cwd`, which is Herdr's own last-observed value). P4.5
/// first increment (ruling 0203, QUALIFIED.md "Unresolved limits"):
/// this is the "pane.process_info (foreground_processes[].cwd)" the
/// qualified design named as the one thing that would close the
/// plain-shell ownership gap and left unimplemented ("has no product
/// client"). Only `pane_id` is required on the wire; every other field
/// is absent when the pane's process table could not be read (the pane
/// closed between listing and querying it, or the platform does not
/// expose it) — `None`/empty is "unknown", never "no process".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneProcessInfo {
    pub pane_id: String,
    pub shell_pid: Option<u32>,
    pub tty: Option<String>,
    pub foreground_process_group_id: Option<u32>,
    #[serde(default)]
    pub foreground_processes: Vec<PaneProcessInfoProcess>,
}

/// One process in `PaneProcessInfo.foreground_processes` — real fields
/// off the box's own process table (`pid`/`name` required; the rest
/// `None` where unreadable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneProcessInfoProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
    pub cwd: Option<String>,
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
    /// P3 native launch selection (PREPARATION-ADJUDICATION.md point 3):
    /// returns Herdr's own `agent_started.argv` — what Herdr says it
    /// submitted to the shell (`refs/herdr` `0f8ad12`
    /// `src/app/agents.rs::start_agent`: `argv = [executable] + args`),
    /// launch **submission** evidence, never proof a provider actually
    /// served the requested model. Previously discarded
    /// (`Result<(), HerdrError>`) — the preparation report's own named
    /// finding ("Herdr already accepts opaque argv and returns launch
    /// argv, which Wirk discards").
    fn start_agent(&self, req: StartAgent) -> Result<Vec<String>, HerdrError>;
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
    /// Row 23 (map.md:40): available on the wire (`pane.read`,
    /// vendored fixture), previously unused — 0017 D57 kept wirk off it
    /// for *Claim evidence* ("evidence is what the actor writes to
    /// files plus its Claim"). Ruling 0052 D156 (P2.6 W2) needs the
    /// pane's last screen lines on a `Blocked` Work's `NeedsInput`
    /// cause: a human-facing surface, not Claim evidence, so D57's
    /// scope does not bar this call — R2/R5: the capability already
    /// exists in the protocol, wrapped the same way every other verb
    /// on this trait is. `source: "visible"` (the currently-onscreen
    /// text, not full scrollback); returns the pane's text as-is, one
    /// line per screen row.
    fn read_pane(&self, pane_id: &str) -> Result<String, HerdrError>;
    /// `pane.process_info` (protocol 22; P4.5 first increment, ruling
    /// 0203): the box's own live process table for one pane — real
    /// terminal/pane process identity, not Herdr's cached `PaneInfo`
    /// fields. `wirk work clean`'s ownership check uses this as the
    /// authoritative confirmation once `snapshot()`'s `cwd`/
    /// `foreground_cwd` has already narrowed to a candidate pane: a
    /// cached path can be stale by one command; the process table
    /// cannot.
    fn pane_process_info(&self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError>;
    /// Row 20: subscribe, hand back raw events; dedup-by-identity lives
    /// above this trait in `Reconciler`, not inside the client — a fake
    /// can replay a fixed `Vec<HerdrEvent>` with no dedup of its own.
    fn subscribe(
        &self,
        subs: Vec<EventSubscription>,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>, HerdrError>;
    /// P3 native launch attempt admission: *which Herdr* this client
    /// talks to, as one stable string wirkd can bind a Run's launch to
    /// (`EventKind::RunLaunchAttempted.destination`). Not a wire verb —
    /// it asks the client about itself, which is why it has no row in
    /// the protocol map. A Run's launch is bound to the first
    /// destination that attempted it, so a recovery invocation pointed
    /// at a *different* Herdr is refused rather than allowed to turn an
    /// uncertain launch into a second real one somewhere the first
    /// agent is neither visible nor name-colliding.
    fn destination(&self) -> String;
}

/// How long `probe_opencode_pane_config` will wait, in total, for a
/// probe pane to answer. Generous against a measured answer of ~50ms on
/// a live 0.9.0 server (2026-09-12), and bounded because a launch that
/// cannot learn its own configuration must disclose that rather than
/// hang: the deadline is the only thing standing between a wedged pane
/// shell and a launch that never starts.
const OPENCODE_ENV_PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);
/// How long one sent command is given before it is sent again — a
/// freshly split pane may still be starting its shell and drop the
/// first line.
const OPENCODE_ENV_PROBE_RESEND: std::time::Duration = std::time::Duration::from_millis(750);
/// How often the completion marker is checked while waiting.
const OPENCODE_ENV_PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(20);

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
    fn start_agent(&self, req: StartAgent) -> Result<Vec<String>, HerdrError> {
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
    fn destination(&self) -> String {
        (**self).destination()
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
    fn read_pane(&self, pane_id: &str) -> Result<String, HerdrError> {
        (**self).read_pane(pane_id)
    }
    fn pane_process_info(&self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
        (**self).pane_process_info(pane_id)
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
    /// P4.1: how a codex launch asks codex itself whether adding the
    /// worker contract to `developer_instructions` would displace a
    /// value already configured (`worker_contract::codex_composition`).
    /// A field, defaulted to the real CLI probe, so a test can pin the
    /// launch path's behaviour for each answer without an installed
    /// codex and without the launch path ever guessing.
    codex_probe: std::sync::Arc<dyn worker_contract::CodexProbe>,
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
    /// P3 native launch selection: Herdr's own `agent_started.argv` for
    /// this launch (`HerdrClient::start_agent`'s own doc) — carried out
    /// here so `RunLoop::launch` can journal it distinctly from
    /// `run.selection` (what was requested).
    pub argv: Vec<String>,
    /// P4.1: how this launch actually delivered the reserved worker
    /// contract, carried out so `RunLoop::launch` can journal it on
    /// `RunLaunched` beside Herdr's own argv — the one delivery fact
    /// argv alone cannot reconstruct, since opencode's mechanism is an
    /// environment variable and the fallback is prompt text.
    ///
    /// `None` when this Run's World carries no contract at all (every
    /// World reserved before this wave).
    pub contract: Option<wirk_core::ContractDelivery>,
    /// P4.1 (ruling 0208): whether this launch actually installed wirk's
    /// own Claim-filing hook in the actor's pane, carried out for the
    /// same reason `contract` is — the standing prompt must tell the
    /// actor the truth about its own turn end, and neither argv nor the
    /// pane's environment says whether the hook reached it.
    ///
    /// `None` only for a launch path that records nothing (the
    /// `Executor::launch` trait row); every `launch_actor` returns an
    /// answer.
    pub claim_hook: Option<wirk_core::ClaimHookDelivery>,
}

impl std::fmt::Debug for LaunchedRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchedRun")
            .field("pane", &self.pane)
            .finish_non_exhaustive()
    }
}

/// True when `err` is Herdr's pane-busy refusal on `agent.start`, wire
/// code `agent_pane_busy` (`refs/herdr/src/app/agents.rs:254-255`).
/// `HerdrError` has no dedicated variant for it: `SocketClient::
/// map_error`'s only named business code is `agent_not_ready` (to
/// `Blocked`, D52); every other code, `agent_pane_busy` included, falls
/// to its catch-all `HerdrError::Invalid(format!("{other}: {message}"))`
/// (`socket.rs:564-572`) — so the check is the message's own prefix,
/// not a variant match.
fn is_agent_pane_busy(err: &HerdrExecutorError) -> bool {
    matches!(
        err,
        HerdrExecutorError::Herdr(HerdrError::Invalid(msg)) if msg.starts_with("agent_pane_busy")
    )
}

/// Why a Run's own pinned runtime could not be established. Every
/// variant is a refusal, never a degrade: `actor_pane` surfaces it as
/// `HerdrExecutorError::RuntimePin` *before* any pane is created, so an
/// actor never launches under a weaker `wirk` guarantee than the one
/// its Run was promised (P3 execution-recovery correction item 1 —
/// "refuse an unfulfilled required pin before launching rather than
/// changing capability silently or merely logging").
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimePinError {
    /// `std::env::current_exe` gave nothing to pin.
    #[error(
        "this driver cannot read its own executable path (current_exe): there is nothing to pin \
         as this Run's own `wirk`"
    )]
    NoCurrentExe,
    /// A filesystem step (read, install, link, verify) failed.
    #[error("{step} at {path} failed: {reason}")]
    Io {
        step: String,
        path: String,
        reason: String,
    },
    /// This Run already has a pinned runtime whose bytes no longer match
    /// the digest recorded when it was pinned, and the image it was
    /// bound to is gone too. Replacing it with the *current* driver's
    /// bytes would silently change this Run's runtime identity mid-Run,
    /// so the launch is refused instead.
    #[error(
        "this Run's pinned runtime at {path} no longer matches its recorded digest {expected} \
         (found {found}) and image {image} is not available to restore it — refusing to \
         re-pin a different binary into a Run already bound to one"
    )]
    Unrestorable {
        path: String,
        expected: String,
        found: String,
        image: String,
    },
}

/// SHA-256 of a file's bytes, lowercase hex. R5: `sha2`, already this
/// crate's own declared dependency, used the ordinary way and in the
/// same shape `wirkd::server`'s own `sha256_hex` uses it.
fn sha256_file(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    // A fixed buffer rather than reading the whole binary into memory:
    // these are tens of megabytes of debug binary, hashed on an actor
    // launch path.
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn pin_io(step: &str, path: &Path, err: &io::Error) -> RuntimePinError {
    RuntimePinError::Io {
        step: step.to_string(),
        path: path.display().to_string(),
        reason: err.to_string(),
    }
}

/// Install `exe`'s bytes, once, as a shared immutable image at
/// `<estate_root>/.wirk/runtime/images/<sha256>/wirk`, and return that
/// path. Content-addressed, so a driver image already installed by an
/// earlier Run is reused byte-for-byte rather than copied again — the
/// estate owns exactly one copy per *distinct* driver binary that has
/// ever launched an actor in it, not one per Run and not one per
/// reattach.
///
/// Durable and atomic (item 1, "std/platform atomic durable install/
/// verification primitives"): the bytes go to a uniquely-named
/// temporary file in the image's own directory, are flushed and
/// `sync_all`'d, and only then `rename`d onto the final name —
/// `rename(2)` within one directory is atomic, so a concurrent Run
/// either sees no image or sees a complete one, never a half-written
/// file. An image that already exists is verified by digest and left
/// exactly as it is.
fn install_runtime_image(estate_root: &str, exe: &Path) -> Result<PathBuf, RuntimePinError> {
    let digest =
        sha256_file(exe).map_err(|err| pin_io("reading this driver's own binary", exe, &err))?;
    let dir = Path::new(estate_root)
        .join(".wirk")
        .join("runtime")
        .join("images")
        .join(&digest);
    let image = dir.join("wirk");
    if image.exists() {
        // Already installed by this or an earlier Run. Verify rather
        // than trust the path: a truncated or replaced image is a
        // re-install, never a silent hand-off.
        match sha256_file(&image) {
            Ok(found) if found == digest => return Ok(image),
            Ok(_) | Err(_) => {}
        }
    }
    std::fs::create_dir_all(&dir)
        .map_err(|err| pin_io("creating the runtime image directory", &dir, &err))?;
    let staged = dir.join(format!(
        "wirk.staged.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    // `std::fs::copy` preserves the source's Unix permission bits, so
    // the staged file (and the image it becomes) stays executable.
    std::fs::copy(exe, &staged)
        .map_err(|err| pin_io("staging the runtime image", &staged, &err))?;
    {
        let file = std::fs::File::open(&staged)
            .map_err(|err| pin_io("opening the staged runtime image", &staged, &err))?;
        file.sync_all()
            .map_err(|err| pin_io("flushing the staged runtime image", &staged, &err))?;
    }
    let staged_digest = sha256_file(&staged)
        .map_err(|err| pin_io("verifying the staged runtime image", &staged, &err))?;
    if staged_digest != digest {
        let _ = std::fs::remove_file(&staged);
        return Err(RuntimePinError::Io {
            step: "verifying the staged runtime image".to_string(),
            path: staged.display().to_string(),
            reason: format!("staged bytes hash {staged_digest}, not the source's own {digest}"),
        });
    }
    std::fs::rename(&staged, &image).map_err(|err| {
        let _ = std::fs::remove_file(&staged);
        pin_io("installing the runtime image", &image, &err)
    })?;
    Ok(image)
}

/// Bind `image` into this Run's own `bin` directory as a file literally
/// named `wirk`. A hard link first (`std::fs::hard_link`, R3): the Run
/// gets its own durable directory entry to an immutable inode, at the
/// cost of zero additional bytes, and the bytes survive the image
/// directory being removed for as long as this Run's link exists. A
/// full copy is the fallback for a filesystem that refuses the link
/// (a cross-device image store, a filesystem without hard links) —
/// correctness first, bytes second.
fn bind_runtime_image(image: &Path, pinned: &Path) -> Result<(), RuntimePinError> {
    match std::fs::hard_link(image, pinned) {
        Ok(()) => Ok(()),
        Err(_) => {
            let staged = pinned.with_extension(format!("staged.{}", std::process::id()));
            std::fs::copy(image, &staged)
                .map_err(|err| pin_io("staging this Run's own wirk", &staged, &err))?;
            std::fs::rename(&staged, pinned).map_err(|err| {
                let _ = std::fs::remove_file(&staged);
                pin_io("installing this Run's own wirk", pinned, &err)
            })
        }
    }
}

/// Where one Run's own pinned `wirk` lives, as a pure path
/// computation with no I/O: the directory
/// `<estate_root>/.wirk/runtime/<run_id>/bin/`, and inside it a file
/// literally named `wirk`. The same precedent
/// `claim_hook::claude_settings_path` already sets for claude's
/// settings file — one function owning the layout, so two call sites
/// that share no state still name the identical file instead of each
/// re-spelling the joins.
///
/// P3 runtime-guidance: three consumers now need this layout without
/// having a `PathBuf` threaded to them — the installer
/// (`ensure_pinned_wirk_bin`, which computes it and then does the
/// I/O), and the standing prompt (`run_loop::compose_first_prompt`),
/// which tells the actor the exact absolute command to invoke for its
/// own Run and is a formatting function with no access to the
/// installer's return value. Deriving it from the execution triple the
/// actor already carries (`WIRK_ESTATE_ROOT`/`WIRK_RUN_ID`, 0022 D73)
/// is not a fourth authority variable: it is the same two values, read
/// through the one layout function the installer itself uses.
pub fn run_wirk_bin_dir(estate_root: &str, run_id: &str) -> PathBuf {
    Path::new(estate_root)
        .join(".wirk")
        .join("runtime")
        .join(run_id)
        .join("bin")
}

/// The Run's own pinned `wirk` file itself — `run_wirk_bin_dir` plus
/// the literal name `wirk` the pin is always bound as.
pub fn run_wirk_bin(estate_root: &str, run_id: &str) -> PathBuf {
    run_wirk_bin_dir(estate_root, run_id).join("wirk")
}

/// A directory scoped to one Run
/// (`<estate_root>/.wirk/runtime/<run_id>/bin/`, the same `.wirk`
/// convention `claim_hook::run_dir` already uses under the estate root
/// wirk already owns — never the worktree, never `~/`) holding a file
/// literally named `wirk`, whose bytes are this Run's runtime for the
/// whole life of the Run. `actor_pane` prepends the returned directory
/// to the pane's own `PATH`, ahead of `exe.parent()`, so `command -v
/// wirk` in that pane resolves to exactly the binary this Run is bound
/// to regardless of what that binary's own on-disk filename is — this
/// estate's own review discipline preserves candidate binaries under
/// commit-named files, which broke the bare-name assumption
/// `exe.parent()` alone makes (native-progress-contract-use/HANDOFF.md
/// §1.4).
///
/// P3 execution-recovery **correction**, item 1. The prior version
/// removed and re-copied `exe` on *every* call, so a reattach (or any
/// second `wirk run` for the same Run, from a different driver image)
/// silently replaced bytes the Run was already using, and a failure
/// only printed a line and let the pane launch under the weaker
/// `exe.parent()` guarantee. Both are corrected here:
///
/// - **Stable.** An existing pin whose bytes still match the digest
///   recorded in `wirk.pin` beside it is returned untouched — a
///   reattach, a second driver image, a rebuilt/renamed/deleted
///   original `exe`, none of them can change what this Run's `wirk`
///   is. Only an *unpinned* Run installs anything.
/// - **Restorable, not re-pinnable.** If the Run's own file is missing
///   or corrupt, it is restored from the shared image its `wirk.pin`
///   names — the same bytes, not the current driver's. Only if that
///   image is gone too is the launch refused
///   (`RuntimePinError::Unrestorable`), rather than quietly binding
///   the Run to a different binary.
/// - **Refusing, not degrading.** Every failure returns `Err`;
///   `actor_pane` turns it into `HerdrExecutorError::RuntimePin` before
///   any pane exists.
/// - **Bounded.** One shared immutable image per *distinct* driver
///   binary in the estate (`install_runtime_image`), one hard link plus
///   a ~70-byte `wirk.pin` record per Run — not one full binary copy
///   per Run, and not one per reattach.
pub fn ensure_pinned_wirk_bin(
    estate_root: &str,
    run_id: &str,
    exe: &Path,
) -> Result<PathBuf, RuntimePinError> {
    let dir = run_wirk_bin_dir(estate_root, run_id);
    let pinned = run_wirk_bin(estate_root, run_id);
    let record = dir.join("wirk.pin");

    // Already pinned? Then this Run's runtime is decided, and nothing
    // about the driver now attaching gets to change it.
    if let Ok(recorded) = std::fs::read_to_string(&record) {
        let recorded = recorded.trim().to_string();
        if !recorded.is_empty() {
            if let Ok(found) = sha256_file(&pinned)
                && found == recorded
            {
                return Ok(dir);
            }
            // The file is missing or no longer holds the pinned bytes:
            // restore it from the image that digest names, never from
            // whatever `exe` happens to be now.
            let image = Path::new(estate_root)
                .join(".wirk")
                .join("runtime")
                .join("images")
                .join(&recorded)
                .join("wirk");
            let restorable = sha256_file(&image).map(|d| d == recorded).unwrap_or(false);
            if restorable {
                let _ = std::fs::remove_file(&pinned);
                bind_runtime_image(&image, &pinned)?;
                let found = sha256_file(&pinned)
                    .map_err(|err| pin_io("verifying this Run's own wirk", &pinned, &err))?;
                if found != recorded {
                    return Err(RuntimePinError::Io {
                        step: "verifying this Run's own wirk".to_string(),
                        path: pinned.display().to_string(),
                        reason: format!("restored bytes hash {found}, not the pinned {recorded}"),
                    });
                }
                return Ok(dir);
            }
            let found = sha256_file(&pinned).unwrap_or_else(|_| "<absent>".to_string());
            return Err(RuntimePinError::Unrestorable {
                path: pinned.display().to_string(),
                expected: recorded,
                found,
                image: image.display().to_string(),
            });
        }
    }

    // Not pinned yet: this is the one call that decides the Run's
    // runtime. Install the shared image, bind it, record the digest.
    let image = install_runtime_image(estate_root, exe)?;
    let digest = image
        .parent()
        .and_then(|dir| dir.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::fs::create_dir_all(&dir)
        .map_err(|err| pin_io("creating this Run's own bin directory", &dir, &err))?;
    let _ = std::fs::remove_file(&pinned);
    bind_runtime_image(&image, &pinned)?;
    let found = sha256_file(&pinned)
        .map_err(|err| pin_io("verifying this Run's own wirk", &pinned, &err))?;
    if found != digest {
        return Err(RuntimePinError::Io {
            step: "verifying this Run's own wirk".to_string(),
            path: pinned.display().to_string(),
            reason: format!("bound bytes hash {found}, not the image's own {digest}"),
        });
    }
    std::fs::write(&record, format!("{digest}\n"))
        .map_err(|err| pin_io("recording this Run's own runtime digest", &record, &err))?;
    Ok(dir)
}

/// P4.1: one launch's working state for the reserved worker contract —
/// the proven bytes, where they are, and which mechanism ended up
/// delivering them.
///
/// It exists because the decision is taken in two places: opencode's
/// delivery is part of the pane's environment (`actor_pane`), and
/// claude's and codex's are argv elements (`start_actor_agent`). Both
/// write into the same plan, and the prompt fallback is whatever is left
/// when neither claimed it — so a kind can never be served twice, and
/// can never be served not at all.
struct ContractPlan {
    reference: wirk_core::WorkerContractRef,
    path: std::path::PathBuf,
    text: String,
    delivery: Option<wirk_core::ContractDelivery>,
    /// Why a native mechanism that was tried did not work, carried
    /// forward so the prompt fallback discloses the real reason rather
    /// than a generic one.
    pending_fallback: Option<String>,
}

impl ContractPlan {
    fn deliver(&mut self, mode: wirk_core::ContractDeliveryMode, reason: Option<String>) {
        self.delivery = Some(worker_contract::delivery(&self.reference, mode, reason));
    }
}

impl<C: HerdrClient> HerdrExecutor<C> {
    pub fn new(client: C) -> Self {
        Self {
            client,
            codex_probe: std::sync::Arc::new(worker_contract::CodexCliProbe),
        }
    }

    /// Replaces the codex composition probe (P4.1). The default is the
    /// real `codex debug prompt-input` renderer; tests substitute a
    /// fixed answer so each branch of the composition decision is
    /// exercised deterministically.
    pub fn with_codex_probe(
        mut self,
        probe: std::sync::Arc<dyn worker_contract::CodexProbe>,
    ) -> Self {
        self.codex_probe = probe;
        self
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
        // D1, "validate before unnecessary execution-side effects": a
        // request no mapping can honor, or one that contradicts itself
        // (`validate_selection`), is refused before a pane is created
        // — not after, as it was when the only check lived inside
        // `start_actor_agent`.
        validate_selection(run.kind.0.as_str(), &run.selection)?;
        // P4.1 (ruling 0202): prove the reserved contract before any
        // pane side effect, for the same reason `validate_selection`
        // runs here — "a verified contract must actually be delivered,
        // or launch must refuse".
        let mut contract = self.verified_contract(world)?;
        let mut claim_hook: Option<wirk_core::ClaimHookDelivery> = None;
        let pane = self.actor_pane(run, world, contract.as_mut(), &mut claim_hook)?;

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
        let mut events = self.client.subscribe(vec![
            EventSubscription::PaneAgentStatusChanged {
                pane_id: pane.pane_id.clone(),
            },
            EventSubscription::PaneUpdated {
                pane_id: pane.pane_id.clone(),
            },
        ])?;

        let argv = self.start_actor_agent_when_ready(
            run,
            &pane.pane_id,
            world,
            &mut events,
            contract.as_mut(),
            &mut claim_hook,
        )?;

        Ok(LaunchedRun {
            pane,
            events,
            argv,
            contract: contract.and_then(|plan| plan.delivery),
            claim_hook,
        })
    }

    /// Retries `start_actor_agent` on Herdr's pane-busy refusal (P2.5
    /// W2, 0050 D151, `orient/launch.md` §1-§2, amended by the build
    /// brief's §7.2): a freshly split pane's shell is still finishing
    /// startup, and `agent.start` refuses `agent_pane_busy`
    /// (`refs/herdr/src/app/agents.rs:186-193`, `available_pane_shell`)
    /// until it settles. There is no separate readiness field to poll
    /// (§1's own finding), so the only signal is the refusal itself:
    /// on it, print the pane and that the launch is waiting, then block
    /// on the **already-open** subscription (D51 order — `events` was
    /// opened before the first attempt, by `launch_actor` above) for
    /// its next event addressed to this pane, and retry. No count, no
    /// timer (0044 D134): the loop ends only because reality — the
    /// pane's own activity — settles, never because an attempt budget
    /// ran out. Any other error propagates unchanged, as today.
    fn start_actor_agent_when_ready(
        &self,
        run: &wirk_core::Run,
        pane_id: &str,
        world: &wirk_core::World,
        events: &mut Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>,
        contract: Option<&mut ContractPlan>,
        claim_hook: &mut Option<wirk_core::ClaimHookDelivery>,
    ) -> Result<Vec<String>, HerdrExecutorError> {
        // P4.1: the plan is re-borrowed on every retry, never re-decided
        // — a pane-busy retry must not be able to change which
        // mechanism this Run is recorded as having used.
        let mut contract = contract;
        loop {
            match self.start_actor_agent(run, pane_id, world, contract.as_deref_mut(), claim_hook) {
                Ok(argv) => return Ok(argv),
                Err(err) if is_agent_pane_busy(&err) => {
                    println!(
                        "wirk: pane {pane_id} is busy, waiting for its next event before \
                         retrying agent.start"
                    );
                    match events.next() {
                        Some(Ok(_)) => continue,
                        Some(Err(err)) => return Err(err.into()),
                        None => {
                            return Err(HerdrExecutorError::Herdr(HerdrError::Transport(format!(
                                "subscription for pane {pane_id} closed while waiting for \
                                     it to become ready"
                            ))));
                        }
                    }
                }
                Err(other) => return Err(other),
            }
        }
    }

    /// P4.1 (ruling 0202): reads and **proves** the shared worker
    /// contract this Run's World was reserved with, before anything
    /// irreversible happens.
    ///
    /// Called first by both launch paths, ahead of `actor_pane`, so a
    /// contract that cannot be honoured costs no workspace, no pane and
    /// no agent — the same ordering `validate_selection` and the runtime
    /// pin already have. `Ok(None)` is the honest answer for every World
    /// reserved before this wave: it carries no contract, and its launch
    /// is exactly the launch it always was.
    fn verified_contract(
        &self,
        world: &wirk_core::World,
    ) -> Result<Option<ContractPlan>, HerdrExecutorError> {
        let wirk_core::World::Actor(actor) = world else {
            return Ok(None);
        };
        let Some(reference) = actor.contract.clone() else {
            return Ok(None);
        };
        let (path, text) = worker_contract::read_verified(
            std::path::Path::new(&actor.triple.estate_root),
            &reference,
        )?;
        Ok(Some(ContractPlan {
            reference,
            path,
            text,
            delivery: None,
            pending_fallback: None,
        }))
    }

    /// The actor's pane: reuse-and-split when one exists for this Run,
    /// create a workspace and split otherwise. No subscription, no
    /// agent — shared by `launch_actor` and the `Executor::launch`
    /// trait row.
    fn actor_pane(
        &self,
        run: &wirk_core::Run,
        world: &wirk_core::World,
        contract: Option<&mut ContractPlan>,
        claim_hook: &mut Option<wirk_core::ClaimHookDelivery>,
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

        // `current_exe` (R3, stdlib), read once here and reused below
        // for the Claim hook's own invocation (native-progress-
        // contract-use/HANDOFF.md §1.4, Rule 4): the one value both
        // uses thread from, rather than two separate lookups.
        let exe = std::env::current_exe().ok();

        // 0050 D151: an actor's by-hand `wirk claim` is `command not
        // found` unless the running binary's own directory is on the
        // pane's PATH — the session it inherits from was not
        // necessarily started with the build directory prepended.
        // `current_exe` follows the launching binary wherever it runs
        // from, ahead of the pane's inherited PATH; scoped to this
        // actor pane's own env map, not the session's (`orient/
        // launch.md` §3, J1). This does not make the *hook's own*
        // invocation resolvable when the binary is renamed — see the
        // absolute-path hook delivery below.
        let mut path_entries = Vec::new();
        // P3 execution-recovery item 4, as corrected: a Run-scoped
        // directory holding a file literally named `wirk`, whose bytes
        // are pinned for the life of the Run, prepended ahead of
        // everything else. D151's own `exe.parent()` prepend (kept,
        // just below) only resolves `wirk` when the file *in* that
        // directory happens to be named `wirk`; this estate's own
        // review discipline preserves candidate binaries under
        // commit-named files (`wirk-96f5a6a-verify`, ...), so a fresh
        // actor pane launched from one of those sees the right
        // directory on `PATH` and still gets `command not found` —
        // then, observed live, falls back to whatever else is already
        // on its inherited `PATH`, which can be an unrelated, mutable
        // shared `debug/wirk` (native-progress-contract-use/
        // HANDOFF.md §1.4; MECHANISM-REPORT.md qualification 4, fourth
        // bullet).
        //
        // The pin is **required**, not best-effort: an unfulfilled pin
        // is refused here, before any pane is created, rather than
        // launching the actor under the weaker guarantee with a line
        // printed about it (correction item 1). Nothing about this
        // Run's runtime changes on a reattach — `ensure_pinned_wirk_bin`
        // returns an already-pinned Run's own directory untouched.
        let exe = exe.as_deref().ok_or(HerdrExecutorError::RuntimePin(
            RuntimePinError::NoCurrentExe,
        ))?;
        let pinned_dir = ensure_pinned_wirk_bin(&actor.triple.estate_root, &run.id.0, exe)?;
        // The Run's own pinned `wirk`, not the driver's mutable `exe`:
        // every consumer downstream of the launch decision — the actor's
        // by-hand `wirk claim` via `PATH` (just below), and both
        // Claim-hook writers (opencode here, claude in
        // `start_actor_agent`) — must resolve to the *same* stable
        // bytes `ensure_pinned_wirk_bin` just decided for this Run, or
        // pinning the `PATH` entry alone leaves the hook itself invoking
        // whatever `exe` happened to be at launch time, unpinned
        // (RECOVERY-CHILD-CHECK.md's read of `c4936910`: the opencode
        // hook at old line 1267 and the claude hook at old line 1390
        // both still threaded raw `exe`/`current_exe()`).
        let pinned_wirk = pinned_dir.join("wirk");
        path_entries.push(pinned_dir);
        if let Some(dir) = exe.parent() {
            path_entries.push(dir.to_path_buf());
        }
        path_entries.extend(std::env::split_paths(
            &std::env::var("PATH").unwrap_or_default(),
        ));
        if let Ok(path) = std::env::join_paths(path_entries) {
            env.insert("PATH".to_string(), path.to_string_lossy().into_owned());
        }

        // P2.6 W3 (rerun findings, `03-orient.log`): the actor built the
        // whole workspace in place inside its worktree, into a stray
        // `.target-local/` (345M, out of boundary), because the pane it
        // ran in carried no `CARGO_TARGET_DIR` of its own — nothing told
        // it where the named-kept cache (0030; 0039 D126) lives. Same
        // mechanism as `PATH` above: read from the driver's own process
        // env and passed through only when set, never invented (an
        // actor pane started against a driver with none configured gets
        // none either, same as today).
        if let Ok(cache) = std::env::var("CARGO_TARGET_DIR") {
            env.insert("CARGO_TARGET_DIR".to_string(), cache);
        }

        // P2.7 Wave 2 (`orient/reorient.md` §6 item 1): an opencode
        // Run gets wirk's own claim-filing plugin delivered with no
        // write into the worktree and no write under `~/` —
        // `w2-probe.md`'s Mechanism 2, measured live. The directory is
        // wirk-owned, under the estate root (`claim_hook::run_dir`,
        // the same `.wirk` convention `wirkd::client::locate` already
        // uses), never `actor.worktree_path`. A write failure here
        // does not fail the launch: the actor still starts, just
        // without the hook, the same "degrade, don't block" posture
        // `CARGO_TARGET_DIR` above already has (best-effort, passed
        // through only when it can be). Gated on `run.kind` directly
        // (not `claim_hook::hook_installed_for`, which W3 widened to
        // include claude too): opencode's own delivery mechanism is an
        // env var, claude's is an argv element built in
        // `start_actor_agent` below — the two kinds share the "is a
        // hook installed" predicate for the standing prompt, never the
        // delivery mechanism itself. **The plugin invokes this Run's own
        // pinned `wirk` (`pinned_wirk`, just installed above), not the
        // driver's mutable `exe`** — the pin exists precisely so this
        // Run's runtime cannot be silently swapped by a later reattach
        // or by the original `exe` being replaced/removed, and a hook
        // that ran `exe` instead would defeat that for the one path a
        // real opencode actor actually invokes (P3 execution-recovery
        // correction, connected-gap close). A write is skipped (same
        // degrade posture) when `current_exe` could not be read, exactly
        // as a `PATH`-prepend write above would have nothing to add —
        // this arm is unreachable in that case anyway, since `exe` above
        // is unwrapped before `pinned_dir`/`pinned_wirk` exist.
        //
        // P4.1 (ruling 0202): the same overlay carries the shared
        // worker contract, as one more entry in opencode's own
        // `instructions` array — measured additive across an
        // `OPENCODE_CONFIG_CONTENT` layer (the owner's global
        // `instructions` and `plugin`, and those of any
        // `OPENCODE_CONFIG` file this launch already carries, all
        // survive and concatenate), so no new mechanism is introduced
        // and nothing the user configured is replaced. The *contract's* delivery is not best-effort the
        // way the hook's is: if the overlay cannot be written, this Run
        // has no native contract delivery, and the prompt fallback in
        // `start_actor_agent` below picks it up rather than an actor
        // launching with no contract at all.
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
        //
        // Ruling 0205 splits this into *deciding where the pane goes*
        // and *creating it*, because an opencode Run has to read the
        // environment that placement will actually give the pane before
        // it can know which configuration slot is free. Nothing about
        // the placement itself changed.
        let existing = self.client.get_pane(&run.id.0).ok();
        let (workspace_id, target_pane_id) = match existing {
            Some(pane) => (Some(pane.workspace_id), Some(pane.pane_id)),
            None => (
                self.client
                    .create_workspace(CreateWorkspace {
                        cwd: actor.worktree_path.clone(),
                        env: env.clone(),
                        label: None,
                    })
                    .ok()
                    .map(|w| w.workspace_id),
                None,
            ),
        };

        // P2.7 Wave 2 (`orient/reorient.md` §6 item 1): an opencode
        // Run gets wirk's own claim-filing plugin delivered with no
        // write into the worktree and no write under `~/` —
        // `w2-probe.md`'s Mechanism 2, measured live. The directory is
        // wirk-owned, under the estate root (`claim_hook::run_dir`,
        // the same `.wirk` convention `wirkd::client::locate` already
        // uses), never `actor.worktree_path`. A write failure here
        // does not fail the launch: the actor still starts, just
        // without the hook, the same "degrade, don't block" posture
        // `CARGO_TARGET_DIR` above already has. Gated on `run.kind`
        // directly (not `claim_hook::hook_installed_for`, which W3
        // widened to include claude too): opencode's own delivery
        // mechanism is an env var, claude's is an argv element built in
        // `start_actor_agent` below. **The plugin invokes this Run's own
        // pinned `wirk` (`pinned_wirk`, installed above), not the
        // driver's mutable `exe`.**
        //
        // P4.1 (ruling 0202): the same overlay carries the shared
        // worker contract, as one more entry in opencode's own
        // `instructions` array. The *contract's* delivery is not
        // best-effort the way the hook's is: if the overlay cannot be
        // written, this Run has no native contract delivery, and the
        // prompt fallback in `start_actor_agent` below picks it up
        // rather than an actor launching with no contract at all.
        if run.kind == wirk_core::ActorKind::opencode() {
            let contract_path = contract.as_ref().map(|plan| plan.path.clone());
            match claim_hook::write_wirk_claim_hook(
                &actor.triple.estate_root,
                &run.id.0,
                &pinned_wirk,
                contract_path.as_deref(),
            ) {
                Ok(overlay) => {
                    // opencode has **two** per-launch layers that
                    // compose with the owner's global config and with
                    // each other — `OPENCODE_CONFIG` (one file) and
                    // `OPENCODE_CONFIG_CONTENT` (one inline document) —
                    // and each names exactly one thing. Writing either
                    // over a value the launch already carries replaces
                    // that layer wholesale: measured with `opencode
                    // debug config`, 1.18.30, an inherited inline
                    // layer's `instructions`, `plugin`, `permission`
                    // and scalar entries all disappear (0204). So which
                    // slot wirk takes is decided from what is free, and
                    // only the both-taken case reads the inherited
                    // bytes at all.
                    //
                    // Ruling 0205: "what is free" is a fact about **the
                    // pane this launch will create**, not about this
                    // process. A pane's environment is the Herdr
                    // *server's* environment overlaid per key by the
                    // `env` map passed at creation; the driver's own
                    // environment is on neither path. So the two values
                    // are read by `probe_opencode_pane_config` from a
                    // short-lived pane placed exactly where the actor's
                    // pane is about to be placed, and `std::env::var`
                    // is not consulted for either key — it answered a
                    // different question, wrongly in both directions.
                    // When the probe cannot be read, wirk sets neither
                    // variable and discloses why: an unknown
                    // configuration is never overwritten on a guess.
                    match self.probe_opencode_pane_config(
                        &actor.triple.estate_root,
                        &run.id.0,
                        workspace_id.clone(),
                        target_pane_id.clone(),
                        &actor.worktree_path,
                        env.clone(),
                    ) {
                        Ok((inherited_content, inherited_config)) => {
                            let (key, value, fallback) = match claim_hook::opencode_delivery(
                                &overlay,
                                Some(inherited_content.as_str()),
                                Some(inherited_config.as_str()),
                            ) {
                                claim_hook::OpencodeDelivery::Content(bytes)
                                | claim_hook::OpencodeDelivery::ComposedContent(bytes) => (
                                    Some(claim_hook::OPENCODE_CONFIG_CONTENT_ENV.to_string()),
                                    Some(bytes),
                                    None,
                                ),
                                claim_hook::OpencodeDelivery::ConfigFile(path) => (
                                    Some(claim_hook::OPENCODE_CONFIG_ENV.to_string()),
                                    Some(path.to_string_lossy().into_owned()),
                                    None,
                                ),
                                claim_hook::OpencodeDelivery::Unsupported(reason) => {
                                    (None, None, Some(reason))
                                }
                            };
                            if let (Some(key), Some(value)) = (key, value) {
                                env.insert(key, value);
                            }
                            // 0208: the overlay carries wirk's Claim
                            // plugin and the contract together, so a
                            // slot taken is both delivered and a slot
                            // refused is neither. Recorded either way:
                            // the standing prompt promises an automatic
                            // claim only where one was actually handed
                            // to this pane.
                            *claim_hook = Some(match &fallback {
                                None => wirk_core::ClaimHookDelivery::Installed,
                                Some(reason) => wirk_core::ClaimHookDelivery::NotInstalled {
                                    reason: reason.clone(),
                                },
                            });
                            if let Some(plan) = contract {
                                match fallback {
                                    None => plan.deliver(
                                        wirk_core::ContractDeliveryMode::OpencodeInstructions,
                                        None,
                                    ),
                                    Some(reason) => plan.pending_fallback = Some(reason),
                                }
                            }
                        }
                        Err(reason) => {
                            println!("wirk: {reason}");
                            *claim_hook = Some(wirk_core::ClaimHookDelivery::NotInstalled {
                                reason: reason.clone(),
                            });
                            if let Some(plan) = contract {
                                plan.pending_fallback = Some(reason);
                            }
                        }
                    }
                }
                Err(error) => {
                    let reason = format!(
                        "wirk's opencode configuration overlay could not be written ({error})"
                    );
                    *claim_hook = Some(wirk_core::ClaimHookDelivery::NotInstalled {
                        reason: reason.clone(),
                    });
                    if let Some(plan) = contract {
                        plan.pending_fallback = Some(reason);
                    }
                }
            }
        }

        let pane = self.client.split_pane(SplitPane {
            workspace_id,
            target_pane_id,
            // `Down`: the actor's pane appears below the existing one,
            // matching the old hardcoded `Vertical`'s intent (a
            // vertical stack) now expressed in the schema's own
            // `right`/`down` vocabulary — kept as one hardcoded value
            // here, same as before (J1, local/reversible).
            direction: SplitDirection::Down,
            cwd: actor.worktree_path.clone(),
            env,
        })?;
        Ok(pane)
    }

    /// Reads the two opencode per-launch configuration values **as the
    /// actor's pane will actually have them** (ruling 0205), by running
    /// `claim_hook::OPENCODE_ENV_PROBE_SH` in a short-lived pane placed
    /// exactly where the actor's pane is about to be placed.
    ///
    /// The probe pane is created with **the actor's own base launch
    /// environment** — the very `env` map `actor_pane` has built for
    /// this launch, handed in by value at the one moment it holds
    /// exactly what the actor will get minus the opencode key this
    /// decision is about to choose (ruling 0208). A Herdr pane's
    /// environment is the server's environment overlaid per key by the
    /// caller's `env` map, so this makes the probe's pane and the
    /// actor's pane the same context in both halves rather than only in
    /// the inherited half: same cwd, same shell startup, same
    /// `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`/`PATH`/
    /// `CARGO_TARGET_DIR`. It closes the one residual the previous
    /// `env: {}` left open — a shell rule that *derives* an opencode
    /// variable from a launch variable would have been read wrongly by a
    /// pane that did not carry those launch variables
    /// (`affected-verify/VERIFY.md`, "Residual limit"), and is measured
    /// green by an owned control on this box.
    ///
    /// R2: only `pane.split`, `pane.send_text` and `pane.close` are used
    /// — verbs this client already speaks; R4 was checked first and
    /// herdr 0.9.0 returns no pane environment from any read verb. The
    /// script/marker/deadline protocol itself is **R7**, reached only
    /// because R1–R5 fail: reusing native verbs does not make a new
    /// invocation-local protocol pre-existing functionality (0208).
    ///
    /// The command is re-sent until the completion marker appears or
    /// the deadline passes, because a freshly split pane's shell is
    /// still starting and may not have consumed the first line — the
    /// same settling `start_actor_agent_when_ready` waits out on
    /// `agent_pane_busy`. Re-sending is safe: the script only rewrites
    /// the same three files from the same environment. The marker is
    /// checked before each wait, so a client that answers
    /// synchronously (the fake) costs no sleep at all.
    ///
    /// `Err` carries the sentence a caller discloses as the contract's
    /// `fallback_reason`: when this cannot be answered, wirk sets
    /// neither variable rather than replace a layer it cannot see.
    fn probe_opencode_pane_config(
        &self,
        estate_root: &str,
        run_id: &str,
        workspace_id: Option<String>,
        target_pane_id: Option<String>,
        cwd: &std::path::Path,
        env: BTreeMap<String, String>,
    ) -> Result<(String, String), String> {
        let unknown = |what: &str| {
            format!(
                "wirk could not read this pane's own opencode configuration ({what}), so it set \
                 neither {} nor {} rather than replace a layer this launch may already carry: \
                 neither the worker contract nor wirk's own Claim plugin is delivered to this \
                 pane natively",
                claim_hook::OPENCODE_CONFIG_ENV,
                claim_hook::OPENCODE_CONFIG_CONTENT_ENV,
            )
        };

        let probe = claim_hook::write_opencode_env_probe(estate_root, run_id)
            .map_err(|error| unknown(&format!("its probe could not be written: {error}")))?;

        // Ruling 0208: the answer files hold *the launch's own*
        // configuration — an inline document that can carry anything the
        // owner or the Route put in it — copied into wirk-owned scratch
        // so one decision can be taken from it. They are cleared before
        // the probe runs, so a stale answer from an earlier attempt can
        // never be read as this one's, and cleared again on **every**
        // return below, not only the successful one: the failure paths
        // are the ones that used to keep an inherited layer on disk for
        // the life of the estate (`affected-verify/VERIFY.md` §4). Only
        // the three files this Run's own probe writes are removed; the
        // script beside them is wirk's own program and carries nothing
        // from the launch, and no path wirk does not own is touched.
        probe.clear();
        let outcome =
            self.read_opencode_pane_config(&probe, workspace_id, target_pane_id, cwd, env);
        probe.clear();
        outcome.map_err(|what| unknown(&what))
    }

    /// The probe itself: one pane, one command line, three files. Split
    /// out from `probe_opencode_pane_config` so that function can clear
    /// the answer files on every return through a single path, whatever
    /// this one does (ruling 0208). `Err` carries only the clause the
    /// caller's own disclosure sentence is built around.
    fn read_opencode_pane_config(
        &self,
        probe: &claim_hook::OpencodeEnvProbe,
        workspace_id: Option<String>,
        target_pane_id: Option<String>,
        cwd: &std::path::Path,
        env: BTreeMap<String, String>,
    ) -> Result<(String, String), String> {
        let pane = self
            .client
            .split_pane(SplitPane {
                workspace_id,
                target_pane_id,
                direction: SplitDirection::Down,
                cwd: cwd.to_path_buf(),
                env,
            })
            .map_err(|error| format!("its probe pane could not be created: {error}"))?;

        let command = probe.command();
        let deadline = std::time::Instant::now() + OPENCODE_ENV_PROBE_DEADLINE;
        let mut sent = Err(String::new());
        while std::time::Instant::now() < deadline {
            if let Err(error) = self.client.send_input(&pane.pane_id, &command) {
                sent = Err(format!("its probe could not be sent: {error}"));
                break;
            }
            sent = Ok(());
            let attempt = std::time::Instant::now() + OPENCODE_ENV_PROBE_RESEND;
            while std::time::Instant::now() < attempt {
                if probe.done.exists() {
                    break;
                }
                std::thread::sleep(OPENCODE_ENV_PROBE_POLL);
            }
            if probe.done.exists() {
                break;
            }
        }

        // The probe pane has done its one job either way; a close that
        // fails leaves an idle shell, never a wrong answer.
        let _ = self.client.close_pane(&pane.pane_id);

        sent?;
        if !probe.done.exists() {
            return Err("its probe did not report back in time".to_string());
        }
        probe
            .read()
            .map_err(|error| format!("its probe's answer was unreadable: {error}"))
    }

    /// `agent.start` on the actor's pane, named by `run.id` — the name
    /// every later `agent.*` call targets (`agent.prompt`,
    /// `agent.send_keys`: confirmed live, `tried/RESULT.md` run 3,
    /// 04-blocked). Returns Herdr's own `agent_started.argv`
    /// (`HerdrClient::start_agent`'s own doc).
    fn start_actor_agent(
        &self,
        run: &wirk_core::Run,
        pane_id: &str,
        world: &wirk_core::World,
        contract: Option<&mut ContractPlan>,
        claim_hook: &mut Option<wirk_core::ClaimHookDelivery>,
    ) -> Result<Vec<String>, HerdrExecutorError> {
        // P3 native launch selection (BUILD-BRIEF.md, superseding W1's
        // 0041 D129 hardcoded per-kind defaults): `run.selection`, not a
        // machine-specific model literal baked into the product
        // (PREPARATION-ADJUDICATION.md point 4 — "the product must not
        // require edits when our local Qwen endpoint/model changes").
        // `build_selection_args` maps a *requested* model/effort onto
        // the harness's own real, installed, interactive CLI flags
        // (verified by hand against `claude --help`/`opencode --help`/
        // `codex --help`, not the headless/`exec` examples the
        // preparation report's own illustrative code copied instead —
        // adjudication point 2) and refuses, before `agent.start` is
        // ever called, a request no mapping can honor without guessing
        // syntax (`SelectionError`) rather than silently dropping or
        // downgrading it. Omitted model/effort adds no flag at all: the
        // harness's own native default runs, honestly unrepresented as
        // any particular model identity.
        //
        // 0056 D164 stands unchanged for the *kind* itself: a kind with
        // no row here launches bare when no model/effort was requested,
        // passed through to `agent.start` verbatim; Herdr's own answer
        // to that call is the kind's own validation, not a match arm
        // added here.
        let kind_str = run.kind.0.as_str();
        let mut args = build_selection_args(kind_str, &run.selection)?;

        // P2.7 Wave 3 (`build-brief.md` §6 item 1, `reorient.md` §C),
        // corrected by ruling 0208: a claude Run gets wirk's own
        // Claim-filing hook via one `--plugin-dir <dir>` argv element
        // pair naming a wirk-owned plugin directory under the estate
        // root (never the worktree, never `~/`).
        //
        // It was `--settings <path>`, and that silently destroyed a
        // launch's own `--settings`: claude resolves that flag
        // last-wins, measured in owned panes on 2.1.270 — the route's
        // `SessionStart` hook fires with its settings alone and does not
        // fire once wirk appends its own, and the route's `env` block
        // goes with it. `--plugin-dir` is claude's own repeatable,
        // session-scoped mechanism (`claude --help`), measured additive
        // in the same controls against a launch's own `--settings` *and*
        // its own `--plugin-dir`, so wirk now takes a slot that is not
        // exclusive and reads, parses and replaces nothing the launch
        // configured. No settings file of the user's, the repository's
        // or the launch's is written, widened or inspected. Same
        // "degrade, don't block" posture as opencode's env-var delivery
        // above: a write failure (including `current_exe` itself being
        // unreadable, or this Run's own pin having become unrestorable
        // since `actor_pane` ran) leaves the launch unaffected, just
        // without the hook. **The hook's own command invokes this Run's
        // own pinned `wirk`, not the driver's mutable `current_exe`** —
        // resolved fresh here via `ensure_pinned_wirk_bin` rather than
        // threaded from `actor_pane`, a separate call with no shared
        // state (0001 D9's own boundary between the two methods), but by
        // the time this call is reached `actor_pane` has already pinned
        // this Run, so the call here is the same "already pinned, return
        // untouched" no-op read `ensure_pinned_wirk_bin`'s own doc
        // describes — never a second install, never a re-pin (P3
        // execution-recovery correction, connected-gap close: a Stop
        // hook that ran `current_exe` bypassed the pin for the one
        // command a real claude actor's own turn end actually fires).
        if run.kind == wirk_core::ActorKind::claude()
            && let wirk_core::World::Actor(actor) = world
        {
            let written = std::env::current_exe()
                .map_err(|error| error.to_string())
                .and_then(|exe| {
                    ensure_pinned_wirk_bin(&actor.triple.estate_root, &run.id.0, &exe)
                        .map_err(|error| error.to_string())
                })
                .and_then(|pinned_dir| {
                    claim_hook::write_claude_claim_plugin(
                        &actor.triple.estate_root,
                        &run.id.0,
                        &pinned_dir.join("wirk"),
                    )
                    .map_err(|error| error.to_string())
                });
            match written {
                Ok(plugin_dir) => {
                    args.push("--plugin-dir".to_string());
                    args.push(plugin_dir.to_string_lossy().into_owned());
                    *claim_hook = Some(wirk_core::ClaimHookDelivery::Installed);
                }
                // 0208: the same "degrade, don't block" posture as
                // before — the launch still happens — but the degrade is
                // now *recorded*, so the standing prompt can stop
                // promising an automatic claim this pane will never get.
                Err(error) => {
                    *claim_hook = Some(wirk_core::ClaimHookDelivery::NotInstalled {
                        reason: format!(
                            "wirk's own Claim-hook plugin could not be written for this pane \
                             ({error})"
                        ),
                    });
                }
            }
        }

        // 0208: every other kind. `hook_installed_for` names the kinds
        // wirk has a mechanism *for*; this records what this launch
        // actually did, and for a kind with no mechanism the honest
        // record is "none was installed", not silence. Set only when
        // nothing above already answered, so opencode's own answer
        // (taken in `actor_pane`, where its delivery is decided) is
        // never overwritten here.
        if claim_hook.is_none() {
            *claim_hook = Some(wirk_core::ClaimHookDelivery::NotInstalled {
                reason: format!(
                    "wirk has no automatic Claim-filing hook for the `{kind_str}` harness"
                ),
            });
        }

        // P4.1 (ruling 0202): the shared worker contract's own argv
        // half. Additive in every arm — claude appends rather than
        // replaces, codex only composes when codex itself says the
        // composition adds without displacing — and whatever is not
        // claimed here falls through to disclosed prompt delivery, so no
        // actor ever starts without the contract its World reserved.
        if let Some(plan) = contract {
            match kind_str {
                // `--append-system-prompt-file`, not
                // `--append-system-prompt`: Herdr types the quoted argv
                // line into the pane's shell and refuses any element
                // containing a control character, so a multi-line
                // contract cannot ride in argv at all. The file variant
                // also keeps `--system-prompt`'s *replacing* semantics
                // permanently out of the picture. The file must stay
                // readable for the life of the Run: claude re-renders
                // the appended prompt after a compaction, from the same
                // path.
                "claude" => {
                    // Review F3, the concrete transport collision: this
                    // launch may already carry an append control of its
                    // own. Whether claude 2.1.269 concatenates two
                    // append sources or keeps the last could not be
                    // settled without a live actor, so wirk does not
                    // guess: it adds nothing, leaves the launch's own
                    // append exactly as authored, and delivers the
                    // contract as disclosed prompt text instead.
                    //
                    // Narrow on purpose (0202: "do not blanket-refuse
                    // … user base prompts or other unrelated
                    // settings"). Only the two *append* controls
                    // collide with the mechanism wirk uses; `--agent`,
                    // `--system-prompt[-file]` and every other
                    // base-prompt setting compose with an append and
                    // are untouched here.
                    match existing_claude_append_control(&args) {
                        Some(flag) => {
                            plan.pending_fallback = Some(format!(
                                "this launch already carries `{flag}`, and wirk cannot show \
                                 that adding a second append source composes rather than \
                                 replacing it"
                            ));
                        }
                        None => {
                            args.push("--append-system-prompt-file".to_string());
                            args.push(plan.path.to_string_lossy().into_owned());
                            plan.deliver(
                                wirk_core::ContractDeliveryMode::AppendSystemPromptFile,
                                None,
                            );
                        }
                    }
                }
                // codex has no file-valued instruction key and no
                // additive one wirk may write (the `managed_`/
                // `additional_` keys belong to enterprise-managed
                // configuration). `-c developer_instructions=` is an
                // override, so it is used **only** when codex's own dry
                // render shows the override adds the contract and
                // displaces nothing already configured; otherwise the
                // user's value is left entirely alone and the contract
                // is delivered by disclosed prompt.
                "codex" => {
                    let cwd = match world {
                        wirk_core::World::Actor(actor) => actor.worktree_path.clone(),
                        wirk_core::World::Deterministic(_) => std::path::PathBuf::from("."),
                    };
                    // Review F1: the decision is taken under the
                    // arguments this launch will actually run with —
                    // `build_selection_args`' mapped model/effort and
                    // the Route's own `selection.args` — never under a
                    // configuration no launch will ever have.
                    match worker_contract::codex_composition(
                        &cwd,
                        &plan.text,
                        self.codex_probe.as_ref(),
                        &args,
                    ) {
                        worker_contract::CodexComposition::Additive => {
                            args.push("-c".to_string());
                            args.push(worker_contract::codex_override(&plan.text));
                            plan.deliver(
                                wirk_core::ContractDeliveryMode::CodexDeveloperInstructions,
                                None,
                            );
                        }
                        worker_contract::CodexComposition::Fallback(reason) => {
                            plan.pending_fallback = Some(reason);
                        }
                    }
                }
                _ => {}
            }
            if plan.delivery.is_none() {
                // 0056 D164 stands: a kind with no native mechanism is
                // not refused and not silently skipped. It is served by
                // ordinary prompt text under an explicit disclosure, and
                // the mode is journaled, so "native" and "prompt" are a
                // recorded difference rather than an assumption.
                let reason = plan.pending_fallback.clone().unwrap_or_else(|| {
                    format!(
                        "wirk has no verified native instruction-delivery mechanism for the \
                         `{kind_str}` harness"
                    )
                });
                plan.deliver(wirk_core::ContractDeliveryMode::Prompt, Some(reason));
            }
            // The invariant Herdr enforces at `agent.start`, restated
            // here so a future contract that cannot cross the shell
            // fails in wirk's own tests instead of at launch.
            debug_assert!(
                !args.iter().any(|arg| arg.chars().any(char::is_control)),
                "an argv element carrying a control character is refused by Herdr: {args:?}"
            );
        }

        let argv = self.client.start_agent(StartAgent {
            pane_id: pane_id.to_string(),
            kind: kind_str.to_string(),
            name: run.id.0.clone(),
            args,
            timeout_ms: None,
        })?;
        Ok(argv)
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
    /// P3 native launch selection: a requested model/effort this
    /// harness has no real, verified argv mapping for
    /// (`build_selection_args`'s own doc) — surfaced before
    /// `agent.start` is ever called, so no pane launches with a
    /// guessed or silently-dropped setting.
    #[error(transparent)]
    Selection(#[from] SelectionError),
    /// P3 execution-recovery correction item 1: this Run's own `wirk`
    /// runtime could not be pinned, or an existing pin could not be
    /// honoured. Refused before the pane is created — an actor never
    /// launches with an ambiguous or mutable `wirk` on its PATH.
    #[error(
        "wirk run: refusing to launch this actor: {0}. The pane would have had to resolve \
         `wirk` from a mutable or ambiguous location instead of this Run's own pinned runtime"
    )]
    RuntimePin(#[from] RuntimePinError),
    /// P4.1 (ruling 0202): this Run's World reserved a shared worker
    /// contract whose bytes this launch cannot verify — missing,
    /// unreadable, or not hashing to the reserved digest. Refused
    /// **before any pane side effect**, the same fail-closed posture
    /// `RuntimePin` above already takes, and for the reason 0202 gives:
    /// "a verified contract must actually be delivered, or launch must
    /// refuse; a swallowed overlay error followed by an
    /// instruction-less actor is not success".
    #[error(transparent)]
    Contract(#[from] worker_contract::ContractError),
}

/// Every way `build_selection_args` refuses a requested model/effort
/// (PREPARATION-ADJUDICATION.md point 2: "An explicitly requested
/// option must not silently degrade into a different execution ...
/// fail visibly before actor launch"). Never fired for an *absent*
/// model/effort — omitting one always launches bare of it, the
/// harness's own native default engaging unrepresented as any
/// particular value (adjudication point 4).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SelectionError {
    /// The harness has an installed, interactive `--model`/`-m`-shaped
    /// control (claude/opencode/codex all do) but no verified native
    /// way to set reasoning effort for it — requesting one anyway is
    /// refused rather than silently dropped or guessed at.
    #[error(
        "{kind} has no verified native effort control wirk can map to (requested {effort:?}); \
         pass a raw flag via the Route's own `selection.args` if {kind} actually supports one"
    )]
    UnsupportedEffort { kind: String, effort: String },
    /// A kind outside wirk's three verified harnesses (claude, opencode,
    /// codex — 0056 D164 still accepts any kind string Herdr does, this
    /// is not a kind allowlist): wirk has never inspected its
    /// interactive CLI, so guessing a flag for a requested model/effort
    /// would be exactly the silent-invention this contract forbids.
    /// `selection.args` remains the escape hatch — an author who knows
    /// the kind's real flag can pass it there directly.
    #[error(
        "{kind} is not one of wirk's verified harnesses (claude, opencode, codex); an explicit \
         model/effort request for it cannot be mapped without guessing its syntax — pass it via \
         the Route's own `selection.args` instead"
    )]
    UnmappedKind { kind: String },
    /// P3 native launch selection, D2: the same setting was requested
    /// twice, once structurally (`selection.model`/`selection.effort`)
    /// and once raw (`selection.args`), for the same harness. Both
    /// tokens would be submitted; which one the harness's own parser
    /// honors is its business, and wirk's recorded request would then
    /// describe only one of them.
    ///
    /// The rule is deliberately the narrowest one that cannot lie:
    /// **no implicit precedence — an overlap is refused, before
    /// launch.** Raw args remain the escape hatch for everything the
    /// convenience fields do not cover; they simply may not restate a
    /// field that was also given structurally. Removing either side
    /// resolves it, and the author says which one they meant.
    #[error(
        "{kind}: `selection.{field}` and the raw argument {raw:?} both set {kind}'s own \
         {flag} — wirk will not submit both and then record only one as the request; \
         drop `selection.{field}` or drop the raw argument"
    )]
    RawArgConflict {
        kind: String,
        field: String,
        flag: String,
        raw: String,
    },
}

/// The real, installed, interactive CLI controls for the three
/// harnesses wirk has actually inspected — verified by hand this wave
/// (`claude --help`, `opencode --help`, `codex --help` against the
/// binaries on this box), not copied from a headless/`exec` example
/// (PREPARATION-ADJUDICATION.md point 2, naming exactly that failure
/// mode in the preparation report's own illustrative code):
///
/// * **claude**: `--model <model>` and `--effort <level>` are both
///   real, direct interactive flags.
/// * **opencode**: `-m`/`--model <provider/model>` is a real
///   interactive flag; there is no effort/reasoning-effort control of
///   any kind on the interactive command (`opencode --help`'s full
///   option list carries none) — an explicit effort request for
///   opencode is `SelectionError::UnsupportedEffort`, not a silently
///   dropped flag.
/// * **codex**: `-m`/`--model <MODEL>` is a real interactive flag;
///   effort has no dedicated flag but a real, documented config
///   control, `model_reasoning_effort` (`~/.codex/config.toml`'s own
///   key, confirmed installed on this box), set the same way any other
///   Codex config override is — `-c model_reasoning_effort=<level>` —
///   never a codex-specific flag wirk would otherwise have to invent.
///
/// Any kind outside this verified set launches bare when no
/// model/effort is requested (0056 D164, unchanged); an explicit
/// request for one is `SelectionError::UnmappedKind`. `selection.args`
/// (raw pass-through, exact token boundaries preserved) is appended
/// after whatever this function maps, for every kind — the escape
/// hatch for anything neither convenience field covers.
/// The native spellings by which a raw `selection.args` token would set
/// the same thing a structured field sets, per verified harness — the
/// real syntax variants of the installed CLIs, checked by hand this
/// wave against `claude --help` (2.1.263), `opencode --help` (1.18.29)
/// and `codex --help` (0.153.4):
///
/// * claude: `--model <v>` / `--model=<v>`, `--effort <v>` /
///   `--effort=<v>`. There is no short alias for either.
///   `--fallback-model` is a different setting and is not a conflict.
/// * opencode: `-m <v>` / `--model <v>` / `--model=<v>` / `-m=<v>`.
///   opencode has no effort control at all, so no effort row exists.
/// * codex: `-m <v>` / `--model <v>` / `--model=<v>` / `-m=<v>`, plus
///   the config-override forms wirk itself uses for effort —
///   `-c`/`--config` followed by `model_reasoning_effort=<v>` for
///   effort, and by `model=<v>` for model, in both the separate-token
///   and `=`-joined spellings.
///
/// Returns the offending raw token when `args` restates `field`.
/// Nothing here parses the harness's whole command line: it recognizes
/// only the handful of spellings of the flags wirk itself emits, which
/// is exactly the overlap it has to be honest about.
/// The claude *append* controls a launch may already carry, if any —
/// the one concrete transport collision with wirk's own
/// `--append-system-prompt-file` delivery (review F3).
///
/// Exactly two flags, in both value forms claude's own parser accepts
/// (`--flag value` and `--flag=value`), because those are the two that
/// occupy the same mechanism. Deliberately **not** a category ban:
/// `--agent`, `--system-prompt`, `--system-prompt-file` and any profile
/// or base-prompt setting set what an append is appended *to*, compose
/// with it, and are neither inspected nor refused here (0202).
fn existing_claude_append_control(args: &[String]) -> Option<&'static str> {
    const APPEND_CONTROLS: [&str; 2] = ["--append-system-prompt", "--append-system-prompt-file"];
    args.iter().find_map(|arg| {
        APPEND_CONTROLS
            .into_iter()
            .find(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
    })
}

fn conflicting_raw_arg(kind: &str, field: SelectionField, args: &[String]) -> Option<String> {
    let (flags, config_keys): (&[&str], &[&str]) = match (kind, field) {
        ("claude", SelectionField::Model) => (&["--model"], &[]),
        ("claude", SelectionField::Effort) => (&["--effort"], &[]),
        ("opencode", SelectionField::Model) => (&["-m", "--model"], &[]),
        ("codex", SelectionField::Model) => (&["-m", "--model"], &["model"]),
        ("codex", SelectionField::Effort) => (&[], &["model_reasoning_effort"]),
        _ => (&[], &[]),
    };
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        for flag in flags {
            if arg.starts_with(&format!("{flag}=")) {
                return Some(arg.clone());
            }
            if arg == flag {
                // Report the value with the flag, so the error names
                // the whole offending pair the author actually wrote.
                return Some(match iter.peek() {
                    Some(value) => format!("{arg} {value}"),
                    None => arg.clone(),
                });
            }
        }
        if config_keys.is_empty() {
            continue;
        }
        // `-c key=value` / `--config key=value`, and their `=`-joined
        // spellings `-c=key=value` / `--config=key=value`.
        let joined = ["-c=", "--config="]
            .iter()
            .find_map(|prefix| arg.strip_prefix(prefix));
        let separate = if arg == "-c" || arg == "--config" {
            iter.peek().map(|next| next.as_str())
        } else {
            None
        };
        for key in config_keys {
            let prefix = format!("{key}=");
            if joined.is_some_and(|rest| rest.starts_with(&prefix))
                || separate.is_some_and(|next| next.starts_with(&prefix))
            {
                return Some(match separate {
                    Some(next) => format!("{arg} {next}"),
                    None => arg.clone(),
                });
            }
        }
    }
    None
}

/// Which structured field a conflict is about (`conflicting_raw_arg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionField {
    Model,
    Effort,
}

impl SelectionField {
    fn name(self) -> &'static str {
        match self {
            SelectionField::Model => "model",
            SelectionField::Effort => "effort",
        }
    }
}

/// P3 native launch selection D1/D2: everything about a resolved
/// request that can be judged without touching Herdr — the harness
/// mapping (`build_selection_args`) and the raw/structured overlap
/// (`conflicting_raw_arg`). Called by `RunLoop::launch` *before* it
/// binds the request or creates a pane, so an unmappable or
/// self-contradicting request costs no execution-side effect at all:
/// no pane, no journal entry, no agent.
pub fn validate_selection(
    kind: &str,
    selection: &wirk_core::ActorSelection,
) -> Result<(), SelectionError> {
    build_selection_args(kind, selection).map(|_| ())
}

fn build_selection_args(
    kind: &str,
    selection: &wirk_core::ActorSelection,
) -> Result<Vec<String>, SelectionError> {
    // D2: refuse before mapping anything. A structured field and a raw
    // restatement of the same harness flag are a conflict, never a
    // silent precedence — the recorded request would otherwise name one
    // token while two were submitted.
    for (field, requested) in [
        (SelectionField::Model, selection.model.is_some()),
        (SelectionField::Effort, selection.effort.is_some()),
    ] {
        if !requested {
            continue;
        }
        if let Some(raw) = conflicting_raw_arg(kind, field, &selection.args) {
            return Err(SelectionError::RawArgConflict {
                kind: kind.to_string(),
                field: field.name().to_string(),
                flag: match field {
                    SelectionField::Model => "model",
                    SelectionField::Effort => "reasoning effort",
                }
                .to_string(),
                raw,
            });
        }
    }
    let mut args = Vec::new();
    match kind {
        "claude" => {
            if let Some(model) = &selection.model {
                args.push("--model".to_string());
                args.push(model.clone());
            }
            if let Some(effort) = &selection.effort {
                args.push("--effort".to_string());
                args.push(effort.clone());
            }
        }
        "opencode" => {
            if let Some(model) = &selection.model {
                args.push("--model".to_string());
                args.push(model.clone());
            }
            if let Some(effort) = &selection.effort {
                return Err(SelectionError::UnsupportedEffort {
                    kind: kind.to_string(),
                    effort: effort.clone(),
                });
            }
        }
        "codex" => {
            if let Some(model) = &selection.model {
                args.push("--model".to_string());
                args.push(model.clone());
            }
            if let Some(effort) = &selection.effort {
                args.push("-c".to_string());
                args.push(format!("model_reasoning_effort={effort}"));
            }
        }
        other => {
            if selection.model.is_some() || selection.effort.is_some() {
                return Err(SelectionError::UnmappedKind {
                    kind: other.to_string(),
                });
            }
        }
    }
    args.extend(selection.args.iter().cloned());
    Ok(args)
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
        validate_selection(run.kind.0.as_str(), &run.selection)?;
        let mut contract = self.verified_contract(world)?;
        let mut claim_hook = None;
        let pane = self.actor_pane(run, world, contract.as_mut(), &mut claim_hook)?;
        self.start_actor_agent(
            run,
            &pane.pane_id,
            world,
            contract.as_mut(),
            &mut claim_hook,
        )?;
        Ok(())
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
