//! P2.6 W4 (`knowledge/work/p2-build-wave/w4/BUILD.md`; P2.6 run 3's
//! own three disclosed defects, `RESULT-rerun3.md` /
//! `evidence/p2-build-wave-2026-09-05/rerun3/verdict.md`): a real
//! `wirkd` and a real scratch git repo throughout (0040 D127), no
//! Herdr session (these checks never touch a pane — the same reasoning
//! `boundary_claim.rs`'s own module doc gives).
//!
//! (a)/(b) drive a real three-Waypoint Route (`w4_orient_build_verify`)
//! matching `build-wave.json`'s own shape: an Actor `orient`, an Actor
//! `build` that commits, a Deterministic `verify`. (c) drives a real
//! two-Waypoint Route (`w4_boundary_two_wp`) through an `OutOfBoundary`
//! refusal, a retry, and a late Claim against the superseded Run. (d)
//! drives an ad hoc Deterministic Work (`--kind deterministic
//! --command`) whose `cwd` is a real git repo of its own, retried after
//! the branch moves.

use wirk::wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{ClaimPayload, FailPayload, Reply, Request, RetryPayload, StatusPayload, WirkdPointer};

use wirk_core::{
    ClaimKind, EventKind, ExecutionTriple, Journal, RunId, WaypointId, WorkId, World, WorldHash,
};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// `wirk run-deterministic --executor child` with the `wirk` binary
/// under test on `PATH`.
///
/// `ChildExecutor::launch` passes its own environment through to the
/// deterministic child untouched apart from the injected triple, so a
/// Route whose command calls `wirk output dir` — the public command a
/// real deterministic stage uses to find where to write — needs that
/// binary reachable by name. `CARGO_BIN_EXE_wirk`'s own directory is
/// prepended rather than replacing `PATH`, so `sh` and everything else
/// the command needs still resolve.
fn run_deterministic_with_wirk_on_path(estate: &Path, work_id: &str) -> std::process::Output {
    let bin_dir = Path::new(wirk_bin())
        .parent()
        .expect("the test binary has a directory")
        .to_path_buf();
    let path = match std::env::var_os("PATH") {
        Some(existing) => {
            let mut dirs = vec![bin_dir];
            dirs.extend(std::env::split_paths(&existing));
            std::env::join_paths(dirs).expect("PATH joins")
        }
        None => bin_dir.into_os_string(),
    };
    wirk_cli()
        .args(["run-deterministic", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--executor", "child"])
        .env("PATH", path)
        .output()
        .expect("run-deterministic runs")
}

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

fn start_wirkd(estate: &Path) -> (KillOnDrop, WirkdPointer) {
    let child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_wirkd(estate);
    (child, pointer)
}

fn stop_wirkd(estate: &Path, mut child: KillOnDrop) {
    let stop = wirk_cli()
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

fn git(repo: &Path, args: &[&str]) -> String {
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
}

fn git_commit_all(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    git(repo, &["rev-parse", "HEAD"])
}

/// A real scratch repo with `src/lib.rs` and `docs/notes.md` committed
/// on one base commit (`boundary_claim.rs`'s own `scratch_repo`, R6
/// duplicate — a different test binary).
fn scratch_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("repo tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    fs::create_dir_all(repo.join("src")).expect("mkdir src");
    fs::write(repo.join("src/lib.rs"), b"// lib\n").expect("write src/lib.rs");
    let base_sha = git_commit_all(repo, "base");
    (dir, base_sha)
}

fn fixture(estate: &Path, name: &str) -> PathBuf {
    let text: &str = match name {
        "w4_orient_build_verify.json" => {
            include_str!("fixtures/routes/w4_orient_build_verify.json")
        }
        "w4_boundary_two_wp.json" => include_str!("fixtures/routes/w4_boundary_two_wp.json"),
        other => panic!("no route fixture named {other} under wirk/tests/fixtures/routes/"),
    };
    let dir = estate.join("fixtures").join("routes");
    fs::create_dir_all(&dir).expect("create estate fixtures/routes/ dir");
    let path = dir.join(name);
    fs::write(&path, text).expect("write embedded fixture");
    path
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

fn submit_actor(estate: &Path, route_path: &Path, repo: &Path, base_sha: &str) -> (String, String) {
    let output = wirk_cli()
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(route_path)
        .args(["--kind", "actor"])
        .args(["--repo", "demo:write", "--base", base_sha, "--repo-path"])
        .arg(repo)
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (work_id, run_id, _waypoint) =
        parse_submit_stdout(&String::from_utf8_lossy(&output.stdout));
    (work_id, run_id)
}

fn submit_deterministic(estate: &Path, base: &str, command: &[&str]) -> (String, String, String) {
    let mut cmd = wirk_cli();
    cmd.args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--kind",
            "deterministic",
            "--source-basis",
            "git",
            "--base",
            base,
            "--repo-path",
        ])
        .arg(estate)
        .args(["--repo", "demo:write", "--command"])
        .args(command);
    let output = cmd.output().expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_submit_stdout(&String::from_utf8_lossy(&output.stdout))
}

fn reserved_world(socket: &Path, work_id: &str) -> World {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload::admin(WorkId(work_id.to_string()))),
    )
    .expect("status call succeeds");
    match reply {
        Reply::Ok { result, .. } => {
            serde_json::from_value(result["world"].clone()).expect("world deserializes")
        }
        Reply::Err { error, .. } => panic!(
            "status unexpectedly refused: {} {}",
            error.code, error.message
        ),
    }
}

