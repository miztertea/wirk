//! Parses `herdr-plugin.toml` (item 7 W1, ruling-cited in the manifest
//! file's own header comment) and pins its shape: the required
//! top-level fields, exactly five uniquely-id'd actions, one `split`
//! pane, no `[[events]]`, and every command either naming a script
//! that exists in the repo or resolving the `wirk` binary the same way
//! `plugin/startup.sh` does (`WIRK_BIN_PATH`/`CARGO_TARGET_DIR`). A
//! separate `bash -n` check pins `startup.sh`'s own syntax.

use std::path::PathBuf;
use std::process::Command;
use toml::Value;

/// The manifest and the plugin directory as the repo carries them,
/// embedded at compile time (`include_str!`, R3) and materialized into
/// a temp root per test, so the binary never reads a source path at run
/// time (a binary compiled in one worktree and reused from the shared
/// cargo cache after that worktree was removed failed at the P2.3 land,
/// 2026-09-05). The temp root is removed when the guard drops.
fn repo_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("wirk-plugin-manifest-")
        .tempdir_in("/var/tmp")
        .expect("temp root under /var/tmp");
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("herdr-plugin.toml"), MANIFEST_TEXT).unwrap();
    std::fs::create_dir_all(root.join("plugin")).unwrap();
    for (name, text) in PLUGIN_FILES {
        std::fs::write(root.join("plugin").join(name), *text).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in EXECUTABLE_SCRIPTS {
            std::fs::set_permissions(
                root.join("plugin").join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }
    (dir, root)
}

const MANIFEST_TEXT: &str = include_str!("../../herdr-plugin.toml");

/// Every file under `plugin/`, embedded so the tests run against a
/// materialized copy rather than a source path.
const PLUGIN_FILES: &[(&str, &str)] = &[
    ("startup.sh", include_str!("../../plugin/startup.sh")),
    ("assistant.sh", include_str!("../../plugin/assistant.sh")),
    ("configure.sh", include_str!("../../plugin/configure.sh")),
    ("browser.sh", include_str!("../../plugin/browser.sh")),
    ("build.sh", include_str!("../../plugin/build.sh")),
    ("wirk-bin.sh", include_str!("../../plugin/wirk-bin.sh")),
    ("README.md", include_str!("../../plugin/README.md")),
];

/// The scripts a shell runs as programs; `wirk-bin.sh` is sourced, not
/// executed, so it is deliberately not here.
const EXECUTABLE_SCRIPTS: &[&str] = &[
    "startup.sh",
    "assistant.sh",
    "configure.sh",
    "browser.sh",
    "build.sh",
];

fn manifest() -> Value {
    toml::from_str(MANIFEST_TEXT).expect("herdr-plugin.toml must parse as TOML")
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
fn exactly_five_actions_with_unique_ids() {
    let doc = manifest();
    let actions = doc["actions"].as_array().expect("[[actions]] present");
    assert_eq!(
        actions.len(),
        5,
        "assistant, configure, claim, browser, wirkd-status"
    );
    let mut seen = std::collections::HashSet::new();
    for action in actions {
        let id = action["id"].as_str().expect("action id is a string");
        assert!(seen.insert(id), "duplicate action id: {id}");
    }
    let ids: std::collections::HashSet<&str> = seen;
    for expected in ["assistant", "configure", "claim", "browser", "wirkd-status"] {
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
/// the allow-listed shape for item 7 W1, which permits the two script
/// files this manifest names, `startup.sh` and `assistant.sh`.
#[test]
fn every_command_resolves_to_a_repo_script_or_the_wirk_binary() {
    let doc = manifest();
    let (_guard, root) = repo_root();

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
                script.contains("plugin/wirk-bin.sh")
                    && script.contains("wirk_resolve_bin")
                    && script.contains("wirk_bin_explain_missing"),
                "an inline script must resolve the binary through the shared helper, \
                 and explain a failure to resolve it: {script}"
            );
            assert!(
                !script.contains("WIRK_BIN_PATH") && !script.contains("CARGO_TARGET_DIR"),
                "an inline script must not re-implement binary resolution: {script}"
            );
        } else {
            let path = root.join(second);
            assert!(path.is_file(), "missing script named by command: {second}");
        }
    }
}

#[test]
fn every_plugin_script_is_executable_and_syntactically_valid() {
    let (_guard, root) = repo_root();

    for (name, _) in PLUGIN_FILES {
        if !name.ends_with(".sh") {
            continue;
        }
        let path = root.join("plugin").join(name);
        assert!(path.is_file(), "plugin/{name} must exist");

        #[cfg(unix)]
        if EXECUTABLE_SCRIPTS.contains(name) {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "plugin/{name} must be executable");
        }

        let output = Command::new("bash")
            .arg("-n")
            .arg(&path)
            .output()
            .expect("bash must be runnable to check syntax");
        assert!(
            output.status.success(),
            "bash -n plugin/{name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
    let (_guard, root) = repo_root();
    let path = root.join("plugin/startup.sh");
    let config_dir =
        std::env::temp_dir().join(format!("wirk-plugin-manifest-test-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new("bash")
        .arg(&path)
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .env("HERDR_PLUGIN_STATE_DIR", &config_dir)
        .env("HERDR_PLUGIN_ROOT", &root)
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

/// A temp directory to use as both the plugin config dir and (when
/// asked) an estate root that actually exists.
fn scratch(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/var/tmp")
        .expect("temp dir under /var/tmp")
}

/// `wirk plugin init` writes each value it is given, one line per file,
/// into `$HERDR_PLUGIN_CONFIG_DIR` — the files `startup.sh`, the
/// manifest's actions and the status pane read.
#[test]
fn plugin_init_writes_the_estate_and_harness_files() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    let dir = scratch("wirk-plugin-init-");
    let config_dir = dir.path().join("config");
    let estate = dir.path().join("estate");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&estate).unwrap();

    let output = Command::new(bin)
        .args(["plugin", "init"])
        .arg("--estate")
        .arg(&estate)
        .args(["--harness", "claude"])
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .output()
        .expect("wirk plugin init must run");

    assert!(
        output.status.success(),
        "wirk plugin init must exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(config_dir.join("estate"))
            .unwrap()
            .trim(),
        estate.display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(config_dir.join("harness"))
            .unwrap()
            .trim(),
        "claude"
    );
}

/// An estate that is not a directory is refused at the moment it is
/// named. Writing it would move the failure to the startup hook, which
/// would report "no wirkd" for what is really "that path is not there".
#[test]
fn plugin_init_refuses_an_estate_that_is_not_a_directory() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    let dir = scratch("wirk-plugin-init-bad-");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let missing = dir.path().join("nowhere");

    let output = Command::new(bin)
        .args(["plugin", "init"])
        .arg("--estate")
        .arg(&missing)
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .output()
        .expect("wirk plugin init must run");

    assert!(!output.status.success(), "must not exit 0");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is not an existing directory"),
        "must say why: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !config_dir.join("estate").exists(),
        "a refused estate must not be written"
    );
}

/// `wirk plugin show` reads rather than writes: with nothing configured
/// it exits 0 and names each unset value together with the command that
/// sets it.
#[test]
fn plugin_show_names_what_is_unset_and_how_to_set_it() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    let dir = scratch("wirk-plugin-show-");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let output = Command::new(bin)
        .args(["plugin", "show"])
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .output()
        .expect("wirk plugin show must run");

    assert!(output.status.success(), "show is a read, so it exits 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("estate     (not set)"), "got: {stdout}");
    assert!(stdout.contains("harness    (not set)"), "got: {stdout}");
    assert!(
        stdout.contains("wirk plugin init --estate")
            && stdout.contains("wirk plugin init --harness"),
        "must name the commands that set them: {stdout}"
    );
}

/// Outside a Herdr plugin invocation there is no per-plugin config
/// directory, and every `wirk plugin` verb says so rather than guessing
/// a path.
#[test]
fn plugin_verbs_refuse_without_a_plugin_config_dir() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    for args in [
        vec!["plugin", "show"],
        vec!["plugin", "init", "--harness", "claude"],
    ] {
        let output = Command::new(bin)
            .args(&args)
            .env_remove("HERDR_PLUGIN_CONFIG_DIR")
            .output()
            .expect("wirk plugin must run");
        assert!(!output.status.success(), "{args:?} must not exit 0");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("HERDR_PLUGIN_CONFIG_DIR"),
            "{args:?} must name the missing variable"
        );
    }
}

