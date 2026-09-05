//! `RunLoop` tests (item 4, W2; rebuilt fix 2, ruling 0044, W3). Every
//! fake-backed test below drives `RunLoop::drive` against a real
//! channel for both streams it blocks on (`FakeHerdrClient::
//! with_subscribe_channel`, `FakeWirkdApi::push_watch_event`/
//! `close_watch`) — a test feeds and closes them, never a canned
//! one-shot reply standing in for what is, in production, a blocking
//! stream (0040 D127). `drive` blocks, so every such test runs it on
//! its own thread and joins it after feeding (and, where the scenario
//! calls for it, closing) both channels. `d9_6` is the real half
//! `wirk-core` cannot run itself (0001 D7's crate boundary): real `git`
//! in a tempdir, via `wirk_herdr::git`.

#[path = "support/live_herdr.rs"]
mod live_herdr;

use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ClaimId, ClaimKind, ClaimVerdict, Event,
    EventId, EventKind, ExecutionTriple, FailureCause, OutputContract, RouteId, Run, RunId,
    RunState, Timestamp, WaypointId, WorkId, World, WorldHash,
};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::run_loop::{FakeWirkdApi, Outcome, RunLoop, RunLoopError};
use wirk_herdr::{AgentStatus, HerdrError, HerdrEvent, PaneInfo};

fn work_id() -> WorkId {
    WorkId("work-1".to_string())
}

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: ActorKind::Opencode,
    }
}

fn actor_world(run: &Run, worktree_path: &std::path::Path) -> World {
    World::Actor(ActorWorld {
        repository: "wirk".to_string(),
        worktree_path: worktree_path.to_path_buf(),
        branch: "p1/herdr-executor".to_string(),
        base_sha: "abc123".to_string(),
        triple: ExecutionTriple {
            estate_root: "/estate".to_string(),
            work_id: work_id(),
            run_id: run.id.clone(),
        },
        intent: "write report.md summarizing the repo".to_string(),
        output_contract: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["src/**".to_string()]),
    })
}

fn pane_info(pane_id: &str, agent_status: AgentStatus, revision: u64) -> PaneInfo {
    PaneInfo {
        pane_id: pane_id.to_string(),
        terminal_id: format!("term-{pane_id}"),
        workspace_id: "w1".to_string(),
        tab_id: "tab1".to_string(),
        focused: false,
        agent_status,
        revision,
        agent: None,
        agent_session: None,
        cwd: None,
        display_agent: None,
        foreground_cwd: None,
        label: None,
        scroll: None,
        state_labels: None,
        terminal_title: None,
        terminal_title_stripped: None,
        title: None,
        tokens: None,
    }
}

fn status_changed(run: &Run, status: AgentStatus) -> HerdrEvent {
    HerdrEvent::PaneAgentStatusChanged {
        pane_id: run.id.0.clone(),
        workspace_id: "w1".to_string(),
        agent: Some("opencode".to_string()),
        agent_status: status,
        display_agent: None,
        state_labels: None,
        title: None,
    }
}

fn watch_event(run_id: Option<&RunId>, kind: EventKind) -> Event {
    Event {
        id: EventId("ev".to_string()),
        work: work_id(),
        run: run_id.cloned(),
        at: Timestamp(0),
        kind,
    }
}

fn work_submitted() -> EventKind {
    EventKind::WorkSubmitted {
        route: RouteId("route-1".to_string()),
        repositories: Vec::new(),
        intent: "write report.md".to_string(),
        waypoints: vec![WaypointId("route-1/wp-1".to_string())],
        waypoint_defs: Vec::new(),
    }
}

fn run_opened(run: &Run) -> EventKind {
    EventKind::RunOpened {
        run: run.id.clone(),
        waypoint: run.waypoint.clone(),
        attempt: run.attempt,
        world_hash: run.world_hash.clone(),
    }
}

fn claim_recorded_done(claim_id: &str) -> EventKind {
    EventKind::ClaimRecorded {
        claim: ClaimId(claim_id.to_string()),
        claim_kind: ClaimKind::Done,
        verdict: ClaimVerdict::Validated,
    }
}

fn waypoint_reserved(run: &Run, world: World) -> EventKind {
    EventKind::WaypointReserved {
        waypoint: run.waypoint.clone(),
        world_hash: run.world_hash.clone(),
        world,
    }
}

fn run_launched(run: &Run) -> EventKind {
    EventKind::RunLaunched {
        run: run.id.clone(),
        actor_kind: run.kind,
    }
}

