//! `herdr plugin install` of this repository, executed for real against
//! a local candidate repository.
//!
//! `herdr plugin install` takes only `<OWNER/REPO[/SUBDIR]>` and builds
//! the remote as `https://github.com/{owner}/{repo}.git`; full URLs, SSH
//! and local paths are refused (executed: `herdr plugin install --help`
//! on 0.9.0). So the only way to exercise the real install path —
//! Herdr's own `git init`/`fetch`/`checkout`, its `[[build]]` step, and
//! the registration that follows — without publishing anything is to
//! rewrite that one URL for the duration of the command. Git's
//! `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` do exactly
//! that, in the environment of the process and its children, touching no
//! config file anywhere.
//!
//! **This is local installation qualification, not proof that a private
//! GitHub repository installs.** It proves Herdr fetches, builds and
//! registers this manifest; it says nothing about credentials against
//! `github.com`, which only a real remote can answer.
//!
//! Network-free but expensive: the `[[build]]` step is a genuine
//! `cargo build --release` in a fresh checkout with its own `target/`,
//! which is minutes of CPU and hundreds of megabytes. So, like
//! `plugin_github_install.rs`, it is `#[ignore]`d and gated on its own
//! variable, with a printed skip reason when unset.
//!
//! **The owner's own plugin registration is never touched.** Plugin
//! registration is user-global, so this isolates the entire registry
//! with `XDG_CONFIG_HOME`/`XDG_STATE_HOME` pointed into a tempdir, the
//! same way `plugin_github_install.rs` does, and reads the ambient
//! `herdr plugin list` before and after to prove it did not move.

#[path = "support/live_herdr.rs"]
mod live_herdr;

use std::path::{Path, PathBuf};
use std::process::Command;

use live_herdr::LiveHerdrSession;
use serde_json::Value;

const PLUGIN_ID: &str = "wirk";
/// The shorthand the install is asked for. Nothing is fetched from
/// GitHub: the URL it expands to is rewritten to the local candidate
/// below. It only has to be a well-formed `owner/repo`.
const SHORTHAND: &str = "miztertea/wirk";
const REWRITTEN_URL: &str = "https://github.com/miztertea/wirk.git";

fn live_enabled() -> bool {
    std::env::var("WIRK_PLUGIN_LOCAL_INSTALL_LIVE").as_deref() == Ok("1")
}

/// This crate's repository root — the directory holding the manifest
/// under test. Used only to publish a candidate; a run from a tree that
/// has since gone away skips rather than fails.
fn repo_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .to_path_buf();
    root.join("herdr-plugin.toml").is_file().then_some(root)
}

fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

/// The ambient registry, read with no environment override — the
/// owner's own default-session view, compared before and after.
fn ambient_plugin_list() -> String {
    let output = Command::new("herdr")
        .args(["plugin", "list"])
        .output()
        .expect("ambient herdr plugin list spawns");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `Drop`-guards `herdr plugin uninstall` against the isolated session
/// even on panic: only `uninstall` removes the managed checkout, and it
/// is several hundred megabytes once the build step has run.
struct UninstallGuard<'a> {
    session: &'a LiveHerdrSession,
}

impl Drop for UninstallGuard<'_> {
    fn drop(&mut self) {
        let name = self.session.name().to_string();
        let _ = self
            .session
            .herdr(&["--session", &name, "plugin", "uninstall", PLUGIN_ID]);
    }
}

