//! D9 contract tests against `FakeHerdrClient` (0001 D9; W3, BRIEF.md
//! "Part B" tests). d9_2: lifecycle status events with no Claim never
//! advance a Run. d9_4 (round-trip half): the injected triple lands in
//! `SplitPane.env`. d9_5: rebind tracks a moved pane by `terminal_id`;
//! a vanished pane maps `poll` to `Vanished`, and `Run::apply(RunVanished)`
//! never yields `Claimed`. Plus replay dedup on `Reconciler::admit`. No
//! sleeps anywhere (issue 359): every event here is a fixed, already-
//! computed value, nothing waited on.

use std::collections::BTreeMap;

use wirk_core::{
    Event, EventId, EventKind, Executor, Run, RunId, RunState, Timestamp, WaypointId, WorldHash,
};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{
    AgentStatus, Bearing, HerdrClient, HerdrError, HerdrEvent, HerdrExecutor, PaneBinding,
    PaneInfo, Reconciler, Snapshot,
};

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
    }
}

fn event(id: &str, run_id: Option<&str>, kind: EventKind) -> Event {
    Event {
        id: EventId(id.to_string()),
        work: wirk_core::WorkId("work-1".to_string()),
        run: run_id.map(|r| RunId(r.to_string())),
        at: Timestamp(0),
        kind,
    }
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

fn status_changed(pane_id: &str, agent_status: AgentStatus) -> HerdrEvent {
    HerdrEvent::PaneAgentStatusChanged {
        pane_id: pane_id.to_string(),
        workspace_id: "w1".to_string(),
        agent: Some("claude".to_string()),
        agent_status,
        display_agent: None,
        state_labels: None,
        title: None,
    }
}

fn actor_world(run: &Run) -> wirk_core::World {
    wirk_core::World::Actor(wirk_core::ActorWorld {
        repository: "wirk".to_string(),
        worktree_path: "/var/tmp/w1".into(),
        branch: "p1/executor-design".to_string(),
        base_sha: "abc123".to_string(),
        triple: wirk_core::ExecutionTriple {
            estate_root: "/estate".to_string(),
            work_id: wirk_core::WorkId("work-1".to_string()),
            run_id: run.id.clone(),
        },
        intent: "do the thing".to_string(),
        output_contract: wirk_core::OutputContract(vec![]),
        boundary: wirk_core::Boundary(vec!["src/**".to_string()]),
    })
}

/// D9#2 ("Lifecycle events never advance a Waypoint; only a validated
/// Claim does"), driven against a Herdr-shaped fake: `subscribe` yields
/// idle, working, done status events and no Claim; each is folded into
/// a `LifecycleObserved` core Event and applied to the Run. The Run
/// stays Open throughout, and `HerdrExecutor::poll` (a blocked/idle/
/// done pane is all still `Running`, D52) never itself reports
/// completion — only a validated Claim can (0017 D56).
#[test]
fn d9_2_status_events_with_no_claim_leave_run_open() {
    let pane_id = "p1";
    let mut run = open_run("run-1");

    let fake = FakeHerdrClient::default()
        .with_subscribe_events(vec![
            status_changed(pane_id, AgentStatus::Idle),
            status_changed(pane_id, AgentStatus::Working),
            status_changed(pane_id, AgentStatus::Done),
        ])
        .with_get_pane_response("run-1", Ok(pane_info(pane_id, AgentStatus::Done, 1)));
    let executor = HerdrExecutor::new(fake);

    let events = executor.client().subscribe(vec![]).expect("subscribe");
    for ev in events {
        let ev = ev.expect("fixed fake events never error");
        let status = match &ev {
            HerdrEvent::PaneAgentStatusChanged { agent_status, .. } => format!("{agent_status:?}"),
            other => panic!("unexpected event: {other:?}"),
        };
        run.apply(&event(
            "ev-lifecycle",
            Some("run-1"),
            EventKind::LifecycleObserved { status },
        ));
        assert!(matches!(run.state, RunState::Open));
    }
    assert!(matches!(run.state, RunState::Open));

    // poll: still Running, never a completion signal, even for a pane
    // Herdr reports as `done` — completion is only a validated Claim.
    let observation = executor.poll(&run).expect("poll");
    assert!(matches!(observation, wirk_core::RunObservation::Running));
}

/// D9#4 round-trip half ("The injected execution triple round-trips
/// ... through the launch path"): after `HerdrExecutor::launch`, the
/// fake's recorded `SplitPane.env` carries `WIRK_ESTATE_ROOT`,
/// `WIRK_WORK_ID`, `WIRK_RUN_ID` equal to the Run's own triple. The
/// "fabricated one is recorded, not honored" half needs no
/// `HerdrClient` (`wirk claim` reads the process env, not Herdr) and
/// is `wirk-core`'s own test (`orient/herdr.md` §3) — not duplicated
/// here.
#[test]
fn d9_4_launch_carries_the_runs_triple_in_split_pane_env() {
    let run = open_run("run-1");
    let world = actor_world(&run);

    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().split_pane_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "launch should call split_pane exactly once");
    let want: BTreeMap<String, String> = [
        ("WIRK_ESTATE_ROOT".to_string(), "/estate".to_string()),
        ("WIRK_WORK_ID".to_string(), "work-1".to_string()),
        ("WIRK_RUN_ID".to_string(), "run-1".to_string()),
    ]
    .into_iter()
    .collect();
    assert_eq!(calls[0].env, want);
}