/// The exact `RunFailed` shape read from the live journal (0050 D151,
/// `wirkd-watch-a2.ndjson`): `agent_pane_busy`, the cause P2.4's own
/// tried step actually hit.
fn run_failed_agent_pane_busy() -> EventKind {
    EventKind::RunFailed {
        cause: FailureCause {
            status: None,
            request_id: None,
            at: Timestamp(0),
            detail: Some(
                "invalid: agent_pane_busy: agent target pane is not an available shell".to_string(),
            ),
        },
    }
}

fn claim_recorded_question(claim_id: &str) -> EventKind {
    EventKind::ClaimRecorded {
        claim: ClaimId(claim_id.to_string()),
        claim_kind: ClaimKind::Question("what should I do".to_string()),
        verdict: ClaimVerdict::Validated,
    }
}

/// A `FakeHerdrClient` wired for `HerdrExecutor::launch_actor` to
/// succeed against `run`'s pane, with `subscribe` backed by a real
/// channel the test feeds via the returned `Sender` — `RunLoop::drive`
/// reads it exactly like `SocketClient`'s own live subscription (module
/// doc).
fn client_for(
    run: &Run,
) -> (
    Arc<FakeHerdrClient>,
    mpsc::Sender<Result<HerdrEvent, HerdrError>>,
) {
    let (tx, rx) = mpsc::channel();
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_response(pane_info(&run.id.0, AgentStatus::Idle, 1))
            .with_subscribe_channel(rx),
    );
    (client, tx)
}

/// Runs `loop_.drive(&work_id(), run, world)` on its own thread (module
/// doc: `drive` blocks) and hands back the `JoinHandle` to join once the
/// test has fed (and, where needed, closed) both channels.
fn spawn_drive(
    mut loop_: RunLoop<Arc<FakeHerdrClient>, Arc<FakeWirkdApi>>,
    run: Run,
    world: World,
) -> std::thread::JoinHandle<Result<Outcome, RunLoopError<Arc<FakeWirkdApi>>>> {
    std::thread::spawn(move || loop_.drive(&work_id(), &run, &world))
}

/// Bounded poll (a test's own termination bound, never a product one —
/// the owner's ruling of 2026-09-02 §3) for `predicate` to become true;
/// panics naming what was never observed rather than hanging the suite.
fn wait_until(what: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "never observed: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---- (1) the run 2 bug: a second, content-identical Idle is prompted -----

#[test]
fn the_run2_bug_a_second_identical_idle_is_still_prompted() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Idle, Working, then a second Idle identical in content to the
    // first (fix 2's own bug: the old `Reconciler` hashed this as a
    // replay and dropped it). The second prompt here is the intent's
    // own first *continuation* (P2.3 W6: no baseline existed after the
    // intent, so it is earned unconditionally regardless of the
    // worktree) — the `b.txt` write below is not what earns it, it just
    // keeps this scenario a real turn rather than a no-op one, since a
    // later wave may add a comparison here too.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    std::fs::write(dir.path().join("b.txt"), b"a real turn happened\n").expect("write b.txt");
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("second prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });

    // End the drive cleanly: a Claim on the watch stream.
    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

// ---- (2) Working then Blocked across many events: zero prompts, one notify

#[test]
fn working_then_blocked_sends_zero_prompts() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Blocked)))
        .unwrap();
    // P2.3 W4 (build-brief.md §8 finding 2): the transition into Blocked
    // notifies once — waited for here so the 20 `PaneUpdated` events
    // below (a different `HerdrEvent` variant `observe_herdr` ignores
    // outright, standing in for Herdr's own chatter while a pane sits
    // idle-blocked) cannot race the assertion below.
    wait_until("blocked notify sent", || {
        client.notify_calls.lock().unwrap().len() == 1
    });
    for _ in 0..20 {
        herdr_tx
            .send(Ok(HerdrEvent::PaneUpdated {
                pane: pane_info(&run.id.0, AgentStatus::Blocked, 2),
            }))
            .unwrap();
    }
    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
    assert_eq!(
        client.prompt_agent_calls.lock().unwrap().len(),
        0,
        "never prompted while Working or Blocked"
    );
    let notify_calls = client.notify_calls.lock().unwrap();
    assert_eq!(
        notify_calls.len(),
        1,
        "exactly one notify on the transition to Blocked, none of the 20 no-op \
         PaneUpdated events after it firing a second: {notify_calls:?}"
    );
    assert_eq!(
        notify_calls[0].body, run.id.0,
        "the notify body must name the pane: {:?}",
        notify_calls[0]
    );
}

