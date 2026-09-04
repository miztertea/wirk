//! `FakeHerdrClient`: a fixed-response `HerdrClient` for the D9 contract
//! tests (W3, BRIEF.md "Part B" tests section — no sleep anywhere,
//! issue 359). Not gated behind `cfg(test)`: `wirk-herdr/tests/contracts.rs`
//! is a separate compilation unit that links the crate as built, so a
//! `cfg(test)`-gated item would not be visible there; kept as a plain
//! `pub mod fake` instead (R6 — the simplest shape that is actually
//! visible to an integration test, no feature flag or dev-dependency
//! self-reference needed for one small fake).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use crate::{
    AgentStatus, CloseWorkspace, CreateWorkspace, EventSubscription, FocusPane, HerdrClient,
    HerdrError, HerdrEvent, Notify, OpenWorktree, PaneInfo, PromptAgent, ReleaseAgent,
    RemoveWorktree, ReportAgent, ReportAgentSession, ReportMetadata, SendKeys, Snapshot, SplitPane,
    StartAgent, WorkspaceInfo, WorktreeInfo,
};

/// A `HerdrClient` whose responses are fixed in advance, recording the
/// requests it receives. `Mutex`, not `RefCell` (R6): `HerdrClient:
/// Send + Sync` requires interior mutability that is `Sync`, and
/// `RefCell` is not. Only `split_pane`, `get_pane`, `snapshot`, and
/// `subscribe` are configurable; every other verb returns an inert `Ok`
/// (or `Err(Transport)` where there is no sensible default), since no
/// D9 test in this item exercises them.
#[derive(Default)]
pub struct FakeHerdrClient {
    pub split_pane_calls: Mutex<Vec<SplitPane>>,
    pub split_pane_response: Mutex<Option<PaneInfo>>,
    pub get_pane_responses: Mutex<BTreeMap<String, Result<PaneInfo, HerdrError>>>,
    pub snapshots: Mutex<VecDeque<Snapshot>>,
    pub subscribe_events: Mutex<Vec<HerdrEvent>>,
    /// Fix 2 (0040, ruling 0044): a real channel a test can feed and
    /// close, standing in for Herdr's own blocking subscription — used
    /// instead of `subscribe_events`'s fixed `Vec` when set
    /// (`with_subscribe_channel`), since `RunLoop::drive`'s tests need
    /// to control exactly when the subscription "ends" relative to the
    /// wirkd watch fake, not dump every event at once.
    pub subscribe_channel: Mutex<Option<std::sync::mpsc::Receiver<Result<HerdrEvent, HerdrError>>>>,
    /// W1 (0041 D129): records every `agent.start` call so a test can
    /// assert `kind`/`args` per actor kind, the way `split_pane_calls`
    /// already does for `split_pane`.
    pub start_agent_calls: Mutex<Vec<StartAgent>>,
    /// Fix 2 (item C, D133): records every `agent.prompt` call so a
    /// `RunLoop` test can assert how many prompts were sent and what
    /// they said.
    pub prompt_agent_calls: Mutex<Vec<PromptAgent>>,
}

impl FakeHerdrClient {
    pub fn with_split_pane_response(self, pane: PaneInfo) -> Self {
        *self.split_pane_response.lock().unwrap() = Some(pane);
        self
    }

    pub fn with_get_pane_response(
        self,
        pane_id: &str,
        result: Result<PaneInfo, HerdrError>,
    ) -> Self {
        self.get_pane_responses
            .lock()
            .unwrap()
            .insert(pane_id.to_string(), result);
        self
    }

    pub fn with_snapshots(self, snapshots: Vec<Snapshot>) -> Self {
        *self.snapshots.lock().unwrap() = snapshots.into();
        self
    }

    pub fn with_subscribe_events(self, events: Vec<HerdrEvent>) -> Self {
        *self.subscribe_events.lock().unwrap() = events;
        self
    }

    /// Real-channel form (fix 2, 0040): the test keeps the paired
    /// `Sender`, pushing `Ok(event)` to simulate a pushed Herdr event
    /// and dropping it (or sending `Err`) to simulate the subscription
    /// ending — `RunLoop`'s own reader thread reads this exactly like a
    /// live `SocketClient::subscribe` iterator.
    pub fn with_subscribe_channel(
        self,
        rx: std::sync::mpsc::Receiver<Result<HerdrEvent, HerdrError>>,
    ) -> Self {
        *self.subscribe_channel.lock().unwrap() = Some(rx);
        self
    }
}

