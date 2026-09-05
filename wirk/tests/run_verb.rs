//! Live integration test for `wirk run` (item 4, W3, converted to a
//! live Herdr session per 0040 D127 — was an "ungated... against a
//! scripted fake Herdr socket server", now a throwaway named session).
//! Drives the real built binary end to end: a real `wirk wirkd`, a
//! real `wirk work submit --kind actor`, a real `wirk run
//! --actor-kind opencode` against `LiveHerdrSession`'s own socket.
//! `opencode` (cheap, local) is the actor kind, per the item's standing
//! brief. No sleeps as waits (issue 359): every synchronization point
//! is a bounded poll or read, never a tuned delay. `wirkd` and `wirk
//! run` are both real child processes; `KillOnDrop` guards both for the
//! whole test body so a failed assertion still leaves no process
//! behind (ruling 0030); `LiveHerdrSession`'s own `Drop` tears down the
//! session.
//!
//! This is also the live twin item 4's own `orient/child.md` §5-class
//! defect surfaced (W1 tried step, `RESULT.md`): once the Claim lands
//! in wirkd, `wirk run`'s Herdr subscription can legitimately go quiet
//! (the agent's own pane produces no further events) — historically a
//! live server's read timeout then surfaced as a transport error `wirk
//! run` treated as fatal. Fix 2 (ruling 0044) removed that timeout
//! entirely: the subscription just blocks. This test asserts exit
//! status 0 directly.
//!
//! W2/fix 2: this is also the live twin for `wirk-herdr/tests/
//! run_loop.rs::
//! claim_recorded_on_the_watch_stream_stops_the_loop_with_no_status_call`,
//! which stays fake-backed for wirkd's own side — the crate boundary
//! (0001 D7) means `wirk-herdr`'s own tests cannot reach the real
//! `WirkdRunLoopApi` (`wirk/src/executor.rs`) or spawn a real `wirk
//! wirkd`. Here the Claim below is filed directly over the real wirkd
//! socket, bypassing the agent entirely, so the only way this process
//! can exit 0 is `RunLoop::drive`'s real `watch` stream carrying the
//! real `ClaimRecorded` fact and stopping the loop — exactly the
//! behaviour the fake-backed test pins, proven here against a real
//! wirkd whose journal received a real Claim.

#[path = "../../wirk-herdr/tests/support/live_herdr.rs"]
mod live_herdr;
#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wirkd::{ClaimPayload, Reply, Request, WirkdPointer};

use wirk_core::{ClaimKind, EventKind, ExecutionTriple, Journal, RunId, WorkId};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// Bounded poll (issue 359) for `<estate>/.wirk/wirkd.json` to exist —
/// written only after the listener is already bound (`orient/
/// transport.md` §3). Duplicated from `wirkd_process.rs` (R6: a few
/// lines, no shared-utility module warranted for it).
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

/// `git init` plus one commit, so `--base HEAD` resolves to a real SHA
/// (`session.md` §2's throwaway-repo mechanics, git-identity half only —
/// this test's own repo, torn down by the tempdir).
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

