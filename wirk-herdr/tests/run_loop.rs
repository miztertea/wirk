//! `RunLoop` tests (item 4, W2; `orient/loop.md` §4, `orient/build-
//! brief.md` §3 W2). Every fake-backed test uses `ManualClock` — no
//! sleep anywhere (issue 359). `d9_6` is the real half `wirk-core`
//! cannot run itself (0001 D7's crate boundary): real `git` in a
//! tempdir, via `wirk_herdr::git`.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;

use wirk_core::{
    ActorWorld, Boundary, ClaimId, EventKind, ExecutionTriple, OutputContract, Run, RunId,
    RunState, WaypointId, WorkId, World, WorldHash,
};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::run_loop::{
    FakeWirkdApi, ManualClock, Outcome, RunLoop, RunStatusEntry, WorkStatus,
};
use wirk_herdr::{AgentStatus, HerdrEvent, PaneInfo};

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
    }
}

fn work_id() -> WorkId {
    WorkId("work-1".to_string())
}

fn actor_world(run: &Run) -> wirk_core::World {
    wirk_core::World::Actor(ActorWorld {
        repository: "wirk".to_string(),
        worktree_path: "/var/tmp/w1".into(),
        branch: "p1/herdr-executor".to_string(),
        base_sha: "abc123".to_string(),
        triple: ExecutionTriple {
            estate_root: "/estate".to_string(),
            work_id: work_id(),
            run_id: run.id.clone(),
        },
        intent: "do the thing".to_string(),
        output_contract: OutputContract(vec![]),
        boundary: Boundary(vec!["src/**".to_string()]),
    })
}

fn pane_info(pane_id: &str, agent_status: AgentStatus) -> PaneInfo {
    PaneInfo {
        pane_id: pane_id.to_string(),
        terminal_id: format!("term-{pane_id}"),
        workspace_id: "w1".to_string(),
        tab_id: "tab1".to_string(),
        focused: false,
        agent_status,
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

fn status_changed(run: &Run, agent_status: AgentStatus) -> HerdrEvent {
    HerdrEvent::PaneAgentStatusChanged {
        pane_id: run.id.0.clone(),
        workspace_id: "w1".to_string(),
        agent: Some("claude".to_string()),
        agent_status,
        display_agent: None,
        state_labels: None,
        title: None,
    }
}

fn pane_updated(run: &Run, revision: u64) -> HerdrEvent {
    HerdrEvent::PaneUpdated {
        pane: PaneInfo {
            revision,
            ..pane_info(&run.id.0, AgentStatus::Working)
        },
    }
}

fn no_runs_claimed_status() -> WorkStatus {
    WorkStatus {
        work_state: wirk_core::WorkState::Active,
        runs: Vec::new(),
    }
}

/// Blocked never prompts (0017 D52): a `Blocked` status observed sets
/// the loop's blocked flag, sends no prompt, and a `maybe_nudge` call
/// past the bound still refuses while blocked.
#[test]
fn blocked_never_prompts() {
    let run = open_run("run-1");
    let world = actor_world(&run);
    let World::Actor(actor) = &world else {
        unreachable!()
    };

    let client = FakeHerdrClient::default();
    let wirkd = Arc::new(FakeWirkdApi::default().with_status(no_runs_claimed_status()));
    let clock = Arc::new(ManualClock::new());
    let mut loop_ = RunLoop::new(client, wirkd, clock, Duration::from_secs(10));

    loop_
        .observe(
            &work_id(),
            &run,
            actor,
            &status_changed(&run, AgentStatus::Blocked),
        )
        .expect("observe");
    assert!(loop_.is_blocked());
    assert!(!loop_.first_prompt_sent());
    assert!(!loop_.prompt_gate_busy());

    // Even well past the nudge bound, a blocked pane is never nudged.
    let fired = loop_.maybe_nudge(&run).expect("maybe_nudge");
    assert!(!fired, "a blocked pane must never be nudged");
    assert!(!loop_.nudge_sent());
}

/// The nudge fires exactly once after `nudge_after` with no activity,
/// never a second time (0001 D3; issue 274).
#[test]
fn nudge_fires_once_after_the_bound_with_no_activity() {
    let run = open_run("run-1");
    let world = actor_world(&run);

    let client =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle));
    let wirkd = Arc::new(FakeWirkdApi::default().with_status(no_runs_claimed_status()));
    let clock = Arc::new(ManualClock::new());
    let mut loop_ = RunLoop::new(client, wirkd, clock.clone(), Duration::from_secs(10));

    // The Run's one subscription (fix 3) is not drained here: this test
    // drives the nudge policy directly.
    drop(loop_.launch(&work_id(), &run, &world).expect("launch"));
    assert!(!loop_.nudge_sent());

    clock.advance(Duration::from_secs(10));
    let fired = loop_.maybe_nudge(&run).expect("maybe_nudge");
    assert!(fired, "the nudge should fire once the bound has elapsed");
    assert!(loop_.nudge_sent());

    // Advancing further and calling again never nudges a second time.
    clock.advance(Duration::from_secs(30));
    let fired_again = loop_.maybe_nudge(&run).expect("maybe_nudge");
    assert!(!fired_again, "never a second nudge");
}

