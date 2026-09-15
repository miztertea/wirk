//! P2.7 Wave 2 (`orient/reorient.md` §6 item 1, R4): the wirk-owned
//! opencode plugin's *delivery* mechanism, pinned deterministically —
//! no live opencode process runs here (that is the wave's own tried
//! step, `knowledge/evidence/p2-plugin-surface-2026-09-05/
//! wave2-opencode-tried.ndjson`). What this file pins: when
//! `HerdrExecutor::launch` builds an opencode Run's pane env
//! (`actor_pane`), it writes a wirk-owned plugin file and a config
//! naming it under the estate root — never the worktree, never `~/` —
//! and sets `OPENCODE_CONFIG_CONTENT` on the pane to that config's own
//! bytes; a
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
use wirk_herdr::claim_hook::{
    OPENCODE_CONFIG_CONTENT_ENV, OPENCODE_CONFIG_ENV, wirk_claim_plugin_js,
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
        branch: "p2-plugin-surface/w2".to_string(),
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

/// Red on `main` (`BUILD.md`'s pasted output): no key naming wirk's own
/// opencode configuration was ever inserted into `SplitPane.env` — this
/// test did not exist before that wave and `actor_pane` wrote only the
/// triple, `PATH`, and (when set) `CARGO_TARGET_DIR`.
///
/// The layer is delivered inline, through `OPENCODE_CONFIG_CONTENT`, and
/// `OPENCODE_CONFIG` is deliberately never set: it names a single file,
/// so a launch already carrying one would lose it (P4.1 correction,
/// OpenCode config scope). The file on disk stays the readable record,
/// and is where the plugin the layer names lives.
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
    // Ruling 0205: an opencode launch splits a short-lived probe pane
    // first — it reads the two opencode configuration values as the
    // pane will actually have them, since a pane's environment comes
    // from the Herdr server and not from this process — and the
    // actor's own pane second. `opencode_pane_env.rs` pins that
    // separation; here the assertions are about the actor's pane.
    let calls = client.split_pane_calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "the probe pane, then the actor's pane");
    let env = &calls[1].env;

    assert!(
        !env.contains_key(OPENCODE_CONFIG_ENV),
        "{OPENCODE_CONFIG_ENV} names one file and must be left to the launch: {env:?}"
    );
    let inline = env
        .get(OPENCODE_CONFIG_CONTENT_ENV)
        .unwrap_or_else(|| panic!("{OPENCODE_CONFIG_CONTENT_ENV} missing from pane env: {env:?}"));
    assert!(
        !inline.chars().any(char::is_control),
        "the inline layer crosses the pane environment as one line: {inline:?}"
    );

    // Neither the worktree nor `~/` was touched: the config file lives
    // under the estate root, outside the worktree entirely.
    let config_path = estate
        .path()
        .join(".wirk")
        .join("opencode")
        .join(&run.id.0)
        .join("wirk-opencode-config.json");
    let config_path = config_path.as_path();
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
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(inline).expect("the inline layer is JSON"),
        config,
        "the delivered layer and the file on disk must not drift"
    );
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
    // Rule 4 (`native-progress-contract-use/HANDOFF.md` §1.4), corrected
    // by the P3 execution-recovery connected-gap close: the written
    // plugin invokes this Run's own *pinned* `wirk` — not this test
    // binary's raw `current_exe()` — by absolute path, never the bare
    // name `wirk` (breaks the moment the driver binary is preserved or
    // renamed) and never the driver's own mutable `exe` directly (the
    // one thing item 1's pin exists to stop a hook from bypassing).
    let exe = std::env::current_exe().expect("current_exe");
    let pinned = estate
        .path()
        .join(".wirk")
        .join("runtime")
        .join(&run.id.0)
        .join("bin")
        .join("wirk");
    assert_eq!(
        plugin_contents,
        wirk_claim_plugin_js(&pinned),
        "the written plugin file matches the template with this Run's pinned wirk path \
         spliced in"
    );
    assert!(
        plugin_contents.contains("execFile(WIRK_CLAIM_BIN"),
        "the plugin must invoke WIRK_CLAIM_BIN, not the bare name `wirk`, which is \
         `command not found` whenever the driver binary is preserved or renamed: \
         {plugin_contents}"
    );
    assert!(
        plugin_contents.contains(&pinned.to_string_lossy().into_owned()),
        "the plugin must name this Run's own pinned wirk at {pinned:?}: {plugin_contents}"
    );
    assert!(
        !plugin_contents.contains(&exe.to_string_lossy().into_owned()),
        "the plugin must not name the driver's mutable current_exe directly, only this Run's \
         pinned copy of it: {plugin_contents}"
    );
    assert_eq!(
        std::fs::read(&pinned).expect("this Run's pinned wirk exists"),
        std::fs::read(&exe).expect("current_exe readable"),
        "this Run's pinned wirk must hold the driver's own bytes"
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
    for key in [OPENCODE_CONFIG_ENV, OPENCODE_CONFIG_CONTENT_ENV] {
        assert!(
            !calls[0].env.contains_key(key),
            "claude must not get the opencode plugin env key {key}"
        );
    }
}

