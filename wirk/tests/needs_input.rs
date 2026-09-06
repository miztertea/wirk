//! P2.3 W1 (0033 D102; 0044; states.md §2): `wirkd`'s `status` reply
//! and `wirk work status`'s printed line both carry a Work's
//! `needs_input` cause once one exists. Drives the real built binary
//! against a real `wirkd` child process, the same discipline
//! `wirk/tests/wirkd_process.rs` uses; `fail`'s `RunFailed` is filed
//! over the wire the way `run-deterministic` files one for real (this
//! test skips spawning the deterministic executor itself — R1, the
//! wire call is what `handle_status`/`wirkd_status_command` read, not
//! how the `RunFailed` got journaled).

#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{
    FailPayload, RecordPayload, Reply, Request, RetryPayload, StatusPayload, WirkdPointer,
    WorkFailPayload,
};

use wirk_core::{EventKind, ExecutionTriple, Journal, RunId, WaypointId, WorkId, World, WorldHash};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// Same bounded poll as `wirkd_process.rs::wait_for_pointer` (issue
/// 359).
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

/// Same as `wirkd_process.rs::submit`: `wirk work submit --route smoke`
/// against the estate's own copy of the fixture, parsing `work_id`/
/// `run_id`/`waypoint` off stdout.
fn submit(estate: &Path, repo: &str) -> (String, String, String) {
    route_fixture::install_route_fixture(estate, "smoke");
    if !estate.join(".git").exists() {
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(estate)
                .status()
                .expect("git init runs")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=needs-input-test",
                    "-c",
                    "user.email=needs-input@example.test",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "base",
                ])
                .current_dir(estate)
                .status()
                .expect("git commit runs")
                .success()
        );
    }
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--route",
            "smoke",
            "--repo",
            repo,
            "--base",
            "HEAD",
            "--kind",
            "actor",
            "--repo-path",
        ])
        .arg(estate)
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

/// Files a `RunFailed{cause}` for `run_id` through wirkd's `fail` verb
/// directly over the socket (`FailPayload`, `handle_fail`) — the same
/// wire call `run-deterministic`'s own non-zero-exit path makes
/// (`wirkd_fail`, `main.rs`).
fn fail(socket: &Path, estate: &Path, work_id: &str, run_id: &str, status: &str, detail: &str) {
    let reply = wirkd::client::call(
        socket,
        &Request::fail(FailPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.to_string()),
                run_id: RunId(run_id.to_string()),
            },
            status: Some(status.to_string()),
            detail: Some(detail.to_string()),
        }),
    )
    .expect("fail call succeeds");
    match reply {
        Reply::Ok { .. } => {}
        Reply::Err { error, .. } => panic!(
            "fail unexpectedly refused: {} {}",
            error.code, error.message
        ),
    }
}

/// Calls wirkd's `retry` verb directly over the socket (`RetryPayload`,
/// `handle_retry`) and returns the raw `Reply` — callers decide whether
/// `Ok`/`Err` is expected (P2.3 W2, decide.md §1).
fn retry(socket: &Path, estate: &Path, work_id: &str, run_id: &str) -> Reply {
    wirkd::client::call(
        socket,
        &Request::retry(RetryPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.to_string()),
                run_id: RunId(run_id.to_string()),
            },
        }),
    )
    .expect("retry call succeeds")
}

/// Calls wirkd's `workfail` verb directly over the socket
/// (`WorkFailPayload`, `handle_workfail`) and returns the raw `Reply`.
fn workfail(socket: &Path, work_id: &str, reason: &str) -> Reply {
    wirkd::client::call(
        socket,
        &Request::workfail(WorkFailPayload {
            work_id: WorkId(work_id.to_string()),
            reason: reason.to_string(),
        }),
    )
    .expect("workfail call succeeds")
}

