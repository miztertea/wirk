//! `RunLoop`: the executor's own drive loop around `HerdrExecutor`
//! (item 4, W2; `knowledge/work/p1-herdr-executor/orient/loop.md`,
//! `orient/build-brief.md` §2.2). Owns `PromptGate`'s real gating and
//! the first-prompt/continuation-prompt composition, and the journal
//! writes `launch`/`poll` deliberately never make (0017 D56: the
//! `Executor` trait is read/write-split) — sent through a `WirkdApi`
//! trait this item defines.
//!
//! Rebuilt for ruling 0044 (fix 2, W3): **wirk blocks on state, never
//! on time.** `drive` blocks on one `std::mpsc` channel fed by exactly
//! two reader threads — Herdr's own event subscription (`HerdrClient::
//! subscribe`, no read timeout: `read_line` to `EOF`, and `EOF` or a
//! read error *is* the observation "Herdr is gone", not a case needing
//! a timeout to detect) and wirkd's own `watch` stream (`WirkdApi::
//! watch`, item B: one line per journal append of this Work, `EOF`
//! meaning wirkd itself is gone). No `Clock`, no `nudge_after`, no
//! one-nudge budget, no inactivity timer, no `Reconciler`/
//! `event_identity` dedup (Herdr does not replay to a new subscription,
//! measured — `knowledge/work/p2-dogfood/orient/herdr-events-measured.md`
//! — so a status event is handled whenever it differs from the last one
//! this loop knew, which is what lets the pane's second Idle in a
//! Working/Idle/Idle sequence be seen and prompted, the run 2 bug
//! `knowledge/work/p2-dogfood/ASSESSMENT.md`'s last section names).
//!
//! Prompting (0044 D133): every time the pane goes Idle while the Run
//! is unclaimed (learned from the watch stream, never a status poll)
//! and the Work is not `NeedsInput` and the pane is not `Blocked`, it
//! is prompted to continue — the Waypoint's intent, the required
//! artifacts, and the literal claim instruction (`compose_first_prompt`,
//! reused for every prompt, not only the first). Prompting stops on a
//! `ClaimRecorded` for this Run (`Claimed`), the Work moving to
//! `NeedsInput`, Herdr saying the pane is gone, or **no progress**: a
//! prompt's own baseline (one `get_pane` call's `revision`, one
//! worktree fingerprint, `wirk_herdr::git::fingerprint`) compared
//! against the same two readings at the *next* Idle — unchanged is the
//! actor stuck (`Outcome::NeedsInput`, `stuck_observation()` names
//! what was observed); changed prompts again.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use thiserror::Error;

use wirk_core::{
    ActorWorld, Event, EventKind, FailureCause, Run, RunId, RunState, Timestamp, WorkId, WorkState,
    World, fold,
};

use crate::{
    AgentStatus, HerdrClient, HerdrError, HerdrEvent, HerdrExecutor, HerdrExecutorError,
    PromptAgent, PromptGate,
};

// ---- WirkdApi -------------------------------------------------------------

/// One Run's state, as `WirkdApi::status` reports it — a subset of
/// `wirk_core::RunState` the caller filters `WorkStatus::runs` for by
/// `RunId` (transport.md §2's `{work_id} -> {state, runs:[{id,state}]}`
/// shape, R2: `RunState` is reused verbatim, no parallel enum). Kept on
/// the trait for a caller that still wants a point-in-time read (e.g.
/// `wirk wirkd status`); `RunLoop::drive` itself no longer calls
/// `status` at all (fix 2: Claimed/NeedsInput are learned from the
/// watch stream, never polled).
#[derive(Debug, Clone)]
pub struct RunStatusEntry {
    pub run_id: RunId,
    pub state: RunState,
}

/// `WirkdApi::status`'s reply: the Work's own state (wirkd's `fold`
/// output) plus every Run it knows about.
#[derive(Debug, Clone)]
pub struct WorkStatus {
    pub work_state: WorkState,
    pub runs: Vec<RunStatusEntry>,
}