/// The manifest declares how an installation gets a binary. Without a
/// `[[build]]` step, `herdr plugin install` registers a plugin whose
/// every command points at a `wirk` that was never built.
#[test]
fn the_manifest_declares_a_build_step_naming_the_build_script() {
    let doc = manifest();
    let builds = doc["build"].as_array().expect("[[build]] present");
    assert_eq!(builds.len(), 1, "one build step");
    let argv: Vec<&str> = builds[0]["command"]
        .as_array()
        .expect("build command is an argv array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(argv, vec!["bash", "plugin/build.sh"]);
}

/// Binary resolution, in order, exercised against the helper every
/// entry point sources. The point of the helper is that there is one
/// answer; this is that answer.
#[test]
fn the_shared_helper_resolves_the_binary_in_one_documented_order() {
    let (_guard, root) = repo_root();
    let helper = root.join("plugin/wirk-bin.sh");
    let target = root.join("target");
    std::fs::create_dir_all(target.join("release")).unwrap();
    std::fs::create_dir_all(target.join("debug")).unwrap();
    let path_dir = root.join("fakepath");
    std::fs::create_dir_all(&path_dir).unwrap();

    // A stand-in has to behave like wirk where the resolver actually
    // looks: `wirk --help` writes a usage line naming the verbs this
    // plugin invokes, to stderr, and exits non-zero. A bare `exit 0`
    // is exactly the shape the resolver now refuses.
    let make = |path: &std::path::Path| {
        std::fs::write(
            path,
            "#!/bin/sh
echo 'usage: wirk claim | wirk wirkd start | wirk plugin show' >&2
exit 1
",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    };
    let release = target.join("release/wirk");
    let debug = target.join("debug/wirk");
    let on_path = path_dir.join("wirk");
    let explicit = root.join("explicit-wirk");

    let resolve = |extra_env: &[(&str, &str)]| -> (bool, String) {
        let mut command = Command::new(bash());
        command
            .arg("-c")
            .arg(format!(". {}; wirk_resolve_bin", helper.display()))
            .env("HERDR_PLUGIN_ROOT", &root)
            .env("PATH", path_dir.display().to_string())
            .env_remove("WIRK_BIN_PATH")
            .env_remove("CARGO_TARGET_DIR");
        for (k, v) in extra_env {
            command.env(k, v);
        }
        let output = command.output().expect("bash runs the helper");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )
    };

    // Nothing anywhere: no answer, and a non-zero status so a caller
    // can tell "none" from "this one".
    let (ok, out) = resolve(&[]);
    assert!(!ok && out.is_empty(), "no binary must resolve to nothing");

    make(&debug);
    assert_eq!(resolve(&[]).1, debug.display().to_string(), "debug is last");

    make(&on_path);
    assert_eq!(
        resolve(&[]).1,
        on_path.display().to_string(),
        "PATH beats a debug build"
    );

    make(&release);
    assert_eq!(
        resolve(&[]).1,
        release.display().to_string(),
        "the install-time release build beats PATH"
    );

    make(&explicit);
    assert_eq!(
        resolve(&[("WIRK_BIN_PATH", &explicit.display().to_string())]).1,
        explicit.display().to_string(),
        "an explicit WIRK_BIN_PATH beats everything"
    );

    // Set but not executable is an error, never a fall-through to a
    // binary the operator did not name.
    let (ok, out) = resolve(&[("WIRK_BIN_PATH", "/nonexistent/wirk")]);
    assert!(
        !ok && out.is_empty(),
        "an unusable WIRK_BIN_PATH must not silently resolve to something else"
    );
}

/// The explanation an operator actually gets. It has to name a way out;
/// a list of paths that were tried is not one.
#[test]
fn the_shared_helper_explains_a_missing_binary_actionably() {
    let (_guard, root) = repo_root();
    let helper = root.join("plugin/wirk-bin.sh");

    let output = Command::new(bash())
        .arg("-c")
        .arg(format!(". {}; wirk_bin_explain_missing", helper.display()))
        .env("HERDR_PLUGIN_ROOT", &root)
        .env_remove("WIRK_BIN_PATH")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("bash runs the helper");
    let text = String::from_utf8_lossy(&output.stdout);

    for expected in [
        "herdr plugin install",
        "cargo build --release",
        "WIRK_BIN_PATH",
        "rustup",
    ] {
        assert!(
            text.contains(expected),
            "the explanation must name {expected:?}: {text}"
        );
    }
}

/// The install-time build step refuses rather than half-installing when
/// the toolchain it needs is absent, and says which toolchain.
#[test]
fn the_build_step_stops_with_an_explanation_when_cargo_is_absent() {
    let (_guard, root) = repo_root();
    // bash is spawned by absolute path, so the child's PATH can hold
    // nothing at all and the script still runs — the point being that
    // there is no cargo anywhere on it.
    let empty_path = root.join("emptypath");
    std::fs::create_dir_all(&empty_path).unwrap();

    let output = Command::new(bash())
        .arg(root.join("plugin/build.sh"))
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("PATH", empty_path.display().to_string())
        .env_remove("WIRK_BIN_PATH")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("bash runs build.sh");

    assert!(
        !output.status.success(),
        "a build that cannot run must not report success"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("no 'cargo' on PATH") && text.contains("rustup.rs"),
        "must say what is missing and where to get it: {text}"
    );
}

/// `bash`'s absolute path on the current `PATH`. The tests below give
/// their child processes a `PATH` of their own choosing, so the shell
/// itself has to be named outright rather than looked up in it.
fn bash() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|dir| dir.join("bash"))
        .find(|candidate| candidate.is_file())
        .expect("bash is on PATH")
}

