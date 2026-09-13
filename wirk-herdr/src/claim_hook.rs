//! P2.7 Waves 2 and 3 (`orient/reorient.md` §6 item 1, R4): delivers
//! wirk's own Claim-filing hook to an actor's pane, per kind, with
//! **no write into the worktree and no write under `~/`**.
//!
//! **opencode** (Wave 2): an `OPENCODE_CONFIG_CONTENT` env var carrying
//! a wirk-owned configuration layer whose `"plugin"` array names a
//! wirk-owned plugin file by absolute path — `w2-probe.md`'s Mechanism 2, measured
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
//! **claude** (Wave 3, `build-brief.md` §6 item 1, `reorient.md` §C;
//! corrected by ruling 0208): one `--plugin-dir <dir>` argv element on
//! the `claude` launch (`start_actor_agent`) naming a wirk-owned plugin
//! directory, under the same `.wirk`-under-the-estate-root scheme as
//! opencode's config above
//! (`<estate>/.wirk/claude/<run_id>/wirk-claim-plugin/`). The plugin
//! declares a `Stop` hook (`type: "command"`) running the driver's own
//! binary, by absolute path, with `claim --automatic` and nothing
//! else — no
//! permissions, no other hooks, no commands, no agents, no MCP servers
//! (0054 D163a: no permission policy is written by wirk).
//!
//! It used to be one `--settings <path>` element, and that was a
//! measured configuration loss: `--settings` is a single slot claude
//! resolves **last-wins**, so wirk appending its own after a launch's
//! own discarded that launch's hooks and its `env` block silently
//! (0208; controls in `write_claude_claim_plugin`'s own doc).
//! `--plugin-dir` is claude's own *repeatable* session-scoped
//! mechanism, so wirk's hook is one more loaded plugin beside whatever
//! the launch, the repository or the user already configured, and
//! `--settings` is never touched at all.
//!
//! In both cases the hook fires unconditionally at turn end and lets
//! wirkd's validator judge — a refused claim is state the run loop
//! already acts on (0049, 0052, 0044); neither hook pre-checks
//! anything.
//!
//! Ruling 0257: both hooks now say *which* of the two they are, with
//! `claim --automatic`. That word is the only thing wirkd could not
//! otherwise recover — a hook-filed Claim and a hand-typed one were
//! byte-identical on the wire — and it is read at wirkd's own
//! serialized validation, never here: a hook that checked for a
//! standing question and then submitted anyway would simply have
//! moved the race into the gap between the two calls. The effect is
//! narrow: an automatic attempt no longer completes a Run whose own
//! `wirk claim --question` is still unanswered. Everything else is
//! unchanged, the actor's own deliberate `wirk claim` included — that
//! is still what finishes the Run, after an answer or without one.
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
//! no shell involved, so no quoting is needed; the claude plugin hook's
//! command is a `sh -c` string, so it is POSIX single-quoted
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
///
/// `pub(crate)` since P3 runtime-guidance: the standing prompt
/// (`run_loop::compose_first_prompt`) hands the actor the same absolute
/// pinned-`wirk` path as a command to type, and must quote it by the
/// identical rule the claude `Stop` hook already quotes it by — one
/// function, not a second spelling of the same escape.
pub(crate) fn shell_quote(path: &Path) -> String {
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

/// opencode's own per-launch config-**file** variable. It names a
/// single file, so wirk sets it only when the launch carries none and
/// the inline slot below is already taken — writing it over a value a
/// launch carries would shadow that file wholesale (`opencode_delivery`,
/// ruling 0204).
pub const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG";

/// The env var this module's caller (`actor_pane`) sets on the opencode
/// actor's pane: opencode's own **inline** config layer, carrying the
/// wirk overlay's exact bytes.
///
/// Measured on opencode 1.18.30, 2026-09-12, with `opencode debug
/// config`: `OPENCODE_CONFIG_CONTENT` is an independent layer that
/// composes with the global config *and* with an `OPENCODE_CONFIG` file
/// a launch already carries — all three `instructions` entries and both
/// `plugin` entries resolve together. Setting `OPENCODE_CONFIG` instead
/// would have replaced that per-launch file wholesale. This is
/// opencode's own layering (R4), not a merge wirk performs.
///
/// It names a single inline document, though, so it is no safer to
/// overwrite than the file variable is: a launch already carrying one
/// keeps it, and wirk takes the other slot or appends
/// (`opencode_delivery`, ruling 0204).
pub const OPENCODE_CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// What one opencode launch's overlay is: the file on disk (a readable
/// record, and the plugin it names lives beside it) and the exact
/// compact bytes handed to opencode through `OPENCODE_CONFIG_CONTENT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeOverlay {
    /// `<estate_root>/.wirk/opencode/<run_id>/wirk-opencode-config.json`.
    pub config_path: PathBuf,
    /// The same configuration as one line of JSON — no control
    /// characters, so it crosses the pane's environment the same way
    /// `PATH` already does.
    pub config_content: String,
}

