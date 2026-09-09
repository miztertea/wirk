//! P3 execution-recovery item 2, as corrected: `wirk run`'s own
//! reattachment decision, driven through the real `wirk` binary against
//! a real `wirkd` and real git repositories — one admitted case and
//! four adverse controls, each planted on disk rather than described.
//!
//! The prior pass shipped this item with no `wirk run`-level test of its
//! own (its own report says so: "Not built here: a fresh dedicated
//! adversarial test that plants a foreign branch or a disconnected
//! repository at the exact `worktree_path` and drives it through `wirk
//! run` itself"). These are that test.
//!
//! **No Herdr session is needed and none is started.** `run_command`
//! decides reattachment — worktree materialization, branch identity,
//! repository identity, base ancestry — strictly *before* it connects
//! to a Herdr socket, so each case below is observed at exactly the
//! point it is decided: the admitted case prints its recovery line and
//! only then fails to reach the (deliberately absent) Herdr socket; a
//! refused case never gets that far. Both exit 2, so the exit code
//! alone is never the assertion — the printed decision is.

#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirk_core::{EventKind, Journal, RunId, WorkId, World};
use wirkd::{RecordPayload, Reply, Request, WirkdPointer};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

fn wait_for_pointer(estate: &Path) -> WirkdPointer {
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
            "wirkd pointer file never appeared at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=recovery",
            "-c",
            "user.email=recovery@invalid",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
}

fn rev_parse(dir: &Path, rev: &str) -> String {
    String::from_utf8_lossy(&git(dir, &["rev-parse", rev]).stdout)
        .trim()
        .to_string()
}