/// Ruling 0117: the two operator surfaces this manifest wraps —
/// the `wirkd-status` action and the status pane — read the estate the
/// operator configured, administratively. They must **name** that
/// scope. The verb's own default now follows the environment the
/// process is in, so a wrapper that says nothing is a wrapper whose
/// surface depends on which triple its pane inherited; the guard here
/// is what stops one edit from reintroducing the omitted default the
/// correction removed.
#[test]
fn the_operator_wrappers_name_the_scope_they_read_in() {
    let doc = manifest();
    let program = |command: &Value| -> String {
        command
            .as_array()
            .expect("command is an array")
            .iter()
            .map(|part| part.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let status_action = doc["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .find(|action| action["id"].as_str() == Some("wirkd-status"))
        .expect("the wirkd-status action");
    let status_pane = doc["panes"]
        .as_array()
        .expect("panes")
        .iter()
        .find(|pane| pane["id"].as_str() == Some("status"))
        .expect("the status pane");

    for (what, command) in [
        ("the wirkd-status action", &status_action["command"]),
        ("the status pane", &status_pane["command"]),
    ] {
        let text = program(command);
        assert!(
            text.contains("--admin"),
            "{what} must name the administrative scope it reads in: {text}"
        );
    }
}

/// A relative `CARGO_TARGET_DIR` must resolve to the same place
/// regardless of which process (the install-time build, later a
/// startup hook or action) calls the shared helper, and regardless of
/// what that process's own working directory happens to be — a bare
/// `"$CARGO_TARGET_DIR/release/wirk"` is only right when the caller's
/// cwd happens to already be the plugin root, which is true for
/// `build.sh` and every action pane by construction but not something
/// this function itself may assume.
#[test]
fn a_relative_cargo_target_dir_is_anchored_to_the_plugin_root_not_the_caller_cwd() {
    let (_guard, root) = repo_root();
    let helper = root.join("plugin/wirk-bin.sh");
    let elsewhere = root.parent().expect("repo_root has a parent").to_path_buf();

    let target_dir = |cwd: &std::path::Path| -> String {
        let output = Command::new(bash())
            .arg("-c")
            .arg(format!(". {}; wirk_target_dir", helper.display()))
            .current_dir(cwd)
            .env("HERDR_PLUGIN_ROOT", &root)
            .env("CARGO_TARGET_DIR", "build-out")
            .output()
            .expect("bash runs the helper");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    let expected = root.join("build-out").display().to_string();
    assert_eq!(
        target_dir(&root),
        expected,
        "called from the plugin root itself"
    );
    assert_eq!(
        target_dir(&elsewhere),
        expected,
        "called from an unrelated cwd, the answer must not change"
    );

    // An absolute CARGO_TARGET_DIR is untouched either way — it already
    // names one place regardless of caller cwd.
    let output = Command::new(bash())
        .arg("-c")
        .arg(format!(". {}; wirk_target_dir", helper.display()))
        .current_dir(&elsewhere)
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("CARGO_TARGET_DIR", "/var/tmp/some-absolute-target")
        .output()
        .expect("bash runs the helper");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "/var/tmp/some-absolute-target",
        "an absolute CARGO_TARGET_DIR passes through unchanged"
    );
}

/// The build step must not silently repeat a several-minute source
/// build in place of an operator-named `WIRK_BIN_PATH` that turns out
/// not to be executable — that is exactly the "set but not executable
/// is an error, never a fall-through" contract `wirk_resolve_bin` and
/// `wirk_bin_explain_missing` already hold for every other entry point.
/// A fake `cargo` stands in so the test proves the *message*, not that
/// a build ran, and stays fast either way.
#[test]
fn the_build_step_names_an_invalid_wirk_bin_path_rather_than_building_over_it_silently() {
    let (_guard, root) = repo_root();
    let fake_bin_dir = root.join("fakebin");
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let fake_cargo = fake_bin_dir.join("cargo");
    std::fs::write(
        &fake_cargo,
        format!(
            "#!/bin/sh\nmkdir -p \"$(dirname \"$0\"/../../target/release)\" 2>/dev/null\n\
             out=\"{}/target/release/wirk\"\nmkdir -p \"$(dirname \"$out\")\"\n\
             printf '#!/bin/sh\\necho \"usage: wirk <command>\"\\nexit 2\\n' >\"$out\"\n\
             chmod +x \"$out\"\nexit 0\n",
            root.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = Command::new(bash())
        .arg(root.join("plugin/build.sh"))
        .env("HERDR_PLUGIN_ROOT", &root)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("WIRK_BIN_PATH", "/nonexistent/not-a-real-wirk")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("bash runs build.sh");

    assert!(
        output.status.success(),
        "an invalid WIRK_BIN_PATH must not fail the build, only be ignored loudly: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("WIRK_BIN_PATH")
            && text.contains("/nonexistent/not-a-real-wirk")
            && text.contains("not an executable"),
        "must name the invalid value rather than silently building over it: {text}"
    );
}

/// `assistant.sh` cannot resolve its own harness's first-run trust
/// prompt, but a start failure that is actually that prompt must not
/// read the same as any other launch failure — the operator needs to
/// know to go look at the pane, not reinstall anything. A fake `herdr`
/// reproduces the exact error shape `wait_for_named_agent` returns
/// (refs/herdr-0.9.0 src/cli/agent.rs: `agent_not_ready` / "blocked
/// during startup") so this stays a deterministic unit test.
#[test]
fn assistant_names_a_blocked_startup_as_a_pane_to_go_look_at() {
    let (_guard, root) = repo_root();
    let fake_bin_dir = root.join("fakebin");
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let fake_herdr = fake_bin_dir.join("herdr");
    std::fs::write(
        &fake_herdr,
        r#"#!/bin/sh
case "$1 $2" in
    "tab create")
        echo '{"tab_id":"t1","pane_id":"w1:p2"}'
        exit 0
        ;;
    "agent start")
        echo '{"error":{"code":"agent_not_ready","message":"agent wirk is blocked during startup and is not ready for prompts"}}' >&2
        exit 1
        ;;
    "agent get")
        exit 1
        ;;
esac
exit 0
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    install_real_wirk(&root);
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("estate"), root.display().to_string()).unwrap();
    std::fs::write(config_dir.join("harness"), "claude\n").unwrap();

    let output = Command::new(bash())
        .arg(root.join("plugin/assistant.sh"))
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("WIRK_BIN_PATH")
        .env_remove("CARGO_TARGET_DIR")
        // This is the one test here that actually shells out to
        // "$HERDR", so it is the one that must not inherit an ambient
        // real herdr session: an ambient HERDR_BIN_PATH bypasses PATH
        // entirely (`HERDR="${HERDR_BIN_PATH:-herdr}"`), and this
        // process runs inside exactly such a session. Every plugin-
        // session variable a real one sets is scrubbed, matching what
        // assistant.sh's own pane launch clears for the triple.
        .env_remove("HERDR_BIN_PATH")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("HERDR_ENV")
        .output()
        .expect("bash runs assistant.sh");

    assert!(
        !output.status.success(),
        "a blocked start must not be reported as a conversation opened"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("first-run prompt") && text.contains("w1:p2"),
        "must point at the pane and name it as a first-run prompt, not a bare error dump: {text}"
    );
}

/// The scaffolding the harness-argument tests share: a fake `herdr` on
/// `PATH` that answers the three calls `assistant.sh` makes and appends
/// its own `agent start` argv to a log **one argument per line**, which
/// is the only shape that can tell `be brief * always` (one argument)
/// apart from four. A real `wirk` binary is planted where
/// `plugin/wirk-bin.sh` looks so resolution succeeds on its own merits.
/// Runs `plugin/browser.sh` the way a Herdr plugin action invokes it:
/// no terminal on stdin, the Herdr server's own environment, and the
/// invoking pane's context in `HERDR_PLUGIN_CONTEXT_JSON`. Returns what
/// the script asked Herdr to put in the new pane's `WIRK_WORK_ID`.
fn browser_entry_work(
    root: &std::path::Path,
    fake_bin_dir: &std::path::Path,
    context_json: &str,
    inherited_work: Option<&str>,
) -> String {
    let argv_log = root.join("tab-argv.log");
    // One call per log: this helper is called more than once per test,
    // and a shared append log would answer the second call with the
    // first call's argv.
    let _ = std::fs::remove_file(&argv_log);
    let fake_herdr = fake_bin_dir.join("herdr");
    std::fs::write(
        &fake_herdr,
        format!(
            r#"#!/bin/sh
if [ "$1 $2" = "tab create" ]; then
    printf '%s\n' "$@" >>"{log}"
    echo '{{"tab_id":"t1","pane_id":"w1:p2"}}'
    exit 0
fi
exit 0
"#,
            log = argv_log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cmd = Command::new(bash());
    cmd.arg(root.join("plugin/browser.sh"))
        .stdin(std::process::Stdio::null())
        .env("HERDR_PLUGIN_ROOT", root)
        .env("HERDR_PLUGIN_CONFIG_DIR", root.join("config"))
        .env("HERDR_PLUGIN_CONTEXT_JSON", context_json)
        .env("HERDR_BIN_PATH", &fake_herdr)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("WIRK_BIN_PATH")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_RUN_ID");
    match inherited_work {
        Some(work) => cmd.env("WIRK_WORK_ID", work),
        None => cmd.env_remove("WIRK_WORK_ID"),
    };
    let out = cmd.output().expect("bash runs browser.sh");
    assert!(
        out.status.success(),
        "browser.sh failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = std::fs::read_to_string(&argv_log).unwrap_or_default();
    log.lines()
        .find_map(|line| line.strip_prefix("WIRK_WORK_ID=").map(str::to_string))
        .unwrap_or_default()
}

/// A Herdr plugin action does not receive the invoking pane's `WIRK_*`
/// (checked against herdr 0.9.0). It receives the *server's* — and on
/// this estate the Herdr server was itself started inside a Work, so
/// `WIRK_WORK_ID` is set, real, and the wrong Work. Preferring it
/// silently served someone else's Work and looked like it had worked.
///
/// The invoking pane decides. The inherited value is used only when
/// there is no plugin context at all: a person running this script
/// inside their own Work's pane.
#[test]
fn the_browser_entry_takes_its_work_from_the_pane_not_the_herdr_server() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, _log) = assistant_fake_herdr(&root);
    std::fs::create_dir_all(root.join("works/work-from-the-pane")).unwrap();
    std::fs::create_dir_all(root.join("works/work-the-server-had")).unwrap();
    let context = format!(
        r#"{{"focused_pane_cwd":"{}/worktrees/work-from-the-pane"}}"#,
        root.display()
    );

    let chosen = browser_entry_work(&root, &fake_bin_dir, &context, Some("work-the-server-had"));
    assert_eq!(
        chosen, "work-from-the-pane",
        "a stale WIRK_WORK_ID from the Herdr server won over the invoking pane"
    );

    // A pane that is not a Work's checkout is not quietly replaced by
    // whatever the server happened to carry either.
    let elsewhere = browser_entry_work(
        &root,
        &fake_bin_dir,
        r#"{"focused_pane_cwd":"/somewhere/else"}"#,
        Some("work-the-server-had"),
    );
    assert_eq!(
        elsewhere, "",
        "a pane outside the estate was served the server's own Work"
    );
}

/// Same fixture and the same fake `herdr` as `browser_entry_work`, but
/// varying `WIRK_ESTATE_ROOT` instead of `WIRK_WORK_ID` and reading back
/// what the script asked Herdr to put in the new pane's
/// `WIRK_ESTATE_ROOT`. `context_json` is `None` for a direct invocation
/// (no plugin context at all, `HERDR_PLUGIN_CONTEXT_JSON` unset) — both
/// still go through the no-terminal relaunch branch here, since neither
/// `stdin` is a tty under a test harness, and both log the same way.
fn browser_entry_estate(
    root: &std::path::Path,
    fake_bin_dir: &std::path::Path,
    context_json: Option<&str>,
    inherited_estate: Option<&str>,
) -> String {
    let argv_log = root.join("tab-argv.log");
    let _ = std::fs::remove_file(&argv_log);
    let fake_herdr = fake_bin_dir.join("herdr");
    std::fs::write(
        &fake_herdr,
        format!(
            r#"#!/bin/sh
if [ "$1 $2" = "tab create" ]; then
    printf '%s\n' "$@" >>"{log}"
    echo '{{"tab_id":"t1","pane_id":"w1:p2"}}'
    exit 0
fi
exit 0
"#,
            log = argv_log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cmd = Command::new(bash());
    cmd.arg(root.join("plugin/browser.sh"))
        .stdin(std::process::Stdio::null())
        .env("HERDR_PLUGIN_ROOT", root)
        .env("HERDR_PLUGIN_CONFIG_DIR", root.join("config"))
        .env("HERDR_BIN_PATH", &fake_herdr)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("WIRK_BIN_PATH")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    match context_json {
        Some(context) => cmd.env("HERDR_PLUGIN_CONTEXT_JSON", context),
        None => cmd.env_remove("HERDR_PLUGIN_CONTEXT_JSON"),
    };
    match inherited_estate {
        Some(estate) => cmd.env("WIRK_ESTATE_ROOT", estate),
        None => cmd.env_remove("WIRK_ESTATE_ROOT"),
    };
    let out = cmd.output().expect("bash runs browser.sh");
    assert!(
        out.status.success(),
        "browser.sh failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = std::fs::read_to_string(&argv_log).unwrap_or_default();
    log.lines()
        .find_map(|line| line.strip_prefix("WIRK_ESTATE_ROOT=").map(str::to_string))
        .unwrap_or_default()
}

/// `assistant_fake_herdr` writes `config/estate` as this
/// fixture's own `root` — the operator's configured estate. A plugin
/// action's Herdr *server* is, on this same estate, ordinarily started
/// inside some Work's own pane, so `WIRK_ESTATE_ROOT` in that server's
/// environment is real and set — just not the configured estate this
/// plugin action is supposed to act on. Preferring it silently sent this
/// action to serve a different estate than the one 'Configure Wirk' set
/// up, the same defect `the_browser_entry_takes_its_work_from_the_pane_
/// not_the_herdr_server` already pins for the Work id.
#[test]
fn the_browser_entry_takes_its_estate_from_configuration_not_the_herdr_server() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, _log) = assistant_fake_herdr(&root);
    let server_estate = root.join("some-other-estate-the-server-happened-to-be-started-in");
    std::fs::create_dir_all(&server_estate).unwrap();

    let chosen = browser_entry_estate(
        &root,
        &fake_bin_dir,
        Some(r#"{"focused_pane_cwd":"/somewhere/else"}"#),
        Some(&server_estate.display().to_string()),
    );
    assert_eq!(
        chosen,
        root.display().to_string(),
        "a server-inherited WIRK_ESTATE_ROOT overrode the operator's configured estate"
    );
}

/// Direct invocation is the other half: a person running this script
/// inside their own Work's pane (no `HERDR_PLUGIN_CONTEXT_JSON` at all)
/// carries a real, intentional `WIRK_ESTATE_ROOT`, and that must still
/// win over whatever `config/estate` happens to say.
#[test]
fn the_browser_entry_keeps_a_direct_invocations_own_estate() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, _log) = assistant_fake_herdr(&root);
    let direct_estate = root.join("the-actual-bound-estate");
    std::fs::create_dir_all(&direct_estate).unwrap();

    let chosen = browser_entry_estate(
        &root,
        &fake_bin_dir,
        None,
        Some(&direct_estate.display().to_string()),
    );
    assert_eq!(
        chosen,
        direct_estate.display().to_string(),
        "a direct invocation's own WIRK_ESTATE_ROOT must be used, not config/estate"
    );
}

/// `HERDR_PLUGIN_CONTEXT_JSON` is `serde_json::to_string` of Herdr's
/// own context struct, so a path with a quote or a backslash in it
/// arrives escaped. Matching up to the next `"` truncates it and then
/// names a different Work, or none.
#[test]
fn the_browser_entry_reads_an_escaped_pane_path() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, _log) = assistant_fake_herdr(&root);
    let odd = "work-\"quoted\"";
    std::fs::create_dir_all(root.join("works").join(odd)).unwrap();
    // Exactly what serde_json writes for that path.
    let context = format!(
        r#"{{"focused_pane_cwd":"{}/worktrees/work-\"quoted\"/sub"}}"#,
        root.display()
    );

    let chosen = browser_entry_work(&root, &fake_bin_dir, &context, None);
    assert_eq!(
        chosen, odd,
        "an escaped pane path was not read back as the path Herdr serialized"
    );
}

/// Puts the real wirk this test binary was built alongside at the path
/// an installation's own build step produces, so the plugin's resolver
/// passes it on its own `--help` merits.
///
/// Linked, never copied. Writing a file and then executing it from a
/// process that is also forking on other threads races the kernel's
/// ETXTBSY check: a spawn in flight anywhere in this binary inherits a
/// duplicate of the copy's write descriptor for the moment between its
/// fork and its exec, and an exec of that inode in that moment fails
/// with "Text file busy" (os error 26). The resolver reads that as a
/// candidate it cannot use, falls past it, and the entry point reports
/// no wirk at all -- a real 239 MB copy takes long enough to make that
/// overlap ordinary rather than exotic. A symlink never opens the
/// executed inode for writing, so the window does not exist.
fn install_real_wirk(root: &std::path::Path) {
    let wirk_bin = root.join("target/release/wirk");
    std::fs::create_dir_all(wirk_bin.parent().unwrap()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_wirk"), &wirk_bin).unwrap();
}

fn assistant_fake_herdr(root: &std::path::Path) -> (PathBuf, PathBuf) {
    let fake_bin_dir = root.join("fakebin");
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let argv_log = root.join("herdr-argv.log");
    let fake_herdr = fake_bin_dir.join("herdr");
    std::fs::write(
        &fake_herdr,
        format!(
            r#"#!/bin/sh
case "$1 $2" in
    "tab create")
        echo '{{"tab_id":"t1","pane_id":"w1:p2"}}'
        exit 0
        ;;
    "agent start")
        printf '%s\n' "$@" >>"{log}"
        exit 0
        ;;
    "agent get")
        exit 1
        ;;
    "agent prompt")
        exit 0
        ;;
esac
exit 0
"#,
            log = argv_log.display()
        ),
    )
    .unwrap();
    install_real_wirk(root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("estate"), root.display().to_string()).unwrap();
    std::fs::write(config_dir.join("harness"), "claude\n").unwrap();
    (fake_bin_dir, argv_log)
}

