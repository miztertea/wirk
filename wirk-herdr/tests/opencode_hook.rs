//! P2.7 Wave 2 (`orient/reorient.md` §6 item 1, R4): the wirk-owned
//! opencode plugin's *delivery* mechanism, pinned deterministically —
//! no live opencode process runs here (that is the wave's own tried
//! step, `knowledge/evidence/p2-plugin-surface-2026-09-05/
//! wave2-opencode-tried.ndjson`). What this file pins: when
//! `HerdrExecutor::launch` builds an opencode Run's pane env
//! (`actor_pane`), it writes a wirk-owned plugin file and a config
//! naming it under the estate root — never the worktree, never `~/` —
//! and sets `OPENCODE_CONFIG` on the pane to that config's path; a
//! claude Run gets neither. The plugin's own runtime *logic* (does it
//! actually call `wirk claim` on `session.idle` and ignore child
//! sessions) is pinned separately in `opencode_hook_plugin.rs`, since
//! `FakeHerdrClient` never runs a real opencode process to exercise it
//! — a shell-scripted stand-in (`support/scripted_actor.rs`) is not
//! opencode and cannot load a JS plugin either (`BUILD.md` names this
//! choice).

use std::sync::Arc;

use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ExecutionTriple, Executor, OutputContract, Run,
    RunId, RunState, WaypointId, WorkId, World, WorldHash,
};
use wirk_herdr::claim_hook::{OPENCODE_CONFIG_ENV, WIRK_CLAIM_PLUGIN_JS};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{AgentStatus, HerdrExecutor, PaneInfo};

fn run_with_kind(kind: ActorKind) -> Run {
    Run {
        id: RunId("run-1".to_string()),
        waypoint: WaypointId("route-1/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind,
    }
}

fn actor_world(run: &Run, estate_root: &std::path::Path, worktree_path: &std::path::Path) -> World {
    World::Actor(ActorWorld {
        repository: "wirk".to_string(),
        worktree_path: worktree_path.to_path_buf(),
        branch: "p2-plugin-surface/w2".to_string(),
        base_sha: "abc123".to_string(),
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

/// Red on `main` (`BUILD.md`'s pasted output): no key named
/// `OPENCODE_CONFIG` is ever inserted into `SplitPane.env` — this test
/// did not exist before this wave and `actor_pane` wrote only the
/// triple, `PATH`, and (when set) `CARGO_TARGET_DIR`.
#[test]
fn opencode_run_gets_a_wirk_owned_claim_plugin_with_no_worktree_or_home_write() {
    let run = run_with_kind(ActorKind::opencode());
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let world = actor_world(&run, estate.path(), worktree.path());

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    executor.launch(&run, &world).expect("launch succeeds");

    // `actor_pane` reuses `split_pane` on the reuse branch (`get_pane`
    // unset on the fake defaults to `NotFound`, so this goes through
    // the fresh-workspace `create_workspace` + `split_pane` branch,
    // `lib.rs:918-937`) — either branch writes the same `env` map,
    // asserted here on the one `split_pane` call this launch makes.
    let calls = client.split_pane_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one split_pane call");
    let env = &calls[0].env;

    let config_path = env
        .get(OPENCODE_CONFIG_ENV)
        .unwrap_or_else(|| panic!("{OPENCODE_CONFIG_ENV} missing from pane env: {env:?}"));

    // Neither the worktree nor `~/` was touched: the config path lives
    // under the estate root, outside the worktree entirely.
    let config_path = std::path::Path::new(config_path);
    assert!(
        config_path.starts_with(estate.path()),
        "{config_path:?} is not under the estate root {:?}",
        estate.path()
    );
    assert!(
        !config_path.starts_with(worktree.path()),
        "{config_path:?} must not be written into the worktree"
    );
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        assert!(
            !config_path.starts_with(&home),
            "{config_path:?} must not be written under $HOME"
        );
    }

    let config_contents = std::fs::read_to_string(config_path).expect("config file exists");
    let config: serde_json::Value =
        serde_json::from_str(&config_contents).expect("config file is valid JSON");
    let plugin_array = config["plugin"]
        .as_array()
        .expect("config declares a \"plugin\" array");
    assert_eq!(plugin_array.len(), 1, "exactly one plugin named");
    let plugin_path = plugin_array[0]
        .as_str()
        .expect("plugin array entry is a path string");
    let plugin_path = std::path::Path::new(plugin_path);
    assert!(
        plugin_path.is_absolute(),
        "the plugin array must name the plugin by absolute path (w2-probe.md Mechanism 2)"
    );
    assert!(plugin_path.starts_with(estate.path()));

    let plugin_contents = std::fs::read_to_string(plugin_path).expect("plugin file exists");
    assert_eq!(
        plugin_contents, WIRK_CLAIM_PLUGIN_JS,
        "the written plugin file is exactly the crate's shipped plugin, unmodified"
    );
}

/// A claude Run gets no `OPENCODE_CONFIG` key at all — this wave adds
/// nothing for claude's own hook (Wave 3's item, `build-brief.md` §6).
#[test]
fn claude_run_gets_no_opencode_config_key() {
    let run = run_with_kind(ActorKind::claude());
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let world = actor_world(&run, estate.path(), worktree.path());

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    executor.launch(&run, &world).expect("launch succeeds");

    let calls = client.split_pane_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(
        !calls[0].env.contains_key(OPENCODE_CONFIG_ENV),
        "claude must not get the opencode plugin env key"
    );
}
