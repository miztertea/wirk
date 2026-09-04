//! Process tests for Route files (p2-route-files W1, `BRIEF.md`'s own
//! red-before check, `orient/build-brief.md` §3 W1). Drives the real
//! built binary against a real `wirkd`, same discipline
//! `wirkd_process.rs`/`proving_route.rs` already use: a path-like
//! `--route` value (contains `/` or ends `.json`) loads the file at
//! submit and refuses before any journal write on any `RouteError`; a
//! valid file journals its own `WaypointDefinition`s onto
//! `WorkSubmitted.waypoint_defs`, and `handle_claim`'s auto-advance
//! reads the *next* Waypoint's command from there, never re-reading the
//! file.

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{EventKind, Journal, WorkId, World};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/routes")
        .join(name)
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

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_wirkd(estate: &Path) -> (KillOnDrop, WirkdPointer) {
    let child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(estate);
    (child, pointer)
}

fn stop_wirkd(estate: &Path, mut child: KillOnDrop) {
    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let exit_status = child.0.wait().expect("reap wirkd child");
    assert!(
        exit_status.success(),
        "wirkd did not exit clean: {exit_status:?}"
    );
}

/// `wirk work submit --estate <estate> --route <path> --repo <repo>
/// --base main`, returning the raw process output (not asserting
/// success — callers on the refusal side check the failure). No
/// `--intent`: an Actor Waypoint's intent is authored in the Route file
/// (p2-route-files W2, J1).
fn submit_route(estate: &Path, route_path: &Path, repo: &str) -> std::process::Output {
    Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(route_path)
        .args(["--repo", repo, "--base", "main"])
        .output()
        .expect("work submit runs")
}

