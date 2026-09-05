//! Process tests for P2.6 Wave 1 (`orient/route.md` §3,
//! `orient/build-brief.md` §3): `handle_claim`'s auto-advance reserves a
//! World for an Actor Waypoint the same way it already does for a
//! Deterministic one — the prior Run's worktree path, branch, and
//! repository carried forward, the next Waypoint's own intent, output
//! contract, and boundary from the journaled definitions, a fresh
//! triple keyed to a newly-minted Run id. Before this wave the `Actor`
//! arm of that match was `None`: a Route whose next Waypoint is Actor
//! never got a second `RunOpened`, and `wirk run` had nothing to fetch.
//!
//! (a) Actor then Actor (`actor_then_actor.json`): the second
//! Waypoint's Run is Open with a `World::Actor` whose `worktree_path`
//! equals the first Run's.
//! (b) Deterministic then Actor (`deterministic_then_actor.json`): same
//! shape, proving the fallback path when the prior World carries no
//! `repository`/`branch` of its own (only `World::Actor` does).
//! (c) The existing Actor-then-Deterministic proving path is unchanged
//! by this wave — already covered live by `proving_route.rs` and
//! `route_files.rs`'s own tests, re-run here as-is by the same suite,
//! not duplicated.

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{WorkId, World};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// The two fixtures this wave adds, embedded at compile time
/// (`include_str!`, R3 — the same discipline `route_files.rs`'s own
/// `fixture` helper uses, avoiding the `env!("CARGO_MANIFEST_DIR")`
/// run-time read the P2.3 land found stale-binary-fragile).
fn fixture(estate: &Path, name: &str) -> PathBuf {
    let text: &str = match name {
        "actor_then_actor.json" => include_str!("fixtures/routes/actor_then_actor.json"),
        "deterministic_then_actor.json" => {
            include_str!("fixtures/routes/deterministic_then_actor.json")
        }
        other => panic!("no route fixture named {other} under wirk/tests/fixtures/routes/"),
    };
    let dir = estate.join("fixtures");
    fs::create_dir_all(&dir).expect("create estate fixtures/ dir");
    let path = dir.join(name);
    fs::write(&path, text).expect("write embedded fixture");
    path
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
/// --base main`, returning the parsed `work_id run_id waypoint` triple
/// (no `--kind`: the default World-assembly arm, `worktree_path` at the
/// estate root itself, the same convention `route_files.rs`'s own
/// helper uses).
fn submit_route(estate: &Path, route_path: &Path, repo: &str) -> (String, String, String) {
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(route_path)
        .args(["--repo", repo, "--base", "main"])
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
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

/// (a) Actor then Actor: claiming wp-1 Done auto-advances wp-2, a
/// *new* Run, `World::Actor` whose `worktree_path` equals wp-1's own
/// (`state.estate_root`, the default submit arm's convention), whose
/// `intent`/`output_contract`/`boundary` are wp-2's own journaled
/// definition — never `None`, the gap this wave closes.
#[test]
fn actor_then_actor_auto_advance_reserves_a_world_for_the_second_actor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, run1, waypoint1) = submit_route(
        &estate,
        &fixture(&estate, "actor_then_actor.json"),
        "demo:write",
    );
    assert_eq!(waypoint1, "actor-then-actor/wp-1");

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
        Some("actor-then-actor/wp-2"),
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
    let World::Actor(actor) = world else {
        panic!("wp-2's World must be Actor, got: {world:?}");
    };
    assert_eq!(
        actor.worktree_path,
        estate.clone(),
        "wp-2's worktree_path must equal wp-1's own"
    );
    assert_eq!(
        actor.intent, "read report.md, write verdict.md naming what it found",
        "wp-2's World must carry its own journaled intent"
    );
    assert_eq!(
        actor.output_contract.0[0].name, "verdict.md",
        "wp-2's own declared outputs, not wp-1's"
    );

    stop_wirkd(&estate, wirkd_child);
}

/// (b) Deterministic then Actor: the prior World (`World::Deterministic`)
/// carries no `repository`/`branch` of its own, proving the fallback
/// (the Work's repository binding, the one branch every Waypoint of
/// this Work shares) — same `worktree_path`-equals-the-prior-Run's
/// assertion as (a).
#[test]
fn deterministic_then_actor_auto_advance_reserves_a_world_for_the_actor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let estate = dir.path().to_path_buf();
    let (wirkd_child, pointer) = start_wirkd(&estate);

    let (work_id, _run1, waypoint1) = submit_route(
        &estate,
        &fixture(&estate, "deterministic_then_actor.json"),
        "demo:write",
    );
    assert_eq!(waypoint1, "deterministic-then-actor/wp-1");

    let run_det = Command::new(wirk_bin())
        .args(["run-deterministic", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--executor", "child"])
        .output()
        .expect("run-deterministic runs");
    assert!(
        run_det.status.success(),
        "run-deterministic (wp-1) failed: {}",
        String::from_utf8_lossy(&run_det.stderr)
    );

    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("deterministic-then-actor/wp-2"),
        "status after wp-1 completes: {result}"
    );
    assert_eq!(result["state"].as_str(), Some("active"), "status: {result}");

    let world: World =
        serde_json::from_value(result["world"].clone()).expect("status carries a World for wp-2");
    let World::Actor(actor) = world else {
        panic!("wp-2's World must be Actor, got: {world:?}");
    };
    assert_eq!(
        actor.worktree_path,
        estate.clone(),
        "wp-2's worktree_path must equal wp-1's own cwd \
         (the default submit arm's estate-root convention)"
    );
    assert_eq!(
        actor.repository, "demo",
        "falls back to the Work's own repository binding name \
         when the prior World (Deterministic) carries none"
    );
    assert!(
        actor.branch.starts_with("wirk/"),
        "falls back to the one branch this Work shares: {}",
        actor.branch
    );
    assert_eq!(
        actor.intent,
        "read prepared.md, write verdict.md naming what it found"
    );
    assert_eq!(actor.output_contract.0[0].name, "verdict.md");

    stop_wirkd(&estate, wirkd_child);
}