/// P2.6 W2 (ruling 0052 D156): journals `LifecycleObserved{Blocked}`
/// for `run_id` through wirkd's generic `record` verb directly over
/// the socket — the same wire call `wirk-herdr::run_loop::RunLoop`'s
/// `WirkdApi::record` makes when it observes a Blocked pane
/// (`observe_herdr`), so this simulates the loop's own write, not a
/// dedicated "blocked" verb (there is none — `fold`, not a new wire
/// endpoint, is what turns this into `NeedsInput`).
fn blocked(socket: &Path, estate: &Path, work_id: &str, run_id: &str, detail: &str) {
    let status = match wirkd::client::call(
        socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.to_string()),
        }),
    )
    .expect("status call succeeds")
    {
        Reply::Ok { result, .. } => result,
        reply => panic!("status refused: {reply:?}"),
    };
    let waypoint = WaypointId(
        status["current_waypoint"]
            .as_str()
            .expect("current waypoint")
            .to_string(),
    );
    let mut world: World = serde_json::from_value(status["world"].clone()).expect("Actor World");
    let World::Actor(actor) = &mut world else {
        panic!("expected Actor World");
    };
    let worktree = estate.join("worktrees").join(work_id);
    let head = wirk_herdr::git::worktree_add(
        Path::new(&actor.repository),
        &worktree,
        &actor.branch,
        &actor.base_sha,
    )
    .expect("materialize actor worktree");
    let created = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: Some(RunId(run_id.to_string())),
            kind: EventKind::WorktreeCreated {
                repo: actor.repository.clone(),
                base_sha: head,
            },
        }),
    )
    .expect("worktree record call succeeds");
    assert!(matches!(created, Reply::Ok { .. }), "{created:?}");
    actor.worktree_path = worktree;
    let world_hash = WorldHash::of(&world);
    let materialized = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: Some(RunId(run_id.to_string())),
            kind: EventKind::WaypointReserved {
                waypoint,
                world_hash,
                world,
            },
        }),
    )
    .expect("materialization record call succeeds");
    assert!(matches!(materialized, Reply::Ok { .. }), "{materialized:?}");
    let launched = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: Some(RunId(run_id.to_string())),
            kind: EventKind::RunLaunched {
                run: RunId(run_id.to_string()),
                actor_kind: Default::default(),
            },
        }),
    )
    .expect("launch record call succeeds");
    assert!(matches!(launched, Reply::Ok { .. }), "{launched:?}");
    let reply = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: Some(RunId(run_id.to_string())),
            kind: EventKind::LifecycleObserved {
                status: "Blocked".to_string(),
                detail: Some(detail.to_string()),
            },
        }),
    )
    .expect("record call succeeds");
    assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
}

/// Replays a Work's journal straight off disk (`<estate>/works/<id>`,
/// the same layout `journal_for` uses server-side) — for assertions the
/// wire's `status` reply doesn't carry, like `WorkFailed`'s own reason
/// text (states.md: `needs_input` is left as the last `RunFailed`/
/// `RunVanished`/Question cause set it, not overwritten by a later
/// terminal `WorkFailed`).
fn journal_events(estate: &Path, work_id: &str) -> Vec<wirk_core::Event> {
    let journal = Journal::open(estate.join("works").join(work_id)).expect("journal opens");
    journal.replay().expect("journal replays")
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

/// `handle_status`'s reply carries `needs_input.reason`/`.detail` (and
/// `.run`) once a `RunFailed` has moved the Work to `NeedsInput` — red
/// before this wave (the field did not exist on the reply at all).
#[test]
fn wirkd_status_reports_needs_input_cause() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );

    let reply = wirkd::client::call(
        &pointer.socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.clone()),
        }),
    )
    .expect("status call succeeds");
    let result = match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "status unexpectedly refused: {} {}",
            error.code, error.message
        ),
    };

    assert_eq!(result["state"].as_str(), Some("needs_input"));
    let cause = &result["needs_input"];
    assert_eq!(cause["run"].as_str(), Some(run_id.as_str()));
    assert_eq!(cause["reason"].as_str(), Some("run_failed"));
    assert_eq!(cause["detail"].as_str(), Some("exit 1: command failed"));
}

/// `wirk work status`'s printed line names the reason and the detail
/// (`wirkd_status_command`'s own new `needs_input` field) — red before
/// this wave (the line printed only `state`/`current_waypoint`).
#[test]
fn cli_work_status_prints_needs_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "actor could not make progress: spec.md does not exist",
    );

    let output = Command::new(wirk_bin())
        .args(["work", "status", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id])
        .output()
        .expect("work status runs");
    assert!(
        output.status.success(),
        "work status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("needs_input run_failed: actor could not make progress"),
        "stdout must name the reason and detail: {stdout:?}"
    );
}

