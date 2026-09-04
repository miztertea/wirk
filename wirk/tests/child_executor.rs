//! `ChildExecutor` tests (item 5 W1; `orient/child.md` §5;
//! `orient/build-brief.md` §3 W1's `d5_1`-`d5_6`), real child processes
//! throughout, no sleeps as waits (bounded `try_wait`/poll loops only).
//!
//! Claim filing (`d5_1`, `d5_6b`) is proved live (0040 D127) against a
//! real `wirk wirkd` process: `wirk work submit --kind deterministic
//! --command ...` reserves a real Run whose Waypoint always requires
//! `report.md` by name (`wirkd/server.rs`'s hardcoded
//! `output_contract`, unconditional regardless of `--kind`) — `d5_1`'s
//! command writes it, so the real Claim validates; `d5_6b`'s command
//! does not, so the real Claim is genuinely refused
//! `MissingArtifact`, no scripted refusal needed. `run_verb.rs`'s own
//! `submit_actor`/`wait_for_pointer` shape, duplicated for a
//! Deterministic submit (R6: two submit helpers for two World kinds).
//!
//! `wirk` has no `lib.rs` (bin-only): `wirkd` and `executors` are
//! compiled into this test binary's own crate root via `#[path]`, the
//! established move (`wirkd_client.rs`, R2) rather than a library
//! target added purely for tests.
//! `wirk` has no `lib.rs` (bin-only): `wirkd` and `executors` are
//! compiled into this test binary's own crate root via `#[path]`, the
//! established move (`wirkd_client.rs`, R2) rather than a library
//! target added purely for tests.

#[path = "../src/executors/mod.rs"]
mod executors;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use executors::child::ChildExecutor;
use wirk_core::{
    DeterministicWorld, Executor, OutputContract, Run, RunId, RunObservation, RunState, WaypointId,
    WorkId, World, WorldHash,
};

// ---- shared fixtures ---------------------------------------------------

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("smoke/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: Default::default(),
    }
}

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// As `run_verb.rs`'s own `wait_for_pointer` (R6 duplicate — two test
/// binaries, no shared-utility crate for a few lines).
fn wait_for_pointer_live(estate: &Path) -> wirkd::WirkdPointer {
    let path = estate.join(".wirk").join("wirkd.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(bytes) = fs::read(&path)
            && let Ok(pointer) = serde_json::from_slice::<wirkd::WirkdPointer>(&bytes)
        {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "wirkd pointer file never appeared (readable) at {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// `wirk work submit --estate <estate> --intent <intent> --kind
/// deterministic --command <argv...> --base <base> --repo demo:write`,
/// parsing its `work_id <id> run_id <id> waypoint <id>` stdout line
/// (`run_verb.rs`'s own `submit_actor` shape, R6 duplicate — a
/// different World kind). The real wirkd's Waypoint (the "smoke"
/// Route's own hardcoded definition) always requires `report.md` by
/// name regardless of `--kind` or `--command` — the caller's own
/// command decides whether the later Claim honors that.
fn submit_deterministic(
    estate: &Path,
    base: &str,
    intent: &str,
    command: &[&str],
) -> (String, String, String) {
    let mut cmd = Command::new(wirk_bin());
    cmd.args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--intent",
            intent,
            "--kind",
            "deterministic",
            "--base",
            base,
        ])
        .args(["--repo", "demo:write", "--command"])
        .args(command);
    let output = cmd.output().expect("work submit runs");
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

/// Guards a spawned `wirk wirkd` child so a failed assertion still
/// kills and reaps it (ruling 0030), `run_verb.rs`'s own `KillOnDrop`
/// shape (R6 duplicate).
struct KillWirkdOnDrop(std::process::Child);

impl Drop for KillWirkdOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn deterministic_world(
    command: Vec<&str>,
    cwd: &Path,
    expected_artifacts: OutputContract,
) -> World {
    World::Deterministic(DeterministicWorld {
        command: command.into_iter().map(str::to_string).collect(),
        base_sha: "abc123".to_string(),
        cwd: cwd.to_path_buf(),
        env: BTreeMap::new(),
        expected_artifacts,
    })
}

/// Polls `condition` every 20 ms (`orient/child.md` §5's own step, the
/// no-sleep-as-a-wait answer to issue 359) until it returns `true` or
/// `deadline` elapses; returns whether it succeeded.
fn poll_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if condition() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

const POLL_DEADLINE: Duration = Duration::from_secs(5);

/// Kills the wrapped process by pid on drop, including during a
/// panicking unwind (`w1-fix` VERIFY probe (a): the death-signal test's
/// unhardened control `sleep 300` leaked exactly this way — an
/// assertion panicked ahead of the test's own explicit `kill -9`, so
/// the explicit cleanup line never ran). Any test in this file that
/// spawns a long-lived child wraps its pid in this immediately after
/// the pid is known, so a panic anywhere after that point still cannot
/// leak it. `SIGKILL`ing an already-dead pid is a harmless `ESRCH`.
struct KillPidOnDrop(i32);

impl Drop for KillPidOnDrop {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
        }
    }
}