/// P2.6 W2 (ruling 0052 D156): the `LifecycleObserved` the loop
/// journals on the transition into `Blocked` carries the pane's last
/// screen lines (`HerdrClient::read_pane`, R2/R5) as `detail` — the
/// fold side of D156 (`wirk-core`'s `needs_input.rs`) is pinned
/// separately; this pins that the loop is the one supplying the text
/// `fold` reads. Red before this wave: `LifecycleObserved` carried no
/// `detail` field at all.
#[test]
fn blocked_observation_journals_the_panes_screen_lines() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (tx, rx) = mpsc::channel();
    let screen = "┃ Permission required\n┃ Access external directory /tmp".to_string();
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_response(pane_info(&run.id.0, AgentStatus::Idle, 1))
            .with_pane_read_response(&run.id.0, Ok(screen.clone()))
            .with_subscribe_channel(rx),
    );
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    tx.send(Ok(status_changed(&run, AgentStatus::Blocked)))
        .unwrap();
    wait_until("blocked notify sent", || {
        client.notify_calls.lock().unwrap().len() == 1
    });

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);

    let recorded = wirkd.recorded();
    let blocked_event = recorded
        .iter()
        .find(|(_, _, kind)| matches!(kind, EventKind::LifecycleObserved { status, .. } if status == "Blocked"))
        .map(|(_, _, kind)| kind)
        .expect("a LifecycleObserved{Blocked} must be recorded");
    let EventKind::LifecycleObserved { detail, .. } = blocked_event else {
        unreachable!()
    };
    let detail = detail
        .as_ref()
        .expect("a Blocked observation must carry Some(detail)");
    assert!(
        detail.contains(&run.id.0) && detail.contains(&screen),
        "detail must name the pane and carry its last screen lines verbatim: {detail:?}"
    );
}

/// A Blocked episode that clears (a later Working) and recurs notifies
/// again — the flag is per-episode, not per-Run.
#[test]
fn blocked_then_working_then_blocked_notifies_twice() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Blocked)))
        .unwrap();
    wait_until("first blocked notify sent", || {
        client.notify_calls.lock().unwrap().len() == 1
    });
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Blocked)))
        .unwrap();
    wait_until("second blocked notify sent", || {
        client.notify_calls.lock().unwrap().len() == 2
    });

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
    assert_eq!(
        client.notify_calls.lock().unwrap().len(),
        2,
        "a second Blocked episode notifies again, once each"
    );
}

/// The pane reporting `Blocked` again with no `Working` between (Herdr
/// re-announcing the same status, or a duplicate status event) is not a
/// new episode — `observe_herdr`'s own `changed` guard means this
/// scenario never actually reaches the notify branch a second time, but
/// pinned explicitly since it is the shape build-brief.md §8 names
/// ("Blocked staying Blocked across several polls: still one").
#[test]
fn blocked_staying_blocked_across_several_polls_still_notifies_once() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    for _ in 0..5 {
        herdr_tx
            .send(Ok(status_changed(&run, AgentStatus::Blocked)))
            .unwrap();
    }
    wait_until("blocked notify sent", || {
        client.notify_calls.lock().unwrap().len() == 1
    });
    // A few more identical Blocked events after the notify already fired.
    for _ in 0..5 {
        herdr_tx
            .send(Ok(status_changed(&run, AgentStatus::Blocked)))
            .unwrap();
    }

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
    assert_eq!(
        client.notify_calls.lock().unwrap().len(),
        1,
        "staying Blocked across many polls notifies only once"
    );
}

// ---- (3) ClaimRecorded stops the loop with zero status calls -------------

#[test]
fn claim_recorded_on_the_watch_stream_stops_the_loop_with_no_status_call() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, _herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client, wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
    assert_eq!(
        wirkd.status_calls(),
        0,
        "Claimed is learned from the watch stream, never a status poll"
    );
}

// ---- (4) NeedsInput on the stream stops the loop --------------------------

#[test]
fn needs_input_on_the_watch_stream_stops_the_loop() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, _herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client, wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    wirkd.push_watch_event(watch_event(None, work_submitted()));
    wirkd.push_watch_event(watch_event(Some(&run.id), run_opened(&run)));
    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_question("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::NeedsInput);
}

// ---- (4b) P2.5 W3: the retry race (0050 D151) ------------------------------
//
// `wirkd::watch` always replays the *entire* journal before live-tailing
// (`handle_watch_connection`), so a retry's own driver is handed the
// previous attempt's `RunOpened`/`RunFailed` before it ever reaches the
// retry's own `RunOpened` -- the one event whose fold arm clears
// `NeedsInput` back to `Active`. Before the fix, `observe_watch` decided
// `NeedsInput` off of every pushed event, including that stale prefix,
// and returned before the retry's own `RunOpened` (let alone its Claim)
// was ever read from the channel.

