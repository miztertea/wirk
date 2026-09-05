//! P2.4 W1: the diff-at-claim boundary check (`orient/build-brief.md`
//! §3 W1, §8 amendment 1), against a real wirkd and a real git repo
//! (0040 D127) — no live Herdr session is needed for this check (it
//! never touches a pane), so the worktree is created and the World's
//! `worktree_path` filled in the same two steps `wirk run` itself takes
//! (`wirk/src/executor.rs`'s own "Step 2", `wirk_herdr::git::
//! worktree_add` then a re-emitted `WaypointReserved` through wirkd's
//! `record` verb) rather than driving a live actor pane end to end —
//! the tried step (W3) is where a live opencode actor exercises this
//! for real.

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{
    ClaimPayload, RecordPayload, Reply, Request, RetryPayload, StatusPayload, WirkdPointer,
};

use wirk_core::{ClaimKind, EventKind, ExecutionTriple, RunId, RunState, WorkId, World, WorldHash};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// Writes the canonical `boundary_src_only.json` fixture (embedded at
/// compile time, `include_str!`, R3) under `<estate>/fixtures/routes/`
/// and returns its path: a test binary carries its fixtures and never
/// reads a source path at run time — a binary compiled in one worktree
/// and reused from the shared cargo cache after that worktree was
/// removed failed all twelve tests in this file at the W6b land,
/// 2026-09-05 (the `env!("CARGO_MANIFEST_DIR")` read this replaced).
fn boundary_src_only_route(estate: &Path) -> PathBuf {
    let text: &str = include_str!("fixtures/routes/boundary_src_only.json");
    let dir = estate.join("fixtures").join("routes");
    fs::create_dir_all(&dir).expect("create estate fixtures/routes/ dir");
    let path = dir.join("boundary_src_only.json");
    fs::write(&path, text).expect("write embedded fixture");
    path
}

/// P2.7 W1: a Waypoint with two required declared outputs
/// (`report.md`, `summary.md`), boundary `["**"]` so a boundary refusal
/// never masks the artifact check this fixture exists to exercise.
fn two_required_outputs_route(estate: &Path) -> PathBuf {
    let text: &str = include_str!("fixtures/routes/two_required_outputs.json");
    let dir = estate.join("fixtures").join("routes");
    fs::create_dir_all(&dir).expect("create estate fixtures/routes/ dir");
    let path = dir.join("two_required_outputs.json");
    fs::write(&path, text).expect("write embedded fixture");
    path
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
        Command::new(wirk_bin())
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

/// A real scratch repo (0040 D127) with `src/lib.rs` and `docs/notes.md`
/// committed on one base commit, returning `(repo_dir_kept_alive,
/// base_sha)`.
fn scratch_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("repo tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    fs::create_dir_all(repo.join("src")).expect("mkdir src");
    fs::create_dir_all(repo.join("docs")).expect("mkdir docs");
    fs::write(repo.join("src/lib.rs"), b"// lib\n").expect("write src/lib.rs");
    fs::write(repo.join("docs/notes.md"), b"notes\n").expect("write docs/notes.md");
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
            "base",
        ],
    );
    let base_sha = git(repo, &["rev-parse", "HEAD"]);
    (dir, base_sha)
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

/// `wirk work submit --route <path> --kind actor --repo demo:write
/// --base <sha> --repo-path <repo>`, the same shape
/// `route_files.rs::submit_route`'s sibling actor calls use.
fn submit_actor(estate: &Path, route_path: &Path, repo: &Path, base_sha: &str) -> (String, String) {
    submit_actor_with_repo(estate, route_path, repo, base_sha, "demo:write")
}

