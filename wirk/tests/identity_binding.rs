//! P3 identity binding regressions. Each case drives the compiled `wirk`
//! binary and a real `wirkd` child against disposable estates and Git repos;
//! no in-process server or substitute service stands in for either boundary.

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirk_core::{
    Access, ActorWorld, ArtifactSpec, Boundary, ClaimKind, Event, EventId, EventKind,
    ExecutionTriple, Journal, OutputContract, RepositoryBinding, RouteId, RunId, SourceBasis,
    Timestamp, WaypointDefinition, WaypointId, WaypointKind, WorkId, World, WorldHash,
};
use wirkd::{
    ClaimPayload, RecordPayload, Reply, Request, RetryPayload, StatusPayload, WirkdPointer,
};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_pointer(estate: &Path) -> WirkdPointer {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(pointer) = wirkd::client::locate(estate) {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "wirkd pointer did not appear under {}",
            estate.display()
        );
        std::thread::sleep(Duration::from_millis(20));
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
            .expect("spawn real wirkd"),
    );
    let pointer = wait_for_pointer(estate);
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
    assert!(
        child.0.wait().expect("reap wirkd").success(),
        "wirkd must exit cleanly"
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn scratch_repo(root: &Path) -> (PathBuf, String) {
    let repo = root.join("repo");
    fs::create_dir(&repo).expect("create repo dir");
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("seed.txt"), "seed\n").expect("write seed");
    git(&repo, &["add", "seed.txt"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=identity-test",
            "-c",
            "user.email=identity-test@example.test",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
    );
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    (repo, sha)
}

fn actor_route(root: &Path) -> PathBuf {
    let route = root.join("actor-route.json");
    fs::write(
        &route,
        r#"{"id":"identity","waypoints":[{"id":"identity/wp","kind":"Actor","intent":"identity","declared_outputs":[{"name":"report.md","required":true}],"boundary":["**"]}]}"#,
    )
    .expect("write route");
    route
}

fn parse_submit(output: &std::process::Output) -> (String, String, String) {
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<_> = stdout.split_whitespace().collect();
    let mut work = String::new();
    let mut run = String::new();
    let mut waypoint = String::new();
    for pair in words.chunks(2) {
        if let [key, value] = pair {
            match *key {
                "work_id" => work = (*value).to_owned(),
                "run_id" => run = (*value).to_owned(),
                "waypoint" => waypoint = (*value).to_owned(),
                _ => {}
            }
        }
    }
    assert!(!work.is_empty() && !run.is_empty() && !waypoint.is_empty());
    (work, run, waypoint)
}

fn submit_actor(estate: &Path, route: &Path, repo: &Path, sha: &str) -> (String, String, String) {
    parse_submit(
        &Command::new(wirk_bin())
            .args(["work", "submit", "--estate"])
            .arg(estate)
            .args(["--route"])
            .arg(route)
            .args(["--kind", "actor", "--repo", "probe:write", "--base"])
            .arg(sha)
            .args(["--repo-path"])
            .arg(repo)
            .output()
            .expect("actor submit runs"),
    )
}

fn submit_deterministic(estate: &Path) -> (String, String, String) {
    parse_submit(
        &Command::new(wirk_bin())
            .args(["work", "submit", "--estate"])
            .arg(estate)
            .args([
                "--kind",
                "deterministic",
                "--base",
                "probe-base",
                "--command",
                "sh",
                "-c",
                "true",
            ])
            .output()
            .expect("deterministic submit runs"),
    )
}

fn submit_deterministic_with(
    estate: &Path,
    base: &str,
    repository: &str,
    extra: &[&str],
) -> std::process::Output {
    let mut command = Command::new(wirk_bin());
    command
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args([
            "--kind",
            "deterministic",
            "--base",
            base,
            "--repo",
            repository,
        ]);
    command
        .args(extra)
        .args(["--command", "sh", "-c", "echo ok > report.md"]);
    command.output().expect("deterministic submit runs")
}

fn status(socket: &Path, work: &str) -> serde_json::Value {
    match wirkd::client::call(
        socket,
        &Request::status(StatusPayload::admin(WorkId(work.to_owned()))),
    )
    .expect("status call")
    {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!("status refused: {} {}", error.code, error.message),
    }
}

fn journal_len(estate: &Path, work: &str) -> usize {
    Journal::open(estate.join("works").join(work))
        .expect("open journal")
        .replay()
        .expect("replay journal")
        .len()
}

fn raw_event(work: &str, run: Option<&str>, at: i64, kind: EventKind) -> Event {
    Event {
        id: EventId(String::new()),
        work: WorkId(work.to_string()),
        run: run.map(|run| RunId(run.to_string())),
        at: Timestamp(at),
        kind,
    }
}

fn raw_work_submitted(waypoint: &str, repositories: Vec<RepositoryBinding>) -> EventKind {
    EventKind::WorkSubmitted {
        route: RouteId("identity".to_string()),
        repositories,
        intent: "identity".to_string(),
        waypoints: vec![WaypointId(waypoint.to_string())],
        waypoint_defs: vec![WaypointDefinition {
            id: WaypointId(waypoint.to_string()),
            kind: WaypointKind::Deterministic,
            declared_outputs: vec![ArtifactSpec {
                name: "report.md".to_string(),
                required: true,
            }],
            intent: None,
            command: Some(vec!["true".to_string()]),
            boundary: Boundary(Vec::new()),
            leaves: Vec::new(),
            required_child_outcomes: Vec::new(),
            selection: None,
            verifies: None,
            orient: None,
        }],
        parent: None,
        execution_repo: None,
        execution_identity: None,
    }
}

fn append_history(estate: &Path, work: &str, events: Vec<Event>) {
    let mut journal = Journal::open(estate.join("works").join(work)).expect("open raw journal");
    for event in events {
        journal.append(&event).expect("append raw event");
    }
}

fn question(socket: &Path, estate: &Path, work: &str, run: &str) -> Reply {
    wirkd::client::call(
        socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work.to_owned()),
                run_id: RunId(run.to_owned()),
            },
            kind: ClaimKind::Question("need retry".to_owned()),
            artifacts: BTreeMap::new(),
            outputs: Default::default(),
        }),
    )
    .expect("question claim call")
}

