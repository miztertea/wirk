//! Ruling 0205, the converse half of the defect, isolated in its own
//! test binary: a value in the **driver's** environment is not in the
//! pane's, and must not steer the delivery decision.
//!
//! Measured against herdr 0.9.0 on 2026-09-12: a pane created with no
//! `env` of its own, against a server carrying no `OPENCODE_CONFIG*`,
//! receives neither variable however the calling process is set up. The
//! previous code read exactly that calling process, so a driver-set
//! value made it divert to the file slot, or — with both set — append
//! wirk's entries to **the driver's own configuration document** and
//! inject it into the pane. Neither layer was ever there.
//!
//! This file holds exactly one test because `std::env::set_var` is
//! process-wide and cargo runs one file's tests on threads of a single
//! process. A separate integration binary is the isolation (R3: no
//! crate adopted for it, and no serialising harness introduced).

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ExecutionTriple, Executor, OutputContract, Run,
    RunId, RunState, WaypointId, WorkId, World, WorldHash,
};
use wirk_herdr::claim_hook::{OPENCODE_CONFIG_CONTENT_ENV, OPENCODE_CONFIG_ENV};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{AgentStatus, HerdrExecutor, PaneInfo, SplitPane};

fn opencode_run() -> Run {
    Run {
        id: RunId("run-1".to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: ActorKind::opencode(),
        selection: Default::default(),
        launched: false,
        launch_requested: false,
        launch_argv: Vec::new(),
        launch_attempt: None,
        expansions: Vec::new(),
        contract_delivery: None,
        claim_hook: None,
    }
}

fn actor_world(run: &Run, estate_root: &std::path::Path, worktree_path: &std::path::Path) -> World {
    World::Actor(ActorWorld {
        doctrine: Vec::new(),
        repository: "wirk".to_string(),
        worktree_path: worktree_path.to_path_buf(),
        branch: "p4-worker-contract/pane-config".to_string(),
        base_sha: "abc123".to_string(),
        source_basis: wirk_core::SourceBasis::Git {
            base: "abc123".to_string(),
        },
        triple: ExecutionTriple {
            estate_root: estate_root.to_string_lossy().into_owned(),
            work_id: WorkId("work-1".to_string()),
            run_id: run.id.clone(),
        },
        intent: "write report.md".to_string(),
        output_contract: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["src/**".to_string()]),
        review_targets: Vec::new(),
        evidence: None,
        contract: None,
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
        name: None,
        label: None,
        scroll: None,
        state_labels: None,
        terminal_title: None,
        terminal_title_stripped: None,
        title: None,
        tokens: None,
    }
}

/// Launches an opencode Run against a fake whose panes carry `carried`,
/// and returns the two `split_pane` calls: the short-lived probe pane
/// first, the actor's own pane second.
fn launch_with_pane_env(
    estate: &std::path::Path,
    worktree: &std::path::Path,
    carried: BTreeMap<String, String>,
) -> (Vec<SplitPane>, Vec<String>) {
    let run = opencode_run();
    let world = actor_world(&run, estate, worktree);
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_responses(vec![pane_info("probe-pane"), pane_info(&run.id.0)])
            .with_split_pane_response(pane_info(&run.id.0))
            .with_pane_env(carried),
    );
    HerdrExecutor::new(client.clone())
        .launch(&run, &world)
        .expect("launch succeeds");
    let splits = client.split_pane_calls.lock().unwrap().clone();
    let closed = client.close_pane_calls.lock().unwrap().clone();
    (splits, closed)
}

/// **The converse direction.** A value in the *driver's* environment is
/// not in the pane's, so it must not steer the decision. This test sets
/// both variables in its own process — the exact state that made the
/// previous code divert or compose against a layer the pane never had —
/// and asserts wirk still takes the genuinely free inline slot.
///
#[test]
fn the_drivers_own_environment_is_not_the_panes_and_does_not_steer_the_decision() {
    // SAFETY: this integration binary holds exactly this one test, so
    // no other thread can be reading the environment concurrently.
    unsafe {
        std::env::set_var(
            OPENCODE_CONFIG_CONTENT_ENV,
            r#"{"instructions":["/tmp/driver.md"]}"#,
        );
        std::env::set_var(OPENCODE_CONFIG_ENV, "/tmp/driver.json");
    }
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let (splits, _) = launch_with_pane_env(estate.path(), worktree.path(), BTreeMap::new());
    let env = &splits[1].env;

    assert!(
        !env.contains_key(OPENCODE_CONFIG_ENV),
        "the pane's file slot is free; diverting to it would protect a layer that is not there: \
         {env:?}"
    );
    let inline = env
        .get(OPENCODE_CONFIG_CONTENT_ENV)
        .unwrap_or_else(|| panic!("the pane's inline slot is free and wirk takes it: {env:?}"));
    assert!(
        !inline.contains("/tmp/driver.md"),
        "the driver's own configuration document must never be injected into a pane: {inline}"
    );
}
