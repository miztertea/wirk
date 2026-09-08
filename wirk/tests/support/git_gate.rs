//! A test-side scheduler that parks the **real daemon** at a chosen
//! point inside its own production code, so a race can be *ordered*
//! rather than hoped for.
//!
//! Why this exists. `findings.rs`'s Application-currency races used to
//! be staggered sleeps: spawn the racer, sleep `n` milliseconds, fire
//! the concurrent transition, and hope the two landed on opposite sides
//! of the window. That is not a test (`CLAUDE.md`: "a test is
//! deterministic and has been watched fail, or it is not a test") — it
//! can lose the race under load and it can pass on a single lucky
//! sample. 0121 requires the same contract proved with deterministic
//! coordination across the real in-flight window.
//!
//! Why no daemon instrumentation. R2: nothing test-only already exists
//! in the product. R3/R4/R5: `wirkd` already shells out to the platform
//! `git` CLI (`Command::new("git")`, `wirk-atlas/src/git.rs`,
//! `server.rs::read_blob`) at points that sit *inside* both windows the
//! Application contract is about —
//!
//! * `atlas.resolve_exact`'s `git cat-file blob <before-oid>` runs after
//!   `handle_finding_applied`'s unlocked producer read and **before** it
//!   takes a single journal lock: the exact window a concurrent
//!   `fail`/`cancel`/`retry` used to slip through;
//! * `resolve_claim_attribution`'s `git cat-file -p <after-oid>` runs
//!   **under** every journal guard the append needs, between the
//!   re-derivation of the producer's authority and the append itself.
//!
//! A process's `PATH` is a native facility (R4), so putting a wrapper
//! that delegates to the real `git` in front of the daemon parks it at
//! either point with **no product change at all** — no new flag, no env
//! var read by wirk, nothing that can exist in a normal or release
//! runtime. The daemon under test is the shipped daemon; every `git`
//! call is the real `git`; only *when* one of them returns is the
//! test's to decide.
//!
//! It is a scheduler, not a fake (ruling 0040): it answers nothing
//! itself and changes no bytes.

// Included by `#[path]` into one test binary today; the module-level
// allow is `route_fixture.rs`'s own, same reason.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub struct GitGate {
    dir: PathBuf,
}

impl GitGate {
    /// Writes the wrapper into `dir/bin/git`, resolving the real `git`
    /// once, now, through the ambient `PATH` — the wrapper never looks
    /// itself up, so it cannot recurse.
    pub fn install(dir: &Path) -> GitGate {
        let real = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate git");
        let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
        assert!(
            !real.is_empty() && Path::new(&real).is_file(),
            "a real git is required on PATH for the daemon to be gated in front of"
        );
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            r#"#!/bin/sh
# Test-side scheduler (wirk/tests/support/git_gate.rs). Every call is
# the real git; a single call whose argv matches an armed pattern parks
# first, announces that it parked, and waits to be released.
GATE='{gate}'
if [ -s "$GATE/pattern" ] && [ -f "$GATE/arm" ]; then
  pat=$(cat "$GATE/pattern")
  case " $* " in
    *"$pat"*)
      if mv "$GATE/arm" "$GATE/consumed" 2>/dev/null; then
        : > "$GATE/parked"
        i=0
        while [ ! -e "$GATE/release" ] && [ "$i" -lt 6000 ]; do
          sleep 0.02
          i=$((i + 1))
        done
      fi
      ;;
  esac
fi
exec '{git}' "$@"
"#,
            gate = dir.display(),
            git = real,
        );
        let shim = bin.join("git");
        std::fs::write(&shim, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        GitGate {
            dir: dir.to_path_buf(),
        }
    }

    /// The `PATH` a gated process must run with: this gate's `bin`
    /// first, the ambient one behind it.
    pub fn path_env(&self) -> String {
        let inherited = std::env::var("PATH").unwrap_or_default();
        format!("{}:{inherited}", self.dir.join("bin").display())
    }

    /// Arms the gate for the next `git` invocation whose whole argv
    /// contains `pattern` verbatim. One-shot: the first matching call
    /// takes the gate (an atomic rename), every later call runs
    /// straight through.
    pub fn arm(&self, pattern: &str) {
        for name in ["parked", "release", "consumed"] {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
        std::fs::write(self.dir.join("pattern"), pattern).unwrap();
        std::fs::write(self.dir.join("arm"), "").unwrap();
    }

    /// Blocks until the daemon is actually parked at the armed call.
    /// A timeout here is a hard failure, never a skipped assertion: it
    /// means the code path under test never reached the window, so
    /// whatever the test asserts afterwards would prove nothing.
    pub fn wait_until_parked(&self, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.dir.join("parked").exists() {
            assert!(
                Instant::now() < deadline,
                "the daemon never reached the armed git call while {what}: the window under \
                 test was never entered, so nothing below is a proof"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Lets the parked call return. The returned instant is the causal
    /// reference point every "did this land before or after the window
    /// closed?" assertion is made against — a real observation, never a
    /// journal timestamp guess.
    pub fn release(&self) -> Instant {
        std::fs::write(self.dir.join("release"), "").unwrap();
        Instant::now()
    }

    pub fn is_parked(&self) -> bool {
        self.dir.join("parked").exists()
    }
}