/// Where this module writes the plugin and its naming config, for one
/// Run: `<estate_root>/.wirk/opencode/<run_id>/`, the same `.wirk`
/// convention `wirkd::client::locate` already uses for the estate's
/// own pointer file (`wirk/src/wirkd/client.rs:82`) — under the estate
/// root wirk already owns, never the worktree (0050's boundary check
/// never sees it: it is not written under `actor.worktree_path` at
/// all) and never `~/`.
///
/// `pub` (P4.5 first increment, ruling 0203): `wirk work clean`'s own
/// per-Run directory removal (`server::handle_clean`, a different
/// crate) names this exact directory rather than re-deriving the
/// `.wirk/opencode/<run_id>` join by hand — one layout owner, per this
/// module's own established pattern for `claude_plugin_dir` and
/// `run_wirk_bin_dir` below.
pub fn run_dir(estate_root: &str, run_id: &str) -> PathBuf {
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
/// never the bare name `wirk`. `contract`, when given, is the absolute
/// path of this Run's reserved worker contract, added to the overlay's
/// own `instructions` array (P4.1). Returns the overlay: the file's
/// absolute path, and the exact bytes `OPENCODE_CONFIG_CONTENT_ENV` is
/// set to.
pub fn write_wirk_claim_hook(
    estate_root: &str,
    run_id: &str,
    exe: &Path,
    contract: Option<&Path>,
) -> io::Result<OpencodeOverlay> {
    let dir = run_dir(estate_root, run_id);
    std::fs::create_dir_all(&dir)?;

    let plugin_path = dir.join("wirk-claim.js");
    std::fs::write(&plugin_path, wirk_claim_plugin_js(exe))?;

    let config_path = dir.join("wirk-opencode-config.json");
    let mut config = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "plugin": [plugin_path.to_string_lossy()],
    });
    // P4.1 (ruling 0202): opencode's own `instructions` key —
    // "Additional instruction files or patterns to include" — measured
    // to **concatenate** across an `OPENCODE_CONFIG` overlay rather than
    // replace, exactly as `plugin` above already does. So the shared
    // worker contract is one more absolute path in this same
    // wirk-owned file: no new mechanism, no new env var, and the
    // owner's own global `instructions` entries keep loading beside it.
    // wirk passes a path, never a glob, and never rewrites an array it
    // does not own.
    if let Some(contract) = contract {
        config["instructions"] = serde_json::json!([contract.to_string_lossy()]);
    }
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("plugin config is representable as JSON"),
    )?;

    // The delivered layer is the compact form of exactly this
    // configuration: one source of truth, and no newline to carry
    // through the pane's environment.
    let config_content =
        serde_json::to_string(&config).expect("plugin config is representable as JSON");

    Ok(OpencodeOverlay {
        config_path,
        config_content,
    })
}

