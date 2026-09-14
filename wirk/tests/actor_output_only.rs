//! Output-only (`SourceBasis::OutputOnly`) Actor execution: a Work
//! whose sources are Read grants and whose execution happens in an
//! owned directory rather than a Git checkout.
//!
//! What each check holds to, and what it would have to see to fail:
//!
//! * a submission naming *two* Read sources is admitted, and neither
//!   becomes "the execution checkout" — the refusal to beat here is
//!   `AmbiguousExecutionRepository`, which a second Read binding drew
//!   before the basis was ever consulted;
//! * a Read grant actually reaches Atlas admission, and a source the
//!   Work is *not* bound to is still denied — admitting Read bindings
//!   must widen nothing;
//! * a Write binding is refused, because no checkout exists to write
//!   through and no Claim could be checked against one;
//! * the owned directory is identified by the estate's own address for
//!   this Work, so a substituted entry is neither executed in nor
//!   removed by cleanup;
//! * a later stage consumes the *bytes* an earlier stage's validated
//!   Claim was checked against, and an export of those bytes
//!   byte-matches the Claim;
//! * the Git Actor path is unchanged.
//!
//! Real wirkd throughout; no live Herdr session — these checks never
//! touch a pane, the same reasoning `boundary_claim.rs`'s own
//! `create_worktree_for_run` gives for its Git counterpart. A live
//! actor exercising this for real is a later stage's job, not this
//! file's.
//!
//! `wirk` has no `lib.rs` (bin-only), so `wirkd` is compiled into this
//! test binary via `#[path]` — the move other test files in this crate
//! already use.

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{FailPayload, RecordPayload, Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{
    ClaimKind, ClaimVerdict, EventKind, ExecutionTriple, Journal, RunId, SourceBasis, WaypointId,
    WorkId, World, WorldHash,
};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

fn embed_route(estate: &Path, file_name: &str, text: &str) -> PathBuf {
    let dir = estate.join("fixtures").join("routes");
    fs::create_dir_all(&dir).expect("create estate fixtures/routes/ dir");
    let path = dir.join(file_name);
    fs::write(&path, text).expect("write embedded fixture");
    path
}

fn output_only_actor_route(estate: &Path) -> PathBuf {
    embed_route(
        estate,
        "output_only_actor.json",
        include_str!("fixtures/routes/output_only_actor.json"),
    )
}

fn output_only_actor_boundary_route(estate: &Path) -> PathBuf {
    embed_route(
        estate,
        "output_only_actor_boundary.json",
        include_str!("fixtures/routes/output_only_actor_boundary.json"),
    )
}

fn output_only_actor_revise_route(estate: &Path) -> PathBuf {
    embed_route(
        estate,
        "output_only_actor_revise.json",
        include_str!("fixtures/routes/output_only_actor_revise.json"),
    )
}

fn deterministic_then_output_only_actor_route(estate: &Path) -> PathBuf {
    embed_route(
        estate,
        "deterministic_then_output_only_actor.json",
        include_str!("fixtures/routes/deterministic_then_output_only_actor.json"),
    )
}

fn deterministic_then_bounded_actor_route(estate: &Path) -> PathBuf {
    embed_route(
        estate,
        "deterministic_then_bounded_actor.json",
        include_str!("fixtures/routes/deterministic_then_bounded_actor.json"),
    )
}

fn output_only_actor_revise_oriented_route(estate: &Path) -> PathBuf {
    embed_route(
        estate,
        "output_only_actor_revise_oriented.json",
        include_str!("fixtures/routes/output_only_actor_revise_oriented.json"),
    )
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

/// Its own expensive-job host pool — the `resources.json` pattern
/// `job_authority.rs` and `estate_storage.rs` established and ruling
/// 0291 generalized to every fixture that reaches Atlas.
///
/// This file defines its own `start_wirkd` rather than going through
/// `support/nested_harness.rs`, so 0291's correction of that shared
/// entry point did not reach it and its own list did not name it. The
/// gap is the same one: `publish_document_source` calls `atlas
/// acquire`, which takes an admission slot in the unset default pool
/// (`$XDG_RUNTIME_DIR/wirk/expensive`, capacity 2, shared by every wirk
/// job for this uid on the box), so under real `cargo test` parallelism
/// this file's estates raced every other test estate and lost to
/// `HostExpensiveBusy`. Nested under the estate's own path, unique for
/// the life of its tempdir, and never the live default pool a real
/// concurrent `wirk` job might be using.
fn ensure_isolated_host_pool(estate: &Path) {
    let wirk_dir = estate.join(".wirk");
    fs::create_dir_all(&wirk_dir).expect("create estate .wirk dir");
    let pool = wirk_dir.join("host-pool");
    fs::write(
        wirk_dir.join("resources.json"),
        format!(
            "{{\"host_pool_dir\": {:?}}}\n",
            pool.to_str().expect("pool path is utf-8")
        ),
    )
    .expect("write isolated resources.json");
}

fn start_wirkd(estate: &Path) -> (KillOnDrop, WirkdPointer) {
    ensure_isolated_host_pool(estate);
    let child = KillOnDrop(
        wirk_cli()
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
    let stop = wirk_cli()
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

/// `wirk work submit --kind actor --source-basis output-only --base
/// <reference>` with the given bindings and no `--repo-path`: an
/// output-only Actor owns no Git checkout, and its bindings are Read
/// source grants rather than a checkout selection.
fn submit_output_only_actor_with(
    estate: &Path,
    route_path: &Path,
    reference: &str,
    bindings: &[&str],
) -> Result<(String, String, String), String> {
    let mut command = wirk_cli();
    command
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(route_path)
        .args(["--kind", "actor"])
        .args(["--source-basis", "output-only"])
        .args(["--base", reference]);
    for binding in bindings {
        command.args(["--repo", binding]);
    }
    let output = command.output().expect("work submit runs");
    if output.status.success() {
        Ok(parse_submit_stdout(&String::from_utf8_lossy(
            &output.stdout,
        )))
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn submit_output_only_actor(
    estate: &Path,
    route_path: &Path,
    reference: &str,
) -> Result<(String, String, String), String> {
    submit_output_only_actor_with(estate, route_path, reference, &[])
}

/// A tiny source collection, acquired and published through the estate's
/// own supported acquisition path so the alias a Read binding names is a
/// membership Atlas admission can actually match.
///
/// **What this publishes today, stated plainly: a Git-acquired source.**
/// The bound outcome needs a real *document* collection, and that is a
/// different admission story, not a different spelling of this one — a
/// document `atlas publish` is expensive admitted work that can be
/// refused while the estate is busy, where a Git publish is a catalog
/// edit that succeeds unconditionally. The earlier report called the
/// shared files "additive" and the difference "textual rather than
/// semantic"; that was wrong, and switching these helpers to a document
/// collection changes their behaviour, not only their merge text. The
/// acquisition and job-admission interface is ruling 0279's, and is not
/// edited or consumed here.
///
/// What is prepared for that switch is the shape below: every publish
/// goes through `publish_admitted`, which asserts the admission outcome
/// and retries a busy refusal instead of assuming success. When the
/// document path lands, only the acquisition call changes.
///
/// What these checks hold to meanwhile is the *binding* channel — an
/// alias and an access level, carrying no repository path and no Git
/// field of its own — so a source acquired by another means admits
/// through exactly the same door.
fn publish_document_source(estate: &Path, alias: &str, docs: &[(&str, &str)]) -> PathBuf {
    let repo = estate.join("sources").join(alias);
    fs::create_dir_all(&repo).expect("create source dir");
    for (name, body) in docs {
        fs::write(repo.join(name), body).expect("write document");
    }
    for args in [vec!["init", "--initial-branch=main"], vec!["add", "."]] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(&args)
            .output()
            .expect("git runs");
        assert!(ok.status.success(), "git {args:?}: {ok:?}");
    }
    let commit = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["-c", "user.email=t@example.invalid", "-c", "user.name=t"])
        .args(["commit", "-m", "documents"])
        .output()
        .expect("git commit runs");
    assert!(commit.status.success(), "git commit: {commit:?}");

    let acquired = atlas_json(
        estate,
        &[
            "acquire",
            "--source",
            alias,
            "--repository",
            repo.to_str().expect("utf-8 source path"),
            "--revision",
            "HEAD",
        ],
    );
    let generation = acquired["generation"]["generation"]
        .as_str()
        .expect("acquired generation id")
        .to_string();
    publish_admitted(estate, alias, &generation);
    repo
}

/// `atlas publish`, with its admission outcome asserted rather than
/// assumed.
///
/// A Git publish is a catalog edit and simply succeeds. A document
/// publish (ruling 0279) is admitted work: when the estate is already
/// running something expensive it is *refused*, and a control that
/// asserts unconditional success would fail for a reason that has
/// nothing to do with what it is checking. Waiting and re-asking is the
/// supported answer to a busy refusal — it is a capacity statement, not
/// a verdict on the request — and a refusal that is not about capacity
/// still fails loudly here.
fn publish_admitted(estate: &Path, alias: &str, generation: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (ok, value, err) = atlas(
            estate,
            &["publish", "--source", alias, "--generation", generation],
        );
        if ok {
            return value;
        }
        let busy = ["Busy", "AtCapacity", "Admission", "admitted", "capacity"]
            .iter()
            .any(|marker| err.contains(marker));
        assert!(
            busy,
            "atlas publish refused {alias} for a reason that is not capacity: {err}"
        );
        assert!(
            Instant::now() < deadline,
            "atlas publish of {alias} stayed refused for capacity for 30s: {err}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Changes a published source and publishes the change: a new
/// generation of the same alias, which is how a later stage observes
/// that its evidence moved.
fn republish_document_source(estate: &Path, alias: &str, docs: &[(&str, &str)]) {
    let repo = estate.join("sources").join(alias);
    for (name, body) in docs {
        fs::write(repo.join(name), body).expect("rewrite document");
    }
    let args = ["add", "."];
    let ok = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(ok.status.success(), "git {args:?}: {ok:?}");
    let commit = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["-c", "user.email=t@example.invalid", "-c", "user.name=t"])
        .args(["commit", "-m", "revised documents"])
        .output()
        .expect("git commit runs");
    assert!(commit.status.success(), "git commit: {commit:?}");

    let refreshed = atlas_json(
        estate,
        &["refresh", "--source", alias, "--revision", "HEAD"],
    );
    let generation = refreshed["generation"]["generation"]
        .as_str()
        .expect("refreshed generation id")
        .to_string();
    publish_admitted(estate, alias, &generation);
}

/// The CLI, spawned with this *suite's own* actor triple removed from
/// the child's environment.
///
/// A test process inherits whatever `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/
/// `WIRK_RUN_ID` its runner had, and `resolve_scope` reads exactly those
/// to decide whether a call is an actor's own or an operator's. Left
/// inherited, a fixture's administrative call against its temp estate
/// means one thing under a bare shell and another inside a bound Run —
/// which is not a test. Removing them pins the fixture to the
/// operator-at-the-estate-root reading it intends (R2: `claim.rs`,
/// `job_authority.rs` and `estate_storage.rs` already spawn this way).
///
/// Sites that mean to act *as* an actor set the three back explicitly on
/// the returned command, and a later `env` overrides this removal.
fn wirk_cli() -> Command {
    let mut command = Command::new(wirk_bin());
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}

fn atlas(estate: &Path, args: &[&str]) -> (bool, serde_json::Value, String) {
    let mut full = vec!["atlas"];
    full.extend_from_slice(args);
    full.push("--estate");
    let estate_str = estate.to_str().expect("utf-8 estate path");
    full.push(estate_str);
    full.push("--json");
    let output = wirk_cli().args(&full).output().expect("wirk atlas runs");
    (
        output.status.success(),
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
            .unwrap_or(serde_json::Value::Null),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

fn atlas_json(estate: &Path, args: &[&str]) -> serde_json::Value {
    let (ok, value, err) = atlas(estate, args);
    assert!(ok, "wirk atlas {args:?} failed: {err}");
    value
}

/// `wirk artifact read|export`, run with this Run's own injected triple.
fn artifact_verb(
    estate: &Path,
    work_id: &str,
    run_id: &str,
    args: &[&str],
) -> (Option<i32>, Vec<u8>, String) {
    let output = wirk_cli()
        .arg("artifact")
        .args(args)
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk artifact runs");
    (
        output.status.code(),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

/// The Claim id this Work's own journal records for `run_id`'s
/// validated `Done` Claim — the id a later stage addresses that
/// stage's artifact by. Read from the journal rather than from a
/// rendering, so the check is pinned to what was actually recorded.
fn validated_claim_id(estate: &Path, work_id: &str, run_id: &str) -> String {
    let events = Journal::open(estate.join("works").join(work_id))
        .expect("open journal")
        .replay()
        .expect("replay journal");
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ClaimRecorded {
                claim,
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
                ..
            } if event.run.as_ref().map(|run| run.0.as_str()) == Some(run_id) => {
                Some(claim.0.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no validated Done Claim recorded for run {run_id}"))
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

fn status_result(socket: &Path, work_id: &str) -> serde_json::Value {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload::admin(WorkId(work_id.to_string()))),
    )
    .expect("status call succeeds");
    match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => panic!(
            "status unexpectedly refused: {} {}",
            error.code, error.message
        ),
    }
}

fn reserved_world(socket: &Path, work_id: &str) -> World {
    let result = status_result(socket, work_id);
    serde_json::from_value(result["world"].clone()).expect("world deserializes")
}

/// Steps 2-3 of `wirk run` for an output-only World, replicated without
/// starting a Herdr session: `create_dir_all` in place of `git worktree
/// add`, `WorktreeCreated` journaled with the World's own (empty)
/// `repository` and its basis reference as `base_sha`, then the World's
/// `worktree_path` filled in through a re-emitted `WaypointReserved` —
/// what `resolve_run_binding`'s materialization-detection loop expects.
fn materialize_output_only_run(
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
    assert!(
        matches!(actor.source_basis, SourceBasis::OutputOnly { .. }),
        "expected an output-only source basis, got {:?}",
        actor.source_basis
    );
    let worktree_path = estate.join("worktrees").join(work_id);
    fs::create_dir_all(&worktree_path).expect("create_dir_all succeeds");
    // The creation-time marker `executor.rs`'s own Step 2 writes. It is
    // what distinguishes a directory this estate made from one that was
    // moved into the address afterwards, so a helper that stood in for
    // the executor without writing it would be standing in for a
    // materialization that never happened.
    wirk_core::write_owned_marker(
        &worktree_path,
        &WorkId(work_id.to_string()),
        &RunId(run_id.to_string()),
    )
    .expect("record this Run's ownership of the owned directory");
    wirkd_record(
        socket,
        work_id,
        Some(run_id),
        EventKind::WorktreeCreated {
            repo: actor.repository.clone(),
            base_sha: actor.base_sha.clone(),
            // Ruling 0283: the executor records the identity of the
            // directory it actually created, read from the directory
            // itself. A helper standing in for it has to record the
            // same fact, or it stands in for a materialization that
            // registered nothing.
            identity: wirk_core::directory_identity(&worktree_path),
        },
    );
    let mut updated_actor = actor;
    updated_actor.worktree_path = worktree_path.clone();
    let updated_world = World::Actor(updated_actor);
    let world_hash = WorldHash::of(&updated_world);
    wirkd_record(
        socket,
        work_id,
        Some(run_id),
        EventKind::WaypointReserved {
            waypoint: WaypointId(waypoint.to_string()),
            world_hash,
            world: updated_world,
        },
    );
    worktree_path
}

fn output_dir(estate: &Path, work_id: &str, run_id: &str) -> PathBuf {
    let out = wirk_cli()
        .args(["output", "dir"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk output dir runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "wirk output dir: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn claim(estate: &Path, work_id: &str, run_id: &str) -> (Option<i32>, String) {
    let output = wirk_cli()
        .args(["claim"])
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

fn run_clean(estate: &Path, work_id: &str) -> (bool, serde_json::Value) {
    let output = wirk_cli()
        .args(["work", "clean", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--json"])
        .output()
        .expect("wirk work clean runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // A refusal writes its reason to stderr and leaves stdout empty, so
    // reporting only the parsed stdout renders every failure as a bare
    // `null` and hides what the daemon actually said.
    let parsed = serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap_or_else(|_| {
        serde_json::json!({
            "stdout": stdout,
            "stderr": String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    });
    (output.status.success(), parsed)
}

fn fail_run(socket: &Path, estate: &Path, work_id: &str, run_id: &str) {
    let reply = wirkd::client::call(
        socket,
        &Request::fail(FailPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.to_string()),
                run_id: RunId(run_id.to_string()),
            },
            status: Some("interrupted".to_string()),
            detail: Some("injected: the stage did not finish".to_string()),
        }),
    )
    .expect("fail call succeeds");
    assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
}

fn retry_cli(estate: &Path, work_id: &str) -> (Option<i32>, String) {
    let output = wirk_cli()
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

/// The Run the Work is currently on — after an auto-advance, the second
/// stage's own Run rather than the one that just claimed.
fn current_run_id(socket: &Path, work_id: &str) -> String {
    let result = status_result(socket, work_id);
    // `run_id` is what `work status` actually names its current Run —
    // the same field `actor_advance.rs` reads after an auto-advance
    // (R2). `run`/`current_run` are not keys this reply has ever had.
    result["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no current Run reported for {work_id}: {result:#}"))
        .to_string()
}

// ---- submission: source grants, not a checkout selection ----------------

#[test]
fn output_only_actor_is_admitted_with_no_repo_and_empty_boundary() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let result = submit_output_only_actor(estate, &route, "doc-set-1");
    assert!(
        result.is_ok(),
        "expected admission, got refusal: {:?}",
        result.err()
    );

    stop_wirkd(estate, wirkd_child);
}

/// The correction this file exists for. Two admitted document sources
/// are two pieces of evidence, not two candidate checkouts, so neither
/// has to be nominated as "the execution checkout" for the submission
/// to be admitted.
///
/// Fails if execution-checkout resolution is asked before the declared
/// basis is consulted: the second `--repo` binding then draws
/// `AmbiguousExecutionRepository` and the Work is never reserved.
#[test]
fn output_only_actor_is_admitted_with_two_read_sources_and_names_no_execution_repo() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n\nalpha evidence.\n")]);
    publish_document_source(estate, "docs-b", &[("b.md", "# B\n\nbeta evidence.\n")]);

    let route = output_only_actor_route(estate);
    let submitted =
        submit_output_only_actor_with(estate, &route, "doc-set-1", &["docs-a:read", "docs-b:read"]);
    let (work_id, _run_id, _waypoint) = match submitted {
        Ok(ids) => ids,
        Err(stderr) => panic!("two Read sources were refused admission: {stderr}"),
    };

    // Reserved truthfully: no execution repository, no branch, and the
    // basis reference standing only for the execution/inspection basis.
    let World::Actor(actor) = reserved_world(&pointer.socket, &work_id) else {
        panic!("expected an Actor World");
    };
    assert_eq!(
        actor.repository, "",
        "an output-only Actor names no execution repository; a document source alias in this \
         field would report a checkout this Work never had"
    );
    assert_eq!(actor.branch, "");
    assert!(matches!(
        actor.source_basis,
        SourceBasis::OutputOnly { ref reference } if reference == "doc-set-1"
    ));

    stop_wirkd(estate, wirkd_child);
}

/// Admitting Read bindings must widen nothing: the grant list is still
/// the only thing Atlas admission matches a membership against.
#[test]
fn output_only_actor_is_denied_a_source_it_is_not_bound_to() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n\nalpha evidence.\n")]);
    publish_document_source(estate, "docs-b", &[("b.md", "# B\n\nbeta evidence.\n")]);

    let route = output_only_actor_route(estate);
    let (work_id, _run_id, _waypoint) =
        submit_output_only_actor_with(estate, &route, "doc-set-1", &["docs-a:read"])
            .expect("one Read source is admitted");

    // The bound source answers.
    let bound = atlas_json(
        estate,
        &[
            "search", "--work", &work_id, "--query", "alpha", "--source", "docs-a",
        ],
    );
    assert_eq!(
        bound["admission"]["admitted"].as_u64(),
        Some(1),
        "the bound source should be admitted: {bound:#}"
    );

    // The unbound one is denied, and returns nothing.
    let unbound = atlas_json(
        estate,
        &[
            "search", "--work", &work_id, "--query", "beta", "--source", "docs-b",
        ],
    );
    assert_eq!(
        unbound["admission"]["admitted"].as_u64(),
        Some(0),
        "a source this Work declared no binding for must never be admitted: {unbound:#}"
    );
    assert_eq!(
        unbound["hits"].as_array().map(Vec::len),
        Some(0),
        "a denied source must contribute no hits: {unbound:#}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// A Write binding declares a mutation surface this World does not
/// have. It is refused at submit rather than accepted and left
/// uncheckable: `validate_claim` skips the worktree diff for this
/// basis, so no Claim of this Work could ever be checked against it.
#[test]
fn output_only_actor_refuses_a_write_binding() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n")]);
    let route = output_only_actor_route(estate);
    let refusal = submit_output_only_actor_with(estate, &route, "doc-set-1", &["docs-a:write"])
        .expect_err("a Write binding must be refused for an output-only Actor");
    assert!(
        refusal.contains("IncompatibleSourceBasis"),
        "expected IncompatibleSourceBasis naming the missing checkout, got: {refusal}"
    );

    stop_wirkd(estate, wirkd_child);
}

#[test]
fn output_only_actor_refuses_repo_path() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);
    let route = output_only_actor_route(estate);

    let output = wirk_cli()
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(&route)
        .args(["--kind", "actor"])
        .args(["--source-basis", "output-only"])
        .args(["--base", "doc-set-1", "--repo-path"])
        .arg(estate) // any existing path; never inspected before the refusal
        .output()
        .expect("work submit runs");
    assert!(
        !output.status.success(),
        "expected refusal combining --source-basis output-only with --repo-path"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("BadRequest"),
        "expected a BadRequest naming the conflict, got: {stderr}"
    );

    stop_wirkd(estate, wirkd_child);
}

#[test]
fn output_only_actor_refuses_an_execution_repo_selection() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n")]);
    let route = output_only_actor_route(estate);
    let output = wirk_cli()
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(&route)
        .args(["--kind", "actor"])
        .args(["--source-basis", "output-only"])
        .args(["--base", "doc-set-1"])
        .args(["--repo", "docs-a:read"])
        .args(["--execution-repo", "docs-a"])
        .output()
        .expect("work submit runs");
    assert!(
        !output.status.success(),
        "an output-only Actor has no execution checkout for --execution-repo to name"
    );

    stop_wirkd(estate, wirkd_child);
}

#[test]
fn output_only_actor_refuses_a_declared_checkout_boundary() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    let route = output_only_actor_boundary_route(estate);
    let refusal = submit_output_only_actor(estate, &route, "doc-set-1")
        .expect_err("a declared checkout boundary must be refused");
    assert!(
        refusal.contains("IncompatibleSourceBasis"),
        "expected IncompatibleSourceBasis naming the absent worktree, got: {refusal}"
    );

    stop_wirkd(estate, wirkd_child);
}

// ---- owned execution directory ------------------------------------------

#[test]
fn output_only_actor_materializes_an_owned_directory_and_claims_a_managed_output() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n\nalpha evidence.\n")]);
    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor_with(estate, &route, "doc-set-1", &["docs-a:read"])
            .expect("admitted");

    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    assert!(worktree.is_dir(), "the owned directory exists");
    assert!(
        !worktree.join(".git").exists(),
        "no Git metadata is fabricated for an owned execution directory"
    );

    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), "# Draft\n\ncites docs-a/a.md\n")
        .expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");

    stop_wirkd(estate, wirkd_child);
}

/// Drives a Work to a terminal state so `wirk work clean` reaches its
/// identity checks at all.
///
/// This is the step the earlier substitution control was missing.
/// `clean_work` refuses `NotTerminal` before it inspects anything, so a
/// check that substituted an entry and then called clean was asserting
/// against a refusal that had nothing to do with ownership.
fn drive_to_terminal(estate: &Path, work_id: &str, run_id: &str) {
    let staging = output_dir(estate, work_id, run_id);
    fs::write(staging.join("draft.md"), "# Draft\n").expect("write the declared output");
    let (code, stdout) = claim(estate, work_id, run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
}

/// Address equality plus `is_dir` is not creation identity.
///
/// The case that matters is **not** a symlink — the pre-existing guard
/// at the top of `verify_worktree_identity` refuses a symlink outright,
/// which is why the previous version of this file's account of cleanup
/// "following it out of the estate" was wrong: that path refused before
/// `remove_dir_all` was ever reached. The residual case is an ordinary,
/// real directory that this estate did not create, standing at the
/// address. It answers canonical address equality and `is_dir` exactly
/// as the genuine materialization does, and `remove_dir_all` would take
/// it and everything under it.
///
/// What tells them apart is the marker the materialization itself wrote.
#[test]
fn an_unrelated_real_directory_at_the_owned_address_is_not_this_works_materialization() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    drive_to_terminal(estate, &work_id, &run_id);

    // A real directory, not a symlink, holding something that must
    // survive — swapped into the owned address after the genuine one is
    // gone. Nothing about its *address* distinguishes it.
    fs::remove_dir_all(&worktree).expect("remove the genuine materialization");
    fs::create_dir_all(&worktree).expect("create the unrelated directory in its place");
    fs::write(worktree.join("original.md"), "untouched\n").expect("write the original");

    let (ok, result) = run_clean(estate, &work_id);
    assert!(ok, "clean should report, not fail: {result:#}");
    assert_ne!(
        result["worktree_removed"].as_bool(),
        Some(true),
        "a directory this estate never created must never be reported as this Work's own \
         directory removed: {result:#}"
    );
    assert!(
        worktree.join("original.md").exists(),
        "cleanup removed a directory this estate did not create"
    );
    // Ruling 0283: and the report says so. `worktree_removed: false`
    // alone reads exactly like "there was nothing there", which is a
    // different fact about a different estate.
    assert_eq!(
        result["worktree_state"].as_str(),
        Some("unproven"),
        "a directory found and deliberately left alone must be reported as such: {result:#}"
    );
    assert!(
        result["worktree_detail"]
            .as_str()
            .is_some_and(|detail| !detail.is_empty()),
        "the unproven report must say what was found: {result:#}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// The symlink case, asserted for the shape it actually has: an explicit
/// `PathMismatch` refusal, not a report.
#[test]
fn a_symlink_at_the_owned_address_is_refused_rather_than_followed() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    drive_to_terminal(estate, &work_id, &run_id);

    let elsewhere = estate.join("not-ours");
    fs::create_dir_all(&elsewhere).expect("create the unrelated directory");
    fs::write(elsewhere.join("original.md"), "untouched\n").expect("write the original");
    fs::remove_dir_all(&worktree).expect("remove the genuine materialization");
    std::os::unix::fs::symlink(&elsewhere, &worktree).expect("substitute a symlink");

    let (ok, result) = run_clean(estate, &work_id);
    assert!(
        !ok,
        "a symlink at the owned address is refused, not reported: {result:#}"
    );
    assert!(
        elsewhere.join("original.md").exists(),
        "cleanup followed a substituted entry and reached content this Work never owned"
    );

    stop_wirkd(estate, wirkd_child);
}

/// An intermediate redirect. Canonicalizing *both* sides of the address
/// comparison follows the same substituted `worktrees` directory, so the
/// two agree and address equality proves nothing; the container is
/// checked directly for exactly this case.
#[test]
fn a_substituted_worktrees_container_is_not_this_estates_own_address() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    drive_to_terminal(estate, &work_id, &run_id);

    // The whole container moved aside and replaced by a link to an
    // identical-looking one somewhere this estate does not own.
    let containers = estate.join("worktrees");
    let elsewhere = estate.join("not-ours-worktrees");
    fs::rename(&containers, &elsewhere).expect("move the container aside");
    fs::write(elsewhere.join(&work_id).join("original.md"), "untouched\n")
        .expect("write the original");
    std::os::unix::fs::symlink(&elsewhere, &containers).expect("substitute the container");

    let (ok, result) = run_clean(estate, &work_id);
    assert!(ok, "clean should report, not fail: {result:#}");
    assert_ne!(
        result["worktree_removed"].as_bool(),
        Some(true),
        "a redirected container is not this estate's own address: {result:#}"
    );
    assert!(
        elsewhere.join(&work_id).join("original.md").exists(),
        "cleanup walked through a substituted container"
    );

    stop_wirkd(estate, wirkd_child);
}

/// Nothing at the address at all. Honest report: nothing was removed,
/// and no failure — there is nothing left to fail on.
#[test]
fn an_absent_owned_directory_reports_that_nothing_was_removed() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    drive_to_terminal(estate, &work_id, &run_id);
    fs::remove_dir_all(&worktree).expect("remove the owned directory out from under the estate");

    let (ok, result) = run_clean(estate, &work_id);
    assert!(ok, "clean should report, not fail: {result:#}");
    assert_ne!(
        result["worktree_removed"].as_bool(),
        Some(true),
        "nothing was there to remove: {result:#}"
    );
    assert_eq!(
        result["worktree_state"].as_str(),
        Some("absent"),
        "absent is a different finding from a directory left alone unproven: {result:#}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// The executor's own materialization arm, driven through the real `wirk
/// run` path rather than replicated through `record`.
///
/// `materialize_output_only_run` above stands in for `wirk run` steps
/// 2-3 so the rest of this file needs no live Herdr session — which
/// means it exercises none of `executor.rs`'s own guards. This one does:
/// the refusal happens during materialization, before any session is
/// launched, so it is reachable in-suite. The *positive* executor path
/// (a real materialization, a real session, a real reattachment) is not
/// reachable here and belongs to the native two-Actor use.
#[test]
fn wirk_run_refuses_a_substituted_entry_at_the_owned_address() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, _run_id, _waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");

    // A regular file where the owned execution directory would be
    // created. Never this estate's materialization, whatever the address
    // says.
    let worktree = estate.join("worktrees").join(&work_id);
    fs::create_dir_all(worktree.parent().expect("worktrees parent")).expect("create container");
    fs::write(&worktree, "not a directory\n").expect("substitute a regular file");

    let output = wirk_cli()
        .args(["run", "--estate"])
        .arg(estate)
        .args(["--work", &work_id, "--session", "substituted"])
        .output()
        .expect("wirk run runs");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "wirk run must refuse to execute in an entry this estate did not create: {stderr}"
    );
    assert!(
        stderr.contains("not a directory this Run owns")
            || stderr.contains("carries no record of this estate having created it"),
        "expected an ownership refusal naming the address, got: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&worktree).expect("the substituted entry survives"),
        "not a directory\n",
        "the substituted entry must be left exactly as it was"
    );

    stop_wirkd(estate, wirkd_child);
}

#[test]
fn wirk_work_clean_removes_the_owned_directory_without_a_git_call() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    // Working state of exactly the kind a Git `ignored`/`dirty` guard
    // would have refused to remove; here it is this Run's own and
    // nothing else's.
    fs::write(worktree.join("scratch.tmp"), "in progress\n").expect("write scratch");

    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), "# Draft\n").expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");

    let (ok, result) = run_clean(estate, &work_id);
    assert!(ok, "clean failed: {result:#}");
    assert_eq!(
        result["worktree_removed"].as_bool(),
        Some(true),
        "the owned directory should be released: {result:#}"
    );
    assert_eq!(
        result["worktree_state"].as_str(),
        Some("registered"),
        "the directory this estate created is reported as registered: {result:#}"
    );
    assert!(!worktree.exists(), "the owned directory is gone from disk");

    stop_wirkd(estate, wirkd_child);
}

// ---- artifact handoff and export ----------------------------------------

/// The revision path. A later stage must be able to read the *bytes*
/// the earlier stage's Claim was validated against — not a bounded
/// summary of them, and not whatever happens to be sitting in a shared
/// scratch directory under the same name.
#[test]
fn a_later_stage_reads_the_prior_validated_claims_own_bytes() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n\nalpha evidence.\n")]);
    let route = output_only_actor_revise_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor_with(estate, &route, "doc-set-1", &["docs-a:read"])
            .expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);

    // Longer than the 320-byte summary the orientation projection binds,
    // so a check that passed on the summary alone would fail here.
    let draft = format!("# Draft\n\ncites docs-a/a.md\n\n{}\n", "body. ".repeat(200));
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), &draft).expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "stage 1 claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    // The auto-advanced second stage, reading stage 1's claimed bytes.
    let next_run = current_run_id(&pointer.socket, &work_id);
    assert_ne!(next_run, run_id, "the Work advanced to a second stage");
    let (code, stdout, stderr) = artifact_verb(
        estate,
        &work_id,
        &next_run,
        &["read", "--claim", &claim_id, "--name", "draft.md"],
    );
    assert_eq!(code, Some(0), "wirk artifact read failed: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        draft,
        "the later stage must receive the bytes the prior Claim was validated against"
    );

    stop_wirkd(estate, wirkd_child);
}

