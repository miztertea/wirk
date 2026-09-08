//! Shared real-daemon harness for the W-A nested-work test binaries
//! (`nested_work.rs`, `nested_correction.rs`). Extracted verbatim from
//! `nested_work.rs` when the correction wave added a second binary that
//! needs the same real `wirkd`/`git`/CLI discipline (R2: one harness,
//! not a second copy). Included with `#[path]` the same way
//! `route_fixture.rs` already is; each including binary brings its own
//! `mod wirkd` (the `#[path = "../src/wirkd/mod.rs"]` include) and this
//! module reads it through `crate::wirkd`.

// Included into several test binaries; whichever helper a given binary
// does not call would otherwise warn as dead code there
// (`route_fixture.rs`'s own module-level allow, same reason).
#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::wirkd::{self, FailPayload, Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{Event, EventId, EventKind, ExecutionTriple, Journal, RunId, Timestamp, WorkId};

pub fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

pub fn wait_for_pointer(estate: &Path) -> WirkdPointer {
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

pub struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn start_wirkd(estate: &Path) -> (KillOnDrop, WirkdPointer) {
    start_wirkd_with_path(estate, None)
}

/// The same real daemon, optionally started with a `PATH` of the
/// caller's choosing. `findings.rs`'s deterministic Application races
/// use it to put `git_gate`'s wrapper in front of the `git` the daemon
/// itself already shells out to (R4: a process's own `PATH`), which is
/// what lets a race be ordered without a single line of product
/// instrumentation. `None` is the ambient environment, byte for byte
/// what every other caller already gets.
pub fn start_wirkd_with_path(estate: &Path, path: Option<&str>) -> (KillOnDrop, WirkdPointer) {
    let mut command = Command::new(wirk_bin());
    command
        .args(["wirkd", "start", "--estate"])
        .arg(estate)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let child = KillOnDrop(command.spawn().expect("spawn wirkd"));
    let pointer = wait_for_pointer(estate);
    (child, pointer)
}

pub fn stop_wirkd(estate: &Path, mut child: KillOnDrop) {
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

/// A fresh throwaway Git repository with one empty base commit — every
/// Work in this file gets its own (module doc: `cwd` is the checkout
/// itself for a Git-basis Deterministic World, no per-Work worktree).
/// The base commit's author and committer dates are **pinned**, so
/// every repository this harness creates has one fixed base SHA rather
/// than one that depends on which wall-clock second the process reached
/// this line.
///
/// Several tests here compare two independently created repositories'
/// derived identities — an obligation `basis` folds the reserved
/// World's hash, which folds the repository's own `base_sha` — and
/// therefore silently required both `init_repo` calls to land inside
/// the same second. They passed on a quiet machine and failed under
/// load, which is not a test (`CLAUDE.md`: "a test is deterministic and
/// has been watched fail, or it is not a test"). Observed as
/// `child_investigation_confirmed_...` failing its own
/// "the stranger's own verification is real and settled" assertion
/// during a full-suite run while passing alone.
pub fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create repo dir");
    let run = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+0000")
                .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+0000")
                .status()
                .expect("git runs")
                .success(),
            "git {args:?} failed"
        );
    };
    run(&["init", "-q"]);
    run(&[
        "-c",
        "user.name=nested-work-test",
        "-c",
        "user.email=nested-work@example.test",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "base",
    ]);
}

pub fn write_file(repo: &Path, name: &str, content: &str) {
    fs::write(repo.join(name), content).unwrap_or_else(|err| panic!("write {name}: {err}"));
}

#[derive(Debug)]
pub struct Submitted {
    pub work_id: String,
    pub run_id: String,
    pub waypoint: String,
}

pub struct ParentRef<'a> {
    pub work: &'a str,
    pub waypoint: &'a str,
    pub run: &'a str,
    pub role: &'a str,
    /// W-A correction (F1/F4): the container *activation* the child
    /// serves, `--parent-attempt`. `None` lets wirkd bind the
    /// container's own current activation (the ordinary case); `Some`
    /// states it explicitly, which is how a stale generation is caught.
    pub attempt: Option<u32>,
}

/// `wirk work submit` against a real Git-verified Deterministic World
/// reserved from `route`'s own first (flattened) Waypoint — `--kind` is
/// deliberately omitted (module doc's own CLI note): passing `--kind
/// deterministic` selects the *ad hoc*, Route-less single-Waypoint
/// shape instead of loading `route` at all.
pub fn submit(
    estate: &Path,
    route: &str,
    repo: &Path,
    repos: &[&str],
    parent: Option<ParentRef>,
) -> Result<Submitted, String> {
    submit_kind(estate, route, repo, repos, parent, None)
}

/// `submit`, with the `--kind` the CLI needs when the Route's own first
/// leaf is an `Actor`: `--kind actor` is what selects the Git-verified
/// Actor World (repository path, real base SHA) a real checkout can be
/// materialized from — without it the Actor arm reserves the ad hoc
/// unknown-basis shape.
pub fn submit_kind(
    estate: &Path,
    route: &str,
    repo: &Path,
    repos: &[&str],
    parent: Option<ParentRef>,
    kind: Option<&str>,
) -> Result<Submitted, String> {
    let mut cmd = Command::new(wirk_bin());
    cmd.args(["work", "submit", "--estate"]).arg(estate);
    if let Some(kind) = kind {
        cmd.args(["--kind", kind]);
    }
    for binding in repos {
        cmd.args(["--repo", binding]);
    }
    // P3 W3 (ruling 0090): more than one `--repo` binding is now
    // ambiguous without an explicit `--execution-repo`; every caller
    // here always meant the first one (the real preserved legacy
    // reading), so the harness says so explicitly rather than every
    // call site repeating it.
    if let Some(first) = repos.first()
        && repos.len() > 1
        && let Some((name, _)) = first.split_once(':')
    {
        cmd.args(["--execution-repo", name]);
    }
    cmd.args(["--base", "HEAD", "--source-basis", "git", "--repo-path"])
        .arg(repo)
        .args(["--route", route]);
    if let Some(parent) = parent {
        cmd.args([
            "--parent-work",
            parent.work,
            "--parent-waypoint",
            parent.waypoint,
            "--parent-run",
            parent.run,
            "--role",
            parent.role,
        ]);
        if let Some(attempt) = parent.attempt {
            cmd.args(["--parent-attempt", &attempt.to_string()]);
        }
    }
    let output = cmd.output().expect("work submit runs");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        });
    }
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
    Ok(Submitted {
        work_id,
        run_id,
        waypoint,
    })
}

