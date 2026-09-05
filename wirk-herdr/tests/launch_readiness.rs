//! P2.5 W2 (0050 D151; `orient/launch.md` §1-§2, amended by the build
//! brief §7.2): `HerdrExecutor::launch_actor` retries `agent.start` on
//! Herdr's `agent_pane_busy` refusal by blocking on the pane's own
//! already-open subscription for its next event, never a timer or a
//! count (0044 D134); and the actor pane's `env` carries `PATH` with
//! the running `wirk` binary's own directory prepended (0050 D151).
//!
//! (a)/(b)/(c) drive `launch_actor` on its own thread (it blocks) and
//! feed/close a real channel via `FakeHerdrClient::with_subscribe_channel`
//! — never a canned one-shot reply standing in for a blocking stream
//! (0040 D127) — polling `start_agent_calls`/`prompt_agent_calls`-style
//! counters with a test's own bounded `wait_until` (never the product's
//! own wait, never a `sleep` as a wait, 0044 D134).

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ExecutionTriple, OutputContract, Run, RunId,
    RunState, WaypointId, WorkId, World, WorldHash,
};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{AgentStatus, HerdrError, HerdrEvent, HerdrExecutor, PaneInfo};

fn run_id() -> Run {
    Run {
        id: RunId("run-1".to_string()),
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
        branch: "p2/w2-launch-readiness".to_string(),
        base_sha: "abc123".to_string(),
        triple: ExecutionTriple {
            estate_root: "/estate".to_string(),
            work_id: WorkId("work-1".to_string()),
            run_id: run.id.clone(),
        },
        intent: "write report.md".to_string(),
        output_contract: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["src/**".to_string()]),
    })
}

fn pane_info(pane_id: &str) -> PaneInfo {
    PaneInfo {
        pane_id: pane_id.to_string(),
        terminal_id: format!("term-{pane_id}"),
        workspace_id: "w1".to_string(),
        tab_id: "tab1".to_string(),
        focused: false,
        agent_status: AgentStatus::Idle,
        revision: 1,
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

fn pane_updated(pane_id: &str) -> HerdrEvent {
    HerdrEvent::PaneUpdated {
        pane: pane_info(pane_id),
    }
}

fn agent_pane_busy() -> HerdrError {
    HerdrError::Invalid("agent_pane_busy: pane not ready".to_string())
}

/// A test's own termination bound (never the product's — 0044 D134,
/// the owner's 2026-09-02 ruling §3): panics naming what was never
/// observed rather than hanging the suite.
fn wait_until(what: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "never observed: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---- (a) busy once, one event, then accepts --------------------------------

#[test]
fn launch_waits_on_one_pane_busy_refusal_then_succeeds() {
    let run = run_id();
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (tx, rx) = mpsc::channel();
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_response(pane_info(&run.id.0))
            .with_subscribe_channel(rx)
            .with_start_agent_responses(vec![Err(agent_pane_busy()), Ok(())]),
    );
    let executor = HerdrExecutor::new(client.clone());

    let handle = {
        let run = run.clone();
        std::thread::spawn(move || executor.launch_actor(&run, &world))
    };

    // The first attempt refuses busy; the launch must block on the
    // subscription rather than spin — confirmed here by requiring
    // exactly one attempt before the event that unblocks the second.
    wait_until("first start_agent attempt", || {
        client.start_agent_calls.lock().unwrap().len() == 1
    });
    tx.send(Ok(pane_updated(&run.id.0))).unwrap();

    let launched = handle
        .join()
        .unwrap()
        .expect("launch_actor succeeds after one wait");
    assert_eq!(launched.pane.pane_id, run.id.0);
    assert_eq!(
        client.start_agent_calls.lock().unwrap().len(),
        2,
        "one busy refusal, one retry that succeeds"
    );
}

// ---- (b) busy twice, two events, succeeds on the third attempt -------------

#[test]
fn launch_waits_on_two_pane_busy_refusals_then_succeeds() {
    let run = run_id();
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (tx, rx) = mpsc::channel();
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_response(pane_info(&run.id.0))
            .with_subscribe_channel(rx)
            .with_start_agent_responses(vec![
                Err(agent_pane_busy()),
                Err(agent_pane_busy()),
                Ok(()),
            ]),
    );
    let executor = HerdrExecutor::new(client.clone());

    let handle = {
        let run = run.clone();
        std::thread::spawn(move || executor.launch_actor(&run, &world))
    };

    wait_until("first start_agent attempt", || {
        client.start_agent_calls.lock().unwrap().len() == 1
    });
    tx.send(Ok(pane_updated(&run.id.0))).unwrap();
    wait_until("second start_agent attempt", || {
        client.start_agent_calls.lock().unwrap().len() == 2
    });
    tx.send(Ok(pane_updated(&run.id.0))).unwrap();

    let launched = handle
        .join()
        .unwrap()
        .expect("launch_actor succeeds after two waits");
    assert_eq!(launched.pane.pane_id, run.id.0);
    assert_eq!(client.start_agent_calls.lock().unwrap().len(), 3);
}

// ---- (c) no event ever arrives: the launch blocks, never spins -------------

#[test]
fn launch_blocks_on_pane_busy_with_no_event_then_fails_on_closed_subscription() {
    let run = run_id();
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (tx, rx) = mpsc::channel();
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_response(pane_info(&run.id.0))
            .with_subscribe_channel(rx)
            .with_start_agent_responses(vec![Err(agent_pane_busy())]),
    );
    let executor = HerdrExecutor::new(client.clone());

    let handle = {
        let run = run.clone();
        std::thread::spawn(move || executor.launch_actor(&run, &world))
    };

    wait_until("the one attempt made before the wait", || {
        client.start_agent_calls.lock().unwrap().len() == 1
    });
    // The thread must still be blocked, not spinning: a further pause
    // (a test's own bound, not a product wait) with the call count
    // unchanged is the closest a test can get to observing "never
    // returned and never spun" without a product-side signal for it.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        client.start_agent_calls.lock().unwrap().len(),
        1,
        "no further agent.start attempt without an event"
    );
    assert!(
        !handle.is_finished(),
        "launch_actor must still be blocked on the subscription"
    );

    // The test closes the channel; the launch fails naming the closed
    // stream rather than retrying forever.
    drop(tx);
    let err = handle
        .join()
        .unwrap()
        .expect_err("a closed subscription while waiting is an error, never a silent return");
    let message = err.to_string();
    assert!(
        message.contains("closed") && message.contains(&run.id.0),
        "error must name the closed subscription and its pane: {message}"
    );
    assert_eq!(
        client.start_agent_calls.lock().unwrap().len(),
        1,
        "still exactly one attempt: the closed stream is a failure, not a nudge to retry again"
    );
}

