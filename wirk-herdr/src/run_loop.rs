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
//! artifacts, and how the claim gets filed for this Run's actor kind
//! (`compose_first_prompt`, P2.7 W2b: by hand, or by the hook already
//! installed for it — reused for every prompt, not only the first).
//! Prompting stops on a
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
//! line — a human waiting on the pane has something to see. A later
//! `Working` clears the notified flag, so a second `Blocked` episode on
//! the same Run notifies again.
//!
//! **P2.6 W2 (ruling 0052 D156):** a Blocked pane is an actor waiting
//! on a human, not a failed Run — `observe_herdr`'s existing
//! status-change write now carries the pane's last screen lines
//! (`HerdrClient::read_pane`) as `LifecycleObserved{Blocked}.detail`,
//! and `fold` (`wirk-core`) reads that same event to put the Work in
//! `NeedsInput` with cause `"blocked"`; `wirk work retry`/`wirk work
//! fail` then apply exactly as they do to any other `NeedsInput` Work
//! (0049 D147). The loop itself makes no new decision here — it
//! journals what it observes, same as every other status; the fold is
//! what turns `Blocked` into `NeedsInput`, and a later
//! `LifecycleObserved{Working}` is what clears it back to `Active`
//! (already journaled by this same write, for every status).
//!
//! **P3 native usability (ruling 0113): resuming the SAME Run after a
//! resolved permission prompt.** A Run held at a harness trust or
//! permission prompt ends `NeedsInput{blocked}`. When the human answers
//! it, the next `wirk run` reconciles onto that still-live pane
//! (`observe_admitted_launch`) — and used to read the block straight
//! back off the replayed `wirkd watch` stream and stop again, without
//! ever delivering the task. `run_opened_this_run` cannot gate that: a
//! resume drives the same Run, whose own `RunOpened` sits in the
//! replayed prefix *ahead* of the block. Two things resolve it, and
//! both are observations, never inferences. `drive` observes the status
//! Herdr already reported for the reconciled pane
//! (`observe_agent_status`), because a subscription delivers only
//! *changes* and an answered-and-idle actor makes none until it is
//! prompted; and `observe_watch` withholds a `NeedsInput` decision
//! whose cause is *this* Run's own block while that observation says
//! the pane is no longer blocked (`block_this_run_has_since_left`). A
//! pane still sitting on its prompt still stops the loop, with the
//! reason it reads off the pane now; every other `NeedsInput` cause is
//! untouched.
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
    ActorKind, ActorWorld, AttemptHolder, Event, EventKind, FailureCause, Run, RunId, RunState,
    Timestamp, Work, WorkId, WorkState, World, fold,
};