/// The shell script wirk runs **inside a Herdr pane** to read the two
/// opencode per-launch configuration values as that pane actually has
/// them, and the three files it writes them to.
///
/// **Why this exists at all.** A Herdr pane's environment is the Herdr
/// *server's* environment, overlaid per key by the `env` map the caller
/// passes to `workspace.create`/`pane.split`. The wirk driver's own
/// environment is on neither path: measured 2026-09-12 against herdr
/// 0.9.0, a pane created with no `env` of its own carries the server's
/// `OPENCODE_CONFIG_CONTENT` verbatim, and a variable exported only in
/// the calling process never reaches the pane at all (ruling 0205;
/// `verify/VERIFY.md` §V9). So `std::env::var` in the driver answers a
/// different question than the one `opencode_delivery` asks, in both
/// directions, and this estate's own development server merely happens
/// to carry neither variable today.
///
/// **Why a probe pane, and what was checked first.** R4: herdr 0.9.0's
/// socket API exposes no pane environment to read — `env` appears in
/// `workspace.create`, `pane.split`, `tab.create` and
/// `plugin.pane.open` *parameters* only, and in no response;
/// `pane.get`, `pane.process_info`, `pane.layout`, `workspace.get`,
/// `session.snapshot` and `layout.export` were each run against a live
/// 0.9.0 server and none returns one. R2: the read therefore happens
/// through verbs wirk's own client already speaks — `pane.split`,
/// `pane.send_text`, `pane.close` — in a pane created with **the
/// actor's own base launch environment** (ruling 0208), so what it
/// reports is exactly what the actor's pane will have, in the inherited
/// half and in wirk's own half alike. No new Herdr requirement, no
/// environment scan, no read of another process, no dump of anything:
/// two named variables, two files. The protocol this constant is the
/// body of — a script, three files, a deadline and a re-send loop — is
/// **R7**, the minimum that works once R1-R5 have failed; the verbs it
/// travels through being pre-existing does not make it pre-existing
/// (0208).
///
/// The script is invoked as `sh <script> <content> <config> <done>`
/// rather than typed as shell syntax, so it does not depend on the
/// pane's own shell being POSIX. Each value is written raw to its own
/// file — no quoting, no escaping and no parsing of a value wirk does
/// not own — and `done` is written last, so its existence is the
/// completion signal. An unset variable and an empty one both produce
/// an empty file, which `opencode_delivery` already reads as a free
/// slot.
pub const OPENCODE_ENV_PROBE_SH: &str = "\
#!/bin/sh
# Written by wirk. Reports this pane's own opencode per-launch
# configuration values, so wirk can compose with them instead of
# replacing them. Reads two variables, writes three files, sets nothing.
printf '%s' \"${OPENCODE_CONFIG_CONTENT-}\" > \"$1\" || exit 1
printf '%s' \"${OPENCODE_CONFIG-}\" > \"$2\" || exit 1
printf 'ok' > \"$3\"
";

/// The probe's four wirk-owned paths, all under this Run's own
/// `run_dir` beside the overlay — never the worktree, never `~/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeEnvProbe {
    /// The script itself.
    pub script: PathBuf,
    /// Receives `OPENCODE_CONFIG_CONTENT` exactly as the pane has it.
    pub content: PathBuf,
    /// Receives `OPENCODE_CONFIG` exactly as the pane has it.
    pub config: PathBuf,
    /// Written last; its existence means the other two are complete.
    pub done: PathBuf,
}

impl OpencodeEnvProbe {
    /// The one command line wirk types into the probe pane, every path
    /// shell-quoted by the same `shell_quote` the claude `Stop` hook
    /// and the standing prompt already use.
    pub fn command(&self) -> String {
        format!(
            "sh {} {} {} {}\n",
            shell_quote(&self.script),
            shell_quote(&self.content),
            shell_quote(&self.config),
            shell_quote(&self.done)
        )
    }

    /// Removes the two value files and the completion marker, so a
    /// stale answer from an earlier attempt can never be read as this
    /// one's. Best effort: a file that is not there is already gone.
    pub fn clear(&self) {
        for path in [&self.content, &self.config, &self.done] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Reads the two values back. `Err` when either is unreadable —
    /// the caller must then disclose a fallback rather than guess.
    pub fn read(&self) -> io::Result<(String, String)> {
        Ok((
            std::fs::read_to_string(&self.content)?,
            std::fs::read_to_string(&self.config)?,
        ))
    }
}

/// Writes the probe script for `run_id` under `estate_root`, beside the
/// overlay `write_wirk_claim_hook` writes, and returns its paths.
/// Idempotent and synchronous, matching this module's own style.
pub fn write_opencode_env_probe(estate_root: &str, run_id: &str) -> io::Result<OpencodeEnvProbe> {
    let dir = run_dir(estate_root, run_id);
    std::fs::create_dir_all(&dir)?;
    let probe = OpencodeEnvProbe {
        script: dir.join("wirk-opencode-env-probe.sh"),
        content: dir.join("wirk-opencode-env-content"),
        config: dir.join("wirk-opencode-env-config"),
        done: dir.join("wirk-opencode-env-done"),
    };
    std::fs::write(&probe.script, OPENCODE_ENV_PROBE_SH)?;
    Ok(probe)
}

/// Which of opencode's two per-launch native configuration slots this
/// Run's overlay takes, and what goes in it.
///
/// opencode 1.18.30 reads three variables; two of them are per-launch
/// layers that compose with the owner's global config and with each
/// other: `OPENCODE_CONFIG` (one file) and `OPENCODE_CONFIG_CONTENT`
/// (one inline document). Each names exactly one thing, so wirk can
/// only take a slot the launch has left free — measured with `opencode
/// debug config`, 2026-09-12 (`REPAIR.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpencodeDelivery {
    /// The inline slot was free: `OPENCODE_CONFIG_CONTENT` carries
    /// wirk's own bytes, and `OPENCODE_CONFIG` is left alone.
    Content(String),
    /// The inline slot was taken and the file slot was free:
    /// `OPENCODE_CONFIG` names wirk's own overlay file, a second native
    /// layer beside the launch's inline one. No parsing, no merge.
    ConfigFile(PathBuf),
    /// Both slots were taken: `OPENCODE_CONFIG_CONTENT` carries the
    /// inherited document with wirk's own two entries **appended**.
    ComposedContent(String),
    /// Both slots were taken and the inherited inline document is
    /// outside the declared input scope below. Nothing is set, nothing
    /// the launch configured is lost, and the reason says what wirk
    /// therefore did not deliver.
    Unsupported(String),
}

