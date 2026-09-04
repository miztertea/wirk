//! `ChildExecutor`: a real OS child process per Run, `World::Deterministic`
//! only (0001 D4; 0022 D78). Carved from sergeant-rs v0.3.0's child
//! executor as a way (0023 D83; `orient/child.md`), reshaped: sergeant's
//! `ChildLifetime::Execution` exemption is dropped — every deterministic
//! child is hardened, none exempted, because a `ChildExecutor`'s child
//! is the whole Run, not a turn borrowed inside someone else's process
//! (`orient/child.md` §1) — and the executor files the Claim itself,
//! in-process, through the same `wirkd::client` module `wirk claim`'s
//! CLI handler uses, because there is no actor for a Deterministic
//! Waypoint (0001 D4; `orient/child.md` §4).
//!
//! Mechanism (`orient/child.md` §3, R3/R5): own process group
//! (`CommandExt::process_group(0)`, stdlib) plus
//! `libc::prctl(PR_SET_PDEATHSIG, SIGKILL)` armed in `pre_exec`, the
//! reference's own mechanism reused the same way, not reimplemented
//! against `nix`. Kill-then-reap on every exit path (clean, non-zero,
//! or signalled): a child that forked something before dying does not
//! leak it.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use wirk_core::{
    ArtifactRef, ClaimKind, ExecutionTriple, Executor, FailureCause, OutputContract, Run, RunId,
    RunObservation, Timestamp, WorkId, World,
};

use crate::wirkd::{ClaimPayload, Reply, Request, client};

/// Bytes of stderr retained for `FailureCause.detail` (`orient/
/// child.md` §1: mirrors sergeant's `SERVE_STDERR_TAIL_BYTES`, the
/// answer to issue 279 — "the actual failure detail, not only an
/// actor's own paraphrase").
const STDERR_TAIL_BYTES: usize = 4096;

/// One process, many concurrent Runs (`orient/child.md` §2); each Run
/// owns exactly one child. Scoped to one Work: `Executor::launch`/
/// `poll` take only `&Run` (0027's landed trait; `Run` itself carries
/// no `WorkId`), so the `ExecutionTriple` this executor files a Claim
/// with needs `work_id` from somewhere else. J1 (local, reversible, no
/// contract narrowed — the alternative is threading a `WorkId` through
/// the landed `Executor` trait itself, out of this item's allow-list):
/// the caller constructs one `ChildExecutor` per Work, the same way
/// wirkd already scopes one `Mutex<Journal>` per Work (0034 D105); the
/// Route-runner that will do this construction is W3's wiring (`orient/
/// child.md` §7 item 2), not built here.
pub struct ChildExecutor {
    estate_root: PathBuf,
    work_id: WorkId,
    runs: Mutex<BTreeMap<RunId, ChildState>>,
}

struct ChildState {
    child: Child,
    /// == the child's own pid: `process_group(0)` (§3) makes the child
    /// the leader of its own new group, so the pgid to signal is always
    /// its pid.
    pgid: i32,
    cwd: PathBuf,
    expected_artifacts: OutputContract,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    /// `Some` until `poll`'s exit arm joins it (issue 279 VERIFY: a
    /// `poll` that read `stderr_tail` right after `try_wait` reported
    /// the child gone, with no synchronization against this thread
    /// still draining the closing pipe, raced the reader and could read
    /// an empty tail even though the child wrote to stderr — silently
    /// destroying the evidence issue 279 exists to keep). `take()`n
    /// exactly once, on the tick the exit is first observed, so a later
    /// no-op poll (`claim_filed` already true) never tries to join an
    /// already-joined handle.
    stderr_reader: Option<JoinHandle<()>>,
    /// Set once `poll` has filed (or attempted) the Claim on this Run's
    /// exit-0 tick, so a later `poll` call never re-files it (`orient/
    /// child.md` §4: filing is a one-time event on the tick the exit is
    /// first observed).
    claim_filed: bool,
}

