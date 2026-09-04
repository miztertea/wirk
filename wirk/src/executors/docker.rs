//! `DockerExecutor`: a real Docker container per Run, `World::
//! Deterministic` only (0001 D4; 0022 D78), the same contract
//! `ChildExecutor` implements over a real OS process (`orient/
//! docker.md`, `orient/build-brief.md` §3 W2). Carved from sergeant-rs
//! v0.3.0's `DockerBackend` as a way (0023 D83); reshaped: the
//! `PR_SET_PDEATHSIG` mechanism `ChildExecutor` uses is structurally
//! unavailable to a container (the process Docker's client spawns is
//! `docker`/`dockerd`, not the container's own process tree, `orient/
//! docker.md` §1) — the substitute is `--rm` plus label-and-adopt, not
//! a kernel death-signal coupling; recovery-sweep re-adoption of a
//! still-open Run's container across a wirkd crash is a build-brief
//! policy decision (`orient/build-brief.md` §2 "Binding and recovery")
//! for a later item's startup path, not built here.
//!
//! `docker create` then `docker start` via `std::process::Command` (R3:
//! no docker API/HTTP crate earned, `orient/docker.md` §2, §6), image
//! `alpine:3.24` already on the box (no pull path exists anywhere in
//! this module). `poll` checks `docker inspect --format '{{.State.
//! Status}} {{.State.ExitCode}}'` every non-terminal tick, matching the
//! task's own literal shape, but **completion is decided by a
//! supervisor thread `launch` spawns, not by that `inspect` call, and
//! the actual `docker start` runs inside that thread too, as `docker
//! start -a` (attached)** — a build-time correctness fix, verified live
//! (BUILD.md), J1 (local, reversible, narrows no landed contract): with
//! `--rm` (added over sergeant's own argv, `orient/docker.md` §3), a
//! fast-exiting container (`sh -c "echo boom; exit 1"` in `alpine`) can
//! already be gone by the time any *separate*, later subprocess tries
//! to observe it — reproduced directly against the live daemon
//! 2026-09-04: a post-hoc `docker inspect`/`docker logs` both returned
//! "no such object"; even a `docker logs -f` follower spawned as its
//! own subprocess right after `docker start` returned lost the race
//! and captured nothing, and attaching it *before* `docker start`
//! (hoping to beat the container's own startup) captured nothing
//! either — `docker logs -f`, in either order, is not reliable against
//! a container this fast under `--rm`. `docker start -a` sidesteps the
//! problem entirely: it starts the container and attaches synchronously
//! in one daemon call, so there is no separate subprocess racing to
//! catch up, its own piped stdout/stderr is exactly the container's
//! output with nothing missed, and its own process exit status is set
//! to the container's exit status (verified live) — a `-1` in
//! `exit_code` means `docker start -a` itself could not be read (the
//! daemon gone, a malformed reply), not a container-side failure code.
//! `poll` reads the supervisor's result: `None` yet is `Running` (the
//! `docker inspect` heartbeat above still runs every tick, matching the
//! task's literal instruction, but its result is not otherwise acted
//! on — completion only ever comes from the supervisor); `Some(0)`
//! files the Claim itself, the same in-process wirkd-client call
//! `ChildExecutor` makes (there is no actor for a Deterministic
//! Waypoint, 0001 D4), reporting `Ok(Running)` for that tick
//! (`RunObservation` has no `Completed` variant, 0027); `Some(code !=
//! 0)` is `Failed` with the code and the last 4096 bytes of the
//! attached output.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use wirk_core::{
    ArtifactRef, ClaimKind, DeterministicWorld, ExecutionTriple, Executor, FailureCause,
    OutputContract, Run, RunId, RunObservation, Timestamp, WorkId, World,
};

use crate::wirkd::{ClaimPayload, Reply, Request, client};

/// The image every `DockerExecutor` Run launches (`orient/docker.md`
/// §3: on the box now, 13 MB, has `/bin/sh` — no pull step exists
/// anywhere in this module).
const IMAGE: &str = "alpine:3.24";

