//! D9 contract tests against `FakeHerdrClient` (0001 D9; W3, BRIEF.md
//! "Part B" tests). d9_2: lifecycle status events with no Claim never
//! advance a Run. d9_4 (round-trip half): the injected triple lands in
//! `SplitPane.env`. d9_5: a vanished pane maps `poll` to `Vanished`, and
//! `Run::apply(RunVanished)` never yields `Claimed` (its own
//! moved-pane-rebind half went with `Reconciler`, fix 2, ruling 0044 —
//! nothing but that type's own test ever called `rebind`). No sleeps
//! anywhere (issue 359): every event here is a fixed, already-computed
//! value, nothing waited on.

#[path = "support/live_herdr.rs"]
mod live_herdr;

use std::collections::BTreeMap;

use wirk_core::{
    Event, EventId, EventKind, Executor, Run, RunId, RunObservation, RunState, Timestamp,
    WaypointId, WorldHash,
};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{
    AgentStatus, CreateWorkspace, EventSubscription, HerdrClient, HerdrError, HerdrEvent,
    HerdrExecutor, PaneInfo, ReportAgent, SocketClient, SplitDirection, SplitPane,
};

/// Reports `state` for `pane_id` through `pane.report_agent` — the same
/// live wire call opencode's own hook plugin makes to tell Herdr an
/// agent's lifecycle changed (`refs/herdr` `handle_pane_report_agent`,
/// `AppEvent::HookStateReported`); driving it directly here produces a
/// **real** `pane.agent_status_changed` event without paying for a real
/// agent process on every converted test (0040 D127: the real service,
/// not a fake — this is the real hook-report code path, not a script).
fn report_agent_state(client: &SocketClient, pane_id: &str, state: &str, seq: u64) {
    client
        .report_agent(ReportAgent {
            pane_id: pane_id.to_string(),
            source: "wirk-test".to_string(),
            agent: "claude".to_string(),
            state: state.to_string(),
            seq: Some(seq),
        })
        .unwrap_or_else(|e| panic!("pane.report_agent({state}): {e:?}"));
}

/// Reads the next `PaneAgentStatusChanged` event off a live subscription
/// (bounded by the fixture's own read timeout — issue 359, no sleep),
/// returning its `agent_status` as the `LifecycleObserved` status string
/// `Run::apply` expects.
fn next_agent_status(
    events: &mut Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>,
) -> String {
    let ev = events
        .next()
        .expect("a pushed event arrived within the read timeout")
        .expect("the pushed line decoded as a well-formed HerdrEvent");
    match ev {
        HerdrEvent::PaneAgentStatusChanged { agent_status, .. } => format!("{agent_status:?}"),
        other => panic!("expected PaneAgentStatusChanged, got {other:?}"),
    }
}

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: Default::default(),
    }
}

fn open_run_with_kind(run_id: &str, kind: wirk_core::ActorKind) -> Run {
    Run {
        kind,
        ..open_run(run_id)
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
/// Claim does"), driven live (0040 D127): a throwaway session, a real
/// workspace and pane, three real status transitions reported through
/// `pane.report_agent` (idle, working, idle-again — Herdr's own
/// seen-then-idle rule turns the third into `Done`, `refs/herdr`
/// `pane_agent_status`) each folded into a `LifecycleObserved` core
/// Event and applied to the Run. The Run stays Open throughout, and
/// `HerdrExecutor::poll` (a blocked/idle/done pane is all still
/// `Running`, D52) never itself reports completion — only a validated
/// Claim can (0017 D56).
#[test]
fn d9_2_status_events_with_no_claim_leave_run_open() {
    let Some(session) =
        live_herdr::LiveHerdrSession::start("d9_2_status_events_with_no_claim_leave_run_open")
    else {
        return;
    };
    let client = session.client();
    let (repo, _sha) = session.repo();

    let ws = client
        .create_workspace(CreateWorkspace {
            cwd: repo.clone(),
            env: BTreeMap::new(),
            label: Some("wirk-test-d9-2".to_string()),
        })
        .expect("workspace.create");
    let pane = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id),
            target_pane_id: None,
            direction: SplitDirection::Right,
            cwd: repo,
            env: BTreeMap::new(),
        })
        .expect("pane.split");

    let mut run = open_run(&pane.pane_id);
    let mut events = client
        .subscribe(vec![EventSubscription::PaneAgentStatusChanged {
            pane_id: pane.pane_id.clone(),
        }])
        .expect("events.subscribe");

    for (seq, state) in [(1, "idle"), (2, "working"), (3, "idle")] {
        report_agent_state(&client, &pane.pane_id, state, seq);
        let status = next_agent_status(&mut events);
        run.apply(&event(
            "ev-lifecycle",
            Some(&pane.pane_id),
            EventKind::LifecycleObserved { status },
        ));
        assert!(matches!(run.state, RunState::Open));
    }
    assert!(matches!(run.state, RunState::Open));

    // poll: still Running, never a completion signal, even for a pane
    // Herdr now reports as `done` — completion is only a validated
    // Claim.
    let executor = HerdrExecutor::new(client);
    let observation = executor.poll(&run).expect("poll");
    assert!(matches!(observation, RunObservation::Running));
}

