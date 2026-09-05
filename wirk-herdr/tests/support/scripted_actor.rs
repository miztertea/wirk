//! `ScriptedActor`: installs `scripted_actor.sh` (this same directory)
//! as an executable named `opencode` in a throwaway temp directory, and
//! writes a script file for it to follow (P2.5 W1, ruling 0049 D148).
//!
//! The test that owns one prepends `bin_dir()` to the *session's own
//! process* `PATH` before starting Herdr (`LiveHerdrSession::
//! start_with_env`, the test's own env injection for its pane, never a
//! real actor's PATH and never under `/var/tmp/wirk-target`) and sets
//! `WIRK_SCRIPTED_ACTOR_SCRIPT` to `script_path()` the same way, so
//! every pane in that session inherits both (`refs/herdr/src/pane.rs`'s
//! `CommandBuilder` never calls `env_clear`, so a pane's shell inherits
//! the launching `herdr server` process's own environment, exactly
//! like `PATH` already had to for `wirk` itself, 0050 D151). Herdr's
//! `agent.start{kind:"opencode"}` then resolves this program through
//! an ordinary shell `PATH` lookup (`refs/herdr/src/detect/mod.rs`
//! `interactive_agent_executable`), no product change needed.
//!
//! `#[path]`-included the same way `live_herdr.rs` already is (`tests.md`
//! §2) — several independent test binaries each pull in the subset
//! they need.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// The scripted actor's own source, embedded at compile time (R2: the
/// same `include_str!` pattern the plugin/route fixtures in this
/// workspace already use for a file that must ship with the test
/// binary rather than be read from a path that may not exist at
/// runtime).
const SCRIPTED_ACTOR_SH: &str = include_str!("scripted_actor.sh");

/// A throwaway directory holding the `opencode`-named program and the
/// script file it follows, torn down with the temp directory when the
/// owning test ends (its `TempDir` is kept alive by the caller holding
/// this struct for the test's own body).
pub struct ScriptedActor {
    dir: tempfile::TempDir,
}

impl ScriptedActor {
    /// Writes `scripted_actor.sh` as `<dir>/opencode` (mode 0755) and
    /// `steps` (one per line) as `<dir>/script.txt`. An empty `steps`
    /// slice is a valid, deliberate "idle forever" script (the quiet-
    /// pane test's own shape) — not a defect.
    pub fn install(steps: &[&str]) -> Self {
        let dir = tempfile::tempdir().expect("scripted actor temp dir");

        let program = dir.path().join("opencode");
        fs::write(&program, SCRIPTED_ACTOR_SH).expect("write scripted actor program");
        set_executable(&program);

        let script = dir.path().join("script.txt");
        let mut contents = steps.join("\n");
        if !contents.is_empty() {
            contents.push('\n');
        }
        fs::write(&script, contents).expect("write scripted actor script file");

        ScriptedActor { dir }
    }

    /// The directory to prepend to a session's `PATH` so Herdr's
    /// `agent.start{kind:"opencode"}` finds this program before any
    /// real `opencode` binary.
    pub fn bin_dir(&self) -> &Path {
        self.dir.path()
    }

    /// The script file's path, for `WIRK_SCRIPTED_ACTOR_SCRIPT`.
    pub fn script_path(&self) -> PathBuf {
        self.dir.path().join("script.txt")
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .expect("stat scripted actor program")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod scripted actor program");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {
    // R1: every box this suite runs on is unix (the reference hooks
    // this program copies are all `/bin/sh`, `refs/herdr/src/
    // integration/assets/*/herdr-agent-state.sh`); nothing to do on a
    // platform this suite never targets.
}