fn status(socket: &Path, work_id: &str) -> serde_json::Value {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload::admin(WorkId(work_id.to_string()))),
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

fn wirkd_record(socket: &Path, work_id: &str, run: Option<&str>, kind: EventKind) {
    let reply = wirkd::client::call(
        socket,
        &Request::record(wirkd::RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: run.map(|r| RunId(r.to_string())),
            kind,
        }),
    )
    .expect("record call succeeds");
    match reply {
        Reply::Ok { .. } => {}
        Reply::Err { error, .. } => panic!("record refused: {} {}", error.code, error.message),
    }
}

/// `wirk run`'s steps 2-3, replicated without Herdr (`boundary_claim.rs`'s
/// own `create_worktree_for_run`, R6 duplicate): a real `git worktree
/// add`, `WorktreeCreated` journaled, the World's `worktree_path`
/// filled in through a re-emitted `WaypointReserved`.
fn create_worktree_for_run(
    estate: &Path,
    socket: &Path,
    work_id: &str,
    run_id: &str,
    waypoint: &str,
) -> PathBuf {
    let world = reserved_world(socket, work_id);
    let actor = match world {
        World::Actor(actor) => actor,
        World::Deterministic(_) => panic!("expected an Actor World"),
    };
    let worktree_path = estate.join("worktrees").join(work_id);
    let head = wirk_herdr::git::worktree_add(
        Path::new(&actor.repository),
        &worktree_path,
        &actor.branch,
        &actor.base_sha,
    )
    .expect("worktree_add succeeds");
    wirkd_record(
        socket,
        work_id,
        Some(run_id),
        EventKind::WorktreeCreated {
            repo: actor.repository.clone(),
            base_sha: head,
            identity: None,
        },
    );
    let mut updated_actor = actor;
    updated_actor.worktree_path = worktree_path.clone();
    let updated_world = World::Actor(updated_actor);
    let world_hash = WorldHash::of(&updated_world);
    wirkd_record(
        socket,
        work_id,
        Some(run_id),
        EventKind::WaypointReserved {
            waypoint: WaypointId(waypoint.to_string()),
            world_hash,
            world: updated_world,
        },
    );
    worktree_path
}

fn claim(
    estate: &Path,
    work_id: &str,
    run_id: &str,
    artifacts: &[(&str, &str)],
) -> (Option<i32>, String) {
    let mut args = vec!["claim".to_string()];
    for (name, path) in artifacts {
        args.push("--artifact".to_string());
        args.push(format!("{name}={path}"));
    }
    let output = wirk_cli()
        .args(&args)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk claim runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    )
}

