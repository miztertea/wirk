//! `LiveHerdrSession`: a throwaway named Herdr session, started and torn
//! down by the test that owns it (0040 D127) — never the owner's
//! `default` session (AGENTS.md standing line). Started the exact shape
//! `p1-herdr-executor`'s tried steps and this item's own W1 tried step
//! used: `setsid herdr --session <name> server >/dev/null 2>&1 &
//! disown`, socket path polled bounded (issue 359: no sleep-as-wait, a
//! deadline poll loop), `Drop` stops and deletes the session and removes
//! the scratch repo even on panic (no `panic = "abort"` in this
//! workspace, so unwind reaches `Drop` — R1, nothing to build for that).
//!
//! Gated on `herdr` being on PATH (`tests.md` §3): `start` returns
//! `None` with a printed reason when it is not, rather than failing the
//! run outright — the common case (this box) never hits that branch,
//! and a future box that never installed Herdr reads as "passed", named
//! rather than hidden. `herdr_on_path` is the shared gate every
//! converted test (and `live_sweep.rs`) calls.

// `#[path]`-included into several independent test binaries (`tests.md`
// §2), each of which uses a different subset of this module's public
// surface — the same reason `wirk/src/executors/mod.rs` and `wirk/src/
// wirkd/mod.rs` already carry this exact attribute at their crate root
// (R2: the established answer to a shared `#[path]` module, not a new
// pattern).
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use wirk_herdr::{HerdrClient, SocketClient};

/// Bound on how long `start` polls for the session's socket to appear.
/// Generous for a live `herdr` server spawn on this box (P1's own tried
/// step observed "within 1s"); short enough that a genuinely wedged
/// `herdr` binary fails the test instead of hanging the suite.
const SESSION_START_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether `herdr` is on `PATH` at all — the gate every converted live
/// test (and `live_sweep.rs`) checks before doing anything else. `Ok`
/// from spawning `herdr --help` is enough; the exit status is not
/// examined (a nonzero exit from `--help` still proves the binary
/// exists and runs).
pub fn herdr_on_path() -> bool {
    Command::new("herdr").arg("--help").output().is_ok()
}

/// The shared early-return gate (`tests.md` §3): prints the standing
/// reason and returns `false` when `herdr` is not on PATH, so a test
/// that calls `if !support::require_herdr_or_skip("name") { return; }`
/// reads "passed" in `cargo test`'s summary rather than failing a box
/// that never installed Herdr.
pub fn require_herdr_or_skip(test_name: &str) -> bool {
    if herdr_on_path() {
        true
    } else {
        eprintln!("skip: {test_name}: herdr not on PATH");
        false
    }
}

fn herdr(args: &[&str]) -> std::process::Output {
    Command::new("herdr")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("herdr {args:?}: spawn failed: {e}"))
}

/// FNV-1a (R3-adjacent: a few lines of arithmetic, not a dependency)
/// over a test's own name, for a short, still-per-test session suffix
/// (see the comment at its one call site).
fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in s.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn session_socket_path(name: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set to locate the Herdr config dir");
    PathBuf::from(home)
        .join(".config")
        .join("herdr")
        .join("sessions")
        .join(name)
        .join("herdr.sock")
}

/// A throwaway named Herdr session (0040 D127), and — when a test needs
/// one — a throwaway git repository under the same scratch directory
/// (D127: "a real repository, started and torn down by the test").
/// Never the owner's `default` session.
pub struct LiveHerdrSession {
    name: String,
    socket: PathBuf,
    scratch: PathBuf,
}