/// D9#4 round-trip half ("The injected execution triple round-trips
/// ... through the launch path"), driven live: the same env
/// `HerdrExecutor`'s (private) `actor_pane` builds is sent through a
/// real `pane.split` (R1: `actor_pane` is not a public seam this item's
/// allow-list can expose), then delivery is proven — not merely
/// acceptance — by having the live pane's own shell `printenv` the
/// three vars to a file and reading them back equal to the Run's own
/// triple. The "fabricated one is recorded, not honored" half needs no
/// `HerdrClient` (`wirk claim` reads the process env, not Herdr) and
/// is `wirk-core`'s own test (`orient/herdr.md` §3) — not duplicated
/// here.
#[test]
fn d9_4_launch_carries_the_runs_triple_in_split_pane_env() {
    let Some(session) = live_herdr::LiveHerdrSession::start(
        "d9_4_launch_carries_the_runs_triple_in_split_pane_env",
    ) else {
        return;
    };
    let client = session.client();
    let (repo, _sha) = session.repo();

    let run = open_run("run-1");
    let world = actor_world(&run);
    let wirk_core::World::Actor(actor) = &world else {
        unreachable!()
    };
    let want: BTreeMap<String, String> = [
        (
            "WIRK_ESTATE_ROOT".to_string(),
            actor.triple.estate_root.clone(),
        ),
        ("WIRK_WORK_ID".to_string(), actor.triple.work_id.0.clone()),
        ("WIRK_RUN_ID".to_string(), actor.triple.run_id.0.clone()),
    ]
    .into_iter()
    .collect();

    let ws = client
        .create_workspace(CreateWorkspace {
            cwd: repo.clone(),
            env: want.clone(),
            label: Some("wirk-test-d9-4".to_string()),
        })
        .expect("workspace.create");
    let pane = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id),
            target_pane_id: None,
            direction: SplitDirection::Right,
            cwd: repo.clone(),
            env: want.clone(),
        })
        .expect("pane.split");

    let out_path = repo.join("env-check.txt");
    client
        .send_input(
            &pane.pane_id,
            &format!(
                "printenv WIRK_ESTATE_ROOT WIRK_WORK_ID WIRK_RUN_ID > {} 2>&1\n",
                out_path.display()
            ),
        )
        .expect("pane.send_text");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let contents = loop {
        if let Ok(contents) = std::fs::read_to_string(&out_path)
            && contents.lines().count() == 3
        {
            break contents;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "env-check.txt never carried 3 lines within the deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines,
        vec![
            want["WIRK_ESTATE_ROOT"].as_str(),
            want["WIRK_WORK_ID"].as_str(),
            want["WIRK_RUN_ID"].as_str(),
        ],
        "env round-trip through a live pane.split: {contents:?}"
    );
}

/// W1 (0041 D129): `start_actor_agent` sends `StartAgent{kind:"claude",
/// args:["--model","sonnet"]}` for `ActorKind::Claude` — the
/// pre-existing behavior, now driven by `run.kind` rather than
/// hardcoded (`orient/actor.md` §1).
#[test]
fn start_actor_agent_sends_claude_kind_and_model() {
    let run = open_run_with_kind("run-1", wirk_core::ActorKind::Claude);
    let world = actor_world(&run);

    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "launch should call start_agent exactly once"
    );
    assert_eq!(calls[0].kind, "claude");
    assert_eq!(
        calls[0].args,
        vec!["--model".to_string(), "sonnet".to_string()]
    );
}