#[test]
fn artifact_export_byte_matches_the_claim_and_refuses_an_occupied_destination() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);

    let draft = "# Draft\n\ncites docs-a/a.md\n";
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), draft).expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    let destination = estate_dir.path().join("exported.md");
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            destination.to_str().expect("utf-8 destination"),
        ],
    );
    assert_eq!(code, Some(0), "export failed: {stderr}");
    assert_eq!(
        fs::read(&destination).expect("read the export"),
        draft.as_bytes(),
        "an export must byte-match the Claim it names"
    );

    // The destination is the caller's, and this verb never silently
    // replaces what is already there.
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            destination.to_str().expect("utf-8 destination"),
        ],
    );
    assert_ne!(code, Some(0), "an occupied destination must be refused");
    assert!(
        stderr.contains("already exists"),
        "expected the refusal to name the occupied destination, got: {stderr}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// A Claim vouches for bytes; it does not vouch for a name. An output
/// the named Claim was never validated against resolves to nothing,
/// however plausibly it is spelled.
#[test]
fn artifact_read_refuses_a_name_the_claim_was_not_validated_against() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), "# Draft\n").expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    // Present in the same staging area, and never claimed.
    fs::write(staging.join("revision.md"), "# Not claimed\n").expect("write an unclaimed file");
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &["read", "--claim", &claim_id, "--name", "revision.md"],
    );
    assert_ne!(
        code,
        Some(0),
        "sharing a staging directory is not the same as being claimed"
    );
    assert!(
        stderr.contains("NotFound"),
        "expected NotFound for an unclaimed name, got: {stderr}"
    );

    stop_wirkd(estate, wirkd_child);
}