fn retry(socket: &Path, estate: &Path, work: &str, run: &str) -> Reply {
    wirkd::client::call(
        socket,
        &Request::retry(RetryPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work.to_owned()),
                run_id: RunId(run.to_owned()),
            },
        }),
    )
    .expect("retry call")
}

fn record(socket: &Path, work: &str, run: Option<&str>, kind: EventKind) -> Reply {
    wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work.to_owned()),
            run: run.map(|id| RunId(id.to_owned())),
            kind,
        }),
    )
    .expect("record call")
}

fn actor_world_for_run(status: &serde_json::Value, run: &str) -> serde_json::Value {
    status["runs"]
        .as_array()
        .expect("status runs")
        .iter()
        .find(|entry| entry["run"]["id"].as_str() == Some(run))
        .unwrap_or_else(|| panic!("status does not include {run}: {status}"))["world"]
        .clone()
}

fn materialize_legacy_actor(
    estate: &Path,
    socket: &Path,
    work: &str,
    run: &str,
    waypoint: &str,
    repo: &Path,
) {
    let initial = status(socket, work);
    let mut world: World = serde_json::from_value(initial["world"].clone()).expect("actor world");
    let World::Actor(actor) = &mut world else {
        panic!("expected actor World");
    };
    let worktree = estate.join("worktrees").join(work);
    fs::create_dir_all(worktree.parent().expect("worktree parent")).expect("worktree parent");
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &actor.branch,
            worktree.to_str().expect("utf8 worktree"),
            &actor.base_sha,
        ],
    );
    assert!(matches!(
        record(
            socket,
            work,
            Some(run),
            EventKind::WorktreeCreated {
                repo: actor.repository.clone(),
                base_sha: actor.base_sha.clone(),
            },
        ),
        Reply::Ok { .. }
    ));
    actor.worktree_path = worktree;
    let hash = WorldHash::of(&world);
    assert!(matches!(
        record(
            socket,
            work,
            Some(run),
            EventKind::WaypointReserved {
                waypoint: WaypointId(waypoint.to_owned()),
                world_hash: hash,
                world,
            },
        ),
        Reply::Ok { .. }
    ));
}

