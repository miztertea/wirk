//! `DockerExecutor` tests (item 5 W2; `orient/docker.md` §5;
//! `orient/build-brief.md` §3 W2's `d5_7`-`d5_10`).
//!
//! `d5_7`/`d5_8` need no docker daemon (the argv builder is a pure
//! function; `base_sha` refusal happens before any `docker` invocation)
//! and always run. `d5_9`/`d5_10` are `#[ignore]`d unless
//! `WIRK_DOCKER_LIVE=1`, against `alpine:3.24` already on the box (no
//! pull path exists anywhere in `docker.rs`). `d5_9`'s wirkd half is
//! live (0040 D127): a real `wirk wirkd`, a real `wirk work submit
//! --kind deterministic` — `child_executor.rs`'s own move, duplicated
//! (R6, a third test binary); `d5_10` needs no wirkd at all (a nonzero
//! exit never reaches the claim path), unchanged.
//!
//! `wirk` has no `lib.rs` (bin-only): `wirkd` and `executors` are
//! compiled into this test binary's own crate root via `#[path]`, the
//! established move (`child_executor.rs`, R2).

#[path = "../src/executors/mod.rs"]
mod executors;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use executors::docker::{DockerExecutor, DockerExecutorError, create_argv};
use wirk_core::{
    Access, DeterministicWorld, EventId, EventKind, ExecutionTriple, Executor, Journal,
    OutputContract, RepositoryBinding, RouteId, Run, RunId, RunObservation, RunState, Timestamp,
    WaypointId, WorkId, World, WorldHash,
};
use wirkd::WirkdPointer;
use wirkd::server::{RunMatch, match_docker_runs, open_deterministic_runs};

// ---- shared fixtures (R2: same shape as `child_executor.rs`'s) --------

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

/// As `child_executor.rs`'s own `wait_for_pointer_live`/
/// `submit_deterministic` (R6 duplicate — a third test binary).
fn wait_for_pointer_live(estate: &Path) -> WirkdPointer {
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
        thread::sleep(Duration::from_millis(20));
    }
}

/// No `--intent`: removed from `wirk work submit` (p2-route-files W2,
/// J1) — this is the Route-less ad hoc `--kind deterministic --command`
/// shape (build-brief.md §7.3).
fn submit_deterministic(estate: &Path, base: &str, command: &[&str]) -> (String, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wirk"));
    cmd.args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--kind", "deterministic", "--base", base])
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

/// Polls `condition` every 100 ms (coarser than `child_executor.rs`'s
/// 20 ms: each tick here is at least one `docker inspect` subprocess,
/// not an in-process `try_wait`) until it returns `true` or `deadline`
/// elapses; returns whether it succeeded.
fn poll_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if condition() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// `docker rm -f`s the named container on drop, including during a
/// panicking unwind — the guard `orient/build-brief.md` §3 W2's own
/// verifier probe names ("with `WIRK_DOCKER_LIVE=1` locally, delete
/// `--rm` and confirm the live test's `docker ps -a` assertion catches
/// the leaked container"): `--rm` already removes a container that
/// exits normally, this is the backstop for a panic before that point.
/// Idempotent: `docker rm -f` on an already-gone name is a harmless
/// no-op, discarded here the same way `DockerExecutor::remove_owned`
/// discards it.
struct RemoveContainerOnDrop(String);

impl Drop for RemoveContainerOnDrop {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .arg("rm")
            .arg("-f")
            .arg(&self.0)
            .output();
    }
}

/// Kills the wrapped `wirk wirkd` child on drop, including during a
/// panicking unwind (ruling 0030: "no wirkd... survives the run that
/// started it" — found live by this item's own wrong-assertion probe on
/// `d5_9`, which panicked ahead of the test's own explicit `wirkd
/// stop`/`kill` and left a `wirk wirkd` process running against a
/// `/tmp` estate until this guard was added). `child_executor.rs`'s own
/// `KillWirkdOnDrop` shape (R6 duplicate — a third test binary).
struct KillWirkdOnDrop(std::process::Child);