/// Runs `plugin/assistant.sh` against that fixture with the ambient
/// Herdr/cargo environment removed, so the test sees the fake and not
/// whatever session the developer happens to be sitting in.
fn run_assistant(
    root: &std::path::Path,
    fake_bin_dir: &std::path::Path,
    harness_args_env: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(bash());
    cmd.arg(root.join("plugin/assistant.sh"))
        .env("HERDR_PLUGIN_ROOT", root)
        .env("HERDR_PLUGIN_CONFIG_DIR", root.join("config"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("WIRK_BIN_PATH")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("HERDR_BIN_PATH")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("HERDR_ENV");
    match harness_args_env {
        Some(value) => cmd.env("WIRK_ASSISTANT_HARNESS_ARGS", value),
        None => cmd.env_remove("WIRK_ASSISTANT_HARNESS_ARGS"),
    };
    cmd.output().expect("bash runs assistant.sh")
}

/// Which model and effort a conversation runs at is the operator's
/// choice, and `assistant.sh` had no way to carry one: it called
/// `agent start` with a fixed argv, so the only lever was the harness's
/// own environment, which
/// cannot reach `--effort` at all.
///
/// The configured file is the answer, and its serialization is the
/// whole point of this test: **one argument per line, taken exactly as
/// written**. `be brief * always` must arrive as a single argv word
/// with the `*` intact. A `HARNESS_ARGS=( $VALUE )` unquoted expansion
/// -- the obvious way to write this -- passes the `--model`/`--effort`
/// case and fails exactly here: it splits that into three words and
/// then globs the `*` into a listing of the plugin root (observed:
/// `herdr-plugin.toml`, `plugin`). Comments and blank lines are skipped
/// so the file can say what it is.
#[test]
fn assistant_forwards_configured_harness_args_one_per_line() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, argv_log) = assistant_fake_herdr(&root);
    std::fs::write(
        root.join("config/harness-args"),
        "# this installation's own choice\n\n--model\nclaude-sonnet-5\n--effort\nmedium\n--append-system-prompt\nbe brief * always\n",
    )
    .unwrap();

    let output = run_assistant(&root, &fake_bin_dir, None);
    assert!(
        output.status.success(),
        "a successful start with forwarded args must be reported as opened: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let argv: Vec<String> = std::fs::read_to_string(&argv_log)
        .expect("fake herdr logged its agent-start argv")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        argv,
        vec![
            "agent",
            "start",
            "wirk",
            "--kind",
            "claude",
            "--pane",
            "w1:p2",
            "--",
            "--model",
            "claude-sonnet-5",
            "--effort",
            "medium",
            "--append-system-prompt",
            "be brief * always",
        ],
        "each configured line must reach Herdr as exactly one argument after the '--'"
    );
}

