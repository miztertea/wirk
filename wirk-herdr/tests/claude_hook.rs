//! P2.7 Wave 3 (`build-brief.md` §6 item 1, `reorient.md` §C): a
//! claude Run's `agent.start` argv carries `--settings <path>` naming a
//! wirk-owned settings file under the estate root — never the
//! worktree, never `~/` — declaring a `Stop` hook that runs bare
//! `wirk claim` and nothing else. No live opencode/claude process runs
//! here: this file pins the *delivery* mechanism (does
//! `start_actor_agent` build the right argv and does the file it names
//! exist and say the right thing) deterministically, against
//! `FakeHerdrClient`. The hook *command*'s own behaviour (does running
//! it actually invoke `wirk claim` with the pane's env) is pinned
//! separately in `claude_hook_command.rs`, since neither of those is a
//! claim about Claude Code's own process actually loading and firing
//! the file — that is this wave's tried step.

use std::sync::Arc;

use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ExecutionTriple, Executor, OutputContract, Run,
    RunId, RunState, WaypointId, WorkId, World, WorldHash,
};
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
        branch: "p2-plugin-surface/w3".to_string(),
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

/// Red on `main` (`BUILD.md`'s pasted output): `start_actor_agent`'s
/// claude arm is `["--model", "sonnet"]` and nothing else — no
/// `--settings` element, no file written anywhere.
#[test]
fn claude_run_gets_a_settings_flag_naming_a_wirk_owned_stop_hook_under_the_estate_root() {
    let run = run_with_kind(ActorKind::claude());
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let world = actor_world(&run, estate.path(), worktree.path());

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    executor.launch(&run, &world).expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one agent.start call");
    let args = &calls[0].args;

    let flag_index = args
        .iter()
        .position(|a| a == "--settings")
        .unwrap_or_else(|| panic!("no --settings element in claude's argv: {args:?}"));
    let settings_path = args
        .get(flag_index + 1)
        .unwrap_or_else(|| panic!("--settings has no following value: {args:?}"));
    let settings_path = std::path::Path::new(settings_path);

    // Neither the worktree nor `~/` was touched: the settings file
    // lives under the estate root, outside the worktree entirely.
    assert!(
        settings_path.starts_with(estate.path()),
        "{settings_path:?} is not under the estate root {:?}",
        estate.path()
    );
    assert!(
        !settings_path.starts_with(worktree.path()),
        "{settings_path:?} must not be written into the worktree"
    );
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        assert!(
            !settings_path.starts_with(&home),
            "{settings_path:?} must not be written under $HOME"
        );
    }

    let contents = std::fs::read_to_string(settings_path).expect("settings file exists");
    let settings: serde_json::Value =
        serde_json::from_str(&contents).expect("settings file is valid JSON");

    // Declares nothing but a Stop hook: no permissions, no other hook
    // event, no model (0054 D163a; `build-brief.md` §6 item 1).
    let obj = settings.as_object().expect("settings is a JSON object");
    assert_eq!(
        obj.keys().collect::<Vec<_>>(),
        vec!["hooks"],
        "settings must declare nothing but hooks: {settings}"
    );
    let hooks = settings["hooks"].as_object().expect("hooks is an object");
    assert_eq!(
        hooks.keys().collect::<Vec<_>>(),
        vec!["Stop"],
        "hooks must declare nothing but Stop: {settings}"
    );

    let stop = settings["hooks"]["Stop"]
        .as_array()
        .expect("Stop is an array of matcher groups");
    assert_eq!(stop.len(), 1);
    let inner = stop[0]["hooks"].as_array().expect("hooks array");
    assert_eq!(inner.len(), 1, "exactly one hook command: {settings}");
    assert_eq!(inner[0]["type"], "command");
    assert_eq!(
        inner[0]["command"], "wirk claim",
        "the Stop hook's command must be bare `wirk claim` (W1's flagless \
         form self-populates from wirkd's own declared-output contract)"
    );
    assert!(
        inner[0].get("permissions").is_none(),
        "no permission policy is written (0054 D163a)"
    );
}

/// An opencode Run gets no `--settings` element at all — this wave adds
/// nothing for opencode's own hook (delivered by env var, Wave 2).
#[test]
fn opencode_run_gets_no_settings_flag() {
    let run = run_with_kind(ActorKind::opencode());
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let world = actor_world(&run, estate.path(), worktree.path());

    let client =
        Arc::new(FakeHerdrClient::default().with_split_pane_response(pane_info(&run.id.0)));
    let executor = HerdrExecutor::new(client.clone());
    executor.launch(&run, &world).expect("launch succeeds");

    let calls = client.start_agent_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(
        !calls[0].args.iter().any(|a| a == "--settings"),
        "opencode must not get the claude --settings flag: {:?}",
        calls[0].args
    );
}