/// `launch` refuses a `Deterministic` world (not this executor's kind,
/// 0022 D78) without touching the fake at all.
#[test]
fn launch_refuses_a_deterministic_world() {
    let run = open_run("run-1");
    let world = wirk_core::World::Deterministic(wirk_core::DeterministicWorld {
        command: vec!["cargo".to_string(), "test".to_string()],
        cwd: "/var/tmp/w1".into(),
        env: Default::default(),
        expected_artifacts: wirk_core::OutputContract(vec![]),
    });
    let executor = HerdrExecutor::new(FakeHerdrClient::default());
    let err = executor
        .launch(&run, &world)
        .expect_err("Deterministic must be refused");
    assert!(matches!(
        err,
        wirk_herdr::HerdrExecutorError::NotDeterministicKind
    ));
    assert!(
        executor
            .client()
            .split_pane_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}

/// D9#5 ("A moved pane rebinds from `session.snapshot`; a vanished pane
/// ends unresolved, not complete"). Snapshot 1 binds terminal `t1` to
/// pane `p1`; snapshot 2 has `t1` under `p2` — `rebind` updates the
/// binding's `pane_id` while `terminal_id` stays unchanged; snapshot 3
/// lacks `t1` entirely — `rebind` returns it vanished, `poll` on the
/// tied Run maps to `Vanished`, and `Run::apply(RunVanished)` yields
/// `Vanished`, never `Claimed`.
#[test]
fn d9_5_rebind_tracks_a_moved_pane_and_flags_a_vanished_one() {
    let mut reconciler = Reconciler::new();
    let initial_bearing = Bearing {
        workspace_id: "w1".to_string(),
        tab_id: "tab1".to_string(),
        pane_id: "p1".to_string(),
        terminal_id: "t1".to_string(),
    };
    reconciler.bind(PaneBinding {
        terminal_id: "t1".to_string(),
        bearing: initial_bearing.clone(),
    });

    // Snapshot 1: t1 still under p1 — a no-op rebind, nothing vanished.
    let snap1 = Snapshot {
        workspaces: vec![initial_bearing.clone()],
    };
    assert!(reconciler.rebind(&snap1).is_empty());
    assert_eq!(reconciler.binding("t1").unwrap().bearing.pane_id, "p1");

    // Snapshot 2: t1 moved to p2 — pane_id updates, terminal_id does not.
    let moved_bearing = Bearing {
        workspace_id: "w1".to_string(),
        tab_id: "tab1".to_string(),
        pane_id: "p2".to_string(),
        terminal_id: "t1".to_string(),
    };
    let snap2 = Snapshot {
        workspaces: vec![moved_bearing],
    };
    assert!(reconciler.rebind(&snap2).is_empty());
    let binding = reconciler
        .binding("t1")
        .expect("t1 still bound after a move");
    assert_eq!(binding.bearing.pane_id, "p2");
    assert_eq!(binding.terminal_id, "t1");

    // Snapshot 3: t1 is gone entirely.
    let snap3 = Snapshot { workspaces: vec![] };
    let vanished = reconciler.rebind(&snap3);
    assert_eq!(vanished, vec!["t1".to_string()]);

    // poll on the tied Run maps to Vanished: the fake's get_pane has no
    // response configured for run-1, so it answers NotFound.
    let run = open_run("run-1");
    let executor = HerdrExecutor::new(FakeHerdrClient::default());
    let observation = executor.poll(&run).expect("poll");
    assert!(matches!(observation, wirk_core::RunObservation::Vanished));

    // Run::apply(RunVanished) yields Vanished, never Claimed.
    let mut run = run;
    run.apply(&event("ev-vanished", Some("run-1"), EventKind::RunVanished));
    assert!(matches!(run.state, RunState::Vanished));
    assert!(!matches!(run.state, RunState::Claimed(_)));
}

/// `HerdrExecutor::poll` also maps an explicit `NotFound` from
/// `get_pane` to `Vanished`, independent of `Reconciler` — the same
/// mapping `d9_5` exercises via a default (unconfigured) fake.
#[test]
fn poll_maps_not_found_to_vanished() {
    let run = open_run("run-1");
    let fake = FakeHerdrClient::default()
        .with_get_pane_response("run-1", Err(HerdrError::NotFound("run-1".to_string())));
    let executor = HerdrExecutor::new(fake);
    let observation = executor.poll(&run).expect("poll");
    assert!(matches!(observation, wirk_core::RunObservation::Vanished));
}

/// Replay: admitting the same event twice refuses the second (dedup by
/// `event_identity`, 0017 D51).
#[test]
fn replay_of_the_same_event_is_refused_the_second_time() {
    let mut reconciler = Reconciler::new();
    let ev = HerdrEvent::PaneClosed {
        pane_id: "p1".to_string(),
        workspace_id: "w1".to_string(),
    };
    assert!(reconciler.admit(&ev), "first admission should succeed");
    assert!(
        !reconciler.admit(&ev),
        "replay of the same event must be refused"
    );
}

/// Two `PaneUpdated` events with different `revision` are both
/// admitted: `pane_created`/`pane_updated` identity is
/// `(type, pane_id, revision)`, so a genuine revision bump is never
/// mistaken for a replay.
#[test]
fn two_pane_updated_events_with_different_revision_are_both_admitted() {
    let mut reconciler = Reconciler::new();
    let first = HerdrEvent::PaneUpdated {
        pane: pane_info("p1", AgentStatus::Idle, 1),
    };
    let second = HerdrEvent::PaneUpdated {
        pane: pane_info("p1", AgentStatus::Idle, 2),
    };
    assert!(reconciler.admit(&first));
    assert!(reconciler.admit(&second));
}
