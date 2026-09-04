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
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ClaimId, ClaimKind, ClaimVerdict, Event,
    EventId, EventKind, ExecutionTriple, OutputContract, RouteId, Run, RunId, RunState, Timestamp,
    WaypointId, WorkId, World, WorldHash,
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
    let world = actor_world(&run, dir.path());
    let (client, herdr_tx) = client_for(&run);
    let wirkd = Arc::new(FakeWirkdApi::default());
    let loop_ = RunLoop::new(client.clone(), wirkd.clone());

    let handle = spawn_drive(loop_, run.clone(), world);

    // Idle, Working, then a second Idle identical in content to the
    // first (fix 2's own bug: the old `Reconciler` hashed this as a
    // replay and dropped it).
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
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

// ---- (2) Working then Blocked across many events: zero prompts -----------

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
}

// ---- (6) no progress vs. progress -----------------------------------------

/// A prompt, then Idle again with the pane's own revision and the
/// worktree both unchanged: the actor is stuck — `NeedsInput`, and no
/// second prompt is ever sent.
#[test]
fn no_progress_since_the_last_prompt_stops_the_loop_needs_input() {
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

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Nothing about the pane or the worktree changes.
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
        1,
        "no second prompt once the actor is judged stuck"
    );
}

/// The same shape, but the pane's own revision changes between the
/// prompt and the next Idle: not stuck, prompted again.
#[test]
fn progress_since_the_last_prompt_prompts_again() {
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

    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("first prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 1
    });
    // Progress: the pane's own revision moved on (a real turn happened).
    client.get_pane_responses.lock().unwrap().insert(
        run.id.0.clone(),
        Ok(pane_info(&run.id.0, AgentStatus::Idle, 8)),
    );
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Working)))
        .unwrap();
    herdr_tx
        .send(Ok(status_changed(&run, AgentStatus::Idle)))
        .unwrap();
    wait_until("second prompt sent", || {
        client.prompt_agent_calls.lock().unwrap().len() == 2
    });

    wirkd.push_watch_event(watch_event(Some(&run.id), claim_recorded_done("c1")));
    let outcome = handle.join().unwrap().expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
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