/// Activity (a `PaneUpdated` revision bump) resets the inactivity
/// clock, suppressing a nudge that would otherwise have fired.
#[test]
fn activity_resets_the_nudge_clock() {
    let run = open_run("run-1");
    let world = actor_world(&run);
    let World::Actor(actor) = &world else {
        unreachable!()
    };

    let client =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle));
    let wirkd = Arc::new(FakeWirkdApi::default().with_status(no_runs_claimed_status()));
    let clock = Arc::new(ManualClock::new());
    let mut loop_ = RunLoop::new(client, wirkd, clock.clone(), Duration::from_secs(10));

    drop(loop_.launch(&work_id(), &run, &world).expect("launch")); // t=0

    clock.advance(Duration::from_secs(6)); // t=6
    loop_
        .observe(&work_id(), &run, actor, &pane_updated(&run, 2))
        .expect("observe"); // resets last_activity to t=6

    clock.advance(Duration::from_secs(6)); // t=12: 12-6=6 < 10
    assert!(
        !loop_.maybe_nudge(&run).expect("maybe_nudge"),
        "activity at t=6 should suppress a nudge due at t=10 from t=0"
    );

    clock.advance(Duration::from_secs(5)); // t=17: 17-6=11 >= 10
    assert!(
        loop_.maybe_nudge(&run).expect("maybe_nudge"),
        "the nudge should fire once 10s have elapsed since the reset activity"
    );
}

/// A vanished pane (0017 D53) is journaled `RunVanished` by the loop —
/// `poll` itself never journals (D56) — and never reaches `Claimed`
/// through `poll_claimed`.
#[test]
fn vanished_pane_journals_run_vanished_and_never_claimed() {
    let run = open_run("run-1");
    // No get_pane response configured: the fake answers NotFound,
    // matching `poll_maps_not_found_to_vanished` in tests/contracts.rs.
    let client = FakeHerdrClient::default();
    let wirkd = Arc::new(FakeWirkdApi::default().with_status(WorkStatus {
        work_state: wirk_core::WorkState::Active,
        runs: vec![RunStatusEntry {
            run_id: run.id.clone(),
            state: RunState::Vanished,
        }],
    }));
    let clock = ManualClock::new();
    let mut loop_ = RunLoop::new(client, wirkd.clone(), clock, Duration::from_secs(120));

    let vanished = loop_
        .poll_vanished(&work_id(), &run)
        .expect("poll_vanished");
    assert!(vanished);

    let recorded = wirkd.recorded();
    assert!(
        recorded
            .iter()
            .any(|(_, run_id, kind)| run_id == &run.id && matches!(kind, EventKind::RunVanished)),
        "RunVanished must be journaled: {recorded:?}"
    );

    let outcome = loop_
        .poll_claimed(&work_id(), &run.id)
        .expect("poll_claimed");
    assert_ne!(
        outcome,
        Some(Outcome::Claimed),
        "a vanished run is never Claimed"
    );
}