fn git_rev_parse(dir: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", rev])
        .output()
        .expect("git rev-parse runs");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Writes a single-Waypoint Actor Route (`smoke_waypoint`'s old shape,
/// now authored per call so each test's own distinctive intent lands
/// in the file, p2-route-files W2) as `<estate>/routes/smoke.json`,
/// then `wirk work submit --estate <estate> --route smoke --kind actor
/// --repo-path <repo> --base HEAD` (no `--intent`, removed), parsing
/// its `work_id <id> run_id <id> waypoint <id>` stdout line (same parse
/// `wirkd_process.rs`'s `submit` uses, R6 duplicate — the two tests
/// submit different World kinds).
fn submit_actor(estate: &Path, repo: &Path, intent: &str) -> (String, String, String) {
    let route_json = format!(
        r#"{{"id":"smoke","waypoints":[{{"id":"smoke/wp-1","kind":"Actor","intent":{intent:?},"declared_outputs":[{{"name":"report.md","required":true}}],"boundary":["**"]}}]}}"#
    );
    route_fixture::write_route(estate, "smoke", &route_json);

    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--route", "smoke", "--kind", "actor", "--repo-path"])
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

/// Bounded poll (issue 359) of a Work's journal for a predicate over its
/// replayed events.
fn wait_for_event(estate: &Path, work_id: &str, mut matches: impl FnMut(&EventKind) -> bool) {
    // 90s (widened from 60s): back-to-back live opencode sessions in
    // one suite run occasionally see slower model startup on this box
    // (a test's own termination bound, never a product one — the
    // owner's ruling of 2026-09-02 §3).
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

/// Guards every child process spawned this test for its whole body: a
/// failed assertion still kills and reaps each one (ruling 0030 — "no
/// wirkd... survives the run that started it"), the same discipline
/// `wirkd_process.rs`'s own `KillOnDrop` uses, widened to hold more
/// than one child.
struct KillOnDrop(Vec<std::process::Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn wirk_run_drives_one_actor_run_to_claimed() {
    let Some(session) =
        live_herdr::LiveHerdrSession::start("wirk_run_drives_one_actor_run_to_claimed")
    else {
        return;
    };
    // Held for the whole test body: this test starts a real opencode
    // agent against the one local model endpoint (live_herdr.rs).
    let _live_model_guard = live_herdr::live_model_lock();

    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();
    let repo_dir = tempfile::tempdir().expect("repo tempdir");
    let repo = repo_dir.path().to_path_buf();
    init_repo(&repo);
    let base_sha = git_rev_parse(&repo, "HEAD");

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

    let (work_id, run_id, _waypoint) = submit_actor(&estate, &repo, "write report.md, then claim");

    guard.0.push(
        Command::new(wirk_bin())
            .args(["run", "--estate"])
            .arg(&estate)
            .args(["--work", &work_id, "--session", session.name()])
            .args(["--herdr-socket"])
            .arg(session.socket_path())
            .args(["--actor-kind", "opencode"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirk run"),
    );

    // Bounded poll (issue 359) for `RunLaunched`: `wirk run`'s own
    // worktree + Herdr launch has happened, so this test's own write
    // and Claim (below) land after it, never racing a Run that has not
    // opened yet.
    wait_for_event(&estate, &work_id, |kind| {
        matches!(kind, EventKind::RunLaunched { .. })
    });

    // The worktree convention `wirk/src/executor.rs` documents:
    // `<estate>/worktrees/<work_id>`.
    let worktree_path = estate.join("worktrees").join(&work_id);
    fs::write(
        worktree_path.join("report.md"),
        b"a throwaway repo for the tried step",
    )
    .expect("write report.md into the worktree");

    let claim_reply = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.clone()),
                run_id: RunId(run_id.clone()),
            },
            kind: ClaimKind::Done,
            artifacts: BTreeMap::from([("report.md".to_string(), "report.md".to_string())]),
        }),
    )
    .expect("claim call reaches wirkd");
    match claim_reply {
        Reply::Ok { result, .. } => {
            assert_eq!(result["verdict"], "Validated", "claim result: {result:?}");
        }
        Reply::Err { error, .. } => {
            panic!(
                "claim unexpectedly refused: {} {}",
                error.code, error.message
            )
        }
    }

    // `wirk run` exits 0 (Claimed) — the last of `guard.0`. Bounded by
    // the test harness's own timeout (no fixed wait here): a real
    // opencode agent's own pane may keep producing status events for a
    // while after the Claim already landed by the path above, and
    // `wirk run`'s loop only re-checks wirkd's status after each
    // observed event or once its subscription goes quiet (this item's
    // disclosed fix in `wirk-herdr/src/run_loop.rs`).
    let run_status = guard
        .0
        .last_mut()
        .expect("wirk run child is in guard")
        .wait()
        .expect("reap wirk run");
    assert!(run_status.success(), "wirk run exit status: {run_status:?}");

    // The journal: WorktreeCreated with the commit's SHA, RunLaunched
    // with actor_kind Opencode, ClaimRecorded{Done, Validated}.
    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");

    let worktree_created = events.iter().find_map(|event| match &event.kind {
        EventKind::WorktreeCreated {
            repo,
            base_sha: sha,
        } => Some((repo.clone(), sha.clone())),
        _ => None,
    });
    assert_eq!(
        worktree_created,
        Some((repo.display().to_string(), base_sha)),
        "expected WorktreeCreated with the commit's SHA"
    );

    let run_launched_opencode = events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RunLaunched { run, actor_kind } if run.0 == run_id && matches!(actor_kind, wirk_core::ActorKind::Opencode)
        )
    });
    assert!(
        run_launched_opencode,
        "expected RunLaunched{{actor_kind: Opencode}} for {run_id}"
    );

    let claimed = events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::ClaimRecorded {
                claim_kind: ClaimKind::Done,
                verdict: wirk_core::ClaimVerdict::Validated,
                ..
            }
        )
    });
    assert!(claimed, "expected ClaimRecorded{{Done, Validated}}");

    // Teardown: stop wirkd, then let `guard`'s Drop reap both children;
    // `LiveHerdrSession`'s own `Drop` tears down the session.
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
}