// ---- the Git Actor path is unchanged ------------------------------------

/// The Git arm keeps its own contract: `--repo-path` is still required
/// when no output-only basis is declared, so nothing here turned the
/// Git Actor into an optionally-checkout-less one.
#[test]
fn a_git_actor_still_requires_a_repo_path() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let output = wirk_cli()
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(&route)
        .args(["--kind", "actor"])
        .args(["--base", "HEAD"])
        .output()
        .expect("work submit runs");
    assert!(
        !output.status.success(),
        "a Git Actor submission without --repo-path must still refuse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--repo-path is required"),
        "expected the existing Git-arm refusal, got: {stderr}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// Interruption and retry preserve the established identities: a retry
/// reuses the same owned directory and the same basis reference rather
/// than materializing a second one or minting a fresh identity.
#[test]
fn retry_after_an_interrupted_stage_keeps_the_owned_directory_and_basis() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    // Partial work from the interrupted attempt.
    fs::write(worktree.join("partial.md"), "half written\n").expect("write partial work");

    fail_run(&pointer.socket, estate, &work_id, &run_id);
    let (code, stdout) = retry_cli(estate, &work_id);
    assert_eq!(code, Some(0), "retry refused: {stdout}");

    let World::Actor(actor) = reserved_world(&pointer.socket, &work_id) else {
        panic!("expected an Actor World");
    };
    assert_eq!(
        actor.worktree_path, worktree,
        "a retry reuses this Work's own owned directory, never a second one"
    );
    assert!(matches!(
        actor.source_basis,
        SourceBasis::OutputOnly { ref reference } if reference == "doc-set-1"
    ));
    assert_eq!(actor.base_sha, "doc-set-1");
    assert!(
        worktree.join("partial.md").exists(),
        "a retry preserves the evidence the interrupted attempt left behind"
    );

    stop_wirkd(estate, wirkd_child);
}