/// Same as `submit_actor`, with the `--repo` binding spec (`<name>:
/// read|write`) named explicitly — P2.4 W2's Read-binding tests need
/// `demo:read` on the same fixture Route.
fn submit_actor_with_repo(
    estate: &Path,
    route_path: &Path,
    repo: &Path,
    base_sha: &str,
    repo_spec: &str,
) -> (String, String) {
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(route_path)
        .args(["--kind", "actor"])
        .args(["--repo", repo_spec, "--base", base_sha, "--repo-path"])
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

/// `wirk work status --estate <root> --work <id>`, the CLI verb (not
/// the raw socket call `reserved_world` uses) — P2.4 W2's own decisive
/// check names `wirk work status` showing `needs_input` with the
/// paths, so this reads it the way an operator would.
fn work_status_cli(estate: &Path, work_id: &str) -> String {
    let output = Command::new(wirk_bin())
        .args(["work", "status", "--estate"])
        .arg(estate)
        .args(["--work", work_id])
        .output()
        .expect("wirk work status runs");
    assert!(
        output.status.success(),
        "wirk work status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Reads back the reserved `World` for the Work's one Run via wirkd's
/// own `status` verb (`server.rs::handle_status`'s `result["world"]`).
fn reserved_world(socket: &Path, work_id: &str) -> World {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.to_string()),
        }),
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

fn wirkd_record(socket: &Path, work_id: &str, run: Option<&str>, kind: EventKind) {
    let reply = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
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

/// Steps 2-3 of `wirk run` (`wirk/src/executor.rs`), replicated without
/// starting a Herdr session at all — this check never touches a pane,
/// only the worktree's diff, so a live actor is the tried step's job
/// (W3), not this one's: `git worktree add` from the reserved World's
/// own repository/branch/base_sha, `WorktreeCreated` journaled with the
/// SHA read back, and the World's `worktree_path` filled in through a
/// re-emitted `WaypointReserved` — the exact sequence
/// `worktree_path_for_run`/`world_for_waypoint` (`server.rs`) expect.
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
        },
    );
    let mut updated_actor = actor;
    updated_actor.worktree_path = worktree_path.clone();
    let updated_world = World::Actor(updated_actor);
    let world_hash = WorldHash::of(&updated_world);
    wirkd_record(
        socket,
        work_id,
        None,
        EventKind::WaypointReserved {
            waypoint: wirk_core::WaypointId(waypoint.to_string()),
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
    let output = Command::new(wirk_bin())
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

/// Also proves `ClaimPayload`/`ExecutionTriple` still construct as
/// expected (compile-time only — the crate imports would otherwise be
/// dead), matching what `wirk claim` itself sends.
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
    }
}

