//! `RunLoop`: the executor's own drive loop around `HerdrExecutor`
//! (item 4, W2; `knowledge/work/p1-herdr-executor/orient/loop.md`,
//! `orient/build-brief.md` §2.2). Owns the `Reconciler`, `PromptGate`'s
//! real gating, an injected `Clock`, the nudge policy (0001 D3; issue
//! 274), the blocked policy (0017 D52), the first-prompt composition,
//! and the journal writes `launch`/`poll` deliberately never make
//! (0017 D56: the `Executor` trait is read/write-split) — sent through
//! a `WirkdApi` trait this item defines. wirkd's own `status` verb
//! answers only `{state, current_waypoint, events}` today (`wirk/src/
//! wirkd/server.rs::handle_status`), not the `{state, runs:[{id,
//! state}]}` shape `build-brief.md` §2.2 names; `WirkdApi` here names
//! the shape item 4 needs against a fake, and widening wirkd's real
//! reply to match is W3's job (R6 — no lower rung supplies it; the
//! trait is the minimum this item can build against without wirkd
//! wired up).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;

use wirk_core::{
    ActorWorld, EventKind, Executor, FailureCause, Run, RunId, RunObservation, RunState, Timestamp,
    WorkId, WorkState, World,
};

use crate::{
    AgentStatus, HerdrClient, HerdrEvent, HerdrExecutor, HerdrExecutorError, PromptAgent,
    PromptGate, Reconciler,
};

// ---- Clock --------------------------------------------------------------

/// Injected so `RunLoop`'s inactivity/nudge logic never sleeps as a
/// wait (issue 359) — a test drives time forward explicitly.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The real clock, for the `wirk` bin (W3).
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test clock: starts at the moment it is constructed, then only ever
/// moves forward on an explicit `advance` call — never wall-clock time
/// (issue 359).
pub struct ManualClock {
    now: Mutex<Instant>,
}

impl ManualClock {
    pub fn new() -> Self {
        ManualClock {
            now: Mutex::new(Instant::now()),
        }
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap();
        *now += by;
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
}

/// So a test can hold an `Arc<ManualClock>` (advancing it after handing
/// a clone to `RunLoop::new`) or an `Arc<FakeWirkdApi>` (reading
/// `recorded()` after handing a clone to `RunLoop::new`) and still
/// satisfy `RunLoop`'s `K: Clock`/`W: WirkdApi` bounds directly.
impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn now(&self) -> Instant {
        (**self).now()
    }
}

// ---- WirkdApi -------------------------------------------------------------

/// One Run's state, as `WirkdApi::status` reports it — a subset of
/// `wirk_core::RunState` the caller filters `WorkStatus::runs` for by
/// `RunId` (transport.md §2's `{work_id} -> {state, runs:[{id,state}]}`
/// shape, R2: `RunState` is reused verbatim, no parallel enum).
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

/// The wirkd calls `RunLoop` needs: `status` to learn a Run reached
/// `Claimed` or the Work moved to `NeedsInput` (loop.md §1 rows 8, 10 —
/// wirkd never pushes, the executor polls), and `record` for the
/// journal writes `launch`/`poll` never make themselves (`RunLaunched`,
/// `RunVanished`, `RunFailed{cause}`, `LifecycleObserved{status}`).
pub trait WirkdApi: Send + Sync {
    type Error: std::error::Error + 'static;
    fn status(&self, work_id: &WorkId) -> Result<WorkStatus, Self::Error>;
    fn record(&self, work_id: &WorkId, run_id: &RunId, kind: EventKind) -> Result<(), Self::Error>;
}

impl<T: WirkdApi + ?Sized> WirkdApi for Arc<T> {
    type Error = T::Error;
    fn status(&self, work_id: &WorkId) -> Result<WorkStatus, Self::Error> {
        (**self).status(work_id)
    }
    fn record(&self, work_id: &WorkId, run_id: &RunId, kind: EventKind) -> Result<(), Self::Error> {
        (**self).record(work_id, run_id, kind)
    }
}

// ---- RunLoop ----------------------------------------------------------

