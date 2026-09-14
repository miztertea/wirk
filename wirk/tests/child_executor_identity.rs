//! `ChildExecutor` real-process identity tests: a real deterministic
//! child must see this Run's own `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/
//! `WIRK_RUN_ID`, authoritative over both an absent/foreign caller
//! environment and a Route-declared `det.env` entry that happens to
//! name one of those three keys.
//!
//! Isolated in its own test binary, `wirk-herdr/tests/
//! opencode_driver_env.rs`'s own precedent (R2): `std::env::set_var`/
//! `remove_var` is process-wide and cargo runs one file's tests on
//! threads of a single process, so a file that mutates the *caller's*
//! own environment has to own that isolation. Here two tests share the
//! same binary — both need the mutation, not just one — so `ENV_LOCK`
//! serializes them instead of splitting into two more files.
//!
//! No wirkd is started: these tests assert on the real child's own
//! environment, written to a file inside its `cwd`, before it exits —
//! the Claim attempt that follows (`ChildExecutor::wait`'s own
//! post-exit step) has nothing to file a Claim *with* (no daemon) and
//! its outcome is irrelevant to what's being checked here, matching
//! `docker_executor.rs`'s own `d5_10` ("No wirkd needed").

#[path = "../src/executors/mod.rs"]
mod executors;
use wirk::wirkd;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use executors::child::ChildExecutor;
use wirk_core::{
    DeterministicWorld, Executor, OutputContract, Run, RunId, RunState, WaypointId, WorkId, World,
    WorldHash,
};

/// Serializes the two tests below against each other's process-wide
/// `std::env::set_var`/`remove_var` calls (module doc).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("smoke/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: Default::default(),
        selection: Default::default(),
        launched: false,
        launch_requested: false,
        launch_argv: Vec::new(),
        launch_attempt: None,
        expansions: Vec::new(),
        contract_delivery: None,
        claim_hook: None,
    }
}

fn deterministic_world(command: Vec<&str>, cwd: &Path, env: BTreeMap<String, String>) -> World {
    World::Deterministic(DeterministicWorld {
        command: command.into_iter().map(str::to_string).collect(),
        base_sha: "abc123".to_string(),
        source_basis: wirk_core::SourceBasis::OutputOnly {
            reference: "abc123".to_string(),
        },
        cwd: cwd.to_path_buf(),
        env,
        expected_artifacts: OutputContract(Vec::new()),
    })
}

/// Reads `identity.txt` from a finished child's own `cwd`. `wait`
/// already blocked on the child's exit before this is called, so the
/// file is written; the short poll only covers the write's own flush,
/// not process completion.
fn read_identity(cwd: &Path) -> Vec<String> {
    let path = cwd.join("identity.txt");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            return text.lines().map(str::to_string).collect();
        }
        assert!(
            Instant::now() < deadline,
            "identity.txt never appeared at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---- caller identity absent ---------------------------------------------

#[test]
fn a_real_child_gets_its_own_identity_when_the_callers_environment_has_none() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for var in ["WIRK_ESTATE_ROOT", "WIRK_WORK_ID", "WIRK_RUN_ID"] {
        unsafe {
            std::env::remove_var(var);
        }
    }

    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let executor = ChildExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
    let run = open_run("run-1");
    let world = deterministic_world(
        vec![
            "sh",
            "-c",
            "printf '%s\\n%s\\n%s\\n' \"$WIRK_ESTATE_ROOT\" \"$WIRK_WORK_ID\" \
             \"$WIRK_RUN_ID\" > identity.txt",
        ],
        cwd.path(),
        BTreeMap::new(),
    );
    executor.launch(&run, &world).expect("launch");
    // Claim filing fails (no wirkd here) and that is fine: only the
    // real child's own written environment is under test.
    let _ = executor.wait(&run);

    let lines = read_identity(cwd.path());
    assert_eq!(
        lines,
        vec![
            estate.path().display().to_string(),
            "work-1".to_string(),
            "run-1".to_string(),
        ],
        "a real child launched with no WIRK_* identity in the caller's own environment \
         must still see this Run's own triple: {lines:?}"
    );
}

// ---- foreign caller + authored det.env collision -------------------------

#[test]
fn a_real_child_keeps_its_own_identity_over_a_foreign_caller_and_an_authored_collision() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe {
        std::env::set_var("WIRK_ESTATE_ROOT", "/nonexistent/foreign-estate");
        std::env::set_var("WIRK_WORK_ID", "foreign-work");
        std::env::set_var("WIRK_RUN_ID", "foreign-run");
    }

    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let executor = ChildExecutor::new(estate.path().to_path_buf(), WorkId("work-2".to_string()));
    let run = open_run("run-2");

    // A Route-declared det.env with its own collision on one of the
    // three keys, plus one unrelated var that must still pass through
    // untouched.
    let mut env = BTreeMap::new();
    env.insert("WIRK_RUN_ID".to_string(), "det-env-run".to_string());
    env.insert("KEEP_ME".to_string(), "still-here".to_string());
    let world = deterministic_world(
        vec![
            "sh",
            "-c",
            "printf '%s\\n%s\\n%s\\n%s\\n' \"$WIRK_ESTATE_ROOT\" \"$WIRK_WORK_ID\" \
             \"$WIRK_RUN_ID\" \"$KEEP_ME\" > identity.txt",
        ],
        cwd.path(),
        env,
    );
    executor.launch(&run, &world).expect("launch");
    let _ = executor.wait(&run);

    let lines = read_identity(cwd.path());
    assert_eq!(
        lines,
        vec![
            estate.path().display().to_string(),
            "work-2".to_string(),
            "run-2".to_string(),
            "still-here".to_string(),
        ],
        "the real child's own triple must win over both a foreign inherited caller \
         value and det.env's authored collision, while an unrelated det.env entry \
         still reaches the child: {lines:?}"
    );

    unsafe {
        std::env::remove_var("WIRK_ESTATE_ROOT");
        std::env::remove_var("WIRK_WORK_ID");
        std::env::remove_var("WIRK_RUN_ID");
    }
}