/// (a) The exact P2.4 sequence (`wirkd-watch-a2.ndjson`): attempt 1 opens,
/// launches, and fails `agent_pane_busy` (the `NeedsInput`-causing
/// event) -- all *before* the retry's own `RunOpened` for `run-2`, the
/// Run this `RunLoop` is actually driving. Red before the fix: the loop
/// folds the stale `RunFailed` before `run-2`'s own `RunOpened` is ever
/// pushed and returns `Outcome::NeedsInput` right there, so the drive
/// never reaches `run-2`'s own Claim below and this assertion fails.
#[test]
fn retry_does_not_exit_needs_input_on_the_previous_runs_history() {
    let run1 = open_run("run-1");
    let run2 = open_run("run-2");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run2, dir.path());
    let (client, _herdr_tx) = client_for(&run2);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client, wirkd.clone());

    let handle = spawn_drive(loop_, run2.clone(), world.clone());

    wirkd.push_watch_event(watch_event(None, work_submitted()));
    wirkd.push_watch_event(watch_event(None, waypoint_reserved(&run1, world.clone())));
    wirkd.push_watch_event(watch_event(Some(&run1.id), run_opened(&run1)));
    wirkd.push_watch_event(watch_event(Some(&run1.id), run_launched(&run1)));
    wirkd.push_watch_event(watch_event(Some(&run1.id), run_failed_agent_pane_busy()));
    // Attempt 1's own `RunFailed` already put the Work in `NeedsInput`
    // (0050 D151's actual journal has no separate refusal event for
    // this cause -- `RunFailed` alone is the cause here, per fold.md
    // §1). `wirk work retry` reserves the same waypoint again and opens
    // a fresh Run:
    wirkd.push_watch_event(watch_event(None, waypoint_reserved(&run2, world.clone())));
    wirkd.push_watch_event(watch_event(Some(&run2.id), run_opened(&run2)));
    wirkd.push_watch_event(watch_event(Some(&run2.id), run_launched(&run2)));
    wirkd.push_watch_event(watch_event(Some(&run2.id), claim_recorded_done("c1")));

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(
        outcome,
        Outcome::Claimed,
        "the retry's own RunOpened must clear the previous attempt's \
         NeedsInput before this Run's own Claim is ever reached -- a \
         stale replayed prefix must never end the drive early"
    );
}

/// (b) No retry at all: this Run's own `RunOpened`, then a `RunFailed`
/// for the *same* Run. The gate (§7.1) hides only a stale prefix from
/// *before* this Run's own `RunOpened`, never a real failure of this
/// Run itself -- `NeedsInput` must still surface exactly as today.
#[test]
fn run_failed_for_this_run_after_its_own_run_opened_still_surfaces_needs_input() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, _herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client, wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    wirkd.push_watch_event(watch_event(None, work_submitted()));
    wirkd.push_watch_event(watch_event(Some(&run.id), run_opened(&run)));
    wirkd.push_watch_event(watch_event(Some(&run.id), run_failed_agent_pane_busy()));

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::NeedsInput);
}

/// (c) A Claim for this Run arriving after its own `RunOpened`: `Claimed`,
/// unaffected by the §7.1 gate -- `Claimed` is decided from `Run::apply`
/// alone, which already ignores any event that does not name this Run.
#[test]
fn claim_for_this_run_after_its_own_run_opened_is_claimed() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, _herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client, wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    wirkd.push_watch_event(watch_event(None, work_submitted()));
    wirkd.push_watch_event(watch_event(Some(&run.id), run_opened(&run)));
    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

// ---- (5) either stream closing --------------------------------------------

/// The Herdr subscription's channel closing (its sender dropped) is
/// `RunVanished`, journaled, `Outcome::Vanished`.
#[test]
fn the_herdr_channel_closing_journals_run_vanished_and_returns_vanished() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client, wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);
    drop(herdr_tx); // EOF: Herdr is gone

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Vanished);
    let recorded = wirkd.recorded();
    assert!(
        recorded
            .iter()
            .any(|(_, run_id, kind)| run_id == &run.id && matches!(kind, EventKind::RunVanished)),
        "RunVanished must be journaled: {recorded:?}"
    );
}

/// The wirkd `watch` channel closing (`EOF`) is a fatal error naming
/// wirkd — nothing can be journaled about it, since wirkd is the thing
/// that is gone.
#[test]
fn the_watch_channel_closing_errors_naming_wirkd() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, _herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client, wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);
    wirkd.close_watch();

    let err = handle.join().unwrap().expect_err("watch ending is fatal");
    assert!(
        matches!(err, RunLoopError::WirkdGone { .. }),
        "expected WirkdGone, got {err:?}"
    );
    // (d), P2.3 W5 (build-brief.md §9): the loop's own error text names
    // wirkd -- the live twin (`wirk/tests/run_verb.rs`) is where the
    // driver's printed line carrying this text is read; this fake-backed
    // test pins only the error text itself, which `wirk run`'s generic
    // `Err(err) => eprintln!("wirk run: {err}")` arm prints verbatim.
    assert!(
        err.to_string().contains("wirkd"),
        "the error must name wirkd: {err}"
    );
}

