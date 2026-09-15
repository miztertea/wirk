//! Ruling 0205: which opencode configuration slot an actor's launch may
//! take is a fact about **the pane the actor will run in**, and this
//! file pins that the launch reads it from there.
//!
//! A Herdr pane's environment is the Herdr *server's* environment,
//! overlaid per key by the `env` map the caller passes at creation. The
//! wirk driver's own environment is on neither path. Measured against
//! herdr 0.9.0 on 2026-09-12: a pane created with no `env` of its own
//! carries the server's `OPENCODE_CONFIG_CONTENT` verbatim, an explicit
//! per-pane value replaces it wholesale, and a variable exported only in
//! the calling process never reaches the pane at all. So the previous
//! `std::env::var` read in the driver answered a different question,
//! wrongly in both directions, and `opencode debug config` inside a real
//! pane showed the consequence: a server-carried layer's `instructions`,
//! `plugin`, `permission.bash` and `small_model` all gone, journalled as
//! a successful native delivery.
//!
//! `FakeHerdrClient` models a pane as a shell with an environment
//! (`pane_env`), and removes both opencode variables from this process's
//! own environment before applying it — so these tests pin the
//! separation itself, not only the decision.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::tempdir;

use wirk_core::{
    ActorKind, ActorWorld, ArtifactSpec, Boundary, ExecutionTriple, Executor, OutputContract, Run,
    RunId, RunState, WaypointId, WorkId, World, WorldHash,
};
use wirk_herdr::claim_hook::{OPENCODE_CONFIG_CONTENT_ENV, OPENCODE_CONFIG_ENV};
use wirk_herdr::fake::FakeHerdrClient;
use wirk_herdr::{AgentStatus, HerdrError, HerdrExecutor, PaneInfo, SplitPane};

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

fn pane_env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
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

/// Ruling 0208. The probe pane is created with **the actor's own base
/// launch environment** — byte-identical to the actor pane's `env` map
/// except for the opencode key this very decision then chooses — so the
/// two panes are the same context in both halves, not only in the
/// inherited half.
///
/// Red on 5195eaa: the probe pane was split with `env: {}` while the
/// actor pane carried `WIRK_ESTATE_ROOT`, `WIRK_WORK_ID`, `WIRK_RUN_ID`,
/// `PATH` and `CARGO_TARGET_DIR`. That asymmetry was harmless for every
/// rc this box has, and unmeasurable for one that *derives* an opencode
/// variable from a launch variable — the residual the independent
/// affected-path report recorded as unproven. Handing the probe the
/// same map closes it by construction rather than by observation.
///
/// The probe pane is still closed again, leaving nothing behind but the
/// actor's own pane.
#[test]
fn the_probe_pane_carries_the_actors_own_base_launch_environment_and_is_closed() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let (splits, closed) = launch_with_pane_env(estate.path(), worktree.path(), BTreeMap::new());

    assert_eq!(splits.len(), 2, "a probe pane, then the actor's own pane");
    assert!(
        splits[0].env.contains_key("WIRK_RUN_ID"),
        "the probe must carry the launch's own variables, or a shell rule deriving an \
         opencode key from one of them would be read wrongly: {:?}",
        splits[0].env
    );
    let mut actor_base = splits[1].env.clone();
    for key in [OPENCODE_CONFIG_ENV, OPENCODE_CONFIG_CONTENT_ENV] {
        actor_base.remove(key);
    }
    assert_eq!(
        splits[0].env, actor_base,
        "the probe's pane and the actor's pane must differ only in the opencode key this \
         probe exists to choose"
    );
    assert_eq!(
        splits[0].cwd, splits[1].cwd,
        "and in nothing else about the placement"
    );
    assert!(
        closed.contains(&"probe-pane".to_string()),
        "the probe pane is closed once it has answered: {closed:?}"
    );
    assert!(
        !closed.contains(&"run-1".to_string()),
        "the actor's own pane is never closed by the probe: {closed:?}"
    );
}

/// **The defect, as a test.** The pane carries an inline layer the
/// driver cannot see. Before this fix the launch read its own process
/// environment, found the inline slot free, and set
/// `OPENCODE_CONFIG_CONTENT` — which replaces that layer wholesale.
/// Measured consequence, `opencode debug config` inside a real Herdr
/// pane: the launch's `instructions`, `plugin`, `permission.bash` and
/// `small_model` all disappear.
#[test]
fn an_inline_layer_only_the_pane_can_see_is_never_replaced() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let carried = pane_env(&[(
        OPENCODE_CONFIG_CONTENT_ENV,
        r#"{"instructions":["/tmp/server.md"],"permission":{"bash":"ask"},"small_model":"server/chose-this"}"#,
    )]);
    let (splits, _) = launch_with_pane_env(estate.path(), worktree.path(), carried);
    let env = &splits[1].env;

    assert!(
        !env.contains_key(OPENCODE_CONFIG_CONTENT_ENV),
        "the inline slot is taken by a layer only the pane can see; setting it would replace \
         that layer wholesale: {env:?}"
    );
    let file = env
        .get(OPENCODE_CONFIG_ENV)
        .unwrap_or_else(|| panic!("wirk must take the free file slot instead: {env:?}"));
    assert!(
        std::path::Path::new(file).is_file(),
        "the file slot names wirk's own overlay, written on disk: {file}"
    );
}