fn parse_submit_stdout(stdout: &str) -> (String, String, String) {
    let words: Vec<&str> = stdout.split_whitespace().collect();
    let (mut work_id, mut run_id, mut waypoint) = (String::new(), String::new(), String::new());
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

/// A missing Route file is refused (exit 2), and no journal is written
/// for it: `<estate>/works/` never gains any entry from this submit.
#[test]
fn route_file_missing_is_refused_no_journal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let works_before: Vec<_> = fs::read_dir(estate.join("works"))
        .map(|entries| entries.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();

    let output = submit_route(
        &estate,
        Path::new("routes/does-not-exist.json"),
        "demo:write",
    );
    assert!(
        !output.status.success(),
        "submit with a missing Route file must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("RouteError"),
        "expected a RouteError refusal, got: {stderr}"
    );

    let works_after: Vec<_> = fs::read_dir(estate.join("works"))
        .map(|entries| entries.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert_eq!(
        works_before.len(),
        works_after.len(),
        "a refused submit must never create a Work journal"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// A bare `--route` name (no `/`, no `.json` suffix) resolves against
/// the estate's own `routes/` directory (format.md §2): with
/// `<estate>/routes/` present but no `proving.json` in it, an actor
/// submit (`--kind actor --repo-path`, `--base` a real commit SHA of
/// the named repo) is refused naming the missing file, and no journal
/// is written for it: `<estate>/works/` never gains any entry.
#[test]
fn route_file_bare_name_with_no_file_is_refused_no_journal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    fs::create_dir_all(estate.join("routes")).expect("estate routes/ exists, empty");

    // The submit's own honest actor shape: a real repo with one commit
    // behind `--repo-path` and `--base` resolved to a commit SHA.
    let repo_dir = tempfile::tempdir().expect("repo tempdir");
    let repo = repo_dir.path();
    let git = |args: &[&str]| -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    fs::write(repo.join("seed.txt"), b"seed\n").expect("seed file");
    git(&["add", "seed.txt"]);
    git(&[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "-q",
        "-m",
        "seed",
    ]);
    let base_sha = git(&["rev-parse", "HEAD"]);
    assert!(!base_sha.is_empty(), "base sha resolves");

    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let works_before: Vec<_> = fs::read_dir(estate.join("works"))
        .map(|entries| entries.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();

    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(&estate)
        .args(["--route", "proving", "--kind", "actor"])
        .args(["--repo", "demo:write", "--base", &base_sha, "--repo-path"])
        .arg(repo)
        .output()
        .expect("work submit runs");
    assert!(
        !output.status.success(),
        "submit with a bare route name and no routes/proving.json must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("RouteError"),
        "expected a RouteError refusal, got: {stderr}"
    );
    let expected_path = estate.join("routes").join("proving.json");
    assert!(
        stderr.contains("route file not found")
            && stderr.contains(expected_path.to_string_lossy().as_ref()),
        "the refusal must name the missing file {}, got: {stderr}",
        expected_path.display()
    );

    let works_after: Vec<_> = fs::read_dir(estate.join("works"))
        .map(|entries| entries.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert_eq!(
        works_before.len(),
        works_after.len(),
        "a refused submit must never create a Work journal"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The unknown-field fixture (D134 row 10: `"retries": 3`) is refused
/// the same way, over the real wire, no journal write.
#[test]
fn route_file_unknown_field_is_refused_no_journal_over_the_wire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let output = submit_route(&estate, &fixture("unknown_field.json"), "demo:write");
    assert!(
        !output.status.success(),
        "submit with an unknown-field Route file must fail"
    );
    assert!(
        !estate.join("works").exists()
            || fs::read_dir(estate.join("works")).unwrap().next().is_none(),
        "a refused submit must never create a Work journal"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// A malformed (truncated) Route file is refused the same way.
#[test]
fn route_file_malformed_json_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let output = submit_route(&estate, &fixture("malformed_json.json"), "demo:write");
    assert!(
        !output.status.success(),
        "submit with a malformed Route file must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("RouteError"),
        "expected a RouteError refusal, got: {stderr}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// The BRIEF's own red-before check: a three-Waypoint Route submitted
/// from a file journals all three ids, in file order, on
/// `WorkSubmitted.waypoints` — and the full `WaypointDefinition`s onto
/// `WorkSubmitted.waypoint_defs`.
#[test]
fn three_waypoint_route_from_file_journals_three_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let output = submit_route(&estate, &fixture("three_waypoint.json"), "demo:write");
    assert!(
        output.status.success(),
        "submit with a valid three-Waypoint Route file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (work_id, _run1, waypoint1) = parse_submit_stdout(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(waypoint1, "three-wp/wp-1");

    let journal = Journal::open(estate.join("works").join(&work_id)).expect("journal opens");
    let events = journal.replay().expect("journal replays");
    let (waypoints, waypoint_defs) = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::WorkSubmitted {
                waypoints,
                waypoint_defs,
                ..
            } => Some((waypoints.clone(), waypoint_defs.clone())),
            _ => None,
        })
        .expect("WorkSubmitted is journaled");

    assert_eq!(
        waypoints.iter().map(|w| w.0.as_str()).collect::<Vec<_>>(),
        vec!["three-wp/wp-1", "three-wp/wp-2", "three-wp/wp-3"],
        "all three ids must be journaled, in file order"
    );
    assert_eq!(
        waypoint_defs.len(),
        3,
        "the full WaypointDefinitions must be journaled too"
    );
    assert_eq!(
        waypoint_defs[1].command,
        Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "wc -l < report.md > summary.md".to_string(),
        ])
    );

    stop_wirkd(&estate, wirkd_child);
}

/// Auto-advance reads wp-2's command from the journaled
/// `WaypointDefinition`, not `PROVING_WP2_COMMAND` — a distinctive
/// command in the fixture proves it wasn't the hardcoded fallback.
#[test]
fn auto_advance_reads_journaled_waypoint_def_not_hardcoded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let output = submit_route(
        &estate,
        &fixture("two_waypoint_distinctive.json"),
        "demo:write",
    );
    assert!(
        output.status.success(),
        "submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (work_id, run1, waypoint1) = parse_submit_stdout(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(waypoint1, "two-wp-distinctive/wp-1");

    // wp-1's default (no `--kind`) World reserves `worktree_path` at the
    // estate root itself (same convention `proving_route.rs` uses).
    fs::write(estate.join("report.md"), b"the report\n").expect("write report.md");
    let (code, claim_stdout) = claim(
        &estate,
        &work_id,
        &run1,
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(0), "wp-1 claim stdout: {claim_stdout}");
    assert_eq!(claim_stdout, "Validated");

    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("two-wp-distinctive/wp-2"),
        "status after wp-1's Claim: {result}"
    );
    let world: World =
        serde_json::from_value(result["world"].clone()).expect("status carries a World for wp-2");
    let World::Deterministic(deterministic) = world else {
        panic!("wp-2's World must be Deterministic, got: {world:?}");
    };
    assert_eq!(
        deterministic.command,
        vec!["sh", "-c", "echo distinctive-not-proving > summary.md"],
        "wp-2's World must carry the journaled Route file's own command, \
         never PROVING_WP2_COMMAND"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// §7.1's own test: editing the Route file on disk after submit must
/// not change what a Work already reserved — auto-advance reads
/// `WorkSubmitted.waypoint_defs`, never `Route::load` again.
#[test]
fn editing_the_route_file_after_submit_does_not_change_the_reserved_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let route_path = dir.path().join("mutable-route.json");
    fs::copy(fixture("two_waypoint_distinctive.json"), &route_path)
        .expect("copy fixture to a mutable path");

    let (wirkd_child, pointer) = start_wirkd(&estate);

    let output = submit_route(&estate, &route_path, "demo:write");
    assert!(
        output.status.success(),
        "submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (work_id, run1, _waypoint1) = parse_submit_stdout(&String::from_utf8_lossy(&output.stdout));

    // Edit the file on disk after submit, before the Claim that
    // triggers auto-advance: wp-2's command changes to something the
    // Work never reserved.
    let edited = fs::read_to_string(&route_path)
        .expect("read route file")
        .replace(
            "echo distinctive-not-proving > summary.md",
            "echo edited-after-submit > summary.md",
        );
    fs::write(&route_path, edited).expect("edit route file after submit");

    fs::write(estate.join("report.md"), b"the report\n").expect("write report.md");
    let (code, claim_stdout) = claim(
        &estate,
        &work_id,
        &run1,
        &["--artifact", "report.md=report.md"],
    );
    assert_eq!(code, Some(0), "wp-1 claim stdout: {claim_stdout}");
    assert_eq!(claim_stdout, "Validated");

    let result = status(&pointer.socket, &work_id);
    let world: World =
        serde_json::from_value(result["world"].clone()).expect("status carries a World for wp-2");
    let World::Deterministic(deterministic) = world else {
        panic!("wp-2's World must be Deterministic, got: {world:?}");
    };
    assert_eq!(
        deterministic.command,
        vec!["sh", "-c", "echo distinctive-not-proving > summary.md"],
        "the reserved command must still be the one journaled at submit, \
         not the file's post-submit edit"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// A Route file with a `"retries"`/`"timeout"`-shaped field (D134: no
/// count or duration field anywhere) is refused as an unknown field,
/// not silently accepted.
#[test]
fn route_file_with_retries_field_is_refused_as_unknown_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, _pointer) = start_wirkd(&estate);

    let route_path = dir.path().join("retries.json");
    fs::write(
        &route_path,
        r#"{
          "id": "retries-route",
          "waypoints": [
            {
              "id": "retries-route/wp-1",
              "kind": "Deterministic",
              "command": ["true"],
              "declared_outputs": [],
              "timeout": 30
            }
          ]
        }"#,
    )
    .expect("write retries fixture");

    let output = submit_route(&estate, &route_path, "demo:write");
    assert!(
        !output.status.success(),
        "a Route file carrying a duration/count field must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("RouteError"),
        "expected a RouteError refusal, got: {stderr}"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// p2-route-files W2 (build-brief.md §7.3, J1): `--intent` is removed
/// from `wirk work submit` — passing it is a usage exit (exit 1, no
/// daemon call), not a silently-ignored flag. No `wirkd` is started for
/// this: the CLI itself refuses before ever building a payload.
#[test]
fn submit_with_intent_flag_is_usage_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(dir.path())
        .args(["--route", "smoke", "--intent", "still passed"])
        .args(["--repo", "demo:write", "--base", "main"])
        .output()
        .expect("work submit runs");
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

/// p2-route-files W2 (build-brief.md §7.3): `--route` is required for
/// every submit except the ad hoc `--kind deterministic --command`
/// single-Waypoint Work — a submit with neither is a usage exit before
/// `wirkd` is ever called.
#[test]
fn submit_without_route_or_command_is_usage_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(dir.path())
        .args(["--repo", "demo:write", "--base", "main"])
        .output()
        .expect("work submit runs");
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

/// build-brief.md §7.3's own "advances in the file's order": a Route
/// file whose two Waypoints are deliberately *misnamed* against their
/// array position (`proving_reversed.json`: the id `.../wp-2` is the
/// file's first entry, `.../wp-1` its second) reserves and advances by
/// array position alone — the id's own numeric suffix decides nothing.
#[test]
fn hand_edited_reversed_route_advances_in_the_journaled_file_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let output = submit_route(&estate, &fixture("proving_reversed.json"), "demo:write");
    assert!(
        output.status.success(),
        "submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (work_id, _run1, waypoint1) = parse_submit_stdout(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(
        waypoint1, "proving-reversed/wp-2",
        "the file's first array entry (id .../wp-2) must be reserved first, \
         despite the id naming it \"2\""
    );

    // The file's first entry (`.../wp-2`) writes `report.md`; complete
    // it for real.
    let run_det = Command::new(wirk_bin())
        .args(["run-deterministic", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--executor", "child"])
        .output()
        .expect("run-deterministic runs");
    assert!(
        run_det.status.success(),
        "run-deterministic (first entry) failed: {}",
        String::from_utf8_lossy(&run_det.stderr)
    );

    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("proving-reversed/wp-1"),
        "auto-advance must reserve the file's *second* array entry next \
         (id .../wp-1), never re-deriving order from the id: {result}"
    );

    // The file's second entry (`.../wp-1`) counts `report.md`'s lines
    // into `summary.md`; complete it too.
    let run_det = Command::new(wirk_bin())
        .args(["run-deterministic", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--executor", "child"])
        .output()
        .expect("run-deterministic runs");
    assert!(
        run_det.status.success(),
        "run-deterministic (second entry) failed: {}",
        String::from_utf8_lossy(&run_det.stderr)
    );

    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["state"].as_str(),
        Some("completed"),
        "both entries done, in the file's own order: {result}"
    );
    let summary = fs::read_to_string(estate.join("summary.md")).expect("summary.md written");
    assert_eq!(summary.trim(), "1", "wc -l of the one-line report.md");

    stop_wirkd(&estate, wirkd_child);
}
