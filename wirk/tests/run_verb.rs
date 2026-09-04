//! Ungated integration test for `wirk run` (item 4, W3; `orient/
//! build-brief.md` "Ungated test in wirk/tests/run_verb.rs"). Drives the
//! real built binary end to end — a real `wirk wirkd`, a real `wirk work
//! submit --kind actor`, a real `wirk run` — against a scripted fake
//! Herdr socket server standing in for a live Herdr session (never a
//! live one: `cargo test` touches no Herdr, matching `client.md` §4's
//! gating). No sleeps as waits (issue 359): every synchronization point
//! below is a bounded `mpsc::recv`/read/poll, not a tuned delay — the
//! rendezvous around `agent.prompt` in particular guarantees the Claim
//! lands in wirkd *before* `wirk run`'s own subscription can observe the
//! next status change and poll `Claimed`, so the outcome is
//! deterministic rather than raced. `wirkd` and `wirk run` are both real
//! child processes; `KillOnDrop` guards both for the whole test body so
//! a failed assertion still leaves no process behind (ruling 0030).

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
/// no Claude trust block in this test, nothing opens a pane for real).
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

/// `wirk work submit --estate <estate> --intent <text> --kind actor
/// --repo-path <repo> --base HEAD`, parsing its `work_id <id> run_id
/// <id> waypoint <id>` stdout line (same parse `wirkd_process.rs`'s
/// `submit` uses, R6 duplicate — the two tests submit different World
/// kinds).
fn submit_actor(estate: &Path, repo: &Path, intent: &str) -> (String, String, String) {
    let output = Command::new(wirk_bin())
        .args(["work", "submit", "--estate"])
        .arg(estate)
        .args(["--intent", intent, "--kind", "actor", "--repo-path"])
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

// ---- the fake Herdr socket server --------------------------------------
//
// One `UnixListener`, one accept loop, one thread per accepted
// connection (`wirk-herdr/tests/socket.rs`'s own pattern, reused). A
// connection's first request line decides its role: `events.subscribe`
// makes it a subscription connection — acked with the request's own id
// verbatim (`refs/herdr` `0f8ad12` `src/api/server.rs:722-733` writes
// `SuccessResponse { id: request_id, result: subscription_started }`),
// then streamed `blocked`/`idle`/`working`. There is exactly one such
// connection per Run since fix 3: `HerdrExecutor::launch_actor` opens
// the Run's single subscription before `agent.start` and hands it to
// `RunLoop`, which no longer opens a second one. Anything else is a
// plain request
// connection, matching live Herdr 0.8.2 (module doc comment in
// `wirk-herdr/src/socket.rs`; `refs/herdr` `0f8ad12`
// `src/api/server.rs:274-301`; `socket-api.mdx:668`): dispatch this one
// request, write its one reply, close — never loop back to read a
// second line on it. `SocketClient` now dials one such connection per
// call (`pane.get`, `workspace.create`, `pane.split`, `agent.start`,
// `agent.prompt`, ...), so this fake sees several short-lived
// connections where it used to see one held one.

fn write_reply(writer: &mut UnixStream, id: &str, result: Value) {
    let mut line = serde_json::to_string(&json!({"id": id, "result": result})).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).unwrap();
    writer.flush().unwrap();
}

fn write_error(writer: &mut UnixStream, id: &str, code: &str, message: &str) {
    let mut line =
        serde_json::to_string(&json!({"id": id, "error": {"code": code, "message": message}}))
            .unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).unwrap();
    writer.flush().unwrap();
}

/// One pushed `pane.agent_status_changed` line, in the shape a real
/// server sends for a `pane.agent_status_changed` subscription: the
/// `SubscriptionEventEnvelope` of the vendored schema
/// (`herdr-schema-0.8.2-p20.json`, schema `subscription_event`) — the
/// **dotted** kind in `event`, and `data` untagged, with no `"type"`
/// field at all (`refs/herdr` `0f8ad12` `src/api/schema/events.rs:377-
/// 389`, `#[serde(untagged)] SubscriptionEventData`). This fake used to
/// send an invented envelope (`"event"` an object, `data` tagged);
/// `parse_pushed_event` supplies the tag from the dotted kind now, so
/// this line is what the client is actually built against.
fn write_status_event(writer: &mut UnixStream, status: &str) {
    let data = json!({
        "pane_id": "actor-pane",
        "workspace_id": "ws1",
        "agent_status": status,
    });
    let mut line =
        serde_json::to_string(&json!({"event": "pane.agent_status_changed", "data": data}))
            .unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).unwrap();
    writer.flush().unwrap();
}

fn sample_pane_json(pane_id: &str) -> Value {
    json!({
        "pane_id": pane_id,
        "terminal_id": "term1",
        "workspace_id": "ws1",
        "tab_id": "tab1",
        "focused": true,
        "agent_status": "idle",
        "revision": 1,
    })
}

fn sample_workspace_json(workspace_id: &str) -> Value {
    json!({
        "workspace_id": workspace_id,
        "number": 1,
        "label": "w1",
        "focused": true,
        "pane_count": 1,
        "tab_count": 1,
        "active_tab_id": "tab1",
        "agent_status": "idle",
    })
}

