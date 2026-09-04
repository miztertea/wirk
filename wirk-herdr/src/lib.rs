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
//! `HerdrClient` trait; `event_identity`/`Reconciler` (D51's dedup and
//! rebind); `HerdrExecutor`, implementing `wirk_core::Executor`;
//! `PromptGate` (item 4's per-pane serialisation, D56, named here only).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use thiserror::Error;

pub mod fake;

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

/// Split direction for `SplitPane` (schema `event.$defs.SplitDirection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
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
    PaneAgentStatusChanged { pane_id: String },
    PaneOutputMatched { pane_id: String },
    PaneScrollChanged { pane_id: String },
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
/// `Transport` is socket-level, item 4's concern.
#[derive(Debug, Clone, Error)]
pub enum HerdrError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("blocked: {0}")]
    Blocked(String),
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
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>>>, HerdrError>;
}

// ---- Event identity for dedup (D51) --------------------------------------

/// Identity for deduplicating replayed `HerdrEvent`s (0017 D51). For
/// `PaneCreated`/`PaneUpdated`, `"<type>:<pane_id>:<revision>"` — the
/// schema nests a full `PaneInfo` carrying `revision` on exactly these
/// two variants. For every other variant, `"<type>:"` plus the
/// lowercase-hex SHA-256 of `serde_json::to_string(e)`: no other
/// variant carries a sequence, id, or revision field
/// (`herdr api schema --json`, protocol 20 — `event_id`/`timestamp`
/// occur nowhere in the schema; every `seq` hit is an outgoing request
/// param, not an identity Herdr stamps on emitted events).
///
/// Collision assumption (R7 — no lower rung covers an ad hoc envelope
/// shape; J1, herdr.md §2): Herdr never emits two content-identical
/// events for distinct facts. This is a real gap the schema itself does
/// not close, and is re-checked at each Herdr protocol bump (currently
/// 20).
pub fn event_identity(e: &HerdrEvent) -> String {
    match e {
        HerdrEvent::PaneCreated { pane } => {
            format!("pane_created:{}:{}", pane.pane_id, pane.revision)
        }
        HerdrEvent::PaneUpdated { pane } => {
            format!("pane_updated:{}:{}", pane.pane_id, pane.revision)
        }
        other => {
            let type_name = event_type_name(other);
            let json = serde_json::to_string(other).expect("HerdrEvent always serializes");
            let mut hasher = Sha256::new();
            hasher.update(json.as_bytes());
            let digest = hasher.finalize();
            format!("{type_name}:{}", hex_lower(&digest))
        }
    }
}

/// The schema's underscored `type` tag for a `HerdrEvent` variant,
/// matching `#[serde(tag = "type", rename_all = "snake_case")]` above.
fn event_type_name(e: &HerdrEvent) -> &'static str {
    match e {
        HerdrEvent::WorkspaceCreated { .. } => "workspace_created",
        HerdrEvent::WorkspaceClosed { .. } => "workspace_closed",
        HerdrEvent::WorkspaceMetadataUpdated { .. } => "workspace_metadata_updated",
        HerdrEvent::WorkspaceFocused { .. } => "workspace_focused",
        HerdrEvent::WorktreeOpened { .. } => "worktree_opened",
        HerdrEvent::WorktreeRemoved { .. } => "worktree_removed",
        HerdrEvent::TabCreated { .. } => "tab_created",
        HerdrEvent::TabFocused { .. } => "tab_focused",
        HerdrEvent::PaneCreated { .. } => "pane_created",
        HerdrEvent::PaneUpdated { .. } => "pane_updated",
        HerdrEvent::PaneClosed { .. } => "pane_closed",
        HerdrEvent::PaneFocused { .. } => "pane_focused",
        HerdrEvent::PaneMoved { .. } => "pane_moved",
        HerdrEvent::PaneExited { .. } => "pane_exited",
        HerdrEvent::PaneAgentDetected { .. } => "pane_agent_detected",
        HerdrEvent::PaneAgentStatusChanged { .. } => "pane_agent_status_changed",
    }
}

