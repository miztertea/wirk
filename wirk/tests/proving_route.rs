//! Process test for the proving Route (item 8, `orient/route.md`
//! §4, `BRIEF.md`'s decisive check). Drives the real built binary
//! against a real `wirkd`, same discipline `wirkd_process.rs` already
//! uses: `submit --route proving`, a hand-crafted `wc -l ...`-style
//! Claim for wp-1 with `report.md` present, `status` naming wp-2
//! reserved with a new `run_id` and a `Deterministic` World whose
//! `cwd` equals wp-1's own worktree path, then `wirk run-deterministic
//! --executor child` completing wp-2 for real and `status` reporting
//! `completed`.
//!
//! Readiness is the same bounded poll on the pointer file
//! `wirkd_process.rs` uses (issue 359), never a sleep.

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{EventKind, Journal, WorkId, World};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

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

/// `wirk work submit --estate <estate> --route proving --intent <text>
/// --repo <repo> --base main`, parsing its `work_id <id> run_id <id>
/// waypoint <id>` stdout line (wp-1's own triple).
fn submit_proving(estate: &Path, repo: &str) -> (String, String, String) {
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--route",
            "proving",
            "--intent",
            "write report.md with one line",
            "--repo",
            repo,
            "--base",
            "main",
        ])
        .output()
        .expect("work submit --route proving runs");
    assert!(
        output.status.success(),
        "work submit --route proving failed: {}",
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
    assert_eq!(
        waypoint, "proving/wp-1",
        "wp-1 must be the reserved Waypoint"
    );
    (work_id, run_id, waypoint)
}

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