/// The environment variable is the single-run lever (a test, a CI job,
/// one launch) and wins over the configured file when both are set. It
/// is split on whitespace by `read -a` -- the shell's own splitting,
/// which does not also glob -- so a `*` in it stays a `*` rather than
/// becoming the plugin root's directory listing.
#[test]
fn assistant_environment_harness_args_win_and_are_split_without_globbing() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, argv_log) = assistant_fake_herdr(&root);
    std::fs::write(root.join("config/harness-args"), "--model\nfrom-the-file\n").unwrap();

    let output = run_assistant(
        &root,
        &fake_bin_dir,
        Some("--model claude-sonnet-5 --effort medium --tag *"),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let argv: Vec<String> = std::fs::read_to_string(&argv_log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        &argv[7..],
        [
            "--",
            "--model",
            "claude-sonnet-5",
            "--effort",
            "medium",
            "--tag",
            "*"
        ],
        "the environment must override the file, split on whitespace and not glob: {argv:?}"
    );
}

/// An operator who chose nothing must see no product default appear:
/// the call stays byte-for-byte what it always was, with no trailing
/// `--` and no invented model. A file that exists but only carries
/// comments is "chose nothing" too, not an empty selection.
#[test]
fn assistant_omits_the_double_dash_when_no_harness_args_are_chosen() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, argv_log) = assistant_fake_herdr(&root);
    std::fs::write(root.join("config/harness-args"), "# nothing chosen yet\n\n").unwrap();

    let output = run_assistant(&root, &fake_bin_dir, None);
    assert!(
        output.status.success(),
        "no chosen args must not break an ordinary start: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let argv: Vec<String> = std::fs::read_to_string(&argv_log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        argv,
        vec![
            "agent", "start", "wirk", "--kind", "claude", "--pane", "w1:p2"
        ],
        "with nothing chosen the call must be exactly what it always was"
    );
}

