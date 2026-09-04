//! Process test for wirkd (W3, BRIEF.md decisive check; `orient/
//! transport.md` §7). Drives the real built binary
//! (`env!("CARGO_BIN_EXE_wirk")`) as a child process, never a library
//! call — the same discipline `wirk/tests/claim.rs` and
//! `journal_demo.rs` already use. `wirkd::client`/`Request`/`Reply`
//! (compiled in via `#[path]`, same move `wirkd_client.rs` makes) are
//! used only for the `status` verb, which has no CLI subcommand of its
//! own this wave — every other verb goes through `wirk wirkd`/`wirk
//! work`/`wirk claim`.
//!
//! Readiness is a bounded poll on the pointer file appearing (issue
//! 359), never a sleep standing in for "wirkd is up".

#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{Journal, WorkId};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// Bounded poll (issue 359: "convert the test to `wait_until`", never a
/// tuned sleep) for `<estate>/.wirk/wirkd.json` to exist — written only
/// after the listener is already bound (transport.md §3).
fn wait_for_pointer(estate: &Path) -> WirkdPointer {
    let path = estate.join(".wirk").join("wirkd.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(bytes) = fs::read(&path)
            && let Ok(pointer) = serde_json::from_slice::<WirkdPointer>(&bytes)
        {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "wirkd pointer file never appeared (readable) at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `wirk work submit --estate <estate> --route smoke --repo
/// <repo> --base main` (the estate's own copy of the canonical
/// `smoke.json` fixture, p2-route-files W2 — `--route` is required now
/// and no `--intent` exists any more, an Actor Waypoint's intent being
/// authored in the file) and parses its `work_id <id> run_id <id>
/// waypoint <id>` stdout line.
fn submit(estate: &Path, repo: &str) -> (String, String, String) {
    route_fixture::install_route_fixture(estate, "smoke");
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route", "smoke", "--repo", repo, "--base", "main"])
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
    let mut waypoint = String::new();
    for pair in words.chunks(2) {
        if let [key, value] = pair {
            match *key {
                "work_id" => work_id = (*value).to_string(),
                "run_id" => run_id = (*value).to_string(),
                "waypoint" => waypoint = (*value).to_string(),
                _ => {}
            }
        }
    }
    assert!(
        !work_id.is_empty() && !run_id.is_empty() && !waypoint.is_empty(),
        "unexpected work submit stdout: {stdout:?}"
    );
    (work_id, run_id, waypoint)
}

/// Runs `wirk claim` as a child with the triple in env (BRIEF.md's own
/// decisive-check shape) and the given extra args, returning its exit
/// code and stdout.
fn claim(estate: &Path, work_id: &str, run_id: &str, args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(wirk_bin())
        .arg("claim")
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .args(args)
        .output()
        .expect("wirk claim runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    )
}

/// Queries wirkd's `status` verb directly over the socket (no CLI
/// subcommand for it this wave) and returns the folded `state` string.
fn status(socket: &Path, work_id: &str) -> String {
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
/// `wirkd` started on a temp estate; a valid claim (accepted, journaled
/// `ClaimRecorded{Validated}`, status `completed`); a missing-artifact
/// claim (refused, status stays `active`); a fabricated-run-id claim
/// (`Refused(TripleMismatch)`, recorded not honored, status stays
/// `active`); two further Works claimed concurrently from two threads
/// with no interleaved journal line and a gap-free `seq`; `wirkd`
/// stopped, pointer and socket gone, the child reaped clean.
///
/// `KillOnDrop` guards the spawned `wirkd` child for the whole test body:
/// an assertion failing partway through must still not leak the process
/// (ruling 0030 — "no wirkd... survives the run that started it"), so
/// the guard's `Drop` sends a kill on any unwind, not only the success
/// path's own explicit `stop`.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn wirkd_process_lifecycle() {
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

    let pointer = wait_for_pointer(&estate);
    assert_eq!(pointer.protocol_version, 1);

    let ping = Command::new(wirk_bin())
        .args(["wirkd", "ping", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd ping runs");
    assert!(
        ping.status.success(),
        "ping failed: {}",
        String::from_utf8_lossy(&ping.stderr)
    );
    assert!(String::from_utf8_lossy(&ping.stdout).contains("protocol_version 1"));

    // -- valid claim: accepted, Run/Work complete ------------------------
    let (work1, run1, _waypoint1) = submit(&estate, "demo:write");
    fs::write(estate.join("report.md"), b"the report").expect("write report.md");
    let (code, stdout) = claim(
        &estate,
        &work1,
        &run1,
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(0), "valid claim stdout: {stdout}");
    assert_eq!(stdout, "Validated");
    assert_eq!(status(&pointer.socket, &work1), "completed");

    // -- missing artifact: refused, Run/Work stay open --------------------
    let (work2, run2, _waypoint2) = submit(&estate, "demo:write");
    let (code, stdout) = claim(&estate, &work2, &run2, &[]);
    assert_eq!(code, Some(3), "missing-artifact claim stdout: {stdout}");
    assert!(stdout.contains("MissingArtifact"), "stdout: {stdout}");
    assert_eq!(status(&pointer.socket, &work2), "active");

    // -- fabricated run id: refused and recorded, never honored -----------
    let (work3, _run3, _waypoint3) = submit(&estate, "demo:write");
    let (code, stdout) = claim(
        &estate,
        &work3,
        "01JFABRICATED0000000000000",
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(3), "fabricated-triple claim stdout: {stdout}");
    assert!(stdout.contains("TripleMismatch"), "stdout: {stdout}");
    assert_eq!(status(&pointer.socket, &work3), "active");

    // -- two Works claimed concurrently: no interleave, contiguous seq ----
    let (work4, run4, _) = submit(&estate, "demo:write");
    let (work5, run5, _) = submit(&estate, "demo:write");
    let estate_a = estate.clone();
    let (work4a, run4a) = (work4.clone(), run4.clone());
    let handle_a = std::thread::spawn(move || {
        claim(
            &estate_a,
            &work4a,
            &run4a,
            &["--artifact", "report.md=report.md"],
        )
    });
    let estate_b = estate.clone();
    let (work5b, run5b) = (work5.clone(), run5.clone());
    let handle_b = std::thread::spawn(move || {
        claim(
            &estate_b,
            &work5b,
            &run5b,
            &["--artifact", "report.md=report.md"],
        )
    });
    let (code_a, stdout_a) = handle_a.join().expect("claim thread a joins");
    let (code_b, stdout_b) = handle_b.join().expect("claim thread b joins");
    assert_eq!(code_a, Some(0), "concurrent claim a stdout: {stdout_a}");
    assert_eq!(code_b, Some(0), "concurrent claim b stdout: {stdout_b}");

    // `Journal::open`/`replay` fail closed on a sequence gap or a
    // malformed (torn/interleaved) line (0033 D101) — a clean replay of
    // each Work's own journal is the proof no interleave happened.
    for work_id in [&work4, &work5] {
        let work_dir = estate.join("works").join(work_id);
        let journal = Journal::open(&work_dir).expect("open journal");
        let events = journal
            .replay()
            .expect("journal replays cleanly: contiguous seq, no interleaved line");
        // submit journals 3 (WorkSubmitted, WaypointReserved, RunOpened),
        // claim journals 2 more (ClaimFiled, ClaimRecorded) — exactly 5.
        assert_eq!(
            events.len(),
            5,
            "expected 5 events for {work_id}, got {}",
            events.len()
        );
    }

    // -- stop: pointer and socket removed, child exits clean --------------
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

/// The 0034 follow-up (ruling 0034, "Follow-ups carried": "The on-disk
/// artifact check has no test that names an artifact whose file is
/// absent"): distinct from `wirkd_process_lifecycle`'s own
/// missing-artifact case (`work2`/`run2` above), which claims with
/// *no* `--artifact` at all and is refused by `validate_claim`'s own
/// declared-outputs check before the on-disk check in `handle_claim`
/// ever runs (`orient/validate.md` §3). Here the claim *names*
/// `report.md` — satisfying `validate_claim` — but the file is never
/// written to the reserved World's worktree path, so only the on-disk
/// existence check (build-brief amendment 3, `worktree_path_for_run`)
/// can catch it: `Refused(MissingArtifact("report.md"))`, exit 3, the
/// Run stays open (`active`).
#[test]
fn claim_names_an_artifact_whose_file_is_absent_on_disk() {
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
    let pointer = wait_for_pointer(&estate);

    let (work, run, _waypoint) = submit(&estate, "demo:write");
    // Deliberately no `fs::write(estate.join("report.md"), ...)` here —
    // the claim names the file but it is absent from the worktree path.
    assert!(!estate.join("report.md").exists());

    let (code, stdout) = claim(&estate, &work, &run, &["--artifact", "report.md=report.md"]);
    assert_eq!(code, Some(3), "on-disk-missing claim stdout: {stdout}");
    assert!(
        stdout.contains("MissingArtifact") && stdout.contains("report.md"),
        "stdout: {stdout}"
    );
    assert_eq!(status(&pointer.socket, &work), "active");

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
}
