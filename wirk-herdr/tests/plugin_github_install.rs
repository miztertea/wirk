//! `herdr plugin install`/`uninstall` of wirk's own GitHub-hosted
//! manifest (P2.7 W4; 0053 D158; `orient/integration.md` §0-§6;
//! `orient/build-brief.md` item 5). Closes the `plugin.*`-GitHub half
//! of `herdr-operation-map.md` row 31 with a real `git clone` against
//! `github.com`, never folded into the ordinary offline suite (a
//! network dependency Herdr's own presence is not, unlike Docker's
//! daemon — same shape as `wirk/tests/docker_executor.rs`'s
//! `WIRK_DOCKER_LIVE` gate, R2): `#[ignore]`d, gated on
//! `WIRK_PLUGIN_INSTALL_LIVE=1`, a printed skip reason when unset.
//!
//! **The owner's own `wirk` plugin registration is never touched.**
//! `plugin.*` registration is user-global (`plugins.mdx:196-198`,
//! confirmed live: `herdr plugin list` on this box names the owner's
//! `local:/home/miztertea/wirk-workspace/repos/wirk` link), so this
//! test isolates the *entire registry*, not just the session: the
//! throwaway server is started with `XDG_CONFIG_HOME`/`XDG_STATE_HOME`
//! pointed at a tempdir (Herdr's own `config_dir()`/`state_dir()`,
//! `refs/herdr/src/config/io.rs:30-42`, read those two variables ahead
//! of the platform default), which relocates `plugins.json`
//! (`refs/herdr/src/persist/plugin_registry.rs:11`,
//! `config_dir().join("plugins.json")`) and every plugin config/state
//! dir out from under `~/.config` and `~/.local/state` entirely. This
//! was measured live before writing this test (isolation measurement,
//! `w4/BUILD.md`): a session started this way shows zero plugins while
//! the ambient `herdr plugin list` (no override) still shows the
//! owner's `wirk` link, unchanged, throughout.
//!
//! `WIRK_BIN_PATH` is exported into the same server environment
//! (`orient/integration.md` §2): the managed GitHub checkout carries no
//! `[[build]]` table, so without it every action/pane `exec`s a
//! `target/debug/wirk` that was never built inside the clone.
//!
//! Teardown calls `plugin uninstall` (not `unlink` — only `uninstall`
//! removes the managed GitHub checkout, `plugins.mdx:206-207`) via an
//! `UninstallGuard` whose `Drop` runs even on panic (0040 D127), then
//! `LiveHerdrSession`'s own `Drop` stops and deletes the session.

#[path = "support/live_herdr.rs"]
mod live_herdr;

use std::path::PathBuf;

use live_herdr::LiveHerdrSession;
use serde_json::Value;

const PLUGIN_ID: &str = "wirk";
const GITHUB_SHORTHAND: &str = "miztertea/wirk";

fn live_install_enabled() -> bool {
    std::env::var("WIRK_PLUGIN_INSTALL_LIVE").as_deref() == Ok("1")
}

/// The ambient `herdr plugin list`, no env override — the owner's real,
/// default-session view of the registry, read before and after this
/// test's own isolated run to prove it never moved.
fn ambient_plugin_list() -> String {
    let output = std::process::Command::new("herdr")
        .args(["plugin", "list"])
        .output()
        .expect("ambient herdr plugin list spawns");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `Drop`-guards `herdr plugin uninstall <id>` against the isolated
/// session even on panic (0040 D127; `orient/integration.md` §3/§6:
/// registration is global, and only `uninstall`, not `unlink`, removes
/// the managed GitHub checkout under `~/.config/herdr/plugins/github/`
/// — inside the *isolated* `XDG_CONFIG_HOME` here, never the owner's).
struct UninstallGuard<'a> {
    session: &'a LiveHerdrSession,
    plugin_id: &'static str,
}

impl Drop for UninstallGuard<'_> {
    fn drop(&mut self) {
        let name = self.session.name().to_string();
        let _ = self
            .session
            .herdr(&["--session", &name, "plugin", "uninstall", self.plugin_id]);
    }
}

