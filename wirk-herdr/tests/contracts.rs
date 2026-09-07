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
    HerdrExecutor, PaneInfo, ReportAgent, SocketClient, SplitDirection, SplitPane, StartAgent,
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
        selection: Default::default(),
        launched: false,
        launch_requested: false,
        launch_argv: Vec::new(),
        launch_attempt: None,
    }
}

fn open_run_with_kind(run_id: &str, kind: wirk_core::ActorKind) -> Run {
    Run {
        kind,
        ..open_run(run_id)
    }
}

fn open_run_with_selection(
    run_id: &str,
    kind: wirk_core::ActorKind,
    selection: wirk_core::ActorSelection,
) -> Run {
    Run {
        kind,
        selection,
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
        source_basis: wirk_core::SourceBasis::Git {
            base: "abc123".to_string(),
        },
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
            EventKind::LifecycleObserved {
                status,
                detail: None,
            },
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

/// P3 native launch selection (PREPARATION-ADJUDICATION.md point 4:
/// "eliminate machine-specific model literals as product
/// restrictions"), superseding W1's 0041 D129 hardcoded
/// `["--model","sonnet"]`: a claude Run with no requested model/effort
/// launches with **no** model/effort flag at all — the harness's own
/// native default engages, never a wirk-invented exact model identity.
#[test]
fn claude_with_no_selection_launches_with_no_model_or_effort_flag() {
    let run = open_run_with_kind("run-1", wirk_core::ActorKind::claude());
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
        Vec::<String>::new(),
        "no selection requested: no --model, no --effort, no invented default"
    );
}

/// Same absence-of-forced-default, opencode side (previously hardcoded
/// to `hecate/qwen3.8-27b-udiq3s-mtp`, orient/actor.md §5).
#[test]
fn opencode_with_no_selection_launches_with_no_model_flag() {
    let run = open_run_with_kind("run-1", wirk_core::ActorKind::opencode());
    let world = actor_world(&run);

    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].kind, "opencode");
    assert_eq!(calls[0].args, Vec::<String>::new());
}

/// A requested model translates to claude's own real, installed,
/// interactive `--model <model>` flag (verified by hand against
/// `claude --help` on this box — PREPARATION-ADJUDICATION.md point 2).
#[test]
fn claude_with_requested_model_sends_the_real_model_flag() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind::claude(),
        wirk_core::ActorSelection {
            model: Some("opus".to_string()),
            effort: None,
            args: Vec::new(),
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(
        calls[0].args,
        vec!["--model".to_string(), "opus".to_string()]
    );
}

/// Claude also has a real, direct interactive `--effort <level>` flag
/// (`claude --help`, distinct from `opencode`'s total absence of one
/// below) — both requested together produce both real flags, model
/// first (matching the order `build_selection_args` builds them in).
#[test]
fn claude_with_requested_model_and_effort_sends_both_real_flags() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind::claude(),
        wirk_core::ActorSelection {
            model: Some("sonnet".to_string()),
            effort: Some("high".to_string()),
            args: Vec::new(),
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(
        calls[0].args,
        vec![
            "--model".to_string(),
            "sonnet".to_string(),
            "--effort".to_string(),
            "high".to_string(),
        ]
    );
}

/// Opencode's own real interactive `-m`/`--model` flag (`opencode
/// --help` on this box), verified distinct from claude's spelling only
/// by coincidence — both happen to be `--model`.
#[test]
fn opencode_with_requested_model_sends_the_real_model_flag() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind::opencode(),
        wirk_core::ActorSelection {
            model: Some("hecate/qwen3.8-27b-udiq3s-mtp".to_string()),
            effort: None,
            args: Vec::new(),
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(
        calls[0].args,
        vec![
            "--model".to_string(),
            "hecate/qwen3.8-27b-udiq3s-mtp".to_string()
        ]
    );
}

/// PREPARATION-ADJUDICATION.md point 2: "An explicitly requested
/// option must not silently degrade into a different execution ...
/// fail visibly before actor launch." `opencode --help`'s full option
/// list carries no effort/reasoning-effort control at all (verified by
/// hand on this box) — an explicit effort request for it is refused
/// before `agent.start` is ever called, not silently dropped.
#[test]
fn opencode_with_requested_effort_is_refused_before_launch() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind::opencode(),
        wirk_core::ActorSelection {
            model: None,
            effort: Some("high".to_string()),
            args: Vec::new(),
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    let err = executor
        .launch(&run, &world)
        .expect_err("opencode has no native effort control");
    assert!(
        matches!(
            err,
            wirk_herdr::HerdrExecutorError::Selection(
                wirk_herdr::SelectionError::UnsupportedEffort { .. }
            )
        ),
        "expected Selection(UnsupportedEffort), got {err:?}"
    );
    assert_eq!(
        executor.client().start_agent_calls.lock().unwrap().len(),
        0,
        "agent.start must never be called for a refused selection"
    );
}