/// The second half of the revision story: a later stage must be able to
/// see that its evidence moved.
///
/// For this basis the execution reference is deliberately *not* where
/// source currentness lives — it names the execution basis and nothing
/// else, and carrying it forward unchanged across stages is correct. A
/// source change is an explicit publication of a new generation, and
/// what the later stage queries is the alias it is bound to, now
/// resolving to that new generation. The earlier stage's claimed bytes
/// do not move when it happens.
#[test]
fn a_published_source_change_is_visible_to_the_stage_that_follows_it() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n\nalpha evidence.\n")]);
    let route = output_only_actor_revise_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor_with(estate, &route, "doc-set-1", &["docs-a:read"])
            .expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);

    let draft = "# Draft\n\ncites docs-a/a.md: alpha evidence.\n";
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), draft).expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "stage 1 claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    // The source moves, explicitly and by publication.
    republish_document_source(estate, "docs-a", &[("a.md", "# A\n\ngamma evidence.\n")]);

    let next_run = current_run_id(&pointer.socket, &work_id);
    let found = atlas_json(
        estate,
        &[
            "search", "--work", &work_id, "--query", "gamma", "--source", "docs-a",
        ],
    );
    assert!(
        found["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()),
        "the stage that follows the change must see the changed source: {found:#}"
    );

    // And the prior Claim's bytes are exactly what they were.
    let (code, bytes, stderr) = artifact_verb(
        estate,
        &work_id,
        &next_run,
        &["read", "--claim", &claim_id, "--name", "draft.md"],
    );
    assert_eq!(code, Some(0), "wirk artifact read failed: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        draft,
        "a source change must not disturb the bytes an earlier Claim was validated against"
    );

    stop_wirkd(estate, wirkd_child);
}