/// Queries wirkd's `status` verb directly over the socket and returns
/// the full `result` object (this test reads `current_waypoint`,
/// `run_id`, and `world`, not only `state`).
fn status(socket: &Path, work_id: &str) -> serde_json::Value {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.to_string()),
        }),
    )
    .expect("status call succeeds");
    match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "status unexpectedly refused: {} {}",
            error.code, error.message
        ),
    }
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The decisive check (`BRIEF.md`, `orient/route.md` §4): submit on the
/// proving Route; wp-1's Claim (report.md present) validates and, under
/// the same journal lock, auto-advances wp-2 — `status` immediately
/// after names wp-2 as `current_waypoint`, a *new* `run_id` (not wp-1's
/// own), and a `Deterministic` World whose `cwd` equals wp-1's own
/// worktree path (here, the estate root — the default World every
/// `submit` without `--kind` reserves, same as `wirkd_process.rs`'s own
/// `submit` helper). `wirk run-deterministic --executor child` then
/// completes wp-2 for real (`wc -l < report.md > summary.md`); `status`
/// afterward reports `completed` and `summary.md` holds the line count.
#[test]
fn proving_route_advances_and_completes() {
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

    let (work_id, run1, waypoint1) = submit_proving(&estate, "demo:write");

    // wp-1's default (no `--kind`) World reserves `worktree_path` at
    // the estate root itself (`handle_submit`'s smoke-World fallback
    // arm, `server.rs`) — write `report.md` there, same convention
    // `wirkd_process.rs`'s own valid-claim case uses.
    fs::write(estate.join("report.md"), b"the report\n").expect("write report.md");

    let (code, stdout) = claim(
        &estate,
        &work_id,
        &run1,
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(0), "wp-1 claim stdout: {stdout}");
    assert_eq!(stdout, "Validated");

    // -- auto-advance: wp-2 reserved under the same journal lock -----
    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("proving/wp-2"),
        "status after wp-1's Claim: {result}"
    );
    assert_eq!(result["state"].as_str(), Some("active"), "status: {result}");
    let run2 = result["run_id"]
        .as_str()
        .expect("status names a run_id for wp-2")
        .to_string();
    assert_ne!(run2, run1, "wp-2 must open a new Run, not reuse wp-1's");

    let world: World =
        serde_json::from_value(result["world"].clone()).expect("status carries a World for wp-2");
    let World::Deterministic(deterministic) = world else {
        panic!("wp-2's World must be Deterministic, got: {world:?}");
    };
    assert_eq!(
        deterministic.cwd,
        estate.clone(),
        "wp-2's cwd must equal wp-1's own worktree path"
    );
    assert_eq!(
        deterministic.command,
        vec!["sh", "-c", "wc -l < report.md > summary.md"]
    );
    let _ = waypoint1; // asserted above via submit_proving's own check

    // -- run-deterministic --executor child completes wp-2 -----------
    let run_det = Command::new(wirk_bin())
        .args(["run-deterministic", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--executor", "child"])
        .output()
        .expect("run-deterministic runs");
    assert!(
        run_det.status.success(),
        "run-deterministic failed: {}",
        String::from_utf8_lossy(&run_det.stderr)
    );

    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["state"].as_str(),
        Some("completed"),
        "status after wp-2 completes: {result}"
    );
    let summary = fs::read_to_string(estate.join("summary.md")).expect("summary.md written");
    assert_eq!(summary.trim(), "1", "wc -l of a one-line report.md");

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

/// `git init` plus one commit, so `--base HEAD`/`--repo-path` resolves
/// to a real SHA (`wirk-herdr/tests/run_verb.rs::init_repo`'s own
/// shape, R6 duplicate — a shared test-utility module isn't warranted
/// for four lines across two crates' test binaries).
fn init_repo(dir: &Path) {
    let init = Command::new("git")
        .current_dir(dir)
        .args(["init", "-q"])
        .status()
        .expect("git init runs");
    assert!(init.success());
    let commit = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=spike",
            "-c",
            "user.email=spike@invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ])
        .status()
        .expect("git commit runs");
    assert!(commit.success());
}

/// W3 (`orient/route.md` §2/§8, build-brief.md §7.1): the smallest
/// Route slice for Move C's scaffolding — a `--route proving --kind
/// actor --command <argv...>` submit journals the command onto
/// `WorkSubmitted.wp2_command` (read directly off the journal, not
/// inferred from `status`, since `status` never echoes `WorkSubmitted`
/// back), and once wp-1's Claim validates, `handle_claim`'s
/// auto-advance builds wp-2's `DeterministicWorld` from that submitted
/// command — not the hardcoded `PROVING_WP2_COMMAND` —, with
/// `CARGO_TARGET_DIR=/var/tmp/wirk-target` in its `env` (the one
/// named-kept warm cargo cache, 0030; 0039 D126). `--kind actor`'s own
/// `worktree_path` stays empty until a real `wirk run` fills it in
/// (`RecordPayload`'s doc), so `wirkd` is spawned with the throwaway
/// repo as its own cwd here — the same relative-path resolution
/// `handle_claim`'s artifact-existence check performs server-side —
/// letting this test claim `report.md` without a live actor/worktree.
#[test]
fn proving_route_actor_submit_carries_wp2_command_to_auto_advance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let repo_dir = tempfile::tempdir().expect("tempdir for the throwaway repo");
    let repo = repo_dir.path().to_path_buf();
    init_repo(&repo);

    let mut wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);

    let wp2_command = ["sh", "-c", "echo hi > summary.md"];
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(&estate)
        .args([
            "--intent",
            "actor wp2_command scaffolding test",
            "--route",
            "proving",
            "--kind",
            "actor",
            "--repo-path",
        ])
        .arg(&repo)
        .args(["--base", "HEAD", "--command"])
        .args(wp2_command)
        .output()
        .expect("work submit --route proving --kind actor --command runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    let (mut work_id, mut run1, mut waypoint1) = (String::new(), String::new(), String::new());
    for pair in words.chunks(2) {
        if let [key, value] = pair {
            match *key {
                "work_id" => work_id = (*value).to_string(),
                "run_id" => run1 = (*value).to_string(),
                "waypoint" => waypoint1 = (*value).to_string(),
                _ => {}
            }
        }
    }
    assert!(
        !work_id.is_empty() && !run1.is_empty() && !waypoint1.is_empty(),
        "unexpected work submit stdout: {stdout:?}"
    );
    assert_eq!(waypoint1, "proving/wp-1");

    // -- test 1: the submitted command is journaled on WorkSubmitted --
    let journal = Journal::open(estate.join("works").join(&work_id))
        .expect("journal opens for the submitted Work");
    let events = journal.replay().expect("journal replays");
    let journaled_wp2_command = events.iter().find_map(|event| match &event.kind {
        EventKind::WorkSubmitted { wp2_command, .. } => Some(wp2_command.clone()),
        _ => None,
    });
    assert_eq!(
        journaled_wp2_command,
        Some(Some(wp2_command.iter().map(|s| s.to_string()).collect())),
        "the submitted --command must be journaled on WorkSubmitted.wp2_command"
    );

    // wp-1's own Claim: `report.md` written where the server's own
    // relative-path check resolves it (its own cwd, the repo — see
    // this test's own doc comment).
    fs::write(repo.join("report.md"), b"the report\n").expect("write report.md");
    let (code, claim_stdout) = claim(
        &estate,
        &work_id,
        &run1,
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(0), "wp-1 claim stdout: {claim_stdout}");
    assert_eq!(claim_stdout, "Validated");

    // -- test 2: wp-2's auto-advanced World carries the submitted -----
    // -- command (not PROVING_WP2_COMMAND) and the cargo cache env ----
    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("proving/wp-2"),
        "status after wp-1's Claim: {result}"
    );
    let world: World =
        serde_json::from_value(result["world"].clone()).expect("status carries a World for wp-2");
    let World::Deterministic(deterministic) = world else {
        panic!("wp-2's World must be Deterministic, got: {world:?}");
    };
    assert_eq!(
        deterministic.command,
        wp2_command.to_vec(),
        "wp-2's World must carry the submitted --command, not PROVING_WP2_COMMAND"
    );
    assert_eq!(
        deterministic
            .env
            .get("CARGO_TARGET_DIR")
            .map(String::as_str),
        Some("/var/tmp/wirk-target"),
        "wp-2's World must carry the named-kept warm cargo cache in its env"
    );

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