/// The dogfood run 1 defect (0036 D113, since superseded; `knowledge/
/// evidence/p2-dogfood-2026-09-04/{verdict.md,03-run-wp1.log}`): `wirk
/// run`'s subscription used to read a `WouldBlock`/`TimedOut` off a
/// quiet pane as a transport error and fold it straight to `RunFailed`,
/// killing the Run before the actor ever reached `wirk claim`. Fix 2
/// (ruling 0044) removed the read timeout that produced that error
/// entirely: `SocketClient` sets none anywhere any more, so this test
/// now pins the stronger, simpler fact directly — a pane that is quiet
/// for longer than any former timeout (`QUIET_POLL`, well past the old
/// 5s/30s bounds) never produces a `RunFailed`, because there is no
/// timeout left that could ever fold to one.
///
/// It does **not** stay open unclaimed for the whole window any more,
/// and does not try to force it to: item C's own no-progress check
/// (D133) is a second, separate fix this same ruling landed, and a
/// pane that is told to "sit still" and genuinely does is, correctly,
/// what that check exists to catch — `wirk run` prompts it once, sees
/// no progress on the next Idle, and stops `NeedsInput` (exit 4), well
/// inside `QUIET_POLL`. That is not the defect this test pins (a
/// `RunFailed` from a stale read timeout never happens either way);
/// asserting `RunFailed` never lands, and that the process ends with
/// one of its two legitimate non-crash outcomes, is what "survives a
/// quiet pane" now means.
const QUIET_POLL: Duration = Duration::from_secs(40);

