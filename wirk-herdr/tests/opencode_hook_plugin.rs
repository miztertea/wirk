//! P2.7 Wave 2: the wirk-owned opencode plugin's own *runtime logic*,
//! pinned by running its exported handler under a real Node against a
//! fake opencode `event` and a fake driver binary that records its
//! argv and env (`BUILD.md`'s stated fallback — neither `FakeHerdrClient`
//! nor `scripted_actor.sh` runs a real opencode process or loads a JS
//! plugin, so this is the ceiling of "deterministic" available for the
//! plugin's own body; the wave's tried step is a real opencode process
//! actually loading and firing it,
//! `knowledge/evidence/p2-plugin-surface-2026-09-05/
//! wave2-opencode-tried.ndjson`). `opencode_hook.rs` pins the
//! *delivery* mechanism (the file lands where `OPENCODE_CONFIG` says);
//! this file pins the delivered file's own dispatch.
//!
//! The fake driver is deliberately **not** named `wirk` and its
//! directory is **not** put on `PATH` — the shape
//! `native-progress-contract-use/HANDOFF.md` §1.4 proved live breaks
//! bare `execFile("wirk", …)` (Rule 4). The plugin must still reach it,
//! by the absolute path `wirk_claim_plugin_js` splices in.
//!
//! Skips (prints why, exit success — same posture as this suite's
//! other environment-gated live checks) when `node` is not on `PATH`;
//! every box this item was built and run on has it (opencode itself
//! needs a JS runtime to load any plugin at all).

use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use wirk_herdr::claim_hook::wirk_claim_plugin_js;

const FAKE_WIRK_SH: &str = include_str!("support/fake_wirk.sh");

const HARNESS_MJS: &str = r#"
import { WirkClaimPlugin } from "./plugin.mjs";

const hooks = await WirkClaimPlugin();
const scenario = process.argv[2];

if (scenario === "root-idle") {
  await hooks.event({
    event: { type: "session.idle", properties: { sessionID: "root-1" } },
  });
} else if (scenario === "child-idle-ignored") {
  // The plugin tracks a child session the same way
  // herdr-agent-state.js does: any event carrying properties.info with
  // a parentID marks that session's own id as a child, before its own
  // session.idle would otherwise fire the hook.
  await hooks.event({
    event: {
      type: "session.updated",
      properties: {
        sessionID: "child-1",
        info: { id: "child-1", parentID: "root-1" },
      },
    },
  });
  await hooks.event({
    event: { type: "session.idle", properties: { sessionID: "child-1" } },
  });
} else if (scenario === "other-event-ignored") {
  await hooks.event({
    event: { type: "session.error", properties: { sessionID: "root-1" } },
  });
} else {
  throw new Error(`unknown scenario: ${scenario}`);
}
"#;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Runs one scenario against the real plugin file under `node`, with a
/// fake driver binary named `wirk-renamed-probe` (never `wirk`) whose
/// directory is deliberately kept off `PATH` — the plugin must reach it
/// only through the absolute path baked into `plugin.mjs` by
/// `wirk_claim_plugin_js`. Returns the fake driver's record file's
/// contents if it ran, `None` if it did not (the file was never
/// written).
fn run_scenario(scenario: &str) -> Option<String> {
    let dir = tempfile::tempdir().expect("scenario tempdir");

    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let fake_wirk = bin_dir.join("wirk-renamed-probe");
    std::fs::write(&fake_wirk, FAKE_WIRK_SH).expect("write fake wirk");
    set_executable(&fake_wirk);

    std::fs::write(
        dir.path().join("plugin.mjs"),
        wirk_claim_plugin_js(&fake_wirk),
    )
    .expect("write plugin.mjs");
    std::fs::write(dir.path().join("harness.mjs"), HARNESS_MJS).expect("write harness.mjs");

    let record = dir.path().join("record.txt");

    let status = Command::new("node")
        .arg("harness.mjs")
        .arg(scenario)
        .current_dir(dir.path())
        .env("WIRK_FAKE_RECORD", &record)
        .env("WIRK_ESTATE_ROOT", "/estate/sentinel")
        .env("WIRK_WORK_ID", "work-sentinel")
        .env("WIRK_RUN_ID", "run-sentinel")
        .status()
        .expect("run node harness");
    assert!(
        status.success(),
        "node harness exited non-zero for {scenario}"
    );

    std::fs::read_to_string(&record).ok()
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .expect("stat fake wirk")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod fake wirk");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

/// Red before Rule 4 (`native-progress-contract-use/HANDOFF.md` §1.4):
/// with the fake driver renamed and off `PATH`, the old
/// `execFile("wirk", …)` body left the plugin unable to run it at all —
/// `record` was always `None`. Green: the absolute-path invocation
/// runs it regardless of its name or `PATH`.
#[test]
fn root_session_idle_runs_wirk_claim_with_the_panes_env() {
    if !node_available() {
        eprintln!("skip: node not on PATH");
        return;
    }
    let record = run_scenario("root-idle").expect("fake wirk must have run and recorded");
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{record}");
    assert!(record.starts_with("ARGV:claim\n"), "got: {record}");
    assert!(
        record.contains("WIRK_ESTATE_ROOT=/estate/sentinel"),
        "got: {record}"
    );
    assert!(
        record.contains("WIRK_WORK_ID=work-sentinel"),
        "got: {record}"
    );
    assert!(record.contains("WIRK_RUN_ID=run-sentinel"), "got: {record}");
}

#[test]
fn a_child_sessions_idle_never_fires_the_claim() {
    if !node_available() {
        eprintln!("skip: node not on PATH");
        return;
    }
    let record = run_scenario("child-idle-ignored");
    assert!(
        record.is_none(),
        "a child session's own idle must not run wirk claim, got: {record:?}"
    );
}

#[test]
fn a_non_idle_event_never_fires_the_claim() {
    if !node_available() {
        eprintln!("skip: node not on PATH");
        return;
    }
    let record = run_scenario("other-event-ignored");
    assert!(
        record.is_none(),
        "a non-idle event must not run wirk claim, got: {record:?}"
    );
}
