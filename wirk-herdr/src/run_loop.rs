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
//! prompt's own baseline (one worktree fingerprint,
//! `wirk_herdr::git::fingerprint`) compared against the same reading at
//! the *next* Idle — unchanged is the actor stuck (`Outcome::
//! NeedsInput`, `stuck_observation()` names what was observed); changed
//! prompts again. **P2.3 W4 (build-brief.md §8 finding 1):** the pane's
//! own `revision` left this comparison — any output by the actor
//! (answering a prompt, thinking aloud) advances the pane's revision
//! whether or not it did anything, so counting it as progress meant an
//! actor that only ever answers prompts and never edits was never
//! judged stuck. Progress since the last prompt now means the worktree
//! changed; the journal's own movement (a Claim, a Question) already
//! ends the loop through `observe_watch`'s own `NeedsInput` fold and
//! needs no part in this snapshot either.
//!
//! Blocked (P2.3 W4, build-brief.md §8 finding 2): the loop never
//! prompts a `Blocked` pane (unchanged), but on the *transition* to
//! `Blocked` it now calls `HerdrClient::notify` once and prints one
//! line — a human waiting on the pane has something to see. The Work's
//! own state and journal are untouched (`LifecycleObserved{Blocked}` is
//! already journaled by `observe_herdr`'s existing status-change write);
//! a later `Working` clears the notified flag, so a second `Blocked`
//! episode on the same Run notifies again.
//!
//! **P2.3 W5 (build-brief.md §9): `Done` is a turn end, exactly like
//! `Idle`.** Herdr's own `status_name` (`refs/herdr/src/app/
//! agent_view.rs`) maps one detector state, `AgentState::Idle`, to two
//! wire values by whether the pane has been *viewed* since:
//! `(Idle, seen=true) -> "idle"`, `(Idle, seen=false) -> "done"`. Every
//! pane `wirk run` drives is headless — nothing ever views it — so the
//! actor's turn ending reports `Done`, never `Idle`, live (confirmed by
//! the rerun, `knowledge/evidence/p2-retry-escalation-2026-09-04/
//! rerun/03-stuck.log`: `Idle -> Working -> Done`, never a second
//! `Idle`). `turn_ended` below is the one place this equivalence is
//! decided (R2: Herdr's own, `src/cli/agent.rs`'s `idle | done`
//! readiness check makes the same call) — every place this loop used to
//! read `AgentStatus::Idle` as "the turn ended" (the prompt gate, the
//! no-progress comparison, the `Blocked`-flag clearing) now reads
//! `turn_ended` instead, so a `Done` pane is prompted, its progress
//! compared, and `Blocked` cleared exactly as an `Idle` one always was.
//!
//! **P2.3 W6 (build-brief.md §10, rerun2's own correction, 0044):** the
//! progress baseline used to be captured right after *every* prompt this
//! loop ever sent, the very first one included — the very first prompt a
//! `RunLoop` sends carries the Waypoint's own intent (`compose_first_
//! prompt`, always the task, never a nudge to continue), so an actor
//! whose first turn is reading and planning, ending with no worktree
//! edit yet, was declared stuck without ever having been told to
//! continue — exactly the rerun2 evidence (`knowledge/evidence/
//! p2-retry-escalation-2026-09-04/rerun2/driver.log`,
//! `journal-B.ndjson`): stuck fired 22s after launch, on the very first
//! turn end. Fixed: the baseline is now taken only after a
//! **continuation** prompt — any prompt sent while `has_prompted` is
//! already true, i.e. the second prompt onward. The very first prompt
//! (`PromptProgress::First`) sets `has_prompted` and takes no baseline;
//! the next turn end therefore finds no baseline either and earns its
//! own unconditional prompt (`PromptProgress::FirstContinuation`) — the
//! first *continuation*, this time with the baseline taken right after
//! it. Only the turn end after that compares against a baseline at all.
//! "Stuck" now always means: told to continue at least once, and did
//! nothing since. One more turn granted to every actor, no number
//! involved (0044 D134: no count or timer governs this — `has_prompted`
//! and `progress_baseline`'s own presence are read state, exactly like
//! the existing `blocked`/`claimed`/`needs_input` flags, never a budget).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use thiserror::Error;