/// P2.3 W2 (decide.md §1): `retry` refuses `NotNeedsInput` on a Work
/// that has never failed — no journal write. Red before this wave: the
/// `retry` verb doesn't exist.
#[test]
fn handle_retry_refuses_not_needs_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    let before = journal_events(&estate, &work_id).len();

    let reply = retry(&pointer.socket, &estate, &work_id, &run_id);
    match reply {
        Reply::Err { error, .. } => assert_eq!(error.code, "NotNeedsInput"),
        Reply::Ok { .. } => panic!("retry on an active Work must be refused"),
    }
    assert_eq!(
        journal_events(&estate, &work_id).len(),
        before,
        "a refused retry must not write to the journal"
    );
}

/// Probe (decide.md §1, D9#4): `retry`'s `run_id` naming a triple with
/// no `RunOpened` in this Work's journal is refused `TripleMismatch`,
/// the same check `claim`/`fail` make — a fabricated or stale run id
/// never opens a Run.
#[test]
fn handle_retry_unknown_run_id_refuses_triple_mismatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );

    let reply = retry(&pointer.socket, &estate, &work_id, "run-does-not-exist");
    match reply {
        Reply::Err { error, .. } => assert_eq!(error.code, "TripleMismatch"),
        Reply::Ok { .. } => panic!("retry on a fabricated run id must be refused"),
    }
}

/// P2.3 W2 (decide.md §1): `retry` on a `NeedsInput` Work opens a fresh
/// Run (a new `RunId`, one new `RunOpened`) — `wirk work status`'s
/// `run_id` and `world_hash` both move to the retry, `world_hash`
/// unchanged (`WorldHash::of` excludes `triple`, the only field that
/// moves). Red before this wave.
///
/// P2.6 W3 (rerun findings; ruling 0052) revises this test's own prior
/// assertion: an Actor World's retry now *does* write one new
/// `WaypointReserved`, carrying a triple that names the new Run rather
/// than reusing the refused/failed Run's stale one
/// (`retry_after_out_of_boundary_refusal_reserves_a_world_naming_the_
/// new_run`, `boundary_claim.rs`, is this same fix's live-boundary
/// twin). The old assertion ("no new WaypointReserved") pinned the
/// defect this wave answers; updated here rather than left contradicting
/// the fix it exists to prove.
#[test]
fn handle_retry_opens_fresh_run_same_world() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );

    let status_before = |pointer: &WirkdPointer| -> serde_json::Value {
        match wirkd::client::call(
            &pointer.socket,
            &Request::status(StatusPayload {
                work_id: WorkId(work_id.clone()),
            }),
        )
        .expect("status call succeeds")
        {
            Reply::Ok { result, .. } => result,
            Reply::Err { error, .. } => panic!("status refused: {} {}", error.code, error.message),
        }
    };
    let before = status_before(&pointer);
    assert_eq!(before["state"].as_str(), Some("needs_input"));
    let world_hash_before = before["world_hash"].clone();

    let events_before_retry = journal_events(&estate, &work_id);
    let run_opened_count_before = events_before_retry
        .iter()
        .filter(|e| matches!(e.kind, EventKind::RunOpened { .. }))
        .count();
    let waypoint_reserved_count_before = events_before_retry
        .iter()
        .filter(|e| matches!(e.kind, EventKind::WaypointReserved { .. }))
        .count();

    let reply = retry(&pointer.socket, &estate, &work_id, &run_id);
    let result = match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "retry unexpectedly refused: {} {}",
            error.code, error.message
        ),
    };
    let old_run_id = result["old_run_id"].as_str().expect("old_run_id present");
    let new_run_id = result["new_run_id"].as_str().expect("new_run_id present");
    assert_eq!(old_run_id, run_id);
    assert_ne!(new_run_id, run_id, "retry must open a fresh RunId");

    let events_after_retry = journal_events(&estate, &work_id);
    let run_opened_count_after = events_after_retry
        .iter()
        .filter(|e| matches!(e.kind, EventKind::RunOpened { .. }))
        .count();
    let waypoint_reserved_count_after = events_after_retry
        .iter()
        .filter(|e| matches!(e.kind, EventKind::WaypointReserved { .. }))
        .count();
    assert_eq!(
        run_opened_count_after,
        run_opened_count_before + 1,
        "retry must append exactly one new RunOpened"
    );
    assert_eq!(
        waypoint_reserved_count_after,
        waypoint_reserved_count_before + 1,
        "retry on an Actor World must write one new WaypointReserved carrying the new Run's \
         own triple"
    );

    let after = status_before(&pointer);
    assert_eq!(after["state"].as_str(), Some("active"));
    assert_eq!(after["run_id"].as_str(), Some(new_run_id));
    assert_eq!(
        after["world_hash"], world_hash_before,
        "WorldHash::of excludes triple, so the retried World still hashes the same"
    );
    let world: World = serde_json::from_value(after["world"].clone()).expect("world deserializes");
    match world {
        World::Actor(actor) => assert_eq!(
            actor.triple.run_id.0, new_run_id,
            "the retried Run's reserved World must name the new Run, not the old one"
        ),
        World::Deterministic(_) => panic!("expected an Actor World"),
    }
}