/// Everything a `ChildExecutor` call can fail with (`orient/child.md`
/// §2). Kept as one flat enum with `impl std::error::Error`, matching
/// `wirkd::client::ClientError`'s own R3-stdlib shape rather than
/// pulling `thiserror` into this wave's allow-list for one more type.
#[derive(Debug)]
pub enum ChildExecutorError {
    /// `launch` given a `World::Actor`, not `Deterministic` (0001 D4:
    /// `ChildExecutor` refuses everything else).
    WrongWorldKind,
    /// `DeterministicWorld.base_sha` is empty (issue 285; item 5's own
    /// addition to `wirk-core`, `orient/child.md` §7 item 1): the base
    /// ref must be explicit and validated, never the checkout's.
    MissingBaseSha,
    /// The command failed to spawn (ENOENT, permission, ...) — `launch`
    /// returns this, never panics (`orient/child.md` §5, `d5_4`).
    Spawn(std::io::Error),
    /// `try_wait` itself returned an I/O error (rare: a `wait(2)`
    /// failure, not a process exit).
    Wait(std::io::Error),
    /// `poll` called for a `RunId` this executor never `launch`ed.
    UnknownRun(RunId),
    /// The Claim was filed but wirkd refused it, or the transport to
    /// wirkd itself failed. `orient/child.md` §4: a refusal is wirkd's
    /// verdict, not `poll`'s to interpret — this `Err` is what the
    /// Route-runner (W3) turns into a journaled `RunFailed`, `poll`
    /// itself never invents a `RunObservation::Failed` for it.
    ClaimFiling(String),
}

impl fmt::Display for ChildExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChildExecutorError::WrongWorldKind => {
                write!(f, "ChildExecutor given a World::Actor, not Deterministic")
            }
            ChildExecutorError::MissingBaseSha => {
                write!(f, "DeterministicWorld carries no base_sha (issue 285)")
            }
            ChildExecutorError::Spawn(err) => write!(f, "spawn failed: {err}"),
            ChildExecutorError::Wait(err) => write!(f, "wait failed: {err}"),
            ChildExecutorError::UnknownRun(run_id) => write!(f, "no such run: {run_id:?}"),
            ChildExecutorError::ClaimFiling(msg) => write!(f, "claim filing failed: {msg}"),
        }
    }
}

impl std::error::Error for ChildExecutorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ChildExecutorError::Spawn(err) | ChildExecutorError::Wait(err) => Some(err),
            _ => None,
        }
    }
}