#[test]
#[ignore]
fn a_local_install_builds_its_own_binary_and_declares_a_configure_action() {
    if !live_enabled() {
        eprintln!(
            "skipped: set WIRK_PLUGIN_LOCAL_INSTALL_LIVE=1 to run \
             (runs a real cargo --release build in a fresh checkout: minutes, hundreds of MB)"
        );
        return;
    }
    let Some(root) = repo_root() else {
        eprintln!("skipped: this crate's repository root is not there to publish from");
        return;
    };

    let ambient_before = ambient_plugin_list();

    let scratch = tempfile::Builder::new()
        .prefix("wirk-local-install-")
        .tempdir_in("/var/tmp")
        .expect("scratch under /var/tmp");
    let candidate = scratch.path().join("candidate.git");
    let xdg_config = scratch.path().join("config");
    let xdg_state = scratch.path().join("state");

    // The candidate: this working tree's own HEAD, in a bare repository
    // of its own. Nothing is published and no remote is contacted.
    let init = git(scratch.path(), &["init", "--bare", "-q", "candidate.git"]);
    assert!(init.status.success(), "git init --bare failed");
    let push = git(
        &root,
        &[
            "push",
            "-q",
            candidate.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );
    assert!(
        push.status.success(),
        "publishing HEAD to the local candidate failed: {}",
        String::from_utf8_lossy(&push.stderr)
    );
    let symref = git(&candidate, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    assert!(symref.status.success(), "setting candidate HEAD failed");

    let xdg_config_s = xdg_config.display().to_string();
    let xdg_state_s = xdg_state.display().to_string();
    let rewrite_key = format!("url.file://{}.insteadOf", candidate.display());

    let Some(session) = LiveHerdrSession::start_with_env_unset(
        "a_local_install_builds_its_own_binary",
        &[
            ("XDG_CONFIG_HOME", &xdg_config_s),
            ("XDG_STATE_HOME", &xdg_state_s),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", &rewrite_key),
            ("GIT_CONFIG_VALUE_0", REWRITTEN_URL),
        ],
        // What a fresh installation must not inherit from whoever is
        // running the tests: a shared cargo target would put the built
        // binary somewhere no ordinary installation has, and an
        // explicit WIRK_BIN_PATH would skip the build entirely.
        &["CARGO_TARGET_DIR", "WIRK_BIN_PATH"],
    ) else {
        return; // herdr not on PATH — printed reason already given
    };
    let name = session.name().to_string();

    let before = session.herdr(&["--session", &name, "plugin", "list"]);
    assert!(before.status.success(), "plugin list (before) failed");
    assert!(
        !String::from_utf8_lossy(&before.stdout).contains(PLUGIN_ID),
        "the isolated session already shows a '{PLUGIN_ID}' plugin"
    );

    // Created before the install: a panic on any assertion below must
    // still remove the managed checkout the build step filled.
    let guard = UninstallGuard { session: &session };

    let install = session.herdr(&["--session", &name, "plugin", "install", SHORTHAND, "--yes"]);
    assert!(
        install.status.success(),
        "plugin install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );
    // Nothing is asserted about what the build printed. Herdr captures
    // a build command's stdout and stderr and reports them only when it
    // fails (0.9.0 `run_plugin_build_command`), so a successful install
    // shows the operator nothing the step said -- the artifact below is
    // the evidence that it ran, and it is the better evidence anyway.

    let listed = session.herdr(&["--session", &name, "plugin", "list", "--json"]);
    assert!(listed.status.success(), "plugin list --json failed");
    let json: Value = serde_json::from_slice(&listed.stdout)
        .unwrap_or_else(|e| panic!("plugin list --json did not parse: {e}"));
    let entry = json["result"]["plugins"]
        .as_array()
        .expect("result.plugins is an array")
        .iter()
        .find(|p| p["plugin_id"] == PLUGIN_ID)
        .unwrap_or_else(|| panic!("no plugin_id={PLUGIN_ID:?} entry in {json}"));
    assert_eq!(entry["enabled"], true, "installed plugin not enabled");

    // The point of the build step: a runnable binary inside the
    // installation, reached without WIRK_BIN_PATH and without anybody's
    // development tree.
    let plugin_root = PathBuf::from(
        entry["plugin_root"]
            .as_str()
            .unwrap_or_else(|| panic!("no plugin_root in {entry}")),
    );
    let built = plugin_root.join("target/release/wirk");
    assert!(
        built.is_file(),
        "the install produced no binary at {}",
        built.display()
    );
    let ran = Command::new(&built)
        .output()
        .unwrap_or_else(|e| panic!("running {}: {e}", built.display()));
    assert!(
        String::from_utf8_lossy(&ran.stderr).starts_with("usage: wirk "),
        "the binary the install produced does not run as wirk"
    );

    // And the way an operator configures it, reachable as an action
    // rather than as an instruction to run a command they cannot reach.
    let actions = session.herdr(&["--session", &name, "plugin", "action", "list"]);
    assert!(actions.status.success(), "plugin action list failed");
    let actions_text = String::from_utf8_lossy(&actions.stdout);
    assert!(
        actions_text.contains("\"action_id\":\"configure\""),
        "no configure action is declared: {actions_text}"
    );

    drop(guard); // uninstall now, still inside the isolated session

    let after = session.herdr(&["--session", &name, "plugin", "list"]);
    assert!(after.status.success(), "plugin list (after) failed");
    assert!(
        !String::from_utf8_lossy(&after.stdout).contains(PLUGIN_ID),
        "the plugin is still registered after uninstall"
    );

    assert_eq!(
        ambient_before,
        ambient_plugin_list(),
        "the owner's ambient (default-session) plugin registry changed during this run"
    );
}
