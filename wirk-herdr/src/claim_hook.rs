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
//! The plugin itself (`wirk_claim_plugin_js`) is modelled on
//! `herdr-agent-state.js`'s own `"event"` dispatch: it runs the
//! driver's own binary, by absolute path, with `claim` (W1 already
//! teaches the binary to fill the Waypoint's declared outputs from
//! wirkd's own contract) when the *root* session's `session.idle`
//! fires, ignoring child sessions the same way (`info.parentID`
//! tracked in a `Set`), and does nothing else.
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
//! (`type: "command"`) running the driver's own binary, by absolute
//! path, with `claim` and nothing else — no permissions, no other
//! hooks, no model (0054 D163a: no permission policy is written by
//! wirk).
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
//!
//! **The hook invokes the driver's own binary by absolute path, not
//! the bare name `wirk`** (`native-progress-contract-use/HANDOFF.md`
//! §1.4, Rule 4 — proved live: 0050 D151 prepends the running binary's
//! *directory* to the actor pane's `PATH`, but this estate's own review
//! discipline preserves a candidate binary under a name that says which
//! commit it is, e.g. `wirk-96f5a6a-verify` — no file in that directory
//! is named `wirk`, so bare `wirk claim`/`execFile("wirk", …)` is
//! `command not found` every time a stage is driven by a preserved or
//! renamed binary). Both writers below take the driver's own
//! `std::env::current_exe()` (R3, stdlib — the same value
//! `wirk-herdr/src/lib.rs`'s `actor_pane` already reads for the `PATH`
//! prepend, threaded here instead of re-derived) and invoke it
//! directly: `execFile` (opencode) takes it as the literal command with
//! no shell involved, so no quoting is needed; the claude settings
//! file's command is a `sh -c` string, so it is POSIX single-quoted
//! (`shell_quote`) to survive spaces and shell metacharacters in the
//! path unchanged. No host symlink or `PATH` alias is installed for
//! this — `PATH`'s own prepend (D151) stays only for the actor's own
//! by-hand `wirk claim`.

use std::io;
use std::path::{Path, PathBuf};

/// Embedded at compile time (R3: `include_str!`, the same pattern
/// `scripted_actor.rs` already uses for a file that must ship with the
/// binary rather than be read from a path that may not exist at
/// runtime) as a **template**: `wirk_claim_plugin_js` substitutes the
/// one placeholder string literal with the driver's own absolute path,
/// JSON/JS-string-escaped (`serde_json::to_string` — a JSON string
/// literal is a valid JS string literal), before it is written per Run.
pub const WIRK_CLAIM_PLUGIN_JS_TEMPLATE: &str = include_str!("wirk-claim-plugin.js");

/// The exact substring `wirk_claim_plugin_js` replaces — a quoted JS
/// string literal, so the replacement (also a quoted string literal,
/// from `serde_json::to_string`) drops in without touching the
/// surrounding `const WIRK_CLAIM_BIN = …;` syntax around it.
const PLUGIN_BIN_PLACEHOLDER: &str = "\"__WIRK_CLAIM_BIN__\"";

/// The opencode plugin's content for one Run: the shipped template with
/// the driver's own absolute binary path spliced in as a JS string
/// literal, so `execFile(WIRK_CLAIM_BIN, …)` runs that exact binary
/// regardless of what it is named or what is on `PATH`.
pub fn wirk_claim_plugin_js(exe: &Path) -> String {
    let literal = serde_json::to_string(&exe.to_string_lossy())
        .expect("a path is representable as a JSON/JS string literal");
    let out = WIRK_CLAIM_PLUGIN_JS_TEMPLATE.replacen(PLUGIN_BIN_PLACEHOLDER, &literal, 1);
    debug_assert_ne!(
        out, WIRK_CLAIM_PLUGIN_JS_TEMPLATE,
        "the plugin template's placeholder must be present exactly once: {PLUGIN_BIN_PLACEHOLDER}"
    );
    out
}

/// POSIX single-quotes `path` for use inside a `sh -c "<command>"`
/// string: wrap in `'…'`, and replace any embedded `'` with `'\''`
/// (close the quote, an escaped literal quote, reopen). Nothing inside
/// single quotes is interpreted by a POSIX shell except this one escape
/// for the quote character itself, so this is safe for every byte a
/// path can hold — spaces, `$`, `` ` ``, `"`, `\`, glob characters — not
/// only the ones this estate happens to use today. R3: no crate adopted
/// for one function.
fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

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
/// (fixed content for a given `exe`, overwritten every call — a retry
/// mints a fresh `run_id`, ruling 0053, so this never collides across
/// Runs) and synchronous, matching `actor_pane`'s own style. `exe` is
/// the driver's own absolute binary path (`std::env::current_exe()`,
/// read once by the caller) — the plugin invokes exactly that binary,
/// never the bare name `wirk`. Returns the config file's absolute path,
/// the value `OPENCODE_CONFIG_ENV` is set to.
pub fn write_wirk_claim_hook(estate_root: &str, run_id: &str, exe: &Path) -> io::Result<PathBuf> {
    let dir = run_dir(estate_root, run_id);
    std::fs::create_dir_all(&dir)?;

    let plugin_path = dir.join("wirk-claim.js");
    std::fs::write(&plugin_path, wirk_claim_plugin_js(exe))?;

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
/// "command"`) running `<exe> claim` — `exe` shell-quoted
/// (`shell_quote`) so a path containing spaces or shell metacharacters
/// still names exactly one command, `claim` bare (W1's flagless form
/// asks wirkd for the Waypoint's declared outputs itself) — and nothing
/// else: no permissions, no other hooks, no model (0054 D163a). Shape
/// confirmed live, `reorient.md` §C / `claude --help`:
/// `{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"..."}]}]}}`.
pub fn claude_settings_json(exe: &Path) -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "Stop": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": format!("{} claim", shell_quote(exe))
                        }
                    ]
                }
            ]
        }
    })
}

/// Writes claude's settings file for `run_id` under `estate_root`.
/// Idempotent (for a given `exe`) and synchronous, matching
/// `write_wirk_claim_hook`'s own style (a retry mints a fresh `run_id`,
/// ruling 0053, so this never collides across Runs). `exe` is the
/// driver's own absolute binary path, threaded through to
/// `claude_settings_json`. Returns the settings file's absolute path,
/// the value `start_actor_agent` appends after `--settings`.
pub fn write_claude_claim_hook(estate_root: &str, run_id: &str, exe: &Path) -> io::Result<PathBuf> {
    let path = claude_settings_path(estate_root, run_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&claude_settings_json(exe))
            .expect("claude settings are representable as JSON"),
    )?;
    Ok(path)
}