impl Drop for KillWirkdOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn docker_managed_containers() -> String {
    let output = Command::new("docker")
        .arg("ps")
        .arg("-a")
        .arg("--filter")
        .arg("label=io.wirk.managed=true")
        .arg("--format")
        .arg("{{.Names}}")
        .output()
        .expect("docker ps -a");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

// ---- d5_7: docker create argv is exact and ordered (no daemon) --------

#[test]
fn d5_7_docker_create_argv_is_exact_and_ordered() {
    let mut env = BTreeMap::new();
    env.insert("FOO".to_string(), "bar".to_string());
    let det = DeterministicWorld {
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo hi > report.md".to_string(),
        ],
        base_sha: "abc123".to_string(),
        cwd: std::path::PathBuf::from("/var/tmp/wirk-estate/works/work-1/run-run-1/worktree"),
        env,
        expected_artifacts: OutputContract(Vec::new()),
    };
    let triple = ExecutionTriple {
        estate_root: "/var/tmp/wirk-estate".to_string(),
        work_id: WorkId("work-1".to_string()),
        run_id: RunId("run-1".to_string()),
    };

    let argv = create_argv("wirk-run-1", 4242, 1001, 1001, &det, &triple);

    let expected: Vec<String> = [
        "--name",
        "wirk-run-1",
        "--label",
        "io.wirk.managed=true",
        "--label",
        "io.wirk.run=run-1",
        "--label",
        "io.wirk.wirkd_pid=4242",
        "--rm",
        "--init",
        "--network",
        "none",
        "--user",
        "1001:1001",
        "--workdir",
        "/work",
        "--mount",
        "type=bind,source=/var/tmp/wirk-estate/works/work-1/run-run-1/worktree,target=/work",
        "-e",
        "WIRK_ESTATE_ROOT=/var/tmp/wirk-estate",
        "-e",
        "WIRK_WORK_ID=work-1",
        "-e",
        "WIRK_RUN_ID=run-1",
        "-e",
        "FOO=bar",
        "alpine:3.24",
        "sh",
        "-c",
        "echo hi > report.md",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    assert_eq!(argv, expected);
}

// ---- d5_8: a Deterministic World without base_sha is refused (docker) -

#[test]
fn d5_8_a_deterministic_world_without_base_sha_is_refused_docker() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    let executor = DockerExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
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
        .expect_err("an empty base_sha must be refused (issue 285), symmetric with d5_6");
    assert!(matches!(err, DockerExecutorError::MissingBaseSha));
}

// ---- d5_9/d5_10: gated live round trips --------------------------------

fn docker_live_enabled() -> bool {
    std::env::var("WIRK_DOCKER_LIVE").as_deref() == Ok("1")
}