// ---- d5_1: true completes and files a claim (live, 0040 D127) ----------

#[test]
fn d5_1_true_completes_by_claim() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();

    let mut guard = KillWirkdOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer_live(&estate);

    // The real Waypoint always requires `report.md` by name
    // (`wirkd/server.rs`'s hardcoded output_contract); this command
    // writes it into the World's own `cwd`, matching `expected_
    // artifacts` below so the real Claim validates.
    let (work_id, run_id, waypoint) = submit_deterministic(
        &estate,
        "abc123",
        "d5_1 tried live",
        &["sh", "-c", "echo hi > report.md"],
    );

    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let executor = ChildExecutor::new(estate.clone(), WorkId(work_id.clone()));
    let run = Run {
        id: RunId(run_id.clone()),
        waypoint: WaypointId(waypoint),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: Default::default(),
    };
    let world = deterministic_world(
        vec!["sh", "-c", "echo hi > report.md"],
        cwd.path(),
        OutputContract(vec![wirk_core::ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
    );
    executor.launch(&run, &world).expect("launch true");

    // Poll until the process exits and the executor's own claim-filing
    // tick fires (`poll` stays `Ok(Running)` on both a filed *and* a
    // refused Claim, 0028's `orient/child.md` §5 — the wirkd journal
    // is the decisive, real-service signal): a bounded poll on the
    // real wirkd's journal for `ClaimRecorded{Validated}`.
    let journal_path = estate.join("works").join(&work_id);
    let deadline = Instant::now() + POLL_DEADLINE;
    let claimed = loop {
        match executor.poll(&run) {
            Ok(RunObservation::Running) => {}
            other => panic!("expected Running throughout (no Completed variant), got {other:?}"),
        }
        if let Ok(journal) = wirk_core::Journal::open(&journal_path)
            && let Ok(events) = journal.replay()
            && events.iter().any(|e| {
                matches!(
                    &e.kind,
                    wirk_core::EventKind::ClaimRecorded {
                        verdict: wirk_core::ClaimVerdict::Validated,
                        ..
                    }
                )
            })
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        claimed,
        "the real wirkd's journal never recorded ClaimRecorded{{Validated}} for {run_id}"
    );

    // Teardown: stop wirkd, then let `guard`'s Drop reap it.
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
    let _ = guard.0.try_wait();
}

// ---- d5_2: nonzero exit is Failed with status and stderr ---------------

#[test]
fn d5_2_nonzero_exit_is_failed_with_status_and_stderr() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    // No wirkd needed: a nonzero exit never reaches the claim path.

    let executor = ChildExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
    let run = open_run("run-1");
    let world = deterministic_world(
        vec!["sh", "-c", "echo boom >&2; exit 3"],
        cwd.path(),
        OutputContract(Vec::new()),
    );
    executor.launch(&run, &world).expect("launch");

    let mut observed = None;
    poll_until(POLL_DEADLINE, || match executor.poll(&run) {
        Ok(RunObservation::Running) => false,
        other => {
            observed = Some(other);
            true
        }
    });

    match observed.expect("poll settled within the deadline") {
        Ok(RunObservation::Failed(cause)) => {
            assert_eq!(cause.status.as_deref(), Some("3"));
            assert!(
                cause.detail.as_deref().unwrap_or_default().contains("boom"),
                "detail was {:?}",
                cause.detail
            );
        }
        other => panic!("expected Failed(status 3), got {other:?}"),
    }
}