/// brief's own scenario: boundary `["src/**"]`, the actor edits under
/// `docs/`, claims `report.md` (inside `src/**`? no — the artifact's
/// own path is excluded from enforcement, see the sibling test below;
/// here the artifact is written correctly under `src/` and the offense
/// is an *extra*, undeclared write to `docs/notes.md`). The Claim is
/// refused `OutOfBoundary` naming `docs/notes.md`.
#[test]
fn claim_refused_out_of_boundary_names_the_path() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    // Inside the boundary: the required artifact.
    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited\n").expect("edit src/lib.rs");
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    // Outside the boundary, undeclared: the offending change.
    fs::write(worktree.join("docs/notes.md"), b"notes\nedited\n").expect("edit docs/notes.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(code, Some(3), "expected exit 3 (Refused), stdout: {stdout}");
    assert!(
        stdout.starts_with("Refused: OutOfBoundary"),
        "expected an OutOfBoundary refusal, got: {stdout}"
    );
    assert!(
        stdout.contains("docs/notes.md"),
        "refusal must name the offending path, got: {stdout}"
    );
    // `src/lib.rs`, inside the boundary, is never named.
    assert!(
        !stdout.contains("src/lib.rs"),
        "an in-boundary change must not be named, got: {stdout}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// Same boundary, a change that stays inside `src/**` only: Validated.
#[test]
fn claim_validated_when_change_stays_inside_boundary() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited\n").expect("edit src/lib.rs");
    fs::write(worktree.join("src/report.md"), b"# report\n").expect("write src/report.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[("report.md", "src/report.md")]);
    assert_eq!(
        code,
        Some(0),
        "expected exit 0 (Validated), stdout: {stdout}"
    );
    assert_eq!(stdout, "Validated");

    stop_wirkd(estate, wirkd_child);
}

/// The Claim's own declared artifact, written outside the boundary
/// (the ordinary case for a Route whose declared output lives at the
/// worktree root, `orient/refuse.md` §4's own hazard), does not
/// self-refuse: it is excluded from the enforced diff before the
/// boundary is checked.
#[test]
fn claim_declared_artifact_output_does_not_self_refuse() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    // The only change is the declared output, at the worktree root —
    // outside `src/**`.
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(
        code,
        Some(0),
        "the Claim's own declared output must not self-refuse, stdout: {stdout}"
    );
    assert_eq!(stdout, "Validated");

    stop_wirkd(estate, wirkd_child);
}

/// A claimed artifact path containing `..` is refused `OutOfBoundary`
/// naming it, closing the `worktree_path.join(&artifact.path)` escape
/// gap (`orient/refuse.md` §4) — exercised before any worktree diff
/// runs at all.
#[test]
fn claim_refused_on_artifact_path_escape() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");

    let (code, stdout) = claim(
        estate,
        &work_id,
        &run_id,
        &[("report.md", "report.md"), ("evidence", "../escape.txt")],
    );
    assert_eq!(code, Some(3), "expected exit 3 (Refused), stdout: {stdout}");
    assert!(
        stdout.starts_with("Refused: OutOfBoundary"),
        "expected an OutOfBoundary refusal, got: {stdout}"
    );
    assert!(
        stdout.contains("../escape.txt"),
        "refusal must name the escaping artifact path, got: {stdout}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// W6 (P2.4 W1 follow-up, `p2-concurrency/ASSESSMENT.md`'s "two
/// defects"): an *absolute* artifact path landing inside the worktree
/// — the shape the Docker/child executors always claim in
/// (`cwd.join(&spec.name)` display()-formatted, `cwd == worktree_path`)
/// — is Validated, not refused `OutOfBoundary` on itself. Red before
/// this wave: the boundary diff's own membership test compared the raw
/// absolute string against `changed_paths`' worktree-relative output
/// and never matched, so the Claim's own declared output looked like
/// an undeclared, out-of-boundary write.
#[test]
fn claim_validated_when_artifact_path_is_absolute_inside_worktree() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");

    let absolute_report = worktree.join("report.md").display().to_string();
    let (code, stdout) = claim(
        estate,
        &work_id,
        &run_id,
        &[("report.md", &absolute_report)],
    );
    assert_eq!(
        code,
        Some(0),
        "an absolute artifact path inside the worktree must be Validated, stdout: {stdout}"
    );
    assert_eq!(stdout, "Validated");

    stop_wirkd(estate, wirkd_child);
}

/// The absolute-path twin of `claim_refused_on_artifact_path_escape`:
/// an absolute artifact path naming somewhere else entirely (here, a
/// sibling of the worktree under the same estate) is still refused
/// `OutOfBoundary` — canonicalizing the join must never turn an
/// escaping absolute path into an accepted one.
#[test]
fn claim_refused_when_absolute_artifact_path_escapes_worktree() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    let escaping = estate.join("outside.txt");
    fs::write(&escaping, b"escaped\n").expect("write outside the worktree");
    let escaping_str = escaping.display().to_string();

    let (code, stdout) = claim(
        estate,
        &work_id,
        &run_id,
        &[("report.md", "report.md"), ("evidence", &escaping_str)],
    );
    assert_eq!(code, Some(3), "expected exit 3 (Refused), stdout: {stdout}");
    assert!(
        stdout.starts_with("Refused: OutOfBoundary"),
        "expected an OutOfBoundary refusal, got: {stdout}"
    );
    assert!(
        stdout.contains("outside.txt"),
        "refusal must name the escaping artifact path, got: {stdout}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// P2.4 W1, item 1 (`orient/build-brief.md` §8 amendment 1): the
/// World's `boundary` at reservation is the Route-authored Waypoint's
/// own globs, not the repository path — `server.rs:722`'s (and its
/// sibling arm's) old `Boundary(vec![repo_path])`.
#[test]
fn world_boundary_at_reservation_is_the_waypoints_globs() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, _run_id) = submit_actor(estate, &route, repo, &base_sha);

    let world = reserved_world(&pointer.socket, &work_id);
    match world {
        World::Actor(actor) => {
            assert_eq!(
                actor.boundary.0,
                vec!["src/**".to_string()],
                "the reserved World's boundary must be the Waypoint's own globs, not the repo path"
            );
            assert_ne!(
                actor.boundary.0,
                vec![repo.display().to_string()],
                "the boundary must never be the repository path"
            );
        }
        World::Deterministic(_) => panic!("expected an Actor World"),
    }

    stop_wirkd(estate, wirkd_child);
}