#[test]
#[ignore]
fn d5_9_docker_live_round_trip_completes_by_claim() {
    if !docker_live_enabled() {
        eprintln!("skipped: set WIRK_DOCKER_LIVE=1 to run");
        return;
    }
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();

    // The wirkd half is real (0040 D127): a real `wirk wirkd`, a real
    // `wirk work submit --kind deterministic`, whose Waypoint always
    // requires `report.md` by name (`wirkd/server.rs`'s hardcoded
    // output_contract) — matching what this command actually writes.
    let mut wirkd_guard = KillWirkdOnDrop(
        std::process::Command::new(env!("CARGO_BIN_EXE_wirk"))
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer_live(&estate);
    let (work_id, run_id, waypoint) =
        submit_deterministic(&estate, "abc123", &["sh", "-c", "echo hi > report.md"]);

    let executor = DockerExecutor::new(estate.clone(), WorkId(work_id.clone()));
    let run = Run {
        id: RunId(run_id),
        waypoint: WaypointId(waypoint),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
        kind: Default::default(),
    };
    let artifacts = OutputContract(vec![wirk_core::ArtifactSpec {
        name: "report.md".to_string(),
        required: true,
    }]);
    // `cwd` is the estate root itself — matching what a real `wirk
    // run-deterministic` actually hands the executor (`main.rs`'s
    // `reserved_deterministic` reads the World wirkd journaled at
    // submit, whose `cwd` is `state.estate_root`, `server.rs::
    // handle_submit`); an ad-hoc, unrelated tempdir here would make
    // the container's real write land outside the Run's own journaled
    // worktree, which the boundary guard (W6) correctly refuses.
    let world = deterministic_world(vec!["sh", "-c", "echo hi > report.md"], &estate, artifacts);
    executor.launch(&run, &world).expect("launch");
    let container_name = executor
        .container_name(&run.id)
        .expect("container name recorded after launch");
    let _guard = RemoveContainerOnDrop(container_name.clone());

    // `poll` stays `Ok(Running)` on both a filed and a refused Claim
    // (`orient/child.md` §5's rule, shared by `DockerExecutor`); the
    // real wirkd's journal is the decisive, real-service signal.
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
        assert!(
            Instant::now() < deadline,
            "DockerExecutor never filed a claim within the deadline"
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert!(
        claimed,
        "the real wirkd's journal never recorded ClaimRecorded{{Validated}}"
    );
    assert!(
        estate.join("report.md").exists(),
        "the container's write through the /work bind mount must land on the host cwd"
    );

    // The guard above removes the container on a panic; here, on the
    // success path, `--rm` should already have removed it the instant
    // the container exited — assert that directly (decisive check: no
    // `io.wirk.managed` container survives).
    assert!(
        poll_until(Duration::from_secs(5), || !docker_managed_containers()
            .lines()
            .any(|name| name == container_name)),
        "container {container_name} was not removed by --rm"
    );

    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_wirk"))
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let _ = wirkd_guard.0.kill();
    let _ = wirkd_guard.0.wait();
}

#[test]
#[ignore]
fn d5_10_docker_live_nonzero_exit_is_failed_with_status() {
    if !docker_live_enabled() {
        eprintln!("skipped: set WIRK_DOCKER_LIVE=1 to run");
        return;
    }
    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    // No wirkd needed: a nonzero exit never reaches the claim path.

    let executor = DockerExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
    let run = open_run("run-1");
    let world = deterministic_world(
        vec!["sh", "-c", "echo boom; exit 1"],
        cwd.path(),
        OutputContract(Vec::new()),
    );
    executor.launch(&run, &world).expect("launch");
    let container_name = executor
        .container_name(&run.id)
        .expect("container name recorded after launch");
    let _guard = RemoveContainerOnDrop(container_name.clone());

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
            assert_eq!(cause.status.as_deref(), Some("1"));
            assert!(
                cause.detail.as_deref().unwrap_or_default().contains("boom"),
                "detail was {:?}",
                cause.detail
            );
        }
        other => panic!("expected Failed(status 1), got {other:?}"),
    }

    assert!(
        poll_until(Duration::from_secs(5), || !docker_managed_containers()
            .lines()
            .any(|name| name == container_name)),
        "container {container_name} was not removed by --rm"
    );
}

// ---- W5: the docker recovery sweep's matching logic (0035 D110) -------
//
// `open_deterministic_runs`/`match_docker_runs` are pure (no `docker`
// call in either): the docker listing is injected as a `HashSet` here,
// same as `orient/build-brief.md`'s own "(a) a unit test of the
// matching (containers listed, journals read, the three cases) with
// the docker listing injected" (R2: same journal-fixture shape
// `wirk-core/tests/needs_input.rs` already uses, duplicated per that
// file's own precedent — `wirk-core`'s tests are out of this wave's
// allow-list, `wirk`'s own crate has no such helper yet).