/// The file above has to be reachable without an editor, or it is a
/// development knob rather than a setting: `configure.sh` is the
/// 'Configure Wirk' action's own pane, and this pins that typing one
/// ordinary line there produces the one-argument-per-line file
/// `assistant.sh` reads.
///
/// `script` gives it the pty it requires -- the script puts itself in a
/// Herdr pane when stdin is not a terminal, so piping alone would test
/// the wrong branch. With no `HERDR_SOCKET_PATH` there is no harness
/// list to read, so the harness question is skipped and the answers are
/// the estate, the arguments, and the closing Enter.
#[test]
fn configure_writes_a_typed_harness_argument_line_one_per_line() {
    let (_guard, root) = repo_root();
    let (fake_bin_dir, _argv_log) = assistant_fake_herdr(&root);
    let estate = root.join("estate");
    std::fs::create_dir_all(&estate).unwrap();
    let config_dir = root.join("config");
    std::fs::remove_file(config_dir.join("harness-args")).ok();

    let output = Command::new("script")
        .arg("-qec")
        .arg(format!(
            "{} {}",
            bash().display(),
            root.join("plugin/configure.sh").display()
        ))
        .arg("/dev/null")
        .current_dir(&root)
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("WIRK_BIN_PATH", root.join("target/release/wirk"))
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("HERDR_BIN_PATH")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            let answers = format!(
                "{}\n--model claude-sonnet-5 --effort medium\n\n",
                estate.display()
            );
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(answers.as_bytes())?;
            child.wait_with_output()
        })
        .expect("script runs configure.sh on a pty");

    let written = std::fs::read_to_string(config_dir.join("harness-args")).unwrap_or_else(|e| {
        panic!(
            "configure.sh must write the arguments file: {e}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        written, "--model\nclaude-sonnet-5\n--effort\nmedium\n",
        "one typed line must be stored one argument per line, the shape assistant.sh reads"
    );
}

/// The rung order above says *which* candidate wins. This says what it
/// takes to be a candidate at all.
///
/// Before this check, "exists and is executable" was the whole test, so
/// `WIRK_BIN_PATH=/bin/echo` resolved happily and the manifest's
/// `claim` action -- `exec "$WIRK_BIN" claim` -- printed "claim" and
/// exited 0. An operator reading that saw a successful claim. Nothing
/// had been claimed. That is the case this pins: the resolver has to
/// ask whether a candidate can run the commands this plugin invokes,
/// not merely whether the file is runnable.
///
/// Deliberately not a version check. wirk has no --version or
/// build-identity output to compare against, and a stale wirk that
/// still lists all three verbs passes here -- the limit is real and is
/// named in the helper's own comment rather than answered with a
/// version framework this pre-1.0 product does not have.
#[test]
fn an_executable_that_cannot_run_this_plugins_commands_is_not_a_wirk() {
    let (_guard, root) = repo_root();
    let helper = root.join("plugin/wirk-bin.sh");
    let empty_path = root.join("emptypath");
    std::fs::create_dir_all(&empty_path).unwrap();

    let write_exec = |name: &str, body: &str| -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    };

    // Resolve with only an explicit WIRK_BIN_PATH available: no target
    // directory, nothing on PATH, so the answer is that candidate or
    // nothing at all.
    let resolve = |explicit: &std::path::Path| -> (bool, String) {
        let output = Command::new(bash())
            .arg("-c")
            .arg(format!(". {}; wirk_resolve_bin", helper.display()))
            .env("HERDR_PLUGIN_ROOT", &root)
            .env("PATH", empty_path.display().to_string())
            .env("WIRK_BIN_PATH", explicit.display().to_string())
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .expect("bash runs the helper");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )
    };

    // The exact shape that used to resolve: runnable, exits 0, is not
    // wirk and cannot answer for one.
    let silent = write_exec("silent-exit-zero", "#!/bin/sh\nexit 0\n");
    let (ok, out) = resolve(&silent);
    assert!(
        !ok && out.is_empty(),
        "an executable that answers nothing must not resolve as wirk: {out}"
    );

    // An echoing executable: the /bin/echo case, which made the claim
    // action print its own argument and exit 0.
    let echoes = write_exec("echoes", "#!/bin/sh\necho \"$@\"\n");
    let (ok, out) = resolve(&echoes);
    assert!(
        !ok && out.is_empty(),
        "an executable that echoes its arguments must not resolve as wirk: {out}"
    );

    // A wirk missing one of the verbs this plugin invokes. `plugin` is
    // the one `configure` and `assistant` both need; without it the
    // binary is a wirk this manifest still cannot use.
    let partial = write_exec(
        "wirk-without-plugin-verb",
        "#!/bin/sh\necho 'usage: wirk claim | wirk wirkd start' >&2\nexit 1\n",
    );
    let (ok, out) = resolve(&partial);
    assert!(
        !ok && out.is_empty(),
        "a wirk lacking a verb this plugin invokes must not resolve: {out}"
    );

    // And the real shape does resolve, so the check is a filter and not
    // a wall.
    let usable = write_exec(
        "usable-wirk",
        "#!/bin/sh\necho 'usage: wirk claim | wirk wirkd start|stop | wirk plugin show' >&2\nexit 1\n",
    );
    let (ok, out) = resolve(&usable);
    assert!(
        ok && out == usable.display().to_string(),
        "a binary offering every verb this plugin invokes must resolve: {out}"
    );
}