fn journal_events(estate: &Path, work_id: &str) -> Vec<wirk_core::Event> {
    let journal = Journal::open(estate.join("works").join(work_id)).expect("journal opens");
    journal.replay().expect("journal replays")
}

fn waypoint_reserved_count_for(events: &[wirk_core::Event], waypoint: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(&e.kind, EventKind::WaypointReserved { waypoint: w, .. } if w.0 == waypoint)
        })
        .count()
}

/// Also proves `ClaimPayload`/`ExecutionTriple` still construct as
/// expected (compile-time only, `boundary_claim.rs`'s own precedent).
#[allow(dead_code)]
fn payload_shape_compiles(work_id: &str, run_id: &str, estate: &str) -> ClaimPayload {
    ClaimPayload {
        triple: ExecutionTriple {
            estate_root: estate.to_string(),
            work_id: WorkId(work_id.to_string()),
            run_id: RunId(run_id.to_string()),
        },
        kind: ClaimKind::Done,
        artifacts: Default::default(),
        outputs: Default::default(),
        origin: None,
    }
}

/// (b) The first Actor's own declared output (`orient.md`) is left
/// untracked in the shared worktree — never committed, never removed
/// (`rerun3`'s own `NOTE-orient-md-lost.txt`/`orient.md` finding: this
/// is exactly the state a real orient actor leaves behind). Red before
/// this wave: `build`'s Claim was refused `OutOfBoundary: orient.md`,
/// because `orient.md` is `orient`'s own declared output, not `build`'s
/// — outside `build`'s own boundary (`["src/**", "build.md"]`) and not
/// among `build`'s own claimed artifacts.
#[test]
fn earlier_waypoints_declared_output_never_refuses_a_later_claim() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = fixture(estate, "w4_orient_build_verify.json");
    let (work_id, run1) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run1,
        "w4-orient-build-verify/orient",
    );

    // orient writes its own declared output and claims it.
    fs::write(worktree.join("orient.md"), b"# orient\n").expect("write orient.md");
    let (code1, stdout1) = claim(estate, &work_id, &run1, &[("orient.md", "orient.md")]);
    assert_eq!(code1, Some(0), "orient claim stdout: {stdout1}");
    assert_eq!(stdout1, "Validated");

    let result = status(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("w4-orient-build-verify/build")
    );
    let run2 = result["run_id"]
        .as_str()
        .expect("run_id for build")
        .to_string();

    // build edits inside its own boundary and writes its own declared
    // output — orient.md is STILL untracked in this same worktree, never
    // removed.
    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited\n").expect("edit src/lib.rs");
    fs::write(worktree.join("build.md"), b"# build\n").expect("write build.md");
    let (code2, stdout2) = claim(estate, &work_id, &run2, &[("build.md", "build.md")]);
    assert_eq!(
        code2,
        Some(0),
        "build's Claim must Validate despite orient's untracked declared output, got: {stdout2}"
    );
    assert_eq!(stdout2, "Validated");

    stop_wirkd(estate, wirkd_child);
}