/// The wirkd calls `RunLoop` needs: `record` for the journal writes
/// `launch`/`poll` never make themselves (`RunLaunched`, `RunVanished`,
/// `RunFailed{cause}`, `LifecycleObserved{status}`), `watch` for the
/// blocking journal stream `drive` blocks on alongside Herdr's own
/// subscription (item B), and `status` kept for a caller that wants a
/// one-shot read outside `drive`'s own loop.
/// The blocking iterator `WirkdApi::watch` returns, factored into a
/// named alias only to keep the trait's own signature (and clippy's
/// type-complexity lint) readable — not a new abstraction.
pub type WatchEvents<E> = Box<dyn Iterator<Item = Result<Event, E>> + Send>;

pub trait WirkdApi: Send + Sync {
    type Error: std::error::Error + Send + 'static;
    fn status(&self, work_id: &WorkId) -> Result<WorkStatus, Self::Error>;
    fn record(&self, work_id: &WorkId, run_id: &RunId, kind: EventKind) -> Result<(), Self::Error>;
    /// Item B: a **blocking** iterator over `work_id`'s journal — every
    /// event already appended, then one more per line as wirkd pushes
    /// it (`server::handle_watch_connection`), ending only when the
    /// connection does (`EOF`: wirkd stopped, or refused the watch
    /// outright). `drive` reads this on its own thread, forwarding each
    /// item into the loop's one merged channel (module doc); the
    /// `+ Send` bound is what makes that forwarding thread legal to
    /// spawn.
    fn watch(&self, work_id: &WorkId) -> Result<WatchEvents<Self::Error>, Self::Error>;
}

impl<T: WirkdApi + ?Sized> WirkdApi for Arc<T> {
    type Error = T::Error;
    fn status(&self, work_id: &WorkId) -> Result<WorkStatus, Self::Error> {
        (**self).status(work_id)
    }
    fn record(&self, work_id: &WorkId, run_id: &RunId, kind: EventKind) -> Result<(), Self::Error> {
        (**self).record(work_id, run_id, kind)
    }
    fn watch(&self, work_id: &WorkId) -> Result<WatchEvents<Self::Error>, Self::Error> {
        (**self).watch(work_id)
    }
}

// ---- RunLoop ----------------------------------------------------------

/// What one `drive` can conclude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A `ClaimRecorded{Validated, Done}` for this Run arrived on the
    /// watch stream — the loop stops.
    Claimed,
    /// The Work moved to `NeedsInput` (a validated Question claim, or
    /// item C's own no-progress check) — treated like a human wait, not
    /// a failure; the loop stops prompting. `stuck_observation()` names
    /// what item C observed when this came from the no-progress check,
    /// `None` when it came from the watch stream instead.
    NeedsInput,
    /// Herdr's own subscription ended (`EOF`/read error) — journaled as
    /// `RunVanished`, the loop stops.
    Vanished,
    /// The merged channel ended with neither reader thread reporting
    /// its own stream's end (cannot happen by construction; kept so the
    /// channel-receive match stays exhaustive against `RecvError`).
    Pending,
}

#[derive(Debug, Error)]
pub enum RunLoopError<W: WirkdApi> {
    #[error(transparent)]
    Herdr(#[from] HerdrExecutorError),
    #[error("wirkd: {0}")]
    Wirkd(W::Error),
    /// wirkd's own `watch` stream ended (`EOF`) — wirkd itself is gone,
    /// so nothing can be journaled about it, including this fact (item
    /// A: "wirk run exits 5"). Carries the transport detail when the
    /// reader thread had one.
    #[error("wirkd watch stream ended: wirkd is gone{}", detail.as_ref().map(|d| format!(" ({d})")).unwrap_or_default())]
    WirkdGone { detail: Option<String> },
}

/// One item off the loop's single merged channel (module doc): either
/// stream's own event, or that stream ending.
enum LoopMsg {
    Herdr(HerdrEvent),
    HerdrEnded(Option<String>),
    Watch(Event),
    WatchEnded(Option<String>),
}

/// Captured right after a prompt is sent, compared against the same two
/// readings at the next Idle (item C's no-progress check).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgressBaseline {
    pane_revision: u64,
    fingerprint: String,
}