impl ChildExecutor {
    /// `estate_root` locates wirkd's pointer file (`client::locate`,
    /// R2); `work_id` is folded into every Claim this executor files
    /// (see the struct doc's J1).
    pub fn new(estate_root: PathBuf, work_id: WorkId) -> Self {
        ChildExecutor {
            estate_root,
            work_id,
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    /// The launched child's own pid, for a caller that needs to observe
    /// it externally (the `d5_5` death-signal proof: a helper process
    /// writes this pid out so the outer test can `kill -0` it after the
    /// helper is `SIGKILL`ed). `None` before `launch` or after the Run
    /// is no longer tracked.
    pub fn child_pid(&self, run_id: &RunId) -> Option<u32> {
        let runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        runs.get(run_id).map(|state| state.child.id())
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

impl Executor for ChildExecutor {
    type Error = ChildExecutorError;

    /// Refuses a `World::Actor` and a `World::Deterministic` with an
    /// empty `base_sha` (issue 285) before spawning anything. Otherwise
    /// spawns `det.command` hardened (§3) with `det.cwd`/`det.env`,
    /// stdout redirected straight to `<run dir>/stdout.log`, stderr
    /// piped to a reader thread that both tees it to `<run
    /// dir>/stderr.log` and keeps a bounded tail for `FailureCause.
    /// detail` (`orient/child.md` §1, §2). `<run dir>` is
    /// `<estate_root>/.wirk/runs/<run_id>` — alongside wirkd's own
    /// `.wirk` convention (0022 D79), never inside the World's own
    /// `cwd` (the worktree), so a Run's logs never become residue a
    /// Claim's artifact check could stumble over.
    fn launch(&self, run: &Run, world: &World) -> Result<(), Self::Error> {
        let World::Deterministic(det) = world else {
            return Err(ChildExecutorError::WrongWorldKind);
        };
        if det.base_sha.trim().is_empty() {
            return Err(ChildExecutorError::MissingBaseSha);
        }
        let mut words = det.command.iter();
        let Some(program) = words.next() else {
            return Err(ChildExecutorError::Spawn(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "DeterministicWorld.command is empty",
            )));
        };

        let run_dir = self.estate_root.join(".wirk").join("runs").join(&run.id.0);
        fs::create_dir_all(&run_dir).map_err(ChildExecutorError::Spawn)?;
        let stdout_file =
            File::create(run_dir.join("stdout.log")).map_err(ChildExecutorError::Spawn)?;
        let stderr_path = run_dir.join("stderr.log");

        let mut command = Command::new(program);
        command.args(words);
        command.current_dir(&det.cwd);
        command.envs(&det.env);
        command.stdin(Stdio::null());
        command.stdout(Stdio::from(stdout_file));
        command.stderr(Stdio::piped());
        harden_execution_child(&mut command);

        let mut child = command.spawn().map_err(ChildExecutorError::Spawn)?;
        let pgid = child.id() as i32;
        let stderr_pipe = child.stderr.take().expect("stderr piped above");

        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let reader = spawn_stderr_reader(stderr_pipe, stderr_path, stderr_tail.clone());

        let state = ChildState {
            child,
            pgid,
            cwd: det.cwd.clone(),
            expected_artifacts: det.expected_artifacts.clone(),
            stderr_tail,
            stderr_reader: Some(reader),
            claim_filed: false,
        };
        self.runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(run.id.clone(), state);
        Ok(())
    }

    /// `try_wait`: `None` -> `Running`. `Some(status)`:
    /// `kill_process_group` first on every exit path (§3, clean or
    /// not), then a signal death or non-zero exit is `Failed` with the
    /// stderr tail as detail; a clean exit files the Claim itself
    /// (`orient/child.md` §4) — a filing refusal or transport failure
    /// is `Err(ClaimFiling)` for the caller to turn into a journaled
    /// `RunFailed`, never a `RunObservation::Failed` invented here.
    /// Once a Claim has been filed for this Run (successfully or not),
    /// later polls are a no-op `Ok(Running)`: `RunObservation` has no
    /// `Completed` variant (0027), so this tick is the last one that
    /// does anything.
    fn poll(&self, run: &Run) -> Result<RunObservation, Self::Error> {
        let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        let state = runs
            .get_mut(&run.id)
            .ok_or_else(|| ChildExecutorError::UnknownRun(run.id.clone()))?;

        if state.claim_filed {
            return Ok(RunObservation::Running);
        }

        let status = match state.child.try_wait() {
            Ok(None) => return Ok(RunObservation::Running),
            Ok(Some(status)) => status,
            Err(err) => return Err(ChildExecutorError::Wait(err)),
        };

        self.finish_exit(run, state, status)
    }
}

impl ChildExecutor {
    /// Blocks on the child's own exit (`std::process::Child::wait`, R3 —
    /// the child's death is the state this blocks on, ruling 0044: no
    /// poll loop, no timeout) and returns the terminal
    /// `RunObservation` directly — the one wirkd status read after this
    /// call is the caller's job (`main.rs::drive_run`), not this
    /// method's.
    pub fn wait(&self, run: &Run) -> Result<RunObservation, ChildExecutorError> {
        // Held across the blocking `wait(2)` below (module's own
        // pre-existing discipline: `poll` already holds this lock for
        // its own, non-blocking `try_wait`) — `wirk run-deterministic`
        // drives exactly one Run per process, so no other caller shares
        // this executor while this blocks (`orient/child.md` §2's
        // "many concurrent Runs" is a future caller's concern, not this
        // one's, R1).
        let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        let state = runs
            .get_mut(&run.id)
            .ok_or_else(|| ChildExecutorError::UnknownRun(run.id.clone()))?;
        if state.claim_filed {
            return Ok(RunObservation::Running);
        }
        let status = state.child.wait().map_err(ChildExecutorError::Wait)?;
        self.finish_exit(run, state, status)
    }

    /// Shared post-exit handling for both `poll` (non-blocking,
    /// `try_wait`) and `wait` (blocking, `Child::wait`): kill the
    /// process group, join the stderr reader, and file the Claim on a
    /// clean exit (`orient/child.md` §3/§4).
    fn finish_exit(
        &self,
        run: &Run,
        state: &mut ChildState,
        status: std::process::ExitStatus,
    ) -> Result<RunObservation, ChildExecutorError> {
        kill_process_group(state.pgid);

        // Join the stderr reader thread before reading its tail, on
        // every exit path below (clean, non-zero, signalled) — all
        // three branch after this point, none before it (issue 279
        // VERIFY: `try_wait` reporting the child gone does not mean the
        // reader thread has finished draining the now-closing stderr
        // pipe into `stderr_tail`; reading the tail before the reader
        // catches up could silently observe an empty `detail` even
        // though the child wrote to stderr). `kill_process_group` above
        // already ensures nothing in this Run's process group can still
        // be holding the pipe's write end open, so this join is bounded
        // by the reader's own drain-to-EOF, not by a live process.
        if let Some(reader) = state.stderr_reader.take() {
            let _ = reader.join();
        }

        let tail =
            String::from_utf8_lossy(&state.stderr_tail.lock().unwrap_or_else(|p| p.into_inner()))
                .into_owned();

        if let Some(signal) = status.signal() {
            state.claim_filed = true; // no Claim to file for a killed child
            return Ok(RunObservation::Failed(FailureCause {
                status: Some(format!("signal {signal} ({})", signal_name(signal))),
                request_id: None,
                at: now(),
                detail: Some(tail),
            }));
        }

        let code = status.code().unwrap_or(-1);
        if code != 0 {
            state.claim_filed = true;
            return Ok(RunObservation::Failed(FailureCause {
                status: Some(code.to_string()),
                request_id: None,
                at: now(),
                detail: Some(tail),
            }));
        }

        // Clean exit: file the Claim ourselves — no actor exists for a
        // Deterministic Waypoint (0001 D4; `orient/child.md` §4).
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
            .map_err(ChildExecutorError::ClaimFiling)?;
        Ok(RunObservation::Running)
    }
}

/// `orient/child.md` §3, R3+R5: own process group
/// (`CommandExt::process_group(0)`, not wirkd's own group — sharing it
/// would make a group-kill of one Run take wirkd with it) plus
/// `libc::prctl(PR_SET_PDEATHSIG, SIGKILL)` armed in `pre_exec`,
/// matching sergeant-rs `child.rs:150-181`'s mechanism verbatim, not a
/// `nix` reimplementation of the same call.
fn harden_execution_child(command: &mut Command) {
    command.process_group(0);
    // SAFETY: this closure runs on the single forked child thread,
    // strictly after `fork` and strictly before `exec` — no other
    // thread exists yet in this process image, and no lock any other
    // thread held survives the fork to deadlock this one. Only
    // async-signal-safe libc calls are made (`prctl`, `getppid`,
    // `_exit`), no allocation, no `std` I/O — matching the SAFETY
    // discipline sergeant-rs `child.rs:150-159` documents for the same
    // call (`orient/child.md` §3, §6).
    unsafe {
        command.pre_exec(|| {
            let parent_before = libc::getppid();
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Closes the fork/prctl race (`orient/child.md` §3): if the
            // parent died between `fork` and arming `PDEATHSIG`, the
            // signal was never armed against a still-live parent and
            // never will be delivered for this death — better to
            // `_exit` now than exec into a permanently orphaned child.
            if libc::getppid() != parent_before {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

/// Kill-then-reap on every exit path, not only a deadline (`orient/
/// child.md` §1, §3: "a child that forked its own subprocess before
/// dying leaves that grandchild in the same pgid"). `pgid` is always
/// the child's own pid here (`process_group(0)`), so `-pgid` signals
/// the whole group it leads. Reaping the primary child itself already
/// happened inside `try_wait` returning `Some`; a grandchild is not
/// this process's child to `waitpid` on, only to signal.
fn kill_process_group(pgid: i32) {
    // SAFETY: a plain `kill(2)` call; `ESRCH` (already gone) is the
    // expected, ignored outcome on an already-exited group.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

fn spawn_stderr_reader(
    mut pipe: impl Read + Send + 'static,
    path: PathBuf,
    tail: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut file = File::create(&path).ok();
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Some(f) = file.as_mut() {
                        let _ = f.write_all(&buf[..n]);
                    }
                    let mut tail = tail.lock().unwrap_or_else(|p| p.into_inner());
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > STDERR_TAIL_BYTES {
                        let excess = tail.len() - STDERR_TAIL_BYTES;
                        tail.drain(0..excess);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

/// Names the common signals `FailureCause.status` might carry so a
/// journal reader sees `"signal 9 (SIGKILL)"`, not a bare number.
fn signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGKILL => "SIGKILL",
        libc::SIGTERM => "SIGTERM",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGABRT => "SIGABRT",
        libc::SIGINT => "SIGINT",
        libc::SIGBUS => "SIGBUS",
        _ => "unknown",
    }
}

fn now() -> Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Timestamp(ms as i64)
}