/// Decides that slot from what the launch already carries.
///
/// **Where the two inherited values come from, stated plainly.** They
/// are read from *the pane the actor will actually run in*, by
/// `OPENCODE_ENV_PROBE_SH` running inside a short-lived pane created
/// the same way the actor's pane is — not from the driver's own
/// process environment, which is on neither path a pane's environment
/// comes from and which this function was previously given (ruling
/// 0205). Unlike `PATH` and `CARGO_TARGET_DIR`, which wirk *supplies*
/// to the pane from its own environment, these two are values the pane
/// already has and wirk must not lose. If the probe cannot be read the
/// caller sets neither variable and discloses a fallback: a launch
/// whose effective configuration is unknown is never overwritten on a
/// guess.
///
/// **Declared input scope for composition** (the `ComposedContent`
/// arm, the only arm that reads the inherited bytes at all): the
/// inherited value parses as a JSON *object*, and `plugin` and
/// `instructions`, wherever present, are *arrays*. Then wirk appends
/// its own entries to those two arrays and touches nothing else —
/// every other key, including `$schema`, is carried through with the
/// value the launch wrote. Anything else is `Unsupported`. This is not
/// a config parser: wirk reads two array keys it owns entries in and
/// has no opinion about the rest of opencode's schema. (Keys are
/// re-serialised in `serde_json`'s map order, which is alphabetical
/// here; opencode is the only reader of these bytes and reads by key.)
pub fn opencode_delivery(
    overlay: &OpencodeOverlay,
    inherited_content: Option<&str>,
    inherited_config: Option<&str>,
) -> OpencodeDelivery {
    // A variable set to nothing is not a layer opencode can read, so
    // the slot counts as free.
    fn carried(value: Option<&str>) -> Option<&str> {
        value.filter(|v| !v.trim().is_empty())
    }
    let Some(inherited_content) = carried(inherited_content) else {
        return OpencodeDelivery::Content(overlay.config_content.clone());
    };
    if carried(inherited_config).is_none() {
        return OpencodeDelivery::ConfigFile(overlay.config_path.clone());
    }

    let unsupported = |what: &str| {
        OpencodeDelivery::Unsupported(format!(
            "this launch already carries both {OPENCODE_CONFIG_ENV} and \
             {OPENCODE_CONFIG_CONTENT_ENV}, and the inline value {what}, so wirk left both \
             alone rather than replace one: neither the worker contract nor wirk's own Claim \
             plugin is delivered to this pane natively"
        ))
    };
    let (Ok(inherited), Ok(mine)) = (
        serde_json::from_str::<serde_json::Value>(inherited_content),
        serde_json::from_str::<serde_json::Value>(&overlay.config_content),
    ) else {
        return unsupported("is not JSON");
    };
    let (Some(inherited), Some(mine)) = (inherited.as_object(), mine.as_object()) else {
        return unsupported("is not a JSON object");
    };

    let mut composed = inherited.clone();
    for key in ["plugin", "instructions"] {
        let Some(added) = mine.get(key).and_then(serde_json::Value::as_array) else {
            continue;
        };
        let entry = composed
            .entry(key.to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let Some(existing) = entry.as_array_mut() else {
            return unsupported(&format!("has a `{key}` that is not an array"));
        };
        existing.extend(added.iter().cloned());
    }

    OpencodeDelivery::ComposedContent(
        serde_json::to_string(&composed).expect("a JSON object is representable as JSON"),
    )
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

/// Where this module writes claude's own Claim-hook **plugin**, for one
/// Run: `<estate_root>/.wirk/claude/<run_id>/wirk-claim-plugin/` — the
/// same `.wirk`-under-the-estate-root scheme `run_dir` above uses for
/// opencode, never the worktree and never `~/`. A pure path computation
/// (no I/O) so both the writer below and `start_actor_agent`'s argv
/// construction name the identical directory without threading a value
/// between two separate calls.
pub fn claude_plugin_dir(estate_root: &str, run_id: &str) -> PathBuf {
    Path::new(estate_root)
        .join(".wirk")
        .join("claude")
        .join(run_id)
        .join("wirk-claim-plugin")
}

/// The plugin manifest claude reads at `<dir>/.claude-plugin/plugin.json`.
///
/// `hooks` names the hook file by plugin-relative path, which is what
/// makes `claude plugin validate --strict` accept the directory with no
/// warnings (measured below). Nothing else is declared: no permissions,
/// no commands, no agents, no MCP servers, no skills (0054 D163a — no
/// permission policy is written by wirk, and 0208 — nothing is
/// broadened).
pub fn claude_plugin_manifest_json() -> serde_json::Value {
    serde_json::json!({
        "name": "wirk-claim",
        "description": "Files this Run's wirk Claim when a turn ends.",
        "version": "1.0.0",
        "author": { "name": "wirk" },
        "hooks": "./hooks/hooks.json",
    })
}

/// The plugin's hook file: a `Stop` hook (`type: "command"`) running
/// `<exe> claim --automatic` — `exe` shell-quoted (`shell_quote`) so a
/// path containing spaces or shell metacharacters still names exactly
/// one command; no `--artifact`/`--output` (W1's flagless form asks
/// wirkd for the Waypoint's declared outputs itself) and `--automatic`
/// to state that a turn ended rather than that anyone decided
/// (ruling 0257) — and nothing else. Otherwise the same hook entry the
/// `--settings` file used to carry; only the envelope that delivers it
/// changed.
pub fn claude_plugin_hooks_json(exe: &Path) -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "Stop": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": format!("{} claim --automatic", shell_quote(exe))
                        }
                    ]
                }
            ]
        }
    })
}