fn submit_actor(estate: &Path, repo: &Path) -> String {
    let route_json = r#"{"id":"reattach","waypoints":[{"id":"reattach/wp-1","kind":"Actor","intent":"commit progress on this Run's own branch","declared_outputs":[{"name":"report.md","required":true}],"boundary":["**"]}]}"#;
    route_fixture::write_route(estate, "reattach", route_json);
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route", "reattach", "--kind", "actor", "--repo-path"])
        .arg(repo)
        .args(["--base", "HEAD"])
        .output()
        .expect("work submit runs");
    assert!(
        output.status.success(),
        "work submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    let mut work_id = String::new();
    for pair in words.chunks(2) {
        if let [key, value] = pair
            && *key == "work_id"
        {
            work_id = (*value).to_string();
        }
    }
    assert!(!work_id.is_empty(), "unexpected submit stdout: {stdout:?}");
    work_id
}

struct KillOnDrop(Vec<std::process::Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One `wirk run` invocation against a Herdr socket path that does not
/// exist. Everything this file asserts is decided before that socket is
/// ever reached, so the absent socket is the harness, not a workaround:
/// it stops the invocation immediately after the reattachment decision
/// it exists to observe. Returns (stdout, stderr).
fn wirk_run(estate: &Path, work_id: &str) -> (String, String) {
    let output = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--session", "no-such-session"])
        .args(["--herdr-socket"])
        .arg(estate.join("no-such-herdr.sock"))
        .args(["--actor-kind", "opencode"])
        .output()
        .expect("wirk run runs");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn reserved_actor(estate: &Path, work_id: &str) -> wirk_core::ActorWorld {
    let journal = Journal::open(estate.join("works").join(work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays");
    let mut found = None;
    for event in &events {
        if let EventKind::WaypointReserved {
            world: World::Actor(actor),
            ..
        } = &event.kind
        {
            found = Some(actor.clone());
        }
    }
    found.expect("a reserved Actor World")
}

/// The estate, wirkd, repo and materialized worktree every case starts
/// from: one `wirk run` that materializes the Run's own worktree and
/// branch (`WorktreeCreated` + `WaypointReserved`) and then stops at the
/// absent Herdr socket, exactly as a driver whose daemon delivery was
/// lost does.
struct Materialized {
    _estate_dir: tempfile::TempDir,
    _repo_dir: tempfile::TempDir,
    _guard: KillOnDrop,
    estate: std::path::PathBuf,
    repo: std::path::PathBuf,
    work_id: String,
    worktree: std::path::PathBuf,
    branch: String,
    base_sha: String,
}

fn materialize() -> Materialized {
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();
    let repo_dir = tempfile::tempdir().expect("repo tempdir");
    let repo = repo_dir.path().to_path_buf();
    init_repo(&repo);

    let mut guard = KillOnDrop(Vec::new());
    guard.0.push(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(&estate);

    let work_id = submit_actor(&estate, &repo);
    let (stdout, _stderr) = wirk_run(&estate, &work_id);
    assert!(
        stdout.contains("WorktreeCreated"),
        "the first invocation must materialize the Run's own worktree: {stdout}"
    );
    let actor = reserved_actor(&estate, &work_id);
    let worktree = estate.join("worktrees").join(&work_id);
    assert_eq!(actor.worktree_path, worktree);
    Materialized {
        _estate_dir: estate_dir,
        _repo_dir: repo_dir,
        _guard: guard,
        estate,
        repo,
        work_id,
        worktree,
        branch: actor.branch,
        base_sha: actor.base_sha,
    }
}

fn stop_wirkd(estate: &Path) {
    let _ = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(estate)
        .output();
}

const REFUSAL: &str = "the existing Run binding does not match the reusable checkout";

/// **The admitted case.** An actor that has really committed on this
/// Run's own branch, in this Run's own worktree, is reattached — its
/// commits are not reset, its World is not re-reserved, and its
/// `base_sha` is left exactly where it was pinned.
#[test]
fn committed_progress_on_this_runs_own_branch_is_reattached_without_resetting_it() {
    let m = materialize();
    fs::write(m.worktree.join("report.md"), b"real, committed progress\n").expect("write report");
    git(&m.worktree, &["add", "report.md"]);
    git(
        &m.worktree,
        &["commit", "-q", "-m", "the actor's own commit"],
    );
    let progressed = rev_parse(&m.worktree, "HEAD");
    assert_ne!(progressed, m.base_sha);

    let (stdout, stderr) = wirk_run(&m.estate, &m.work_id);
    assert!(
        stdout.contains("recovering in-Work progress"),
        "committed progress on this Run's own branch must be recovered: {stdout} / {stderr}"
    );
    assert!(
        !stderr.contains(REFUSAL),
        "and never refused as a foreign checkout: {stderr}"
    );

    // Nothing was reset and nothing was re-pinned.
    assert_eq!(
        rev_parse(&m.worktree, "HEAD"),
        progressed,
        "the actor's own commit must survive the reattachment"
    );
    assert_eq!(
        fs::read_to_string(m.worktree.join("report.md")).expect("report survives"),
        "real, committed progress\n"
    );
    let actor = reserved_actor(&m.estate, &m.work_id);
    assert_eq!(
        actor.base_sha, m.base_sha,
        "the reserved base is never widened onto the new HEAD"
    );
    assert_eq!(actor.worktree_path, m.worktree);
    stop_wirkd(&m.estate);
}

/// **P3 native closeout item 2.** A Work its owner has explicitly
/// canceled must not be reattached, even though the Run it was driving
/// is still `Open` — cancelling is a decision about the Work, and `fold`
/// deliberately leaves Run states alone, so `RunState::Open` on its own
/// is not a licence to resume.
///
/// The reproduction is the independent reviewer's own, in
/// `p3-world-loop/native-learning-use/raw/recovery-acceptance/
/// recovery-check.md`: `wirk work cancel` reported `work state:
/// canceled  run run-… state Open`, and the very next `wirk run`
/// answered "recovering in-Work progress: … reattaching without
/// resetting". Red before the change: exactly that line, on a canceled
/// Work with committed progress.
#[test]
fn a_canceled_work_is_not_reattached_to_its_leftover_open_run() {
    let m = materialize();
    fs::write(
        m.worktree.join("report.md"),
        b"progress before the cancel
",
    )
    .expect("write report");
    git(&m.worktree, &["add", "report.md"]);
    git(&m.worktree, &["commit", "-q", "-m", "before the cancel"]);
    let progressed = rev_parse(&m.worktree, "HEAD");

    let cancel = Command::new(wirk_bin())
        .args(["work", "cancel", "--estate"])
        .arg(&m.estate)
        .args(["--work", &m.work_id, "--reason", "the owner stopped this"])
        .output()
        .expect("wirk work cancel runs");
    assert!(
        cancel.status.success(),
        "wirk work cancel failed: {}",
        String::from_utf8_lossy(&cancel.stderr)
    );

    // The precondition the defect needs: the Work is canceled while its
    // Run is still Open. Read from the journal, not assumed.
    let journal = Journal::open(m.estate.join("works").join(&m.work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays");
    let work = wirk_core::fold(&events);
    assert_eq!(work.state, wirk_core::WorkState::Canceled, "{work:?}");
    // The Run is still Open: nothing closed it, because cancelling the
    // Work deliberately does not rewrite Run states.
    let opened: Vec<&RunId> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::RunOpened { run, .. } => Some(run),
            _ => None,
        })
        .collect();
    assert_eq!(opened.len(), 1, "one Run: {events:?}");
    assert!(
        !events.iter().any(|event| matches!(
            event.kind,
            EventKind::RunFailed { .. } | EventKind::RunVanished | EventKind::ClaimRecorded { .. }
        )),
        "the defect needs a leftover *Open* Run to reattach to: {events:?}"
    );

    let (stdout, stderr) = wirk_run(&m.estate, &m.work_id);
    assert!(
        !stdout.contains("recovering in-Work progress"),
        "a canceled Work must not be resumed: {stdout} / {stderr}"
    );
    assert!(
        stderr.contains("is canceled") && stderr.contains(&m.work_id),
        "the refusal must name the Work and its state: {stderr} / {stdout}"
    );

    // Nothing about the checkout is touched by the refusal — the
    // progress is preserved for whoever inspects it, not discarded.
    assert_eq!(rev_parse(&m.worktree, "HEAD"), progressed);
    assert_eq!(
        fs::read_to_string(m.worktree.join("report.md")).expect("report survives"),
        "progress before the cancel
"
    );
    stop_wirkd(&m.estate);
}

/// **Adverse control: the same HEAD, the wrong branch.** Ancestry alone
/// would admit this — the HEAD has not moved at all. Identity refuses
/// it: the checkout is no longer on this Run's own branch.
#[test]
fn the_same_head_on_a_different_branch_is_refused() {
    let m = materialize();
    git(
        &m.worktree,
        &["checkout", "-q", "-b", "someone-elses-branch"],
    );
    assert_eq!(
        rev_parse(&m.worktree, "HEAD"),
        m.base_sha,
        "the control is precisely that the HEAD is unchanged"
    );

    let (stdout, stderr) = wirk_run(&m.estate, &m.work_id);
    assert!(
        stderr.contains(REFUSAL) && stderr.contains("someone-elses-branch"),
        "a foreign branch at the right path must be refused by name: {stderr} / {stdout}"
    );
    assert!(
        !stdout.contains("recovering in-Work progress"),
        "and never recovered: {stdout}"
    );
    assert!(
        stderr.contains(&m.branch),
        "the refusal must name this Run's own branch too: {stderr}"
    );
    stop_wirkd(&m.estate);
}

/// **Adverse control: a different repository at the exact
/// `worktree_path`, on a branch with this Run's own name.** Path
/// equality and a matching branch name are not identity.
#[test]
fn a_disconnected_repository_at_the_right_path_and_branch_name_is_refused() {
    let m = materialize();
    // Detach the real worktree, then put a wholly unrelated repository
    // in its place — same directory, same branch name, different
    // repository.
    let worktree_arg = m.worktree.to_string_lossy().into_owned();
    git(&m.repo, &["worktree", "remove", "--force", &worktree_arg]);
    fs::create_dir_all(&m.worktree).expect("recreate the path");
    init_repo(&m.worktree);
    git(&m.worktree, &["checkout", "-q", "-b", &m.branch]);
    assert!(
        m.worktree.join(".git").exists(),
        "the planted checkout is a real, separate repository"
    );

    let (stdout, stderr) = wirk_run(&m.estate, &m.work_id);
    assert!(
        stderr.contains(REFUSAL),
        "a disconnected repository at the right path must be refused: {stderr} / {stdout}"
    );
    assert!(
        !stdout.contains("recovering in-Work progress"),
        "and never recovered: {stdout}"
    );
    stop_wirkd(&m.estate);
}

/// **Adverse control: a rewritten base.** The right repository, the
/// right branch, a moved HEAD — but this Run's reserved `base_sha` is
/// no longer an ancestor of it. Recovery is for progress *on top of*
/// the pinned base, never for a history that no longer contains it.
#[test]
fn a_head_that_no_longer_descends_from_the_reserved_base_is_refused() {
    let m = materialize();
    fs::write(m.worktree.join("report.md"), b"progress\n").expect("write report");
    git(&m.worktree, &["add", "report.md"]);
    git(&m.worktree, &["commit", "-q", "-m", "progress"]);
    // Rewrite the base out of the history: an amend of the root commit
    // leaves a HEAD with entirely different SHAs.
    git(&m.worktree, &["reset", "-q", "--hard", &m.base_sha]);
    git(
        &m.worktree,
        &[
            "commit",
            "-q",
            "--amend",
            "--allow-empty",
            "-m",
            "rewritten base",
        ],
    );
    let rewritten = rev_parse(&m.worktree, "HEAD");
    assert_ne!(rewritten, m.base_sha);
    assert_eq!(
        rev_parse(&m.worktree, "HEAD"),
        rewritten,
        "the control is a HEAD that does not contain the reserved base"
    );

    let (stdout, stderr) = wirk_run(&m.estate, &m.work_id);
    assert!(
        stderr.contains(REFUSAL),
        "a rewritten base must still be refused: {stderr} / {stdout}"
    );
    assert!(
        !stdout.contains("recovering in-Work progress"),
        "and never recovered: {stdout}"
    );
    stop_wirkd(&m.estate);
}

/// **Adverse control: the wrong destination.** The same estate reached
/// by a different path computes a different reusable checkout than the
/// one this Run is bound to. The binding is the reserved
/// `worktree_path`, not "whatever this invocation computed".
#[test]
fn a_reusable_checkout_at_a_different_path_than_the_binding_is_refused() {
    let m = materialize();
    // A second name for the very same estate directory. Everything
    // about the Run is unchanged; only the path this invocation
    // computes its worktree from differs.
    let alias_parent = tempfile::tempdir().expect("alias tempdir");
    let alias = alias_parent.path().join("estate-by-another-name");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&m.estate, &alias).expect("symlink the estate");

    let (stdout, stderr) = wirk_run(&alias, &m.work_id);
    assert!(
        stderr.contains(REFUSAL),
        "a computed checkout that is not this Run's own bound one must be refused: \
         {stderr} / {stdout}"
    );
    assert!(
        !stdout.contains("recovering in-Work progress"),
        "and never recovered: {stdout}"
    );
    stop_wirkd(&m.estate);
}

fn wirkd_record_refusal(socket: &Path, work_id: &str, run: &str, status: &str) -> (String, String) {
    let reply = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: WorkId(work_id.to_string()),
            run: Some(RunId(run.to_string())),
            kind: EventKind::LifecycleObserved {
                status: status.to_string(),
                detail: None,
            },
        }),
    )
    .expect("the record call is answered");
    match reply {
        Reply::Ok { .. } => panic!("expected a refusal, got Ok"),
        Reply::Err { error, .. } => (error.code, error.message),
    }
}

/// **P3 native closeout item 1a, wirkd's own half.** `handle_record`'s
/// guard refuses three different situations; it used to answer all of
/// them with one sentence — "record does not target the current open
/// Run" — which is what a driver saw in the live `run_verb` retry
/// failures and could only read as fatal.
///
/// Nothing here is admitted that was refused before: both records below
/// are still refused, no event is folded, and no record is re-aimed at
/// another Run to make it land. What is pinned is that the two facts are
/// told apart and named, so a driver that raced its own Run's settlement
/// can stop observing while a driver that has been *replaced* still
/// stops driving.
#[test]
fn the_records_guard_names_supersession_and_settlement_apart() {
    let m = materialize();
    let pointer = wait_for_pointer(&m.estate);
    let first_run = Journal::open(m.estate.join("works").join(&m.work_id))
        .expect("open journal")
        .replay()
        .expect("replay")
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::RunOpened { run, .. } => Some(run.0.clone()),
            _ => None,
        })
        .expect("a first Run");

    // (1) Settled: this Run has reached its own outcome. Its own code,
    // so a driver can read it without parsing prose.
    let failed = wirkd::client::call(
        &pointer.socket,
        &Request::record(RecordPayload {
            work_id: WorkId(m.work_id.clone()),
            run: Some(RunId(first_run.clone())),
            kind: EventKind::RunFailed {
                cause: wirk_core::FailureCause {
                    status: Some("stuck".to_string()),
                    request_id: None,
                    at: wirk_core::Timestamp(0),
                    detail: Some("the driver lost its actor".to_string()),
                },
            },
        }),
    )
    .expect("the record call is answered");
    assert!(matches!(failed, Reply::Ok { .. }), "{failed:?}");

    let (code, message) = wirkd_record_refusal(&pointer.socket, &m.work_id, &first_run, "Idle");
    assert_eq!(
        code, "RunSettled",
        "a record against a settled Run has its own code: {message}"
    );
    assert!(
        message.contains("has already settled") && message.contains(&first_run),
        "and names the Run and what settled it: {message}"
    );

    // (2) Superseded: a retry has opened a newer Run for this Waypoint.
    // A different fact, a different sentence, and still `InvalidTransition`.
    let retry = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(&m.estate)
        .args(["--work", &m.work_id])
        .output()
        .expect("wirk work retry runs");
    assert!(
        retry.status.success(),
        "wirk work retry failed: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let fresh_run = Journal::open(m.estate.join("works").join(&m.work_id))
        .expect("open journal")
        .replay()
        .expect("replay")
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::RunOpened { run, .. } => Some(run.0.clone()),
            _ => None,
        })
        .next_back()
        .expect("the retry's own Run");
    assert_ne!(fresh_run, first_run);

    let (code, message) = wirkd_record_refusal(&pointer.socket, &m.work_id, &first_run, "Idle");
    assert_eq!(
        code, "InvalidTransition",
        "supersession is not settlement: {message}"
    );
    assert!(
        message.contains("has since opened Run") && message.contains(&fresh_run),
        "the replaced driver is told which Run replaced it: {message}"
    );

    // Neither refusal wrote anything.
    let events = Journal::open(m.estate.join("works").join(&m.work_id))
        .expect("open journal")
        .replay()
        .expect("replay");
    assert!(
        !events.iter().any(|event| matches!(
            &event.kind,
            EventKind::LifecycleObserved { status, .. } if status == "Idle"
        )),
        "a refused record journals nothing: {events:?}"
    );
    stop_wirkd(&m.estate);
}