#[test]
fn wirk_run_survives_a_quiet_pane_past_the_subscription_timeout() {
    let Some(session) = live_herdr::LiveHerdrSession::start(
        "wirk_run_survives_a_quiet_pane_past_the_subscription_timeout",
    ) else {
        return;
    };
    // Held for the whole test body: this test starts a real opencode
    // agent against the one local model endpoint (live_herdr.rs).
    let _live_model_guard = live_herdr::live_model_lock();

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

    let (work_id, run_id, _waypoint) =
        submit_actor(&estate, &repo, "sit still; do not write anything yet");

    guard.0.push(
        Command::new(wirk_bin())
            .args(["run", "--estate"])
            .arg(&estate)
            .args(["--work", &work_id, "--session", session.name()])
            .args(["--herdr-socket"])
            .arg(session.socket_path())
            .args(["--actor-kind", "opencode"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirk run"),
    );

    wait_for_event(&estate, &work_id, |kind| {
        matches!(kind, EventKind::RunLaunched { .. })
    });

    // Bounded poll (issue 359) confirming no *bogus* RunFailed lands
    // during QUIET_POLL — the defect this test pins: a stale read
    // timeout on a quiet pane must not fail the Run. `wirk run` itself
    // may exit on its own before the deadline (item C's no-progress
    // check, legitimately, this test's own doc comment) — P2.3 W1
    // journals that legitimate case as `RunFailed{cause.status:
    // Some("stuck")}` now (states.md §1), so only a RunFailed whose
    // `cause.status` is *not* `"stuck"` is the regression this poll
    // still watches for.
    let mut run_child = guard.0.pop().expect("wirk run child is in guard");
    let deadline = Instant::now() + QUIET_POLL;
    loop {
        if let Ok(journal) = Journal::open(estate.join("works").join(&work_id))
            && let Ok(events) = journal.replay()
            && let Some(failed) = events.iter().find(|e| {
                matches!(
                    &e.kind,
                    EventKind::RunFailed { cause } if cause.status.as_deref() != Some("stuck")
                )
            })
        {
            panic!("RunFailed landed during the quiet window: {failed:?}");
        }
        if let Ok(Some(_)) = run_child.try_wait() {
            break; // wirk run already reached its own terminal outcome
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // If `wirk run` is still going (it has not yet judged the pane
    // stuck), file the Claim externally, same shape as the sibling
    // test — either way the process must now reach a terminal exit on
    // its own, `Claimed` (0) or the no-progress `NeedsInput` (4), never
    // a crash.
    if run_child.try_wait().ok().flatten().is_none() {
        let worktree_path = estate.join("worktrees").join(&work_id);
        fs::write(
            worktree_path.join("report.md"),
            b"a throwaway repo for the quiet-pane test",
        )
        .expect("write report.md into the worktree");

        let claim_reply = wirkd::client::call(
            &pointer.socket,
            &Request::claim(ClaimPayload {
                triple: ExecutionTriple {
                    estate_root: estate.display().to_string(),
                    work_id: WorkId(work_id.clone()),
                    run_id: RunId(run_id.clone()),
                },
                kind: ClaimKind::Done,
                artifacts: BTreeMap::from([("report.md".to_string(), "report.md".to_string())]),
            }),
        )
        .expect("claim call reaches wirkd");
        // A `TripleMismatch`/refusal here is tolerated: `wirk run` may
        // have reached `NeedsInput` and exited between the `try_wait`
        // check above and this call landing — a race this test does
        // not need to close, since either outcome (a validated Claim,
        // or a refusal because the Run already ended) is consistent
        // with "no RunFailed, no crash".
        let _ = claim_reply;
    }

    let run_status = run_child.wait().expect("reap wirk run");
    let mut run_stderr = String::new();
    if let Some(mut stderr) = run_child.stderr.take() {
        use std::io::Read;
        let _ = stderr.read_to_string(&mut run_stderr);
    }
    guard.0.push(run_child);

    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    // P2.3 W1: a `RunFailed{cause.status: Some("stuck")}` is the
    // legitimate no-progress surfacing (exit 4, `NeedsInput`), not the
    // stale-read-timeout defect this test pins — only a differently
    // caused RunFailed is still a failure here.
    let bogus_run_failed = events.iter().find(|e| {
        matches!(
            &e.kind,
            EventKind::RunFailed { cause } if cause.status.as_deref() != Some("stuck")
        )
    });
    assert!(
        matches!(run_status.code(), Some(0) | Some(4)),
        "wirk run exit status: {run_status:?} (0 Claimed or 4 NeedsInput expected); stderr: \
         {run_stderr:?}; journal RunFailed: {bogus_run_failed:?}"
    );
    assert!(
        bogus_run_failed.is_none(),
        "expected no bogus RunFailed, found {bogus_run_failed:?}"
    );

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
}

/// Item C (D133), live end to end: a real session, a real wirkd, `wirk
/// run` against a real, cheap opencode agent whose first-prompt intent
/// it finishes in one turn without claiming ("reply with the word
/// ready and stop" — a plain instruction competing with `compose_
/// first_prompt`'s own appended claim instructions), so the pane goes
/// Idle unclaimed. Asserts, from the Herdr side (`agent.wait`, the
/// server's own block-until-this-status primitive, targeted at the
/// agent by name — the same name `HerdrExecutor::start_actor_agent`
/// gives it, `run.id.0`), that `wirk run` prompted it again once it
/// went Idle: the agent must be observed returning to `Working` after
/// its first `Idle` — a transition that can only be `wirk run`'s own
/// continuation prompt reaching the pane, since nothing else in this
/// test ever sends it input, and `PromptGate` releases only on a real
/// `working` status (0017 D56). (The brief's own "the pane's screen
/// shows the continuation text" is this test's wire-level equivalent —
/// reading the pane's literal screen content needs a `pane.read`/
/// `agent.read` wire verb this item's allow-list does not add, named
/// here rather than silently substituted.) The Claim is then filed over
/// the wirkd socket (as the sibling tests do), and `wirk run` must exit
/// 0 with no `RunFailed` journaled.
#[test]
fn wirk_run_prompts_an_idle_unclaimed_pane_again_then_claims() {
    let Some(session) = live_herdr::LiveHerdrSession::start(
        "wirk_run_prompts_an_idle_unclaimed_pane_again_then_claims",
    ) else {
        return;
    };
    // Held for the whole test body: this test starts a real opencode
    // agent against the one local model endpoint (live_herdr.rs).
    let _live_model_guard = live_herdr::live_model_lock();

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

    let (work_id, run_id, _waypoint) =
        submit_actor(&estate, &repo, "reply with the word ready and stop");

    let mut run_child = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--session", session.name()])
        .args(["--herdr-socket"])
        .arg(session.socket_path())
        .args(["--actor-kind", "opencode"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wirk run");

    // P2.3 W3: read `wirk run`'s own stdout on its own thread as it is
    // produced (never after `wait()` — the pipe would fill and this
    // long-lived driver would deadlock before Claimed). This is the
    // live twin's own way of "capturing the child's stdout" (BUILD.md's
    // choice, over injecting a sink as `wirk-herdr/tests/run_loop.rs`'s
    // fake-backed test does): a real process, its real pipe, read
    // concurrently — exactly as `stderr` is already read elsewhere in
    // this file, but after the child exits there; here read live
    // instead, since this test's own driver keeps running well past
    // the first prompt.
    let run_stdout = run_child.stdout.take().expect("wirk run stdout piped");
    let run_stdout_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let run_stdout_reader = {
        let lines = Arc::clone(&run_stdout_lines);
        std::thread::spawn(move || {
            for line in BufReader::new(run_stdout).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        })
    };

    guard.0.push(run_child);

    wait_for_event(&estate, &work_id, |kind| {
        matches!(kind, EventKind::RunLaunched { .. })
    });

    let client = session.client();

    // First turn end: the agent finished its one turn. P2.3 W5
    // (build-brief.md §9): Herdr's own `status_name` reports this as
    // `Done`, not `Idle`, whenever the pane has not been viewed since —
    // every pane `wirk run` drives, since it drives headless — so this
    // accepts either rather than pinning `Idle` alone (the rerun
    // observed `Done` here live: `knowledge/evidence/
    // p2-retry-escalation-2026-09-04/rerun/03-stuck.log`).
    wait_agent_status_any(
        &client,
        &run_id,
        &[wirk_herdr::AgentStatus::Idle, wirk_herdr::AgentStatus::Done],
        "the agent's first turn end (Idle or Done)",
    );

    // A return to Working after that Idle: `wirk run`'s own
    // continuation prompt is the only thing in this test that ever
    // sends the pane more input (this test's own doc comment).
    wait_agent_status(
        &client,
        &run_id,
        wirk_herdr::AgentStatus::Working,
        "the agent working again after wirk run's continuation prompt",
    );

    // The Working transition just observed is itself the proof this
    // test exists to pin: `wirk run` prompted the pane again after its
    // first Idle. The Claim is filed now, not after a second Idle — the
    // continuation prompt's own reply ("ready", again) makes no
    // worktree progress by design (the instruction is a no-op), so
    // waiting for a second Idle would race item C's own no-progress
    // check (`RunLoop::observe_herdr`), which would legitimately (and
    // separately from what this test is about) judge that second Idle
    // stuck and stop the loop `NeedsInput` before this test ever got to
    // file the Claim.
    let worktree_path = estate.join("worktrees").join(&work_id);
    fs::write(
        worktree_path.join("report.md"),
        b"filed by the test, proving the pane was still unclaimed and prompted again",
    )
    .expect("write report.md into the worktree");

    let claim_reply = wirkd::client::call(
        &pointer.socket,
        &Request::claim(ClaimPayload {
            triple: ExecutionTriple {
                estate_root: estate.display().to_string(),
                work_id: WorkId(work_id.clone()),
                run_id: RunId(run_id.clone()),
            },
            kind: ClaimKind::Done,
            artifacts: BTreeMap::from([("report.md".to_string(), "report.md".to_string())]),
        }),
    )
    .expect("claim call reaches wirkd");
    match claim_reply {
        Reply::Ok { result, .. } => {
            assert_eq!(result["verdict"], "Validated", "claim result: {result:?}");
        }
        Reply::Err { error, .. } => {
            panic!(
                "claim unexpectedly refused: {} {}",
                error.code, error.message
            )
        }
    }

    let run_status = guard
        .0
        .last_mut()
        .expect("wirk run child is in guard")
        .wait()
        .expect("reap wirk run");
    assert!(run_status.success(), "wirk run exit status: {run_status:?}");

    run_stdout_reader.join().expect("stdout reader thread");
    let stdout_lines = run_stdout_lines.lock().unwrap().clone();
    let prompt_lines: Vec<&String> = stdout_lines
        .iter()
        .filter(|line| line.starts_with("prompt:"))
        .collect();
    // P2.3 W3: this test's own doc comment already pins that `wirk run`
    // prompts the pane exactly once (the Working transition observed
    // above) before the Claim is filed — the driver's stdout must carry
    // that one prompt line, naming the Idle it answered and the first
    // words of what was sent.
    assert_eq!(
        prompt_lines.len(),
        1,
        "wirk run's stdout must carry exactly one prompt line: {stdout_lines:?}"
    );
    // P2.3 W5: the prompt line names whichever turn-ended status it
    // actually answered (`Idle` or `Done` — build-brief.md §9), not a
    // hardcoded "Idle", since a headless pane reports `Done` live.
    assert!(
        (prompt_lines[0].contains("Idle answered") || prompt_lines[0].contains("Done answered"))
            && prompt_lines[0].contains("sending:"),
        "the prompt line must name the turn-ended status it answered (Idle or Done) and the \
         prompt text sent: {:?}",
        prompt_lines[0]
    );

    // P2.3 W5 (build-brief.md §9, second gap: the rerun's driver exited
    // with no printed line naming its outcome, cause unobserved). This
    // run claims, so the outcome line is the plain `Claimed` `run_command`
    // already prints on that exit path — asserted explicitly here as
    // this test's own "the driver's stdout carries a final line naming
    // its outcome" check.
    assert!(
        stdout_lines.iter().any(|line| line == "Claimed"),
        "wirk run's stdout must carry a final line naming its outcome: {stdout_lines:?}"
    );

    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    let run_failed = events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::RunFailed { .. }));
    assert!(
        run_failed.is_none(),
        "expected no RunFailed, found {run_failed:?}"
    );

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
}

/// P2.3 W6 (build-brief.md §10, rerun2's own correction, ruling 0044):
/// live proof that the progress baseline is taken only after a
/// *continuation* prompt, never after the very first (the intent). The
/// stall intent below is rerun2's own, verbatim
/// (`knowledge/evidence/p2-retry-escalation-2026-09-04/rerun2/
/// journal-B.ndjson`): the actor replies once with the single word
/// "waiting" and does nothing else, however many times it is prompted.
/// Expected sequence (`wirk-herdr/src/run_loop.rs` module doc): the
/// intent prompt (turn end 1, no baseline taken -- it is the task, not
/// a continuation), the actor's first reply ends its turn (turn end 2;
/// no baseline existed yet, so this earns an unconditional
/// *continuation* prompt, and the baseline is taken now), the actor's
/// second identical reply ends its turn again with the worktree still
/// untouched (turn end 3) -- a baseline now exists, unchanged, so this
/// is the actor judged stuck. Before this wave's fix, the loop declared
/// stuck already at turn end 2 -- never having sent a continuation at
/// all -- exactly the rerun2 evidence (`driver.log`: stuck 22s after
/// launch, on the actor's very first turn end, one prompt total).
#[test]
fn wirk_run_stuck_after_the_first_continuation_exits_4() {
    let Some(session) =
        live_herdr::LiveHerdrSession::start("wirk_run_stuck_after_the_first_continuation_exits_4")
    else {
        return;
    };
    // Held for the whole test body: this test starts a real opencode
    // agent against the one local model endpoint (live_herdr.rs).
    let _live_model_guard = live_herdr::live_model_lock();

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
    wait_for_pointer(&estate); // wirkd is up; no direct socket call needed in this test

    let (work_id, _run_id, _waypoint) = submit_actor(
        &estate,
        &repo,
        "Reply with the single word waiting and then stop. Do not read or edit any file, do not \
         run any command, do not run wirk claim, do not ask a question. Whenever you are \
         prompted again, reply with the single word waiting and stop.",
    );

    let mut run_child = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--session", session.name()])
        .args(["--herdr-socket"])
        .arg(session.socket_path())
        .args(["--actor-kind", "opencode"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wirk run");

    // Same live-capture shape as `wirk_run_prompts_an_idle_unclaimed_
    // pane_again_then_claims`: read on its own thread as the lines are
    // produced, never after `wait()`.
    let run_stdout = run_child.stdout.take().expect("wirk run stdout piped");
    let run_stdout_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let run_stdout_reader = {
        let lines = Arc::clone(&run_stdout_lines);
        std::thread::spawn(move || {
            for line in BufReader::new(run_stdout).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        })
    };

    wait_for_event(&estate, &work_id, |kind| {
        matches!(kind, EventKind::RunLaunched { .. })
    });

    // The test's own wait is a bounded poll on the child's exit (issue
    // 359's own shape; 0044 D134: a termination bound is reported as
    // "never observed", never a verdict about the agent) -- rerun2's own
    // evidence put the whole sequence at ~22s, so 120s is headroom, not
    // a product timeout.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if run_child.try_wait().ok().flatten().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never observed: wirk run exiting on its own; stdout so far {:?}",
            run_stdout_lines.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let run_status = run_child.wait().expect("reap wirk run");
    run_stdout_reader.join().expect("stdout reader thread");
    guard.0.push(run_child);

    let stdout_lines = run_stdout_lines.lock().unwrap().clone();
    let prompt_lines: Vec<&String> = stdout_lines
        .iter()
        .filter(|line| line.starts_with("prompt:"))
        .collect();
    let continuation_count = prompt_lines
        .iter()
        .filter(|line| line.contains("first continuation"))
        .count();
    assert_eq!(
        continuation_count, 1,
        "exactly one continuation prompt: the intent itself never earns a baseline (W6): \
         {stdout_lines:?}"
    );

    let continuation_index = stdout_lines
        .iter()
        .position(|l| l.starts_with("prompt:") && l.contains("first continuation"))
        .expect("continuation prompt line present");
    let needs_input_index = stdout_lines
        .iter()
        .position(|l| l == "NeedsInput")
        .unwrap_or_else(|| panic!("no NeedsInput outcome line in stdout: {stdout_lines:?}"));
    let stuck_index = stdout_lines
        .iter()
        .position(|l| l.contains("stuck:"))
        .unwrap_or_else(|| panic!("no stuck observation line in stdout: {stdout_lines:?}"));
    assert!(
        continuation_index < needs_input_index && needs_input_index < stuck_index,
        "expected order in the driver's stdout: the continuation prompt, then the NeedsInput \
         outcome, then the stuck detail naming what was observed (rerun2's own driver.log \
         order): {stdout_lines:?}"
    );

    assert_eq!(
        run_status.code(),
        Some(4),
        "wirk run must exit 4 (NeedsInput) on a stuck actor: {run_status:?}, stdout \
         {stdout_lines:?}"
    );

    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    let stuck_failed = events.iter().find(|e| {
        matches!(
            &e.kind,
            EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck")
        )
    });
    assert!(
        stuck_failed.is_some(),
        "expected a journaled RunFailed{{status: stuck}}: {events:?}"
    );

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
}

/// Bounded retry (a test's own termination bound, never a product one —
/// the owner's ruling of 2026-09-02 §3) around `agent.wait`, the
/// server's own block-until-this-status primitive: a single call can
/// itself return `agent_not_ready`/refuse transiently while Herdr's own
/// registration settles (the same live finding `wirk-herdr/tests/
/// run_loop.rs::wait_agent_named` names), so this retries the whole
/// call rather than treating one failure as the status never arriving.
fn wait_agent_status(
    client: &wirk_herdr::SocketClient,
    agent_name: &str,
    status: wirk_herdr::AgentStatus,
    what: &str,
) {
    use wirk_herdr::HerdrClient;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if client.wait_agent(agent_name, status, 5_000).is_ok() {
            return;
        }
        assert!(Instant::now() < deadline, "never observed: {what}");
    }
}

/// P2.3 W5 (build-brief.md §9): the same bounded poll as
/// `wait_agent_status`, widened to accept any of `statuses` — Herdr's
/// own `agent.wait` verb (`HerdrClient::wait_agent`) takes a single
/// target status, so this polls `get_agent` directly instead (a plain
/// point-in-time read, R2: already on the trait, used elsewhere in this
/// crate) rather than racing several blocking waits against one
/// deadline.
fn wait_agent_status_any(
    client: &wirk_herdr::SocketClient,
    agent_name: &str,
    statuses: &[wirk_herdr::AgentStatus],
    what: &str,
) {
    use wirk_herdr::HerdrClient;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Ok(pane) = client.get_agent(agent_name)
            && statuses.contains(&pane.agent_status)
        {
            return;
        }
        assert!(Instant::now() < deadline, "never observed: {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}