// ---- (6) no progress vs. progress -----------------------------------------
//
// P2.3 W4 (build-brief.md §8 finding 1): progress since a prompt is the
// worktree fingerprint alone -- the pane's own revision left the
// comparison, because any output by the actor (answering a prompt,
// thinking aloud) advances it whether or not the actor did anything, so
// counting it as progress meant an actor that only ever answers and
// never edits was never judged stuck.

/// The intent prompt, an unconditional first continuation (P2.3 W6: no
/// baseline existed after the intent, so this Idle earns a prompt no
/// matter what — build-brief.md §10), then Idle again with the worktree
/// still unchanged: *now* the actor is stuck — `NeedsInput`, and no
/// third prompt is ever sent. The pane's own `get_pane` response is not
/// even configured here (`launch_actor` still needs a pane to open, via
/// `client_for`'s own `with_split_pane_response`) — the stuck path no
/// longer reads it at all.
#[test]
fn no_progress_since_the_last_prompt_stops_the_loop_needs_input() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt (P2.3 W6: no baseline taken after
    // it).
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Turn end 2: still no baseline existed, so this earns the first
    // *continuation* unconditionally — the baseline is taken now.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Turn end 3: nothing about the worktree changed since that
    // continuation's own baseline — the actor is now judged stuck.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::NeedsInput);
    assert_eq!(
        client.prompt_agent_calls.lock().unwrap().len(),
        2,
        "no third prompt once the actor is judged stuck"
    );
}

/// (a) The pane's own revision advances between prompts, but the
/// worktree never does: still stuck once a baseline exists to compare
/// against — `NeedsInput`, no prompt beyond the intent and its first
/// continuation. Red before P2.3 W4's fix: the old comparison
/// (`ProgressBaseline` carrying `pane_revision`) counted the revision
/// bump alone as progress, so this same scenario used to prompt again
/// (see `worktree_change_is_progress_prompts_again` below for the
/// scenario that legitimately does).
#[test]
fn pane_revision_alone_is_not_progress_stops_the_loop_needs_input() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    client.get_pane_responses.lock().unwrap().insert(
        run.id.0.clone(),
        Ok(pane_info(&run.id.0, AgentStatus::Idle, 7)),
    );
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt, no baseline taken.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // The pane's own revision moves on between every turn -- Herdr's own
    // accounting of the actor answering -- but the worktree is untouched
    // throughout.
    client.get_pane_responses.lock().unwrap().insert(
        run.id.0.clone(),
        Ok(pane_info(&run.id.0, AgentStatus::Idle, 8)),
    );
    // Turn end 2: no baseline existed yet -- the first continuation,
    // unconditional, baseline taken now.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    client.get_pane_responses.lock().unwrap().insert(
        run.id.0.clone(),
        Ok(pane_info(&run.id.0, AgentStatus::Idle, 9)),
    );
    // Turn end 3: the revision moved again, the worktree never did.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(
        outcome,
        Outcome::NeedsInput,
        "the pane revision alone must not count as progress"
    );
    assert_eq!(
        client.prompt_agent_calls.lock().unwrap().len(),
        2,
        "no prompt beyond the intent and its first continuation: the actor answered but never \
         touched the worktree"
    );
}

/// (b) The worktree changes between the first continuation and the
/// turn end after it (the pane's own revision held fixed, to isolate
/// the claim): not stuck, prompted a third time.
#[test]
fn worktree_change_is_progress_prompts_again() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    client.get_pane_responses.lock().unwrap().insert(
        run.id.0.clone(),
        Ok(pane_info(&run.id.0, AgentStatus::Idle, 7)),
    );
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt, no baseline taken.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Turn end 2: no baseline existed yet -- the first continuation,
    // unconditional, baseline taken now (worktree still untouched).
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Progress since that continuation's own baseline: the worktree
    // itself changed (the pane's own revision is left exactly as it
    // was, at 7, to isolate this from (a) above).
    std::fs::write(dir.path().join("b.txt"), b"a real edit happened\n").expect("write b.txt");
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("third prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 3
    });

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