// ---- (d) the actor pane's PATH carries the executable's own directory -----

#[test]
fn actor_pane_env_path_begins_with_the_running_executable_directory() {
    let run = run_id();
    let dir = tempdir().expect("tempdir");
    let world = actor_world(&run, dir.path());
    let (_tx, rx) = mpsc::channel();
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_response(pane_info(&run.id.0))
            .with_subscribe_channel(rx),
    );
    let executor = HerdrExecutor::new(client.clone());

    executor
        .launch_actor(&run, &world)
        .expect("launch_actor succeeds with no busy refusal configured");

    let calls = client.split_pane_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let env = &calls[0].env;
    assert!(
        env.contains_key("PATH"),
        "actor pane env must carry PATH: {env:?}"
    );
    let exe_dir = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("exe has a parent dir")
        .to_path_buf();
    let path_value = env.get("PATH").unwrap();
    let first_entry = std::env::split_paths(path_value)
        .next()
        .expect("PATH has at least one entry");
    assert_eq!(
        first_entry, exe_dir,
        "PATH's first entry must be the running executable's own directory: {path_value:?}"
    );
    // The triple stays alongside it, plus `CARGO_TARGET_DIR` only when
    // this test process's own env carries one (P2.6 W3: `actor_pane`
    // passes it through exactly when set, same mechanism as `PATH`
    // above) — asserted by content, not a fixed count, since whether
    // `CARGO_TARGET_DIR` is set is this test run's own environment, not
    // this test's own concern.
    let expected_len = if std::env::var("CARGO_TARGET_DIR").is_ok() {
        5
    } else {
        4
    };
    assert_eq!(
        env.len(),
        expected_len,
        "PATH added to the existing triple, plus CARGO_TARGET_DIR only when the driver's own \
         env carries one: {env:?}"
    );
}