/// (a) The full three-Waypoint chain: `build` (the second Actor)
/// commits a real file on the branch, then `verify` (Deterministic,
/// whose own command writes only its declared `verify.log`) Claims.
/// Red before this wave: `verify`'s World carried `orient`'s original
/// `base_sha` forward unchanged, so `build`'s own commit (`src/lib.rs`)
/// read as out-of-boundary for `verify`'s empty-boundary Waypoint.
#[test]
fn deterministic_waypoint_after_a_committing_actor_validates() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = fixture(estate, "w4_orient_build_verify.json");
    let (work_id, run1) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run1,
        "w4-orient-build-verify/orient",
    );

    fs::write(worktree.join("orient.md"), b"# orient\n").expect("write orient.md");
    let (code1, stdout1) = claim(estate, &work_id, &run1, &[("orient.md", "orient.md")]);
    assert_eq!(code1, Some(0), "orient claim stdout: {stdout1}");

    let after_orient = status(&pointer.socket, &work_id);
    let run2 = after_orient["run_id"]
        .as_str()
        .expect("run_id for build")
        .to_string();

    // build edits src/lib.rs and COMMITS it on this Work's branch (the
    // scenario's own name: "the second actor commits a file").
    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited by build\n")
        .expect("edit src/lib.rs");
    let build_sha = git_commit_all(&worktree, "build: edit src/lib.rs");
    fs::write(worktree.join("build.md"), b"# build\n").expect("write build.md");
    let (code2, stdout2) = claim(estate, &work_id, &run2, &[("build.md", "build.md")]);
    assert_eq!(code2, Some(0), "build claim stdout: {stdout2}");
    assert_eq!(stdout2, "Validated");

    let after_build = status(&pointer.socket, &work_id);
    assert_eq!(
        after_build["current_waypoint"].as_str(),
        Some("w4-orient-build-verify/verify")
    );
    let run3 = after_build["run_id"]
        .as_str()
        .expect("run_id for verify")
        .to_string();

    // verify's own reserved World must carry build's own commit as its
    // base — the branch tip right now, not orient's original base_sha —
    // so its own diff-at-claim sees only what verify's own command
    // changes.
    let verify_world = reserved_world(&pointer.socket, &work_id);
    match verify_world {
        World::Deterministic(det) => {
            assert_eq!(
                det.base_sha, build_sha,
                "verify's World must carry build's own commit as its base_sha, \
                 not orient's original base carried forward"
            );
            assert_eq!(det.cwd, worktree, "verify runs in the same shared worktree");
            // P3 native closeout item 4: this is the *auto-advanced*
            // Deterministic path, the one that used to reserve a
            // compiled-in `CARGO_TARGET_DIR=/var/tmp/wirk-target` — one
            // development box's absolute cache path, content-addressed
            // into this World's hash, with no supported override, and
            // disagreeing with the first Waypoint's own empty env. The
            // warm-cache policy survives as an estate choice the daemon
            // and its callers are started with (`ChildExecutor` spawns
            // over an inherited environment); the product pins nothing.
            assert!(
                det.env.is_empty(),
                "an auto-advanced Deterministic World must pin no host cache path: {:?}",
                det.env
            );
        }
        World::Actor(_) => panic!("expected a Deterministic World for verify"),
    }

    // verify's own command writes only its own declared output.
    let run_det = run_deterministic_with_wirk_on_path(estate, &work_id);
    assert!(
        run_det.status.success(),
        "run-deterministic (verify) failed: {}",
        String::from_utf8_lossy(&run_det.stderr)
    );

    let final_status = status(&pointer.socket, &work_id);
    assert_eq!(
        final_status["state"].as_str(),
        Some("completed"),
        "verify's Claim must Validate on the last Waypoint, reaching \
         completed, got: {final_status}"
    );
    let _ = run3;

    stop_wirkd(estate, wirkd_child);
}