/// Bytes of `docker logs` output retained for a non-zero exit's
/// `FailureCause.detail` — the same bound `ChildExecutor`'s stderr tail
/// uses (issue 279's evidence guarantee), applied to the container's
/// combined stdout+stderr log stream.
const LOG_TAIL_BYTES: usize = 4096;

/// One process, many concurrent Runs (mirrors `ChildExecutor`); each
/// Run owns exactly one named container. Scoped one per Work, same J1
/// as `ChildExecutor` (`child.rs`'s struct doc): `Executor::launch`/
/// `poll` take only `&Run` (0027's landed trait), so the
/// `ExecutionTriple` a filed Claim needs comes from the executor's own
/// `work_id`, not threaded through the trait.
pub struct DockerExecutor {
    docker_bin: String,
    estate_root: PathBuf,
    work_id: WorkId,
    /// This wirkd process's own pid, folded into `io.wirk.wirkd_pid`
    /// (`orient/docker.md` §3) so a recovery sweep can tell which
    /// wirkd process last owned a container found still running.
    wirkd_pid: u32,
    runs: Mutex<BTreeMap<RunId, DockerState>>,
}

struct DockerState {
    container_name: String,
    cwd: PathBuf,
    expected_artifacts: OutputContract,
    /// Set once `poll` has filed (or attempted) the Claim on this Run's
    /// exit-0 tick, or recorded a non-zero-exit `Failed` — mirrors
    /// `ChildState.claim_filed`: a later `poll` call is a no-op once the
    /// container's outcome has been observed.
    claim_filed: bool,
    /// Filled exactly once by the supervisor thread `launch` spawns,
    /// from `docker start -a`'s own process exit status (module doc) —
    /// `None` while the container is still running. Race-free against
    /// `--rm`: the alternative, a post-hoc `docker inspect` after
    /// observing `exited`, is not (verified live).
    exit_code: Arc<Mutex<Option<i32>>>,
    /// The container's combined stdout+stderr, captured by the
    /// supervisor's attached `docker start -a` (module doc), bounded to
    /// `LOG_TAIL_BYTES`.
    log_tail: Arc<Mutex<Vec<u8>>>,
    /// The supervisor thread's own handle, so `wait` can block on the
    /// container's exit by joining it directly (ruling 0044: no poll
    /// loop) rather than polling `exit_code`. `Some` until `wait` (or
    /// `poll`'s terminal tick) takes and joins it once.
    supervisor: Option<JoinHandle<()>>,
}

/// Everything a `DockerExecutor` call can fail with — one enum per
/// executor (`orient/build-brief.md` §2 gap 3), same flat R3-stdlib
/// shape as `ChildExecutorError`.
#[derive(Debug)]
pub enum DockerExecutorError {
    /// `launch` given a `World::Actor`, not `Deterministic` (0001 D4).
    WrongWorldKind,
    /// `DeterministicWorld.base_sha` is empty (issue 285), refused
    /// before any `docker` invocation is made.
    MissingBaseSha,
    /// `docker create`/`docker inspect` itself failed to spawn as a
    /// subprocess (the `docker` binary missing, permission denied, ...)
    /// — never a panic. A supervisor-thread `docker start -a` spawn
    /// failure surfaces later, through `poll`, as `exit_code == -1`
    /// (module doc), not this variant — the supervisor has no `launch`
    /// caller left to return an `Err` to by the time it runs.
    Spawn(std::io::Error),
    /// A `docker` subprocess ran but exited non-zero at the CLI level —
    /// distinct from the *container's* own exit code, which is a
    /// `RunObservation::Failed`, not this error. Carries the command's
    /// stderr.
    DockerCommand(String),
    /// `poll` called for a `RunId` this executor never `launch`ed.
    UnknownRun(RunId),
    /// The Claim was filed but wirkd refused it, or the transport to
    /// wirkd itself failed — same meaning as `ChildExecutorError::
    /// ClaimFiling` (`orient/child.md` §4): a refusal is wirkd's
    /// verdict, for the Route-runner to turn into a journaled
    /// `RunFailed`, never invented here as a `RunObservation::Failed`.
    ClaimFiling(String),
}

