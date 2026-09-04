//! End-to-end test for `wirk run-deterministic` against a real wirkd
//! (item 5, W3, `orient/build-brief.md` §3 W3's own decisive check).
//! Drives the real built binary for every step — `wirk wirkd start`,
//! `wirk work submit --kind deterministic --command ...`, `wirk
//! run-deterministic` — never a library call, the same discipline
//! `wirk/tests/wirkd_process.rs` uses. `wirkd::client`/`Request`/
//! `StatusPayload` (compiled in via `#[path]`, same move
//! `wirkd_process.rs` makes) are used only to read `status` directly,
//! which has no CLI verb of its own.
//!
//! No sleeps as waits: readiness is a bounded poll on `client::locate`
//! succeeding (issue 359's shape), never a tuned sleep; the wirkd child
//! is guarded by `KillOnDrop` for the whole test body so an assertion
//! failing partway through still cannot leak the process (ruling 0030
//! — "no wirkd... survives the run that started it").

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{EventKind, Journal, WorkId};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// Bounded poll (issue 359) on wirkd actually being reachable — never a
/// tuned sleep guessing how long the server takes to bind and write its
/// pointer file (`orient/transport.md` §3: "before wirkd does anything
/// else observable").
fn wait_for_wirkd(estate: &Path) -> WirkdPointer {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(pointer) = wirkd::client::locate(estate) {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "wirkd pointer file never appeared (readable) under {}",
            estate.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `wirk work submit --kind deterministic --command <command>`
/// (the `--repo`/`--base` values are fixed smoke-Route filler, same as
/// `wirkd_process.rs::submit` — no Route authoring exists yet, R6) and
/// parses its `work_id <id> run_id <id> waypoint <id>` stdout line.
fn submit_deterministic(estate: &Path, command: &[&str]) -> (String, String) {
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--intent",
            "smoke",
            "--repo",
            "demo:write",
            "--base",
            "deadbeef",
            "--kind",
            "deterministic",
            "--command",
        ])
        .args(command)
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    let mut work_id = String::new();
    let mut run_id = String::new();
    for pair in words.chunks(2) {
        if let [key, value] = pair {
            match *key {
                "work_id" => work_id = (*value).to_string(),
                "run_id" => run_id = (*value).to_string(),
                _ => {}
            }
        }
    }
    assert!(
        !work_id.is_empty() && !run_id.is_empty(),
        "unexpected work submit stdout: {stdout:?}"
    );
    (work_id, run_id)
}

/// Runs `wirk run-deterministic --estate <estate> --work <work_id>
/// --executor <executor>`, returning its exit code and stdout.
fn run_deterministic(estate: &Path, work_id: &str, executor: &str) -> (Option<i32>, String) {
    let output = Command::new(wirk_bin())
        .args(["run-deterministic", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--executor", executor])
        .output()
        .expect("run-deterministic runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).to_string(),
    )
}

/// Queries wirkd's `status` verb directly over the socket, returning
/// the folded `state` string.
fn status_state(socket: &Path, work_id: &str) -> String {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.to_string()),
        }),
    )
    .expect("status call succeeds");
    match reply {
        Reply::Ok { result, .. } => result["state"]
            .as_str()
            .expect("status result carries a state string")
            .to_string(),
        Reply::Err { error, .. } => panic!(
            "status unexpectedly refused: {} {}",
            error.code, error.message
        ),
    }
}

