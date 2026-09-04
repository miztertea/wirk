//! `DockerExecutor` tests (item 5 W2; `orient/docker.md` §5;
//! `orient/build-brief.md` §3 W2's `d5_7`-`d5_10`).
//!
//! `d5_7`/`d5_8` need no docker daemon (the argv builder is a pure
//! function; `base_sha` refusal happens before any `docker` invocation)
//! and always run. `d5_9`/`d5_10` are `#[ignore]`d unless
//! `WIRK_DOCKER_LIVE=1`, against `alpine:3.24` already on the box (no
//! pull path exists anywhere in `docker.rs`) — same shape as
//! `child_executor.rs`'s claim-filing tests: a scripted fake wirkd on a
//! `UnixListener`, not a real `wirkd::server::run` (that file's own
//! header explains why: the real wirkd's only Route today is an
//! unrelated hardcoded Actor Waypoint).
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
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use executors::docker::{DockerExecutor, DockerExecutorError, create_argv};
use wirk_core::{
    DeterministicWorld, ExecutionTriple, Executor, OutputContract, Run, RunId, RunObservation,
    RunState, WaypointId, WorkId, World, WorldHash,
};
use wirkd::{Request, WirkdPointer};

// ---- shared fixtures (R2: same shape as `child_executor.rs`'s) --------

fn open_run(run_id: &str) -> Run {
    Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId("smoke/wp-1".to_string()),
        attempt: 1,
        world_hash: WorldHash("deadbeef".to_string()),
        state: RunState::Open,
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

fn write_pointer(estate: &Path, socket: &Path) {
    fs::create_dir_all(estate.join(".wirk")).expect("mkdir .wirk");
    let pointer = WirkdPointer {
        schema: "wirkd-pointer/1".to_string(),
        socket: socket.to_path_buf(),
        pid: std::process::id(),
        protocol_version: 1,
    };
    fs::write(
        estate.join(".wirk").join("wirkd.json"),
        serde_json::to_vec(&pointer).expect("serialize pointer"),
    )
    .expect("write pointer");
}

fn spawn_fake_wirkd(
    socket_path: &Path,
    scripted_reply: &'static str,
) -> (JoinHandle<()>, mpsc::Receiver<Request>) {
    let listener = UnixListener::bind(socket_path).expect("bind fake wirkd socket");
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if let Ok(request) = serde_json::from_str::<Request>(line.trim_end_matches(['\n', '\r'])) {
            let _ = tx.send(request);
        }
        let mut writer = &stream;
        let _ = writer.write_all(scripted_reply.as_bytes());
        let _ = writer.write_all(b"\n");
    });
    (handle, rx)
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
    let estate = tempfile::tempdir().expect("estate tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let socket = estate.path().join("wirkd.sock");
    let (_server, rx) = spawn_fake_wirkd(&socket, r#"{"ok":true,"result":{}}"#);
    write_pointer(estate.path(), &socket);

    let executor = DockerExecutor::new(estate.path().to_path_buf(), WorkId("work-1".to_string()));
    let run = open_run("run-1");
    let artifacts = OutputContract(vec![wirk_core::ArtifactSpec {
        name: "report.md".to_string(),
        required: true,
    }]);
    let world = deterministic_world(
        vec!["sh", "-c", "echo hi > report.md"],
        cwd.path(),
        artifacts,
    );
    executor.launch(&run, &world).expect("launch");
    let container_name = executor
        .container_name(&run.id)
        .expect("container name recorded after launch");
    let _guard = RemoveContainerOnDrop(container_name.clone());

    let deadline = Instant::now() + POLL_DEADLINE;
    let request = loop {
        match executor.poll(&run) {
            Ok(RunObservation::Running) => {}
            other => panic!("expected Running throughout (no Completed variant), got {other:?}"),
        }
        if let Ok(request) = rx.try_recv() {
            break request;
        }
        assert!(
            Instant::now() < deadline,
            "DockerExecutor never filed a claim within the deadline"
        );
        thread::sleep(Duration::from_millis(100));
    };

    assert_eq!(request.verb, wirkd::Verb::Claim);
    let payload: wirkd::ClaimPayload =
        serde_json::from_value(request.payload).expect("claim payload deserializes");
    assert!(matches!(payload.kind, wirk_core::ClaimKind::Done));
    assert_eq!(payload.triple.run_id, run.id);
    assert_eq!(payload.artifacts.len(), 1);
    assert!(payload.artifacts.contains_key("report.md"));
    assert!(
        cwd.path().join("report.md").exists(),
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