fn sweep_event(id: &str, work: &str, run: Option<&str>, kind: EventKind) -> wirk_core::Event {
    wirk_core::Event {
        id: EventId(id.to_string()),
        work: WorkId(work.to_string()),
        run: run.map(|r| RunId(r.to_string())),
        at: Timestamp(0),
        kind,
    }
}

fn sweep_work_submitted(waypoints: Vec<&str>) -> EventKind {
    EventKind::WorkSubmitted {
        route: RouteId("route-1".to_string()),
        repositories: vec![RepositoryBinding {
            name: "wirk".to_string(),
            access: Access::Write,
        }],
        intent: "run the thing".to_string(),
        waypoints: waypoints
            .into_iter()
            .map(|wp| WaypointId(wp.to_string()))
            .collect(),
        waypoint_defs: Vec::new(),
    }
}

fn sweep_waypoint_reserved(waypoint: &str, cwd: &Path) -> EventKind {
    EventKind::WaypointReserved {
        waypoint: WaypointId(waypoint.to_string()),
        world_hash: WorldHash("deadbeef".to_string()),
        world: World::Deterministic(DeterministicWorld {
            command: vec!["true".to_string()],
            base_sha: "abc123".to_string(),
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
            expected_artifacts: OutputContract(vec![wirk_core::ArtifactSpec {
                name: "report.md".to_string(),
                required: true,
            }]),
        }),
    }
}