#[test]
fn historical_actor_run_keeps_its_world_across_retry_and_restart() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (repo, sha) = scratch_repo(root.path());
    let route = actor_route(root.path());
    let (child, pointer) = start_wirkd(&estate);
    let (work, old_run, waypoint) = submit_actor(&estate, &route, &repo, &sha);

    materialize_legacy_actor(&estate, &pointer.socket, &work, &old_run, &waypoint, &repo);
    assert!(matches!(
        question(&pointer.socket, &estate, &work, &old_run),
        Reply::Ok { .. }
    ));
    let before_retry = journal_len(&estate, &work);
    let reply = retry(&pointer.socket, &estate, &work, &old_run);
    let Reply::Ok { result, .. } = reply else {
        panic!("retry must set up history assertion: {reply:?}")
    };
    let new_run = result["new_run_id"]
        .as_str()
        .expect("new run id")
        .to_owned();
    assert_eq!(
        journal_len(&estate, &work),
        before_retry + 3,
        "retry appends reservation, supersession, open"
    );

    let before_restart = status(&pointer.socket, &work);
    let old_before = actor_world_for_run(&before_restart, &old_run);
    let new_before = actor_world_for_run(&before_restart, &new_run);

    stop_wirkd(&estate, child);
    let (_restarted, pointer) = start_wirkd(&estate);
    let after_restart = status(&pointer.socket, &work);
    assert_eq!(
        after_restart["events"].as_u64(),
        Some((before_retry + 3) as u64)
    );
    let old_after = actor_world_for_run(&after_restart, &old_run);
    let new_after = actor_world_for_run(&after_restart, &new_run);
    assert_eq!(
        old_after, old_before,
        "restart must preserve the old Run's reported World"
    );
    assert_eq!(
        new_after, new_before,
        "restart must preserve the new Run's reported World"
    );
    assert_eq!(
        old_after["Actor"]["triple"]["run_id"].as_str(),
        Some(old_run.as_str()),
        "old Run must retain its own historical triple across retry and restart"
    );
    assert_eq!(
        new_after["Actor"]["triple"]["run_id"].as_str(),
        Some(new_run.as_str()),
        "new Run must retain its own triple across restart"
    );
}

#[test]
fn retry_before_actor_materialization_never_reads_daemon_cwd() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (repo, submitted_sha) = scratch_repo(root.path());
    let route = actor_route(root.path());
    let (_child, pointer) = start_wirkd(&estate);
    let (work, run, _waypoint) = submit_actor(&estate, &route, &repo, &submitted_sha);
    assert!(matches!(
        question(&pointer.socket, &estate, &work, &run),
        Reply::Ok { .. }
    ));
    let before = journal_len(&estate, &work);
    let Reply::Ok { result, .. } = retry(&pointer.socket, &estate, &work, &run) else {
        panic!("retry unexpectedly refused")
    };
    let new_run = result["new_run_id"].as_str().expect("new run");
    let after = status(&pointer.socket, &work);
    assert_eq!(
        journal_len(&estate, &work),
        before + 3,
        "retry transition is fully journaled"
    );
    let new_world = actor_world_for_run(&after, new_run);
    assert_eq!(
        new_world["Actor"]["base_sha"].as_str(),
        Some(submitted_sha.as_str()),
        "unmaterialized retry must retain submitted repository SHA, never daemon cwd HEAD"
    );
    assert!(
        new_world["Actor"]["worktree_path"]
            .as_str()
            .is_some_and(str::is_empty),
        "retry must remain unmaterialized"
    );
}

#[test]
fn claim_refuses_a_different_estate_root() {
    let root = tempfile::tempdir().expect("temp root");
    let estate_a = root.path().join("estate-a");
    let estate_b = root.path().join("estate-b");
    fs::create_dir(&estate_a).expect("estate a");
    fs::create_dir(&estate_b).expect("estate b");
    let (_child, pointer) = start_wirkd(&estate_a);
    let (work, run, _waypoint) = submit_deterministic(&estate_a);
    let before = journal_len(&estate_a, &work);
    let reply = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate_b.display().to_string(),
                work_id: WorkId(work.clone()),
                run_id: RunId(run),
            },
            kind: ClaimKind::Question("wrong estate".to_owned()),
            artifacts: BTreeMap::new(),
            outputs: Default::default(),
        }),
    )
    .expect("claim call");
    let after = journal_len(&estate_a, &work);
    assert!(
        matches!(reply, Reply::Err { .. }) && after == before,
        "a Claim for another existing estate root must be refused without append: \\
         reply={reply:?}; journal before={before}, after={after}"
    );
}

#[test]
fn record_refuses_server_and_operator_owned_events() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (_child, pointer) = start_wirkd(&estate);
    let (work, _run, waypoint) = submit_deterministic(&estate);
    let before = journal_len(&estate, &work);
    let forged_open = record(
        &pointer.socket,
        &work,
        Some("run-forged"),
        EventKind::RunOpened {
            run: RunId("run-forged".to_owned()),
            waypoint: WaypointId(waypoint),
            attempt: 99,
            world_hash: WorldHash("forged".to_owned()),
        },
    );
    let forged_cancel = record(
        &pointer.socket,
        &work,
        None,
        EventKind::WorkCanceled {
            reason: Some("forged operator cancellation".to_owned()),
            caused_by: None,
        },
    );
    let after = journal_len(&estate, &work);
    assert!(
        matches!(forged_open, Reply::Err { .. })
            && matches!(forged_cancel, Reply::Err { .. })
            && after == before,
        "record must reject both forged server-owned RunOpened and operator-owned WorkCanceled: \\
         RunOpened={forged_open:?}; WorkCanceled={forged_cancel:?}; \\
         journal before={before}, after={after}"
    );
}