/// Both slots carried by the pane: wirk appends its own two entries to
/// the pane's inline document and carries every other key through.
/// Nothing the launch configured is lost.
#[test]
fn both_slots_carried_by_the_pane_compose_rather_than_replace() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let carried = pane_env(&[
        (
            OPENCODE_CONFIG_CONTENT_ENV,
            r#"{"instructions":["/tmp/server.md"],"plugin":["/tmp/server.js"],"permission":{"bash":"ask"},"small_model":"server/chose-this"}"#,
        ),
        (OPENCODE_CONFIG_ENV, "/tmp/server.json"),
    ]);
    let (splits, _) = launch_with_pane_env(estate.path(), worktree.path(), carried);
    let env = &splits[1].env;

    assert!(
        !env.contains_key(OPENCODE_CONFIG_ENV),
        "the file slot is taken too and must be left alone: {env:?}"
    );
    let composed = env
        .get(OPENCODE_CONFIG_CONTENT_ENV)
        .unwrap_or_else(|| panic!("the composed layer is missing: {env:?}"));
    let value: serde_json::Value =
        serde_json::from_str(composed).expect("the composed layer is JSON");
    assert_eq!(
        value["instructions"][0],
        serde_json::json!("/tmp/server.md"),
        "the pane's own instructions entry survives first: {value}"
    );
    assert_eq!(
        value["instructions"].as_array().map(Vec::len),
        Some(1),
        "this World reserved no contract, so only the pane's entry is there: {value}"
    );
    assert_eq!(
        value["plugin"].as_array().map(Vec::len),
        Some(2),
        "the pane's own plugin survives, with wirk's Claim plugin appended: {value}"
    );
    assert_eq!(
        value["permission"],
        serde_json::json!({"bash": "ask"}),
        "a key wirk owns no entry in is carried through untouched: {value}"
    );
    assert_eq!(
        value["small_model"],
        serde_json::json!("server/chose-this"),
        "an unrelated scalar the launch set survives: {value}"
    );
}

/// A pane that cannot be asked loses nothing: wirk sets neither
/// variable, and says so rather than overwriting on a guess.
#[test]
fn a_pane_that_cannot_be_asked_keeps_its_configuration_and_the_loss_is_disclosed() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let run = opencode_run();
    let world = actor_world(&run, estate.path(), worktree.path());
    let client = Arc::new(
        FakeHerdrClient::default()
            .with_split_pane_responses(vec![pane_info("probe-pane"), pane_info(&run.id.0)])
            .with_split_pane_response(pane_info(&run.id.0))
            .with_send_input_error(HerdrError::Transport("pane is not reachable".to_string())),
    );
    HerdrExecutor::new(client.clone())
        .launch(&run, &world)
        .expect("an unanswerable probe degrades, it does not fail the launch");

    let splits = client.split_pane_calls.lock().unwrap();
    let env = &splits[1].env;
    for key in [OPENCODE_CONFIG_ENV, OPENCODE_CONFIG_CONTENT_ENV] {
        assert!(
            !env.contains_key(key),
            "an unknown configuration is never overwritten on a guess: {key} in {env:?}"
        );
    }
}

/// Ruling 0208, the residue the independent affected-path report
/// measured live: the probe copies **the launch's own configuration**
/// into wirk-owned scratch, and on 5195eaa that copy survived every
/// error return — `probe.clear()` ran before the probe and after a
/// successful read, and on no `Err` path at all. The failure path is the
/// one that kept it longest: 317 bytes of the launch's inline document,
/// byte-identical, left in the estate's run directory for the life of
/// the estate.
///
/// The failure is produced natively and **after a partial write**: the
/// probe's own config path is pre-created as a directory, so the
/// script's first `printf` writes the inherited inline document to the
/// content file and its second fails, the completion marker is never
/// written, and the probe times out with one value already on disk.
/// Green: nothing owned by this probe is left behind, the launch still
/// degrades instead of failing, and neither opencode variable is set.
#[test]
fn the_probes_copy_of_the_launchs_configuration_is_cleared_on_the_failure_path_too() {
    let estate = tempdir().expect("estate tempdir");
    let worktree = tempdir().expect("worktree tempdir");
    let run_dir = estate.path().join(".wirk").join("opencode").join("run-1");
    std::fs::create_dir_all(run_dir.join("wirk-opencode-env-config"))
        .expect("pre-create the probe's config path as a directory");

    let carried = pane_env(&[(
        OPENCODE_CONFIG_CONTENT_ENV,
        r#"{"instructions":["/tmp/server.md"],"small_model":"server/chose-this"}"#,
    )]);
    let (splits, _) = launch_with_pane_env(estate.path(), worktree.path(), carried);

    assert!(
        !run_dir.join("wirk-opencode-env-content").exists(),
        "the launch's own configuration must not be left in wirk's scratch on an error \
         return: a launch layer can carry anything the owner put in it"
    );
    assert!(
        !run_dir.join("wirk-opencode-env-done").exists(),
        "no completion marker survives a probe that never completed"
    );
    assert!(
        run_dir.join("wirk-opencode-env-config").is_dir(),
        "a path this probe does not own is left exactly as it was found"
    );

    let env = &splits[1].env;
    for key in [OPENCODE_CONFIG_ENV, OPENCODE_CONFIG_CONTENT_ENV] {
        assert!(
            !env.contains_key(key),
            "an unreadable probe never overwrites a layer on a guess: {key} in {env:?}"
        );
    }
}