// ---- P2.3 W5: Done is a turn end, exactly like Idle -----------------------
//
// build-brief.md §9 (the rerun): Herdr's own `status_name` (`refs/herdr/
// src/app/agent_view.rs`) reports the actor's turn ending as `Done`, not
// `Idle`, whenever the pane has not been *viewed* since -- every pane
// `wirk run` drives, since it is headless. Red before this wave's fix
// (`git stash push -- wirk-herdr/src/run_loop.rs`, pasted in BUILD.md):
// `observe_herdr`'s guard was `matches!(agent_status, AgentStatus::Idle)`
// alone, so a pane that never reported a second `Idle` -- only `Done` --
// fell straight through every one of these three scenarios: no prompt,
// no stuck check, nothing.

/// (a) The intent prompt, then an unconditional first continuation
/// (P2.3 W6: no baseline existed after the intent), then the pane's
/// next turn end reports `Done` (never a second `Idle`) with the
/// worktree unchanged since that continuation: the actor is stuck --
/// `NeedsInput` with the stuck observation journaled, exactly as an
/// unchanged `Idle` would be.
#[test]
fn prompted_then_done_with_worktree_unchanged_is_stuck() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt, no baseline taken.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Turn end 2: no baseline existed yet -- the first continuation,
    // unconditional, baseline taken now. This run's own pane reports
    // `Done`, never a second `Idle` -- headless, never viewed.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Turn end 3: nothing about the worktree changed since that
    // continuation's own baseline; the pane's next turn end reports
    // `Done` again.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::NeedsInput);
    assert_eq!(
        client.prompt_agent_calls.lock().unwrap().len(),
        2,
        "no third prompt once the actor is judged stuck on a Done turn end"
    );
    let stuck_failures: Vec<_> = wirkd
        .recorded()
        .into_iter()
        .filter_map(|(_, _, kind)| match kind {
            EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck") => {
                Some(cause)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        stuck_failures.len(),
        1,
        "the stuck observation must be journaled from a Done turn end too"
    );
}

/// (b) Working then Done (never a second Idle), the Run unclaimed: a
/// prompt is sent -- Done alone earns it, exactly as an Idle would.
#[test]
fn working_then_done_prompts_the_pane() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();
    wait_until("prompt sent on Done", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

/// (c) Blocked then Done, no Working in between: the Blocked flag
/// clears on the transition (proven here by the prompt firing at all --
/// `maybe_prompt` refuses outright while `self.blocked` is still true)
/// and the pane is prompted, exactly as a Blocked-then-Idle transition
/// already was.
#[test]
fn blocked_then_done_clears_blocked_and_prompts() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Blocked)))
        .unwrap();
    wait_until("blocked notify sent", || {
        client.notify_calls.lock().unwrap().len() == 1
    });
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();
    wait_until("prompt sent after Blocked cleared by Done", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

// ---- P2.3 W6: the baseline is taken after a continuation, not the intent -
//
// build-brief.md §10 (rerun2's own correction, ruling 0044): the
// progress baseline used to be taken right after *every* prompt this
// loop sent, the very first one (the intent) included — so an actor
// whose first turn is reading and planning, ending with no worktree
// edit yet, was declared stuck without ever having been told to
// continue (rerun2's own evidence: stuck 22s after launch, on the very
// first turn end). Red before this wave's fix (probed by hand, BUILD.md:
// taking the baseline after the intent again makes (a) below fail —
// `NeedsInput` fires one turn end early, with only one prompt ever sent,
// not two).

/// (a) The intent prompt, then a turn end with the worktree unchanged:
/// no baseline existed yet (none is ever taken after the intent), so
/// this earns an unconditional *continuation* prompt — the baseline is
/// taken now — and the actor is **not** yet judged stuck, even though
/// the worktree has not moved since the intent was sent.
#[test]
fn intent_then_unchanged_turn_end_earns_a_continuation_not_stuck() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Turn end 2: Done (headless — never a second Idle), worktree
    // untouched since the intent. Not stuck: no baseline existed to
    // compare against, so this is the first continuation instead.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    assert!(
        wirkd.recorded().into_iter().all(|(_, _, kind)| !matches!(
            kind,
            EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck")
        )),
        "no stuck RunFailed yet — the actor has not been told to continue and failed to yet"
    );

    let prompt_lines_progress = client.prompt_agent_calls.lock().unwrap().len();
    assert_eq!(
        prompt_lines_progress, 2,
        "exactly the intent and its first continuation, nothing judged stuck"
    );

    // End the drive cleanly: a Claim on the watch stream.
    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