#[test]
fn actor_materialization_is_scoped_to_the_run() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (repo, sha) = scratch_repo(root.path());
    let route = actor_route(root.path());
    let (_child, pointer) = start_wirkd(&estate);
    let (work, run, waypoint) = submit_actor(&estate, &route, &repo, &sha);
    materialize_legacy_actor(&estate, &pointer.socket, &work, &run, &waypoint, &repo);

    let events = Journal::open(estate.join("works").join(&work))
        .expect("open journal")
        .replay()
        .expect("replay journal");
    let scoped = events.iter().any(|event| {
        event
            .run
            .as_ref()
            .is_some_and(|event_run| event_run.0 == run)
            && matches!(
                &event.kind,
                EventKind::WaypointReserved {
                    world: World::Actor(actor),
                    ..
                } if !actor.worktree_path.as_os_str().is_empty()
            )
    });
    assert!(
        scoped,
        "materialized reservation must carry the exact Run id"
    );
}

#[test]
fn retry_refuses_when_materialized_git_head_is_unavailable() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (repo, sha) = scratch_repo(root.path());
    let route = actor_route(root.path());
    let (_child, pointer) = start_wirkd(&estate);
    let (work, run, waypoint) = submit_actor(&estate, &route, &repo, &sha);
    materialize_legacy_actor(&estate, &pointer.socket, &work, &run, &waypoint, &repo);
    assert!(matches!(
        question(&pointer.socket, &estate, &work, &run),
        Reply::Ok { .. }
    ));

    let worktree = estate.join("worktrees").join(&work);
    fs::rename(worktree.join(".git"), worktree.join(".git-broken")).expect("break git link");
    let before = journal_len(&estate, &work);
    let refused = retry(&pointer.socket, &estate, &work, &run);
    assert!(matches!(refused, Reply::Err { .. }), "{refused:?}");
    assert_eq!(
        journal_len(&estate, &work),
        before,
        "failed inspection appends nothing"
    );
}

#[test]
fn record_refuses_unknown_mismatched_duplicate_and_terminal_run_transitions() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (_child, pointer) = start_wirkd(&estate);
    let output = submit_deterministic_with(
        &estate,
        "opaque-record",
        "demo:write",
        &["--source-basis", "output-only"],
    );
    let (work, run, _waypoint) = parse_submit(&output);
    let initial = journal_len(&estate, &work);

    let unknown = record(
        &pointer.socket,
        &work,
        Some("run-unknown"),
        EventKind::RunVanished,
    );
    let mismatched = record(
        &pointer.socket,
        &work,
        Some(&run),
        EventKind::RunLaunched {
            run: RunId("run-other".to_string()),
            actor_kind: Default::default(),
            selection: Default::default(),
            launch_argv: Vec::new(),
        },
    );
    assert!(matches!(unknown, Reply::Err { .. }));
    assert!(matches!(mismatched, Reply::Err { .. }));
    assert_eq!(journal_len(&estate, &work), initial);

    fs::write(estate.join("report.md"), "done\n").expect("write report");
    let done = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work.clone()),
                run_id: RunId(run.clone()),
            },
            kind: ClaimKind::Done,
            artifacts: BTreeMap::from([("report.md".to_string(), "report.md".to_string())]),
            outputs: Default::default(),
        }),
    )
    .expect("done claim");
    assert!(matches!(done, Reply::Ok { .. }), "{done:?}");
    let terminal_len = journal_len(&estate, &work);
    let terminal = record(&pointer.socket, &work, Some(&run), EventKind::RunVanished);
    assert!(matches!(terminal, Reply::Err { .. }));
    assert_eq!(journal_len(&estate, &work), terminal_len);

    let duplicate_output = submit_deterministic_with(
        &estate,
        "opaque-duplicate",
        "demo:write",
        &["--source-basis", "output-only"],
    );
    let (duplicate_work, duplicate_run, _) = parse_submit(&duplicate_output);
    assert!(matches!(
        record(
            &pointer.socket,
            &duplicate_work,
            Some(&duplicate_run),
            EventKind::RunVanished,
        ),
        Reply::Ok { .. }
    ));
    let after_first = journal_len(&estate, &duplicate_work);
    let duplicate = record(
        &pointer.socket,
        &duplicate_work,
        Some(&duplicate_run),
        EventKind::RunVanished,
    );
    assert!(matches!(duplicate, Reply::Err { .. }));
    assert_eq!(journal_len(&estate, &duplicate_work), after_first);

    let absent = "work-absent";
    let unknown_work = record(
        &pointer.socket,
        absent,
        Some("run-absent"),
        EventKind::RunVanished,
    );
    assert!(matches!(unknown_work, Reply::Err { .. }));
    assert!(!estate.join("works").join(absent).exists());
}

