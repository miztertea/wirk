//! Parses `herdr-plugin.toml` (item 7 W1, ruling-cited in the manifest
//! file's own header comment) and pins its shape: the required
//! top-level fields, exactly three uniquely-id'd actions, one `split`
//! pane, no `[[events]]`, and every command either naming a script
//! that exists in the repo or resolving the `wirk` binary the same way
//! `plugin/startup.sh` does (`WIRK_BIN_PATH`/`CARGO_TARGET_DIR`). A
//! separate `bash -n` check pins `startup.sh`'s own syntax.

use std::path::PathBuf;
use std::process::Command;
use toml::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn manifest() -> Value {
    let text = std::fs::read_to_string(repo_root().join("herdr-plugin.toml"))
        .expect("herdr-plugin.toml must exist at the repo root");
    toml::from_str(&text).expect("herdr-plugin.toml must parse as TOML")
}

#[test]
fn top_level_fields_match_the_frozen_outcome() {
    let doc = manifest();
    assert_eq!(doc["id"].as_str(), Some("wirk"));
    assert_eq!(doc["min_herdr_version"].as_str(), Some("0.8.2"));
    let platforms = doc["platforms"]
        .as_array()
        .expect("platforms is an array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(platforms, vec!["linux"]);
}

#[test]
fn exactly_three_actions_with_unique_ids() {
    let doc = manifest();
    let actions = doc["actions"].as_array().expect("[[actions]] present");
    assert_eq!(actions.len(), 3, "exactly three actions, per BRIEF.md");
    let mut seen = std::collections::HashSet::new();
    for action in actions {
        let id = action["id"].as_str().expect("action id is a string");
        assert!(seen.insert(id), "duplicate action id: {id}");
    }
    let ids: std::collections::HashSet<&str> = seen;
    for expected in ["submit", "claim", "wirkd-status"] {
        assert!(ids.contains(expected), "missing action id: {expected}");
    }
}

#[test]
fn one_pane_with_placement_split() {
    let doc = manifest();
    let panes = doc["panes"].as_array().expect("[[panes]] present");
    assert_eq!(panes.len(), 1, "exactly one pane");
    assert_eq!(panes[0]["placement"].as_str(), Some("split"));
}

#[test]
fn no_events_table() {
    let doc = manifest();
    assert!(
        doc.get("events").is_none(),
        "no [[events]] per BRIEF.md outcome (R1)"
    );
}

/// Every `command` argv in the manifest (startup, actions, panes) must
/// start `bash`, and its second element must either name a script that
/// exists in the repo relative to the plugin root, or be `-c` with an
/// inline script that names the same binary-resolution variables
/// `plugin/startup.sh` uses (`WIRK_BIN_PATH`, `CARGO_TARGET_DIR`) —
/// the allow-listed shape for item 7 W1, which permits no second
/// script file beyond `startup.sh` itself.
#[test]
fn every_command_resolves_to_a_repo_script_or_the_wirk_binary() {
    let doc = manifest();
    let root = repo_root();

    let mut commands: Vec<&Value> = doc["startup"]
        .as_array()
        .expect("[[startup]] present")
        .iter()
        .map(|s| &s["command"])
        .collect();
    commands.extend(
        doc["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| &a["command"]),
    );
    commands.extend(
        doc["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| &p["command"]),
    );

    assert!(!commands.is_empty(), "at least one command to check");

    for command in commands {
        let argv = command.as_array().expect("command is an argv array");
        assert_eq!(
            argv[0].as_str(),
            Some("bash"),
            "every command starts bash (no bare PATH lookup, plugins.mdx)"
        );
        let second = argv[1].as_str().expect("second argv element is a string");
        if second == "-c" {
            let script = argv[2].as_str().expect("bash -c takes a script string");
            assert!(
                script.contains("WIRK_BIN_PATH") && script.contains("CARGO_TARGET_DIR"),
                "inline script must name the wirk binary resolution: {script}"
            );
        } else {
            let path = root.join(second);
            assert!(path.is_file(), "missing script named by command: {second}");
        }
    }
}

#[test]
fn startup_script_is_executable_and_syntactically_valid() {
    let path = repo_root().join("plugin/startup.sh");
    assert!(path.is_file(), "plugin/startup.sh must exist");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "plugin/startup.sh must be executable");
    }

    let output = Command::new("bash")
        .arg("-n")
        .arg(&path)
        .output()
        .expect("bash must be runnable to check syntax");
    assert!(
        output.status.success(),
        "bash -n plugin/startup.sh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The startup hook's own operator-blocker guard: with
/// `HERDR_PLUGIN_CONFIG_DIR` pointed at an empty temp dir (no `estate`
/// file), the script must print its one no-op line and exit 0 without
/// attempting to spawn anything — the property the run-brief names as
/// dissolving the operator blocker (no code change makes it safe to
/// fire in an unconfigured session, but the no-op-when-unconfigured
/// half is pinned here).
#[test]
fn startup_script_no_ops_without_a_configured_estate() {
    let path = repo_root().join("plugin/startup.sh");
    let config_dir =
        std::env::temp_dir().join(format!("wirk-plugin-manifest-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new("bash")
        .arg(&path)
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .env("HERDR_PLUGIN_STATE_DIR", &config_dir)
        .env("HERDR_PLUGIN_ROOT", repo_root())
        .output()
        .expect("startup.sh must run under bash");

    std::fs::remove_dir_all(&config_dir).ok();

    assert!(
        output.status.success(),
        "startup.sh must exit 0 when unconfigured: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no estate configured"),
        "expected the one-line no-op log, got: {stdout}"
    );
}

/// `wirk plugin init --estate <root>` writes the root into
/// `$HERDR_PLUGIN_CONFIG_DIR/estate`, one line — the file
/// `startup.sh` and the manifest's actions read.
#[test]
fn plugin_init_writes_the_estate_file() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    let config_dir =
        std::env::temp_dir().join(format!("wirk-plugin-init-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(bin)
        .args(["plugin", "init", "--estate", "/var/tmp/some-estate"])
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .output()
        .expect("wirk plugin init must run");

    let written = std::fs::read_to_string(config_dir.join("estate"));
    std::fs::remove_dir_all(&config_dir).ok();

    assert!(output.status.success(), "wirk plugin init must exit 0");
    assert_eq!(written.unwrap().trim(), "/var/tmp/some-estate");
}