pub fn claim(estate: &Path, work_id: &str, run_id: &str, args: &[&str]) -> (Option<i32>, String) {
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

pub fn claim_ok(estate: &Path, work_id: &str, run_id: &str, artifact: &str) {
    let (code, stdout) = claim(estate, work_id, run_id, &["--artifact", artifact]);
    assert_eq!(code, Some(0), "claim {artifact} on {work_id}: {stdout}");
}

pub fn status(socket: &Path, work_id: &str) -> serde_json::Value {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload::admin(WorkId(work_id.to_string()))),
    )
    .expect("status call succeeds");
    match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "status unexpectedly refused for {work_id}: {} {}",
            error.code, error.message
        ),
    }
}

pub fn state_of(socket: &Path, work_id: &str) -> String {
    status(socket, work_id)["state"]
        .as_str()
        .expect("state string")
        .to_string()
}

pub fn retry_cli(estate: &Path, work_id: &str) -> (Option<i32>, String) {
    let output = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(estate)
        .args(["--work", work_id])
        .output()
        .expect("work retry runs");
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// `wirk work retry --estate <root> --work <id> --run <run-id>` (W-A
/// correction, F1/F2): the reopen path — retry naming one exact leaf
/// Run rather than letting the CLI resolve the Work's own current one,
/// which is what reopening an *already-closed* nested stage needs.
pub fn retry_run_cli(estate: &Path, work_id: &str, run_id: &str) -> (Option<i32>, String) {
    let output = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--run", run_id])
        .output()
        .expect("work retry runs");
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

pub fn cancel_cli(estate: &Path, work_id: &str, cascade: bool) -> (Option<i32>, String) {
    let mut args = vec!["work", "cancel", "--estate"];
    let estate_str = estate.to_string_lossy().to_string();
    args.push(&estate_str);
    args.push("--work");
    args.push(work_id);
    if cascade {
        args.push("--cascade");
    }
    let output = Command::new(wirk_bin())
        .args(&args)
        .output()
        .expect("work cancel runs");
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

pub fn fail_via_socket(socket: &Path, estate: &Path, work_id: &str, run_id: &str) {
    let reply = wirkd::client::call(
        socket,
        &Request::fail(FailPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.to_string()),
                run_id: RunId(run_id.to_string()),
            },
            status: Some("boom".to_string()),
            detail: Some("injected for retry test".to_string()),
        }),
    )
    .expect("fail call succeeds");
    assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
}

pub fn raw_append(estate: &Path, work_id: &str, run: Option<&str>, kind: EventKind) {
    let mut journal = Journal::open(estate.join("works").join(work_id)).expect("open raw journal");
    journal
        .append(&Event {
            id: EventId(String::new()),
            work: WorkId(work_id.to_string()),
            run: run.map(|r| RunId(r.to_string())),
            at: Timestamp(0),
            kind,
        })
        .expect("append raw event");
}

pub fn journal_events(estate: &Path, work_id: &str) -> Vec<Event> {
    Journal::open(estate.join("works").join(work_id))
        .expect("open journal")
        .replay()
        .expect("replay journal")
}

/// Materializes an Actor Run's checkout with real `git worktree add`
/// and records the resulting `WorktreeCreated`/`WaypointReserved`
/// through wirkd's own `record` verb — adopted verbatim from
/// `actor_advance.rs`'s helper of the same name (R2), which is
/// model-free by construction: no Herdr pane, no model invocation,
/// only real Git plus the two journal writes a real pane launch would
/// have made. Returns the worktree path the Claim's artifacts must be
/// written into.
pub fn materialize_actor(
    socket: &Path,
    estate: &Path,
    work_id: &str,
    run_id: &str,
) -> std::path::PathBuf {
    use crate::wirkd::RecordPayload;
    use wirk_core::{WaypointId, World, WorldHash};

    let result = status(socket, work_id);
    let mut world: World = serde_json::from_value(result["world"].clone()).expect("Actor World");
    let World::Actor(actor) = &mut world else {
        panic!("expected an Actor World, got: {world:?}");
    };
    let waypoint = result["current_waypoint"]
        .as_str()
        .expect("status names a current waypoint")
        .to_string();
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
    .expect("record WorktreeCreated");
    assert!(matches!(created, Reply::Ok { .. }), "{created:?}");

    actor.worktree_path = worktree.clone();
    let world_hash = WorldHash::of(&world);
    let reserved = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: Some(RunId(run_id.to_string())),
            kind: EventKind::WaypointReserved {
                waypoint: WaypointId(waypoint),
                world_hash,
                world,
            },
        }),
    )
    .expect("record WaypointReserved");
    assert!(matches!(reserved, Reply::Ok { .. }), "{reserved:?}");
    worktree
}