/// (b) Continuing from (a)'s own scenario: a *second* turn end with the
/// worktree still unchanged — this time a baseline does exist (taken
/// right after the first continuation), so this is the actor judged
/// stuck: told to continue once, and did nothing since.
#[test]
fn second_unchanged_turn_end_after_the_continuation_is_stuck() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Second turn end: still nothing since the continuation's own
    // baseline.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::NeedsInput);
    assert_eq!(
        client.prompt_agent_calls.lock().unwrap().len(),
        2,
        "no third prompt: the actor is judged stuck instead"
    );
    let stuck_failures: Vec<_> = wirkd
        .recorded()
        .into_iter()
        .filter_map(|(_, _, kind)| match kind {
            EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck") => {
                Some(cause)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        stuck_failures.len(),
        1,
        "the stuck observation must be journaled once told to continue and doing nothing since"
    );
}

/// (c) Continuing from (a)'s own scenario, but this time the worktree
/// *does* change before the next turn end: progress since the
/// continuation's own baseline — another continuation is sent, never
/// `NeedsInput`.
#[test]
fn worktree_change_after_the_continuation_prompts_again_not_stuck() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Progress since the continuation's own baseline: a real edit.
    std::fs::write(
        dir.path().join("c.txt"),
        b"progress after the continuation\n",
    )
    .expect("write c.txt");
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Done)))
        .unwrap();
    wait_until("second continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 3
    });

    assert!(
        wirkd.recorded().into_iter().all(|(_, _, kind)| !matches!(
            kind,
            EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck")
        )),
        "worktree progress since the continuation must never be judged stuck"
    );

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

// ---- P2.3 W3/W4: the loop prints one line per prompt it sends ------------

/// `worktree_change_is_progress_prompts_again`'s own scenario (a first
/// prompt, then a second once the worktree itself changes), extended to
/// assert on the printed lines themselves rather than only the prompt
/// count: BRIEF.md's amendment names the gap this pins ("the loop
/// prints nothing when it prompts, so run 3's evidence counted zero
/// prompts where the journal shows two"). `with_captured_output`
/// redirects `log_line` here instead of the real stdout (its own doc:
/// many fake-backed tests share one process, so reading the real stdout
/// is not reliable) -- the live twin below reads the real child
/// process's stdout instead, needing no such redirection. P2.3 W4: the
/// second line's wording asserts on the worktree fingerprint change,
/// not a pane revision (build-brief.md §8 finding 1).
#[test]
fn the_loop_prints_one_line_per_prompt_it_sends() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let output: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let loop_ = RunLoop::new(client.clone(), wirkd.clone()).with_captured_output(output.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt (no baseline exists to compare).
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Turn end 2: still no baseline (none is taken after the intent,
    // P2.3 W6) -- the first continuation, unconditional, baseline taken
    // now.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Progress since that continuation's own baseline: the worktree
    // itself changed (a real turn happened).
    std::fs::write(dir.path().join("b.txt"), b"a real edit happened\n").expect("write b.txt");
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("third prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 3
    });

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    handle.join().unwrap().expect("drive");

    let lines = output.lock().unwrap().clone();
    let prompt_lines: Vec<&String> = lines.iter().filter(|l| l.starts_with("prompt:")).collect();
    assert_eq!(
        prompt_lines.len(),
        3,
        "one printed line per prompt sent (three prompts): {lines:?}"
    );
    assert!(
        prompt_lines[0].contains("Idle answered") && prompt_lines[0].contains("first prompt"),
        "the intent's own line must name the Idle it answered and that no earlier baseline \
         existed to compare -- the same shape a later prompt's line takes: {:?}",
        prompt_lines[0]
    );
    assert!(
        prompt_lines[1].contains("Idle answered") && prompt_lines[1].contains("first continuation"),
        "the first continuation's line must name the Idle it answered and that this is the \
         first continuation, the point the baseline is taken (P2.3 W6): {:?}",
        prompt_lines[1]
    );
    assert!(
        prompt_lines[2].contains("Idle answered") && prompt_lines[2].contains("worktree changed"),
        "the third prompt's line must name the Idle it answered and the worktree progress \
         observed since the first continuation's own baseline (P2.3 W4: the fingerprint, not a \
         pane revision): {:?}",
        prompt_lines[2]
    );
    for line in &prompt_lines {
        assert!(
            line.contains("sending:"),
            "every prompt line must name the first words of the prompt text: {line:?}"
        );
    }
}

// ---- P2.3 W1: the stuck observation is journaled, and notify fires once --

