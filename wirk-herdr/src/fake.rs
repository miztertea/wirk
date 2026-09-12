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
/// `RefCell` is not. Only `split_pane`, `get_pane`, `read_pane`,
/// `snapshot`, and `subscribe` are configurable; every other verb
/// returns an inert `Ok` (or `Err(Transport)` where there is no
/// sensible default), since no D9 test in this item exercises them.
#[derive(Default)]
pub struct FakeHerdrClient {
    pub split_pane_calls: Mutex<Vec<SplitPane>>,
    pub split_pane_response: Mutex<Option<PaneInfo>>,
    pub get_pane_responses: Mutex<BTreeMap<String, Result<PaneInfo, HerdrError>>>,
    /// P2.6 W2 (ruling 0052 D156): scripted `pane.read` replies, same
    /// shape as `get_pane_responses`; unset defaults to `Ok(String::new())`
    /// (unlike `get_pane`'s `NotFound` default) since most existing
    /// tests never touch `Blocked` and should not have to configure it.
    pub pane_read_responses: Mutex<BTreeMap<String, Result<String, HerdrError>>>,
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
    /// P2.5 W2: one reply per call, popped in order, defaulting once the
    /// queue is empty (every test predating this wave leaves it unset)
    /// to `Ok([kind] + args)` — the same `argv = [executable] + args`
    /// shape real Herdr's own `agent_started.argv` returns
    /// (`refs/herdr` `0f8ad12` `src/app/agents.rs::start_agent`), so a
    /// fake that never configures this still "behaves like the
    /// service" (0040 D127) rather than an inert `Ok(())` a real
    /// `agent.start` reply never actually shapes like. Lets a test
    /// script Herdr's `agent_pane_busy` refusal on the first N attempts
    /// and `Ok` after, the way `with_subscribe_channel` scripts events.
    pub start_agent_responses: Mutex<VecDeque<Result<Vec<String>, HerdrError>>>,
    /// Fix 2 (item C, D133): records every `agent.prompt` call so a
    /// `RunLoop` test can assert how many prompts were sent and what
    /// they said.
    pub prompt_agent_calls: Mutex<Vec<PromptAgent>>,
    /// P2.3 W1 (states.md §2): records every `notification.show` call
    /// so a `RunLoop` test can assert `notify` fired exactly once on
    /// the stuck-actor path, with the run id in `body` — 0040: a real
    /// recording fake, not a canned reply standing in for the call.
    pub notify_calls: Mutex<Vec<Notify>>,
    /// P2.6 W3 (rerun findings; ruling 0052): records every `pane.close`
    /// call so a test can assert the driver released a stale Run's pane
    /// before launching a retry's own, the way `split_pane_calls`
    /// already does for `split_pane`.
    pub close_pane_calls: Mutex<Vec<String>>,
    /// Ruling 0205: the environment a pane created by this fake
    /// carries — standing in for the Herdr *server's* environment,
    /// which is what a real pane inherits when the caller passes no
    /// `env` of its own. Empty by default, which is exactly this
    /// estate's own development server today; a test sets it to pin
    /// the counterexample a driver-side `std::env::var` could never
    /// see.
    pub pane_env: Mutex<BTreeMap<String, String>>,
    /// Every `pane.send_text` this fake received, as
    /// `(pane_id, text)` — the same recording `split_pane_calls`
    /// already does for `split_pane`.
    pub send_input_calls: Mutex<Vec<(String, String)>>,
    /// One `split_pane` reply per call, popped in order, falling back
    /// to `split_pane_response` once empty — the same shape
    /// `start_agent_responses` already has. Ruling 0205: an opencode
    /// launch now splits a short-lived probe pane before the actor's
    /// own, and a test that cares which pane was closed needs the two
    /// to have different ids.
    pub split_pane_responses: Mutex<VecDeque<PaneInfo>>,
    /// When set, every `pane.send_text` fails with it — standing in for
    /// a pane that cannot be reached at all, so the disclosed-fallback
    /// path is reachable without waiting out a real deadline.
    pub send_input_error: Mutex<Option<HerdrError>>,
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