/// What one `drive` (or the individual steps below, driven by hand in
/// tests) can conclude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// wirkd's `status` reports this Run `Claimed` — the loop stops.
    Claimed,
    /// The Work moved to `NeedsInput` (D87: a Question claim) — treated
    /// like a human wait, not a failure; the loop stops prompting.
    NeedsInput,
    /// The pane vanished (D53) — journaled, the loop stops.
    Vanished,
    /// No terminal condition yet; there is more to drive.
    Pending,
}

#[derive(Debug, Error)]
pub enum RunLoopError<W: WirkdApi> {
    #[error(transparent)]
    Herdr(#[from] HerdrExecutorError),
    #[error("wirkd: {0}")]
    Wirkd(W::Error),
}

/// Drives one `Run` end to end against a `HerdrClient` + `WirkdApi`
/// (item 4, W2). Generic over the client, the wirkd surface, and the
/// clock so every policy below is testable against fakes with no live
/// Herdr and no sleep (issue 359).
pub struct RunLoop<C: HerdrClient, W: WirkdApi, K: Clock> {
    executor: HerdrExecutor<C>,
    wirkd: W,
    clock: K,
    reconciler: Reconciler,
    prompt_gate: PromptGate,
    nudge_after: Duration,
    last_activity: Option<Instant>,
    blocked: bool,
    first_prompt_sent: bool,
    nudge_sent: bool,
    /// The actor pane `launch` opened, once it has one: `pane.get`
    /// takes a structured pane id and never an agent name
    /// (`refs/herdr` `0f8ad12` `src/app/api/panes.rs:159-168`), so
    /// `poll_vanished` asks by this rather than by `run.id` once it is
    /// known (fix 3).
    launched_pane: Option<String>,
}

impl<C: HerdrClient, W: WirkdApi, K: Clock> RunLoop<C, W, K> {
    /// `nudge_after` is this item's own decided bound (build-brief.md
    /// §2.2: 120 s, provisional, named by the `wirk` bin's caller, not
    /// hardcoded here — R6, a constructor parameter, not a constant).
    pub fn new(client: C, wirkd: W, clock: K, nudge_after: Duration) -> Self {
        RunLoop {
            executor: HerdrExecutor::new(client),
            wirkd,
            clock,
            reconciler: Reconciler::new(),
            prompt_gate: PromptGate::default(),
            nudge_after,
            last_activity: None,
            blocked: false,
            first_prompt_sent: false,
            nudge_sent: false,
            launched_pane: None,
        }
    }

    pub fn executor(&self) -> &HerdrExecutor<C> {
        &self.executor
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked
    }

    pub fn nudge_sent(&self) -> bool {
        self.nudge_sent
    }

    pub fn first_prompt_sent(&self) -> bool {
        self.first_prompt_sent
    }

    pub fn prompt_gate_busy(&self) -> bool {
        self.prompt_gate.busy
    }