impl HerdrClient for FakeHerdrClient {
    fn create_workspace(&self, _req: CreateWorkspace) -> Result<WorkspaceInfo, HerdrError> {
        Err(HerdrError::Transport(
            "FakeHerdrClient: create_workspace not configured".to_string(),
        ))
    }

    fn split_pane(&self, req: SplitPane) -> Result<PaneInfo, HerdrError> {
        self.split_pane_calls.lock().unwrap().push(req);
        self.split_pane_response
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| {
                HerdrError::Transport("FakeHerdrClient: split_pane_response not set".to_string())
            })
    }

    fn open_worktree(&self, _req: OpenWorktree) -> Result<WorktreeInfo, HerdrError> {
        Err(HerdrError::Transport(
            "FakeHerdrClient: open_worktree not configured".to_string(),
        ))
    }

    fn remove_worktree(&self, _req: RemoveWorktree) -> Result<(), HerdrError> {
        Ok(())
    }

    fn send_input(&self, _pane_id: &str, _text: &str) -> Result<(), HerdrError> {
        Ok(())
    }

    fn start_agent(&self, req: StartAgent) -> Result<(), HerdrError> {
        self.start_agent_calls.lock().unwrap().push(req);
        Ok(())
    }

    fn prompt_agent(&self, req: PromptAgent) -> Result<(), HerdrError> {
        self.prompt_agent_calls.lock().unwrap().push(req);
        Ok(())
    }

    fn wait_agent(
        &self,
        _target: &str,
        _until: AgentStatus,
        _timeout_ms: u64,
    ) -> Result<AgentStatus, HerdrError> {
        Ok(AgentStatus::Working)
    }

    fn get_pane(&self, pane_id: &str) -> Result<PaneInfo, HerdrError> {
        self.get_pane_responses
            .lock()
            .unwrap()
            .get(pane_id)
            .cloned()
            .unwrap_or_else(|| Err(HerdrError::NotFound(pane_id.to_string())))
    }

    fn get_agent(&self, target: &str) -> Result<PaneInfo, HerdrError> {
        self.get_pane(target)
    }

    fn list_agents(&self) -> Result<Vec<PaneInfo>, HerdrError> {
        Ok(Vec::new())
    }

    fn send_keys(&self, _req: SendKeys) -> Result<(), HerdrError> {
        Ok(())
    }

    fn release_agent(&self, _req: ReleaseAgent) -> Result<(), HerdrError> {
        Ok(())
    }

    fn close_pane(&self, _pane_id: &str) -> Result<(), HerdrError> {
        Ok(())
    }

    fn close_workspace(&self, _req: CloseWorkspace) -> Result<(), HerdrError> {
        Ok(())
    }

    fn snapshot(&self) -> Result<Snapshot, HerdrError> {
        self.snapshots
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| HerdrError::Transport("FakeHerdrClient: no snapshot queued".to_string()))
    }

    fn report_agent_session(&self, _req: ReportAgentSession) -> Result<(), HerdrError> {
        Ok(())
    }

    fn report_agent(&self, _req: ReportAgent) -> Result<(), HerdrError> {
        Ok(())
    }

    fn report_metadata(&self, _req: ReportMetadata) -> Result<(), HerdrError> {
        Ok(())
    }

    fn notify(&self, _req: Notify) -> Result<(), HerdrError> {
        Ok(())
    }

    fn focus_pane(&self, _req: FocusPane) -> Result<(), HerdrError> {
        Ok(())
    }

    fn subscribe(
        &self,
        _subs: Vec<EventSubscription>,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>, HerdrError> {
        if let Some(rx) = self.subscribe_channel.lock().unwrap().take() {
            return Ok(Box::new(rx.into_iter()));
        }
        let events: Vec<Result<HerdrEvent, HerdrError>> = self
            .subscribe_events
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(Ok)
            .collect();
        Ok(Box::new(events.into_iter()))
    }
}