// ---- F1: the execution area a mixed Route hands to its Actor stage ------

/// `wirk work submit` for a Route whose *first* Waypoint is
/// Deterministic on an output-only basis — the shape that reaches the
/// Actor stage through auto-advance rather than through `handle_submit`.
fn submit_mixed_output_only(
    estate: &Path,
    route_path: &Path,
    reference: &str,
    bindings: &[&str],
) -> Result<(String, String, String), String> {
    let mut command = wirk_cli();
    command
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route"])
        .arg(route_path)
        // Deliberately *no* `--kind deterministic`: that names the ad
        // hoc, Route-less single-Waypoint shape, which synthesizes its
        // own Waypoint, requires `--command` and never loads the file
        // named here. A mixed Route is submitted by naming `--route`
        // and letting its own first Waypoint say it is Deterministic —
        // which is the only path that reaches the basis decision this
        // fixture exists to exercise.
        .args(["--source-basis", "output-only"])
        .args(["--base", reference]);
    for binding in bindings {
        command.args(["--repo", binding]);
    }
    let output = command.output().expect("work submit runs");
    if output.status.success() {
        Ok(parse_submit_stdout(&String::from_utf8_lossy(
            &output.stdout,
        )))
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn run_deterministic(estate: &Path, work_id: &str) -> (bool, String) {
    let output = wirk_cli()
        .args(["run-deterministic", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--executor", "child"])
        .output()
        .expect("run-deterministic runs");
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// Neither stage of a mixed output-only Route executes in the estate
/// root.
///
/// The first correction (ruling 0283) stopped the Actor stage
/// *inheriting* the Deterministic stage's estate-root `cwd`, which would
/// have made the whole estate this Work's execution area — hashed into
/// the World as its repository, reported publicly as a checkout the Work
/// never had, and used as the root Claim validation bounds the stage's
/// artifacts against. It left the Deterministic stage itself there, and
/// reserved the Actor stage unmaterialized so `wirk run` would create
/// the owned directory.
///
/// Ruling 0292 corrects the other half: the Deterministic stage owns the
/// same directory, created and registered by `run-deterministic`'s own
/// materialization. So the Actor stage now *legitimately* inherits it —
/// one execution area per Work, as one Git worktree is reused across a
/// Work's Waypoints — and what is asserted is that the address is this
/// estate's own for this Work, never the estate root.
#[test]
fn a_mixed_route_does_not_hand_the_estate_root_to_the_actor_stage() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = deterministic_then_output_only_actor_route(estate);
    let (work_id, _run_id, waypoint) =
        submit_mixed_output_only(estate, &route, "doc-set-1", &[]).expect("admitted");
    assert_eq!(waypoint, "deterministic-then-output-only-actor/wp-1");

    let (ok, log) = run_deterministic(estate, &work_id);
    assert!(ok, "run-deterministic (wp-1) failed: {log}");

    let result = status_result(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("deterministic-then-output-only-actor/wp-2"),
        "the mixed Route must advance, not be refused: {result:#}"
    );
    let world: World =
        serde_json::from_value(result["world"].clone()).expect("a World is reserved for wp-2");
    let World::Actor(actor) = world else {
        panic!("wp-2's World must be an Actor World");
    };
    assert_ne!(
        actor.worktree_path,
        estate.to_path_buf(),
        "the estate root must never become an Actor's execution directory"
    );
    assert_eq!(
        actor.worktree_path,
        estate.join("worktrees").join(&work_id),
        "the Actor stage inherits this Work's own owned directory, which the Deterministic stage \
         created and registered (ruling 0292): {:?}",
        actor.worktree_path
    );
    assert_eq!(
        actor.repository,
        String::new(),
        "there is no execution repository, and naming one reports a checkout this Work never had"
    );
    assert!(
        matches!(actor.source_basis, SourceBasis::OutputOnly { .. }),
        "the basis carries forward: {:?}",
        actor.source_basis
    );

    stop_wirkd(estate, wirkd_child);
}

/// The document half of a mixed Route: an Actor stage that reads
/// admitted sources after a Deterministic stage.
///
/// This is the shape the preserved mixed Route exists for, and it was
/// not expressible. The Deterministic output-only arm refused *Read*
/// bindings at submit and the Actor transition refuses *Write* ones, so
/// `[Deterministic, Actor]` was admissible only with no bindings at all.
/// A Read binding is a source grant, not a demand for Git inspection,
/// and a stage that does not consume one is not thereby incompatible
/// with it.
///
/// Two of them, deliberately: with the execution-repository resolution
/// still running for this basis, two Read aliases answered "which of
/// these is the checkout?" with an ambiguity refusal — for a submission
/// that has no checkout at all.
#[test]
fn a_mixed_route_admits_read_source_grants_and_reaches_the_actor_stage() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n")]);
    publish_document_source(estate, "docs-b", &[("b.md", "# B\n")]);
    let route = deterministic_then_output_only_actor_route(estate);
    let (work_id, _run_id, waypoint) =
        submit_mixed_output_only(estate, &route, "doc-set-1", &["docs-a:read", "docs-b:read"])
            .expect("a mixed Route carrying Read source grants is admitted");
    assert_eq!(waypoint, "deterministic-then-output-only-actor/wp-1");

    let (ok, log) = run_deterministic(estate, &work_id);
    assert!(ok, "run-deterministic (wp-1) failed: {log}");

    let result = status_result(&pointer.socket, &work_id);
    assert_eq!(
        result["current_waypoint"].as_str(),
        Some("deterministic-then-output-only-actor/wp-2"),
        "the Actor stage must be reached with the grants still folded on the Work: {result:#}"
    );
    // What `status` renders is the Work's state and its current
    // Waypoint, not its binding list — so what is asserted here is what
    // is actually observable: the submission with two Read grants was
    // admitted, and the Actor stage was reached. Whether the delivered
    // World then resolves those grants to admitted sources is the
    // acquisition path's own question, exercised by
    // `a_published_source_change_is_visible_to_the_stage_that_follows_it`.
    let world: World =
        serde_json::from_value(result["world"].clone()).expect("a World is reserved for wp-2");
    let World::Actor(actor) = world else {
        panic!("wp-2's World must be an Actor World");
    };
    assert_eq!(
        actor.worktree_path,
        estate.join("worktrees").join(&work_id),
        "the Actor stage is reserved on this Work's own owned directory, not handed the estate \
         root"
    );

    stop_wirkd(estate, wirkd_child);
}

/// The Write refusal held only at the *transition*, which is after the
/// first stage has run and claimed: the reservation errored, the Claim
/// stood, and the Work was left at wp-1 with no open Run — nothing to
/// claim and nothing to retry. The whole authored Route is walked at
/// submit, where refusing it is free, and this combination never starts.
#[test]
fn a_write_binding_on_a_mixed_output_only_route_is_refused_at_submit() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, _pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n")]);
    let route = deterministic_then_output_only_actor_route(estate);
    let refusal = submit_mixed_output_only(estate, &route, "doc-set-1", &["docs-a:write"])
        .expect_err("a Route reaching an Actor stage on an output-only basis refuses Write");
    assert!(
        refusal.contains("IncompatibleSourceBasis"),
        "expected IncompatibleSourceBasis at submit, got: {refusal}"
    );
    assert!(
        refusal.contains("rather than partway through the Route"),
        "the refusal must say it happened before anything ran, got: {refusal}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// The transition refusal that remains — a later stage declaring a
/// checkout boundary, which is a property of the Route file rather than
/// of this submission, and so is not refused at submit.
///
/// The recovery is the point: the first stage's Claim stands, and the
/// Work is recorded *failed* with the reservation's own words, rather
/// than left Active with a closed Run that `retry` and `fail` both
/// refuse. A terminal Work can be inspected and cleaned.
#[test]
fn a_refused_transition_records_the_work_failed_rather_than_wedging_it() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = deterministic_then_bounded_actor_route(estate);
    let (work_id, _run_id, waypoint) =
        submit_mixed_output_only(estate, &route, "doc-set-1", &[]).expect("admitted");
    assert_eq!(waypoint, "deterministic-then-bounded-actor/wp-1");

    let (_ok, log) = run_deterministic(estate, &work_id);

    let result = status_result(&pointer.socket, &work_id);
    assert_ne!(
        result["current_waypoint"].as_str(),
        Some("deterministic-then-bounded-actor/wp-2"),
        "a stage declaring a boundary this basis cannot inspect must not be reserved: {result:#}"
    );
    assert_eq!(
        result["state"].as_str(),
        Some("failed"),
        "a permanently unreservable transition must leave the Work terminal, not wedged: \
         {log} / {result:#}"
    );

    // Terminal means *reachable*: the wedge's own symptom was that no
    // verb would accept the Work at all — it was Active at a stage whose
    // Run was already closed, so every verb bounced off the state itself
    // before it could decide anything about the Work.
    //
    // `clean` is the verb that shows the difference, and what it returns
    // here is a decision about *this* Work rather than a bounce.
    //
    // Until ruling 0292 that decision was the named refusal
    // `DeterministicNotSupported` (ruling 0203's scope limit), because a
    // Deterministic stage had no owned residue `clean` could reason
    // about — it executed in the estate root, which nothing may remove.
    // It does now: an output-only Deterministic Run's execution
    // directory is this estate's own address for this Work, created and
    // registered by its own materialization, and disposing of it is the
    // eventual cleanup the owned-workflow outcome requires. So the
    // decision here is the disposal itself, and the directory is gone
    // afterwards. A *Git*-basis Deterministic World keeps 0203's
    // refusal, which `owned_deterministic_custody.rs` and the identity
    // controls below hold the rest of the line on.
    let (ok, cleaned) = run_clean(estate, &work_id);
    assert!(
        ok,
        "a terminal Work must get a decision about itself, and an owned deterministic directory \
         is disposable: {cleaned:#}"
    );
    assert!(
        !estate.join("worktrees").join(&work_id).exists(),
        "the owned execution directory was removed: {cleaned:#}"
    );

    stop_wirkd(estate, wirkd_child);
}

// ---- F2-F4: artifact custody --------------------------------------------

/// A Claim vouches for the bytes it validated. If what is at the address
/// is no longer those bytes, both verbs refuse and the export writes
/// nothing — the digest is never printed over a buffer nothing checked.
#[test]
fn artifact_read_and_export_refuse_bytes_that_changed_after_the_claim() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), "# Draft\n").expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    // The stored snapshot, rewritten with different bytes of the same
    // length: a length check alone would not notice.
    let stored = estate
        .join("works")
        .join(&work_id)
        .join("outputs")
        .join("claims")
        .join(&claim_id)
        .join("draft.md");
    assert!(stored.is_file(), "the claim snapshot exists at {stored:?}");
    fs::write(&stored, "# Dr@ft\n").expect("rewrite the stored bytes");

    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &["read", "--claim", &claim_id, "--name", "draft.md"],
    );
    assert_ne!(code, Some(0), "changed bytes must refuse: {stderr}");
    assert!(
        stderr.contains("ArtifactBytesChanged"),
        "expected the daemon's own refusal, got: {stderr}"
    );

    let destination = estate_dir.path().join("exported-changed.md");
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            destination.to_str().expect("utf-8 destination"),
        ],
    );
    assert_ne!(code, Some(0), "changed bytes must refuse: {stderr}");
    assert!(
        !destination.exists(),
        "a refused export must leave no destination behind"
    );

    stop_wirkd(estate, wirkd_child);
}