/// Drives one `Run` end to end against a `HerdrClient` + `WirkdApi`
/// (item 4, W2; rebuilt fix 2). Generic over the client and the wirkd
/// surface so every policy here is testable against fakes with no live
/// Herdr and no live wirkd (0040: the fakes behave like the services —
/// a channel the test feeds and closes, never a canned one-shot reply
/// standing in for a stream).
pub struct RunLoop<C: HerdrClient, W: WirkdApi> {
    executor: HerdrExecutor<C>,
    wirkd: W,
    prompt_gate: PromptGate,
    blocked: bool,
    claimed: bool,
    needs_input: bool,
    last_status: Option<AgentStatus>,
    /// The actor pane `launch` opened, once it has one: `pane.get`
    /// takes a structured pane id and never an agent name, so
    /// `progress_snapshot` asks by this once it is known.
    launched_pane: Option<String>,
    /// Reconstructed from the watch stream's own `Event`s, incrementally
    /// (`Run::apply` already ignores an event naming a different Run) —
    /// `None` until `drive` seeds it from its own `run` argument.
    run_state: Option<Run>,
    /// Every `Event` the watch stream has produced so far, oldest
    /// first — `wirk_core::fold` needs the full slice (it starts from
    /// `WorkSubmitted`), so this is threaded through rather than
    /// reduced to a single "current Work state" the loop updates
    /// in place.
    watch_events: Vec<Event>,
    progress_baseline: Option<ProgressBaseline>,
    /// Set by item C's no-progress check when it concludes the actor is
    /// stuck — the caller reads this alongside `Outcome::NeedsInput` to
    /// print what was observed (exit 4 stays as today; needs-input
    /// surfacing itself is P2.3).
    stuck_observation: Option<String>,
}

impl<C: HerdrClient, W: WirkdApi> RunLoop<C, W> {
    pub fn new(client: C, wirkd: W) -> Self {
        RunLoop {
            executor: HerdrExecutor::new(client),
            wirkd,
            prompt_gate: PromptGate::default(),
            blocked: false,
            claimed: false,
            needs_input: false,
            last_status: None,
            launched_pane: None,
            run_state: None,
            watch_events: Vec::new(),
            progress_baseline: None,
            stuck_observation: None,
        }
    }

    pub fn executor(&self) -> &HerdrExecutor<C> {
        &self.executor
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked
    }

    pub fn prompt_gate_busy(&self) -> bool {
        self.prompt_gate.busy
    }

    /// What item C's no-progress check observed, when `drive` returned
    /// `Outcome::NeedsInput` because of it (`None` when the Work simply
    /// moved to `NeedsInput` on the watch stream instead — the two
    /// `NeedsInput` causes are otherwise the same `Outcome` variant).
    pub fn stuck_observation(&self) -> Option<&str> {
        self.stuck_observation.as_deref()
    }