/// The build step decides whether an installation already has a wirk.
/// If it accepts one that cannot run the manifest's commands, the
/// install completes and registers a plugin whose actions are hollow --
/// the failure lands minutes later, on the operator, with no connection
/// back to the value that caused it.
#[test]
fn the_build_step_does_not_skip_the_build_for_an_unusable_executable() {
    let (_guard, root) = repo_root();
    let fake_bin = root.join("fakebin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    // A stand-in cargo, so this test pins the decision and not a
    // multi-minute real build.
    let cargo = fake_bin.join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\necho 'fake cargo' \"$@\"\nexit 0\n").unwrap();
    let not_wirk = root.join("runnable-but-not-wirk");
    std::fs::write(&not_wirk, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&cargo, &not_wirk] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    let output = Command::new(bash())
        .arg(root.join("plugin/build.sh"))
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("WIRK_BIN_PATH", not_wirk.display().to_string())
        .env(
            "CARGO_TARGET_DIR",
            root.join("target").display().to_string(),
        )
        .output()
        .expect("bash runs the build step");

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !text.contains("skipped"),
        "the build must not be skipped on an executable that is not a usable wirk: {text}"
    );
    assert!(
        text.contains("WIRK_BIN_PATH") && text.contains("ignoring it"),
        "the operator must be told which value was ignored and why: {text}"
    );
    assert!(
        text.contains("cargo build --release"),
        "the build must proceed instead of registering a hollow install: {text}"
    );
}