/// Codex's own real interactive `-m`/`--model` flag, plus its real
/// **config** control for effort — `model_reasoning_effort`, set the
/// same way any other Codex config override is (`-c key=value`,
/// confirmed against this box's own installed `~/.codex/config.toml`),
/// never a codex-specific CLI flag wirk invents.
#[test]
fn codex_with_requested_model_and_effort_maps_to_its_real_controls() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind("codex".to_string()),
        wirk_core::ActorSelection {
            model: Some("gpt-6-astra".to_string()),
            effort: Some("high".to_string()),
            args: Vec::new(),
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(calls[0].kind, "codex");
    assert_eq!(
        calls[0].args,
        vec![
            "--model".to_string(),
            "gpt-6-astra".to_string(),
            "-c".to_string(),
            "model_reasoning_effort=high".to_string(),
        ]
    );
}

/// PREPARATION-ADJUDICATION.md point 2/5: a kind outside wirk's three
/// verified harnesses gets no invented flag syntax — an explicit model
/// request for it is refused rather than guessed, distinct from
/// `start_actor_agent_sends_an_unlisted_kind_bare` below (no
/// model/effort requested at all, which still launches bare — 0056
/// D164 unchanged).
#[test]
fn unmapped_kind_with_requested_model_is_refused_before_launch() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind("somekind".to_string()),
        wirk_core::ActorSelection {
            model: Some("whatever".to_string()),
            effort: None,
            args: Vec::new(),
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    let err = executor
        .launch(&run, &world)
        .expect_err("wirk has never inspected somekind's own CLI");
    assert!(
        matches!(
            err,
            wirk_herdr::HerdrExecutorError::Selection(
                wirk_herdr::SelectionError::UnmappedKind { .. }
            )
        ),
        "expected Selection(UnmappedKind), got {err:?}"
    );
    assert_eq!(executor.client().start_agent_calls.lock().unwrap().len(), 0);
}

/// Raw pass-through (`selection.args`) is preserved verbatim, exact
/// token boundaries kept, appended after whatever convenience mapping
/// ran — for any kind, including one wirk has never heard of (the
/// escape hatch `UnmappedKind`'s own error message points an author
/// at).
#[test]
fn raw_pass_through_args_preserve_exact_token_boundaries() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind("somekind".to_string()),
        wirk_core::ActorSelection {
            model: None,
            effort: None,
            args: vec![
                "--flag with spaces".to_string(),
                "--another=value".to_string(),
            ],
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(
        calls[0].args,
        vec![
            "--flag with spaces".to_string(),
            "--another=value".to_string(),
        ],
        "each element is one argv token, not split or rejoined on whitespace"
    );
}