/// **Adverse control: a superseded Run.** A retry mints a fresh Run and
/// closes the one before it. The superseded Run is never what a later
/// `wirk run` reattaches to: nothing is recorded against it again, its
/// own last word stays its supersession, and the reusable checkout it
/// left behind is carried by the *new* Run's binding, not by its own.
#[test]
fn after_a_retry_nothing_is_ever_recorded_against_the_superseded_run() {
    let m = materialize();
    let events_by_run = |estate: &Path, work_id: &str| -> Vec<(String, String)> {
        Journal::open(estate.join("works").join(work_id))
            .expect("open journal")
            .replay()
            .expect("replay")
            .iter()
            .filter_map(|event| {
                event
                    .run
                    .as_ref()
                    .map(|run| (run.0.clone(), format!("{:?}", event.kind)))
            })
            .collect()
    };
    let before = events_by_run(&m.estate, &m.work_id);
    let superseded = before.last().expect("a first Run").0.clone();

    // Put the Work where a retry is legitimate, through wirkd's own
    // write path (the `record` verb — never the journal file opened
    // directly): this Run really failed stuck, exactly as a driver that
    // lost its actor reports.
    let pointer = wait_for_pointer(&m.estate);
    let failed = wirkd::client::call(
        &pointer.socket,
        &Request::record(RecordPayload {
            work_id: WorkId(m.work_id.clone()),
            run: Some(RunId(superseded.clone())),
            kind: EventKind::RunFailed {
                cause: wirk_core::FailureCause {
                    status: Some("stuck".to_string()),
                    request_id: None,
                    at: wirk_core::Timestamp(0),
                    detail: Some("the driver lost its actor".to_string()),
                },
            },
        }),
    )
    .expect("the record call is answered");
    assert!(
        matches!(failed, Reply::Ok { .. }),
        "recording the Run's own failure must be admitted: {failed:?}"
    );

    let retry = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(&m.estate)
        .args(["--work", &m.work_id])
        .output()
        .expect("wirk work retry runs");
    assert!(
        retry.status.success(),
        "wirk work retry failed: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let after_retry = events_by_run(&m.estate, &m.work_id);
    let fresh_run = after_retry
        .last()
        .map(|(run, _)| run.clone())
        .expect("the retry's own Run");
    assert_ne!(fresh_run, superseded, "a retry mints a fresh Run");

    // The superseded Run's own last word.
    let last_for_superseded = after_retry
        .iter()
        .rfind(|(run, _)| run == &superseded)
        .expect("the superseded Run has events")
        .1
        .clone();
    assert!(
        last_for_superseded.contains("RunFailed") && last_for_superseded.contains("retried"),
        "the superseded Run's last event must be its own supersession: {last_for_superseded}"
    );

    // Now drive again. Whatever this invocation does, it does under the
    // fresh Run — the superseded Run gains nothing.
    let (stdout, _stderr) = wirk_run(&m.estate, &m.work_id);
    let after_run = events_by_run(&m.estate, &m.work_id);
    let superseded_before = after_retry
        .iter()
        .filter(|(run, _)| run == &superseded)
        .count();
    let superseded_after = after_run
        .iter()
        .filter(|(run, _)| run == &superseded)
        .count();
    assert_eq!(
        superseded_before, superseded_after,
        "no later invocation may record anything against a superseded Run: {after_run:?}"
    );
    assert!(
        !stdout.contains("recovering in-Work progress"),
        "and the untouched checkout is not a recovery either: {stdout}"
    );

    // The reusable checkout the superseded Run left behind is still
    // exactly the binding the Work carries, base pinned where it was.
    let actor = reserved_actor(&m.estate, &m.work_id);
    assert_eq!(actor.worktree_path, m.worktree);
    assert_eq!(actor.base_sha, m.base_sha);
    stop_wirkd(&m.estate);
}