/// Starts the fake on a tempdir socket, returning its path. `prompt_tx`
/// is signalled (never blocked on) the instant `agent.prompt` arrives;
/// the connection then blocks on `claim_done_rx` before replying —
/// the rendezvous that orders "the Claim is in wirkd's journal" before
/// "`wirk run` observes the next status and polls `Claimed`", with no
/// sleep on either side (issue 359).
fn spawn_fake_herdr(
    dir: &tempfile::TempDir,
    prompt_tx: mpsc::Sender<()>,
    claim_done_rx: Arc<Mutex<mpsc::Receiver<()>>>,
) -> PathBuf {
    let socket_path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind fake herdr socket");
    let subscribe_seen = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let prompt_tx = prompt_tx.clone();
            let claim_done_rx = Arc::clone(&claim_done_rx);
            let subscribe_seen = Arc::clone(&subscribe_seen);
            std::thread::spawn(move || {
                handle_fake_connection(stream, &prompt_tx, &claim_done_rx, &subscribe_seen);
            });
        }
    });
    socket_path
}

fn handle_fake_connection(
    stream: UnixStream,
    prompt_tx: &mpsc::Sender<()>,
    claim_done_rx: &Arc<Mutex<mpsc::Receiver<()>>>,
    subscribe_seen: &Arc<AtomicUsize>,
) {
    let mut writer = stream.try_clone().expect("clone fake herdr stream");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let Ok(req) = serde_json::from_str::<Value>(line.trim_end()) else {
        return;
    };

    if req["method"] == "events.subscribe" {
        let id = req["id"].as_str().unwrap_or("").to_string();
        write_reply(&mut writer, &id, json!({"type": "subscription_started"}));
        assert_eq!(
            subscribe_seen.fetch_add(1, Ordering::SeqCst),
            0,
            "one Run opens exactly one events.subscribe since fix 3"
        );
        // The Run's one subscription: streamed in order,
        // no rendezvous here — the ordering guarantee comes from the
        // *request* connection's agent.prompt handling below, which
        // `RunLoop::observe` waits on synchronously before it can reach
        // the next event in this stream.
        for status in ["blocked", "idle", "working"] {
            write_status_event(&mut writer, status);
        }
        return;
    }

    // A plain request connection: dispatch exactly this one request,
    // write its one reply, then return — closing the connection,
    // matching live Herdr 0.8.2 (see the comment above). Never reads a
    // second line on it: `SocketClient` dials a new connection for its
    // next call.
    let method = req["method"].as_str().unwrap_or("").to_string();
    let id = req["id"].as_str().unwrap_or("").to_string();
    match method.as_str() {
        "pane.get" => write_error(&mut writer, &id, "pane_not_found", "no such pane"),
        "workspace.create" => write_reply(
            &mut writer,
            &id,
            json!({"type": "workspace_created", "workspace": sample_workspace_json("ws1")}),
        ),
        "pane.split" => write_reply(
            &mut writer,
            &id,
            json!({"type": "pane_info", "pane": sample_pane_json("actor-pane")}),
        ),
        "agent.start" => write_reply(&mut writer, &id, json!({"type": "agent_started"})),
        "agent.prompt" => {
            let _ = prompt_tx.send(());
            // Blocks until the test thread has filed and confirmed
            // the Claim through wirkd — the rendezvous.
            let _ = claim_done_rx.lock().unwrap().recv();
            write_reply(&mut writer, &id, json!({"type": "agent_prompted"}));
        }
        "session.snapshot" => write_reply(
            &mut writer,
            &id,
            json!({"type": "session_snapshot", "snapshot": {"panes": []}}),
        ),
        other => {
            write_error(
                &mut writer,
                &id,
                "unexpected_method",
                &format!("fake herdr: unscripted method {other:?}"),
            );
        }
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

    let herdr_dir = tempfile::tempdir().expect("fake herdr socket tempdir");
    let (prompt_tx, prompt_rx) = mpsc::channel::<()>();
    let (claim_done_tx, claim_done_rx) = mpsc::channel::<()>();
    let herdr_socket = spawn_fake_herdr(&herdr_dir, prompt_tx, Arc::new(Mutex::new(claim_done_rx)));

    guard.0.push(
        Command::new(wirk_bin())
            .args(["run", "--estate"])
            .arg(&estate)
            .args(["--work", &work_id, "--session", "unused-in-this-test"])
            .args(["--herdr-socket"])
            .arg(&herdr_socket)
            .args(["--nudge-after", "120"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirk run"),
    );

    // Blocks until the fake sees agent.prompt (no sleep, issue 359); a
    // `wirk run` that never reaches the prompt hangs here and the test
    // harness's own timeout is the backstop, same as every other
    // blocking recv/read in this crate's tests.
    prompt_rx
        .recv()
        .expect("the fake herdr server observes agent.prompt");

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
    claim_done_tx
        .send(())
        .expect("signal the fake to reply to agent.prompt");

    // `wirk run` exits 0 (Claimed) — the last of `guard.0`.
    let run_status = guard
        .0
        .last_mut()
        .expect("wirk run child is in guard")
        .wait()
        .expect("reap wirk run");
    assert!(run_status.success(), "wirk run exit status: {run_status:?}");

    // The journal: WorktreeCreated with the commit's SHA, RunLaunched,
    // ClaimRecorded{Done, Validated} (build-brief.md "Ungated test").
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

    let run_launched = events
        .iter()
        .any(|event| matches!(&event.kind, EventKind::RunLaunched { run } if run.0 == run_id));
    assert!(run_launched, "expected RunLaunched for {run_id}");

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

    // Teardown: stop wirkd, then let `guard`'s Drop reap both children.
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