fn sweep_run_opened(run: &str, waypoint: &str) -> EventKind {
    EventKind::RunOpened {
        run: RunId(run.to_string()),
        waypoint: WaypointId(waypoint.to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
    }
}

/// Writes one Work's journal (`<works_dir>/<work_id>/journal.ndjson`)
/// with `events` appended in order — the on-disk shape
/// `open_deterministic_runs` reads with `Journal::open`/`replay`, same
/// as a real `wirkd` ever produces.
fn write_sweep_journal(works_dir: &Path, work_id: &str, events: &[wirk_core::Event]) {
    let dir = works_dir.join(work_id);
    let mut journal = Journal::open(&dir).expect("journal opens");
    for event in events {
        journal.append(event).expect("journal appends");
    }
}

/// The three cases named in `orient/build-brief.md`'s W5 test (a): an
/// open Deterministic Run whose container the daemon still lists is
/// re-adopted; one the daemon has no record of (removed by its own
/// `--rm`, or never a docker Run at all) is vanished; a Run already
/// `Claimed` before the sweep ever runs is not an open Run at all and
/// never appears in either list. A managed container name matching no
/// open Run in any journal is reported separately, left alone by the
/// caller (`recover_docker_runs`'s own `eprintln!`, not exercised here
/// — this test pins the matching, not the stderr line).
#[test]
fn d5_11_sweep_matches_open_runs_against_the_injected_docker_listing() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    let works_dir = estate.path().join("works");
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    // Work "a": one open Deterministic Run whose container the daemon
    // still lists -- re-adopted.
    write_sweep_journal(
        &works_dir,
        "work-a",
        &[
            sweep_event("ev-a1", "work-a", None, sweep_work_submitted(vec!["wp-1"])),
            sweep_event(
                "ev-a2",
                "work-a",
                None,
                sweep_waypoint_reserved("wp-1", cwd.path()),
            ),
            sweep_event(
                "ev-a3",
                "work-a",
                Some("run-a"),
                sweep_run_opened("run-a", "wp-1"),
            ),
        ],
    );

    // Work "b": one open Deterministic Run whose container the daemon
    // has no record of -- vanished.
    write_sweep_journal(
        &works_dir,
        "work-b",
        &[
            sweep_event("ev-b1", "work-b", None, sweep_work_submitted(vec!["wp-1"])),
            sweep_event(
                "ev-b2",
                "work-b",
                None,
                sweep_waypoint_reserved("wp-1", cwd.path()),
            ),
            sweep_event(
                "ev-b3",
                "work-b",
                Some("run-b"),
                sweep_run_opened("run-b", "wp-1"),
            ),
        ],
    );

    // Work "c": a Run already Claimed before the sweep runs -- not
    // open, must not appear in either list even though the daemon
    // (deliberately) also lists its container name.
    write_sweep_journal(
        &works_dir,
        "work-c",
        &[
            sweep_event("ev-c1", "work-c", None, sweep_work_submitted(vec!["wp-1"])),
            sweep_event(
                "ev-c2",
                "work-c",
                None,
                sweep_waypoint_reserved("wp-1", cwd.path()),
            ),
            sweep_event(
                "ev-c3",
                "work-c",
                Some("run-c"),
                sweep_run_opened("run-c", "wp-1"),
            ),
            sweep_event(
                "ev-c4",
                "work-c",
                Some("run-c"),
                EventKind::ClaimRecorded {
                    claim: wirk_core::ClaimId("claim-c".to_string()),
                    claim_kind: wirk_core::ClaimKind::Done,
                    verdict: wirk_core::ClaimVerdict::Validated,
                },
            ),
        ],
    );

    let open_runs = open_deterministic_runs(estate.path());
    let open_run_ids: std::collections::BTreeSet<String> = open_runs
        .iter()
        .map(|(_, run_id, _)| run_id.0.clone())
        .collect();
    assert_eq!(
        open_run_ids,
        std::collections::BTreeSet::from(["run-a".to_string(), "run-b".to_string()]),
        "run-c is Claimed, not open, and must be excluded"
    );

    let managed: std::collections::HashSet<String> = [
        "wirk-run-a".to_string(),
        "wirk-run-c".to_string(),
        "wirk-run-extra".to_string(),
    ]
    .into_iter()
    .collect();
    let (matches, unmatched) = match_docker_runs(open_runs, &managed);

    assert_eq!(matches.len(), 2, "one match per open Run: {matches:?}");
    let mut reattached = None;
    let mut vanished = None;
    for m in &matches {
        match m {
            RunMatch::Reattach {
                run_id,
                container_name,
                ..
            } => reattached = Some((run_id.0.clone(), container_name.clone())),
            RunMatch::Vanished { run_id, .. } => vanished = Some(run_id.0.clone()),
        }
    }
    assert_eq!(
        reattached,
        Some(("run-a".to_string(), "wirk-run-a".to_string())),
        "run-a's container is in the managed listing: re-adopt"
    );
    assert_eq!(
        vanished,
        Some("run-b".to_string()),
        "run-b's container is not in the managed listing: vanished"
    );

    // "wirk-run-c" (Claimed, never an open Run) and "wirk-run-extra"
    // (never journaled at all) both match no open Run -- left alone.
    let unmatched: std::collections::BTreeSet<String> = unmatched.into_iter().collect();
    assert_eq!(
        unmatched,
        std::collections::BTreeSet::from(["wirk-run-c".to_string(), "wirk-run-extra".to_string()]),
    );
}

/// A restart with no open Runs at all must not scan/hang (the build
/// brief's own probe): an estate with no `works/` directory yet finds
/// nothing, and one whose only Work is already `Claimed` finds nothing
/// either.
#[test]
fn d5_12_sweep_finds_nothing_when_no_run_is_open() {
    let estate = tempfile::tempdir().expect("estate tempdir");
    assert!(
        open_deterministic_runs(estate.path()).is_empty(),
        "no works/ directory at all: nothing to recover"
    );

    let works_dir = estate.path().join("works");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    write_sweep_journal(
        &works_dir,
        "work-done",
        &[
            sweep_event(
                "ev-1",
                "work-done",
                None,
                sweep_work_submitted(vec!["wp-1"]),
            ),
            sweep_event(
                "ev-2",
                "work-done",
                None,
                sweep_waypoint_reserved("wp-1", cwd.path()),
            ),
            sweep_event(
                "ev-3",
                "work-done",
                Some("run-done"),
                sweep_run_opened("run-done", "wp-1"),
            ),
            sweep_event(
                "ev-4",
                "work-done",
                Some("run-done"),
                EventKind::ClaimRecorded {
                    claim: wirk_core::ClaimId("claim-done".to_string()),
                    claim_kind: wirk_core::ClaimKind::Done,
                    verdict: wirk_core::ClaimVerdict::Validated,
                },
            ),
        ],
    );
    assert!(
        open_deterministic_runs(estate.path()).is_empty(),
        "the only Work is already Claimed: nothing open to recover"
    );
}