/// Writes claude's Claim-hook plugin for `run_id` under `estate_root`
/// and returns its directory — the value `start_actor_agent` appends
/// after `--plugin-dir`. Idempotent (for a given `exe`) and synchronous,
/// matching `write_wirk_claim_hook`'s own style (a retry mints a fresh
/// `run_id`, ruling 0053, so this never collides across Runs).
///
/// **Why a plugin rather than the `--settings` file this replaced
/// (ruling 0208).** `--settings` is one slot and claude resolves it
/// last-wins: measured in owned Herdr panes on claude 2.1.270,
/// 2026-09-12, a launch's own `--settings A` fires its `SessionStart`
/// hook alone, and does **not** fire when wirk appends `--settings B`
/// after it — A's hooks and its `env` block are both silently
/// discarded. `--plugin-dir` is claude's own repeatable, additive
/// session-scoped mechanism (`claude --help`: "Load a plugin from a
/// directory or .zip for this session only … repeatable: --plugin-dir A
/// --plugin-dir B.zip"), and the same controls show a launch's own
/// `--settings` hook, its `env` block, its own `--plugin-dir` and
/// wirk's all take effect together. So wirk takes a slot that is not
/// exclusive instead of one that is, and never touches `--settings` at
/// all — nothing the launch, the repository or the user configured is
/// read, parsed, rewritten or replaced.
pub fn write_claude_claim_plugin(
    estate_root: &str,
    run_id: &str,
    exe: &Path,
) -> io::Result<PathBuf> {
    let dir = claude_plugin_dir(estate_root, run_id);
    let manifest_dir = dir.join(".claude-plugin");
    let hooks_dir = dir.join("hooks");
    std::fs::create_dir_all(&manifest_dir)?;
    std::fs::create_dir_all(&hooks_dir)?;
    std::fs::write(
        manifest_dir.join("plugin.json"),
        serde_json::to_vec_pretty(&claude_plugin_manifest_json())
            .expect("the plugin manifest is representable as JSON"),
    )?;
    std::fs::write(
        hooks_dir.join("hooks.json"),
        serde_json::to_vec_pretty(&claude_plugin_hooks_json(exe))
            .expect("the plugin's hooks are representable as JSON"),
    )?;
    Ok(dir)
}