impl LiveHerdrSession {
    /// Starts `setsid herdr --session <name> server >/dev/null 2>&1 &
    /// disown` (the exact form P1's and this item's own W1 tried steps
    /// used), polls the socket path bounded until it appears, then
    /// confirms the session actually answers (`herdr --session <name>
    /// api snapshot`, W1's own live-socket round trip — `herdr api` has
    /// no `ping` subcommand, 0028 tried step finding). Returns `None`,
    /// with a printed reason, when `herdr` is not on PATH (`tests.md`
    /// §3) — never a hard failure.
    pub fn start(test_name: &str) -> Option<Self> {
        if !require_herdr_or_skip(test_name) {
            return None;
        }

        // Short, not the test's own name: a Unix socket path
        // (`~/.config/herdr/sessions/<name>/herdr.sock`) is capped at
        // `sizeof(sockaddr_un.sun_path)` (~108 bytes on Linux), and this
        // crate's own test names run well past what leaves room for
        // that path — a first live run hit exactly this
        // (`InvalidInput: local socket name length exceeds capacity of
        // sun_path`). An 8-hex-digit FNV-1a hash of the test name stays
        // unique per test while short; the pid keeps two parallel
        // `cargo test` processes apart (never two threads of the same
        // process racing the same name: `test-threads` share one pid,
        // but each test's own name hashes to its own suffix).
        let name = format!("wirk-t{:08x}-{}", fnv1a(test_name), std::process::id());
        let scratch = PathBuf::from("/var/tmp").join(format!("{name}-scratch"));
        std::fs::create_dir_all(&scratch)
            .unwrap_or_else(|e| panic!("creating scratch dir {}: {e}", scratch.display()));

        let sh = format!(
            "setsid herdr --session {name} server >/var/tmp/{name}-server.log 2>&1 & disown"
        );
        let status = Command::new("bash")
            .arg("-c")
            .arg(&sh)
            .status()
            .unwrap_or_else(|e| panic!("spawning session {name}: {e}"));
        assert!(status.success(), "spawning session {name} failed");

        let socket = session_socket_path(&name);
        let deadline = Instant::now() + SESSION_START_TIMEOUT;
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "session {name}'s socket never appeared at {} within {SESSION_START_TIMEOUT:?}",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let snapshot = herdr(&["--session", &name, "api", "snapshot"]);
        assert!(
            snapshot.status.success(),
            "session {name} did not answer api snapshot: {}",
            String::from_utf8_lossy(&snapshot.stderr)
        );

        Some(LiveHerdrSession {
            name,
            socket,
            scratch,
        })
    }

    /// A fresh `SocketClient` dialed at this session's socket, with the
    /// fixture's own bounded read timeout.
    pub fn client(&self) -> SocketClient {
        SocketClient::connect(self.socket.clone())
            .unwrap_or_else(|e| panic!("connecting to session {}: {e:?}", self.name))
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// `git init` plus one commit under this session's own scratch
    /// directory (D127: "a real repository ... started and torn down by
    /// the test") — returns its path and HEAD sha.
    pub fn repo(&self) -> (PathBuf, String) {
        let repo = self.scratch.join("repo");
        std::fs::create_dir_all(&repo)
            .unwrap_or_else(|e| panic!("creating repo dir {}: {e}", repo.display()));
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "wirk-test@example.com"]);
        git(&repo, &["config", "user.name", "wirk test"]);
        std::fs::write(repo.join("a.txt"), "one\n").expect("write a.txt");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let sha = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        (repo, sha)
    }

    /// This session's own scratch directory (for a test that needs
    /// somewhere writable beyond `repo()`, e.g. an estate root).
    pub fn scratch_dir(&self) -> &std::path::Path {
        &self.scratch
    }
}

impl Drop for LiveHerdrSession {
    /// Closes every open workspace first (W1's own tried-step teardown
    /// shape, `05-teardown.log`: a workspace left open when the session
    /// stops was this item's own live finding for `agent_pane_busy` —
    /// a lingering pane from an unclosed workspace can still be
    /// "occupied" when the very next throwaway session reuses Herdr's
    /// own `w1:p2`-shaped numbering), then `herdr session stop <name>`,
    /// then `herdr session delete <name>`; asserts `herdr session list`
    /// no longer names it, then removes the scratch directory. Runs on
    /// panic unwind (no `panic = "abort"` in this workspace).
    fn drop(&mut self) {
        if let Ok(client) = SocketClient::connect(self.socket.clone())
            && let Ok(snapshot) = client.snapshot()
        {
            let workspace_ids: std::collections::BTreeSet<String> = snapshot
                .workspaces
                .iter()
                .map(|b| b.workspace_id.clone())
                .collect();
            for workspace_id in workspace_ids {
                let _ = client.close_workspace(wirk_herdr::CloseWorkspace { workspace_id });
            }
        }

        let _ = herdr(&["session", "stop", &self.name]);
        let _ = herdr(&["session", "delete", &self.name]);

        let list = herdr(&["session", "list"]);
        let listing = String::from_utf8_lossy(&list.stdout);
        assert!(
            !listing.contains(&self.name),
            "session {} still listed after delete: {listing}",
            self.name
        );

        let _ = std::fs::remove_dir_all(&self.scratch);
        let _ = std::fs::remove_file(format!("/var/tmp/{}-server.log", self.name));
    }
}

fn git(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