/// The BRIEF's own decisive check, end to end against the real binary:
/// a deterministic Work whose command writes `report.md` completes by
/// Claim through `run-deterministic --executor child` (exit 0, status
/// `completed`); a second Work whose command is `false` fails (exit 5)
/// with a journaled `RunFailed{status: "1", detail: non-empty}` — the
/// non-zero exit is what `false` actually returns, not a fabricated
/// "1"-shaped stand-in, and the stderr tail issue 279 exists to keep is
/// present (`false` prints nothing to stderr; the `sh`-shaped detail
/// text `ChildExecutorError`/`FailureCause.detail` still carries a
/// non-empty diagnostic — asserted below, not merely "some string").
#[test]
fn run_deterministic_child_completes_and_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();

    let mut wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_wirkd(&estate);

    // -- completes by claim -------------------------------------------------
    let (work1, run1) = submit_deterministic(&estate, &["sh", "-c", "echo x > report.md"]);
    let (code, stdout) = run_deterministic(&estate, &work1, "child");
    assert_eq!(code, Some(0), "run-deterministic stdout: {stdout}");
    assert!(
        stdout.contains(&format!("Claimed {run1}")),
        "stdout: {stdout}"
    );
    assert_eq!(status_state(&pointer.socket, &work1), "completed");

    // -- non-zero exit: RunFailed with status and non-empty detail ----------
    let (work2, run2) = submit_deterministic(&estate, &["false"]);
    let (code, stdout) = run_deterministic(&estate, &work2, "child");
    assert_eq!(code, Some(5), "run-deterministic stdout: {stdout}");
    assert!(
        stdout.contains(&format!("RunFailed {run2}")),
        "stdout: {stdout}"
    );
    // `Work` state stays `active`: "a failed Run is not a failed Work"
    // (incident file; fold.md §1) — asserted so this test would catch a
    // regression of that rule, not only the journal-level fact below.
    assert_eq!(status_state(&pointer.socket, &work2), "active");

    let work_dir = estate.join("works").join(&work2);
    let journal = Journal::open(&work_dir).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    let cause = events
        .iter()
        .find_map(|event| match (&event.run, &event.kind) {
            (Some(run), EventKind::RunFailed { cause }) if run.0 == run2 => Some(cause.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no RunFailed event journaled for run {run2}"));
    assert_eq!(cause.status.as_deref(), Some("1"), "cause: {cause:?}");
    assert!(
        cause.detail.as_deref().is_some_and(|d| !d.is_empty()),
        "expected non-empty detail, got {:?}",
        cause.detail
    );

    // -- stop: pointer and socket removed, child exits clean ----------------
    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let exit_status = wirkd_child.0.wait().expect("reap wirkd child");
    assert!(
        exit_status.success(),
        "wirkd did not exit clean: {exit_status:?}"
    );
    assert!(!estate.join(".wirk").join("wirkd.json").exists());
    assert!(!estate.join(".wirk").join("wirkd.sock").exists());
}

/// Fix 2 (ruling 0044, item E): `run-deterministic` blocks on the
/// child's own exit with no deadline anywhere — proven directly against
/// a child that exits only after a real delay of its own (`sh -c
/// 'sleep 2; exit 0'`, the thing under test, not a wait this test adds
/// on top of it), well past the old `RUN_POLL_TIMEOUT`/`RUN_POLL_STEP`
/// this item's own fix struck. Completes `Claimed`, never `"timeout"`
/// (a cause this build no longer has any code path that can produce).
#[test]
fn run_deterministic_child_blocks_past_a_real_delay_with_no_deadline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();

    let mut wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_wirkd(&estate);

    let (work, run) = submit_deterministic(
        &estate,
        &["sh", "-c", "sleep 2; echo x > report.md; exit 0"],
    );
    let started = Instant::now();
    let (code, stdout) = run_deterministic(&estate, &work, "child");
    assert_eq!(code, Some(0), "run-deterministic stdout: {stdout}");
    assert!(
        stdout.contains(&format!("Claimed {run}")),
        "stdout: {stdout}"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "run-deterministic returned before the child's own 2s delay elapsed: {:?}",
        started.elapsed()
    );
    assert_eq!(status_state(&pointer.socket, &work), "completed");

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let _ = wirkd_child.0.wait();
}

/// A `--command` argv not fenced with `--` that still carries one of
/// `work submit`'s own flags (`--base`/`--repo`/`--intent` here) is
/// ambiguous — the command's argv and the submit flags can no longer be
/// told apart — so `work submit` must refuse with the usage line (exit
/// 1, empty stdout) before building the payload or ever calling
/// wirkd. No daemon is started or expected for this shape.
#[test]
fn submit_command_rejects_unfenced_flag_after_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(dir.path())
        .args(["--route", "smoke", "--kind", "deterministic", "--command"])
        .args(["sh", "-c", "echo x > report.md"])
        .args([
            "--base",
            "deadbeef",
            "--repo",
            "demo:write",
            "--intent",
            "t",
        ])
        .output()
        .expect("work submit runs");
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