/// The no-progress branch journals exactly one `RunFailed` whose
/// `cause.status == Some("stuck")` and whose `detail` names the pane
/// (build-brief.md §7 amendment 2), through `WirkdApi::record` — before
/// this wave the branch journaled nothing (states.md's own red).
#[test]
fn run_loop_no_progress_journals_run_failed_stuck() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    client.get_pane_responses.lock().unwrap().insert(
        run.id.0.clone(),
        Ok(pane_info(&run.id.0, AgentStatus::Idle, 7)),
    );
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt, no baseline taken.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Turn end 2: no baseline existed yet -- the first continuation,
    // unconditional, baseline taken now.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Turn end 3: nothing about the worktree changed since that
    // continuation's own baseline -- the actor is now judged stuck.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::NeedsInput);

    let stuck_failures: Vec<_> = wirkd
        .recorded()
        .into_iter()
        .filter_map(|(_, _, kind)| match kind {
            EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck") => {
                Some(cause)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        stuck_failures.len(),
        1,
        "exactly one RunFailed{{status:\"stuck\"}} must be journaled"
    );
    let detail = stuck_failures[0]
        .detail
        .as_deref()
        .expect("the stuck cause carries a detail");
    assert!(
        detail.contains(&run.id.0),
        "the stuck observation must name the pane (run id {}): {detail:?}",
        run.id.0
    );
}

/// `notify` fires exactly once on the stuck path, through the fake
/// client that records `notification.show` calls, carrying the run id
/// in `body` (states.md §2/§4; the loop calls `notify` only from the
/// no-progress branch, never from `observe_watch`'s own `NeedsInput`
/// fold — `drive_channel` returns on the first `Some(Outcome)` any
/// branch produces, so the two branches can never both run inside one
/// `drive()` call; a second `notify_needs_input` call added to
/// `observe_watch` is therefore inert against this exact scenario).
/// Mutation probe run by hand (BUILD.md): duplicating the
/// `notify_needs_input` call within the stuck branch itself makes this
/// assertion fail (2 != 1); reverted before landing.
#[test]
fn run_loop_needs_input_calls_notify_once() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    git_init_repo(dir.path());
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    client.get_pane_responses.lock().unwrap().insert(
        run.id.0.clone(),
        Ok(pane_info(&run.id.0, AgentStatus::Idle, 7)),
    );
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Turn end 1: the intent prompt, no baseline taken.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("intent prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Turn end 2: no baseline existed yet -- the first continuation,
    // unconditional, baseline taken now.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first continuation prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });
    // Turn end 3: nothing about the worktree changed since that
    // continuation's own baseline -- the actor is now judged stuck.
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();

    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::NeedsInput);

    let notify_calls = client.notify_calls.lock().unwrap();
    assert_eq!(
        notify_calls.len(),
        1,
        "notify must fire exactly once: {notify_calls:?}"
    );
    assert!(
        notify_calls[0].body.contains(&run.id.0),
        "the notify body must name the run: {:?}",
        notify_calls[0]
    );
}

// ---- (7) the prompt text carries the artifact name and the claim ---------
// ---- instruction -----------------------------------------------------

#[test]
fn the_prompt_carries_the_artifact_name_and_the_claim_instruction() {
    let run = open_run("run-1");
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    handle.join().unwrap().expect("drive");

    let calls = client.prompt_agent_calls.lock().unwrap();
    let text = &calls.first().expect("one prompt sent").text;
    assert!(text.contains("report.md"), "missing artifact name: {text}");
    assert!(
        text.contains("wirk claim"),
        "missing the literal claim instruction: {text}"
    );
    assert!(
        text.contains("write report.md summarizing the repo"),
        "missing the Waypoint's own intent: {text}"
    );
}

// ---- d9_6 (0001 D9): worktree creation pins the exact base SHA -----------

#[test]
fn d9_6_worktree_pins_the_exact_base_sha() {
    let dir = tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("a.txt"), "one\n").expect("write a.txt");
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let first_sha = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    std::fs::write(repo.join("a.txt"), "two\n").expect("write a.txt again");
    git(&repo, &["commit", "-aq", "-m", "second"]);

    let worktree_path = dir.path().join("worktree");
    let head = wirk_herdr::git::worktree_add(&repo, &worktree_path, "p1/base-pin", &first_sha)
        .expect("worktree_add");
    assert_eq!(
        head, first_sha,
        "the new worktree's HEAD must equal the pinned base_sha exactly"
    );

    // An empty base_sha is refused before git is ever spawned (issue 285).
    let refused = wirk_herdr::git::worktree_add(&repo, &dir.path().join("w2"), "p1/empty", "  ");
    assert!(matches!(
        refused,
        Err(wirk_herdr::git::GitError::EmptyBaseSha)
    ));

    wirk_herdr::git::worktree_remove(&repo, &worktree_path).expect("worktree_remove");
    let branches = git(&repo, &["branch", "--list", "p1/base-pin"]);
    assert!(
        branches.contains("p1/base-pin"),
        "the branch must survive worktree remove (0017 D54): {branches:?}"
    );
}

fn git_init_repo(dir: &std::path::Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "one\n").expect("write a.txt");
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "first"]);
}

fn git(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