/// `wirk output dir`/`list` used to always answer with this Run's
/// managed staging area, even for a Deterministic Waypoint
/// whose own bare Claim (`ContractNames::Checkout`, `main.rs`) reaches
/// into its checkout instead — so the guidance sent an actor to write
/// where its own automatic Claim would never look. **Red before this
/// correction**: `verify`'s `dir` printed a managed-staging path under
/// `works/<work>/outputs/staging/<run>`, not the shared worktree
/// `det.cwd` names, and a `verify.log` written there was invisible to
/// `verify`'s own bare Claim.
///
/// Reuses the exact three-Waypoint chain
/// `deterministic_waypoint_after_a_committing_actor_validates` drives up
/// to `verify`'s reservation, then exercises the guidance and the real
/// bare Claim it must agree with, against the real `wirkd` and a real
/// child process.
#[test]
fn deterministic_output_guidance_names_the_checkout_not_managed_staging() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = fixture(estate, "w4_orient_build_verify.json");
    let (work_id, run1) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run1,
        "w4-orient-build-verify/orient",
    );
    fs::write(worktree.join("orient.md"), b"# orient\n").expect("write orient.md");
    let (code1, stdout1) = claim(estate, &work_id, &run1, &[("orient.md", "orient.md")]);
    assert_eq!(code1, Some(0), "orient claim stdout: {stdout1}");

    let after_orient = status(&pointer.socket, &work_id);
    let run2 = after_orient["run_id"]
        .as_str()
        .expect("run_id for build")
        .to_string();
    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited by build\n")
        .expect("edit src/lib.rs");
    git_commit_all(&worktree, "build: edit src/lib.rs");
    fs::write(worktree.join("build.md"), b"# build\n").expect("write build.md");
    let (code2, stdout2) = claim(estate, &work_id, &run2, &[("build.md", "build.md")]);
    assert_eq!(code2, Some(0), "build claim stdout: {stdout2}");

    let after_build = status(&pointer.socket, &work_id);
    assert_eq!(
        after_build["current_waypoint"].as_str(),
        Some("w4-orient-build-verify/verify")
    );
    let run3 = after_build["run_id"]
        .as_str()
        .expect("run_id for verify")
        .to_string();

    // The decisive check: `wirk output dir`, run exactly as `verify`'s
    // own actor runs it (the injected triple, no argument), must name
    // the same directory `verify`'s own reserved World's `cwd` names —
    // the shared worktree — never managed staging.
    let dir_out = wirk_cli()
        .args(["output", "dir"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", &work_id)
        .env("WIRK_RUN_ID", &run3)
        .output()
        .expect("wirk output dir runs");
    assert!(
        dir_out.status.success(),
        "wirk output dir: {}",
        String::from_utf8_lossy(&dir_out.stderr)
    );
    let advertised = PathBuf::from(String::from_utf8_lossy(&dir_out.stdout).trim().to_string());
    assert_eq!(
        advertised, worktree,
        "a Deterministic Waypoint's advertised output destination must be its own checkout, \
         not managed staging"
    );

    // `wirk output list --json` must agree: the declared `verify.log`
    // resolves to that same checkout path, and is not yet staged.
    let list_out = wirk_cli()
        .args(["output", "list", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", &work_id)
        .env("WIRK_RUN_ID", &run3)
        .output()
        .expect("wirk output list runs");
    assert!(
        list_out.status.success(),
        "wirk output list --json: {}",
        String::from_utf8_lossy(&list_out.stderr)
    );
    let listing: serde_json::Value =
        serde_json::from_slice(&list_out.stdout).expect("wirk output list emits json");
    assert_eq!(listing["kind"].as_str(), Some("deterministic"));
    assert_eq!(
        listing["staging"].as_str(),
        Some(worktree.display().to_string().as_str()),
        "the reported destination must be the checkout: {listing:#}"
    );
    let outputs = listing["outputs"].as_array().expect("outputs array");
    let verify_log = outputs
        .iter()
        .find(|o| o["name"].as_str() == Some("verify.log"))
        .expect("verify.log is declared");
    assert_eq!(verify_log["addressable"].as_bool(), Some(true));
    assert_eq!(
        verify_log["path"].as_str(),
        Some(worktree.join("verify.log").display().to_string().as_str())
    );
    assert_eq!(
        verify_log["staged"].as_bool(),
        Some(false),
        "verify has not run yet"
    );

    // The human guidance `list` prints has to name the addressing this
    // Run's own Claim actually resolves through
    // (`ContractNames::Checkout` for a Deterministic Run), not
    // `--output NAME`, which reaches into managed staging and would
    // find nothing here.
    let text_out = wirk_cli()
        .args(["output", "list"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", &work_id)
        .env("WIRK_RUN_ID", &run3)
        .output()
        .expect("wirk output list runs");
    let text = String::from_utf8_lossy(&text_out.stdout).into_owned();
    assert!(
        text.contains(&format!("execution {}", worktree.display())),
        "the destination line must name this Run's own execution directory: {text}"
    );
    assert!(
        !text.contains("--output NAME"),
        "a Deterministic Run must not be told to claim through managed-output addressing: {text}"
    );
    assert!(
        text.contains("--artifact NAME=NAME"),
        "the by-hand form must be the checkout addressing its own Claim uses: {text}"
    );

    // The same surface for a Run the Work has already moved past: the
    // first (Actor) Run still answers with *its own* bound addressing —
    // managed staging — rather than being re-pointed at whatever
    // Waypoint the Work has since reached. `current` is *not* the
    // discriminator here: it reports whether this Run is the latest
    // attempt of its **own** Waypoint (`latest_run_for_waypoint`), and
    // `orient` was never retried, so it stays `true` while the Work
    // itself is two Waypoints further on.
    let superseded = wirk_cli()
        .args(["output", "list", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", &work_id)
        .env("WIRK_RUN_ID", &run1)
        .output()
        .expect("wirk output list runs");
    let superseded: serde_json::Value =
        serde_json::from_slice(&superseded.stdout).expect("wirk output list emits json");
    assert_eq!(superseded["kind"].as_str(), Some("actor"));
    assert_eq!(superseded["current"].as_bool(), Some(true));
    assert_ne!(
        superseded["staging"].as_str(),
        Some(worktree.display().to_string().as_str()),
        "a superseded Actor Run keeps its managed staging, not the current Waypoint's \
         execution directory: {superseded:#}"
    );

    // Now `verify`'s own real child command runs. It resolves its
    // destination the way a real deterministic stage does — `wirk
    // output dir` inside the child itself, the public command this
    // guidance is — writes its declared output there, exits, and its
    // executor files its Claim automatically
    // (`ChildExecutor::file_claim`) against that same directory. That
    // agreement, end to end through the advertised command, is what
    // this correction is for.
    let run_det = run_deterministic_with_wirk_on_path(estate, &work_id);
    assert!(
        run_det.status.success(),
        "run-deterministic (verify) failed: {}",
        String::from_utf8_lossy(&run_det.stderr)
    );
    assert!(
        worktree.join("verify.log").exists(),
        "verify's own child must have written into the directory `wirk output dir` named it"
    );

    let final_status = status(&pointer.socket, &work_id);
    assert_eq!(
        final_status["state"].as_str(),
        Some("completed"),
        "verify's own automatic Claim must Validate against the guidance's own destination: \
         {final_status:#}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// (c) An `OutOfBoundary` refusal, one retry, the retried Run's own
/// Claim Validates and auto-advances — then a second, late Claim
/// arrives against the *superseded* (retried-away) original Run,
/// reusing the same artifact. Red before this wave: that late Claim
/// Validated too (d9_5's "late claim honored" precedent did not
/// distinguish a Run superseded by its own retry from a genuinely
/// stuck/vanished one), re-triggering auto-advance a second time and
/// leaving two `WaypointReserved`/`RunOpened` pairs for `wp-2`.
#[test]
fn claim_against_a_superseded_run_is_refused_and_never_advances_twice() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = fixture(estate, "w4_boundary_two_wp.json");
    let (work_id, run1) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run1,
        "w4-boundary-two-wp/wp-1",
    );

    // An offending, undeclared, out-of-boundary write.
    fs::write(worktree.join("docs.md"), b"outside src/\n").expect("write docs.md");
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    let (code1, stdout1) = claim(estate, &work_id, &run1, &[("report.md", "report.md")]);
    assert_eq!(code1, Some(3), "expected OutOfBoundary refusal: {stdout1}");
    assert!(stdout1.starts_with("Refused: OutOfBoundary"), "{stdout1}");

    let retry_reply = wirkd::client::call(
        &pointer.socket,
        &Request::retry(RetryPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.clone()),
                run_id: RunId(run1.clone()),
            },
        }),
    )
    .expect("retry call succeeds");
    let run1_retry = match retry_reply {
        Reply::Ok { result, .. } => result["new_run_id"].as_str().unwrap().to_string(),
        Reply::Err { error, .. } => panic!("retry refused: {} {}", error.code, error.message),
    };

    // Clean up the offending file so the retried Run's own Claim
    // Validates.
    fs::remove_file(worktree.join("docs.md")).expect("remove offending file");
    let (code2, stdout2) = claim(estate, &work_id, &run1_retry, &[("report.md", "report.md")]);
    assert_eq!(code2, Some(0), "retried Run's claim stdout: {stdout2}");
    assert_eq!(stdout2, "Validated");

    let after_retry = status(&pointer.socket, &work_id);
    assert_eq!(
        after_retry["current_waypoint"].as_str(),
        Some("w4-boundary-two-wp/wp-2")
    );
    let events_after_retry = journal_events(estate, &work_id);
    assert_eq!(
        waypoint_reserved_count_for(&events_after_retry, "w4-boundary-two-wp/wp-2"),
        1,
        "exactly one WaypointReserved for wp-2 after the retried Run's own Claim"
    );

    // A second, late Claim against the ORIGINAL (superseded-by-retry)
    // Run, reusing the same artifact.
    let (code3, stdout3) = claim(estate, &work_id, &run1, &[("report.md", "report.md")]);
    assert_eq!(
        code3,
        Some(3),
        "a Claim against a superseded Run must be refused, got: {stdout3}"
    );
    assert!(
        stdout3.starts_with("Refused: AlreadyClaimed"),
        "expected AlreadyClaimed, got: {stdout3}"
    );

    let events_final = journal_events(estate, &work_id);
    assert_eq!(
        waypoint_reserved_count_for(&events_final, "w4-boundary-two-wp/wp-2"),
        1,
        "the late Claim against the superseded Run must not reserve wp-2 a second time"
    );

    stop_wirkd(estate, wirkd_child);
}

/// (d) A Deterministic Run's retry: its `cwd` (a real git repo of its
/// own — the ad hoc submit path's World carries no linked worktree,
/// `handle_submit`'s own `cwd: state.estate_root.clone()`) moves ahead
/// with a new commit between the first (failed) Run and the retry. Red
/// before this wave: `handle_retry`'s `Deterministic` arm reused the
/// prior World verbatim — same stale `base_sha`, no new
/// `WaypointReserved` at all.
#[test]
fn deterministic_retry_carries_the_branch_tip_as_base() {
    let repo_dir = tempfile::tempdir().expect("repo tempdir");
    let repo = repo_dir.path();
    git(repo, &["init", "-q"]);
    fs::write(repo.join("seed.txt"), b"seed\n").expect("write seed.txt");
    let base_sha0 = git_commit_all(repo, "base");

    let (wirkd_child, pointer) = start_wirkd(repo);

    let (work_id, run_id, _waypoint) =
        submit_deterministic(repo, &base_sha0, &["sh", "-c", "exit 1"]);

    // A local executor failure, journaled the way `run-deterministic`'s
    // own non-zero-exit path does (`needs_input.rs`'s own `fail`
    // helper, R6 duplicate).
    let fail_reply = wirkd::client::call(
        &pointer.socket,
        &Request::fail(FailPayload {
            triple: ExecutionTriple {
                estate_root: repo.display().to_string(),
                work_id: WorkId(work_id.clone()),
                run_id: RunId(run_id.clone()),
            },
            status: Some("1".to_string()),
            detail: Some("exit 1: command failed".to_string()),
        }),
    )
    .expect("fail call succeeds");
    assert!(matches!(fail_reply, Reply::Ok { .. }), "{fail_reply:?}");

    // P3 execution-recovery item 1: a Deterministic Git-basis Work now
    // executes in its own isolated worktree
    // (`<estate>/worktrees/<work_id>`, `deterministic.cwd`) rather than
    // the caller's own checkout — the same isolation an Actor World's
    // `worktree_path` already has, and the same place this retry's own
    // `resolve_git_sha` reads HEAD from, symmetrically with the Actor
    // arm just above. The branch that moves ahead before the retry is
    // therefore this Work's own worktree branch, not `repo` — advancing
    // `repo` directly (the caller's own checkout) must have no effect,
    // which is exactly the isolation this item exists to guarantee.
    let worktree = repo.join("worktrees").join(&work_id);
    fs::write(worktree.join("advance.txt"), b"advance\n").expect("write advance.txt");
    let base_sha1 = git_commit_all(&worktree, "advance past the failed Run's base");
    assert_ne!(base_sha0, base_sha1);

    let events_before = journal_events(repo, &work_id);
    let reserved_before = events_before
        .iter()
        .filter(|e| matches!(e.kind, EventKind::WaypointReserved { .. }))
        .count();

    let retry_reply = wirkd::client::call(
        &pointer.socket,
        &Request::retry(RetryPayload {
            triple: ExecutionTriple {
                estate_root: repo.display().to_string(),
                work_id: WorkId(work_id.clone()),
                run_id: RunId(run_id.clone()),
            },
        }),
    )
    .expect("retry call succeeds");
    let new_run_id = match retry_reply {
        Reply::Ok { result, .. } => result["new_run_id"].as_str().unwrap().to_string(),
        Reply::Err { error, .. } => panic!("retry refused: {} {}", error.code, error.message),
    };
    assert_ne!(new_run_id, run_id, "retry must open a fresh RunId");

    let events_after = journal_events(repo, &work_id);
    let reserved_after = events_after
        .iter()
        .filter(|e| matches!(e.kind, EventKind::WaypointReserved { .. }))
        .count();
    assert_eq!(
        reserved_after,
        reserved_before + 1,
        "a Deterministic retry must write a fresh WaypointReserved, not reuse the prior one verbatim"
    );

    let world = reserved_world(&pointer.socket, &work_id);
    match world {
        World::Deterministic(det) => {
            assert_eq!(
                det.base_sha, base_sha1,
                "the retried Deterministic World must carry the branch tip as its base_sha, \
                 not the failed Run's original base"
            );
        }
        World::Actor(_) => panic!("expected a Deterministic World"),
    }

    // The superseded Run is still answerable on its own terms. `wirk
    // output` resolves a Deterministic destination through this Run's
    // own bound reservation, so a Run a retry has replaced is answered
    // rather than refused, and is told it is no longer the current one.
    // This retry reserves the same `cwd` for both attempts (this Work's
    // own worktree), so the path alone does not distinguish the bound
    // reservation from the latest one — what it pins is that the
    // superseded Run keeps an answer at all, and that its currentness is
    // reported truthfully.
    let superseded = wirk_cli()
        .args(["output", "list", "--json"])
        .env("WIRK_ESTATE_ROOT", repo)
        .env("WIRK_WORK_ID", &work_id)
        .env("WIRK_RUN_ID", &run_id)
        .output()
        .expect("wirk output list runs");
    assert!(
        superseded.status.success(),
        "a superseded Deterministic Run still has its own bound destination: {}",
        String::from_utf8_lossy(&superseded.stderr)
    );
    let superseded: serde_json::Value =
        serde_json::from_slice(&superseded.stdout).expect("wirk output list emits json");
    assert_eq!(superseded["kind"].as_str(), Some("deterministic"));
    assert_eq!(superseded["current"].as_bool(), Some(false));
    assert_eq!(
        superseded["staging"].as_str(),
        Some(
            repo.join("worktrees")
                .join(&work_id)
                .display()
                .to_string()
                .as_str()
        ),
        "the destination is the one this Run's own reservation names: {superseded:#}"
    );

    stop_wirkd(repo, wirkd_child);
}

/// The `wirk` CLI with the *test runner's own* actor triple removed from
/// the child's environment.
///
/// `resolve_scope` reads `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`
/// to decide whether a call is an actor's own or an operator's, and a
/// test process inherits whatever its runner had. This suite is run from
/// inside a real actor pane often enough that an inherited triple makes
/// a fixture's administrative call against its own temp estate refuse as
/// a cross-estate read — so the fixture has to say which it is rather
/// than depend on who started it.
///
/// Sites that mean to act *as* an actor set the three back explicitly on
/// the returned command; a later `env` overrides this removal.
fn wirk_cli() -> Command {
    let mut command = Command::new(wirk_bin());
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}