impl fmt::Display for DockerExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DockerExecutorError::WrongWorldKind => {
                write!(f, "DockerExecutor given a World::Actor, not Deterministic")
            }
            DockerExecutorError::MissingBaseSha => {
                write!(f, "DeterministicWorld carries no base_sha (issue 285)")
            }
            DockerExecutorError::Spawn(err) => write!(f, "docker subprocess spawn failed: {err}"),
            DockerExecutorError::DockerCommand(msg) => write!(f, "docker command failed: {msg}"),
            DockerExecutorError::UnknownRun(run_id) => write!(f, "no such run: {run_id:?}"),
            DockerExecutorError::ClaimFiling(msg) => write!(f, "claim filing failed: {msg}"),
        }
    }
}

impl std::error::Error for DockerExecutorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DockerExecutorError::Spawn(err) => Some(err),
            _ => None,
        }
    }
}

impl DockerExecutor {
    /// `estate_root` locates wirkd's pointer file (`client::locate`,
    /// R2, same as `ChildExecutor`); `work_id` is folded into every
    /// Claim this executor files.
    pub fn new(estate_root: PathBuf, work_id: WorkId) -> Self {
        DockerExecutor {
            docker_bin: "docker".to_string(),
            estate_root,
            work_id,
            wirkd_pid: std::process::id(),
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    /// The container name recorded for a launched Run (`wirk-<run_id>`,
    /// deterministic from the `RunId` — the resume/retry-safe pattern
    /// `orient/docker.md` §3 names), for a caller that needs to observe
    /// or journal it externally. `None` before `launch` or after
    /// `remove_owned` has dropped the Run.
    pub fn container_name(&self, run_id: &RunId) -> Option<String> {
        let runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        runs.get(run_id).map(|state| state.container_name.clone())
    }

    /// `docker rm -f` by the journaled container name (`orient/
    /// docker.md` §1, §4: exact-owned removal, never a name-prefix
    /// sweep — the name itself already carries the `wirk-<run_id>`
    /// discipline). Best-effort: a container already gone (this Run's
    /// own `--rm` already fired, or a prior call already removed it) is
    /// not an error — `docker rm -f`'s own failure on a missing name is
    /// exactly the outcome wanted, so it is discarded, not surfaced.
    pub fn remove_owned(&self, run_id: &RunId) {
        let name = {
            let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
            runs.remove(run_id).map(|state| state.container_name)
        };
        if let Some(name) = name {
            let _ = Command::new(&self.docker_bin)
                .arg("rm")
                .arg("-f")
                .arg(&name)
                .output();
        }
    }

    fn file_claim(
        &self,
        triple: ExecutionTriple,
        artifacts: Vec<ArtifactRef>,
    ) -> Result<(), String> {
        let pointer = client::locate(&self.estate_root).map_err(|err| err.to_string())?;
        let payload = ClaimPayload {
            triple,
            kind: ClaimKind::Done,
            artifacts: artifacts.into_iter().map(|a| (a.name, a.path)).collect(),
        };
        match client::call(&pointer.socket, &Request::claim(payload)) {
            Ok(Reply::Ok { .. }) => Ok(()),
            Ok(Reply::Err { error, .. }) => Err(format!("{}: {}", error.code, error.message)),
            Err(err) => Err(err.to_string()),
        }
    }
}

impl Executor for DockerExecutor {
    type Error = DockerExecutorError;

    /// Refuses a `World::Actor` and a `World::Deterministic` with an
    /// empty `base_sha` (issue 285) before touching `docker` at all —
    /// symmetric with `ChildExecutor::launch`. Otherwise `docker
    /// create`s the container from `create_argv`'s exact argument list
    /// (labels, `--rm`, `--init`, `--network none`, uid-matched
    /// `--user`, the World's `cwd` bind-mounted at `/work`, the triple
    /// and the World's env as `-e`, `alpine:3.24`, then the command),
    /// synchronously — a `docker create` failure (bad argv, name
    /// collision) is returned from `launch` itself, never a panic —
    /// then hands the created-but-not-yet-running container to a
    /// supervisor thread, which runs `docker start -a` (module doc) and
    /// reports back through `poll`.
    fn launch(&self, run: &Run, world: &World) -> Result<(), Self::Error> {
        let World::Deterministic(det) = world else {
            return Err(DockerExecutorError::WrongWorldKind);
        };
        if det.base_sha.trim().is_empty() {
            return Err(DockerExecutorError::MissingBaseSha);
        }

        let container_name = format!("wirk-{}", run.id.0);
        let triple = ExecutionTriple {
            estate_root: self.estate_root.display().to_string(),
            work_id: self.work_id.clone(),
            run_id: run.id.clone(),
        };
        // SAFETY: `getuid`/`getgid` take no arguments and cannot fail —
        // trivial async-signal-safe reads of the calling process's own
        // credentials.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let argv = create_argv(&container_name, self.wirkd_pid, uid, gid, det, &triple);

        let create_output = Command::new(&self.docker_bin)
            .arg("create")
            .args(&argv)
            .output()
            .map_err(DockerExecutorError::Spawn)?;
        if !create_output.status.success() {
            return Err(DockerExecutorError::DockerCommand(format!(
                "docker create: {}",
                String::from_utf8_lossy(&create_output.stderr)
            )));
        }

        let exit_code = Arc::new(Mutex::new(None));
        let log_tail = Arc::new(Mutex::new(Vec::new()));
        // `docker start` itself runs inside the supervisor thread, as
        // `docker start -a` (module doc: race-free capture) — not
        // called synchronously here, so `launch` returns as soon as the
        // container exists, matching `Executor::launch`'s async
        // contract (it must not block for the container's whole
        // lifetime). Detached: the supervisor's own exit, not this
        // `JoinHandle`, is what `poll` observes (via `exit_code`); the
        // OS thread runs to completion regardless.
        let supervisor = spawn_supervisor(
            self.docker_bin.clone(),
            container_name.clone(),
            exit_code.clone(),
            log_tail.clone(),
        );

        let state = DockerState {
            container_name,
            cwd: det.cwd.clone(),
            expected_artifacts: det.expected_artifacts.clone(),
            claim_filed: false,
            exit_code,
            log_tail,
            supervisor: Some(supervisor),
        };
        self.runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run.id.clone(), state);
        Ok(())
    }

    /// `None` from the launch-spawned supervisor's `exit_code` is
    /// `Running` — a `docker inspect --format '{{.State.Status}} {{.
    /// State.ExitCode}}'` heartbeat still runs every such tick (matching
    /// `orient/docker.md` §5's literal shape), its result unused (module
    /// doc: race-free completion comes from the supervisor, not this
    /// call). `Some(0)` files the Claim itself (`orient/child.md` §4,
    /// same reasoning as `ChildExecutor`) and reports `Ok(Running)` for
    /// that tick; `Some(code != 0)` is `Failed` with the code and the
    /// supervisor's live-streamed log tail. Once a Run's outcome has
    /// been observed, later polls are a no-op `Ok(Running)`, same as
    /// `ChildExecutor`.
    fn poll(&self, run: &Run) -> Result<RunObservation, Self::Error> {
        let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        let state = runs
            .get_mut(&run.id)
            .ok_or_else(|| DockerExecutorError::UnknownRun(run.id.clone()))?;

        if state.claim_filed {
            return Ok(RunObservation::Running);
        }

        let exit_code = *state.exit_code.lock().unwrap_or_else(|p| p.into_inner());
        let Some(exit_code) = exit_code else {
            // Still running: the `docker inspect` heartbeat below is not
            // itself acted on (module doc) — it exists so a tick against
            // a container the daemon has, for whatever reason, already
            // lost track of is still visible in a log if this ever needs
            // debugging, without changing this call's outcome.
            let _ = inspect(&self.docker_bin, &state.container_name);
            return Ok(RunObservation::Running);
        };

        self.finish_exit(run, state, exit_code)
    }
}

impl DockerExecutor {
    /// Blocks on the container's own exit by joining the supervisor
    /// thread `launch` spawned (`docker start -a`, module doc: already
    /// blocks internally on the container's real exit — no polling
    /// added here, ruling 0044) and returns the terminal
    /// `RunObservation` directly; the one wirkd status read after this
    /// call is `main.rs::drive_run`'s job, not this method's.
    pub fn wait(&self, run: &Run) -> Result<RunObservation, DockerExecutorError> {
        let supervisor = {
            let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
            let state = runs
                .get_mut(&run.id)
                .ok_or_else(|| DockerExecutorError::UnknownRun(run.id.clone()))?;
            if state.claim_filed {
                return Ok(RunObservation::Running);
            }
            state.supervisor.take()
        };
        // Joined without holding `runs`'s lock: the supervisor thread
        // itself never touches this executor's state, only the
        // `exit_code`/`log_tail` Arcs it was handed at spawn time, so no
        // other Run's `poll`/`wait` is blocked by this join.
        if let Some(handle) = supervisor {
            let _ = handle.join();
        }
        let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        let state = runs
            .get_mut(&run.id)
            .ok_or_else(|| DockerExecutorError::UnknownRun(run.id.clone()))?;
        if state.claim_filed {
            return Ok(RunObservation::Running);
        }
        let exit_code = state
            .exit_code
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .expect("supervisor joined: exit_code is set");
        self.finish_exit(run, state, exit_code)
    }