/// P3 identity correction: an injected later reservation cannot replace
/// the exact World consumed by this Run's opening transition. `record`
/// refuses it without append, and retry resolves the triggering Run rather
/// than looking up the Waypoint's latest reservation.
#[test]
fn handle_retry_uses_triggering_run_exact_reservation_not_injected_latest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );

    let status_result = |pointer: &WirkdPointer| -> serde_json::Value {
        match wirkd::client::call(
            &pointer.socket,
            &Request::status(StatusPayload {
                work_id: WorkId(work_id.clone()),
            }),
        )
        .expect("status call succeeds")
        {
            Reply::Ok { result, .. } => result,
            Reply::Err { error, .. } => panic!("status refused: {} {}", error.code, error.message),
        }
    };
    let original: World = serde_json::from_value(status_result(&pointer)["world"].clone())
        .expect("world deserializes");
    let mut world = original.clone();
    match &mut world {
        World::Actor(actor) => {
            actor.base_sha = "second-base-sha".to_string();
            actor.source_basis = wirk_core::SourceBasis::Git {
                base: actor.base_sha.clone(),
            };
        }
        World::Deterministic(det) => {
            det.base_sha = "second-base-sha".to_string();
            det.source_basis = wirk_core::SourceBasis::OutputOnly {
                reference: det.base_sha.clone(),
            };
        }
    }
    let second_hash = WorldHash::of(&world);

    let before_forgery = journal_events(&estate, &work_id).len();
    let reply = wirkd::client::call(
        &pointer.socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work_id.clone()),
            run: Some(RunId(run_id.clone())),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId(waypoint.clone()),
                world_hash: second_hash.clone(),
                world: world.clone(),
            },
        }),
    )
    .expect("record call succeeds");
    assert!(matches!(reply, Reply::Err { .. }), "{reply:?}");
    assert_eq!(journal_events(&estate, &work_id).len(), before_forgery);

    let retry_reply = retry(&pointer.socket, &estate, &work_id, &run_id);
    let result = match retry_reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "retry unexpectedly refused: {} {}",
            error.code, error.message
        ),
    };
    let new_run_id = result["new_run_id"].as_str().expect("new_run_id present");

    let after = status_result(&pointer);
    assert_eq!(after["run_id"].as_str(), Some(new_run_id));
    let retried: World = serde_json::from_value(after["world"].clone()).expect("retried world");
    match (original, retried) {
        (World::Actor(original), World::Actor(retried)) => {
            assert_eq!(retried.base_sha, original.base_sha);
            assert_ne!(retried.base_sha, "second-base-sha");
            assert_eq!(retried.triple.run_id.0, new_run_id);
        }
        _ => panic!("expected Actor Worlds"),
    }
}

