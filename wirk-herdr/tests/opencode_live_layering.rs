//! P4.1 (ruling 0204), against the **installed** opencode: the slot
//! `claim_hook::opencode_delivery` picks really does preserve what a
//! launch already configured, and the candidate's unconditional
//! `OPENCODE_CONFIG_CONTENT` write really does not.
//!
//! `opencode debug config` is a dry renderer: it resolves the
//! configuration layers and prints them, starting no session and making
//! no inference. `#[ignore]`, like `codex_live_composition.rs`, so the
//! ordinary suite stays offline; run with `-- --ignored`.
//!
//! The owner's own global config takes part (that is the point — the
//! global layer must survive too) and is never written to. Every
//! fixture is a throwaway file in this test's own tempdir, and no
//! `HOME` or `XDG_*` is redirected.

use std::path::Path;
use std::process::Command;

use tempfile::tempdir;
use wirk_herdr::claim_hook::{
    OPENCODE_CONFIG_CONTENT_ENV, OPENCODE_CONFIG_ENV, OpencodeDelivery, OpencodeOverlay,
    opencode_delivery,
};

/// What the launch already carries inline: two entries wirk owns arrays
/// for, one it does not, and one scalar.
fn launch_inline(dir: &Path) -> String {
    serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "instructions": [dir.join("launch-instructions.md").to_string_lossy()],
        "plugin": [format!("file://{}", dir.join("launch-plugin.js").display())],
        "permission": {"bash": "ask"},
        "small_model": "local-title/launch-chose-this",
    })
    .to_string()
}

fn overlay(dir: &Path) -> OpencodeOverlay {
    let config_path = dir.join("wirk-opencode-config.json");
    let config_content = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "plugin": [dir.join("wirk-claim.js").to_string_lossy()],
        "instructions": [dir.join("contract.md").to_string_lossy()],
    })
    .to_string();
    std::fs::write(&config_path, &config_content).expect("overlay file");
    OpencodeOverlay {
        config_path,
        config_content,
    }
}

fn fixtures(dir: &Path) {
    for (name, body) in [
        ("launch-instructions.md", "LAUNCH INSTRUCTIONS"),
        ("contract.md", "WIRK WORKER CONTRACT"),
        ("launch-plugin.js", "// launch plugin"),
        ("wirk-claim.js", "// wirk claim plugin"),
    ] {
        std::fs::write(dir.join(name), body).expect("fixture");
    }
}

/// Runs the installed renderer with exactly the two variables given.
fn resolved(env: &[(&str, String)], dir: &Path) -> serde_json::Value {
    let mut command = Command::new("opencode");
    command.arg("debug").arg("config").current_dir(dir);
    command.env_remove(OPENCODE_CONFIG_ENV);
    command.env_remove(OPENCODE_CONFIG_CONTENT_ENV);
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().expect("installed opencode runs");
    assert!(
        out.status.success(),
        "opencode debug config failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("debug config prints JSON")
}

fn joined(value: &serde_json::Value, key: &str) -> String {
    value[key].to_string()
}

#[test]
#[ignore = "requires the installed opencode binary"]
fn the_chosen_slot_preserves_the_launch_s_own_layer_where_an_overwrite_loses_it() {
    let dir = tempdir().expect("tempdir");
    let dir = dir.path();
    fixtures(dir);
    let overlay = overlay(dir);
    let inline = launch_inline(dir);

    // Baseline: the launch alone, no wirk. Everything it configured is
    // in the resolved configuration, beside the owner's global entries.
    let baseline = resolved(&[(OPENCODE_CONFIG_CONTENT_ENV, inline.clone())], dir);
    assert!(joined(&baseline, "instructions").contains("launch-instructions.md"));
    assert!(joined(&baseline, "plugin").contains("launch-plugin.js"));
    assert_eq!(baseline["permission"]["bash"], "ask");
    assert_eq!(baseline["small_model"], "local-title/launch-chose-this");

    // The counterexample: writing wirk's own bytes into the inline slot
    // the launch is using, as the candidate did unconditionally.
    let overwritten = resolved(
        &[(OPENCODE_CONFIG_CONTENT_ENV, overlay.config_content.clone())],
        dir,
    );
    assert!(
        !joined(&overwritten, "instructions").contains("launch-instructions.md")
            && !joined(&overwritten, "plugin").contains("launch-plugin.js")
            && overwritten["permission"]["bash"].is_null()
            && overwritten["small_model"] != "local-title/launch-chose-this",
        "an unconditional inline write is supposed to lose the launch's layer: {overwritten}"
    );

    // The correction, inline slot taken and the file slot free: wirk's
    // own file becomes opencode's second per-launch layer.
    let OpencodeDelivery::ConfigFile(path) = opencode_delivery(&overlay, Some(&inline), None)
    else {
        panic!("the free file slot is the decision for an inherited inline layer");
    };
    let both = resolved(
        &[
            (OPENCODE_CONFIG_CONTENT_ENV, inline.clone()),
            (OPENCODE_CONFIG_ENV, path.to_string_lossy().into_owned()),
        ],
        dir,
    );
    for (key, needle) in [
        ("instructions", "launch-instructions.md"),
        ("instructions", "contract.md"),
        ("plugin", "launch-plugin.js"),
        ("plugin", "wirk-claim.js"),
    ] {
        assert!(
            joined(&both, key).contains(needle),
            "{needle} missing from resolved {key}: {}",
            joined(&both, key)
        );
    }
    assert_eq!(both["permission"]["bash"], "ask");
    assert_eq!(both["small_model"], "local-title/launch-chose-this");
    assert!(joined(&both, "instructions").contains("/.config/opencode/"));

    // The correction, both slots taken: wirk's entries are appended to
    // the inherited document, and everything else in it is carried
    // through.
    let launch_file = dir.join("launch-config.json");
    std::fs::write(
        &launch_file,
        serde_json::json!({"instructions": [dir.join("launch-instructions.md").to_string_lossy()]})
            .to_string(),
    )
    .expect("launch config file");
    let OpencodeDelivery::ComposedContent(composed) = opencode_delivery(
        &overlay,
        Some(&inline),
        Some(&launch_file.to_string_lossy()),
    ) else {
        panic!("both slots taken composes into the inherited inline document");
    };
    let composed = resolved(
        &[
            (OPENCODE_CONFIG_CONTENT_ENV, composed),
            (
                OPENCODE_CONFIG_ENV,
                launch_file.to_string_lossy().into_owned(),
            ),
        ],
        dir,
    );
    for (key, needle) in [
        ("instructions", "launch-instructions.md"),
        ("instructions", "contract.md"),
        ("plugin", "launch-plugin.js"),
        ("plugin", "wirk-claim.js"),
    ] {
        assert!(
            joined(&composed, key).contains(needle),
            "{needle} missing from resolved {key}: {}",
            joined(&composed, key)
        );
    }
    assert_eq!(composed["permission"]["bash"], "ask");
    assert_eq!(composed["small_model"], "local-title/launch-chose-this");
}