/// `harness-args` is the plugin's third configuration file, and
/// `wirk plugin init` owns writing it exactly as it owns `estate` and
/// `harness`. This pins the four things that ownership has to mean,
/// because each is a way an operator's stored choice could otherwise be
/// silently altered:
///
/// * every `--harness-arg` is stored verbatim, one per line, so an
///   argument holding a space stays one argument and a `*` is never
///   expanded against anything;
/// * repeating the flag replaces the whole list rather than appending,
///   so the file is what the operator last said in full;
/// * an `init` that does not mention arguments leaves them alone, so
///   changing the harness cannot quietly drop the model chosen for it;
/// * `--clear-harness-args` is the only way back to none, and saying it
///   is a different statement from saying nothing.
#[test]
fn plugin_init_owns_harness_arguments_verbatim_replacing_and_clearing() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    let dir = scratch("wirk-plugin-args-");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let init = |args: &[&str]| -> std::process::Output {
        Command::new(bin)
            .args(["plugin", "init"])
            .args(args)
            .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
            .output()
            .expect("wirk plugin init must run")
    };
    let show = || -> String {
        let out = Command::new(bin)
            .args(["plugin", "show"])
            .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
            .output()
            .expect("wirk plugin show must run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let stored = || std::fs::read_to_string(config_dir.join("harness-args")).ok();

    // A space inside one argument, and a glob that must not be expanded.
    let set = init(&[
        "--harness-arg",
        "--model",
        "--harness-arg",
        "claude-sonnet-5",
        "--harness-arg",
        "two words *.rs",
    ]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert_eq!(
        stored().as_deref(),
        Some("--model\nclaude-sonnet-5\ntwo words *.rs\n"),
        "each argument is one line, exactly as it was given"
    );
    assert!(
        show().contains("two words *.rs"),
        "show must read back the argument unchanged: {}",
        show()
    );

    // Writing an unrelated setting must not touch them.
    assert!(init(&["--harness", "codex"]).status.success());
    assert_eq!(
        stored().as_deref(),
        Some("--model\nclaude-sonnet-5\ntwo words *.rs\n"),
        "an init that named no arguments must leave the stored ones alone"
    );

    // Repeating replaces; it does not append.
    assert!(
        init(&["--harness-arg", "--effort", "--harness-arg", "medium"])
            .status
            .success()
    );
    assert_eq!(
        stored().as_deref(),
        Some("--effort\nmedium\n"),
        "a later init naming arguments replaces the whole list"
    );

    // Contradicting instructions are refused, not resolved by flag order.
    let both = init(&["--harness-arg", "--effort", "--clear-harness-args"]);
    assert_eq!(both.status.code(), Some(2));
    assert_eq!(
        stored().as_deref(),
        Some("--effort\nmedium\n"),
        "a refused init must not have written anything"
    );

    // Clearing is explicit, and is what "none" means.
    assert!(init(&["--clear-harness-args"]).status.success());
    assert_eq!(stored(), None);
    assert!(
        show().contains("harness-args (none)"),
        "show must name an unset list as unset: {}",
        show()
    );
}

/// An argument the one-line-per-argument format cannot represent must be
/// refused before anything is written, not accepted and then silently
/// dropped by the readers. Reproduces the exact contradiction root found
/// against the frozen CLI: `plugin init` reported an empty argument and
/// a `# heading` argument both written, but `plugin show` read back
/// neither.
#[test]
fn plugin_init_refuses_an_unrepresentable_harness_argument_before_writing() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    let dir = scratch("wirk-plugin-args-bad-");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let init = |args: &[&str]| -> std::process::Output {
        Command::new(bin)
            .args(["plugin", "init"])
            .args(args)
            .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
            .output()
            .expect("wirk plugin init must run")
    };
    let stored = || std::fs::read_to_string(config_dir.join("harness-args")).ok();

    for bad in ["", "# heading", "trailing carriage return\r"] {
        let out = init(&["--harness-arg", "ok-arg", "--harness-arg", bad]);
        assert!(
            !out.status.success(),
            "an unrepresentable argument {bad:?} must be refused, not accepted"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot be stored"),
            "must say the argument cannot be stored: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            stored(),
            None,
            "a refused init must not have written any of the arguments it was given, \
             including the ones before the bad one: {bad:?}"
        );
    }

    // A leading dash and an ordinary CR/LF-free value both survive; only
    // the shapes above are unrepresentable.
    let ok = init(&["--harness-arg", "-x", "--harness-arg", "--effort=medium"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(stored().as_deref(), Some("-x\n--effort=medium\n"));
}

/// A `plugin init` naming a valid setting alongside an invalid one must
/// refuse the whole command before writing either file. Order in the
/// argument list must not decide which half lands.
#[test]
fn plugin_init_refuses_the_whole_command_when_one_setting_is_invalid() {
    let bin = env!("CARGO_BIN_EXE_wirk");
    let dir = scratch("wirk-plugin-init-mixed-");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let estate = dir.path().join("estate");
    std::fs::create_dir_all(&estate).unwrap();

    let out = Command::new(bin)
        .args(["plugin", "init"])
        .arg("--estate")
        .arg(&estate)
        .args(["--harness", "two words"])
        .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
        .output()
        .expect("wirk plugin init must run");

    assert!(!out.status.success(), "must not exit 0");
    assert!(
        !config_dir.join("estate").exists(),
        "a valid --estate given alongside a refused --harness must not be \
         written: the command failed as a whole"
    );
    assert!(!config_dir.join("harness").exists());
}

/// The prompt's own promise. `configure.sh` offers three answers for the
/// arguments -- type some, press Enter, or type `-` -- and the two that
/// write nothing new have to mean different things: Enter keeps what is
/// stored, `-` sets none. Pinned because the prompt previously said
/// "Leave empty for none" while an empty answer in fact kept the
/// existing list, so an operator who wanted none and pressed Enter kept
/// a model they had asked to stop using.
#[test]
fn configure_keeps_harness_arguments_on_enter_and_clears_them_on_a_dash() {
    let answer_arguments_with = |typed: &str| -> Option<String> {
        let (_guard, root) = repo_root();
        let (fake_bin_dir, _argv_log) = assistant_fake_herdr(&root);
        let estate = root.join("estate");
        std::fs::create_dir_all(&estate).unwrap();
        let config_dir = root.join("config");
        std::fs::write(config_dir.join("harness-args"), "--model\nalready-chosen\n").unwrap();

        let output = Command::new("script")
            .arg("-qec")
            .arg(format!(
                "{} {}",
                bash().display(),
                root.join("plugin/configure.sh").display()
            ))
            .arg("/dev/null")
            .current_dir(&root)
            .env("HERDR_PLUGIN_ROOT", &root)
            .env("HERDR_PLUGIN_CONFIG_DIR", &config_dir)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fake_bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("WIRK_BIN_PATH", root.join("target/release/wirk"))
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("HERDR_BIN_PATH")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_SESSION")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                // The estate, then the arguments answer under test.
                let answers = format!("{}\n{typed}\n\n", estate.display());
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(answers.as_bytes())?;
                child.wait_with_output()
            })
            .expect("script runs configure.sh on a pty");
        assert!(
            output.status.success(),
            "configure.sh must exit 0: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        std::fs::read_to_string(config_dir.join("harness-args")).ok()
    };

    assert_eq!(
        answer_arguments_with("").as_deref(),
        Some("--model\nalready-chosen\n"),
        "an empty answer leaves the stored arguments exactly as they were"
    );
    assert_eq!(
        answer_arguments_with("-"),
        None,
        "a bare '-' is how the operator says none, and it really clears them"
    );
}