    /// Step 6 (loop.md §1): `HerdrExecutor::launch_actor`, then the
    /// journal write the executor itself never makes — `RunLaunched` on
    /// success, `RunFailed{cause.detail}` on failure (issue 275's
    /// shape).
    ///
    /// Returns the **one** subscription `launch_actor` opened for the
    /// actor's pane before `agent.start` (D51's ordering, fix 3): `drive`
    /// hands this to its own forwarding reader thread and never opens a
    /// second one.
    pub fn launch(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        world: &World,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>, RunLoopError<W>>
    {
        match self.executor.launch_actor(run, world) {
            Ok(launched) => {
                self.launched_pane = Some(launched.pane.pane_id.clone());
                self.wirkd
                    .record(
                        work_id,
                        &run.id,
                        EventKind::RunLaunched {
                            run: run.id.clone(),
                            actor_kind: run.kind,
                        },
                    )
                    .map_err(RunLoopError::Wirkd)?;
                Ok(launched.events)
            }
            Err(err) => {
                let detail = err.to_string();
                self.wirkd
                    .record(
                        work_id,
                        &run.id,
                        EventKind::RunFailed {
                            cause: FailureCause {
                                status: None,
                                request_id: None,
                                at: Timestamp(0),
                                detail: Some(detail),
                            },
                        },
                    )
                    .map_err(RunLoopError::Wirkd)?;
                Err(RunLoopError::Herdr(err))
            }
        }
    }

    /// Drives one `Run` end to end: opens wirkd's `watch` stream and
    /// Herdr's own subscription (via `launch`), spawns one reader thread
    /// per stream feeding a shared channel, then blocks on that channel
    /// (module doc) until a terminal `Outcome` or a fatal error.
    ///
    /// Every error that escapes **after** `RunLaunched` is journaled
    /// `RunFailed{cause.detail}` first (fix 3, 0028 tried step 3's
    /// second finding) — except `RunLoopError::WirkdGone`: wirkd being
    /// gone is precisely why nothing can be journaled about it.
    pub fn drive(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        world: &World,
    ) -> Result<Outcome, RunLoopError<W>> {
        let actor = match world {
            World::Actor(actor) => actor.clone(),
            World::Deterministic(_) => {
                return Err(RunLoopError::Herdr(
                    HerdrExecutorError::NotDeterministicKind,
                ));
            }
        };
        self.run_state = Some(run.clone());
        self.watch_events.clear();

        let watch_events = self.wirkd.watch(work_id).map_err(RunLoopError::Wirkd)?;
        let herdr_events = self.launch(work_id, run, world)?;

        let (tx, rx) = mpsc::channel::<LoopMsg>();
        spawn_herdr_reader(herdr_events, tx.clone());
        spawn_watch_reader(watch_events, tx);

        let outcome = self.drive_channel(work_id, run, &actor, rx);
        if let Err(err) = &outcome
            && !matches!(err, RunLoopError::WirkdGone { .. })
        {
            self.record_run_failed(work_id, run, &err.to_string());
        }
        outcome
    }

    /// Blocks on `rx.recv()` (module doc: no timeout) until a terminal
    /// `Outcome` or a fatal error.
    fn drive_channel(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        actor: &ActorWorld,
        rx: mpsc::Receiver<LoopMsg>,
    ) -> Result<Outcome, RunLoopError<W>> {
        loop {
            match rx.recv() {
                Ok(LoopMsg::Herdr(event)) => {
                    if let Some(outcome) = self.observe_herdr(work_id, run, actor, &event)? {
                        return Ok(outcome);
                    }
                }
                Ok(LoopMsg::HerdrEnded(_detail)) => {
                    self.wirkd
                        .record(work_id, &run.id, EventKind::RunVanished)
                        .map_err(RunLoopError::Wirkd)?;
                    return Ok(Outcome::Vanished);
                }
                Ok(LoopMsg::Watch(event)) => {
                    if let Some(outcome) = self.observe_watch(&event) {
                        return Ok(outcome);
                    }
                }
                Ok(LoopMsg::WatchEnded(detail)) => {
                    return Err(RunLoopError::WirkdGone { detail });
                }
                Err(_) => return Ok(Outcome::Pending),
            }
        }
    }

    /// One Herdr event: updates `last_status`/`blocked`, releases
    /// `PromptGate` on `working`, journals `LifecycleObserved` on every
    /// *changed* status (fix 2: no identity dedup — a status event is
    /// handled whenever it differs from the one this loop last knew, so
    /// a Working/Idle/Idle sequence's second Idle is seen even though
    /// its own content is identical to the first). On a changed Idle:
    /// item C's no-progress check first (if a prompt is awaiting its
    /// follow-up), then `maybe_prompt`.
    fn observe_herdr(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        actor: &ActorWorld,
        event: &HerdrEvent,
    ) -> Result<Option<Outcome>, RunLoopError<W>> {
        let HerdrEvent::PaneAgentStatusChanged { agent_status, .. } = event else {
            return Ok(None);
        };
        let changed = self.last_status != Some(*agent_status);
        self.last_status = Some(*agent_status);
        if !changed {
            return Ok(None);
        }

        self.blocked = matches!(agent_status, AgentStatus::Blocked);
        self.prompt_gate.release_on_working(*agent_status);
        self.wirkd
            .record(
                work_id,
                &run.id,
                EventKind::LifecycleObserved {
                    status: format!("{agent_status:?}"),
                },
            )
            .map_err(RunLoopError::Wirkd)?;

        if !matches!(agent_status, AgentStatus::Idle) {
            return Ok(None);
        }

        if let Some(baseline) = self.progress_baseline.take() {
            let now = self.progress_snapshot(actor);
            if now.as_ref() == Some(&baseline) {
                self.stuck_observation = Some(format!(
                    "no progress since the last prompt: pane revision {} and the worktree \
                     unchanged",
                    baseline.pane_revision
                ));
                return Ok(Some(Outcome::NeedsInput));
            }
        }

        self.maybe_prompt(run, actor)?;
        Ok(None)
    }

    /// One watch-stream `Event`: folds it onto the loop's own tracked
    /// `Run` (`Claimed` stops the loop) and the accumulated Work
    /// (`NeedsInput` stops it too) — the only two ways `drive_channel`
    /// ever learns either, never a status poll (item C).
    fn observe_watch(&mut self, event: &Event) -> Option<Outcome> {
        self.watch_events.push(event.clone());
        if let Some(run_state) = self.run_state.as_mut() {
            run_state.apply(event);
            if matches!(run_state.state, RunState::Claimed(_)) {
                self.claimed = true;
                return Some(Outcome::Claimed);
            }
        }
        // `fold` panics on a slice with no `WorkSubmitted` at all (its
        // own documented precondition) — a real journal always starts
        // with one, but a test's own fake watch stream may not have
        // pushed one, so this guards rather than requiring every test to
        // open with a `WorkSubmitted` it otherwise has no use for.
        let has_work_submitted = self
            .watch_events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkSubmitted { .. }));
        if has_work_submitted {
            let work = fold(&self.watch_events);
            if matches!(work.state, WorkState::NeedsInput) {
                self.needs_input = true;
                return Some(Outcome::NeedsInput);
            }
        }
        None
    }

    /// D133: prompted only while Idle (the caller's own guard),
    /// unclaimed, the Work not `NeedsInput`, and not `Blocked` — gated
    /// by `PromptGate` so a prompt already in flight never doubles up
    /// (D56). Captures item C's own baseline immediately after sending.
    fn maybe_prompt(&mut self, run: &Run, actor: &ActorWorld) -> Result<(), RunLoopError<W>> {
        if self.blocked || self.claimed || self.needs_input {
            return Ok(());
        }
        if !self.prompt_gate.try_acquire() {
            return Ok(());
        }
        let text = compose_first_prompt(actor);
        self.executor
            .client()
            .prompt_agent(PromptAgent {
                target: run.id.0.clone(),
                text,
            })
            .map_err(HerdrExecutorError::from)?;
        self.progress_baseline = self.progress_snapshot(actor);
        Ok(())
    }

    /// One `get_pane` request plus one worktree fingerprint (item C,
    /// literal: "one get_pane request and one worktree fingerprint") —
    /// `None` when the pane cannot be read (about to be Vanished on the
    /// merged channel regardless, so no baseline is ever compared
    /// against a reading that failed).
    fn progress_snapshot(&self, actor: &ActorWorld) -> Option<ProgressBaseline> {
        let pane_id = self.launched_pane.as_ref()?;
        let pane = self.executor.client().get_pane(pane_id).ok()?;
        let fingerprint = crate::git::fingerprint(&actor.worktree_path);
        Some(ProgressBaseline {
            pane_revision: pane.revision,
            fingerprint,
        })
    }

    /// `RunFailed{cause.detail}` for a failure the loop is about to
    /// return. Best-effort: the caller is already failing, and a wirkd
    /// that cannot take this write is itself the more visible problem —
    /// swallowing the record error here keeps the original cause as the
    /// one the caller reports.
    fn record_run_failed(&self, work_id: &WorkId, run: &Run, detail: &str) {
        let _ = self.wirkd.record(
            work_id,
            &run.id,
            EventKind::RunFailed {
                cause: FailureCause {
                    status: None,
                    request_id: None,
                    at: Timestamp(0),
                    detail: Some(detail.to_string()),
                },
            },
        );
    }
}