/// P2.3 W2 (decide.md §1): `fail` refuses `NotNeedsInput` on an active
/// Work — no journal write. Red before this wave: the `workfail` verb
/// doesn't exist.
#[test]
fn handle_workfail_refuses_not_needs_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, _run_id, _waypoint) = submit(&estate, "demo:write");
    let before = journal_events(&estate, &work_id).len();

    let reply = workfail(&pointer.socket, &work_id, "giving up");
    match reply {
        Reply::Err { error, .. } => assert_eq!(error.code, "NotNeedsInput"),
        Reply::Ok { .. } => panic!("fail on an active Work must be refused"),
    }
    assert_eq!(
        journal_events(&estate, &work_id).len(),
        before,
        "a refused fail must not write to the journal"
    );
}

/// P2.3 W2 (decide.md §1): `fail` on a `NeedsInput` Work appends
/// `WorkFailed{cause}` carrying the reason verbatim (0033 D102: never
/// inferred from `RunFailed`) and the Work reaches terminal `failed`.
/// Red before this wave.
#[test]
fn handle_workfail_appends_work_failed_with_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );

    let reply = workfail(
        &pointer.socket,
        &work_id,
        "actor could not make progress: /nonexistent/spec.md does not exist",
    );
    assert!(
        matches!(reply, Reply::Ok { .. }),
        "workfail unexpectedly refused: {reply:?}"
    );

    let status_reply = wirkd::client::call(
        &pointer.socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.clone()),
        }),
    )
    .expect("status call succeeds");
    let result = match status_reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!("status refused: {} {}", error.code, error.message),
    };
    assert_eq!(result["state"].as_str(), Some("failed"));

    let events = journal_events(&estate, &work_id);
    let last_work_failed = events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::WorkFailed { cause } => Some(cause.clone()),
            _ => None,
        })
        .expect("a WorkFailed event was journaled");
    assert_eq!(
        last_work_failed.detail.as_deref(),
        Some("actor could not make progress: /nonexistent/spec.md does not exist")
    );
}

/// P2.3 W2 (decide.md §1, the terminal-Work guard): a second `fail`
/// call on an already-`Failed` Work is refused `NotNeedsInput`, the
/// same guard that refuses the first call on a Work never `NeedsInput`
/// at all — `WorkFailed`'s own `fold` arm sets `Failed`, which is not
/// `NeedsInput` either.
#[test]
fn handle_workfail_second_call_refuses_not_needs_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );
    let first = workfail(&pointer.socket, &work_id, "first reason");
    assert!(matches!(first, Reply::Ok { .. }), "{first:?}");

    let second = workfail(&pointer.socket, &work_id, "second reason");
    match second {
        Reply::Err { error, .. } => assert_eq!(error.code, "NotNeedsInput"),
        Reply::Ok { .. } => panic!("a second fail on an already-failed Work must be refused"),
    }
}

/// `wirk work retry`'s printed line names both the old and new run id
/// (`work_retry_command`'s own print, `main.rs`). Drives the real built
/// binary, same discipline as `cli_work_status_prints_needs_input`.
#[test]
fn cli_work_retry_prints_old_and_new_run_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );

    let output = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id])
        .output()
        .expect("work retry runs");
    assert!(
        output.status.success(),
        "work retry failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .trim_start()
            .starts_with(&format!("Retried {run_id} -> ")),
        "stdout must name the old and new run id: {stdout:?}"
    );
    assert!(
        !stdout.trim_end().ends_with("-> "),
        "the new run id must not be empty: {stdout:?}"
    );
}