    /// Step 6 (loop.md §1): `HerdrExecutor::launch_actor`, then the
    /// journal write the executor itself never makes — `RunLaunched` on
    /// success, `RunFailed{cause.detail}` on failure (issue 275's
    /// shape) — through `WirkdApi::record`. A launch failure is also
    /// this Run's `git worktree add` failure surfaced the same way
    /// (loop.md §1 row 2/12: one failure event for either cause), when
    /// the caller (the `wirk` bin, W3) has already turned a `git.rs`
    /// error into the same `HerdrExecutorError`-shaped report before
    /// calling this.
    ///
    /// Returns the **one** subscription `launch_actor` opened for the
    /// actor's pane before `agent.start` (D51's ordering, fix 3): the
    /// loop drains this, and never opens a second one of its own. The
    /// pane id it was opened for is remembered for `poll_vanished`.
    pub fn launch(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        world: &World,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, crate::HerdrError>>>, RunLoopError<W>>
    {
        match self.executor.launch_actor(run, world) {
            Ok(launched) => {
                self.last_activity = Some(self.clock.now());
                self.launched_pane = Some(launched.pane.pane_id.clone());
                self.wirkd
                    .record(
                        work_id,
                        &run.id,
                        EventKind::RunLaunched {
                            run: run.id.clone(),
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

    /// One event off the single subscription `launch` opened for the
    /// actor's pane before `agent.start` and handed back (fix 3: one
    /// subscription per Run, not the throwaway-plus-second-connection
    /// this loop used to take). Updates the activity clock, the blocked flag,
    /// releases `PromptGate` on `working`, journals `LifecycleObserved`
    /// for every status change, and — once the pane leaves `Blocked`
    /// and no first prompt has gone out yet — sends it (loop.md §1 row
    /// 7), gated by `PromptGate` (D56). A replayed event (per
    /// `Reconciler::admit`, D51) is a no-op.
    pub fn observe(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        actor: &ActorWorld,
        event: &HerdrEvent,
    ) -> Result<(), RunLoopError<W>> {
        if !self.reconciler.admit(event) {
            return Ok(());
        }
        match event {
            HerdrEvent::PaneAgentStatusChanged { agent_status, .. } => {
                self.last_activity = Some(self.clock.now());
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

                if !self.blocked && !self.first_prompt_sent && self.prompt_gate.try_acquire() {
                    let text = compose_first_prompt(actor);
                    self.executor
                        .client()
                        .prompt_agent(PromptAgent {
                            target: run.id.0.clone(),
                            text,
                        })
                        .map_err(HerdrExecutorError::from)?;
                    self.first_prompt_sent = true;
                }
            }
            HerdrEvent::PaneUpdated { .. } => {
                // A revision bump with no status change is still
                // activity (loop.md §3: output scrolling).
                self.last_activity = Some(self.clock.now());
            }
            _ => {}
        }
        Ok(())
    }

    /// The inactivity/nudge policy (0001 D3; issue 274; loop.md §3): no
    /// prompt at all while blocked (D52); at most one nudge per Run,
    /// ever — a second call past `nudge_after` with a nudge already
    /// sent is a no-op ("then surface for a human and stop prompting").
    /// `PromptGate` still applies: a nudge never doubles up on a prompt
    /// already in flight. Elapsed time is read from the injected
    /// `Clock`, never a real sleep (issue 359) — a test moves a
    /// `ManualClock` forward explicitly.
    pub fn maybe_nudge(&mut self, run: &Run) -> Result<bool, RunLoopError<W>> {
        if self.blocked || self.nudge_sent {
            return Ok(false);
        }
        let Some(last) = self.last_activity else {
            return Ok(false);
        };
        if self.clock.now().duration_since(last) < self.nudge_after {
            return Ok(false);
        }
        if !self.prompt_gate.try_acquire() {
            return Ok(false);
        }
        self.executor
            .client()
            .prompt_agent(PromptAgent {
                target: run.id.0.clone(),
                text: NUDGE_TEXT.to_string(),
            })
            .map_err(HerdrExecutorError::from)?;
        self.nudge_sent = true;
        Ok(true)
    }

    /// Step 11 (loop.md §1): `poll` itself only observes and never
    /// journals (the `Executor` trait is read/write-split, D56); a
    /// `Vanished` observation is journaled here, by the loop, exactly
    /// once.
    pub fn poll_vanished(&mut self, work_id: &WorkId, run: &Run) -> Result<bool, RunLoopError<W>> {
        // Ask by the pane id `launch` recorded when it has one:
        // `pane.get` resolves structured pane ids only and answers
        // `pane_not_found` for an agent name (`refs/herdr` `0f8ad12`
        // `src/app/api/panes.rs:159-168`), which this loop would read
        // as `Vanished` (fix 3). Before a launch there is no pane id,
        // and the trait row's own by-name lookup is all there is.
        let observation = match &self.launched_pane {
            Some(pane_id) => self.executor.poll_pane(pane_id),
            None => self.executor.poll(run),
        };
        match observation {
            Ok(RunObservation::Vanished) => {
                self.wirkd
                    .record(work_id, &run.id, EventKind::RunVanished)
                    .map_err(RunLoopError::Wirkd)?;
                Ok(true)
            }
            Ok(_) => Ok(false),
            Err(err) => Err(RunLoopError::Herdr(err)),
        }
    }

    /// Step 8/10 (loop.md §1): wirkd never pushes, so the loop polls
    /// `status` to learn `Claimed` (stop) or the Work's `NeedsInput`
    /// (stop prompting, wait for a human) — `Ok(None)` means keep
    /// driving.
    pub fn poll_claimed(
        &self,
        work_id: &WorkId,
        run_id: &RunId,
    ) -> Result<Option<Outcome>, RunLoopError<W>> {
        let status = self.wirkd.status(work_id).map_err(RunLoopError::Wirkd)?;
        if status
            .runs
            .iter()
            .any(|entry| &entry.run_id == run_id && matches!(entry.state, RunState::Claimed(_)))
        {
            return Ok(Some(Outcome::Claimed));
        }
        if matches!(status.work_state, WorkState::NeedsInput) {
            return Ok(Some(Outcome::NeedsInput));
        }
        Ok(None)
    }

    /// Drives one `Run` end to end: `launch` (which opens the one
    /// subscription, before `agent.start`, and hands it back), that
    /// subscription drained event by event (`observe`) with
    /// `poll_claimed` checked after each; once it is exhausted,
    /// `poll_vanished` then `maybe_nudge`. The `wirk` bin (W3) calls
    /// this against a live, unbounded subscription; a fake-backed test
    /// drives the smaller steps above directly instead, since the
    /// fake's `subscribe` hands back a fixed, already-exhausted `Vec`
    /// (no live stream to block on).
    ///
    /// Every error that escapes **after** `RunLaunched` is journaled
    /// `RunFailed{cause.detail}` first (fix 3, 0028 tried step 3's
    /// second finding: a post-launch crash left the Run stuck at
    /// `RunLaunched` with no record at all, discoverable only by a
    /// human re-running into a different, downstream error). A launch
    /// failure is not journaled twice: `launch` above already recorded
    /// it and its error returns before this guard.
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

        let events = self.launch(work_id, run, world)?;

        let outcome = self.drive_launched(work_id, run, &actor, events);
        if let Err(err) = &outcome {
            self.record_run_failed(work_id, run, &err.to_string());
        }
        outcome
    }

    /// `drive`'s post-launch half, split out so one guard in `drive`
    /// covers every error path through it.
    fn drive_launched(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        actor: &ActorWorld,
        events: Box<dyn Iterator<Item = Result<HerdrEvent, crate::HerdrError>>>,
    ) -> Result<Outcome, RunLoopError<W>> {
        for event in events {
            let event: HerdrEvent = event.map_err(HerdrExecutorError::from)?;
            self.observe(work_id, run, actor, &event)?;
            if let Some(outcome) = self.poll_claimed(work_id, &run.id)? {
                return Ok(outcome);
            }
        }

        if self.poll_vanished(work_id, run)? {
            return Ok(Outcome::Vanished);
        }
        self.maybe_nudge(run)?;
        Ok(Outcome::Pending)
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

const NUDGE_TEXT: &str =
    "Still there? If you're stuck, say what's blocking you with `wirk claim --question \"...\"`.";

/// The first prompt (loop.md §1 row 7): the Waypoint's intent, its
/// required artifacts by name, and the literal instruction to file
/// `wirk claim` (0001 D3). A formatting function, no new type (build-
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
// Mirrors `fake.rs`'s `FakeHerdrClient` (fixed responses, recorded
// calls, a `Mutex` since `WirkdApi: Send + Sync`). Not `cfg(test)`:
// `tests/run_loop.rs` is a separate compilation unit and would not see
// a `cfg(test)`-gated item there, the same reasoning `fake.rs`'s own
// doc comment already gives (R6).

/// A `WirkdApi` whose `status` reply is fixed in advance and whose
/// `record` calls are recorded, never actually journaled anywhere.
#[derive(Debug, Default)]
pub struct FakeWirkdApi {
    status_response: Mutex<Option<WorkStatus>>,
    recorded: Mutex<Vec<(WorkId, RunId, EventKind)>>,
}

impl FakeWirkdApi {
    pub fn with_status(self, status: WorkStatus) -> Self {
        *self.status_response.lock().unwrap() = Some(status);
        self
    }

    /// Replaces the configured `status` reply after construction — a
    /// test simulating "the Claim landed while the loop was mid-drive"
    /// updates this between two `poll_claimed` calls.
    pub fn set_status(&self, status: WorkStatus) {
        *self.status_response.lock().unwrap() = Some(status);
    }

    pub fn recorded(&self) -> Vec<(WorkId, RunId, EventKind)> {
        self.recorded.lock().unwrap().clone()
    }
}

#[derive(Debug, Error)]
#[error("FakeWirkdApi: {0}")]
pub struct FakeWirkdError(pub String);

impl WirkdApi for FakeWirkdApi {
    type Error = FakeWirkdError;

    fn status(&self, _work_id: &WorkId) -> Result<WorkStatus, Self::Error> {
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
}