use wirk_core::{
    ActorWorld, Event, EventKind, FailureCause, Run, RunId, RunState, Timestamp, WorkId, WorkState,
    World, fold,
};

use crate::{
    AgentStatus, HerdrClient, HerdrError, HerdrEvent, HerdrExecutor, HerdrExecutorError, Notify,
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

/// Captured right after a prompt is sent, compared against the same
/// reading at the next Idle (item C's no-progress check). P2.3 W4
/// (build-brief.md §8 finding 1): the pane's own revision left this —
/// only the worktree fingerprint (`wirk_herdr::git::fingerprint`, which
/// never fails: an unreadable/non-repo path folds to "" via its own
/// `unwrap_or_default`) counts as progress.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgressBaseline {
    fingerprint: String,
}

/// P2.3 W3: what `observe_herdr` found when it decided this Idle earns
/// another prompt rather than the stuck path -- handed to `maybe_prompt`
/// so its printed line can name it. `First` is not itself a comparison
/// (there is no earlier baseline to compare against, and none is taken
/// after it either -- W6, module doc: the first prompt carries the
/// intent, not a continuation). `FirstContinuation` (W6) is the next
/// turn end after that: still no baseline to compare against (none was
/// taken after `First`), but this prompt *is* a continuation, so the
/// baseline is taken right after sending it -- the first turn end this
/// `RunLoop` will ever judge for progress is the one after this. Every
/// later prompt is `SinceLastPrompt`, carrying both readings
/// `observe_herdr` already took to decide the actor was not stuck
/// (always a genuine worktree change -- an unchanged fingerprint is the
/// stuck path, returned before `maybe_prompt` is ever reached, so
/// `describe` never needs to print "unchanged" for a prompt line).
enum PromptProgress {
    First,
    FirstContinuation,
    SinceLastPrompt {
        before: ProgressBaseline,
        after: ProgressBaseline,
    },
}

impl PromptProgress {
    fn describe(&self) -> String {
        match self {
            PromptProgress::First => "first prompt, no earlier baseline to compare".to_string(),
            PromptProgress::FirstContinuation => "first continuation, baseline taken".to_string(),
            PromptProgress::SinceLastPrompt { before, after } => format!(
                "progress since the last prompt: worktree changed, fingerprint {} -> {}",
                before.fingerprint, after.fingerprint
            ),
        }
    }
}

/// P2.3 W5 (build-brief.md §9, R6: one helper, used everywhere this
/// loop used to check `matches!(status, AgentStatus::Idle)` alone).
/// True for `Idle` and `Done` — Herdr's own `status_name` (`refs/herdr/
/// src/app/agent_view.rs`) reports the same underlying "actor's turn
/// ended" detector state as `Done` instead of `Idle` whenever the pane
/// has not been *viewed* since, which is every pane `wirk run` drives
/// (headless). `Working`, `Blocked`, and `Unknown` are never turn ends.
fn turn_ended(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Idle | AgentStatus::Done)
}