    pub fn with_pane_read_response(
        self,
        pane_id: &str,
        result: Result<String, HerdrError>,
    ) -> Self {
        self.pane_read_responses
            .lock()
            .unwrap()
            .insert(pane_id.to_string(), result);
        self
    }

    /// Ruling 0205: replies for successive `split_pane` calls.
    pub fn with_split_pane_responses(self, panes: Vec<PaneInfo>) -> Self {
        *self.split_pane_responses.lock().unwrap() = panes.into();
        self
    }

    /// Ruling 0205: makes every `pane.send_text` fail.
    pub fn with_send_input_error(self, error: HerdrError) -> Self {
        *self.send_input_error.lock().unwrap() = Some(error);
        self
    }

    /// Ruling 0205: the environment panes created by this fake carry.
    pub fn with_pane_env(self, env: BTreeMap<String, String>) -> Self {
        *self.pane_env.lock().unwrap() = env;
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

    /// Scripts `start_agent`'s replies in call order (P2.5 W2) — e.g.
    /// `[Err(agent_pane_busy), Ok(vec![])]` for "busy once then
    /// accepts" (an empty `Ok` argv when a test does not care what
    /// Herdr says it submitted).
    pub fn with_start_agent_responses(
        self,
        responses: Vec<Result<Vec<String>, HerdrError>>,
    ) -> Self {
        *self.start_agent_responses.lock().unwrap() = responses.into();
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
    /// A fake is not a Herdr session; it says so rather than borrowing
    /// a real destination's shape.
    fn destination(&self) -> String {
        "fake-herdr".to_string()
    }

    fn create_workspace(&self, _req: CreateWorkspace) -> Result<WorkspaceInfo, HerdrError> {
        Err(HerdrError::Transport(
            "FakeHerdrClient: create_workspace not configured".to_string(),
        ))
    }

    fn split_pane(&self, req: SplitPane) -> Result<PaneInfo, HerdrError> {
        self.split_pane_calls.lock().unwrap().push(req);
        if let Some(pane) = self.split_pane_responses.lock().unwrap().pop_front() {
            return Ok(pane);
        }
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

    /// A pane is a shell with an environment, so this fake is one
    /// (0040 D127 — "behaves like the service", not a canned reply a
    /// real `pane.send_text` never shapes like): it records the text
    /// and then actually runs it, with `pane_env` above applied over
    /// this process's own environment and the two opencode
    /// configuration variables **removed first**. That removal is the
    /// point: whatever the test process itself carries, a pane created
    /// by this fake carries only what `pane_env` says — the same way a
    /// real pane's environment comes from the Herdr server and never
    /// from the wirk driver (ruling 0205). Any failure to spawn is
    /// swallowed: the caller's own timeout is what a silent pane means.
    fn send_input(&self, pane_id: &str, text: &str) -> Result<(), HerdrError> {
        self.send_input_calls
            .lock()
            .unwrap()
            .push((pane_id.to_string(), text.to_string()));
        if let Some(error) = self.send_input_error.lock().unwrap().clone() {
            return Err(error);
        }
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(text)
            .env_remove(crate::claim_hook::OPENCODE_CONFIG_ENV)
            .env_remove(crate::claim_hook::OPENCODE_CONFIG_CONTENT_ENV)
            .envs(self.pane_env.lock().unwrap().iter())
            .status();
        Ok(())
    }

    fn start_agent(&self, req: StartAgent) -> Result<Vec<String>, HerdrError> {
        let default_argv: Vec<String> = std::iter::once(req.kind.clone())
            .chain(req.args.iter().cloned())
            .collect();
        self.start_agent_calls.lock().unwrap().push(req);
        self.start_agent_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(default_argv))
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

    fn read_pane(&self, pane_id: &str) -> Result<String, HerdrError> {
        self.pane_read_responses
            .lock()
            .unwrap()
            .get(pane_id)
            .cloned()
            .unwrap_or_else(|| Ok(String::new()))
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

    fn close_pane(&self, pane_id: &str) -> Result<(), HerdrError> {
        self.close_pane_calls
            .lock()
            .unwrap()
            .push(pane_id.to_string());
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

    fn notify(&self, req: Notify) -> Result<(), HerdrError> {
        self.notify_calls.lock().unwrap().push(req);
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