#[test]
fn historical_deterministic_run_keeps_its_world_across_retry_and_restart() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (_child, pointer) = start_wirkd(&estate);
    let output = submit_deterministic_with(
        &estate,
        "opaque-abc123",
        "demo:write",
        &["--source-basis", "output-only"],
    );
    let (work, run, _waypoint) = parse_submit(&output);

    let first = status(&pointer.socket, &work);
    let old_world = actor_world_for_run(&first, &run);
    assert_eq!(
        first["world"]["Deterministic"]["source_basis"]["kind"].as_str(),
        Some("output_only")
    );
    assert_eq!(
        first["world"]["Deterministic"]["source_basis"]["reference"].as_str(),
        Some("opaque-abc123")
    );

    assert!(matches!(
        question(&pointer.socket, &estate, &work, &run),
        Reply::Ok { .. }
    ));
    let before = journal_len(&estate, &work);
    let Reply::Ok { result, .. } = retry(&pointer.socket, &estate, &work, &run) else {
        panic!("output-only retry must reuse its declared reference")
    };
    let new_run = result["new_run_id"].as_str().expect("new run");
    let after = status(&pointer.socket, &work);
    assert_eq!(journal_len(&estate, &work), before + 3);
    assert_eq!(
        after["world"]["Deterministic"]["source_basis"]["reference"].as_str(),
        Some("opaque-abc123")
    );
    assert_eq!(
        after["world_binding"]["inspection"].as_str(),
        Some("output_only")
    );
    assert_eq!(
        after["runs"]
            .as_array()
            .expect("runs")
            .iter()
            .find(|entry| entry["run"]["id"].as_str() == Some(new_run))
            .expect("new run is reported")["world"]["Deterministic"]["source_basis"]["reference"]
            .as_str(),
        Some("opaque-abc123")
    );

    stop_wirkd(&estate, _child);
    let (_restarted, pointer) = start_wirkd(&estate);
    let restarted = status(&pointer.socket, &work);
    assert_eq!(actor_world_for_run(&restarted, &run), old_world);
    assert_eq!(
        restarted["world_binding"]["inspection"].as_str(),
        Some("output_only")
    );
}

#[test]
fn git_deterministic_unavailable_then_repaired_checkout_is_fail_closed() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (repo, sha) = scratch_repo(root.path());
    let (_child, pointer) = start_wirkd(&estate);
    let output = submit_deterministic_with(
        &estate,
        &sha,
        "probe:write",
        &[
            "--source-basis",
            "git",
            "--repo-path",
            repo.to_str().expect("utf8 repo"),
        ],
    );
    let (work, run, _waypoint) = parse_submit(&output);
    let before = journal_len(&estate, &work);

    // P3 execution-recovery item 1: this Deterministic Git-basis Work
    // now executes in its own worktree, not `repo` directly — the
    // artifact this test inspects git-unavailability/repair against
    // lives there.
    let worktree = estate.join("worktrees").join(&work);
    fs::write(worktree.join("report.md"), "present before inspection\n").expect("write output");
    let git_dir = worktree.join(".git");
    let hidden_git = worktree.join(".git-hidden");
    fs::rename(&git_dir, &hidden_git).expect("hide git metadata");
    let refused = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work.clone()),
                run_id: RunId(run.clone()),
            },
            kind: ClaimKind::Done,
            artifacts: BTreeMap::from([(
                "report.md".to_string(),
                worktree.join("report.md").display().to_string(),
            )]),
            outputs: Default::default(),
        }),
    )
    .expect("claim call");
    assert!(matches!(refused, Reply::Err { .. }), "{refused:?}");
    assert_eq!(
        journal_len(&estate, &work),
        before + 2,
        "the refusal is recorded"
    );

    fs::rename(&hidden_git, &git_dir).expect("repair git metadata");
    fs::write(worktree.join("report.md"), "repaired\n").expect("write output");
    let repaired = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work),
                run_id: RunId(run),
            },
            kind: ClaimKind::Done,
            artifacts: BTreeMap::from([(
                "report.md".to_string(),
                worktree.join("report.md").display().to_string(),
            )]),
            outputs: Default::default(),
        }),
    )
    .expect("repaired claim call");
    assert!(matches!(repaired, Reply::Ok { .. }), "{repaired:?}");
}