/// The first `n` words of `text`, joined back with single spaces and
/// suffixed `...` when more remain — the prompt line's own trimmed
/// naming of what was sent (BRIEF.md: "the first words of the prompt
/// text"), never the whole (multi-artifact, multi-paragraph) prompt.
fn first_words(text: &str, n: usize) -> String {
    let mut words = text.split_whitespace();
    let head: Vec<&str> = words.by_ref().take(n).collect();
    let joined = head.join(" ");
    if words.next().is_some() {
        format!("{joined}...")
    } else {
        joined
    }
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
    /// P2.3 W4 (build-brief.md §8 finding 2): true once `notify_blocked`
    /// has fired for the *current* `Blocked` episode — set on the
    /// transition into `Blocked`, cleared on the next `Working`, so a
    /// second `Blocked` episode on the same Run notifies again while a
    /// pane that stays `Blocked` across many polls notifies only once.
    blocked_notified: bool,
    /// The actor pane `launch` opened, once it has one: used both by
    /// `notify_blocked`/`notify_needs_input` to name the pane and, in
    /// production, by `get_pane` callers elsewhere in this crate.
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
    /// P2.3 W6 (build-brief.md §10): true once this `RunLoop` has sent
    /// its very first prompt (the intent, `PromptProgress::First`) --
    /// read by `observe_herdr` to tell that first prompt apart from the
    /// first *continuation* (`PromptProgress::FirstContinuation`), the
    /// next turn end after it, since both are reached with
    /// `progress_baseline` still `None`. Never cleared: one `RunLoop`
    /// drives exactly one `Run` end to end (module doc), so there is
    /// only ever one "first prompt" for it to remember.
    has_prompted: bool,
    /// Set by item C's no-progress check when it concludes the actor is
    /// stuck — the caller reads this alongside `Outcome::NeedsInput` to
    /// print what was observed (exit 4 stays as today; needs-input
    /// surfacing itself is P2.3).
    stuck_observation: Option<String>,
    /// P2.3 W3: where `log_line` writes. `None` (the only value
    /// `RunLoop::new` sets, unchanged — no caller outside this file
    /// constructs one differently) means the real path `notify_
    /// needs_input` already used before this wave: `println!`, the
    /// same mechanism `wirk run` prints `Claimed`/`NeedsInput` through
    /// (`wirk/src/executor.rs`, R2). A fake-backed test that cannot
    /// read the process's own stdout reliably (many tests share one
    /// process) instead calls `with_captured_output` to redirect every
    /// line here — the live twin (`wirk/tests/run_verb.rs`) needs no
    /// such thing, since it already reads the real child process's
    /// piped stdout (R2 over inventing a sink trait: this is the
    /// narrowest thing that makes the fake-backed case observable).
    captured_output: Option<Arc<Mutex<Vec<String>>>>,
}

impl<C: HerdrClient, W: WirkdApi> RunLoop<C, W> {
    pub fn new(client: C, wirkd: W) -> Self {
        RunLoop {
            executor: HerdrExecutor::new(client),
            wirkd,
            prompt_gate: PromptGate::default(),
            blocked: false,
            blocked_notified: false,
            claimed: false,
            needs_input: false,
            last_status: None,
            launched_pane: None,
            run_state: None,
            watch_events: Vec::new(),
            progress_baseline: None,
            has_prompted: false,
            stuck_observation: None,
            captured_output: None,
        }
    }

    /// P2.3 W3: redirect `log_line`'s output into `sink` instead of
    /// the real `println!` — a fake-backed test's own way to assert on
    /// the loop's printed lines (see `captured_output`'s own doc).
    /// Never called from production code (`wirk/src/executor.rs` keeps
    /// using the unadorned `RunLoop::new`).
    pub fn with_captured_output(mut self, sink: Arc<Mutex<Vec<String>>>) -> Self {
        self.captured_output = Some(sink);
        self
    }