/// Forwards Herdr's own subscription into the merged channel (module
/// doc, reader thread (a)): every pushed event, then exactly one
/// `HerdrEnded` on `EOF` or a transport error — never both, never
/// neither, so `drive_channel`'s own exhaustiveness on `RecvError`
/// really is unreachable in practice.
fn spawn_herdr_reader(
    events: Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>,
    tx: mpsc::Sender<LoopMsg>,
) {
    std::thread::spawn(move || {
        for event in events {
            match event {
                Ok(event) => {
                    if tx.send(LoopMsg::Herdr(event)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx.send(LoopMsg::HerdrEnded(Some(err.to_string())));
                    return;
                }
            }
        }
        let _ = tx.send(LoopMsg::HerdrEnded(None));
    });
}

/// Forwards wirkd's own `watch` stream into the merged channel (module
/// doc, reader thread (b)): every appended `Event`, then exactly one
/// `WatchEnded` on `EOF` or a transport error.
fn spawn_watch_reader<E: std::error::Error + Send + 'static>(
    events: WatchEvents<E>,
    tx: mpsc::Sender<LoopMsg>,
) {
    std::thread::spawn(move || {
        for event in events {
            match event {
                Ok(event) => {
                    if tx.send(LoopMsg::Watch(event)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx.send(LoopMsg::WatchEnded(Some(err.to_string())));
                    return;
                }
            }
        }
        let _ = tx.send(LoopMsg::WatchEnded(None));
    });
}

/// The prompt sent every time an Idle pane is eligible (D133): the
/// Waypoint's intent, its required artifacts by name, and the literal
/// instruction to file `wirk claim` — reused for every prompt, not only
/// the first (fix 2: 0044 struck the one-nudge budget along with every
/// other count/timer). A formatting function, no new type (build-
/// brief.md §2.2, R6).
pub fn compose_first_prompt(actor: &ActorWorld) -> String {
    let required: Vec<&str> = actor
        .output_contract
        .0
        .iter()
        .filter(|a| a.required)
        .map(|a| a.name.as_str())
        .collect();
    let artifacts_line = if required.is_empty() {
        String::new()
    } else {
        format!("\n\nRequired artifacts (by name): {}", required.join(", "))
    };
    format!(
        "{intent}{artifacts_line}\n\nWhen you are done, file the claim from this pane: `wirk claim \
         --artifact <name>=<path> ... --done`. If you need input before you can finish, file \
         `wirk claim --question \"...\"` instead.",
        intent = actor.intent,
    )
}

// ---- FakeWirkdApi, for tests ---------------------------------------------
//
// Mirrors `fake.rs`'s `FakeHerdrClient`: a real channel the test feeds
// and closes (0040 D127 — a fake behaves like the service, never a
// canned one-shot reply standing in for a stream), calls recorded, a
// `Mutex` since `WirkdApi: Send + Sync`. Not `cfg(test)`: `tests/
// run_loop.rs` is a separate compilation unit and would not see a
// `cfg(test)`-gated item there (R6, same reasoning `fake.rs`'s own doc
// comment already gives).

/// A `WirkdApi` whose `watch` stream is a channel the test feeds
/// (`push_watch_event`) and closes (`close_watch`); `status`'s reply is
/// fixed in advance (kept for a caller that still reads it — `RunLoop`
/// itself no longer does); `record` calls are recorded, never actually
/// journaled anywhere.
#[derive(Debug)]
pub struct FakeWirkdApi {
    status_response: Mutex<Option<WorkStatus>>,
    recorded: Mutex<Vec<(WorkId, RunId, EventKind)>>,
    watch_tx: Mutex<Option<mpsc::Sender<Result<Event, FakeWirkdError>>>>,
    watch_rx: Mutex<Option<mpsc::Receiver<Result<Event, FakeWirkdError>>>>,
    status_calls: Mutex<u32>,
}

impl Default for FakeWirkdApi {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        FakeWirkdApi {
            status_response: Mutex::new(None),
            recorded: Mutex::new(Vec::new()),
            watch_tx: Mutex::new(Some(tx)),
            watch_rx: Mutex::new(Some(rx)),
            status_calls: Mutex::new(0),
        }
    }
}