    /// Shared post-exit handling for both `poll` (still-observed via the
    /// `exit_code` Arc, module doc's heartbeat path) and `wait`
    /// (blocking join): file the Claim on a clean exit or report the
    /// exit code and captured log tail as `Failed`.
    fn finish_exit(
        &self,
        run: &Run,
        state: &mut DockerState,
        exit_code: i32,
    ) -> Result<RunObservation, DockerExecutorError> {
        if exit_code == 0 {
            let artifacts: Vec<ArtifactRef> = state
                .expected_artifacts
                .0
                .iter()
                .map(|spec| ArtifactRef {
                    name: spec.name.clone(),
                    path: state.cwd.join(&spec.name).display().to_string(),
                })
                .collect();
            let triple = ExecutionTriple {
                estate_root: self.estate_root.display().to_string(),
                work_id: self.work_id.clone(),
                run_id: run.id.clone(),
            };
            state.claim_filed = true;
            self.file_claim(triple, artifacts)
                .map_err(DockerExecutorError::ClaimFiling)?;
            return Ok(RunObservation::Running);
        }

        let detail =
            String::from_utf8_lossy(&state.log_tail.lock().unwrap_or_else(|p| p.into_inner()))
                .into_owned();
        state.claim_filed = true;
        Ok(RunObservation::Failed(FailureCause {
            status: Some(exit_code.to_string()),
            request_id: None,
            at: now(),
            detail: Some(detail),
        }))
    }
}

/// `docker create`'s exact argument list, order included (`orient/
/// docker.md` §3, §5: label ordering and `--mount`'s CSV form are both
/// under contract). Excludes `docker`/`create` themselves — those are
/// the `Command::new`/`.arg("create")` the caller supplies — so this
/// function is testable with no daemon at all.
pub(crate) fn create_argv(
    container_name: &str,
    wirkd_pid: u32,
    uid: u32,
    gid: u32,
    det: &DeterministicWorld,
    triple: &ExecutionTriple,
) -> Vec<String> {
    let mut argv = vec![
        "--name".to_string(),
        container_name.to_string(),
        "--label".to_string(),
        "io.wirk.managed=true".to_string(),
        "--label".to_string(),
        format!("io.wirk.run={}", triple.run_id.0),
        "--label".to_string(),
        format!("io.wirk.wirkd_pid={wirkd_pid}"),
        "--rm".to_string(),
        "--init".to_string(),
        "--network".to_string(),
        "none".to_string(),
        "--user".to_string(),
        format!("{uid}:{gid}"),
        "--workdir".to_string(),
        "/work".to_string(),
        "--mount".to_string(),
        format!("type=bind,source={},target=/work", det.cwd.display()),
        "-e".to_string(),
        format!("WIRK_ESTATE_ROOT={}", triple.estate_root),
        "-e".to_string(),
        format!("WIRK_WORK_ID={}", triple.work_id.0),
        "-e".to_string(),
        format!("WIRK_RUN_ID={}", triple.run_id.0),
    ];
    for (key, value) in &det.env {
        argv.push("-e".to_string());
        argv.push(format!("{key}={value}"));
    }
    argv.push(IMAGE.to_string());
    argv.extend(det.command.iter().cloned());
    argv
}

/// Runs `docker inspect --format '{{.State.Status}} {{.State.
/// ExitCode}}' <name>`, returning the two whitespace-separated fields.
fn inspect(docker_bin: &str, name: &str) -> Result<(String, i32), DockerExecutorError> {
    let output = Command::new(docker_bin)
        .arg("inspect")
        .arg("--format")
        .arg("{{.State.Status}} {{.State.ExitCode}}")
        .arg(name)
        .output()
        .map_err(DockerExecutorError::Spawn)?;
    if !output.status.success() {
        return Err(DockerExecutorError::DockerCommand(format!(
            "docker inspect: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.trim().splitn(2, ' ');
    let status = fields.next().unwrap_or("").to_string();
    let code = fields
        .next()
        .unwrap_or("0")
        .trim()
        .parse::<i32>()
        .unwrap_or(-1);
    Ok((status, code))
}

/// Spawned once at `launch`, one per Run: runs `docker start -a <name>`
/// (attached), which starts the *created-but-not-yet-running* container
/// and blocks until it exits, with its stdout/stderr piped back and its
/// own process exit status set to the *container's* exit status —
/// verified live (module doc): unlike `docker inspect`/`docker logs`
/// called after the fact, or a `docker logs -f` follower spawned after
/// `docker start` returns, an attached start cannot miss output or race
/// `--rm`'s removal, because capture is synchronous with the run, not a
/// separate subprocess trying to catch up to one already started.
/// Fills `exit_code` exactly once with the container's exit code (`-1`
/// if `docker start -a` itself could not even be read — the daemon
/// gone, a malformed reply — still a terminal signal so `poll` does not
/// hang forever waiting on a `None` that will never arrive) and
/// `log_tail` with the last `LOG_TAIL_BYTES` of its combined output.
fn spawn_supervisor(
    docker_bin: String,
    name: String,
    exit_code: Arc<Mutex<Option<i32>>>,
    log_tail: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let output = Command::new(&docker_bin)
            .arg("start")
            .arg("-a")
            .arg(&name)
            .output();
        let (code, combined) = match output {
            Ok(out) => {
                let mut combined = out.stdout;
                combined.extend_from_slice(&out.stderr);
                (out.status.code().unwrap_or(-1), combined)
            }
            Err(_) => (-1, Vec::new()),
        };
        let start = combined.len().saturating_sub(LOG_TAIL_BYTES);
        *log_tail.lock().unwrap_or_else(|p| p.into_inner()) = combined[start..].to_vec();
        *exit_code.lock().unwrap_or_else(|p| p.into_inner()) = Some(code);
    })
}

fn now() -> Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Timestamp(ms as i64)
}