    /// The one place every line this loop prints goes through — real
    /// `println!` in production, `captured_output` in a fake-backed
    /// test (see that field's doc).
    fn log_line(&self, line: &str) {
        match &self.captured_output {
            Some(sink) => sink.lock().unwrap().push(line.to_string()),
            None => println!("{line}"),
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

        // P2.3 W4 (build-brief.md §8 finding 2): the loop never prompts
        // a Blocked pane (below, `maybe_prompt`'s own guard, unchanged),
        // but on the *transition* into Blocked it now notifies once — a
        // human waiting on the pane has something to see. The Work's
        // own state and journal are untouched beyond the
        // `LifecycleObserved` write just above (already journaled for
        // every changed status, Blocked included). A later Working
        // clears `blocked_notified`, so a second Blocked episode on the
        // same Run notifies again; staying Blocked across many polls
        // notifies only once, since only a *changed* status reaches
        // this point at all (the `if !changed` return above).
        match agent_status {
            AgentStatus::Blocked => {
                if !self.blocked_notified {
                    self.notify_blocked(work_id);
                    self.blocked_notified = true;
                }
            }
            AgentStatus::Working => {
                self.blocked_notified = false;
            }
            _ => {}
        }

        if !turn_ended(*agent_status) {
            return Ok(None);
        }

        let progress = if let Some(baseline) = self.progress_baseline.take() {
            let now = self.progress_snapshot(actor);
            if now == baseline {
                let pane_id = self.launched_pane.clone().unwrap_or_default();
                // build-brief.md §7 amendment 2: the observation names
                // the pane (it stays alive after the loop exits, a
                // human reads it there) and the fingerprint compared
                // (P2.3 W4, build-brief.md §8 finding 1: the pane's own
                // revision no longer participates — any output by the
                // actor advanced it whether or not the actor did
                // anything, so it never actually pinned "stuck"). The
                // pane's own screen text is not read here — Herdr's
                // `pane.read` is not on `HerdrClient` today (map row 23:
                // available, never called, 0017 D57 kept wirk off it for
                // Claim evidence) and adding it crosses this wave's file
                // allow-list (`wirk-herdr/src/lib.rs`, `socket.rs`);
                // named gap, BUILD.md.
                let observation = format!(
                    "stuck: pane {pane_id} — no progress since the last prompt: worktree \
                     fingerprint {} unchanged",
                    baseline.fingerprint
                );
                self.stuck_observation = Some(observation.clone());
                self.wirkd
                    .record(
                        work_id,
                        &run.id,
                        EventKind::RunFailed {
                            cause: FailureCause {
                                status: Some("stuck".to_string()),
                                request_id: None,
                                at: Timestamp(0),
                                detail: Some(observation.clone()),
                            },
                        },
                    )
                    .map_err(RunLoopError::Wirkd)?;
                self.notify_needs_input(work_id, "stuck", &observation);
                return Ok(Some(Outcome::NeedsInput));
            }
            // Progress *was* observed: the worktree fingerprint changed.
            PromptProgress::SinceLastPrompt {
                before: baseline,
                after: now,
            }
        } else if self.has_prompted {
            // W6: no baseline exists, but a prompt (the intent) was
            // already sent once before -- this turn end earns the first
            // *continuation*, unconditionally, with the baseline taken
            // right after it (`maybe_prompt`). Only the turn end after
            // this one is ever compared for progress.
            PromptProgress::FirstContinuation
        } else {
            PromptProgress::First
        };

        self.maybe_prompt(run, actor, *agent_status, progress)?;
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
    /// (D56). Takes item C's own baseline right after sending, except
    /// for the very first prompt this `RunLoop` ever sends (W6, module
    /// doc: that prompt is the intent, not a continuation).
    ///
    /// P2.3 W3 (BRIEF.md's amendment): prints one line, through
    /// `log_line`, for every prompt actually sent — the gap the
    /// amendment names ("the loop prints nothing when it prompts, so
    /// run 3's evidence counted zero prompts where the journal shows
    /// two"). Printed only inside the `try_acquire` success branch, so
    /// a call that finds the gate already busy (a prompt already in
    /// flight) prints nothing for it — there is no second prompt to
    /// name. `progress` is `PromptProgress::First` for the very first
    /// prompt this `RunLoop` ever sends (the intent; no baseline is
    /// taken after it — W6), `FirstContinuation` for the next one (the
    /// baseline *is* taken now), and `SinceLastPrompt` for every one
    /// after that — "the first prompt prints the same shape" (BRIEF.md),
    /// just with each variant's own wording in place of a comparison.
    /// `agent_status` is the turn-ended status that earned this prompt
    /// (P2.3 W5: `Idle` or `Done`, per `turn_ended`) — named in the
    /// printed line so a headless run's `Done`-answered prompts read the
    /// same as an `Idle`-answered one always did, not a hardcoded
    /// "Idle".
    fn maybe_prompt(
        &mut self,
        run: &Run,
        actor: &ActorWorld,
        agent_status: AgentStatus,
        progress: PromptProgress,
    ) -> Result<(), RunLoopError<W>> {
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
                text: text.clone(),
            })
            .map_err(HerdrExecutorError::from)?;
        let describe = progress.describe();
        // W6 (module doc, build-brief.md §10): the baseline is taken
        // after every prompt *except* the very first (the intent) — that
        // first prompt only marks `has_prompted`, so the next turn end
        // earns an unconditional continuation instead of being compared
        // against a baseline that was never a "continue" ask.
        match progress {
            PromptProgress::First => self.has_prompted = true,
            PromptProgress::FirstContinuation | PromptProgress::SinceLastPrompt { .. } => {
                self.progress_baseline = Some(self.progress_snapshot(actor));
            }
        }
        self.log_line(&format!(
            "prompt: {agent_status:?} answered (run {}); {}; sending: {}",
            run.id.0,
            describe,
            first_words(&text, 8)
        ));
        Ok(())
    }