/// 0056 D164: "per-kind launch defaults... are not extended" — a kind
/// with no row in `start_actor_agent`'s match (`wirk-herdr/src/lib.rs`)
/// launches bare: `StartAgent.kind` carries the string through
/// verbatim and `args` is empty, never a wirk-invented default guessed
/// for a kind wirk has never heard of.
#[test]
fn start_actor_agent_sends_an_unlisted_kind_bare() {
    let run = open_run_with_kind("run-1", wirk_core::ActorKind("codex".to_string()));
    let world = actor_world(&run);

    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);

    executor.launch(&run, &world).expect("launch");

    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].kind, "codex");
    assert_eq!(
        calls[0].args,
        Vec::<String>::new(),
        "an unlisted kind gets no per-kind launch args"
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
        source_basis: wirk_core::SourceBasis::OutputOnly {
            reference: "abc123".to_string(),
        },
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

/// Live decisive check (0056 D164, 0040): `agent.start{kind:"notakind"}`
/// against a real Herdr session, direct on `SocketClient` — the wirk
/// layer above no longer has a parser to reject this kind at all
/// (`parse_actor_kind` is now infallible, `wirk/src/executor.rs`), so
/// any error observed here can only be Herdr's own answer. Confirmed
/// live 2026-09-05 against a throwaway named session: Herdr's
/// `start_agent` (`refs/herdr` `src/app/agents.rs::start_agent`,
/// `AgentStartError::UnsupportedKind`) refuses before any process is
/// spawned — `crate::detect::parse_agent_label("notakind")` returns
/// `None` because Herdr's own closed `Agent` enum (`refs/herdr`
/// `src/detect/mod.rs`) has no such label either — encoded as the
/// business error `{"code":"unsupported_agent_kind","message":
/// "unsupported interactive agent kind notakind"}`, which
/// `SocketClient` maps to `HerdrError::Invalid` (D51's map, `lib.rs`:
/// not `pane_not_found`/`agent_not_found`/`workspace_not_found` so not
/// `NotFound`, not `agent_not_ready` so not `Blocked`). No `claude`,
/// `codex`, or `opencode` binary is ever named or spawned by this test
/// (standing line): "notakind" never resolves to a real agent kind on
/// either side, so `start_agent` fails at Herdr's own kind-parse step,
/// before any executable lookup.
#[test]
fn live_agent_start_with_an_unknown_kind_surfaces_herdrs_own_error_not_wirks() {
    let Some(session) = live_herdr::LiveHerdrSession::start(
        "live_agent_start_with_an_unknown_kind_surfaces_herdrs_own_error_not_wirks",
    ) else {
        return;
    };
    let client = session.client();
    let (repo, _sha) = session.repo();

    let ws = client
        .create_workspace(CreateWorkspace {
            cwd: repo.clone(),
            env: BTreeMap::new(),
            label: Some("wirk-test-unknown-kind".to_string()),
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

    let err = client
        .start_agent(StartAgent {
            pane_id: pane.pane_id.clone(),
            kind: "notakind".to_string(),
            name: "live-unknown-kind-run".to_string(),
            args: Vec::new(),
            timeout_ms: None,
        })
        .expect_err("Herdr refuses a kind its own detector does not name");

    match err {
        HerdrError::Invalid(msg) => {
            assert!(
                msg.contains("unsupported_agent_kind"),
                "expected Herdr's unsupported_agent_kind business error, got: {msg}"
            );
            assert!(
                msg.contains("notakind"),
                "Herdr's own message should name the refused kind: {msg}"
            );
        }
        other => panic!("expected HerdrError::Invalid(unsupported_agent_kind...), got {other:?}"),
    }
}

// `Reconciler::admit`'s own dedup-by-`event_identity` tests are gone
// with the type (fix 2, ruling 0044): Herdr does not replay events to a
// new subscription (measured,
// `knowledge/work/p2-dogfood/orient/herdr-events-measured.md`), and the
// dedup this type existed to provide was actively wrong — it dropped a
// pane's second, content-identical Idle as a "replay" of the first,
// which is the run 2 bug `run_loop.rs`'s own tests now pin the fix for
// (`the_run2_bug_a_second_identical_idle_is_still_prompted`).

// ---- P3 native launch selection, D2: raw arguments may not restate a
// structured field ---------------------------------------------------

/// The review's D2 counterexample, executed: a Route authoring
/// `model: "model-A"` **and** `args: ["--model","raw-B"]` submitted
/// **two** `--model` tokens while the recorded request and the printed
/// provenance both named only the first. Whichever token claude's own
/// parser honors, one of the two records is then false.
///
/// The rule chosen is the narrowest one that cannot lie: no implicit
/// precedence — the overlap is refused, before launch, naming both
/// sides. Raw args stay the escape hatch for everything the convenience
/// fields do not cover (`raw_pass_through_args_preserve_exact_token_boundaries`,
/// `raw_args_that_do_not_restate_a_structured_field_still_pass_through`);
/// they simply may not restate one that was also given.
#[test]
fn a_raw_model_flag_alongside_a_requested_model_is_refused_before_launch() {
    for raw in [
        vec!["--model".to_string(), "raw-B".to_string()],
        vec!["--model=raw-B".to_string()],
    ] {
        let run = open_run_with_selection(
            "run-1",
            wirk_core::ActorKind::claude(),
            wirk_core::ActorSelection {
                model: Some("model-A".to_string()),
                effort: None,
                args: raw.clone(),
            },
        );
        let world = actor_world(&run);
        let fake = FakeHerdrClient::default().with_split_pane_response(pane_info(
            "p1",
            AgentStatus::Idle,
            1,
        ));
        let executor = HerdrExecutor::new(fake);

        let err = executor.launch(&run, &world).expect_err(&format!(
            "two --model tokens must be refused, not submitted: {raw:?}"
        ));
        match &err {
            wirk_herdr::HerdrExecutorError::Selection(
                wirk_herdr::SelectionError::RawArgConflict {
                    field, raw: named, ..
                },
            ) => {
                assert_eq!(field, "model");
                assert!(
                    named.contains("raw-B"),
                    "the error names the offending raw token, got {named:?}"
                );
            }
            other => panic!("expected Selection(RawArgConflict), got {other:?}"),
        }
        assert_eq!(
            executor.client().start_agent_calls.lock().unwrap().len(),
            0,
            "agent.start must never be called for a refused selection"
        );
    }
}

/// Every verified harness, in the real spellings its own installed CLI
/// accepts: claude `--model`/`--effort` (no short alias exists),
/// opencode `-m`/`--model`, codex `-m`/`--model` and its
/// `-c model_reasoning_effort=` config override — separate-token and
/// `=`-joined forms alike.
#[test]
fn every_verified_harnesss_own_raw_spelling_of_a_structured_field_is_refused() {
    let cases: Vec<(&str, wirk_core::ActorSelection, &str, &str)> = vec![
        (
            "claude",
            wirk_core::ActorSelection {
                model: Some("model-A".to_string()),
                effort: None,
                args: vec!["--model".to_string(), "raw-B".to_string()],
            },
            "model",
            "--model raw-B",
        ),
        (
            "claude",
            wirk_core::ActorSelection {
                model: None,
                effort: Some("high".to_string()),
                args: vec!["--effort=low".to_string()],
            },
            "effort",
            "--effort=low",
        ),
        (
            "opencode",
            wirk_core::ActorSelection {
                model: Some("prov/m".to_string()),
                effort: None,
                args: vec!["-m".to_string(), "prov/other".to_string()],
            },
            "model",
            "-m prov/other",
        ),
        (
            "codex",
            wirk_core::ActorSelection {
                model: Some("m-1".to_string()),
                effort: None,
                args: vec!["-c".to_string(), "model=m-2".to_string()],
            },
            "model",
            "-c model=m-2",
        ),
        (
            "codex",
            wirk_core::ActorSelection {
                model: None,
                effort: Some("high".to_string()),
                args: vec!["-c".to_string(), "model_reasoning_effort=low".to_string()],
            },
            "effort",
            "-c model_reasoning_effort=low",
        ),
        (
            "codex",
            wirk_core::ActorSelection {
                model: None,
                effort: Some("high".to_string()),
                args: vec!["--config=model_reasoning_effort=low".to_string()],
            },
            "effort",
            "--config=model_reasoning_effort=low",
        ),
    ];
    for (kind, selection, field, raw) in cases {
        let err = wirk_herdr::validate_selection(kind, &selection)
            .expect_err(&format!("{kind}/{field} must be refused"));
        match err {
            wirk_herdr::SelectionError::RawArgConflict {
                kind: err_kind,
                field: err_field,
                raw: err_raw,
                ..
            } => {
                assert_eq!(err_kind, kind);
                assert_eq!(err_field, field);
                assert_eq!(err_raw, raw, "the error names the offending raw token");
            }
            other => panic!("expected RawArgConflict for {kind}/{field}, got {other:?}"),
        }
    }
}

/// The escape hatch is untouched: a raw flag the convenience fields do
/// not cover, and even a raw `--model` when **no** structured model was
/// requested, both pass through exactly as before. This is the control
/// for the rule above — it refuses overlap, not raw arguments.
#[test]
fn raw_args_that_do_not_restate_a_structured_field_still_pass_through() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind::claude(),
        wirk_core::ActorSelection {
            model: None,
            effort: None,
            args: vec!["--model".to_string(), "raw-only".to_string()],
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);
    executor.launch(&run, &world).expect("launch");
    let calls = executor.client().start_agent_calls.lock().unwrap();
    assert_eq!(calls[0].args[0], "--model");
    assert_eq!(calls[0].args[1], "raw-only");

    let unrelated = wirk_core::ActorSelection {
        model: Some("model-A".to_string()),
        effort: Some("high".to_string()),
        args: vec![
            "--fallback-model".to_string(),
            "other".to_string(),
            "--raw-tail".to_string(),
        ],
    };
    wirk_herdr::validate_selection("claude", &unrelated)
        .expect("--fallback-model is a different setting, not a restatement of --model");
}

/// D1's "validate before unnecessary execution-side effects": a request
/// that cannot be honored costs no pane, no agent and no journal entry.
#[test]
fn a_conflicting_selection_never_reaches_agent_start_or_creates_a_pane() {
    let run = open_run_with_selection(
        "run-1",
        wirk_core::ActorKind::claude(),
        wirk_core::ActorSelection {
            model: Some("model-A".to_string()),
            effort: None,
            args: vec!["--model".to_string(), "raw-B".to_string()],
        },
    );
    let world = actor_world(&run);
    let fake =
        FakeHerdrClient::default().with_split_pane_response(pane_info("p1", AgentStatus::Idle, 1));
    let executor = HerdrExecutor::new(fake);
    let err = executor.launch(&run, &world).expect_err("refused");
    assert!(
        matches!(
            err,
            wirk_herdr::HerdrExecutorError::Selection(
                wirk_herdr::SelectionError::RawArgConflict { .. }
            )
        ),
        "expected Selection(RawArgConflict), got {err:?}"
    );
    assert_eq!(
        executor.client().start_agent_calls.lock().unwrap().len(),
        0,
        "agent.start must never be called for a refused selection"
    );
}
