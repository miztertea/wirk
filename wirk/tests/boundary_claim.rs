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

use wirkd::{ClaimPayload, RecordPayload, Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{ClaimKind, EventKind, ExecutionTriple, RunId, WorkId, World, WorldHash};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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

    let route =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/routes/boundary_src_only.json");
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