#[test]
fn legacy_unscoped_materialization_replays_for_its_exact_run() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (repo, sha) = scratch_repo(root.path());
    let work = "work-legacy";
    let run = "run-legacy";
    let waypoint = "identity/wp";
    let worktree = estate.join("worktrees").join(work);
    fs::create_dir_all(worktree.parent().expect("worktree parent")).expect("worktree parent");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "legacy-identity",
            worktree.to_str().expect("utf8 worktree"),
            &sha,
        ],
    );
    let initial = World::Actor(ActorWorld {
        repository: repo.display().to_string(),
        worktree_path: PathBuf::new(),
        branch: "legacy-identity".to_string(),
        base_sha: sha.clone(),
        source_basis: SourceBasis::Unknown,
        triple: ExecutionTriple {
            estate_root: estate.display().to_string(),
            work_id: WorkId(work.to_string()),
            run_id: RunId(run.to_string()),
        },
        intent: "legacy identity".to_string(),
        output_contract: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["**".to_string()]),
        review_targets: Vec::new(),
        evidence: None,
    });
    let hash = WorldHash::of(&initial);
    let mut updated = initial.clone();
    let World::Actor(actor) = &mut updated else {
        unreachable!()
    };
    actor.worktree_path = worktree.clone();
    append_history(
        &estate,
        work,
        vec![
            raw_event(
                work,
                None,
                1,
                EventKind::WorkSubmitted {
                    route: RouteId("identity".to_string()),
                    repositories: vec![RepositoryBinding {
                        name: repo.display().to_string(),
                        access: Access::Write,
                    }],
                    intent: "legacy identity".to_string(),
                    waypoints: vec![WaypointId(waypoint.to_string())],
                    waypoint_defs: vec![WaypointDefinition {
                        id: WaypointId(waypoint.to_string()),
                        kind: WaypointKind::Actor,
                        declared_outputs: vec![ArtifactSpec {
                            name: "report.md".to_string(),
                            required: true,
                        }],
                        intent: Some("legacy identity".to_string()),
                        command: None,
                        boundary: Boundary(vec!["**".to_string()]),
                        leaves: Vec::new(),
                        required_child_outcomes: Vec::new(),
                        selection: None,
                        verifies: None,
                        orient: None,
                    }],
                    parent: None,
                    execution_repo: None,
                    execution_identity: None,
                },
            ),
            raw_event(
                work,
                None,
                2,
                EventKind::WaypointReserved {
                    waypoint: WaypointId(waypoint.to_string()),
                    world_hash: hash.clone(),
                    world: initial,
                },
            ),
            raw_event(
                work,
                Some(run),
                3,
                EventKind::RunOpened {
                    run: RunId(run.to_string()),
                    waypoint: WaypointId(waypoint.to_string()),
                    attempt: 1,
                    world_hash: hash.clone(),
                },
            ),
            raw_event(
                work,
                Some(run),
                4,
                EventKind::WorktreeCreated {
                    repo: repo.display().to_string(),
                    base_sha: sha,
                },
            ),
            raw_event(
                work,
                None,
                5,
                EventKind::WaypointReserved {
                    waypoint: WaypointId(waypoint.to_string()),
                    world_hash: hash,
                    world: updated,
                },
            ),
        ],
    );

    let (child, pointer) = start_wirkd(&estate);
    let before = status(&pointer.socket, work);
    assert_eq!(before["world_binding"]["state"].as_str(), Some("resolved"));
    assert_eq!(before["world_binding"]["inspection"].as_str(), Some("git"));
    assert_eq!(
        before["world_binding"]["legacy_basis"].as_bool(),
        Some(true)
    );
    assert_eq!(
        before["world"]["Actor"]["triple"]["run_id"].as_str(),
        Some(run)
    );
    fs::write(worktree.join("report.md"), "legacy works\n").expect("write legacy output");
    let claim = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work.to_string()),
                run_id: RunId(run.to_string()),
            },
            kind: ClaimKind::Done,
            artifacts: BTreeMap::from([("report.md".to_string(), "report.md".to_string())]),
            outputs: Default::default(),
        }),
    )
    .expect("legacy claim");
    assert!(matches!(claim, Reply::Ok { .. }), "{claim:?}");
    stop_wirkd(&estate, child);
    let (_restarted, pointer) = start_wirkd(&estate);
    let after = status(&pointer.socket, work);
    assert_eq!(after["world_binding"]["inspection"].as_str(), Some("git"));
}

