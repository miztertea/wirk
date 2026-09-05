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
#[path = "../../wirk-herdr/tests/support/scripted_actor.rs"]
mod scripted_actor;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wirkd::{ClaimPayload, Reply, Request, StatusPayload, WirkdPointer};

use wirk_core::{ClaimKind, EventKind, ExecutionTriple, Journal, RunId, WorkId};

fn wirk_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wirk")
}

/// `PATH` for a scripted-actor session: the scripted actor's own bin
/// directory first (so Herdr's `agent.start{kind:"opencode"}` finds it
/// ahead of any real `opencode`), then the built `wirk` binary's own
/// directory (so a `claim:` script step's bare `wirk claim` resolves,
/// 0050 D151), then whatever this test process's own `PATH` already
/// was.
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
    submit_actor_named(estate, repo, intent, "smoke")
}

/// `submit_actor`, parameterized by Route name/id (P2.5 W4): two Works
/// submitted against the *same* estate need distinct `routes/<name>.json`
/// files (`resolve_route_path` reads by name, `route_fixture::write_route`
/// would otherwise overwrite one Work's Route with the other's) and
/// distinct Waypoint ids, so their journals stay tellable apart by
/// waypoint as well as by `work_id`/`run_id`.
fn submit_actor_named(
    estate: &Path,
    repo: &Path,
    intent: &str,
    name: &str,
) -> (String, String, String) {
    let route_json = format!(
        r#"{{"id":{name:?},"waypoints":[{{"id":"{name}/wp-1","kind":"Actor","intent":{intent:?},"declared_outputs":[{{"name":"report.md","required":true}}],"boundary":["**"]}}]}}"#
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
    // P2.5 W1 (ruling 0049 D148): a scripted actor, not a real model,
    // drives this live pane deterministically. Three turns: the first
    // ends idle with no worktree change (the intent alone earns no
    // baseline, W6), the loop's first continuation earns the file
    // write, and the loop's next continuation earns the actor's own
    // `wirk claim` — the same claim path a real actor would take,
    // proven here without a model's cooperation.
    let scripted = scripted_actor::ScriptedActor::install(&[
        "idle",
        "edit:report.md:a throwaway repo for the tried step",
        "claim:--artifact report.md=report.md",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_drives_one_actor_run_to_claimed",
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
            // P2.5 W2 (0050 D151): `HerdrExecutor::actor_pane` now sets
            // the actor pane's own `PATH` from *this* process's
            // inherited `PATH` (prepending the running `wirk`
            // executable's directory), not the herdr session's — so
            // `wirk run` needs the scripted actor's directory on its
            // own `PATH` too, the same value the session above got via
            // `start_with_env`, or the pane's `PATH` would fall back to
            // this test binary's ambient one and resolve the real
            // `opencode` instead of the scripted actor.
            .env("PATH", &path_env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirk run"),
    );

    // Bounded poll (issue 359) for `RunLaunched`: `wirk run`'s own
    // worktree + Herdr launch has happened.
    wait_for_event(&estate, &work_id, |kind| {
        matches!(kind, EventKind::RunLaunched { .. })
    });

    // `wirk run` exits 0 (Claimed) — the last of `guard.0`. Bounded by
    // the test harness's own timeout (no fixed wait here): the scripted
    // actor's own `claim:` step files `wirk claim` itself once its
    // three-turn script runs out, and `wirk run`'s loop only re-checks
    // wirkd's status after each observed event (this item's disclosed
    // fix in `wirk-herdr/src/run_loop.rs`).
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
            EventKind::RunLaunched { run, actor_kind } if run.0 == run_id && *actor_kind == wirk_core::ActorKind::opencode()
        )
    });
    assert!(
        run_launched_opencode,
        "expected RunLaunched{{actor_kind: Opencode}} for {run_id}"
    );

    // 0056 D164's round trip: the kind given at `wirk run --actor-kind`
    // is carried, as given, all the way to `wirk work status`'s own
    // reply — not just the journal file read directly above. `Run`'s
    // `Serialize` (wirk-core/src/lib.rs) puts `kind` on the wire
    // unconditionally, so the raw `status` reply's `runs[].run.kind`
    // already carries it; asserted here against the live wirkd this
    // test already stood up, the same socket call `reserved_world`
    // (`wirk/tests/boundary_claim.rs`) makes for `world`.
    let status_reply = wirkd::client::call(
        &pointer.socket,
        &Request::status(StatusPayload {
            work_id: WorkId(work_id.clone()),
        }),
    )
    .expect("status call reaches wirkd");
    let Reply::Ok { result, .. } = status_reply else {
        panic!("status unexpectedly refused: {status_reply:?}");
    };
    let runs = result["runs"].as_array().expect("runs is an array");
    let status_kind = runs
        .iter()
        .find(|entry| entry["run"]["id"] == run_id)
        .and_then(|entry| entry["run"]["kind"].as_str())
        .expect("the launched run's kind is on the status reply");
    assert_eq!(
        status_kind, "opencode",
        "wirk work status should carry the actor kind through as given"
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
    // P2.5 W1 (ruling 0049 D148): an empty script is a deliberate,
    // guaranteed "idle forever" pane (module doc, scripted_actor.sh) —
    // it reports its boot idle and then never reports again, no matter
    // how many prompts `wirk run` sends, genuinely quiet rather than
    // hoping a real model stays quiet.
    let scripted = scripted_actor::ScriptedActor::install(&[]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_survives_a_quiet_pane_past_the_subscription_timeout",
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
            .env("PATH", &path_env)
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
    // P2.5 W1 (ruling 0049 D148): the first turn ends idle with no
    // worktree change (the script's own `idle` step) -- asserted, not
    // hoped for, since the loop's re-prompt path only used to be
    // reached if a real model happened to end its first turn with
    // nothing changed. The loop's continuation then earns the edit,
    // and its next continuation earns the actor's own `wirk claim`.
    let scripted = scripted_actor::ScriptedActor::install(&[
        "idle",
        "edit:report.md:filed by the scripted actor, proving the pane was still unclaimed and \
         prompted again",
        "claim:--artifact report.md=report.md",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_prompts_an_idle_unclaimed_pane_again_then_claims",
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
    let _pointer = wait_for_pointer(&estate);

    let (work_id, run_id, _waypoint) =
        submit_actor(&estate, &repo, "reply with the word ready and stop");

    let mut run_child = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--session", session.name()])
        .args(["--herdr-socket"])
        .arg(session.socket_path())
        .args(["--actor-kind", "opencode"])
        .env("PATH", &path_env)
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
    // first Idle. The scripted actor's own `edit` step now runs (in
    // answer to that continuation prompt), then the loop's *next*
    // continuation (the worktree changed, so this is not the stuck
    // path) earns the actor's own `wirk claim` — the real claim path,
    // not a claim the test files behind the actor's back.
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
    // P2.5 W1: the re-prompt path is now asserted, not hoped for — at
    // least the intent's own first prompt and one continuation prompt
    // must appear (the scripted actor's three-turn script earns a
    // second continuation too, once the Claim lands); every prompt line
    // names whichever turn-ended status it answered (`Idle` or `Done` —
    // build-brief.md §9), not a hardcoded "Idle", since a headless pane
    // reports `Done` live. A `SinceLastPrompt` line's own `describe()`
    // (`wirk-herdr/src/run_loop.rs`) embeds `wirk_herdr::git::
    // fingerprint`'s value, which is itself `"{status}\n{head_sha}"` —
    // a genuine embedded newline, so that one printed line can arrive
    // as two physical stdout lines; the "names sending:" half of this
    // check is not asserted per fragment for that reason.
    assert!(
        prompt_lines.len() >= 2,
        "wirk run's stdout must carry at least the intent prompt and one continuation: \
         {stdout_lines:?}"
    );
    for line in &prompt_lines {
        assert!(
            line.contains("Idle answered") || line.contains("Done answered"),
            "every prompt line must name the turn-ended status it answered (Idle or Done): \
             {line:?}"
        );
    }
    assert!(
        prompt_lines
            .iter()
            .any(|line| line.contains("first continuation")),
        "expected a FirstContinuation prompt line (the re-prompt this test pins): \
         {stdout_lines:?}"
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
    // P2.5 W1 (ruling 0049 D148): two `idle` turns in a row — the
    // intent earns no baseline (W6), the loop's first continuation
    // earns an unconditional second prompt (`FirstContinuation`) and
    // takes its baseline right after, and the scripted actor's second
    // `idle` turn makes no worktree change, so it must be judged stuck
    // on every run, not only when a real model happens to repeat
    // itself.
    let scripted = scripted_actor::ScriptedActor::install(&["idle", "idle"]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_stuck_after_the_first_continuation_exits_4",
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
        .env("PATH", &path_env)
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

/// The live twin of `wirk-herdr/tests/run_loop.rs::
/// retry_does_not_exit_needs_input_on_the_previous_runs_history` (P2.5
/// W3, ruling 0050 D151): a Work fails once (stuck, same shape as
/// `wirk_run_stuck_after_the_first_continuation_exits_4` above), is
/// retried (`wirk work retry`), and the retry's own driver — reading
/// the real `wirkd watch` stream, which replays the *whole* journal
/// including the first attempt's own `NeedsInput`-causing `RunFailed`
/// before ever reaching the retry's own `RunOpened` — reaches `Claimed`
/// through the scripted actor with no spurious `NeedsInput` printed in
/// its own stdout.
#[test]
fn wirk_run_retries_a_stuck_run_and_reaches_claimed() {
    let stuck_scripted = scripted_actor::ScriptedActor::install(&["idle", "idle"]);
    let stuck_path_env = scripted_actor_path(&stuck_scripted);
    let stuck_script_path = stuck_scripted.script_path();
    let stuck_script_path = stuck_script_path.to_str().expect("script path is utf-8");

    let Some(session1) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_retries_a_stuck_run_and_reaches_claimed_1",
        &[
            ("PATH", &stuck_path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", stuck_script_path),
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

    let (work_id, _run1_id, _waypoint) = submit_actor(
        &estate,
        &repo,
        "Reply with the single word waiting and then stop. Do not read or edit any file, do not \
         run any command, do not run wirk claim, do not ask a question. Whenever you are \
         prompted again, reply with the single word waiting and stop.",
    );

    // First attempt: the same "stuck after the first continuation"
    // shape as the test above -- `wirk run` exits 4, the Work is
    // `NeedsInput`.
    let run1_status = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--session", session1.name()])
        .args(["--herdr-socket"])
        .arg(session1.socket_path())
        .args(["--actor-kind", "opencode"])
        .env("PATH", &stuck_path_env)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .expect("run wirk run (first attempt)");
    assert_eq!(
        run1_status.code(),
        Some(4),
        "the first attempt must exit 4 (NeedsInput/stuck): {run1_status:?}"
    );
    wait_for_event(
        &estate,
        &work_id,
        |kind| matches!(kind, EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck")),
    );
    drop(session1); // this attempt's pane/session is done with

    // A retry reserves the *same* Waypoint's World again -- same
    // `branch`, same `worktree_path` (keyed on `work_id`, not `run_id`,
    // `executor.rs`'s own `run_command`) -- and `worktree_add` always
    // `git worktree add -b <branch>` fresh (`git.rs`): reattaching to
    // an existing worktree/branch is 0050 D151's own carried, separate
    // finding ("relaunch always tries `git worktree add` fresh and
    // fails on the existing branch"), not this item's fix. This test's
    // own cleanup -- removing the first attempt's worktree and branch,
    // exactly what a human operator does today -- isolates the fix
    // this item *does* make (the retry race in `observe_watch`) from
    // that separate, unfixed one.
    let worktree_path = estate.join("worktrees").join(&work_id);
    let branch = format!("wirk/{work_id}");
    let _ = Command::new("git")
        .current_dir(&repo)
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_path)
        .status();
    let _ = Command::new("git")
        .current_dir(&repo)
        .args(["branch", "-D", &branch])
        .status();

    // `wirk work retry`: journals a fresh `RunOpened` for a new run id
    // on the same Waypoint (`handle_retry`, `server.rs`) — the exact
    // race 0050 D151 named: the next `wirk run`'s own `watch` stream
    // will replay the first attempt's `RunOpened`/`RunFailed{stuck}`
    // (which already put the Work in `NeedsInput`) *before* it ever
    // reaches this new `RunOpened`.
    let retry = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id])
        .output()
        .expect("wirk work retry runs");
    assert!(
        retry.status.success(),
        "wirk work retry failed: {}",
        String::from_utf8_lossy(&retry.stderr)
    );

    // Second attempt: a scripted actor that actually claims, in a fresh
    // session (the scripted actor's script is read from an env var set
    // once at session start, `scripted_actor.rs`'s own doc).
    let claims_scripted = scripted_actor::ScriptedActor::install(&[
        "idle",
        "edit:report.md:a throwaway repo for the retry's own tried step",
        "claim:--artifact report.md=report.md",
    ]);
    let claims_path_env = scripted_actor_path(&claims_scripted);
    let claims_script_path = claims_scripted.script_path();
    let claims_script_path = claims_script_path.to_str().expect("script path is utf-8");

    let Some(session2) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_retries_a_stuck_run_and_reaches_claimed_2",
        &[
            ("PATH", &claims_path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", claims_script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

    let mut run2_child = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--session", session2.name()])
        .args(["--herdr-socket"])
        .arg(session2.socket_path())
        .args(["--actor-kind", "opencode"])
        .env("PATH", &claims_path_env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wirk run (retry)");

    let run2_stdout = run2_child.stdout.take().expect("wirk run stdout piped");
    let run2_stdout_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let run2_stdout_reader = {
        let lines = Arc::clone(&run2_stdout_lines);
        std::thread::spawn(move || {
            for line in BufReader::new(run2_stdout).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        })
    };
    let run2_stderr = run2_child.stderr.take().expect("wirk run stderr piped");
    let run2_stderr_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let run2_stderr_reader = {
        let lines = Arc::clone(&run2_stderr_lines);
        std::thread::spawn(move || {
            for line in BufReader::new(run2_stderr).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        })
    };

    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if run2_child.try_wait().ok().flatten().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never observed: wirk run (retry) exiting on its own; stdout so far {:?}",
            run2_stdout_lines.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let run2_status = run2_child.wait().expect("reap wirk run (retry)");
    run2_stdout_reader.join().expect("stdout reader thread");
    run2_stderr_reader.join().expect("stderr reader thread");
    guard.0.push(run2_child);

    let run2_stdout_lines = run2_stdout_lines.lock().unwrap().clone();
    let run2_stderr_lines = run2_stderr_lines.lock().unwrap().clone();
    assert!(
        run2_status.success(),
        "the retry's own driver must reach Claimed (exit 0): {run2_status:?}, stdout \
         {run2_stdout_lines:?}, stderr {run2_stderr_lines:?}"
    );
    assert!(
        !run2_stdout_lines.iter().any(|line| line == "NeedsInput"),
        "the retry's own driver must never print a spurious NeedsInput from the first \
         attempt's replayed history: {run2_stdout_lines:?}"
    );

    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    let run_opened_ids: Vec<RunId> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::RunOpened { run, .. } => Some(run.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        run_opened_ids.len(),
        2,
        "expected exactly two RunOpened (the stuck attempt and its retry): {events:?}"
    );
    let retry_run_id = &run_opened_ids[1];
    let claimed_run = events.iter().find_map(|event| match &event.kind {
        EventKind::ClaimRecorded {
            claim_kind: ClaimKind::Done,
            verdict: wirk_core::ClaimVerdict::Validated,
            ..
        } => event.run.clone(),
        _ => None,
    });
    assert_eq!(
        claimed_run.as_ref(),
        Some(retry_run_id),
        "the ClaimRecorded{{Done, Validated}} must name the retry's own RunOpened id, not the \
         first (stuck) attempt's: {events:?}"
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

/// W6 (`p2-concurrency/tried/RESULT.md` stage 04's own carried finding,
/// 0050 D151): the sibling of
/// `wirk_run_retries_a_stuck_run_and_reaches_claimed` above, with that
/// test's own manual cleanup (`git worktree remove --force` + `git
/// branch -D`) deliberately **not** run before the retry — the second
/// `wirk run` must reuse the first attempt's still-present worktree and
/// branch on its own (`wirk_herdr::git::worktree_add`'s W6 fix) rather
/// than failing on `git worktree add -b <branch>`'s "branch already
/// exists". Reaches `Claimed`, and exactly one `wirk/<work_id>` branch
/// exists afterward — never two, never zero.
#[test]
fn wirk_run_retry_reuses_the_worktree_and_branch() {
    let stuck_scripted = scripted_actor::ScriptedActor::install(&["idle", "idle"]);
    let stuck_path_env = scripted_actor_path(&stuck_scripted);
    let stuck_script_path = stuck_scripted.script_path();
    let stuck_script_path = stuck_script_path.to_str().expect("script path is utf-8");

    let Some(session1) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_retry_reuses_the_worktree_and_branch_1",
        &[
            ("PATH", &stuck_path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", stuck_script_path),
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

    let (work_id, _run1_id, _waypoint) = submit_actor(
        &estate,
        &repo,
        "Reply with the single word waiting and then stop. Do not read or edit any file, do not \
         run any command, do not run wirk claim, do not ask a question. Whenever you are \
         prompted again, reply with the single word waiting and stop.",
    );

    // First attempt: stuck, `NeedsInput`, exactly as the sibling test.
    let run1_status = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--session", session1.name()])
        .args(["--herdr-socket"])
        .arg(session1.socket_path())
        .args(["--actor-kind", "opencode"])
        .env("PATH", &stuck_path_env)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .expect("run wirk run (first attempt)");
    assert_eq!(
        run1_status.code(),
        Some(4),
        "the first attempt must exit 4 (NeedsInput/stuck): {run1_status:?}"
    );
    wait_for_event(
        &estate,
        &work_id,
        |kind| matches!(kind, EventKind::RunFailed { cause } if cause.status.as_deref() == Some("stuck")),
    );
    drop(session1);

    let worktree_path = estate.join("worktrees").join(&work_id);
    let branch = format!("wirk/{work_id}");
    assert!(
        worktree_path.exists(),
        "the first attempt's worktree must still be on disk (nothing removed it)"
    );

    // No manual cleanup here — this is the whole point of the fix.
    let retry = Command::new(wirk_bin())
        .args(["work", "retry", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id])
        .output()
        .expect("wirk work retry runs");
    assert!(
        retry.status.success(),
        "wirk work retry failed: {}",
        String::from_utf8_lossy(&retry.stderr)
    );

    let claims_scripted = scripted_actor::ScriptedActor::install(&[
        "idle",
        "edit:report.md:a throwaway repo for the retry's own reuse test",
        "claim:--artifact report.md=report.md",
    ]);
    let claims_path_env = scripted_actor_path(&claims_scripted);
    let claims_script_path = claims_scripted.script_path();
    let claims_script_path = claims_script_path.to_str().expect("script path is utf-8");

    let Some(session2) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_retry_reuses_the_worktree_and_branch_2",
        &[
            ("PATH", &claims_path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", claims_script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

    let mut run2_child = Command::new(wirk_bin())
        .args(["run", "--estate"])
        .arg(&estate)
        .args(["--work", &work_id, "--session", session2.name()])
        .args(["--herdr-socket"])
        .arg(session2.socket_path())
        .args(["--actor-kind", "opencode"])
        .env("PATH", &claims_path_env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wirk run (retry)");

    let run2_stdout = run2_child.stdout.take().expect("wirk run stdout piped");
    let run2_stdout_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let run2_stdout_reader = {
        let lines = Arc::clone(&run2_stdout_lines);
        std::thread::spawn(move || {
            for line in BufReader::new(run2_stdout).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        })
    };
    let run2_stderr = run2_child.stderr.take().expect("wirk run stderr piped");
    let run2_stderr_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let run2_stderr_reader = {
        let lines = Arc::clone(&run2_stderr_lines);
        std::thread::spawn(move || {
            for line in BufReader::new(run2_stderr).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        })
    };

    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if run2_child.try_wait().ok().flatten().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never observed: wirk run (retry) exiting on its own; stdout so far {:?}",
            run2_stdout_lines.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let run2_status = run2_child.wait().expect("reap wirk run (retry)");
    run2_stdout_reader.join().expect("stdout reader thread");
    run2_stderr_reader.join().expect("stderr reader thread");
    guard.0.push(run2_child);

    let run2_stdout_lines = run2_stdout_lines.lock().unwrap().clone();
    let run2_stderr_lines = run2_stderr_lines.lock().unwrap().clone();
    assert!(
        run2_status.success(),
        "the retry's own driver must reach Claimed (exit 0) by reusing the existing worktree \
         and branch, not failing on \"branch already exists\": {run2_status:?}, stdout \
         {run2_stdout_lines:?}, stderr {run2_stderr_lines:?}"
    );

    let journal = Journal::open(estate.join("works").join(&work_id)).expect("open journal");
    let events = journal.replay().expect("journal replays cleanly");
    let claimed_run = events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::ClaimRecorded {
                claim_kind: ClaimKind::Done,
                verdict: wirk_core::ClaimVerdict::Validated,
                ..
            }
        )
    });
    assert!(
        claimed_run,
        "expected a ClaimRecorded{{Done, Validated}} in the journal: {events:?}"
    );

    // Exactly one branch for this Work — the retry's reuse never left a
    // second one behind, and the collision never forced a manual `-D`
    // that would also leave zero.
    let branch_list = Command::new("git")
        .current_dir(&repo)
        .args(["branch", "--list", &branch])
        .output()
        .expect("git branch --list runs");
    let branch_lines: Vec<String> = String::from_utf8_lossy(&branch_list.stdout)
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(['*', '+'])
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(
        branch_lines,
        vec![branch.clone()],
        "expected exactly one {branch} branch for the Work, got {branch_lines:?}"
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

/// P2.5 W4 (BRIEF.md item 3, `orient/two-works.md` §1): two Works, two
/// scratch repos, one estate/wirkd, **one Herdr session**, two `wirk
/// run` drivers backgrounded concurrently — each driving its own
/// workspace (Herdr names a pane `run.id.0`, `wirk-herdr/src/
/// lib.rs::actor_pane`, so two distinct Runs always get two distinct
/// panes without any extra flag). The orient found no separate
/// mechanism is needed for isolation beyond W2's launch-readiness fix:
/// Herdr's own per-pane subscription filter (`launch_actor` subscribes
/// scoped to its own `pane_id`) and wirkd's per-Work journal map
/// already keep one driver from ever seeing the other's events. This
/// test is the live proof of that read, not a new fix — it pins the
/// isolation as a fact, not as this wave's own change.
#[test]
fn wirk_run_drives_two_works_at_once_with_no_cross_contamination() {
    let scripted = scripted_actor::ScriptedActor::install(&[
        "idle",
        "edit:report.md:a throwaway repo for the two-Works tried step",
        "claim:--artifact report.md=report.md",
    ]);
    let path_env = scripted_actor_path(&scripted);
    let script_path = scripted.script_path();
    let script_path = script_path.to_str().expect("script path is utf-8");

    // One Herdr session for both drivers — the isolation this test
    // pins is Herdr's own per-pane subscription filter and wirkd's
    // per-Work journal, not two sessions kept apart by construction.
    let Some(session) = live_herdr::LiveHerdrSession::start_with_env(
        "wirk_run_drives_two_works_at_once_with_no_cross_contamination",
        &[
            ("PATH", &path_env),
            ("WIRK_SCRIPTED_ACTOR_SCRIPT", script_path),
            ("SHELL", "/bin/sh"),
        ],
    ) else {
        return;
    };

    // One estate, one wirkd: two Works' journals live side by side
    // under it (`WirkdState::journals: Mutex<HashMap<WorkId, ...>>`,
    // `orient/two-works.md` §1) — the isolation under test is between
    // two Works of one estate, not between two estates.
    let estate_dir = tempfile::tempdir().expect("estate tempdir");
    let estate = estate_dir.path().to_path_buf();

    // Two Routes, two scratch repos (keeps each Work's own findings
    // separate, `orient/two-works.md`'s own "tried step" shape).
    let repo_a_dir = tempfile::tempdir().expect("repo A tempdir");
    let repo_a = repo_a_dir.path().to_path_buf();
    init_repo(&repo_a);
    let repo_b_dir = tempfile::tempdir().expect("repo B tempdir");
    let repo_b = repo_b_dir.path().to_path_buf();
    init_repo(&repo_b);

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

    let (work_a, run_a, _wp_a) = submit_actor_named(
        &estate,
        &repo_a,
        "write report.md, then claim (Work A)",
        "two_works_a",
    );
    let (work_b, run_b, _wp_b) = submit_actor_named(
        &estate,
        &repo_b,
        "write report.md, then claim (Work B)",
        "two_works_b",
    );
    assert_ne!(work_a, work_b, "two submits must open two distinct Works");
    assert_ne!(run_a, run_b, "two submits must open two distinct Runs");

    // Two drivers, backgrounded concurrently, one session each Run's
    // own pane lives in — same session name/socket, distinct `--work`.
    fn spawn_driver(
        wirk_bin: &str,
        estate: &Path,
        work_id: &str,
        session: &live_herdr::LiveHerdrSession,
        path_env: &str,
    ) -> std::process::Child {
        Command::new(wirk_bin)
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
            .expect("spawn wirk run")
    }

    let mut driver_a = spawn_driver(wirk_bin(), &estate, &work_a, &session, &path_env);
    let mut driver_b = spawn_driver(wirk_bin(), &estate, &work_b, &session, &path_env);

    // Capture each driver's own stdout/stderr on a reader thread (the
    // pattern `wirk_run_retries_a_stuck_run_and_reaches_claimed` already
    // uses) — needed to assert neither driver ever prints the other's
    // pane (`run.id.0` names the pane, `actor_pane`), not just to watch
    // for the exit.
    fn spawn_line_capture(
        stream: impl std::io::Read + Send + 'static,
    ) -> (Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let lines = Arc::clone(&lines);
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    lines.lock().unwrap().push(line);
                }
            })
        };
        (lines, handle)
    }

    let (stdout_a, stdout_a_join) =
        spawn_line_capture(driver_a.stdout.take().expect("driver A stdout piped"));
    let (stderr_a, stderr_a_join) =
        spawn_line_capture(driver_a.stderr.take().expect("driver A stderr piped"));
    let (stdout_b, stdout_b_join) =
        spawn_line_capture(driver_b.stdout.take().expect("driver B stdout piped"));
    let (stderr_b, stderr_b_join) =
        spawn_line_capture(driver_b.stderr.take().expect("driver B stderr piped"));

    // Bounded poll (issue 359) — never a product-side wait — for both
    // children to exit on their own.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let a_done = driver_a.try_wait().ok().flatten().is_some();
        let b_done = driver_b.try_wait().ok().flatten().is_some();
        if a_done && b_done {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never observed: both wirk run drivers exiting on their own (A done: {a_done}, B \
             done: {b_done}); stdout A so far {:?}, stdout B so far {:?}",
            stdout_a.lock().unwrap(),
            stdout_b.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let status_a = driver_a.wait().expect("reap driver A");
    let status_b = driver_b.wait().expect("reap driver B");
    stdout_a_join.join().expect("driver A stdout reader");
    stderr_a_join.join().expect("driver A stderr reader");
    stdout_b_join.join().expect("driver B stdout reader");
    stderr_b_join.join().expect("driver B stderr reader");
    guard.0.push(driver_a);
    guard.0.push(driver_b);

    let stdout_a = stdout_a.lock().unwrap().clone();
    let stderr_a = stderr_a.lock().unwrap().clone();
    let stdout_b = stdout_b.lock().unwrap().clone();
    let stderr_b = stderr_b.lock().unwrap().clone();

    assert!(
        status_a.success(),
        "driver A must reach Claimed (exit 0): {status_a:?}, stdout {stdout_a:?}, stderr \
         {stderr_a:?}"
    );
    assert!(
        status_b.success(),
        "driver B must reach Claimed (exit 0): {status_b:?}, stdout {stdout_b:?}, stderr \
         {stderr_b:?}"
    );
    assert!(
        stdout_a.iter().any(|line| line == "Claimed"),
        "driver A stdout must print Claimed: {stdout_a:?}"
    );
    assert!(
        stdout_b.iter().any(|line| line == "Claimed"),
        "driver B stdout must print Claimed: {stdout_b:?}"
    );

    // Neither driver's own stdout/stderr ever names the other's pane.
    // Herdr names a pane `run.id.0` (`actor_pane`), so the other Run's
    // id string is the pane's own name — the exact thing that must
    // never leak across the per-pane subscription filter.
    for line in stdout_a.iter().chain(stderr_a.iter()) {
        assert!(
            !line.contains(&run_b),
            "driver A (Run {run_a}) printed a line naming driver B's own pane/Run {run_b}: \
             {line:?}"
        );
    }
    for line in stdout_b.iter().chain(stderr_b.iter()) {
        assert!(
            !line.contains(&run_a),
            "driver B (Run {run_b}) printed a line naming driver A's own pane/Run {run_a}: \
             {line:?}"
        );
    }

    // Each Work's own journal names only its own Run/Waypoint — no
    // cross-Work event ever lands in the wrong journal (wirkd's
    // per-Work journal map, `orient/two-works.md` §1).
    let journal_a = Journal::open(estate.join("works").join(&work_a)).expect("open journal A");
    let events_a = journal_a.replay().expect("journal A replays cleanly");
    let journal_b = Journal::open(estate.join("works").join(&work_b)).expect("open journal B");
    let events_b = journal_b.replay().expect("journal B replays cleanly");

    fn run_ids_named(events: &[wirk_core::Event]) -> std::collections::BTreeSet<String> {
        events
            .iter()
            .filter_map(|event| event.run.as_ref().map(|run| run.0.clone()))
            .collect()
    }
    let run_ids_a = run_ids_named(&events_a);
    let run_ids_b = run_ids_named(&events_b);
    assert_eq!(
        run_ids_a,
        std::collections::BTreeSet::from([run_a.clone()]),
        "Work A's journal must name only its own Run: {events_a:?}"
    );
    assert_eq!(
        run_ids_b,
        std::collections::BTreeSet::from([run_b.clone()]),
        "Work B's journal must name only its own Run: {events_b:?}"
    );

    let claimed_a = events_a.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::ClaimRecorded {
                claim_kind: ClaimKind::Done,
                verdict: wirk_core::ClaimVerdict::Validated,
                ..
            }
        )
    });
    assert!(
        claimed_a,
        "expected Work A ClaimRecorded{{Done, Validated}}"
    );
    let claimed_b = events_b.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::ClaimRecorded {
                claim_kind: ClaimKind::Done,
                verdict: wirk_core::ClaimVerdict::Validated,
                ..
            }
        )
    });
    assert!(
        claimed_b,
        "expected Work B ClaimRecorded{{Done, Validated}}"
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