/// `exists()` follows links, so a *dangling* symlink at `--to` answered
/// "nothing there": the write landed at the link's target, somewhere the
/// caller never named, and the read-back followed the same link and
/// reported success. An ordinary export is atomic and clobbers nothing,
/// symlinks included.
#[test]
fn artifact_export_refuses_a_dangling_symlink_destination() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), "# Draft\n").expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    let elsewhere = estate_dir.path().join("elsewhere.md");
    let destination = estate_dir.path().join("dangling.md");
    std::os::unix::fs::symlink(&elsewhere, &destination).expect("create a dangling symlink");

    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            destination.to_str().expect("utf-8 destination"),
        ],
    );
    assert_ne!(code, Some(0), "a symlink destination must be refused");
    assert!(
        !elsewhere.exists(),
        "the export was written through the link, to a path the caller never named"
    );

    // --force respects the destination the caller actually named; it
    // does not write through whatever is standing at it.
    let (code, _out, stderr_forced) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            destination.to_str().expect("utf-8 destination"),
            "--force",
        ],
    );
    assert_ne!(
        code,
        Some(0),
        "--force must refuse a non-regular destination: {stderr_forced}"
    );
    assert!(
        !elsewhere.exists(),
        "--force followed the link: {stderr} / {stderr_forced}"
    );

    stop_wirkd(estate, wirkd_child);
}