/// P2.4 W2, item 1 (`orient/build-brief.md` §3 W2; `orient/refuse.md`
/// §2): a Work whose one repository binding is `Access::Read` refuses
/// any changed path at all, whatever the Waypoint's globs say — here
/// the edit is *inside* `["src/**"]`, which would Validate on a Write
/// binding (`claim_validated_when_change_stays_inside_boundary` above)
/// but must still refuse on a Read binding.
#[test]
fn claim_refused_out_of_boundary_on_read_binding_for_any_change() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor_with_repo(estate, &route, repo, &base_sha, "demo:read");
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    // Inside the boundary globs; still not tolerated on a Read binding.
    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited\n").expect("edit src/lib.rs");
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(code, Some(3), "expected exit 3 (Refused), stdout: {stdout}");
    assert!(
        stdout.starts_with("Refused: OutOfBoundary"),
        "a Read binding must refuse any change OutOfBoundary, got: {stdout}"
    );
    assert!(
        stdout.contains("src/lib.rs"),
        "refusal must name the changed path even though it matches the boundary glob, got: {stdout}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// P2.4 W2 verify finding (w2/VERIFY.md §6(b)): both existing Read-
/// binding tests only ever edit an already-*tracked* file, so a
/// regression that made `changed_paths` blind to untracked files
/// specifically on the Read-binding path would pass unnoticed. Here
/// the only change under a Read binding is a genuinely new,
/// never-committed file (`src/untracked.rs`, inside the `["src/**"]`
/// boundary glob — the case that distinguishes the Read rule from the
/// glob check, same as the sibling tracked-file test above) plus the
/// declared artifact `report.md`. The Claim must still be refused
/// `OutOfBoundary`, naming the untracked file.
#[test]
fn claim_refused_out_of_boundary_on_read_binding_for_untracked_file() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor_with_repo(estate, &route, repo, &base_sha, "demo:read");
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    // A brand-new file, never committed to the scratch repo — this is
    // the untracked path, not an edit to `src/lib.rs` (already
    // committed in `scratch_repo`).
    fs::write(worktree.join("src/untracked.rs"), b"// new\n").expect("write src/untracked.rs");
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(code, Some(3), "expected exit 3 (Refused), stdout: {stdout}");
    assert!(
        stdout.starts_with("Refused: OutOfBoundary"),
        "a Read binding must refuse an untracked change OutOfBoundary, got: {stdout}"
    );
    assert!(
        stdout.contains("src/untracked.rs"),
        "refusal must name the untracked path even though it matches the boundary glob, got: {stdout}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// P2.4 W2, item 2 (`orient/build-brief.md` §3 W2; `orient/refuse.md`
/// §1): a `ClaimRecorded` refused `OutOfBoundary` on a Work that is
/// not terminal sets the Work `NeedsInput`, with the cause naming the
/// offending path — read back the way an operator would, through
/// `wirk work status`.
#[test]
fn needs_input_set_on_out_of_boundary_refusal() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited\n").expect("edit src/lib.rs");
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    fs::write(worktree.join("docs/notes.md"), b"notes\nedited\n").expect("edit docs/notes.md");

    let (code, claim_stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(
        code,
        Some(3),
        "expected exit 3 (Refused), stdout: {claim_stdout}"
    );

    let status = work_status_cli(estate, &work_id);
    assert!(
        status.contains("state needs_input"),
        "an OutOfBoundary refusal must put the Work in needs_input, got: {status}"
    );
    assert!(
        status.contains("out_of_boundary:"),
        "needs_input's cause must name the out_of_boundary reason, got: {status}"
    );
    assert!(
        status.contains("docs/notes.md"),
        "needs_input's cause must name the offending path, got: {status}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// P2.4 W2, item 2's scoping (`orient/build-brief.md` §3 W2): every
/// other refusal kind (`MissingArtifact`, `TripleMismatch`,
/// `AlreadyClaimed`) leaves the Work's state alone — no `NeedsInput`,
/// since the actor can re-file the same Claim correctly with no human
/// step. Exercised here via `MissingArtifact` (claim a required
/// output that was never written).
#[test]
fn other_refusal_kinds_stay_silent_no_needs_input() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    // `report.md` is a required declared output; never written. No
    // change to the worktree at all, so the boundary check itself has
    // nothing to say — the refusal must come from `MissingArtifact`.
    let (code, claim_stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(
        code,
        Some(3),
        "expected exit 3 (Refused), stdout: {claim_stdout}"
    );
    assert!(
        claim_stdout.starts_with("Refused: MissingArtifact"),
        "expected a MissingArtifact refusal, got: {claim_stdout}"
    );

    let status = work_status_cli(estate, &work_id);
    assert!(
        !status.contains("needs_input out_of_boundary"),
        "a MissingArtifact refusal must not set out_of_boundary needs_input, got: {status}"
    );
    assert!(
        status.contains("needs_input -"),
        "a MissingArtifact refusal must leave needs_input unset, got: {status}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// P2.4 W2, item 3 (`orient/build-brief.md` §3 W2): `wirk claim`
/// already prints the refusal and exits 3 for any `Refused` verdict
/// (`main.rs`, unchanged by this wave) — this pins that the printed
/// line names the offending paths for an `OutOfBoundary` refusal
/// specifically, new coverage over existing behaviour.
#[test]
fn wirk_claim_prints_out_of_boundary_paths_exit_3() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    fs::write(worktree.join("docs/notes.md"), b"notes\nedited\n").expect("edit docs/notes.md");
    fs::write(worktree.join("docs/second.md"), b"another\n").expect("write docs/second.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(code, Some(3), "expected exit 3 (Refused), stdout: {stdout}");
    assert_eq!(
        stdout, "Refused: OutOfBoundary docs/notes.md, docs/second.md",
        "wirk claim's own printed line must name every offending path"
    );

    stop_wirkd(estate, wirkd_child);
}

/// Calls wirkd's `retry` verb directly over the socket, same shape as
/// `needs_input.rs`'s own helper (R2, duplicated per this file's
/// existing precedent of a small per-file copy over a shared module —
/// `flag_value`, `executor.rs`).
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

/// Reads back the folded `Run` for `run_id`, straight from the socket
/// `status` verb's `"runs"` array (`server.rs::handle_status`), so a
/// test can check a Run's own terminal state without opening the
/// journal file directly.
fn run_state(socket: &Path, work_id: &str, run_id: &str) -> RunState {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.to_string()),
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
    result["runs"]
        .as_array()
        .expect("runs array present")
        .iter()
        .find_map(|entry| {
            let run: wirk_core::Run = serde_json::from_value(entry["run"].clone()).ok()?;
            (run.id.0 == run_id).then_some(run.state)
        })
        .unwrap_or_else(|| panic!("no Run {run_id} in status's runs array"))
}

/// P2.6 W3 (rerun findings, `evidence/p2-build-wave-2026-09-05/rerun/`;
/// ruling 0052): after a Claim is `Refused OutOfBoundary` (leaving the
/// Run `Open`, D9#3) and the Work retried, the *new* Run's reserved
/// World must carry a triple naming the *new* Run — not the refused
/// Run's id, the exact staleness `handle_retry` (`server.rs:1736`
/// pre-fix) reproduced live (`03-orient.log` lines 21-23:
/// `agent_name_taken` on the retried pane launch). Red before this
/// wave: the World's `triple.run_id` was still the old, refused Run's
/// id.
#[test]
fn retry_after_out_of_boundary_refusal_reserves_a_world_naming_the_new_run() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited\n").expect("edit src/lib.rs");
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    fs::write(worktree.join("docs/notes.md"), b"notes\nedited\n").expect("edit docs/notes.md");

    let (code, claim_stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(
        code,
        Some(3),
        "expected exit 3 (Refused), stdout: {claim_stdout}"
    );

    let reply = retry(&pointer.socket, estate, &work_id, &run_id);
    let result = match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "retry unexpectedly refused: {} {}",
            error.code, error.message
        ),
    };
    let new_run_id = result["new_run_id"]
        .as_str()
        .expect("new_run_id present")
        .to_string();
    assert_ne!(new_run_id, run_id, "retry must open a fresh RunId");

    let world = reserved_world(&pointer.socket, &work_id);
    let actor = match world {
        World::Actor(actor) => actor,
        World::Deterministic(_) => panic!("expected an Actor World"),
    };
    assert_eq!(
        actor.triple.run_id.0, new_run_id,
        "the retried Run's reserved World must carry a triple naming the new Run, not the \
         refused Run {run_id}"
    );

    // The refused Run must no longer read as `Open` -- otherwise a
    // caller that picks "the" open Run by folding the Work's runs
    // (`wirk/src/executor.rs::fetch_open_run`) can still find the
    // abandoned Run first and collide on its still-alive pane name,
    // the exact live failure this wave answers.
    assert!(
        !matches!(
            run_state(&pointer.socket, &work_id, &run_id),
            RunState::Open
        ),
        "the refused, retried-away Run must not still read as Open"
    );
    assert!(
        matches!(
            run_state(&pointer.socket, &work_id, &new_run_id),
            RunState::Open
        ),
        "the new Run must be the one reading as Open"
    );

    stop_wirkd(estate, wirkd_child);
}

/// P2.6 W3: the retried World is the old one in everything but the
/// triple -- worktree, branch, repository, intent, outputs, and
/// boundary all carry over unchanged (the same worktree a human or
/// actor may already be looking at), only `triple.run_id` moves to the
/// new Run.
#[test]
fn retry_after_out_of_boundary_refusal_keeps_the_world_otherwise_unchanged() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = boundary_src_only_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "boundary-src-only/wp-1",
    );

    fs::write(worktree.join("src/lib.rs"), b"// lib\n// edited\n").expect("edit src/lib.rs");
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    fs::write(worktree.join("docs/notes.md"), b"notes\nedited\n").expect("edit docs/notes.md");

    let (code, claim_stdout) = claim(estate, &work_id, &run_id, &[("report.md", "report.md")]);
    assert_eq!(
        code,
        Some(3),
        "expected exit 3 (Refused), stdout: {claim_stdout}"
    );

    let before = match reserved_world(&pointer.socket, &work_id) {
        World::Actor(actor) => actor,
        World::Deterministic(_) => panic!("expected an Actor World"),
    };
    let world_hash_before = WorldHash::of(&World::Actor(before.clone()));

    let reply = retry(&pointer.socket, estate, &work_id, &run_id);
    match reply {
        Reply::Ok { .. } => {}
        Reply::Err { error, .. } => panic!(
            "retry unexpectedly refused: {} {}",
            error.code, error.message
        ),
    };

    let after = match reserved_world(&pointer.socket, &work_id) {
        World::Actor(actor) => actor,
        World::Deterministic(_) => panic!("expected an Actor World"),
    };
    let world_hash_after = WorldHash::of(&World::Actor(after.clone()));

    assert_eq!(after.repository, before.repository);
    assert_eq!(after.worktree_path, before.worktree_path);
    assert_eq!(after.branch, before.branch);
    assert_eq!(after.base_sha, before.base_sha);
    assert_eq!(after.intent, before.intent);
    assert_eq!(after.boundary.0, before.boundary.0);
    assert_eq!(
        after.output_contract.0.len(),
        before.output_contract.0.len()
    );
    assert_eq!(
        after.triple.estate_root, before.triple.estate_root,
        "only the triple's run_id should move"
    );
    assert_eq!(after.triple.work_id.0, before.triple.work_id.0);
    assert_ne!(
        after.triple.run_id.0, before.triple.run_id.0,
        "the triple's run_id must move to the retry's own Run"
    );
    assert_eq!(
        world_hash_after, world_hash_before,
        "WorldHash::of excludes triple, so a retry's World hashes identically"
    );

    stop_wirkd(estate, wirkd_child);
}

/// P2.7 Wave 1 (`orient/build-brief.md` §6.2, `orient/reorient.md` §D):
/// `wirk claim` with no `--artifact` flags asks wirkd for the current
/// Waypoint's declared output contract (the existing `status` verb,
/// `handle_status`'s `result["world"]`, R2 — no new wire method) and
/// files a Done Claim naming each declared output at its own name as
/// the worktree-relative path, before this change refused
/// `MissingArtifact` naming the one required output (`smoke.json`'s
/// `report.md`) since `claim.artifacts` was empty. This is the red
/// check: on `main` the assertion below is `Refused: MissingArtifact
/// report.md`; after the change it is `Validated`.
#[test]
fn claim_with_no_artifact_flags_uses_the_waypoint_output_contract() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route_text: &str = include_str!("fixtures/routes/smoke.json");
    let route_dir = estate.join("fixtures").join("routes");
    fs::create_dir_all(&route_dir).expect("create estate fixtures/routes/ dir");
    let route = route_dir.join("smoke.json");
    fs::write(&route, route_text).expect("write embedded fixture");

    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree =
        create_worktree_for_run(estate, &pointer.socket, &work_id, &run_id, "smoke/wp-1");

    // The declared output exists in the worktree at its own name; the
    // actor never types `wirk claim --artifact report.md=report.md`.
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[]);
    assert_eq!(
        code,
        Some(0),
        "expected exit 0 (Validated) once the declared output exists, stdout: {stdout}"
    );
    assert_eq!(stdout, "Validated");

    stop_wirkd(estate, wirkd_child);
}

