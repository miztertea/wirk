//! P2.7 Waves 2 and 3 (`orient/reorient.md` §6 item 1, R4): delivers
//! wirk's own Claim-filing hook to an actor's pane, per kind, with
//! **no write into the worktree and no write under `~/`**.
//!
//! **opencode** (Wave 2): an `OPENCODE_CONFIG` env var naming a
//! wirk-owned config file whose `"plugin"` array names a wirk-owned
//! plugin file by absolute path — `w2-probe.md`'s Mechanism 2, measured
//! live. Both the owner's global config
//! (`~/.config/opencode/opencode.json`, the `hecate` provider) and
//! Herdr's own installed global plugin
//! (`~/.config/opencode/plugins/herdr-agent-state.js`) keep loading
//! beside this (opencode merges config, "all hooks run in sequence" —
//! `w2-probe.md` §1/§2, confirmed live in a throwaway Herdr session).
//! The plugin itself (`WIRK_CLAIM_PLUGIN_JS`) is modelled on
//! `herdr-agent-state.js`'s own `"event"` dispatch: it runs `wirk
//! claim` (bare — W1 already teaches the binary to fill the Waypoint's
//! declared outputs from wirkd's own contract) when the *root*
//! session's `session.idle` fires, ignoring child sessions the same
//! way (`info.parentID` tracked in a `Set`), and does nothing else.
//!
//! **claude** (Wave 3, `build-brief.md` §6 item 1, `reorient.md` §C):
//! one `--settings <path>` argv element on the `claude` launch
//! (`start_actor_agent`) naming a wirk-owned settings JSON file, under
//! the same `.wirk`-under-the-estate-root scheme as opencode's config
//! above (`<estate>/.wirk/claude/<run_id>/settings.json`). Hook
//! entries merge across settings levels (`reorient.md` §C, "Hooks >
//! Managed Settings and Inheritance") so this file coexists with
//! whatever `~/.claude/settings.json` or the worktree's own
//! `.claude/settings.json` already declare; it declares a `Stop` hook
//! (`type: "command"`) running bare `wirk claim` and nothing else — no
//! permissions, no other hooks, no model (0054 D163a: no permission
//! policy is written by wirk).
//!
//! In both cases the hook fires unconditionally at turn end and lets
//! wirkd's validator judge — a refused claim is state the run loop
//! already acts on (0049, 0052, 0044); neither hook pre-checks
//! anything.
//!
//! `hook_installed_for` is the one predicate for "does this Run's
//! actor kind get wirk's own Claim-filing hook" — shared by the
//! delivery code below and the standing prompt
//! (`run_loop::compose_first_prompt`) so the two can never drift onto
//! different lists of kinds (P2.7 W2b).

use std::io;
use std::path::{Path, PathBuf};

/// Embedded at compile time (R3: `include_str!`, the same pattern
/// `scripted_actor.rs` already uses for a file that must ship with the
/// binary rather than be read from a path that may not exist at
/// runtime) so the plugin's content is part of the `wirk-herdr` crate,
/// never read from disk at launch time.
pub const WIRK_CLAIM_PLUGIN_JS: &str = include_str!("wirk-claim-plugin.js");

/// The env var this module's caller (`actor_pane`) sets on the
/// opencode actor's pane, naming the wirk-owned config file below.
pub const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG";

/// Where this module writes the plugin and its naming config, for one
/// Run: `<estate_root>/.wirk/opencode/<run_id>/`, the same `.wirk`
/// convention `wirkd::client::locate` already uses for the estate's
/// own pointer file (`wirk/src/wirkd/client.rs:82`) — under the estate
/// root wirk already owns, never the worktree (0050's boundary check
/// never sees it: it is not written under `actor.worktree_path` at
/// all) and never `~/`.
fn run_dir(estate_root: &str, run_id: &str) -> PathBuf {
    Path::new(estate_root)
        .join(".wirk")
        .join("opencode")
        .join(run_id)
}

/// Writes the plugin file and a config file naming it in the
/// `"plugin"` array, for `run_id` under `estate_root`. Idempotent
/// (fixed content, overwritten every call — a retry mints a fresh
/// `run_id`, ruling 0053, so this never collides across Runs) and
/// synchronous, matching `actor_pane`'s own style. Returns the config
/// file's absolute path, the value `OPENCODE_CONFIG_ENV` is set to.
pub fn write_wirk_claim_hook(estate_root: &str, run_id: &str) -> io::Result<PathBuf> {
    let dir = run_dir(estate_root, run_id);
    std::fs::create_dir_all(&dir)?;

    let plugin_path = dir.join("wirk-claim.js");
    std::fs::write(&plugin_path, WIRK_CLAIM_PLUGIN_JS)?;

    let config_path = dir.join("wirk-opencode-config.json");
    let config = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "plugin": [plugin_path.to_string_lossy()],
    });
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("plugin config is representable as JSON"),
    )?;

    Ok(config_path)
}

/// The one predicate for "does this Run's actor kind have wirk's own
/// Claim-filing hook installed" (P2.7 W2b, `tried/RESULT-w2.md`; W3
/// extends it to claude): today, exactly the set of kinds `actor_pane`/
/// `start_actor_agent` deliver a hook for (`wirk-herdr/src/lib.rs`).
/// Shared so the standing prompt (`run_loop::compose_first_prompt`) and
/// the hook delivery it describes can never drift onto two different
/// lists of kinds — R2, no new configuration.
pub fn hook_installed_for(kind: &wirk_core::ActorKind) -> bool {
    matches!(kind.0.as_str(), "opencode" | "claude")
}

/// Where this module writes claude's settings file, for one Run:
/// `<estate_root>/.wirk/claude/<run_id>/settings.json` — the same
/// `.wirk`-under-the-estate-root scheme `run_dir` above uses for
/// opencode, never the worktree and never `~/`. A pure path
/// computation (no I/O) so both the writer below and
/// `start_actor_agent`'s argv construction name the identical file
/// without threading a value between two separate calls.
pub fn claude_settings_path(estate_root: &str, run_id: &str) -> PathBuf {
    Path::new(estate_root)
        .join(".wirk")
        .join("claude")
        .join(run_id)
        .join("settings.json")
}

/// The claude settings file's content: a `Stop` hook (`type:
/// "command"`) running bare `wirk claim` (W1's flagless form asks
/// wirkd for the Waypoint's declared outputs itself) and nothing else
/// — no permissions, no other hooks, no model (0054 D163a). Shape
/// confirmed live, `reorient.md` §C / `claude --help`:
/// `{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"..."}]}]}}`.
pub fn claude_settings_json() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "Stop": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": "wirk claim"
                        }
                    ]
                }
            ]
        }
    })
}

/// Writes claude's settings file for `run_id` under `estate_root`.
/// Idempotent and synchronous, matching `write_wirk_claim_hook`'s own
/// style (a retry mints a fresh `run_id`, ruling 0053, so this never
/// collides across Runs). Returns the settings file's absolute path,
/// the value `start_actor_agent` appends after `--settings`.
pub fn write_claude_claim_hook(estate_root: &str, run_id: &str) -> io::Result<PathBuf> {
    let path = claude_settings_path(estate_root, run_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&claude_settings_json())
            .expect("claude settings are representable as JSON"),
    )?;
    Ok(path)
}