#[test]
fn ambiguous_legacy_binding_is_reported_unavailable() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let work = "work-ambiguous";
    let run = "run-ambiguous";
    let waypoint = "identity/wp";
    let world = World::Deterministic(wirk_core::DeterministicWorld {
        command: vec!["true".to_string()],
        base_sha: "opaque-or-sha".to_string(),
        source_basis: SourceBasis::Unknown,
        cwd: estate.clone(),
        env: BTreeMap::new(),
        expected_artifacts: OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
    });
    let hash = WorldHash::of(&world);
    append_history(
        &estate,
        work,
        vec![
            raw_event(
                work,
                None,
                1,
                raw_work_submitted(
                    waypoint,
                    vec![RepositoryBinding {
                        name: "demo".to_string(),
                        access: Access::Write,
                    }],
                ),
            ),
            raw_event(
                work,
                None,
                2,
                EventKind::WaypointReserved {
                    waypoint: WaypointId(waypoint.to_string()),
                    world_hash: hash.clone(),
                    world,
                },
            ),
            raw_event(
                work,
                Some(run),
                3,
                EventKind::RunOpened {
                    run: RunId(run.to_string()),
                    waypoint: WaypointId(waypoint.to_string()),
                    attempt: 1,
                    world_hash: hash,
                },
            ),
            raw_event(
                work,
                Some(run),
                4,
                EventKind::RunFailed {
                    cause: wirk_core::FailureCause {
                        status: Some("1".to_string()),
                        request_id: None,
                        at: Timestamp(4),
                        detail: Some("legacy failure".to_string()),
                    },
                },
            ),
        ],
    );
    fs::write(estate.join("report.md"), "present\n").expect("write output");

    let (child, pointer) = start_wirkd(&estate);
    let before = status(&pointer.socket, work);
    assert!(before["world"].is_null());
    assert_eq!(
        before["world_binding"]["state"].as_str(),
        Some("unavailable")
    );
    let before_retry = journal_len(&estate, work);
    let refused = retry(&pointer.socket, &estate, work, run);
    assert!(matches!(refused, Reply::Err { .. }));
    assert_eq!(journal_len(&estate, work), before_retry);

    stop_wirkd(&estate, child);
    let (_restarted, pointer) = start_wirkd(&estate);
    let restarted = status(&pointer.socket, work);
    assert!(restarted["world"].is_null());
    assert_eq!(
        restarted["world_binding"]["state"].as_str(),
        Some("unavailable")
    );
}