// ---- d5_3: signal death names the signal --------------------------------

#[test]
fn d5_3_signal_death_names_the_signal() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    let executor = ChildExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
    let run = open_run("run-1");
    // Self-signal (SIGKILL of its own pid): no external timing
    // dependency, and names the exact signal this test asserts on.
    let world = deterministic_world(
        vec!["sh", "-c", "kill -9 $$"],
        cwd.path(),
        OutputContract(Vec::new()),
    );
    executor.launch(&run, &world).expect("launch");

    let mut observed = None;
    poll_until(POLL_DEADLINE, || match executor.poll(&run) {
        Ok(RunObservation::Running) => false,
        other => {
            observed = Some(other);
            true
        }
    });

    match observed.expect("poll settled within the deadline") {
        Ok(RunObservation::Failed(cause)) => {
            let status = cause.status.unwrap_or_default();
            assert!(status.contains("SIGKILL"), "status was {status:?}");
        }
        other => panic!("expected Failed naming SIGKILL, got {other:?}"),
    }
}

// ---- d5_4: missing command word is a diagnostic, not a panic -----------

#[test]
fn d5_4_missing_command_word_gives_a_diagnostic_not_a_panic() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    let executor = ChildExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
    let run = open_run("run-1");
    let world = deterministic_world(
        vec!["/no/such/program-xyz-child-executor-test"],
        cwd.path(),
        OutputContract(Vec::new()),
    );

    let err = executor
        .launch(&run, &world)
        .expect_err("a nonexistent program must not spawn");
    assert!(matches!(
        err,
        executors::child::ChildExecutorError::Spawn(_)
    ));
}

// ---- d5_6: a Deterministic World without base_sha is refused -----------

#[test]
fn d5_6_a_deterministic_world_without_base_sha_is_refused() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    let executor = ChildExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
    let run = open_run("run-1");
    let world = World::Deterministic(DeterministicWorld {
        command: vec!["true".to_string()],
        base_sha: String::new(),
        cwd: cwd.path().to_path_buf(),
        env: BTreeMap::new(),
        expected_artifacts: OutputContract(Vec::new()),
    });

    let err = executor
        .launch(&run, &world)
        .expect_err("an empty base_sha must be refused (issue 285)");
    assert!(matches!(
        err,
        executors::child::ChildExecutorError::MissingBaseSha
    ));
}

// ---- d5_6b: a claim wirkd refuses is recorded, not silently accepted ---
//
// Named `d5_6b` (not `d5_7`, which `orient/build-brief.md` §2 reserves
// for the docker argv-builder test, W2): stands in for the on-disk
// "missing artifact" refusal a real wirkd would give
// (`wirkd/server.rs`'s `worktree_path_for_run` check) — deferred to W3
// per this file's own header and 0034's "Follow-ups carried" (the real
// check needs the Route-runner's real Run, not this file's scripted
// fixture). What this test does prove: `poll` surfaces a wirkd refusal
// as `Err(ClaimFiling)`, never as a fabricated `RunObservation::Failed`
// — `orient/child.md` §4's "a refusal is wirkd's verdict... not poll's
// to interpret".

