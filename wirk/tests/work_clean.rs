//! Live integration tests for `wirk work clean` (P4.5 first increment,
//! ruling 0203). Drives the real built binary end to end: a real `wirk
//! wirkd`, real `wirk work submit`/`wirk run` against a real
//! `LiveHerdrSession`, a scripted actor (0049 D148) standing in for a
//! model so the Claim path is real without needing one, and the real
//! `wirk work clean` command itself. No fakes anywhere in this file
//! (ruling 0040): every refusal is proven against the real installed
//! `git` and a real Herdr session.

#[path = "../../wirk-herdr/tests/support/live_herdr.rs"]
mod live_herdr;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../../wirk-herdr/tests/support/scripted_actor.rs"]
mod scripted_actor;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wirkd::{ClaimPayload, Reply, Request, WirkdPointer};

use wirk_core::{ClaimKind, EventKind, ExecutionTriple, Journal, RunId, WorkId};
use wirk_herdr::HerdrClient;

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

fn scripted_actor_path(scripted: &scripted_actor::ScriptedActor) -> String {
    let wirk_dir = Path::new(wirk_bin())
        .parent()
        .expect("wirk binary has a parent directory");
    format!(
        "{}:{}:{}",
        scripted.bin_dir().display(),
        wirk_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
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
            "wirkd pointer file never appeared (readable) at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn init_repo(dir: &Path) {
    let init = Command::new("git")
        .current_dir(dir)
        .args(["init", "-q"])
        .status()
        .expect("git init runs");
    assert!(init.success());
    let commit = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=spike",
            "-c",
            "user.email=spike@invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ])
        .status()
        .expect("git commit runs");
    assert!(commit.success());
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git spawns")
}

/// No `declared_outputs`: a bare `wirk claim` with no artifacts at all
/// succeeds, so a Work driven through this Route reaches `Claimed` with
/// nothing in the checkout that Claim evidence could ever pin — the
/// ordinary shape most of this file's refusal checks need (they are
/// about the *checkout*, not about managed-output addressing).
fn submit_actor_named(
    estate: &Path,
    repo: &Path,
    intent: &str,
    name: &str,
) -> (String, String, String) {
    submit_actor_named_with_outputs(estate, repo, intent, name, "[]")
}

/// `declared_outputs_json` is the Route Waypoint's own JSON array
/// literal (e.g. `r#"[{"name":"report.md","required":true}]"#`) —
/// callers that need a managed-output or checkout-artifact Claim
/// (`ClaimEvidenceInCheckout`'s own positive and negative controls)
/// supply one explicitly.
fn submit_actor_named_with_outputs(
    estate: &Path,
    repo: &Path,
    intent: &str,
    name: &str,
    declared_outputs_json: &str,
) -> (String, String, String) {
    let route_json = format!(
        r#"{{"id":{name:?},"waypoints":[{{"id":"{name}/wp-1","kind":"Actor","intent":{intent:?},"declared_outputs":{declared_outputs_json},"boundary":["**"]}}]}}"#
    );
    route_fixture::write_route(estate, name, &route_json);

    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route", name, "--kind", "actor", "--repo-path"])
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

fn wait_for_event(estate: &Path, work_id: &str, mut matches: impl FnMut(&EventKind) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Ok(journal) = Journal::open(estate.join("works").join(work_id))
            && let Ok(events) = journal.replay()
            && events.iter().any(|e| matches(&e.kind))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the expected event never appeared in the journal within the deadline"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
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

/// Runs `wirk work clean --estate <estate> --work <id> [--dry-run]
/// --json`, returning `(exit_success, parsed_json_or_null, stderr)`.
fn run_clean(estate: &Path, work_id: &str, dry_run: bool) -> (bool, serde_json::Value, String) {
    let mut args = vec!["work", "clean", "--estate"];
    let estate_str = estate.to_string_lossy().into_owned();
    args.push(&estate_str);
    args.push("--work");
    args.push(work_id);
    if dry_run {
        args.push("--dry-run");
    }
    args.push("--json");
    let output = Command::new(wirk_bin())
        .args(&args)
        .output()
        .expect("wirk work clean runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let parsed = serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null);
    (output.status.success(), parsed, stderr)
}

/// `wirk wirkd status --work <id> --admin --json`'s own unwrapped result
/// object (a single named target is not enumerated, `main.rs`'s own
/// "single read is the object itself" comment) — used to check what the
/// public status surface actually discloses, not only what `wirk work
/// clean` itself just returned.
fn wirkd_status(estate: &Path, work_id: &str) -> serde_json::Value {
    let output = Command::new(wirk_bin())
        .args(["wirkd", "status", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--admin", "--json"])
        .output()
        .expect("wirk wirkd status runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    serde_json::from_str(stdout.trim()).expect("status prints one JSON object")
}

/// The same read as `wirkd_status`, without `--json` — the ordinary
/// human-facing surface (ruling 0224): what a person actually sees
/// running `wirk wirkd status --work <id> --admin`, unparsed.
fn wirkd_status_text(estate: &Path, work_id: &str) -> String {
    let output = Command::new(wirk_bin())
        .args(["wirkd", "status", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--admin"])
        .output()
        .expect("wirk wirkd status runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A validated Claim leaves the actor's own Herdr pane exactly as live
/// as it was — Claiming is not closing (QUALIFIED.md §3's own "the
/// first increment may require the operator to close the owned Herdr
/// workspace first"). Every test whose script ends in a Claim and then
/// expects a real `wirk work clean` to succeed closes that pane's own
/// workspace explicitly first, the same way an operator would — a
/// no-op, logged rather than asserted, if the pane is already gone by
/// the time this runs.
fn close_run_workspace(session: &live_herdr::LiveHerdrSession, run_id: &str) {
    let client = session.client();
    let agents = client.list_agents().expect("agent.list reaches Herdr");
    let Some(workspace_id) = agents
        .into_iter()
        .find(|pane| pane.name.as_deref() == Some(run_id))
        .map(|pane| pane.workspace_id)
    else {
        eprintln!("close_run_workspace({run_id}): no matching pane, nothing to close");
        return;
    };
    client
        .close_workspace(wirk_herdr::CloseWorkspace { workspace_id })
        .expect("closing the actor's own workspace");
}

fn worktree_list(repo: &Path) -> String {
    let output = git(repo, &["worktree", "list", "--porcelain"]);
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Drives one Work through submit -> run -> a real committed Claim
/// against a real Herdr session, and stops once `RunLaunched` lands so
/// the caller can drive the rest of the script itself. Returns the
/// spawned `wirk run` child (still running, guarded by the caller) plus
/// the ids.
fn submit_and_run_with_outputs(
    estate: &Path,
    repo: &Path,
    session: &live_herdr::LiveHerdrSession,
    path_env: &str,
    route_name: &str,
    intent: &str,
    declared_outputs_json: &str,
) -> (std::process::Child, String, String) {
    let (work_id, run_id, _waypoint) =
        submit_actor_named_with_outputs(estate, repo, intent, route_name, declared_outputs_json);
    let child = spawn_run(estate, session, path_env, &work_id);
    (child, work_id, run_id)
}

fn spawn_run(
    estate: &Path,
    session: &live_herdr::LiveHerdrSession,
    path_env: &str,
    work_id: &str,
) -> std::process::Child {
    let child = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(estate)
        .args(["--work", work_id, "--session", session.name()])
        .args(["--herdr-socket"])
        .arg(session.socket_path())
        .args(["--actor-kind", "opencode"])
        .env("PATH", path_env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wirk run");
    wait_for_event(estate, work_id, |kind| {
        matches!(kind, EventKind::RunLaunched { .. })
    });
    child
}

/// The decisive actual-use check (BRIEF.md / QUALIFIED.md §5): submit,
/// run, claim a real Work with a real committed edit, `--dry-run` first
/// (asserting no mutation), then a real `wirk work clean`, then
/// independently confirm with bare `git worktree list`, the runtime pin
/// directory, and `wirk work status --json`'s replayed journal — three
/// real tools, no fake. Also proves the idempotent retry.
#[test]
fn wirk_work_clean_removes_a_claimed_committed_checkout_preserving_the_branch() {
    // A managed-output Claim (ruling 0145): the actor writes its report
    // under `wirk output dir`, never into the checkout, then files a
    // bare `wirk claim` (0212/0213's own default, which selects every
    // required declared output by managed-output addressing). Nothing
    // in the checkout is Claim evidence, so `ClaimEvidenceInCheckout`
    // never applies here — this is the ordinary shape a cleanable
    // Claimed Work actually has.
    // Also commits real, harmless, unrelated source content into the
    // checkout first — ruling 0203's own "a committed-ahead clean
    // checkout is eligible when its branch preserves the commit".
    let scripted = scripted_actor::ScriptedActor::install(&[
        "edit:NOTES.md:a harmless committed note",
        "commit:add a note",
        "output_claim:report.md:a throwaway repo for wirk work clean",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_work_clean_removes_a_claimed_committed_checkout_preserving_the_branch",
        &[
            ("PATH", &path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(&estate);

    let (run_child, work_id, run_id) = submit_and_run_with_outputs(
        &estate,
        &repo,
        &session,
        &path_env,
        "clean-happy",
        "commit a note, write report.md to the managed output area, then claim",
        r#"[{"name":"report.md","required":true}]"#,
    );
    let output = run_child.wait_with_output().expect("reap wirk run");
    assert!(
        output.status.success(),
        "wirk run exit status: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let worktree_path = estate.join("worktrees").join(&work_id);
    let runtime_pin_dir = estate.join(".wirk").join("runtime").join(&run_id);
    let opencode_hook_dir = estate.join(".wirk").join("opencode").join(&run_id);
    assert!(
        worktree_path.is_dir(),
        "the actor's own committed worktree should still be on disk before cleanup"
    );
    assert!(
        runtime_pin_dir.join("bin").join("wirk").is_file(),
        "this Run's pinned wirk binary should exist before cleanup"
    );

    // The Claim landed, but the actor's own Herdr pane is untouched by
    // that — it stays live and registered until its own workspace is
    // closed. This is the "the command may require the operator to
    // close the owned Herdr workspace first" case (0203/QUALIFIED.md
    // §3): close it explicitly, as an operator would, before cleaning.
    close_run_workspace(&session, &run_id);

    // Dry run first: reports the same outcome a real call would, with
    // no mutation at all.
    let (ok, result, stderr) = run_clean(&estate, &work_id, true);
    assert!(ok, "dry-run clean failed: {stderr}");
    assert_eq!(result["dry_run"], serde_json::json!(true));
    assert_eq!(result["worktree_removed"], serde_json::json!(true));
    assert_eq!(
        result["runtime_pins_removed"],
        serde_json::json!([run_id.clone()])
    );
    assert!(
        worktree_path.is_dir(),
        "a dry run must not remove the worktree"
    );
    assert!(
        runtime_pin_dir.exists(),
        "a dry run must not remove the runtime pin directory"
    );

    // The real call.
    let (ok, result, stderr) = run_clean(&estate, &work_id, false);
    assert!(ok, "clean failed: {stderr}");
    assert_eq!(result["dry_run"], serde_json::json!(false));
    assert_eq!(result["worktree_removed"], serde_json::json!(true));
    assert_eq!(
        result["runtime_pins_removed"],
        serde_json::json!([run_id.clone()])
    );

    // Independent confirmation, three real tools, no fake.
    assert!(
        !worktree_path.exists(),
        "the worktree directory should be gone after a real clean"
    );
    let listing = worktree_list(&repo);
    assert!(
        !listing.contains(&worktree_path.display().to_string()),
        "bare `git worktree list` should no longer show the removed path: {listing}"
    );
    let branch_ref = format!("refs/heads/wirk/{work_id}");
    let show_ref = git(&repo, &["show-ref", "--verify", "--quiet", &branch_ref]);
    assert!(
        show_ref.status.success(),
        "the Work's own branch must survive worktree removal (0017 D54)"
    );
    let show_commit = git(&repo, &["log", &branch_ref, "--format=%s", "-n", "5"]);
    let log = String::from_utf8_lossy(&show_commit.stdout);
    assert!(
        log.contains("add a note"),
        "the committed-ahead source change must survive on the preserved branch: {log}"
    );
    assert!(
        !runtime_pin_dir.exists(),
        "the per-Run runtime pin directory should be removed"
    );
    assert!(
        !opencode_hook_dir.exists(),
        "the per-Run opencode hook directory should be removed"
    );
    let images_dir = estate.join(".wirk").join("runtime").join("images");
    // The shared content-addressed image store is untouched by this
    // increment regardless of whether anything was ever installed
    // there for this box.
    let _ = images_dir.exists();

    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    let cleaned_events: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::WorkCleaned { .. }))
        .collect();
    assert_eq!(
        cleaned_events.len(),
        1,
        "exactly one WorkCleaned event should be journaled"
    );
    if let EventKind::WorkCleaned {
        runs,
        worktree_removed,
        runtime_pins_removed,
        complete,
    } = &cleaned_events[0].kind
    {
        assert_eq!(runs, &[RunId(run_id.clone())]);
        assert!(*worktree_removed);
        assert_eq!(runtime_pins_removed, &[RunId(run_id.clone())]);
        assert!(
            *complete,
            "a call that ran every step to its own end says so"
        );
    }

    // Idempotent retry (QUALIFIED.md's own "Partial-cleanup retry",
    // widened here to "already fully cleaned"): a second call succeeds,
    // truthfully reporting nothing left to remove.
    let (ok, result, stderr) = run_clean(&estate, &work_id, false);
    assert!(ok, "second clean call failed: {stderr}");
    assert_eq!(result["worktree_removed"], serde_json::json!(false));
    assert_eq!(result["runtime_pins_removed"], serde_json::json!([]));

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
}

#[test]
fn wirk_work_clean_refuses_a_nonterminal_work() {
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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(&estate);

    // Submitted, never run: state stays Pending/Active, never terminal.
    let (work_id, _run_id, _waypoint) =
        submit_actor_named(&estate, &repo, "never actually run", "clean-nonterminal");

    let (ok, _result, stderr) = run_clean(&estate, &work_id, true);
    assert!(!ok, "clean should refuse a non-terminal Work");
    assert!(
        stderr.contains("NotTerminal"),
        "expected NotTerminal in stderr, got: {stderr}"
    );

    // Nothing touched: no worktree was ever created (no `wirk run` ran),
    // and the journal must not carry a WorkCleaned event.
    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::WorkCleaned { .. })),
        "a refused clean must not journal anything"
    );

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
}

/// Ownership, both halves QUALIFIED.md's own "Unresolved limits" left
/// open the plain-shell half of: an idle *registered agent* refuses via
/// `agent.list` (the part QUALIFIED.md already covered), and a *plain
/// shell* pane with no agent at all — cd'd into the same checkout —
/// also refuses, via `session.snapshot` + `pane.process_info` (this
/// increment's own addition). Both against one real Herdr session, one
/// real worktree.
#[test]
fn wirk_work_clean_refuses_a_live_registered_agent_and_a_plain_shell() {
    // The actor writes the file and then goes idle forever (no `claim:`
    // step) so its own pane stays a live, idle, *registered* agent —
    // the Claim below is filed directly over the wirkd socket,
    // bypassing the actor entirely (the same technique `run_verb.rs`
    // uses), so the Work goes terminal while the pane is still there.
    let scripted =
        scripted_actor::ScriptedActor::install(&["output:report.md:written by the idle actor"]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_work_clean_refuses_a_live_registered_agent_and_a_plain_shell",
        &[
            ("PATH", &path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);

    let (run_child, work_id, run_id) = submit_and_run_with_outputs(
        &estate,
        &repo,
        &session,
        &path_env,
        "clean-live-agent",
        "write report.md to the managed output area, then wait forever",
        r#"[{"name":"report.md","required":true}]"#,
    );
    guard.0.push(run_child);

    // Wait for the actor's own write to actually land before claiming
    // it (a bounded poll on the managed staging file, not a timed
    // sleep, and never the checkout — this Claim is managed-output
    // evidence, not checkout evidence).
    let worktree_path = estate.join("worktrees").join(&work_id);
    let staged_report = estate
        .join("works")
        .join(&work_id)
        .join("outputs")
        .join("staging")
        .join(&run_id)
        .join("report.md");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !staged_report.is_file() {
        assert!(
            Instant::now() < deadline,
            "the scripted actor's own managed-output write never landed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // File the Claim directly over the socket, bypassing the actor
    // (`run_verb.rs`'s own technique): the Work goes terminal while the
    // real Herdr pane stays live and idle.
    let mut outputs = std::collections::BTreeSet::new();
    outputs.insert("report.md".to_string());
    let claim_reply = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.clone()),
                run_id: RunId(run_id.clone()),
            },
            kind: ClaimKind::Done,
            artifacts: Default::default(),
            outputs,
        }),
    )
    .expect("claim call reaches wirkd");
    let Reply::Ok { .. } = claim_reply else {
        panic!("expected the direct Claim to validate: {claim_reply:?}");
    };

    // Half A: the registered, idle agent refuses cleanup.
    let (ok, _result, stderr) = run_clean(&estate, &work_id, true);
    assert!(
        !ok,
        "clean should refuse while the idle agent is still live"
    );
    assert!(
        stderr.contains("ActorLive"),
        "expected ActorLive in stderr, got: {stderr}"
    );
    assert!(
        worktree_path.is_dir(),
        "a refused clean must not touch the worktree"
    );

    // Kill the idle agent's own pane/process now, then open a *plain*
    // shell pane cd'd into the same worktree with no agent registered
    // at all — the gap `agent.list` alone cannot see.
    // Kept alive above only so `wirk run`'s own subscription does not
    // itself observe and react to what follows; killed and reaped
    // directly here rather than left for `KillOnDrop` at teardown.
    if let Some(mut run_run) = guard.0.pop() {
        let _ = run_run.kill();
        let _ = run_run.wait();
    }
    let client = session.client();
    let _ = client.close_workspace(wirk_herdr::CloseWorkspace {
        workspace_id: {
            let agents = client.list_agents().expect("agent.list reaches Herdr");
            agents
                .iter()
                .find(|pane| pane.name.as_deref() == Some(run_id.as_str()))
                .map(|pane| pane.workspace_id.clone())
                .expect("the actor's own workspace is still listed")
        },
    });

    let plain_shell = client
        .create_workspace(wirk_herdr::CreateWorkspace {
            cwd: worktree_path.clone(),
            env: Default::default(),
            label: Some("plain-shell".to_string()),
        })
        .expect("creating a plain shell workspace in the worktree");
    // The workspace's own root pane id, read back from a snapshot
    // (`create_workspace` only returns the `WorkspaceInfo` half of the
    // wire reply) — and a bounded poll for the shell to actually start
    // and settle into its cwd, via Herdr's own `pane.process_info`
    // reporting a shell pid, never a timed sleep.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ready = client.snapshot().ok().is_some_and(|snapshot| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.workspace_id == plain_shell.workspace_id)
                .any(|pane| {
                    client
                        .pane_process_info(&pane.pane_id)
                        .is_ok_and(|info| info.shell_pid.is_some())
                })
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the plain shell pane never reported a shell pid"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let (ok, _result, stderr) = run_clean(&estate, &work_id, true);
    assert!(
        !ok,
        "clean should refuse while a plain shell sits in the worktree"
    );
    assert!(
        stderr.contains("ActorLive"),
        "expected ActorLive (plain-shell coverage) in stderr, got: {stderr}"
    );
    assert!(
        worktree_path.is_dir(),
        "a refused clean must not touch the worktree"
    );

    let _ = client.close_workspace(wirk_herdr::CloseWorkspace {
        workspace_id: plain_shell.workspace_id,
    });

    let (ok, result, stderr) = run_clean(&estate, &work_id, false);
    assert!(
        ok,
        "clean should succeed once every pane is closed: {stderr}"
    );
    assert_eq!(result["worktree_removed"], serde_json::json!(true));

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
}

#[test]
fn wirk_work_clean_refuses_ignored_content_and_refuses_uncommitted_content() {
    let scripted = scripted_actor::ScriptedActor::install(&[
        "edit:.gitignore:*.log\n",
        "commit:add gitignore",
        "edit:committed.txt:committed content",
        "commit:add committed.txt",
        "output_claim:report.md:managed output content",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_work_clean_refuses_ignored_content_and_refuses_uncommitted_content",
        &[
            ("PATH", &path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(&estate);

    let (mut run_child, work_id, run_id) = submit_and_run_with_outputs(
        &estate,
        &repo,
        &session,
        &path_env,
        "clean-ignored",
        "commit a gitignore and a file, write the managed output, then claim",
        r#"[{"name":"report.md","required":true}]"#,
    );
    let run_status = run_child.wait().expect("reap wirk run");
    assert!(run_status.success(), "wirk run exit status: {run_status:?}");
    close_run_workspace(&session, &run_id);

    let worktree_path = estate.join("worktrees").join(&work_id);

    // Simulate build residue landing in the checkout after the actor's
    // own turn ended: an ignored file first.
    fs::write(worktree_path.join("debug.log"), b"noise").expect("write ignored file");
    let (ok, _result, stderr) = run_clean(&estate, &work_id, true);
    assert!(!ok, "clean should refuse ignored content");
    assert!(
        stderr.contains("IgnoredContent") && stderr.contains("debug.log"),
        "expected IgnoredContent naming debug.log, got: {stderr}"
    );
    assert!(worktree_path.join("debug.log").is_file(), "nothing removed");
    fs::remove_file(worktree_path.join("debug.log")).expect("remove ignored file");

    // Now an untracked, non-ignored file: git's own dirty gate refuses.
    // Correction pass: a `--dry-run` call must detect this too, before
    // ever claiming eligibility — not only the real call below, which
    // is what the qualified candidate actually checked.
    fs::write(worktree_path.join("extra.txt"), b"uncommitted").expect("write untracked file");
    let (ok, _result, stderr) = run_clean(&estate, &work_id, true);
    assert!(
        !ok,
        "dry-run clean should refuse uncommitted/untracked content"
    );
    assert!(
        stderr.contains("UncommittedWork"),
        "expected UncommittedWork in dry-run stderr, got: {stderr}"
    );
    assert!(
        worktree_path.join("extra.txt").is_file(),
        "a dry-run refusal must not touch the untracked file"
    );
    assert!(
        worktree_path.is_dir(),
        "a dry-run refusal must not touch the worktree"
    );

    let (ok, _result, stderr) = run_clean(&estate, &work_id, false);
    assert!(!ok, "clean should refuse uncommitted/untracked content");
    assert!(
        stderr.contains("UncommittedWork"),
        "expected UncommittedWork in stderr, got: {stderr}"
    );
    assert!(
        worktree_path.join("extra.txt").is_file(),
        "git's own refusal must leave the untracked file in place"
    );

    fs::remove_file(worktree_path.join("extra.txt")).expect("remove untracked file");
    let (ok, result, stderr) = run_clean(&estate, &work_id, false);
    assert!(
        ok,
        "clean should now succeed on the clean checkout: {stderr}"
    );
    assert_eq!(result["worktree_removed"], serde_json::json!(true));

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
}

/// The identity check's own reason to exist (ruling 0203: "do not
/// follow a substituted symlink into another checkout"; QUALIFIED.md's
/// probe finding that a common Git directory cannot tell two Works'
/// checkouts in one repository apart): two real Works, same repo, both
/// cleanly Claimed; Work A's own worktree directory is replaced with a
/// symlink into Work B's real checkout. `clean` on A must refuse
/// (`PathMismatch`), and B's checkout must be completely unaffected.
#[test]
fn wirk_work_clean_refuses_a_symlink_substituted_into_another_works_checkout() {
    let scripted = scripted_actor::ScriptedActor::install(&[
        "edit:committed.txt:committed content",
        "commit:report",
        "output_claim:report.md:managed output content",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_work_clean_refuses_a_symlink_substituted_into_another_works_checkout",
        &[
            ("PATH", &path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(&estate);

    let (mut child_a, work_a, run_a) = submit_and_run_with_outputs(
        &estate,
        &repo,
        &session,
        &path_env,
        "clean-symlink-a",
        "commit a file, write the managed output, then claim (A)",
        r#"[{"name":"report.md","required":true}]"#,
    );
    assert!(child_a.wait().expect("reap wirk run A").success());
    close_run_workspace(&session, &run_a);

    let (mut child_b, work_b, run_b) = submit_and_run_with_outputs(
        &estate,
        &repo,
        &session,
        &path_env,
        "clean-symlink-b",
        "commit a file, write the managed output, then claim (B)",
        r#"[{"name":"report.md","required":true}]"#,
    );
    assert!(child_b.wait().expect("reap wirk run B").success());
    close_run_workspace(&session, &run_b);

    let worktree_a = estate.join("worktrees").join(&work_a);
    let worktree_b = estate.join("worktrees").join(&work_b);
    assert!(worktree_a.is_dir() && worktree_b.is_dir());
    let b_head_before = git(&worktree_b, &["rev-parse", "HEAD"]).stdout;

    // Substitute A's checkout: remove it, then symlink it at B's real
    // directory. Git's own `worktree list` still names A's path as A's
    // own administrative entry (unaffected by what physically sits
    // there); the guard this proves is `verify_worktree_identity`'s own
    // symlink refusal.
    fs::remove_dir_all(&worktree_a).expect("remove A's own directory");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&worktree_b, &worktree_a).expect("symlink A -> B");

    let (ok, _result, stderr) = run_clean(&estate, &work_a, true);
    assert!(!ok, "clean must refuse a symlink substituted checkout");
    assert!(
        stderr.contains("PathMismatch"),
        "expected PathMismatch in stderr, got: {stderr}"
    );

    // B is completely unaffected: still there, still at the same
    // commit, still a valid registered worktree.
    assert!(worktree_b.is_dir(), "B's checkout must be untouched");
    let b_head_after = git(&worktree_b, &["rev-parse", "HEAD"]).stdout;
    assert_eq!(
        b_head_before, b_head_after,
        "B's own HEAD must not move because of A's refused cleanup"
    );
    let listing = worktree_list(&repo);
    assert!(
        listing.contains(&worktree_b.display().to_string()),
        "B must still be a registered worktree: {listing}"
    );

    // Clean up the symlink by hand so `wirkd stop` and the tempdir
    // teardown do not trip over it, then confirm B itself still cleans
    // normally afterward (unaffected by A's refused attempt).
    fs::remove_file(&worktree_a).ok();
    let (ok, result, stderr) = run_clean(&estate, &work_b, false);
    assert!(ok, "B's own clean must succeed normally: {stderr}");
    assert_eq!(result["worktree_removed"], serde_json::json!(true));

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
}

/// The evidence check's own reason to exist (QUALIFIED.md §2 /
/// ruling 0203): a validated Claim whose artifact lives in the
/// checkout (`ArtifactStore::Worktree`, the default and only kind
/// `--artifact NAME=PATH` ever produces) cannot be preserved once the
/// checkout is gone, so `wirk work clean` refuses rather than losing it
/// — proven here against a real Claim, not a constructed journal.
#[test]
fn wirk_work_clean_refuses_checkout_backed_claim_evidence() {
    let scripted = scripted_actor::ScriptedActor::install(&[
        "edit:report.md:checkout-backed evidence",
        "commit:report",
        "claim:--artifact report.md=report.md",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_work_clean_refuses_checkout_backed_claim_evidence",
        &[
            ("PATH", &path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(&estate);

    let (mut run_child, work_id, _run_id) = submit_and_run_with_outputs(
        &estate,
        &repo,
        &session,
        &path_env,
        "clean-checkout-evidence",
        "commit a report, then claim it as a checkout artifact",
        r#"[{"name":"report.md","required":true}]"#,
    );
    let run_status = run_child.wait().expect("reap wirk run");
    assert!(run_status.success(), "wirk run exit status: {run_status:?}");

    let worktree_path = estate.join("worktrees").join(&work_id);
    let (ok, _result, stderr) = run_clean(&estate, &work_id, true);
    assert!(
        !ok,
        "clean must refuse checkout-backed validated Claim evidence"
    );
    assert!(
        stderr.contains("ClaimEvidenceInCheckout") && stderr.contains("report.md"),
        "expected ClaimEvidenceInCheckout naming report.md, got: {stderr}"
    );
    assert!(
        worktree_path.is_dir(),
        "a refused clean must not touch the worktree"
    );

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
}

/// Correction pass: an actual *partial* removal, not the builder's
/// repetition-after-success. The worktree is removed for real, then a
/// real filesystem permission failure (`chmod 0o500` on `.wirk/runtime`,
/// an existing real test facility — no fake, no injected error type)
/// blocks the per-Run runtime-pin removal that follows it in the same
/// call, so the call returns an error with the checkout already gone
/// and the pin directory still there. The journal must not lie about
/// that: no `WorkCleaned` event from the failed call. Restoring the
/// permission and retrying must then reconcile the remainder and
/// journal exactly once, truthfully reporting `worktree_removed: false`
/// (already gone) alongside the pin directory this retry actually
/// removed.
#[test]
fn wirk_work_clean_reconciles_a_real_partial_removal_on_retry() {
    let scripted = scripted_actor::ScriptedActor::install(&[
        "edit:NOTES.md:a harmless committed note",
        "commit:add a note",
        "output_claim:report.md:a throwaway repo for the partial-removal retry",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_work_clean_reconciles_a_real_partial_removal_on_retry",
        &[
            ("PATH", &path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

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
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    wait_for_pointer(&estate);

    let (run_child, work_id, run_id) = submit_and_run_with_outputs(
        &estate,
        &repo,
        &session,
        &path_env,
        "clean-partial-retry",
        "commit a note, write report.md to the managed output area, then claim",
        r#"[{"name":"report.md","required":true}]"#,
    );
    let output = run_child.wait_with_output().expect("reap wirk run");
    assert!(
        output.status.success(),
        "wirk run exit status: {:?}",
        output.status
    );
    close_run_workspace(&session, &run_id);

    let worktree_path = estate.join("worktrees").join(&work_id);
    let runtime_pin_dir = estate.join(".wirk").join("runtime").join(&run_id);
    let runtime_root = estate.join(".wirk").join("runtime");
    assert!(
        runtime_pin_dir.join("bin").join("wirk").is_file(),
        "this Run's pinned wirk binary should exist before cleanup"
    );

    // A real, induced filesystem failure: no write permission on the
    // per-Run pin directories' own parent, so the real
    // `std::fs::remove_dir_all` call the server makes fails with a real
    // `EACCES`, not a fabricated error type.
    let original_mode = fs::metadata(&runtime_root)
        .expect("stat .wirk/runtime")
        .permissions()
        .mode();
    fs::set_permissions(&runtime_root, fs::Permissions::from_mode(0o500))
        .expect("chmod .wirk/runtime read-only");

    let (ok, _result, stderr) = run_clean(&estate, &work_id, false);
    // Restore permissions immediately regardless of outcome so the
    // rest of this test (and its own teardown) is never left blocked
    // by a directory this test itself made unwritable.
    fs::set_permissions(&runtime_root, fs::Permissions::from_mode(original_mode))
        .expect("restore .wirk/runtime permissions");

    assert!(
        !ok,
        "the induced permission failure must surface as a real error, not a silent success"
    );
    assert!(
        stderr.contains("JournalError"),
        "expected the remove_dir_all failure to surface as JournalError, got: {stderr}"
    );
    assert!(
        !worktree_path.exists(),
        "the worktree removal that precedes the failing step must still have taken effect \
         (real partial removal, not an all-or-nothing rollback)"
    );
    assert!(
        runtime_pin_dir.is_dir(),
        "the per-Run pin directory must survive the induced removal failure"
    );

    // Correction (ruling 0221): the failed call must still journal what
    // it actually completed before the failing step — the worktree
    // removal that already happened — rather than journaling nothing at
    // all. A missing WorkCleaned event is not evidence that nothing
    // happened; this event's own `complete: false` says the call itself
    // did not finish, while its `worktree_removed: true` says truthfully
    // what this call did manage before that.
    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    let cleaned_events: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::WorkCleaned { .. }))
        .collect();
    assert_eq!(
        cleaned_events.len(),
        1,
        "the failed call must still journal its own completed effects"
    );
    if let EventKind::WorkCleaned {
        runs,
        worktree_removed,
        runtime_pins_removed,
        complete,
    } = &cleaned_events[0].kind
    {
        assert_eq!(runs, &[RunId(run_id.clone())]);
        assert!(
            *worktree_removed,
            "the worktree removal that preceded the failing step really happened and must be \
             recorded, not lost because a later step of the same call failed"
        );
        assert_eq!(
            runtime_pins_removed,
            &[] as &[RunId],
            "the pin directory removal itself failed on this call, so no run is credited"
        );
        assert!(
            !*complete,
            "this call did not run to its own end and must say so"
        );
    }

    // Public status must not still claim the checkout is present, nor
    // stay silent about the incomplete cleanup, just because no event
    // says `worktree_removed: true` under the old collapsed reading —
    // a World's captured path is historical context, not current proof.
    let status = wirkd_status(&estate, &work_id);
    let run_status = status["runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|r| r["run"]["id"] == run_id))
        .expect("this Run's own status entry");
    assert_eq!(
        run_status["worktree_present"],
        serde_json::json!(false),
        "status must reflect the checkout's actual, current absence"
    );
    assert_eq!(
        status["cleanup"][0]["complete"],
        serde_json::json!(false),
        "status must disclose that the recorded cleanup attempt was incomplete"
    );

    // Ordinary `wirk wirkd status` (no `--json`) must make the same
    // truth visible to a person reading a terminal, not only to a
    // script parsing the wire result (ruling 0224). Before this Work's
    // rendering change, the human surface printed only `work_id state
    // current_waypoint needs_input scope` and stayed silent about the
    // Run's own worktree/pin presence and the recorded cleanup history
    // above — this is the assertion that must fail red before the
    // rendering change and pass green after it.
    let text = wirkd_status_text(&estate, &work_id);
    assert!(
        text.contains("worktree absent") || text.contains("worktree_present false"),
        "ordinary status text must disclose the checkout's actual absence, got:\n{text}"
    );
    assert!(
        text.contains("incomplete") || text.contains("complete false"),
        "ordinary status text must disclose the incomplete cleanup attempt, got:\n{text}"
    );

    // Retry: the worktree is already gone (reconciled as
    // `worktree_removed: false`, truthfully, not re-attempted), and the
    // still-present pin directory is now removable and actually
    // removed.
    let (ok, result, stderr) = run_clean(&estate, &work_id, false);
    assert!(
        ok,
        "the retry must succeed once the real failure is cleared: {stderr}"
    );
    assert_eq!(result["worktree_removed"], serde_json::json!(false));
    assert_eq!(
        result["runtime_pins_removed"],
        serde_json::json!([run_id.clone()])
    );
    assert!(
        !runtime_pin_dir.exists(),
        "the retry must actually remove the pin directory this time"
    );

    let events = journal.replay().expect("journal replays cleanly");
    let cleaned_events: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::WorkCleaned { .. }))
        .collect();
    assert_eq!(
        cleaned_events.len(),
        2,
        "two WorkCleaned events must now exist: the incomplete first call, then the retry that \
         finished the job — together, not either alone, telling the whole truth"
    );
    if let EventKind::WorkCleaned {
        runs,
        worktree_removed,
        runtime_pins_removed,
        complete,
    } = &cleaned_events[1].kind
    {
        assert_eq!(runs, &[RunId(run_id.clone())]);
        assert!(
            !*worktree_removed,
            "the retry's own WorkCleaned event must truthfully report the worktree was already \
             gone, not re-claim credit for the earlier call's removal"
        );
        assert_eq!(runtime_pins_removed, &[RunId(run_id.clone())]);
        assert!(*complete, "the retry ran to its own end");
    }

    let status = wirkd_status(&estate, &work_id);
    let run_status = status["runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|r| r["run"]["id"] == run_id))
        .expect("this Run's own status entry");
    assert_eq!(
        run_status["runtime_pin_present"],
        serde_json::json!(false),
        "status must reflect the pin directory's actual, current absence after the retry"
    );
    assert_eq!(status["cleanup"].as_array().map(Vec::len), Some(2));
    assert_eq!(status["cleanup"][1]["complete"], serde_json::json!(true));

    // After the retry finishes, the text surface must show the pin
    // directory's current absence and both cleanup attempts in order —
    // the incomplete first call and the completed retry, distinguished
    // from each other, not collapsed into one aggregate line.
    let text = wirkd_status_text(&estate, &work_id);
    assert!(
        text.contains("any_run_pin absent") || text.contains("runtime_pin_present false"),
        "ordinary status text must reflect the pin directory's current absence, got:\n{text}"
    );
    let clean_lines: Vec<&str> = text.lines().filter(|l| l.contains("clean ")).collect();
    assert_eq!(
        clean_lines.len(),
        2,
        "ordinary status text must show both cleanup attempts, in order, got:\n{text}"
    );
    assert!(
        clean_lines[0].contains("complete false") && clean_lines[1].contains("complete true"),
        "the incomplete first attempt and the completed retry must print in their recorded \
         order, not reordered or collapsed, got: {clean_lines:?}"
    );

    // Late-component state: the retry above removed this Run's runtime
    // pin directory (the component the old `runtime_pin` label named),
    // but the wire boolean is true when *any* of the three pin
    // components exists (`run_pin_dirs`: runtime, claude, opencode) —
    // so a leftover opencode pin directory, created here to stand in
    // for one the real actor left behind, must still read `present`
    // even though the runtime component itself is now gone. This is
    // exactly the state the old `runtime_pin` label misnamed: reading
    // it as "the runtime executable is still pinned" would be wrong.
    let opencode_dir = wirk_herdr::claim_hook::run_dir(&estate.display().to_string(), &run_id);
    fs::create_dir_all(&opencode_dir).expect("create a stand-in opencode pin directory");
    assert!(
        !runtime_pin_dir.exists(),
        "the runtime pin component itself must still be gone at this point"
    );
    let status = wirkd_status(&estate, &work_id);
    let run_status = status["runs"]
        .as_array()
        .and_then(|runs| runs.iter().find(|r| r["run"]["id"] == run_id))
        .expect("this Run's own status entry");
    assert_eq!(
        run_status["runtime_pin_present"],
        serde_json::json!(true),
        "the ANY-component boolean must read present once any pin component exists, even with \
         the runtime component itself gone"
    );
    let text = wirkd_status_text(&estate, &work_id);
    assert!(
        text.contains("any_run_pin present"),
        "the human label must say a pin component remains, not the removed runtime-specific \
         wording, got:\n{text}"
    );

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
}