    /// One worktree fingerprint (P2.3 W4, build-brief.md §8 finding 1:
    /// the pane's own revision left this — item C's original "one
    /// `get_pane` request and one worktree fingerprint" is now just the
    /// fingerprint). `wirk_herdr::git::fingerprint` never fails on its
    /// own terms (an unreadable or non-repo path folds to `""` via its
    /// own `unwrap_or_default`), so this always returns a value; kept
    /// non-fallible rather than wrapped in `Option` for the same reason.
    fn progress_snapshot(&self, actor: &ActorWorld) -> ProgressBaseline {
        ProgressBaseline {
            fingerprint: crate::git::fingerprint(&actor.worktree_path),
        }
    }

    /// P2.3 W1 (states.md §2, R2: `HerdrClient::notify` already
    /// defined, `wirk-herdr/src/lib.rs:530`, zero call sites before
    /// this wave). Called exactly once, from the stuck-actor path only
    /// (build-brief.md's own wording: "the loop, on no progress after
    /// a prompt ... calls `HerdrClient::notify` once") — never from
    /// `observe_watch`'s own `NeedsInput` branch: a Question claim or a
    /// deterministic Waypoint's `RunFailed` arriving on the watch
    /// stream is surfaced by `wirk work status` and the pane's own
    /// `watch`, not a desktop notification (build-brief.md §7 item 1).
    /// Probed by hand (BUILD.md): duplicating this call within the
    /// stuck branch itself makes `run_loop_needs_input_calls_notify_once`
    /// fail (2 != 1); reverted before landing. Prints one line naming
    /// the reply — `notify` itself returns no reply payload
    /// (`Result<(), HerdrError>`, unchanged), so the line names success
    /// or the transport error.
    fn notify_needs_input(&self, work_id: &WorkId, reason: &str, detail: &str) {
        let title = format!("wirk: Work {} needs input ({reason})", work_id.0);
        match self.executor.client().notify(Notify {
            title,
            body: detail.to_string(),
        }) {
            Ok(()) => self.log_line(&format!("notify: sent (work {})", work_id.0)),
            Err(err) => self.log_line(&format!("notify: failed (work {}): {err}", work_id.0)),
        }
    }

    /// P2.3 W4 (build-brief.md §8 finding 2, R2: the same `HerdrClient::
    /// notify` `notify_needs_input` already calls). Called once per
    /// `Blocked` episode, from `observe_herdr`'s own transition check —
    /// never from `maybe_prompt`, since a Blocked pane is never
    /// prompted. Title names the Work and that the actor is waiting on
    /// its pane; body is the pane id, so a human reading the
    /// notification knows exactly which pane to look at (the pane
    /// itself is untouched and still waiting — this call journals
    /// nothing, `LifecycleObserved{Blocked}` already covers that).
    fn notify_blocked(&self, work_id: &WorkId) {
        let pane_id = self.launched_pane.clone().unwrap_or_default();
        let title = format!("wirk: Work {} — actor waiting on its pane", work_id.0);
        match self.executor.client().notify(Notify {
            title,
            body: pane_id.clone(),
        }) {
            Ok(()) => self.log_line(&format!(
                "notify: sent (work {}, pane {pane_id} blocked)",
                work_id.0
            )),
            Err(err) => self.log_line(&format!(
                "notify: failed (work {}, pane {pane_id} blocked): {err}",
                work_id.0
            )),
        }
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