#[test]
#[ignore]
fn plugin_install_from_github_round_trips_through_uninstall() {
    if !live_install_enabled() {
        eprintln!(
            "skipped: set WIRK_PLUGIN_INSTALL_LIVE=1 to run (needs network, clones github.com/miztertea/wirk)"
        );
        return;
    }

    let ambient_before = ambient_plugin_list();

    // Isolation: a tempdir this test owns, never `~/.config` or
    // `~/.local/state` (measured live, `w4/BUILD.md`).
    let xdg_root = tempfile::Builder::new()
        .prefix("wirk-plugin-install-xdg-")
        .tempdir_in("/var/tmp")
        .expect("xdg tempdir under /var/tmp");
    let xdg_config = xdg_root.path().join("config").display().to_string();
    let xdg_state = xdg_root.path().join("state").display().to_string();
    let wirk_bin: PathBuf = ["/var/tmp", "wirk-target", "debug", "wirk"]
        .iter()
        .collect();
    assert!(
        wirk_bin.is_file(),
        "expected an already-built wirk at {} (cargo build --offline -p wirk first)",
        wirk_bin.display()
    );
    let wirk_bin_path = wirk_bin.display().to_string();

    let Some(session) = LiveHerdrSession::start_with_env(
        "plugin_install_from_github_round_trips_through_uninstall",
        &[
            ("XDG_CONFIG_HOME", &xdg_config),
            ("XDG_STATE_HOME", &xdg_state),
            ("WIRK_BIN_PATH", &wirk_bin_path),
        ],
    ) else {
        return; // herdr not on PATH — printed reason already given
    };
    let name = session.name().to_string();

    let before = session.herdr(&["--session", &name, "plugin", "list"]);
    assert!(before.status.success(), "plugin list (before) failed");
    let before_text = String::from_utf8_lossy(&before.stdout);
    assert!(
        !before_text.contains(PLUGIN_ID),
        "isolated session already shows a '{PLUGIN_ID}' plugin before install: {before_text}"
    );

    // The guard is created before the install call itself: a panic on
    // the install's own assertion below must still uninstall whatever
    // did land (0040 D127's teardown-even-on-panic requirement).
    let guard = UninstallGuard {
        session: &session,
        plugin_id: PLUGIN_ID,
    };

    let install = session.herdr(&[
        "--session",
        &name,
        "plugin",
        "install",
        GITHUB_SHORTHAND,
        "--yes",
    ]);
    assert!(
        install.status.success(),
        "plugin install {GITHUB_SHORTHAND} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let after_install = session.herdr(&["--session", &name, "plugin", "list", "--json"]);
    assert!(
        after_install.status.success(),
        "plugin list --json (after install) failed"
    );
    let json: Value = serde_json::from_slice(&after_install.stdout)
        .unwrap_or_else(|e| panic!("plugin list --json did not parse: {e}"));
    let plugins = json["result"]["plugins"]
        .as_array()
        .expect("result.plugins is an array");
    let wirk_entry = plugins
        .iter()
        .find(|p| p["plugin_id"] == PLUGIN_ID)
        .unwrap_or_else(|| panic!("no plugin_id={PLUGIN_ID:?} entry in {json}"));
    assert_eq!(
        wirk_entry["source"]["kind"], "github",
        "installed plugin's source.kind is not \"github\": {wirk_entry}"
    );
    assert_eq!(wirk_entry["enabled"], true, "installed plugin not enabled");

    drop(guard); // runs `plugin uninstall wirk` now, still inside the isolated session

    let after_uninstall = session.herdr(&["--session", &name, "plugin", "list"]);
    assert!(
        after_uninstall.status.success(),
        "plugin list (after uninstall) failed"
    );
    let after_text = String::from_utf8_lossy(&after_uninstall.stdout);
    assert!(
        !after_text.contains(PLUGIN_ID),
        "isolated session still shows '{PLUGIN_ID}' after uninstall: {after_text}"
    );

    let ambient_after = ambient_plugin_list();
    assert_eq!(
        ambient_before, ambient_after,
        "the owner's ambient (default-session) plugin registry changed during this run"
    );
}