// ---------------------------------------------------------------------
// P4.1 (ruling 0204): which native slot this Run's overlay takes, given
// what the launch already carries. `opencode_delivery` is a pure
// decision over two inherited values, so it is pinned here directly
// rather than through a pane: setting process environment variables
// from a threaded test is exactly the kind of global mutation these
// suites avoid.
//
// The four cases and the counterexample that forced them are measured
// against installed opencode 1.18.30 with `opencode debug config`
// (2026-09-12, `REPAIR.md`): setting `OPENCODE_CONFIG_CONTENT`
// unconditionally, as the candidate did, drops a launch's own inline
// `instructions`, `plugin`, `permission` and scalar entries wholesale.

fn overlay_fixture() -> wirk_herdr::claim_hook::OpencodeOverlay {
    wirk_herdr::claim_hook::OpencodeOverlay {
        config_path: std::path::PathBuf::from("/estate/.wirk/opencode/run-1/wirk-opencode-config.json"),
        config_content:
            r#"{"$schema":"https://opencode.ai/config.json","plugin":["/estate/wirk-claim.js"],"instructions":["/estate/contract.md"]}"#
                .to_string(),
    }
}

/// Nothing inherited: wirk's own bytes take the free inline slot, and
/// `OPENCODE_CONFIG` stays untouched — the candidate's behaviour, kept.
#[test]
fn a_launch_carrying_neither_variable_gets_the_inline_layer() {
    let overlay = overlay_fixture();
    match wirk_herdr::claim_hook::opencode_delivery(&overlay, None, None) {
        wirk_herdr::claim_hook::OpencodeDelivery::Content(bytes) => {
            assert_eq!(bytes, overlay.config_content);
        }
        other => panic!("expected the inline slot, got {other:?}"),
    }
}

/// A launch carrying its own inline layer must keep it. The free
/// `OPENCODE_CONFIG` slot is opencode's own second layer, measured to
/// compose with the inherited inline one — no parsing, no merge.
#[test]
fn an_inherited_inline_layer_is_never_replaced() {
    let overlay = overlay_fixture();
    let inherited = r#"{"instructions":["/tmp/launch.md"],"permission":{"bash":"ask"}}"#;
    match wirk_herdr::claim_hook::opencode_delivery(&overlay, Some(inherited), None) {
        wirk_herdr::claim_hook::OpencodeDelivery::ConfigFile(path) => {
            assert_eq!(path, overlay.config_path);
        }
        other => panic!("expected wirk's own file in the free OPENCODE_CONFIG slot, got {other:?}"),
    }
}

/// Both native slots taken: wirk appends its own two entries to the
/// inherited inline document and changes nothing else.
#[test]
fn with_both_slots_taken_the_inherited_document_is_appended_to_not_replaced() {
    let overlay = overlay_fixture();
    let inherited = r#"{"instructions":["/tmp/launch.md"],"plugin":["file:///tmp/p.js"],"permission":{"bash":"ask"},"small_model":"launch/chose-this"}"#;
    let composed = match wirk_herdr::claim_hook::opencode_delivery(
        &overlay,
        Some(inherited),
        Some("/tmp/launch.json"),
    ) {
        wirk_herdr::claim_hook::OpencodeDelivery::ComposedContent(bytes) => bytes,
        other => panic!("expected a composed inline layer, got {other:?}"),
    };
    assert!(
        !composed.chars().any(char::is_control),
        "the composed layer crosses the pane environment as one line: {composed:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&composed).expect("composed layer is JSON");
    assert_eq!(
        value["instructions"],
        serde_json::json!(["/tmp/launch.md", "/estate/contract.md"]),
        "the launch's own instructions entry survives, with the contract appended"
    );
    assert_eq!(
        value["plugin"],
        serde_json::json!(["file:///tmp/p.js", "/estate/wirk-claim.js"]),
        "the launch's own plugin survives, with the Claim plugin appended"
    );
    assert_eq!(
        value["permission"],
        serde_json::json!({"bash": "ask"}),
        "a field wirk owns no entry in is carried through untouched"
    );
    assert_eq!(value["small_model"], serde_json::json!("launch/chose-this"));
}

/// Outside the declared input scope — both slots taken and the
/// inherited inline value is not an object of the shape wirk can append
/// to — nothing is overwritten and the loss is disclosed by name.
#[test]
fn unsupported_inherited_content_is_disclosed_rather_than_overwritten() {
    let overlay = overlay_fixture();
    for inherited in [
        r#"["not","an","object"]"#,
        r#"{"instructions":"one-string"}"#,
        "{ not json",
    ] {
        match wirk_herdr::claim_hook::opencode_delivery(
            &overlay,
            Some(inherited),
            Some("/tmp/launch.json"),
        ) {
            wirk_herdr::claim_hook::OpencodeDelivery::Unsupported(reason) => {
                assert!(
                    reason.contains(OPENCODE_CONFIG_CONTENT_ENV),
                    "the reason names the variable wirk left alone: {reason}"
                );
                assert!(
                    reason.contains("Claim"),
                    "the reason says the Claim plugin is not delivered either: {reason}"
                );
            }
            other => panic!("expected a disclosed fallback for {inherited:?}, got {other:?}"),
        }
    }
}

/// An empty or blank inherited value is not a layer: the slot is free.
#[test]
fn a_blank_inherited_inline_value_leaves_the_slot_free() {
    let overlay = overlay_fixture();
    for inherited in ["", "   "] {
        match wirk_herdr::claim_hook::opencode_delivery(&overlay, Some(inherited), None) {
            wirk_herdr::claim_hook::OpencodeDelivery::Content(bytes) => {
                assert_eq!(bytes, overlay.config_content)
            }
            other => panic!("expected the inline slot for {inherited:?}, got {other:?}"),
        }
    }
}
