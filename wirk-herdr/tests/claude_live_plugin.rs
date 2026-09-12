//! The claude Claim-hook plugin wirk actually writes, checked against
//! the **installed** claude's own manifest validator rather than against
//! wirk's idea of the format (ruling 0208, R4).
//!
//! `#[ignore]` by default: this needs claude on `PATH`, so the ordinary
//! suite stays deterministic and offline. Run it explicitly:
//!
//! ```text
//! cargo test -p wirk-herdr --test claude_live_plugin -- --ignored --nocapture
//! ```
//!
//! No model call is made and no session is created: `claude plugin
//! validate` reads a directory and exits. Nothing here writes under
//! `~/.claude`, and no settings file of the owner's, the repository's or
//! a launch's is read or touched — the directory under test is a
//! throwaway estate this test itself creates.

use tempfile::tempdir;
use wirk_herdr::claim_hook;

/// The exact bytes `write_claude_claim_plugin` emits are a plugin the
/// installed claude accepts, with **no warnings** under `--strict` —
/// which is what makes `--plugin-dir <that directory>` a delivery
/// mechanism rather than a hope. Measured green on claude 2.1.270,
/// 2026-09-12; the live half that no unit test can reach (the hooks
/// actually firing, and a launch's own `--settings` surviving beside
/// them) is the owned-pane control recorded in this stage's REPORT.
#[test]
#[ignore = "requires the installed claude CLI"]
fn the_written_plugin_is_one_the_installed_claude_validates_strictly() {
    let estate = tempdir().expect("estate tempdir");
    let exe = std::path::Path::new("/opt/a dir with $pecial 'chars'/wirk-renamed-probe");
    let dir = claim_hook::write_claude_claim_plugin(&estate.path().to_string_lossy(), "run-1", exe)
        .expect("write the claude Claim plugin");

    let output = std::process::Command::new("claude")
        .arg("plugin")
        .arg("validate")
        .arg(&dir)
        .arg("--json")
        .arg("--strict")
        .output()
        .expect("`claude plugin validate` runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{error}: {stdout}"));

    assert_eq!(
        report["success"], true,
        "the installed claude must accept this plugin as written: {report}"
    );
    assert_eq!(
        report["manifest"]["errors"].as_array().map(Vec::len),
        Some(0),
        "{report}"
    );
    assert_eq!(
        report["manifest"]["warnings"].as_array().map(Vec::len),
        Some(0),
        "--strict must be clean, so the runtime is never merely tolerating this: {report}"
    );
}