/// (b) Two declared outputs, one absent: the Claim is refused, naming
/// the absent one, when `wirk claim` is run with no `--artifact` flags.
#[test]
fn claim_with_no_artifact_flags_names_the_missing_declared_output() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = two_required_outputs_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "two-required-outputs/wp-1",
    );

    // report.md is written; summary.md never is.
    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[]);
    assert_eq!(code, Some(3), "expected exit 3 (Refused), stdout: {stdout}");
    assert!(
        stdout.starts_with("Refused: MissingArtifact"),
        "expected a MissingArtifact refusal, got: {stdout}"
    );
    assert!(
        stdout.contains("summary.md"),
        "refusal must name the absent declared output, got: {stdout}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// (a) Once both declared outputs exist, the same no-flag Claim
/// Validates.
#[test]
fn claim_with_no_artifact_flags_validates_once_all_declared_outputs_exist() {
    let (repo_dir, base_sha) = scratch_repo();
    let repo = repo_dir.path();
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = two_required_outputs_route(estate);
    let (work_id, run_id) = submit_actor(estate, &route, repo, &base_sha);
    let worktree = create_worktree_for_run(
        estate,
        &pointer.socket,
        &work_id,
        &run_id,
        "two-required-outputs/wp-1",
    );

    fs::write(worktree.join("report.md"), b"# report\n").expect("write report.md");
    fs::write(worktree.join("summary.md"), b"# summary\n").expect("write summary.md");

    let (code, stdout) = claim(estate, &work_id, &run_id, &[]);
    assert_eq!(
        code,
        Some(0),
        "expected exit 0 (Validated), stdout: {stdout}"
    );
    assert_eq!(stdout, "Validated");

    stop_wirkd(estate, wirkd_child);
}