use crate::{
    AgentStatus, EventSubscription, HerdrClient, HerdrError, HerdrEvent, HerdrExecutor,
    HerdrExecutorError, Notify, PromptAgent, PromptGate, validate_selection,
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

/// What `RunLoop::resume_authority` concluded from the complete
/// current journal (its own doc): whether a resumed drive may act on
/// the pane it just reconciled onto, or must leave it untouched and
/// stop with the outcome the journal names.
#[derive(Debug)]
enum ResumeAuthority {
    Continue,
    Withheld { outcome: Outcome, detail: String },
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
    /// P3 native launch attempt admission: this Run's admitted launch
    /// could be neither confirmed nor ruled out from here, so this
    /// invocation stops instead of launching. Never a claim that
    /// nothing ran and never a claim that something did — the two
    /// cases it covers are "Herdr would not say" (a transport or
    /// protocol error from `agent.get`, which is not an absence) and
    /// "this Run already launched and its agent is gone", where
    /// launching again would be a second external execution rather
    /// than a recovery.
    #[error("{0}")]
    LaunchUnresolved(String),
}

/// One live Herdr subscription, as `launch`/`launch_actor` hand it back
/// (named once so the reconciliation row's own signature stays
/// readable).
pub type HerdrEventStream = Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>;

/// What `observe_admitted_launch` found, once it has asked the one
/// system that can answer.
///
/// There is deliberately no "unknown" variant: an unknown outcome
/// is not a value this caller may act on, so it leaves as
/// `RunLoopError::LaunchUnresolved` instead of as a case the
/// launch path could fall through.
enum AdmittedLaunch {
    /// Herdr reports the agent live: it launched. Attached to,
    /// under the same bound request, with no second agent.
    Reconciled(HerdrEventStream),
    /// Herdr says, definitely, that no agent of this name exists —
    /// `agent_not_found`, its own answer, not an error standing in
    /// for one. The admitted request may be launched now.
    Absent,
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
    /// P2.5 W3 (0050 D151; build-brief.md §7.1 amendment): `false`
    /// until the watch stream has delivered *this Run's own*
    /// `RunOpened` (matched on `run_state`'s id). `handle_watch_connection`
    /// always replays the *entire* journal before live-tailing, so on a
    /// retry this replay still carries a previous attempt's own
    /// `RunFailed`/question that once put the Work in `NeedsInput` --
    /// `fold` runs over every event pushed so far exactly as before
    /// (nothing is filtered out of the accumulation), but a `NeedsInput`
    /// decision drawn from that fold is withheld while this is `false`:
    /// before this Run's own `RunOpened` arrives, the replayed prefix is
    /// history being caught up on, not yet "now". Cleared at the top of
    /// every `drive` alongside `watch_events`.
    run_opened_this_run: bool,
    /// Ruling 0113 (P3 native usability): the status Herdr reported for
    /// the pane this Run was *reconciled onto* (`observe_admitted_launch`),
    /// carried from `launch` to `drive` so the resumed loop starts from
    /// what the pane is doing **now** rather than from what the journal
    /// last recorded about it. Taken (cleared) by `drive` the moment it
    /// is observed, so it is never re-applied.
    resumed_status: Option<AgentStatus>,
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
            run_opened_this_run: false,
            resumed_status: None,
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
        // Step 0 (D1, "validate before unnecessary execution-side
        // effects"): everything about the resolved request that can be
        // judged without Herdr is judged here — before the binding
        // write and before any pane exists. An unmappable or
        // self-contradicting request costs nothing and journals
        // nothing.
        validate_selection(run.kind.0.as_str(), &run.selection)
            .map_err(|err| RunLoopError::Herdr(HerdrExecutorError::Selection(err)))?;

        // Step 1 (D1): durably bind the resolved request *before* the
        // irreversible `agent.start`, atomically against this Run's own
        // journal under wirkd's authority (`handle_record`, which
        // admits at most one `RunLaunchRequested` per Run). A refusal
        // here means someone else already bound this Run's launch: this
        // invocation stops without touching Herdr and without
        // journaling anything of its own, so it can neither start a
        // second differently-configured agent nor append a competing
        // `RunFailed` to the admitted owner's Run.
        let already_bound = run.launch_requested;
        if !already_bound {
            self.wirkd
                .record(
                    work_id,
                    &run.id,
                    EventKind::RunLaunchRequested {
                        run: run.id.clone(),
                        actor_kind: run.kind.clone(),
                        selection: run.selection.clone(),
                    },
                )
                .map_err(RunLoopError::Wirkd)?;
        }

        // Step 2 (the independent review's N1): admitting the *request*
        // is not admitting the *attempt*. Binding the request stops a
        // second invocation from launching a differently-configured
        // agent, but once it is bound, a duplicate invocation and a
        // recovery invocation both used to walk straight into
        // `agent.start` under it, with only Herdr's own agent-name
        // uniqueness between them — an incidental guard that does not
        // exist at all across two Herdr sessions. So the attempt is
        // admitted the same way and in the same place: under wirkd's
        // journal lock, against this Run's own journal, to exactly one
        // live process at a time, bound to the Herdr this process is
        // actually connected to. A refusal here means someone else owns
        // this Run's launch right now: this invocation stops without
        // touching Herdr.
        //
        // `holder` is left default deliberately: wirkd mints it from
        // this connection's own kernel-reported peer credentials and
        // discards whatever a client sends.
        self.wirkd
            .record(
                work_id,
                &run.id,
                EventKind::RunLaunchAttempted {
                    run: run.id.clone(),
                    destination: self.executor.client().destination(),
                    holder: AttemptHolder::default(),
                },
            )
            .map_err(RunLoopError::Wirkd)?;

        if already_bound {
            // An earlier invocation's request was admitted. Whether it
            // reached Herdr, and what Herdr did with it, is the one
            // question this invocation must answer before it can
            // launch anything — and the only place the answer can come
            // from is Herdr.
            match self.observe_admitted_launch(work_id, run)? {
                AdmittedLaunch::Reconciled(events) => return Ok(events),
                AdmittedLaunch::Absent => {}
            }
        }

        self.release_earlier_panes(work_id, run);
        match self.executor.launch_actor(run, world) {
            Ok(launched) => {
                self.launched_pane = Some(launched.pane.pane_id.clone());
                self.wirkd
                    .record(
                        work_id,
                        &run.id,
                        EventKind::RunLaunched {
                            run: run.id.clone(),
                            actor_kind: run.kind.clone(),
                            selection: run.selection.clone(),
                            launch_argv: launched.argv.clone(),
                        },
                    )
                    .map_err(RunLoopError::Wirkd)?;
                Ok(launched.events)
            }
            Err(err) => {
                let detail = err.to_string();
                // D1: `agent.start` failing is not by itself evidence
                // that nothing started — a lost reply looks exactly
                // like a refusal from here. Ask Herdr before claiming
                // either. Only when Herdr has no agent under this Run's
                // name is `RunFailed` the truth; when it does, the
                // honest record is that the outcome of an admitted
                // request is uncertain, which is a state the next
                // invocation can reconcile (above) instead of a failure
                // that discards a live agent.
                // Corrected for the review's transport-error finding:
                // only Herdr's own `agent_not_found` is an absence.
                // Every other error is Herdr declining to answer, which
                // is exactly the case where claiming `RunFailed` would
                // discard a live agent.
                match self.executor.client().get_agent(&run.id.0) {
                    Ok(pane) => {
                        let _ = self.wirkd.record(
                            work_id,
                            &run.id,
                            EventKind::LifecycleObserved {
                                status: "launch-outcome-uncertain".to_string(),
                                detail: Some(format!(
                                    "agent.start for the admitted request returned an error \
                                     ({detail}), but Herdr reports an agent named {} live in \
                                     pane {} — this launch is neither confirmed nor failed",
                                    run.id.0, pane.pane_id
                                )),
                            },
                        );
                    }
                    Err(err) if !matches!(err, HerdrError::NotFound(_)) => {
                        // Herdr declined to answer at all. Two
                        // unknowns compounded — what `agent.start`
                        // did, and what exists now — is still not
                        // "nothing ran".
                        let _ = self.wirkd.record(
                            work_id,
                            &run.id,
                            EventKind::LifecycleObserved {
                                status: "launch-outcome-uncertain".to_string(),
                                detail: Some(format!(
                                    "agent.start for the admitted request returned an error \
                                     ({detail}), and Herdr would not say whether an agent named \
                                     {} exists either ({err}) — this launch is neither \
                                     confirmed nor failed",
                                    run.id.0
                                )),
                            },
                        );
                    }
                    Err(_) => {
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
                    }
                }
                Err(RunLoopError::Herdr(err))
            }
        }
    }

    /// D1 recovery, corrected for the review's N1 and for its
    /// "a timeout, transport error or unobservable Herdr state is not
    /// proof of absence": this Run already has an admitted
    /// `RunLaunchRequested`, so an earlier invocation (or this Run's
    /// own earlier life) got at least as far as being allowed to call
    /// Herdr. Ask Herdr by the agent name the launch uses (`run.id`).
    ///
    /// Three answers, kept apart rather than collapsed:
    ///
    /// * the agent is live — the launch really happened, so reconcile
    ///   onto the pane rather than start a second one. Herdr's argv for
    ///   the original start is not recoverable this way, so no
    ///   `RunLaunched` is written and no argv is invented: the
    ///   reconciliation is recorded as what it is, an observation.
    /// * `NotFound` — Herdr's own `agent_not_found`, a definite
    ///   absence. If no `RunLaunched` ever folded, the admitted request
    ///   may now be launched, unchanged. If one *did* fold, this Run
    ///   already launched and its agent is gone: launching again would
    ///   be a second external execution of a Run that already ran, so
    ///   it is refused and the operator's own `wirk work retry`, which
    ///   opens a new Run, is named.
    /// * any other error — a transport failure, a Herdr that will not
    ///   answer, a protocol error. Previously this whole class was
    ///   `Ok(None)`, read as "no agent", and fell through to a second
    ///   `agent.start`. It is not absence: it is the absence of an
    ///   answer, and it is recorded as an observation and refused.
    fn observe_admitted_launch(
        &mut self,
        work_id: &WorkId,
        run: &Run,
    ) -> Result<AdmittedLaunch, RunLoopError<W>> {
        let pane = match self.executor.client().get_agent(&run.id.0) {
            Ok(pane) => pane,
            Err(HerdrError::NotFound(detail)) => {
                if run.launched {
                    let detail = format!(
                        "this Run's launch is already recorded (RunLaunched) and Herdr reports \
                         no agent named {} ({detail}); it will not be launched a second time \
                         under the same Run — `wirk work retry` opens a new one",
                        run.id.0
                    );
                    self.log_line(&detail);
                    let _ = self.wirkd.record(
                        work_id,
                        &run.id,
                        EventKind::LifecycleObserved {
                            status: "launch-agent-gone".to_string(),
                            detail: Some(detail.clone()),
                        },
                    );
                    return Err(RunLoopError::LaunchUnresolved(detail));
                }
                return Ok(AdmittedLaunch::Absent);
            }
            Err(err) => {
                let detail = format!(
                    "this Run's launch request was admitted and Herdr will not say whether its \
                     agent {} exists ({err}); a transport or protocol error is not proof that \
                     nothing launched, so this invocation records the uncertainty and stops \
                     rather than starting a second agent",
                    run.id.0
                );
                self.log_line(&detail);
                let _ = self.wirkd.record(
                    work_id,
                    &run.id,
                    EventKind::LifecycleObserved {
                        status: "launch-outcome-unobservable".to_string(),
                        detail: Some(detail.clone()),
                    },
                );
                return Err(RunLoopError::LaunchUnresolved(detail));
            }
        };
        let events = self
            .executor
            .client()
            .subscribe(vec![
                EventSubscription::PaneAgentStatusChanged {
                    pane_id: pane.pane_id.clone(),
                },
                EventSubscription::PaneUpdated {
                    pane_id: pane.pane_id.clone(),
                },
            ])
            .map_err(|err| RunLoopError::Herdr(HerdrExecutorError::Herdr(err)))?;
        self.launched_pane = Some(pane.pane_id.clone());
        // Ruling 0113: what this pane is doing *now* is the fact the
        // resume turns on, and this reply is the only place it is
        // available before the subscription's first change event --
        // which, for a pane sitting still after a human answered its
        // prompt, may never come at all. `drive` observes it the same
        // way it observes any other status Herdr reports.
        self.resumed_status = Some(pane.agent_status);
        self.log_line(&format!(
            "launch reconciled: this Run's request was already admitted and Herdr reports its \
             agent live in pane {} ({:?}); attaching to it rather than launching again",
            pane.pane_id, pane.agent_status
        ));
        self.wirkd
            .record(
                work_id,
                &run.id,
                EventKind::LifecycleObserved {
                    status: "launch-reconciled".to_string(),
                    detail: Some(format!(
                        "an admitted launch request with no RunLaunched was reconciled onto the \
                         live agent {} in pane {}; Herdr's own argv for that start was never \
                         returned to wirk and is not recorded",
                        run.id.0, pane.pane_id
                    )),
                },
            )
            .map_err(RunLoopError::Wirkd)?;
        Ok(AdmittedLaunch::Reconciled(events))
    }

    /// P2.6 W3 (rerun findings, `03-orient.log`; ruling 0052): before
    /// this Run's own pane is ever created, close any *other* Run this
    /// Work knows about whose pane (named by convention for `run.id.0`,
    /// `actor_pane`'s own doc) is still alive in Herdr — a retry's own
    /// abandoned predecessor is the live case (`handle_retry`,
    /// `server.rs`, now marks it `RunFailed`, but that journal write
    /// alone never touches Herdr; only `pane.close` does), reproduced
    /// live as `agent_name_taken` when the collision was never
    /// released. Best-effort throughout: a `status` this build cannot
    /// reach, a pane that was never actually launched (`get_pane`
    /// refuses `NotFound`), or a `close_pane`/`record` call that itself
    /// fails, are none of them fatal to this Run's own launch — the
    /// worst case is the collision this step exists to prevent, not
    /// silently, since a real `agent_name_taken` still surfaces from
    /// `launch_actor` itself right after. Never called for its own Run
    /// (`entry.run_id == run.id` is skipped).
    fn release_earlier_panes(&self, work_id: &WorkId, run: &Run) {
        let Ok(status) = self.wirkd.status(work_id) else {
            return;
        };
        for entry in status.runs {
            if entry.run_id == run.id {
                continue;
            }
            let pane_id = entry.run_id.0.clone();
            let Ok(pane) = self.executor.client().get_pane(&pane_id) else {
                continue;
            };
            if self.executor.client().close_pane(&pane.pane_id).is_err() {
                continue;
            }
            let _ = self.wirkd.record(
                work_id,
                &entry.run_id,
                EventKind::LifecycleObserved {
                    status: "released".to_string(),
                    detail: Some(format!(
                        "closed pane {} for a stale Run before launching {}",
                        pane.pane_id, run.id.0
                    )),
                },
            );
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
        let mut actor = match world {
            World::Actor(actor) => actor.clone(),
            World::Deterministic(_) => {
                return Err(RunLoopError::Herdr(
                    HerdrExecutorError::NotDeterministicKind,
                ));
            }
        };
        // P2.5 W3, found live building this wave's own tried step: a
        // retry (`wirk work retry`, `handle_retry`) reuses the
        // Waypoint's already-reserved `World` verbatim for the new Run
        // it opens — it mints a fresh `run_id` but never re-reserves the
        // World, so `actor.triple.run_id` still names whichever Run
        // *first* reserved this Waypoint's World, not the Run this
        // `drive` call is actually driving. `actor_pane`'s env map is
        // built from exactly that field (`lib.rs`'s `actor.triple`), so
        // an actor's `wirk claim` on a retried Run filed itself against
        // the stale, previous Run's id — reproduced live: `ClaimRecorded`
        // landed on the first attempt's own `run_id`, never the retry's,
        // and the retry's driver never saw its own Claim. The same
        // precedent as `run.kind = actor_kind` in `wirk/src/executor.rs`
        // (a Run-specific field corrected against the actual driven Run
        // before use, not the reservation's stale copy): reconciled here
        // rather than in `wirkd`'s own `handle_retry` — R7, local to
        // this crate boundary, the only file this wave touches.
        actor.triple.run_id = run.id.clone();
        let world = World::Actor(actor.clone());
        self.run_state = Some(run.clone());
        self.watch_events.clear();
        self.run_opened_this_run = false;

        let watch_events = self.wirkd.watch(work_id).map_err(RunLoopError::Wirkd)?;
        let herdr_events = self.launch(work_id, run, &world)?;

        let (tx, rx) = mpsc::channel::<LoopMsg>();
        spawn_herdr_reader(herdr_events, tx.clone());
        spawn_watch_reader(watch_events, tx);

        // Ruling 0113: `launch` reconciled onto a pane that was already
        // alive, so this loop has never seen a status for it and the
        // subscription only ever delivers *changes*. A pane whose human
        // has just answered its permission prompt sits at `Idle` and
        // changes nothing further until it is prompted — so waiting for
        // the subscription to say what it is doing is waiting for a
        // transition that the resume itself is what unblocks. The
        // status Herdr already gave in its `agent.get` reply is
        // observed here, through exactly the path a subscribed status
        // takes: journaled as the `LifecycleObserved` it is. Nothing is
        // synthesized — an `Idle` is recorded as `Idle`.
        //
        // The observation is journaled here and *nothing is prompted
        // from it here* (the correction to this path): at this instant
        // not one watch event has been folded, so the loop knows only
        // what this pane is doing, never why the Work is being held.
        // Delivering the task from that alone hands an actor its work
        // back while the human's own decision — a filed Question, an
        // out-of-boundary refusal, a failed Run — is still open.
        // `resume_authority` below is what establishes the other half.
        if let Some(resumed) = self.resumed_status.take() {
            if let Some(outcome) =
                self.observe_agent_status_may_prompt(work_id, run, &actor, &resumed, false)?
            {
                return Ok(outcome);
            }
            match self.resume_authority(work_id, run)? {
                ResumeAuthority::Continue => {
                    if turn_ended(resumed) {
                        let progress = if self.has_prompted {
                            PromptProgress::FirstContinuation
                        } else {
                            PromptProgress::First
                        };
                        self.maybe_prompt(run, &actor, resumed, progress)?;
                    }
                }
                ResumeAuthority::Withheld { outcome, detail } => {
                    self.log_line(&detail);
                    if matches!(outcome, Outcome::NeedsInput) {
                        self.needs_input = true;
                    }
                    return Ok(outcome);
                }
            }
        }

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
        self.observe_agent_status(work_id, run, actor, agent_status)
    }

    /// One observed agent status, wherever it came from: the pane
    /// subscription (`observe_herdr`) or, on a resume, the `agent.get`
    /// reply the reconciliation itself read (`drive`, ruling 0113).
    /// Both are Herdr saying what this pane is doing; neither is
    /// inferred, so both are handled identically and journaled
    /// identically.
    fn observe_agent_status(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        actor: &ActorWorld,
        agent_status: &AgentStatus,
    ) -> Result<Option<Outcome>, RunLoopError<W>> {
        self.observe_agent_status_may_prompt(work_id, run, actor, agent_status, true)
    }

    /// `observe_agent_status`, with the one thing a *resumed* status
    /// may not do yet made explicit: `may_prompt`.
    ///
    /// The whole observation — the `last_status` update, the blocked
    /// notification, the `LifecycleObserved` write, the screen read
    /// that gives a `Blocked` its reason — is identical either way and
    /// happens either way. Only the delivery at the end is withheld,
    /// and only for the reconciliation's own `agent.get` reply
    /// (`drive`), which is read before a single watch event has been
    /// folded. A status arriving on the subscription passes `true`:
    /// that loop has been folding the journal all along.
    fn observe_agent_status_may_prompt(
        &mut self,
        work_id: &WorkId,
        run: &Run,
        actor: &ActorWorld,
        agent_status: &AgentStatus,
        may_prompt: bool,
    ) -> Result<Option<Outcome>, RunLoopError<W>> {
        let changed = self.last_status != Some(*agent_status);
        self.last_status = Some(*agent_status);
        if !changed {
            return Ok(None);
        }

        self.blocked = matches!(agent_status, AgentStatus::Blocked);
        self.prompt_gate.release_on_working(*agent_status);

        // Ruling 0052 D156 (P2.6 W2): a Blocked observation's cause
        // carries the pane's last screen lines — read now, via the pane
        // still being watched (`HerdrClient::read_pane`, R2/R5: the
        // trait wraps Herdr's own `pane.read`), since only the loop
        // holds a live pane to read; `fold` (`wirk-core`) has no pane to
        // read from, only whatever `detail` this event carries. Every
        // other status still journals no detail (`None`, unchanged from
        // before this wave). Best-effort: a `read_pane` failure names
        // itself in `detail` rather than failing this whole observation
        // — the Blocked fact itself (`status`) is what matters most, and
        // is never lost to a screen-read error.
        let pane_id = self.launched_pane.clone().unwrap_or_default();
        let detail = if matches!(agent_status, AgentStatus::Blocked) {
            let screen = self
                .executor
                .client()
                .read_pane(&pane_id)
                .unwrap_or_else(|err| format!("(pane {pane_id}'s screen unreadable: {err})"));
            Some(format!(
                "the actor is waiting on its pane {pane_id}:\n{screen}"
            ))
        } else {
            None
        };
        self.wirkd
            .record(
                work_id,
                &run.id,
                EventKind::LifecycleObserved {
                    status: format!("{agent_status:?}"),
                    detail,
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

        if !turn_ended(*agent_status) || !may_prompt {
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

    /// The correction to ruling 0113's resume: what the **complete
    /// current** journal for this exact Work and this exact Run says
    /// about whether a resumed drive may act at all.
    ///
    /// A resume reconciles onto a live pane and reads its status from
    /// `agent.get`. That is a fresh observation of the pane and nothing
    /// more: it says the actor's turn has ended, never why the Work is
    /// being held. The journal is the other half, and it cannot be read
    /// off the watch stream at this point — `wirkd`'s `watch` replays
    /// the whole journal before it live-tails and carries no
    /// end-of-replay marker, so any fold taken while the replay is
    /// still arriving is a *prefix*: a Run whose first block was
    /// resolved may still have a Question, a refusal, a failure or a
    /// Claim ahead of it in that same replay. Deciding on the prefix is
    /// deciding on history.
    ///
    /// So the authority asked for here is the one read that is already
    /// complete when it answers: `wirkd`'s own `status` (R2 — the
    /// scoped, acknowledged read `release_earlier_panes` already makes
    /// on this same socket, `WirkdApi::status`'s own doc: "kept for a
    /// caller that wants a point-in-time read"). It is taken *after*
    /// this resume has journaled its own observation, so the fold it
    /// answers with includes that observation — the very event that
    /// clears a resolved `"blocked"` cause (`fold`, `wirk-core`) — and
    /// every event that followed it. No sleep orders this and no
    /// `Working` is invented: one request, one complete answer.
    ///
    /// `Continue` requires **both** halves to say so, and says nothing
    /// on its own about prompting (`turn_ended` still governs that):
    ///
    /// * the Work folds `Active`. Every hold this loop must not clear
    ///   is `NeedsInput` in that same fold — a still-unresolved block,
    ///   a filed Question, an `out_of_boundary` refusal, a `RunFailed`,
    ///   a `RunVanished` — and every terminal or held Work
    ///   (`Completed`/`Failed`/`Canceled`/`Waiting`/`Pending`) is not
    ///   `Active` either.
    /// * this Run is `Open`. A Run already `Claimed` mid-Route leaves
    ///   its Work `Active`, and a claimed Run is not one to hand its
    ///   task back to.
    ///
    /// Anything else is `Withheld`, named in a printed line and in this
    /// invocation's exit: `Claimed` when the Run is claimed (what it
    /// is), `NeedsInput` otherwise — a human has to look, which is
    /// exactly what exit 4 means. Withheld never prompts, not even
    /// transiently.
    ///
    /// One consequence, stated rather than hidden: this makes the
    /// daemon a participant in the resume. A `wirkd` whose own `fold`
    /// predates ruling 0113 still answers `NeedsInput{blocked}` after
    /// the resumed `Idle` is journaled, so the resume withholds and
    /// exits 4 — the behaviour before ruling 0113 exactly, not a hang
    /// and not a false continuation. Restarting `wirkd` from a build
    /// that carries the fold is what the resume needs, and the journal
    /// then shows a reader exactly what this decision saw.
    fn resume_authority(
        &self,
        work_id: &WorkId,
        run: &Run,
    ) -> Result<ResumeAuthority, RunLoopError<W>> {
        let status = self.wirkd.status(work_id).map_err(RunLoopError::Wirkd)?;
        let run_state = status
            .runs
            .iter()
            .find(|entry| entry.run_id == run.id)
            .map(|entry| entry.state.clone());
        if matches!(status.work_state, WorkState::Active)
            && matches!(run_state, Some(RunState::Open))
        {
            return Ok(ResumeAuthority::Continue);
        }
        let outcome = if matches!(run_state, Some(RunState::Claimed(_))) {
            Outcome::Claimed
        } else {
            Outcome::NeedsInput
        };
        Ok(ResumeAuthority::Withheld {
            outcome,
            detail: format!(
                "resume withheld: this Work's complete current journal says {:?} (run {}: {}) — \
                 the reconciled pane is left exactly as it is and nothing is delivered to it",
                status.work_state,
                run.id.0,
                run_state
                    .map(|state| format!("{state:?}"))
                    .unwrap_or_else(|| "not present in this Work's status".to_string()),
            ),
        })
    }

    /// One watch-stream `Event`: folds it onto the loop's own tracked
    /// `Run` (`Claimed` stops the loop) and the accumulated Work
    /// (`NeedsInput` stops it too) — the only two ways `drive_channel`
    /// ever learns either, never a status poll (item C).
    ///
    /// P2.5 W3 (0050 D151; build-brief.md §7.1 amendment): `wirkd`'s
    /// `watch` always replays the entire journal before live-tailing
    /// (`handle_watch_connection`), so on a retry this method is handed
    /// the previous attempt's own `RunOpened`/`RunFailed` (and the
    /// `NeedsInput` that `RunFailed` caused) *before* it ever sees the
    /// retry's own `RunOpened` — the very event whose fold arm clears
    /// `NeedsInput` back to `Active`. Every event is still pushed onto
    /// `watch_events` and still folded exactly as before (nothing is
    /// dropped or filtered out of the accumulation: `fold` needs the
    /// full slice from `WorkSubmitted` on, and a real journal's own
    /// later events, this Run's `RunOpened` included, depend on the
    /// reservation/waypoint bookkeeping earlier events establish); what
    /// changes is that a `NeedsInput` *decision* drawn from that fold is
    /// withheld until `run_opened_this_run` is set — i.e. until the
    /// stream has delivered *this* Run's own `RunOpened`. Before that
    /// point the replayed prefix is history being caught up on, not yet
    /// "now", so it is accumulated only. A stream with no retry at all
    /// reaches this Run's own `RunOpened` on (or before) the very event
    /// that later fails it, so `NeedsInput` still surfaces exactly as
    /// today once it does.
    ///
    /// `Claimed` needs no such gate: it is decided from `run_state.apply`
    /// alone, which already ignores any event whose `run` is not this
    /// Run's own id (`Run::apply`'s own guard, `wirk-core/src/lib.rs`) —
    /// a stale prior attempt's events can never be folded onto *this*
    /// Run's state, so there is no equivalent stale-prefix hazard for it
    /// to gate.
    fn observe_watch(&mut self, event: &Event) -> Option<Outcome> {
        self.watch_events.push(event.clone());
        if !self.run_opened_this_run
            && let Some(run_state) = self.run_state.as_ref()
            && matches!(&event.kind, EventKind::RunOpened { run, .. } if run == &run_state.id)
        {
            self.run_opened_this_run = true;
        }
        if let Some(run_state) = self.run_state.as_mut() {
            run_state.apply(event);
            if matches!(run_state.state, RunState::Claimed(_)) {
                self.claimed = true;
                return Some(Outcome::Claimed);
            }
        }
        if !self.run_opened_this_run {
            return None;
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
                if self.block_this_run_has_since_left(&work) {
                    return None;
                }
                self.needs_input = true;
                return Some(Outcome::NeedsInput);
            }
        }
        None
    }

    /// Ruling 0113: true when the `NeedsInput` this fold produced is a
    /// `"blocked"` cause on the very Run this loop is driving, *and*
    /// the loop's own current observation of that pane says it is no
    /// longer blocked.
    ///
    /// `run_opened_this_run` cannot cover this case: a resume drives
    /// the SAME Run, so its own `RunOpened` sits in the replayed prefix
    /// ahead of the block, the gate opens on it, and the block that
    /// follows is read as "now" when it is in fact the condition the
    /// resume exists to have cleared. What separates the two is not
    /// position in the stream but the pane itself, so that is what is
    /// asked: `last_status` is only ever set from a status Herdr
    /// reported (the subscription, or the reconciliation's own
    /// `agent.get` reply — `observe_agent_status`), never inferred.
    ///
    /// Deliberately narrow:
    ///
    /// * only `reason == "blocked"`. A filed Question, a `RunFailed`, a
    ///   vanished Run, a refusal — every other cause is a decision this
    ///   loop has no standing to overrule, and terminal states never
    ///   reach here at all (`fold`'s own `is_terminal` guards).
    /// * only this Run's own block. Another Run's blocked pane is not
    ///   this pane, and this pane's status says nothing about it.
    /// * only `Working`/`Idle`/`Done` — the three statuses that
    ///   positively say the pane is not sitting on a prompt. `Blocked`
    ///   keeps the `NeedsInput`, and `Unknown`, or no observation at
    ///   all, is Herdr declining to answer: honest ambiguity, never a
    ///   resolution.
    ///
    /// The journal reaches the same conclusion by the same evidence:
    /// the observation this loop journals for that status is what
    /// clears the cause in `fold` (`wirk-core`), so a later reader of
    /// the journal alone sees exactly what this decision saw.
    fn block_this_run_has_since_left(&self, work: &Work) -> bool {
        let Some(cause) = work.needs_input.as_ref() else {
            return false;
        };
        let Some(run_state) = self.run_state.as_ref() else {
            return false;
        };
        cause.reason == "blocked"
            && cause.run == run_state.id
            && matches!(
                self.last_status,
                Some(AgentStatus::Working | AgentStatus::Idle | AgentStatus::Done)
            )
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
        let text = compose_first_prompt(actor, &run.kind);
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
/// Waypoint's intent, its required artifacts by name, and how the claim
/// gets filed — reused for every prompt, not only the first (fix 2: 0044
/// struck the one-nudge budget along with every other count/timer). A
/// formatting function, no new type (build-brief.md §2.2, R6).
///
/// P2.7 W2b (`tried/RESULT-w2.md`): this text used to name a `wirk
/// claim` flag (`--artifact <name>=<path> ... --done`) that has never
/// existed (`wirk/src/main.rs`'s `claim` verb takes only `--artifact
/// NAME=PATH` repeated and `--question`, no `--done`), and told every
/// actor to file the claim by hand even for a kind whose hook already
/// files it at turn end — the actor followed that instruction over its
/// own Waypoint's contrary intent. Two corrections, one predicate
/// (`claim_hook::hook_installed_for`, R2 — the same condition
/// `actor_pane` already uses to decide whether to write the hook at
/// all, never a second list of kinds):
///
/// - a kind with the hook installed is told the required outputs by
///   name, and truthfully: a claim is *attempted* at every turn end and
///   *refused* until they exist (`native-progress-contract-use/
///   HANDOFF.md` §1.4/§3 Rule 4, defect E — the old text promised "the
///   claim is filed for you", which is only true from the turn a
///   refusal stops happening; a refusal before then is state wirkd
///   already journals and the run loop already acts on, not a failure
///   for the actor to react to);
/// - a kind without the hook keeps a by-hand instruction, corrected to
///   the real, flagless form W1 built (`wirk claim` alone asks wirkd
///   for the Waypoint's declared outputs and claims each by name).
///
/// Both forms keep the same "ask for input" escape: `wirk claim
/// --question "..."`, a real flag today and unchanged by this wave.
pub fn compose_first_prompt(actor: &ActorWorld, kind: &ActorKind) -> String {
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
    let claim_line = if crate::claim_hook::hook_installed_for(kind) {
        "A claim is attempted automatically at the end of every turn and is refused until \
         the required outputs above exist, so end your turn once they do — a refusal before \
         then is a normal record, not a failure. If you need input before you can finish, \
         file `wirk claim --question \"...\"` instead."
    } else {
        "When you are done, file the claim from this pane: `wirk claim`. If you need input \
         before you can finish, file `wirk claim --question \"...\"` instead."
    };
    format!(
        "{intent}{artifacts_line}\n\n{claim_line}",
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
/// (`push_watch_event`) and closes (`close_watch`); `record` calls are
/// recorded.
///
/// `status` answers one of two ways. A reply fixed in advance
/// (`with_status`/`set_status`) is returned verbatim — what every test
/// written before the resume authority used. Otherwise, if the test
/// seeded a journal (`with_journal`), `status` answers the way the
/// daemon does: it **folds** that journal, including every event
/// `record` has appended to it since, and derives each Run's own state
/// by replaying it through `Run::apply` (0040 D127 — a fake behaves
/// like the service; a canned `WorkState` here would let a test assert
/// a resume decision the real daemon would never have answered with).
#[derive(Debug)]
pub struct FakeWirkdApi {
    status_response: Mutex<Option<WorkStatus>>,
    /// The journal this fake answers `status` from when no reply is
    /// fixed in advance: seeded by the test, appended to by `record`.
    journal: Mutex<Vec<Event>>,
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
            journal: Mutex::new(Vec::new()),
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

    /// Seeds the journal this fake folds to answer `status` — the
    /// history that already existed when the drive under test started,
    /// exactly what a real `wirkd` would have on disk. `record` appends
    /// to this same journal, so an observation the loop writes is in
    /// the next `status` answer, as it is in the real one.
    pub fn with_journal(self, events: Vec<Event>) -> Self {
        *self.journal.lock().unwrap() = events;
        self
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

/// Every Run `events` opens, with the state replaying those same events
/// through `Run::apply` leaves it in — the daemon's own `find_run`
/// (`wirk/src/wirkd/server.rs`) reduced to the one field
/// `WirkdApi::status` reports. `Run::apply` already ignores an event
/// naming another Run, so one pass over the journal serves them all.
fn fake_run_states(events: &[Event]) -> Vec<RunStatusEntry> {
    let mut runs: Vec<Run> = Vec::new();
    for event in events {
        if let EventKind::RunOpened {
            run: opened,
            waypoint,
            attempt,
            world_hash,
        } = &event.kind
            && !runs.iter().any(|run| &run.id == opened)
        {
            runs.push(Run {
                id: opened.clone(),
                waypoint: waypoint.clone(),
                attempt: *attempt,
                world_hash: world_hash.clone(),
                state: RunState::Open,
                kind: ActorKind::default(),
                selection: wirk_core::ActorSelection::default(),
                launched: false,
                launch_requested: false,
                launch_attempt: None,
                launch_argv: Vec::new(),
                expansions: Vec::new(),
            });
        }
        for run in runs.iter_mut() {
            run.apply(event);
        }
    }
    runs.into_iter()
        .map(|run| RunStatusEntry {
            run_id: run.id,
            state: run.state,
        })
        .collect()
}

#[derive(Debug, Error, Clone)]
#[error("FakeWirkdApi: {0}")]
pub struct FakeWirkdError(pub String);

impl WirkdApi for FakeWirkdApi {
    type Error = FakeWirkdError;

    fn status(&self, _work_id: &WorkId) -> Result<WorkStatus, Self::Error> {
        *self.status_calls.lock().unwrap() += 1;
        if let Some(fixed) = self.status_response.lock().unwrap().clone() {
            return Ok(fixed);
        }
        let journal = self.journal.lock().unwrap().clone();
        // A real journal always opens with `WorkSubmitted` (`fold`'s
        // own documented precondition); a fake whose test seeded no
        // history has no Work to answer for, exactly as `wirkd` answers
        // `NotFound` for a Work it has no journal for.
        if !journal
            .iter()
            .any(|event| matches!(event.kind, EventKind::WorkSubmitted { .. }))
        {
            return Err(FakeWirkdError("no status configured".to_string()));
        }
        Ok(WorkStatus {
            work_state: fold(&journal).state,
            runs: fake_run_states(&journal),
        })
    }

    fn record(&self, work_id: &WorkId, run_id: &RunId, kind: EventKind) -> Result<(), Self::Error> {
        let mut journal = self.journal.lock().unwrap();
        let seq = journal.len();
        journal.push(Event {
            id: wirk_core::EventId(format!("fake-event-{seq}")),
            work: work_id.clone(),
            run: Some(run_id.clone()),
            at: Timestamp(0),
            kind: kind.clone(),
        });
        drop(journal);
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