/// `wirk work fail`'s printed line names the reason
/// (`work_fail_command`'s own print, `main.rs`).
#[test]
fn cli_work_fail_prints_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    fail(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "1",
        "exit 1: command failed",
    );

    let output = Command::new(wirk_bin())
        .args(["work", "fail", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--reason", "giving up on this Work"])
        .output()
        .expect("work fail runs");
    assert!(
        output.status.success(),
        "work fail failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("WorkFailed {work_id}: giving up on this Work")),
        "stdout must name the reason: {stdout:?}"
    );
}

// ---- P2.6 W2 (ruling 0052 D156) --------------------------------------
//
// (c) A Blocked Work accepts `wirk work retry` and `wirk work fail`
// exactly as any other `NeedsInput` Work does — `handle_retry`/
// `handle_fail` (and the CLI verbs over them) read `WorkState::
// NeedsInput` alone, with no branch on *why*, so getting there via a
// `LifecycleObserved{Blocked}` observation instead of a `RunFailed`
// needs no change to either verb; these tests pin that the fold change
// alone is sufficient. Red before this wave: `LifecycleObserved` was
// folded inert, so `blocked()` above left the Work `active`, and both
// verbs refused it `NotNeedsInput` (the same refusal
// `handle_workfail_refuses_not_needs_input` pins for an untouched
// Work).

/// `wirkd_status` carries `needs_input.reason == "blocked"` and the
/// pane's screen-lines detail once a `LifecycleObserved{Blocked}` has
/// been journaled — the wire-level twin of
/// `wirkd_status_reports_needs_input_cause` above, sourced from a
/// Blocked observation instead of a `RunFailed`.
#[test]
fn wirkd_status_reports_needs_input_cause_for_blocked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    blocked(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "the actor is waiting on its pane w1:p1:\n\
         Permission required -- Access external directory /tmp",
    );

    let reply = wirkd::client::call(
        &pointer.socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.clone()),
        }),
    )
    .expect("status call succeeds");
    let result = match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "status unexpectedly refused: {} {}",
            error.code, error.message
        ),
    };

    assert_eq!(result["state"].as_str(), Some("needs_input"));
    let cause = &result["needs_input"];
    assert_eq!(cause["run"].as_str(), Some(run_id.as_str()));
    assert_eq!(cause["reason"].as_str(), Some("blocked"));
    assert!(
        cause["detail"]
            .as_str()
            .is_some_and(|d| d.contains("w1:p1") && d.contains("Permission required")),
        "cause.detail must carry the pane and its last screen lines: {cause:?}"
    );
}

/// `wirk work retry` succeeds on a Blocked-caused `NeedsInput` Work,
/// opening a fresh Run the same way it does after a `RunFailed`
/// (`cli_work_retry_prints_old_and_new_run_id`'s live twin) — no
/// `NotNeedsInput` refusal, a new run id printed.
#[test]
fn cli_work_retry_succeeds_on_a_blocked_needs_input_work() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    blocked(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "waiting on pane w1:p1",
    );

    let output = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id])
        .output()
        .expect("work retry runs");
    assert!(
        output.status.success(),
        "work retry failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .trim_start()
            .starts_with(&format!("Retried {run_id} -> ")),
        "stdout must name the old and new run id: {stdout:?}"
    );

    let status_reply = wirkd::client::call(
        &pointer.socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.clone()),
        }),
    )
    .expect("status call succeeds");
    let result = match status_reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!("status refused: {} {}", error.code, error.message),
    };
    assert_eq!(
        result["state"].as_str(),
        Some("active"),
        "a retry's RunOpened clears needs_input back to active: {result:?}"
    );
}

/// `wirk work fail` succeeds on a Blocked-caused `NeedsInput` Work,
/// reaching terminal `failed` with the given reason
/// (`cli_work_fail_prints_reason`'s live twin) — no `NotNeedsInput`
/// refusal.
#[test]
fn cli_work_fail_succeeds_on_a_blocked_needs_input_work() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (_wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run_id, _waypoint) = submit(&estate, "demo:write");
    blocked(
        &pointer.socket,
        &estate,
        &work_id,
        &run_id,
        "waiting on pane w1:p1",
    );

    let output = Command::new(wirk_bin())
        .args(["work", "fail", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--reason", "giving up on this Work"])
        .output()
        .expect("work fail runs");
    assert!(
        output.status.success(),
        "work fail failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("WorkFailed {work_id}: giving up on this Work")),
        "stdout must name the reason: {stdout:?}"
    );

    let status_reply = wirkd::client::call(
        &pointer.socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.clone()),
        }),
    )
    .expect("status call succeeds");
    let result = match status_reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!("status refused: {} {}", error.code, error.message),
    };
    assert_eq!(result["state"].as_str(), Some("failed"), "{result:?}");
}