// ---- W5: the docker recovery sweep, live (0035 D110) -------------------
//
// A real workload sized to genuinely outlive a `wirkd` kill (2 GiB
// through `dd`+`sha256sum`, measured on this box at ~10s under
// `alpine:3.24` — the size is the workload, not a timer, `orient/
// build-brief.md` W5's own instruction) — never a `sleep`. Both tests
// launch the container through a `DockerExecutor` constructed directly
// in this test process, the same technique `d5_9` already uses for its
// "the wirkd half is real" live round trip, and deliberately never call
// `.wait()`/`.poll()` on it: the whole point of `recover_docker_runs`
// is the case where nothing else is left to file the Run's outcome once
// `wirkd` dies (`orient/two-works.md` §2: "what breaks is only the path
// back to the journal, if run-deterministic is also gone or its
// claim-filing call to the dead wirkd fails") — calling `.wait()` here
// too would race the sweep's own `docker wait` thread for the same
// container's exit and could file the outcome twice, an unrelated
// hazard this pair of tests is built to avoid, not to exercise.

/// Polls `docker ps --filter name=<name> --format {{.Status}}` until it
/// starts with `Up` (a state read, not a timer) — the workload really
/// is running before `wirkd` is killed out from under it.
fn wait_for_container_up(name: &str, deadline: Duration) {
    let start = Instant::now();
    loop {
        let output = Command::new("docker")
            .arg("ps")
            .arg("--filter")
            .arg(format!("name={name}"))
            .arg("--format")
            .arg("{{.Status}}")
            .output()
            .expect("docker ps");
        if String::from_utf8_lossy(&output.stdout).starts_with("Up") {
            return;
        }
        assert!(
            start.elapsed() < deadline,
            "container {name} never reached Up within the deadline"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Reads `<estate>/.wirk/wirkd.json` until it parses **and** names
/// `expected_pid` — never the stale pointer a just-restarted wirkd's
/// predecessor left behind (the file is only overwritten once the new
/// process's own listener is bound and `write_pointer` runs, `server.rs`
/// module doc: "before wirkd does anything else observable").
fn wait_for_pointer_pid(estate: &Path, expected_pid: u32) -> WirkdPointer {
    let path = estate.join(".wirk").join("wirkd.json");
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        if let Ok(bytes) = fs::read(&path)
            && let Ok(pointer) = serde_json::from_slice::<WirkdPointer>(&bytes)
            && pointer.pid == expected_pid
        {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "wirkd pointer never named the restarted pid {expected_pid} at {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// `wirk wirkd start --estate <estate>` as a child, `KillWirkdOnDrop`-
/// guarded (R2, `d5_9`'s own helper shape).
fn spawn_wirkd(estate: &Path) -> KillWirkdOnDrop {
    KillWirkdOnDrop(
        Command::new(env!("CARGO_BIN_EXE_wirk"))
            .args(["wirkd", "start", "--estate"])
            .arg(estate)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    )
}

const REAL_WORKLOAD: &str =
    "dd if=/dev/urandom bs=1M count=2048 2>/dev/null | sha256sum > report.md";

fn journal_events(estate: &Path, work_id: &str) -> Vec<wirk_core::Event> {
    let journal_path = estate.join("works").join(work_id);
    wirk_core::Journal::open(&journal_path)
        .and_then(|journal| journal.replay())
        .unwrap_or_default()
}

/// (b): a docker Run outliving a `wirkd` kill is re-adopted at restart
/// and reaches `Claimed` with no other process ever filing its outcome.
#[test]
#[ignore]
fn d5_13_docker_live_wirkd_restart_reattaches_a_running_container_to_claimed() {
    if !docker_live_enabled() {
        eprintln!("skipped: set WIRK_DOCKER_LIVE=1 to run");
        return;
    }
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();

    let wirkd1 = spawn_wirkd(&estate);
    wait_for_pointer_live(&estate);
    let (work_id, run_id, waypoint) =
        submit_deterministic(&estate, "abc123", &["sh", "-c", REAL_WORKLOAD]);

    let executor = DockerExecutor::new(estate.clone(), WorkId(work_id.clone()));
    let run = open_run(&run_id);
    let run = Run {
        waypoint: WaypointId(waypoint),
        ..run
    };
    let artifacts = OutputContract(vec![wirk_core::ArtifactSpec {
        name: "report.md".to_string(),
        required: true,
    }]);
    let world = deterministic_world(vec!["sh", "-c", REAL_WORKLOAD], &estate, artifacts);
    executor.launch(&run, &world).expect("launch");
    let container_name = executor
        .container_name(&run.id)
        .expect("container name recorded after launch");
    let _guard = RemoveContainerOnDrop(container_name.clone());

    wait_for_container_up(&container_name, POLL_DEADLINE);

    // Ruling 0035 D110's own scenario: SIGKILL, no clean shutdown, no
    // pointer/socket cleanup left behind.
    let old_pid = wirkd1.0.id();
    Command::new("kill")
        .arg("-9")
        .arg(old_pid.to_string())
        .output()
        .expect("kill -9 runs");
    // `wirkd1` is intentionally left to drop normally below (its own
    // Drop's kill/wait on an already-dead pid is a harmless no-op, the
    // same discipline `RemoveContainerOnDrop` uses for an already-gone
    // container).

    let mut wirkd2 = spawn_wirkd(&estate);
    let new_pid = wirkd2.0.id();
    wait_for_pointer_pid(&estate, new_pid);

    let deadline = Instant::now() + Duration::from_secs(60);
    let events = loop {
        let events = journal_events(&estate, &work_id);
        if events.iter().any(|e| {
            e.run.as_ref().map(|r| r.0.as_str()) == Some(run_id.as_str())
                && matches!(
                    &e.kind,
                    EventKind::ClaimRecorded {
                        verdict: wirk_core::ClaimVerdict::Validated,
                        ..
                    }
                )
        }) {
            break events;
        }
        assert!(
            Instant::now() < deadline,
            "the restarted wirkd's sweep never re-adopted {container_name} to a validated Claim; \
             journal so far: {events:?}"
        );
        thread::sleep(Duration::from_millis(100));
    };

    let run_opened_count = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::RunOpened { run, .. } if run.0 == run_id))
        .count();
    let claimed_count = events
        .iter()
        .filter(|e| {
            e.run.as_ref().map(|r| r.0.as_str()) == Some(run_id.as_str())
                && matches!(
                    &e.kind,
                    EventKind::ClaimRecorded {
                        verdict: wirk_core::ClaimVerdict::Validated,
                        ..
                    }
                )
        })
        .count();
    // A deterministic Run never gets a `RunLaunched` event at all (only
    // `wirk run`'s actor path journals one, `wirk-core/src/lib.rs`'s own
    // `RunLaunched` doc) -- `RunOpened` is this Run's one "launched"
    // fact, and it is journaled exactly once, at submit.
    assert_eq!(run_opened_count, 1, "exactly one RunOpened: {events:?}");
    assert_eq!(
        claimed_count, 1,
        "exactly one validated ClaimRecorded, no duplicate from a racing second filer: {events:?}"
    );
    assert!(
        estate.join("report.md").exists(),
        "the container's declared artifact must exist on the host estate root"
    );

    assert!(
        poll_until(Duration::from_secs(5), || !docker_managed_containers()
            .lines()
            .any(|name| name == container_name)),
        "container {container_name} was not removed by --rm once the sweep observed its exit"
    );

    let stop = Command::new(env!("CARGO_BIN_EXE_wirk"))
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let _ = wirkd2.0.wait();
}

/// (c): the same scenario, but the container is removed by hand (`docker
/// rm -f`, simulating an operator or a genuinely lost container) before
/// `wirkd` restarts — the sweep journals `RunVanished`, never a hang.
#[test]
#[ignore]
fn d5_14_docker_live_wirkd_restart_journals_run_vanished_for_a_removed_container() {
    if !docker_live_enabled() {
        eprintln!("skipped: set WIRK_DOCKER_LIVE=1 to run");
        return;
    }
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();

    let wirkd1 = spawn_wirkd(&estate);
    wait_for_pointer_live(&estate);
    let (work_id, run_id, waypoint) =
        submit_deterministic(&estate, "abc123", &["sh", "-c", REAL_WORKLOAD]);

    let executor = DockerExecutor::new(estate.clone(), WorkId(work_id.clone()));
    let run = open_run(&run_id);
    let run = Run {
        waypoint: WaypointId(waypoint),
        ..run
    };
    let artifacts = OutputContract(vec![wirk_core::ArtifactSpec {
        name: "report.md".to_string(),
        required: true,
    }]);
    let world = deterministic_world(vec!["sh", "-c", REAL_WORKLOAD], &estate, artifacts);
    executor.launch(&run, &world).expect("launch");
    let container_name = executor
        .container_name(&run.id)
        .expect("container name recorded after launch");

    wait_for_container_up(&container_name, POLL_DEADLINE);

    let old_pid = wirkd1.0.id();
    Command::new("kill")
        .arg("-9")
        .arg(old_pid.to_string())
        .output()
        .expect("kill -9 runs");

    // Removed by hand, before the restart ever sees it (ruling 0044: a
    // state fact, not raced against a timer -- `docker rm -f` blocks
    // until the daemon confirms removal).
    let rm = Command::new("docker")
        .arg("rm")
        .arg("-f")
        .arg(&container_name)
        .output()
        .expect("docker rm -f runs");
    assert!(
        rm.status.success(),
        "docker rm -f failed: {}",
        String::from_utf8_lossy(&rm.stderr)
    );

    let mut wirkd2 = spawn_wirkd(&estate);
    let new_pid = wirkd2.0.id();
    wait_for_pointer_pid(&estate, new_pid);

    let deadline = Instant::now() + POLL_DEADLINE;
    let events = loop {
        let events = journal_events(&estate, &work_id);
        if events.iter().any(|e| {
            e.run.as_ref().map(|r| r.0.as_str()) == Some(run_id.as_str())
                && matches!(&e.kind, EventKind::RunVanished)
        }) {
            break events;
        }
        assert!(
            Instant::now() < deadline,
            "the restarted wirkd's sweep never journaled RunVanished for the removed container; \
             journal so far: {events:?}"
        );
        thread::sleep(Duration::from_millis(50));
    };

    let vanished_count = events
        .iter()
        .filter(|e| {
            e.run.as_ref().map(|r| r.0.as_str()) == Some(run_id.as_str())
                && matches!(&e.kind, EventKind::RunVanished)
        })
        .count();
    assert_eq!(vanished_count, 1, "exactly one RunVanished: {events:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::ClaimRecorded { .. })),
        "a removed container must never also be claimed: {events:?}"
    );

    let stop = Command::new(env!("CARGO_BIN_EXE_wirk"))
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(
        stop.status.success(),
        "wirkd stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let _ = wirkd2.0.wait();
}