#[test]
fn d5_6b_a_claim_wirkd_refuses_surfaces_as_claim_filing_error() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    let mut guard = KillWirkdOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer_live(&estate);

    // The real Waypoint always requires `report.md` by name; `true`
    // never writes it, so the real Claim is genuinely refused
    // `MissingArtifact` — no scripted refusal needed (0040 D127).
    let (work_id, run_id, waypoint) =
        submit_deterministic(&estate, "abc123", "d5_6b tried live", &["true"]);

    let executor = ChildExecutor::new(estate.clone(), WorkId(work_id));
    let run = Run {
        id: RunId(run_id),
        waypoint: WaypointId(waypoint),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: Default::default(),
    };
    let world = deterministic_world(vec!["true"], cwd.path(), OutputContract(Vec::new()));
    executor.launch(&run, &world).expect("launch true");

    let mut observed = None;
    poll_until(POLL_DEADLINE, || match executor.poll(&run) {
        Ok(RunObservation::Running) => false,
        other => {
            observed = Some(other);
            true
        }
    });
    match observed.expect("poll settled within the deadline") {
        Err(executors::child::ChildExecutorError::ClaimFiling(msg)) => {
            assert!(msg.contains("MissingArtifact"), "message was {msg:?}");
        }
        other => panic!("expected Err(ClaimFiling(..)), got {other:?}"),
    }

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
    let _ = guard.0.try_wait();
}

// ---- d5_5: process-group kill of wirkd leaves no child (death-signal) --
//
// `#[cfg(target_os = "linux")]` (`orient/build-brief.md` §3 W1): the
// death-signal coupling this proves (`PR_SET_PDEATHSIG`) is Linux-only.
// A helper role (`WIRK_CHILD_EXECUTOR_HELPER` set) re-executes this same
// test binary, filtered to exactly this test, to play "the process that
// dies" — a plain unit test process cannot prove parent-death coupling
// on itself (`orient/child.md` §5: the re-exec role-helper pattern from
// sergeant's `v1d_probe_child_lifecycle.rs`). The helper launches one
// hardened child via `ChildExecutor` and spawns one *unhardened* plain
// child as a control, writes both pids out, then blocks; the outer test
// `SIGKILL`s the helper process itself and polls for each pid's fate: the
// hardened child must be gone within the bound, the unhardened control
// must still be alive — so a pass cannot mean "it would have exited
// anyway" (`orient/child.md` §5).

#[cfg(target_os = "linux")]
mod death_signal {
    use super::*;

    const ROLE_ENV: &str = "WIRK_CHILD_EXECUTOR_HELPER";
    const HARDENED_PID_ENV: &str = "WIRK_CHILD_EXECUTOR_HELPER_HARDENED_PID_FILE";
    const CONTROL_PID_ENV: &str = "WIRK_CHILD_EXECUTOR_HELPER_CONTROL_PID_FILE";
    const ESTATE_ENV: &str = "WIRK_CHILD_EXECUTOR_HELPER_ESTATE";
    const CWD_ENV: &str = "WIRK_CHILD_EXECUTOR_HELPER_CWD";