/// What `--force` *is* for: an explicit destination the caller owns,
/// holding a regular file.
#[test]
fn artifact_export_force_replaces_a_regular_file() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let draft = "# Draft\n\ncites docs-a/a.md\n";
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), draft).expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    let destination = estate_dir.path().join("exported.md");
    fs::write(&destination, "stale\n").expect("occupy the destination");
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            destination.to_str().expect("utf-8 destination"),
            "--force",
        ],
    );
    assert_eq!(code, Some(0), "--force export failed: {stderr}");
    assert_eq!(
        fs::read(&destination).expect("read the export"),
        draft.as_bytes(),
        "--force must leave the Claim's own bytes at the destination"
    );

    stop_wirkd(estate, wirkd_child);
}

/// An export reads managed storage; it never writes into it. `--force`
/// aimed at the artifact's own stored path would otherwise rewrite the
/// very bytes the Claim was validated against.
#[test]
fn artifact_export_refuses_the_artifacts_own_managed_storage() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let draft = "# Draft\n";
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), draft).expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    let stored = estate
        .join("works")
        .join(&work_id)
        .join("outputs")
        .join("claims")
        .join(&claim_id)
        .join("draft.md");
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            stored.to_str().expect("utf-8 destination"),
            "--force",
        ],
    );
    assert_ne!(
        code,
        Some(0),
        "an export must never write into the area it reads from"
    );
    assert!(
        stderr.contains("managed storage"),
        "expected the refusal to name managed storage, got: {stderr}"
    );
    assert_eq!(
        fs::read(&stored).expect("the stored bytes survive"),
        draft.as_bytes(),
        "the Claim's own stored bytes must be untouched"
    );

    stop_wirkd(estate, wirkd_child);
}

/// `read` writes nothing, so `--force` has nothing to force. It was
/// accepted and silently ignored; a flag that does nothing is a promise
/// the verb does not keep.
#[test]
fn artifact_read_refuses_force() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), "# Draft\n").expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "read", "--claim", &claim_id, "--name", "draft.md", "--force",
        ],
    );
    assert_ne!(code, Some(0), "read must refuse --force: {stderr}");

    stop_wirkd(estate, wirkd_child);
}

/// A historical Run id is not current execution authority. A stale pane
/// restarted with an old triple in its environment must not go on
/// reading this Work's claimed artifacts.
#[test]
fn a_superseded_run_does_not_read_this_works_claimed_artifacts() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_revise_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), "# Draft\n").expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "stage 1 claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    // Stage two opens, is interrupted, and is superseded by a retry.
    let stale_run = current_run_id(&pointer.socket, &work_id);
    assert_ne!(stale_run, run_id, "the Work advanced to a second stage");
    fail_run(&pointer.socket, estate, &work_id, &stale_run);
    let (code, log) = retry_cli(estate, &work_id);
    assert_eq!(code, Some(0), "retry failed: {log}");
    let current = current_run_id(&pointer.socket, &work_id);
    assert_ne!(current, stale_run, "the retry opened a fresh Run");

    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &stale_run,
        &["read", "--claim", &claim_id, "--name", "draft.md"],
    );
    assert_ne!(code, Some(0), "a superseded Run must be refused: {stderr}");
    assert!(
        stderr.contains("superseded"),
        "expected the refusal to say why, got: {stderr}"
    );

    // The current Run still reads the same prior Claim: this bounds the
    // caller, never the Claim's own scope.
    let (code, out, stderr) = artifact_verb(
        estate,
        &work_id,
        &current,
        &["read", "--claim", &claim_id, "--name", "draft.md"],
    );
    assert_eq!(code, Some(0), "the current Run must still read: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out), "# Draft\n");

    stop_wirkd(estate, wirkd_child);
}