/// Lowercase hex encoding of a byte slice (stdlib `format!`, R3 — no
/// hex crate needed for this one call site; same approach as
/// `wirk-core`'s `WorldHash::of`).
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String never fails");
    }
    out
}

// ---- Reconciler -----------------------------------------------------------

/// Dedup-by-identity (`admit`) and terminal_id-keyed rebind (`rebind`)
/// above the `HerdrClient` trait (0017 D51). Holds no client and opens
/// no socket.
#[derive(Debug, Default)]
pub struct Reconciler {
    seen: BTreeSet<String>,
    bindings: BTreeMap<String, PaneBinding>,
}

impl Reconciler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admits an event by its `event_identity`. Returns `false` on a
    /// replay (an identity already seen) rather than re-processing it.
    pub fn admit(&mut self, e: &HerdrEvent) -> bool {
        self.seen.insert(event_identity(e))
    }

    /// Rebinds every known `PaneBinding`'s `Bearing` from a fresh
    /// `Snapshot`, keyed by `terminal_id` (0017 D51; D9 #5). Returns
    /// the `terminal_id`s present before the call but absent from
    /// `snapshot` — vanished, not resolved by inference.
    pub fn rebind(&mut self, snapshot: &Snapshot) -> Vec<String> {
        let mut by_terminal: BTreeMap<&str, &Bearing> = BTreeMap::new();
        for bearing in &snapshot.workspaces {
            by_terminal.insert(bearing.terminal_id.as_str(), bearing);
        }
        let mut vanished = Vec::new();
        for (terminal_id, binding) in self.bindings.iter_mut() {
            match by_terminal.get(terminal_id.as_str()) {
                Some(fresh) => binding.bearing = (*fresh).clone(),
                None => vanished.push(terminal_id.clone()),
            }
        }
        vanished
    }

    /// Adds or replaces a binding, keyed by its `terminal_id` (test and
    /// executor setup helper — the initial bind, distinct from
    /// `rebind`'s update-in-place).
    pub fn bind(&mut self, binding: PaneBinding) {
        self.bindings.insert(binding.terminal_id.clone(), binding);
    }

    pub fn binding(&self, terminal_id: &str) -> Option<&PaneBinding> {
        self.bindings.get(terminal_id)
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

impl<C: HerdrClient> HerdrExecutor<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &C {
        &self.client
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

    fn launch(&self, run: &wirk_core::Run, world: &wirk_core::World) -> Result<(), Self::Error> {
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

        let pane = self.client.split_pane(SplitPane {
            workspace_id: None,
            target_pane_id: None,
            direction: SplitDirection::Vertical,
            cwd: actor.worktree_path.clone(),
            env,
        })?;

        // Subscribe to this pane's status changes before starting the
        // agent, so no early transition is missed (D51/D52). The
        // returned iterator is not consumed here: draining it is the
        // caller's event loop, out of this trait method's scope.
        let _subscription =
            self.client
                .subscribe(vec![EventSubscription::PaneAgentStatusChanged {
                    pane_id: pane.pane_id.clone(),
                }])?;

        self.client.start_agent(StartAgent {
            pane_id: pane.pane_id,
            kind: "claude".to_string(),
            name: run.id.0.clone(),
            args: Vec::new(),
            timeout_ms: None,
        })?;

        Ok(())
    }

    fn poll(&self, run: &wirk_core::Run) -> Result<wirk_core::RunObservation, Self::Error> {
        // `run.waypoint`/`run.id` do not directly carry a pane_id in
        // this item's scope (the binding lives in a `Reconciler` the
        // caller owns, per D51); `poll` here reads the pane by the
        // `run.id` string as the target Herdr was given at launch,
        // matching how `StartAgent.name` was set above.
        match self.client.get_pane(&run.id.0) {
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