impl FakeWirkdApi {
    pub fn with_status(self, status: WorkStatus) -> Self {
        *self.status_response.lock().unwrap() = Some(status);
        self
    }

    /// Replaces the configured `status` reply after construction.
    pub fn set_status(&self, status: WorkStatus) {
        *self.status_response.lock().unwrap() = Some(status);
    }

    pub fn recorded(&self) -> Vec<(WorkId, RunId, EventKind)> {
        self.recorded.lock().unwrap().clone()
    }

    /// How many times `status` was actually called — test (3)'s own
    /// assertion that a `ClaimRecorded` on the watch stream stops the
    /// loop with **no** status call made.
    pub fn status_calls(&self) -> u32 {
        *self.status_calls.lock().unwrap()
    }

    /// Feeds one more line onto the fake `watch` stream — the test's
    /// own "wirkd appended this event" (0040: a real channel, not a
    /// canned `Vec`).
    pub fn push_watch_event(&self, event: Event) {
        if let Some(tx) = self.watch_tx.lock().unwrap().as_ref() {
            let _ = tx.send(Ok(event));
        }
    }

    /// Ends the fake `watch` stream (`EOF`) — the test's own "wirkd
    /// stopped" or "the connection ended".
    pub fn close_watch(&self) {
        *self.watch_tx.lock().unwrap() = None;
    }
}

#[derive(Debug, Error, Clone)]
#[error("FakeWirkdApi: {0}")]
pub struct FakeWirkdError(pub String);

impl WirkdApi for FakeWirkdApi {
    type Error = FakeWirkdError;

    fn status(&self, _work_id: &WorkId) -> Result<WorkStatus, Self::Error> {
        *self.status_calls.lock().unwrap() += 1;
        self.status_response
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| FakeWirkdError("no status configured".to_string()))
    }

    fn record(&self, work_id: &WorkId, run_id: &RunId, kind: EventKind) -> Result<(), Self::Error> {
        self.recorded
            .lock()
            .unwrap()
            .push((work_id.clone(), run_id.clone(), kind));
        Ok(())
    }

    fn watch(&self, _work_id: &WorkId) -> Result<WatchEvents<Self::Error>, Self::Error> {
        let rx =
            self.watch_rx.lock().unwrap().take().ok_or_else(|| {
                FakeWirkdError("watch already taken (one drive() per fake)".into())
            })?;
        Ok(Box::new(rx.into_iter()))
    }
}