/// `wirk world show --json`, run with this Run's own injected triple —
/// the one public surface a second Actor actually has.
fn world_show_json(estate: &Path, work_id: &str, run_id: &str) -> serde_json::Value {
    let output = wirk_cli()
        .args(["world", "show", "--json"])
        .env("WIRK_ESTATE_ROOT", estate)
        .env("WIRK_WORK_ID", work_id)
        .env("WIRK_RUN_ID", run_id)
        .output()
        .expect("wirk world show runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "wirk world show: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("world show emits JSON")
}

/// Discovery through the public surface, not through the journal.
///
/// Every other check in this file learns the Claim id by replaying the
/// journal (`validated_claim_id`) — a privilege a real actor does not
/// have. What a second Actor actually gets is its delivered orientation,
/// which binds the prior stage's artifacts by the coordinate
/// `claim/<id>/artifact/<name>`. This drives `--claim` from that
/// coordinate and from nothing else.
#[test]
fn the_second_stage_discovers_the_artifact_coordinate_through_world_show() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    publish_document_source(estate, "docs-a", &[("a.md", "# A\n\nalpha evidence.\n")]);
    let route = output_only_actor_revise_oriented_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor_with(estate, &route, "doc-set-1", &["docs-a:read"])
            .expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);

    let draft = format!("# Draft\n\ncites docs-a/a.md\n\n{}\n", "body. ".repeat(200));
    let staging = output_dir(estate, &work_id, &run_id);
    fs::write(staging.join("draft.md"), &draft).expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "stage 1 claim refused: {stdout}");

    let next_run = current_run_id(&pointer.socket, &work_id);
    assert_ne!(next_run, run_id, "the Work advanced to a second stage");
    let world = world_show_json(estate, &work_id, &next_run);
    let rendered = world.to_string();
    let coordinate = rendered
        .match_indices("claim/")
        .find_map(|(at, _)| {
            let tail = &rendered[at..];
            let end = tail.find('"')?;
            let candidate = &tail[..end];
            candidate
                .contains("/artifact/draft.md")
                .then(|| candidate.to_string())
        })
        .unwrap_or_else(|| {
            panic!("the delivered orientation names no artifact coordinate: {world:#}")
        });

    // `claim/<id>/artifact/<name>` — the Claim id is read out of the
    // coordinate the public surface delivered.
    let parts: Vec<&str> = coordinate.split('/').collect();
    assert_eq!(parts.len(), 4, "unexpected coordinate shape: {coordinate}");
    let (code, out, stderr) = artifact_verb(
        estate,
        &work_id,
        &next_run,
        &["read", "--claim", parts[1], "--name", parts[3]],
    );
    assert_eq!(code, Some(0), "wirk artifact read failed: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out),
        draft,
        "the coordinate the second stage discovered must address the prior Claim's own bytes"
    );

    stop_wirkd(estate, wirkd_child);
}

// ---- ruling 0283: ownership registration, export authority ---------------

/// An actor tidying its own execution directory is ordinary work in its
/// own area. It must not cost this Work the ability to have that
/// directory recognised and released.
///
/// The marker lives *inside* the directory the actor writes in, so it is
/// removable by the thing it describes. The creation identity lives in
/// the journal, which the actor cannot reach — git keeps its own
/// worktree registration outside the worktree for the same reason — and
/// it names the directory object rather than a path or a Work name, so a
/// marker copied in from elsewhere does not stand in for it either.
#[test]
fn removing_the_marker_does_not_cost_this_work_its_registration() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    drive_to_terminal(estate, &work_id, &run_id);

    let marker = worktree.join(".wirk-owned");
    assert!(marker.is_file(), "the materialization wrote its marker");
    fs::remove_file(&marker).expect("the actor removes a file in its own directory");

    let (ok, result) = run_clean(estate, &work_id);
    assert!(ok, "clean failed: {result:#}");
    assert_eq!(
        result["worktree_state"].as_str(),
        Some("registered"),
        "the journal registers the directory this Work created, marker or no marker: {result:#}"
    );
    assert_eq!(
        result["worktree_removed"].as_bool(),
        Some(true),
        "a registered directory is released: {result:#}"
    );
    assert!(!worktree.exists(), "the owned directory is gone from disk");

    stop_wirkd(estate, wirkd_child);
}

/// A directory swapped for an unrelated real one is still refused when
/// the marker is *carried across with it*. A name in a copied file says
/// what it was copied from, never what this estate created.
#[test]
fn a_copied_marker_does_not_make_a_substituted_directory_this_works_own() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    let worktree =
        materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    drive_to_terminal(estate, &work_id, &run_id);

    let marker = fs::read(worktree.join(".wirk-owned")).expect("read the genuine marker");
    fs::remove_dir_all(&worktree).expect("remove the genuine materialization");
    fs::create_dir_all(&worktree).expect("create an unrelated directory in its place");
    fs::write(worktree.join(".wirk-owned"), &marker).expect("carry the marker across");
    fs::write(worktree.join("original.md"), "untouched\n").expect("write the original");

    let (ok, result) = run_clean(estate, &work_id);
    assert!(ok, "clean should report, not fail: {result:#}");
    assert_eq!(
        result["worktree_state"].as_str(),
        Some("unproven"),
        "a copied marker is not creation identity: {result:#}"
    );
    assert!(
        worktree.join("original.md").exists(),
        "cleanup removed a directory this estate did not create"
    );

    stop_wirkd(estate, wirkd_child);
}

/// Two Works in one estate. An export is a read of managed storage; it
/// is never a way to write into another Work's half of it.
///
/// `--to` is real destination authority over where *this* caller's
/// output goes. It is not authority over another Work's validated Claim
/// bytes, which is what `--force` onto that Claim's stored path would
/// have rewritten: detectable afterwards (that Claim's own reads report
/// changed bytes), and destructive now.
#[test]
fn artifact_export_refuses_another_works_claimed_bytes() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);

    // The other Work, with a validated Claim of its own.
    let (other_work, other_run, other_waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(
        estate,
        &pointer.socket,
        &other_work,
        &other_run,
        &other_waypoint,
    );
    let other_bytes = "# Theirs\n";
    fs::write(
        output_dir(estate, &other_work, &other_run).join("draft.md"),
        other_bytes,
    )
    .expect("write the other Work's declared output");
    let (code, stdout) = claim(estate, &other_work, &other_run);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let other_claim = validated_claim_id(estate, &other_work, &other_run);
    let their_stored = estate
        .join("works")
        .join(&other_work)
        .join("outputs")
        .join("claims")
        .join(&other_claim)
        .join("draft.md");
    assert!(their_stored.is_file(), "the other Claim's snapshot exists");

    // This Work, with its own validated Claim and its own valid triple.
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    fs::write(
        output_dir(estate, &work_id, &run_id).join("draft.md"),
        "# Mine\n",
    )
    .expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            their_stored.to_str().expect("utf-8 destination"),
            "--force",
        ],
    );
    assert_ne!(
        code,
        Some(0),
        "one Work must not write into another Work's managed storage"
    );
    assert!(
        stderr.contains("managed"),
        "expected a refusal naming managed storage, got: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&their_stored).expect("their bytes survive"),
        other_bytes,
        "another Work's validated Claim bytes must be untouched"
    );

    // The journal is the other half of that area, and is protected by
    // the same rule.
    let their_journal = estate
        .join("works")
        .join(&other_work)
        .join("journal.ndjson");
    let before = fs::read(&their_journal).expect("their journal exists");
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            their_journal.to_str().expect("utf-8 destination"),
            "--force",
        ],
    );
    assert_ne!(code, Some(0), "the Trail is not an export destination");
    assert!(
        stderr.contains("managed"),
        "expected a refusal naming managed storage, got: {stderr}"
    );
    assert_eq!(
        fs::read(&their_journal).expect("their journal survives"),
        before,
        "another Work's journal must be untouched"
    );

    stop_wirkd(estate, wirkd_child);
}

/// The destination inside that area this workflow genuinely needs: the
/// caller's own Run output directory, which is where a revising stage
/// puts the prior artifact it is about to work from, and exactly what
/// `wirk output dir` prints.
///
/// A blanket refusal of the whole managed area would have been simpler
/// and would have broken this.
#[test]
fn artifact_export_into_this_runs_own_output_directory_is_allowed() {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path();
    let (wirkd_child, pointer) = start_wirkd(estate);

    let route = output_only_actor_route(estate);
    let (work_id, run_id, waypoint) =
        submit_output_only_actor(estate, &route, "doc-set-1").expect("admitted");
    materialize_output_only_run(estate, &pointer.socket, &work_id, &run_id, &waypoint);
    let staging = output_dir(estate, &work_id, &run_id);
    let draft = "# Draft\n";
    fs::write(staging.join("draft.md"), draft).expect("write the declared output");
    let (code, stdout) = claim(estate, &work_id, &run_id);
    assert_eq!(code, Some(0), "claim refused: {stdout}");
    let claim_id = validated_claim_id(estate, &work_id, &run_id);

    let destination = staging.join("prior-draft.md");
    let (code, _out, stderr) = artifact_verb(
        estate,
        &work_id,
        &run_id,
        &[
            "export",
            "--claim",
            &claim_id,
            "--name",
            "draft.md",
            "--to",
            destination.to_str().expect("utf-8 destination"),
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "a Run must be able to export into its own output directory: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&destination).expect("the export landed"),
        draft,
        "the exported bytes are the Claim's own"
    );

    stop_wirkd(estate, wirkd_child);
}