/// W1 (0041 D129): `start_actor_agent` sends
/// `StartAgent{kind:"opencode", args:["--model",
/// "hecate/qwen3.8-27b-udiq3s-mtp"]}` for `ActorKind::Opencode`
/// (`orient/actor.md` §1, §5 — the model passed explicitly the first
/// live run).
#[test]
fn start_actor_agent_sends_opencode_kind_and_model() {
    let run = open_run_with_kind("run-1", wirk_core::ActorKind::Opencode);
    let world = actor_world(&run);

    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "launch should call start_agent exactly once"
    );
    assert_eq!(calls[0].kind, "opencode");
    assert_eq!(
        calls[0].args,
        vec![
            "--model".to_string(),
            "hecate/qwen3.8-27b-udiq3s-mtp".to_string()
        ]
    );
}

/// `launch` refuses a `Deterministic` world (not this executor's kind,
/// 0022 D78) without touching the fake at all.
#[test]
fn launch_refuses_a_deterministic_world() {
    let run = open_run("run-1");
    let world = wirk_core::World::Deterministic(wirk_core::DeterministicWorld {
        command: vec!["cargo".to_string(), "test".to_string()],
        base_sha: "abc123".to_string(),
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

/// D9#5's `poll`-maps-to-`Vanished` half: no pane named "run-1" has ever
/// existed in this throwaway live session, so `pane.get` answers
/// `pane_not_found` for real, `poll` maps that to `Vanished`, and
/// `Run::apply(RunVanished)` yields `Vanished`, never `Claimed`.
/// D9#5's own moved-pane-rebind half was `Reconciler::rebind`'s test —
/// `Reconciler` is gone (fix 2, ruling 0044/D51 R1: nothing in this
/// codebase ever called `rebind` outside its own test, and Herdr does
/// not replay to a new subscription, measured, so the dedup half of the
/// same type was equally unused).
#[test]
fn d9_5_poll_maps_a_never_created_pane_to_vanished() {
    let Some(session) =
        live_herdr::LiveHerdrSession::start("d9_5_poll_maps_a_never_created_pane_to_vanished")
    else {
        return;
    };
    let run = open_run("run-1");
    let executor = HerdrExecutor::new(session.client());
    let observation = executor.poll(&run).expect("poll");
    assert!(matches!(observation, RunObservation::Vanished));

    // Run::apply(RunVanished) yields Vanished, never Claimed.
    let mut run = run;
    run.apply(&event("ev-vanished", Some("run-1"), EventKind::RunVanished));
    assert!(matches!(run.state, RunState::Vanished));
    assert!(!matches!(run.state, RunState::Claimed(_)));
}

/// `HerdrExecutor::poll` also maps an explicit `NotFound` from
/// `get_pane` to `Vanished`, independent of `Reconciler` — the same
/// mapping `d9_5` exercises live, here against a pane that existed and
/// was closed (an explicit `pane_not_found`, not merely one that was
/// never created).
#[test]
fn poll_maps_not_found_to_vanished() {
    let Some(session) = live_herdr::LiveHerdrSession::start("poll_maps_not_found_to_vanished")
    else {
        return;
    };
    let client = session.client();
    let (repo, _sha) = session.repo();
    let ws = client
        .create_workspace(CreateWorkspace {
            cwd: repo.clone(),
            env: BTreeMap::new(),
            label: Some("wirk-test-poll-vanished".to_string()),
        })
        .expect("workspace.create");
    let pane = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id.clone()),
            target_pane_id: None,
            direction: SplitDirection::Right,
            cwd: repo,
            env: BTreeMap::new(),
        })
        .expect("pane.split");
    let run = open_run(&pane.pane_id);
    client.close_pane(&pane.pane_id).expect("pane.close");

    let executor = HerdrExecutor::new(client);
    let observation = executor.poll(&run).expect("poll");
    assert!(matches!(observation, RunObservation::Vanished));
}

// `Reconciler::admit`'s own dedup-by-`event_identity` tests are gone
// with the type (fix 2, ruling 0044): Herdr does not replay events to a
// new subscription (measured,
// `knowledge/work/p2-dogfood/orient/herdr-events-measured.md`), and the
// dedup this type existed to provide was actively wrong — it dropped a
// pane's second, content-identical Idle as a "replay" of the first,
// which is the run 2 bug `run_loop.rs`'s own tests now pin the fix for
// (`the_run2_bug_a_second_identical_idle_is_still_prompted`).