#[test]
fn output_only_read_binding_is_rejected_before_run_open() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (_child, _pointer) = start_wirkd(&estate);
    let before = fs::read_dir(estate.join("works"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    let output = submit_deterministic_with(
        &estate,
        "opaque-input",
        "demo:read",
        &["--source-basis", "output-only"],
    );
    assert!(
        !output.status.success(),
        "incompatible submit unexpectedly opened a Run"
    );
    let after = fs::read_dir(estate.join("works"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(after, before, "refusal must not create a Work journal");
}

/// P3 (0069 correction, `FINAL-REFUSAL-CORRECTION.md` item 2): a
/// Question carrying supplied artifacts, filed against a never-
/// materialized Actor Run, must refuse `ValidationUnavailable` with a
/// direct no-inspectable-checkout reason *before* artifact existence,
/// lexical normalization, or canonicalization ever runs — none of
/// those may consult the daemon's own current working directory. The
/// daemon here is spawned with an explicit, controlled `current_dir`
/// holding a real file and a real symlink so an existing/missing,
/// relative/absolute, and symlink artifact case can each be proven not
/// to depend on it: every case refuses the same way, for the same
/// reason, whether or not a same-named file happens to sit at the
/// daemon's cwd.
#[test]
fn question_before_actor_materialization_refuses_before_touching_daemon_cwd() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir(&estate).expect("estate");
    let (repo, submitted_sha) = scratch_repo(root.path());
    let route = actor_route(root.path());

    // A sentinel directory that becomes the daemon's own current
    // working directory — a relative artifact path that happens to
    // name a real file (or symlink) there must never change the
    // outcome once the Actor checkout has not materialized.
    let daemon_cwd = root.path().join("daemon-cwd");
    fs::create_dir(&daemon_cwd).expect("daemon cwd dir");
    fs::write(daemon_cwd.join("sentinel.txt"), b"should never be read\n").expect("sentinel file");
    std::os::unix::fs::symlink(
        daemon_cwd.join("sentinel.txt"),
        daemon_cwd.join("sentinel-link.txt"),
    )
    .expect("sentinel symlink");

    let child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .current_dir(&daemon_cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn real wirkd with a controlled cwd"),
    );
    let pointer = wait_for_pointer(&estate);

    let (work, run, _waypoint) = submit_actor(&estate, &route, &repo, &submitted_sha);

    // Existing-at-daemon-cwd (relative), missing (relative), absolute
    // (elsewhere, missing), and a symlink sitting at the daemon's own
    // cwd: all four must refuse identically.
    let elsewhere_missing = root.path().join("nowhere.txt");
    let cases: Vec<(&str, String)> = vec![
        ("sentinel.txt", "sentinel.txt".to_string()),
        ("missing.txt", "missing.txt".to_string()),
        ("absolute.txt", elsewhere_missing.display().to_string()),
        ("sentinel-link.txt", "sentinel-link.txt".to_string()),
    ];
    for (name, path) in &cases {
        let reply = wirkd::client::call(
            &pointer.socket,
            &Request::claim(ClaimPayload {
                triple: ExecutionTriple {
                    estate_root: estate.display().to_string(),
                    work_id: WorkId(work.clone()),
                    run_id: RunId(run.clone()),
                },
                kind: ClaimKind::Question("need retry".to_owned()),
                artifacts: BTreeMap::from([(name.to_string(), path.clone())]),
                outputs: Default::default(),
            }),
        )
        .expect("question claim with artifact call");
        match reply {
            Reply::Err { error, .. } => {
                assert_eq!(
                    error.code, "ValidationUnavailable",
                    "case {name}={path}: expected ValidationUnavailable, got {error:?}"
                );
                assert!(
                    error.message.to_lowercase().contains("materialized"),
                    "case {name}={path}: expected a direct no-inspectable-checkout \
                     reason naming materialization, got: {}",
                    error.message
                );
            }
            other => panic!("case {name}={path}: expected a refusal, got: {other:?}"),
        }
    }

    // Artifact-free Question remains valid — nothing here to inspect.
    assert!(matches!(
        question(&pointer.socket, &estate, &work, &run),
        Reply::Ok { .. }
    ));

    stop_wirkd(&estate, child);
}

/// `wirk work submit --kind actor --repo-path <path>` (the immediate,
/// eagerly-Git-verified shape real actor Runs use) with more than one
/// `--repo` binding, naming which one is the execution checkout
/// explicitly rather than leaving it to `.first()`.
fn submit_multi_repo_actor(
    estate: &Path,
    route: &Path,
    execution_repo_path: &Path,
    sha: &str,
    repo_flags: &[&str],
    execution_repo_name: &str,
) -> (String, String, String) {
    let mut cmd = Command::new(wirk_bin());
    cmd.args(["work", "submit", "--estate"]).arg(estate);
    for flag in repo_flags {
        cmd.args(["--repo", flag]);
    }
    cmd.args(["--execution-repo", execution_repo_name]);
    cmd.args(["--kind", "actor", "--repo-path"])
        .arg(execution_repo_path);
    cmd.args(["--base"]).arg(sha);
    cmd.args(["--route"]).arg(route);
    parse_submit(&cmd.output().expect("multi-repo actor submit runs"))
}

fn claim(estate: &Path, work_id: &str, run_id: &str, args: &[&str]) -> (Option<i32>, String) {
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
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// BUILD-BRIEF.md's own decisive check, direct: "Never select Read/Write
/// from repositories.first(). Both binding orders must be equivalent."
/// Ruling 0090's `is_read_binding` fix reads `Work.execution_repo` (set
/// by `resolve_execution_repo` at submit time for every submit shape),
/// never `ActorWorld.repository` or list position — this proves it
/// against the *immediate* `--kind actor --repo-path` shape specifically
/// (where `ActorWorld.repository` is a bare checkout path, not a
/// binding name, so matching against it would silently miss and fall
/// back to `.first()` for exactly this real-actor submit shape). A
/// genuinely Read execution checkout refuses any change at all,
/// identically, whichever position it is declared in.
#[test]
fn both_repository_binding_orders_refuse_identically_on_the_read_execution_repo() {
    let root = tempfile::tempdir().expect("temp root");
    let estate = root.path().join("estate");
    fs::create_dir_all(&estate).expect("create estate");
    let (wirkd_child, pointer) = start_wirkd(&estate);
    let route = actor_route(root.path());

    for (order_label, repo_flags) in [
        ("write-first", ["wirk:write", "workspace:read"]),
        ("read-first", ["workspace:read", "wirk:write"]),
    ] {
        let case_root = tempfile::tempdir().expect("case root");
        let (read_repo, sha) = scratch_repo(case_root.path());
        let (work, run, waypoint) =
            submit_multi_repo_actor(&estate, &route, &read_repo, &sha, &repo_flags, "workspace");
        materialize_legacy_actor(&estate, &pointer.socket, &work, &run, &waypoint, &read_repo);

        // A Read execution checkout refuses *any* change (0050 D150),
        // even one that exactly matches a declared output.
        fs::write(
            estate.join("worktrees").join(&work).join("report.md"),
            "not really produced under a Write binding\n",
        )
        .expect("write report.md into the Read execution checkout");

        let (code, out) = claim(&estate, &work, &run, &["--artifact", "report.md=report.md"]);
        assert_ne!(
            code,
            Some(0),
            "[{order_label}] a Read execution checkout must refuse any change, got: {out}"
        );
        assert!(
            out.contains("OutOfBoundary"),
            "[{order_label}] expected OutOfBoundary, got: {out}"
        );
    }

    stop_wirkd(&estate, wirkd_child);
}