/// A launch failure (no `split_pane_response` configured: the fake
/// refuses) is journaled `RunFailed{cause.detail}` with the error's own
/// message (issue 275's shape).
#[test]
fn launch_error_journals_run_failed_with_detail() {
    let run = open_run("run-1");
    let world = actor_world(&run);

    let client = FakeHerdrClient::default(); // nothing configured: split_pane refuses
    let wirkd = Arc::new(FakeWirkdApi::default());
    let clock = ManualClock::new();
    let mut loop_ = RunLoop::new(client, wirkd.clone(), clock, Duration::from_secs(120));

    // `launch` now returns the Run's one live subscription on success
    // (fix 3), and `Box<dyn Iterator>` is not `Debug`, so the error is
    // taken by `match` rather than `expect_err`.
    let Err(err) = loop_.launch(&work_id(), &run, &world) else {
        panic!("an unconfigured fake must refuse split_pane");
    };
    assert!(matches!(err, wirk_herdr::run_loop::RunLoopError::Herdr(_)));

    let recorded = wirkd.recorded();
    let failed = recorded.iter().find_map(|(_, run_id, kind)| {
        if run_id != &run.id {
            return None;
        }
        match kind {
            EventKind::RunFailed { cause } => Some(cause.clone()),
            _ => None,
        }
    });
    let cause = failed.expect("RunFailed must be journaled on a launch error");
    assert!(
        cause.detail.as_deref().is_some_and(|d| !d.is_empty()),
        "RunFailed.cause.detail must carry the error text"
    );
}

/// `poll_claimed` learns `Claimed` from `WirkdApi::status` and reports
/// it — the signal `drive` (and a real `wirk` bin loop) stops on.
#[test]
fn claimed_learned_from_wirkd_status_stops_the_loop() {
    let run = open_run("run-1");
    let world = actor_world(&run);

    let client = FakeHerdrClient::default()
        .with_split_pane_response(pane_info("p1", AgentStatus::Idle))
        .with_subscribe_events(vec![status_changed(&run, AgentStatus::Working)]);
    let wirkd = FakeWirkdApi::default().with_status(WorkStatus {
        work_state: wirk_core::WorkState::Completed,
        runs: vec![RunStatusEntry {
            run_id: run.id.clone(),
            state: RunState::Claimed(ClaimId("claim-1".to_string())),
        }],
    });
    let clock = ManualClock::new();
    let mut loop_ = RunLoop::new(client, wirkd, clock, Duration::from_secs(120));

    let outcome = loop_.drive(&work_id(), &run, &world).expect("drive");
    assert_eq!(outcome, Outcome::Claimed);
}

/// A failure that escapes `drive` **after** `RunLaunched` is journaled
/// `RunFailed{cause.detail}` before it returns (fix 3, 0028 tried step
/// 3's second finding: the crash there left the Run stuck at
/// `RunLaunched` with nothing journaled at all). Driven with a
/// `FakeWirkdApi` that has no `status` configured, so `poll_claimed`
/// — reached only after `launch` succeeded and the first event was
/// observed — fails, while `record` keeps working.
#[test]
fn a_failure_after_launch_journals_run_failed_with_detail() {
    let run = open_run("run-1");
    let world = actor_world(&run);

    let client = FakeHerdrClient::default()
        .with_split_pane_response(pane_info("p1", AgentStatus::Idle))
        .with_subscribe_events(vec![status_changed(&run, AgentStatus::Working)]);
    let wirkd = Arc::new(FakeWirkdApi::default()); // no status: poll_claimed fails
    let clock = ManualClock::new();
    let mut loop_ = RunLoop::new(client, wirkd.clone(), clock, Duration::from_secs(120));

    let Err(err) = loop_.drive(&work_id(), &run, &world) else {
        panic!("poll_claimed must fail with no status configured");
    };
    assert!(matches!(err, wirk_herdr::run_loop::RunLoopError::Wirkd(_)));

    let recorded = wirkd.recorded();
    let kinds: Vec<&EventKind> = recorded.iter().map(|(_, _, kind)| kind).collect();
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, EventKind::RunLaunched { .. })),
        "the Run was launched: {kinds:?}"
    );
    let cause = kinds
        .iter()
        .find_map(|kind| match kind {
            EventKind::RunFailed { cause } => Some(cause.clone()),
            _ => None,
        })
        .expect("RunFailed must be journaled for a post-launch failure");
    assert!(
        cause.detail.as_deref().is_some_and(|d| d.contains("wirkd")),
        "RunFailed.cause.detail must carry the escaping error's text: {cause:?}"
    );
}

/// D9#6 (0001 D9): "Worktree creation pins the exact base SHA; branch
/// retained after retirement." The real half `wirk-core` cannot run
/// itself (0001 D7's crate boundary keeps it socket/git-free) — real
/// `git` in a tempdir: init a repo, two commits, `worktree_add` at the
/// first commit's own SHA, assert the new worktree's `HEAD` equals it
/// exactly, `worktree_remove`, assert the branch survives (0017 D54).
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