    #[test]
    fn d5_5_a_process_group_kill_of_wirkd_leaves_no_child() {
        if std::env::var_os(ROLE_ENV).is_some() {
            run_helper_role();
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let hardened_pid_file = dir.path().join("hardened.pid");
        let control_pid_file = dir.path().join("control.pid");
        let estate = dir.path().join("estate");
        let cwd = dir.path().join("cwd");
        fs::create_dir_all(&estate).expect("mkdir estate");
        fs::create_dir_all(&cwd).expect("mkdir cwd");

        let mut helper = Command::new(std::env::current_exe().expect("current_exe"))
            // Fully qualified: this test lives in `mod death_signal`, so
            // libtest's `--exact` needs the module-prefixed path or it
            // silently matches nothing (0 tests run, not an error).
            .arg("death_signal::d5_5_a_process_group_kill_of_wirkd_leaves_no_child")
            .arg("--exact")
            .arg("--test-threads=1")
            .arg("--nocapture")
            .env(ROLE_ENV, "1")
            .env(HARDENED_PID_ENV, &hardened_pid_file)
            .env(CONTROL_PID_ENV, &control_pid_file)
            .env(ESTATE_ENV, &estate)
            .env(CWD_ENV, &cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn helper process");
        // Guarded from here: a panic anywhere below (an assertion, a
        // deadline) still kills the helper process on unwind, rather
        // than leaking it the way probe (a) demonstrated for the
        // control child below.
        let _helper_guard = KillPidOnDrop(helper.id() as i32);

        // Bounded wait for both pid files (the helper writes them,
        // then blocks) — never a fixed sleep guessing readiness.
        let hardened_pid = wait_for_pid_file(&hardened_pid_file);
        let control_pid = wait_for_pid_file(&control_pid_file);
        // Same guard for the unhardened control (`run_helper_role`
        // `mem::forget`s its own `Child` handle, so this pid is this
        // test's only remaining handle on it): the very leak probe (a)
        // found — a panic between here and the explicit cleanup below
        // left `sleep 300` running — is now impossible to reintroduce.
        let _control_guard = KillPidOnDrop(control_pid);

        // Play "wirkd dies": SIGKILL the helper process itself. Its own
        // exit status is irrelevant (it never returns from the block);
        // `wait()` only reaps it.
        unsafe {
            libc::kill(helper.id() as i32, libc::SIGKILL);
        }
        let _ = helper.wait();

        assert!(
            poll_until(POLL_DEADLINE, || !pid_alive(hardened_pid)),
            "hardened child (pid {hardened_pid}) survived the parent's SIGKILL"
        );
        assert!(
            pid_alive(control_pid),
            "unhardened control (pid {control_pid}) died too — the pass would prove nothing"
        );

        // Residue (0030): the control child outlives the helper by
        // design; explicit cleanup here (immediate, observable) stays
        // the primary path — `_control_guard`'s drop below is the
        // backstop for a panic, not a replacement for waiting on the
        // kill to actually take effect before the test returns.
        unsafe {
            libc::kill(control_pid, libc::SIGKILL);
        }
        poll_until(POLL_DEADLINE, || !pid_alive(control_pid));
    }

    fn wait_for_pid_file(path: &Path) -> i32 {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            if let Ok(text) = fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "helper never wrote {} within the deadline",
                path.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn pid_alive(pid: i32) -> bool {
        // SAFETY: `kill(pid, 0)` sends no signal, only probes
        // existence/permission — the standard liveness check.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Never returns: launches the hardened child, spawns the
    /// unhardened control, writes both pids, then blocks so the outer
    /// test can `SIGKILL` this whole process.
    fn run_helper_role() -> ! {
        let hardened_pid_file =
            PathBuf::from(std::env::var(HARDENED_PID_ENV).expect("pid file env"));
        let control_pid_file = PathBuf::from(std::env::var(CONTROL_PID_ENV).expect("pid file env"));
        let estate = PathBuf::from(std::env::var(ESTATE_ENV).expect("estate env"));
        let cwd = PathBuf::from(std::env::var(CWD_ENV).expect("cwd env"));

        let executor = ChildExecutor::new(estate, WorkId("helper-work".to_string()));
        let run = open_run("helper-run");
        let world = deterministic_world(vec!["sleep", "300"], &cwd, OutputContract(Vec::new()));
        executor.launch(&run, &world).expect("helper launch");
        let hardened_pid = executor.child_pid(&run.id).expect("hardened pid tracked");
        fs::write(&hardened_pid_file, hardened_pid.to_string()).expect("write hardened pid");

        // Unhardened control: a plain spawn, no process group, no
        // PDEATHSIG — proves the hardened case's death is the
        // mechanism, not coincidence.
        let control_child = Command::new("sleep")
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn unhardened control");
        fs::write(&control_pid_file, control_child.id().to_string()).expect("write control pid");
        // Detach: the OS process persists regardless of this `Child`
        // handle's lifetime; this thread never calls `wait` on it.
        std::mem::forget(control_child);

        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
}
