//! Live method sweep against a real Herdr session (fix 2, 0028 tried
//! step's lesson: "the live run is finding one call per run" —
//! `knowledge/work/p1-herdr-executor/ASSESSMENT.md`). No longer
//! `#[ignore]`d (0040 D127: "the live sweep becomes the ordinary
//! suite, gated only on Herdr being installed") — it starts and tears
//! down its own throwaway named session via `LiveHerdrSession`, never
//! the owner's `default`, and is skipped with a printed reason (never a
//! hard failure) when `herdr` is not on PATH (`tests.md` §3).
//!
//! Walks `HerdrExecutor::launch`'s own order first — `ping`,
//! `session.snapshot`, `workspace.create`, `pane.split`, `subscribe`,
//! `start_agent` — then the rest of `HerdrClient`'s surface, plus
//! `tab.create` (a real, schema-defined method with no `HerdrClient`
//! wrapper — sent here as a raw NDJSON line, `raw_call`, the same
//! framing `SocketClient::call` uses, R1: no trait method exists to
//! reuse for a one-off protocol check outside this item's scope).
//!
//! Panes are split from the one workspace this test creates: `pane_a`
//! carries the "launch order" continuation (`subscribe` then a genuine
//! `agent.start`, matching what `HerdrExecutor::launch_actor` itself
//! does); `pane_b` never gets `agent.start` called on it and is both
//! the pane put to work writing output continuously for fix 3's
//! sequential-subscription step and the "pane with no agent" the brief
//! names for `agent.prompt`,
//! `agent.wait`, `agent.send_keys`, `pane.release_agent`,
//! `pane.report_agent`, `pane.report_agent_session` — each asserted to
//! come back a well-formed success or a business error carrying a
//! code, never a transport error or a raw `invalid_request` (this
//! item's fix 2: exactly the class of defect the tried step found live
//! and the conformance test in `tests/schema.rs` cannot, since it
//! never touches a real server).
//!
//! Fix 3 adds one step to that: with `pane_b` writing continuously,
//! three `events.subscribe` calls in sequence — the third after a
//! `pane.split` — each acked and each delivering an event. That is the
//! combination tried step 3 crashed on and no earlier test could reach
//! (one subscribe, idle pane); `pane_c`, split between the second and
//! third, is closed with the others at teardown.

#[path = "support/live_herdr.rs"]
mod live_herdr;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tempfile::tempdir;

use wirk_herdr::{
    AgentStatus, Bearing, CloseWorkspace, CreateWorkspace, EventSubscription, FocusPane,
    HerdrClient, HerdrError, Notify, OpenWorktree, PaneInfo, PromptAgent, ReleaseAgent,
    RemoveWorktree, ReportAgent, ReportAgentSession, ReportMetadata, SendKeys, SplitDirection,
    SplitPane, StartAgent,
};

/// The read timeout applied to every request connection this test's
/// client dials, and (via the reader thread `subscribe` starts) the
/// bound on how long the `events.subscribe` step waits for a pushed
/// event before giving up — generous for a live session, short enough
/// that a genuinely wedged server fails this test instead of hanging
/// the run (issue 359: bounded, not a sleep). Re-exported from the
/// fixture so this file's own `raw_call` keeps using the same bound.
use std::time::Duration;

/// This test file's own termination bound for its one raw, hand-framed
/// socket call (`raw_call` — no `HerdrClient` method wraps `tab.create`,
/// module doc): a live-run test may carry a bound whose exhaustion is
/// reported as "never observed" (the owner's ruling of 2026-09-02 §3;
/// ruling 0044's own exception), never a verdict about the product,
/// which itself sets no read timeout anywhere any more (fix 2).
const RAW_CALL_READ_TIMEOUT: Duration = Duration::from_secs(20);

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A pane/agent business error the schema itself names — success, or
/// an error the client's `HerdrError` mapping already recognizes.
/// Never `Transport`: that would mean the request was malformed
/// (schema-invalid) or the connection itself failed, exactly the class
/// of defect this sweep exists to catch (fix 2).
fn assert_ok_or_business_error<T: std::fmt::Debug>(label: &str, result: Result<T, HerdrError>) {
    match result {
        Ok(_) => {}
        Err(HerdrError::NotFound(_))
        | Err(HerdrError::Blocked(_))
        | Err(HerdrError::Invalid(_)) => {}
        Err(HerdrError::Transport(m)) => panic!(
            "{label}: got Transport({m:?}) — a schema-invalid request or a real transport \
             fault, not the well-formed success-or-business-error the schema promises"
        ),
    }
}

/// Sends one raw NDJSON request (`SocketClient::call`'s own framing,
/// duplicated here rather than reused: no `HerdrClient` method wraps
/// `tab.create`, and adding one only for this single test call is out
/// of this item's scope, R1) and returns the decoded reply object,
/// unexamined — the caller checks `result`/`error` itself.
fn raw_call(socket_path: &Path, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(socket_path)
        .unwrap_or_else(|e| panic!("raw_call({method}): connecting: {e}"));
    stream
        .set_read_timeout(Some(RAW_CALL_READ_TIMEOUT))
        .expect("set_read_timeout");
    let request = json!({"id": format!("live-sweep-{method}"), "method": method, "params": params});
    let mut line = serde_json::to_string(&request).expect("request serializes");
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .unwrap_or_else(|e| panic!("raw_call({method}): writing: {e}"));
    stream.flush().expect("flush");
    let mut reader = BufReader::new(stream);
    let mut raw = String::new();
    let n = reader
        .read_line(&mut raw)
        .unwrap_or_else(|e| panic!("raw_call({method}): reading reply: {e}"));
    assert!(
        n > 0,
        "raw_call({method}): connection closed before a reply"
    );
    serde_json::from_str(raw.trim_end())
        .unwrap_or_else(|e| panic!("raw_call({method}): malformed reply: {e}\nraw: {raw}"))
}

/// As `assert_ok_or_business_error`, for a `raw_call` reply: a
/// `"result"` or an `"error"` object is fine (whatever its code); a
/// reply with neither, or one that fails to parse as an object at all,
/// is not.
fn assert_raw_ok_or_business_error(method: &str, reply: &Value) {
    assert!(
        reply.get("result").is_some() || reply.get("error").is_some(),
        "{method}: reply has neither \"result\" nor \"error\": {reply}"
    );
}

#[test]
fn live_sweep_walks_every_socketclient_method_in_launch_order_then_the_rest() {
    let Some(session) = live_herdr::LiveHerdrSession::start(
        "live_sweep_walks_every_socketclient_method_in_launch_order_then_the_rest",
    ) else {
        return;
    };
    let socket_path = session.socket_path().to_path_buf();
    let client = session.client();

    // ---- HerdrExecutor::launch's own order --------------------------

    client.ping().expect("ping");

    let before = client.snapshot().expect("session.snapshot (before)");

    let workspace_dir = tempdir().expect("workspace tempdir");
    let ws = client
        .create_workspace(CreateWorkspace {
            cwd: workspace_dir.path().to_path_buf(),
            env: BTreeMap::new(),
            label: Some("wirk-live-sweep".to_string()),
        })
        .expect("workspace.create");

    // `create_workspace`'s own `WorkspaceInfo` carries no pane list;
    // find the pane(s) it seeded by diffing a fresh snapshot against
    // `before` by `workspace_id`, the same way `HerdrExecutor::launch`
    // itself locates a pane it did not create directly (via `get_pane`,
    // this test's equivalent is `snapshot`, already exercised above).
    let after = client.snapshot().expect("session.snapshot (after create)");
    let before_terminals: std::collections::BTreeSet<&str> = before
        .workspaces
        .iter()
        .map(|b| b.terminal_id.as_str())
        .collect();
    let seed: &Bearing = after
        .workspaces
        .iter()
        .find(|b| {
            b.workspace_id == ws.workspace_id && !before_terminals.contains(b.terminal_id.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "workspace.create ({}) seeded no pane visible in session.snapshot",
                ws.workspace_id
            )
        });

    // pane.split, each direction, off the seeded pane.
    let pane_a: PaneInfo = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id.clone()),
            target_pane_id: Some(seed.pane_id.clone()),
            direction: SplitDirection::Right,
            cwd: workspace_dir.path().to_path_buf(),
            env: BTreeMap::new(),
        })
        .expect("pane.split (right)");
    let pane_b: PaneInfo = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id.clone()),
            target_pane_id: Some(seed.pane_id.clone()),
            direction: SplitDirection::Down,
            cwd: workspace_dir.path().to_path_buf(),
            env: BTreeMap::new(),
        })
        .expect("pane.split (down)");

    client.get_pane(&pane_a.pane_id).expect("pane.get");
    client.list_agents().expect("agent.list");

    // events.subscribe: subscribe to pane_a, cause an update (a
    // harmless echo via pane.send_text), then read at least one event
    // before dropping the subscription — a genuine blocking read, no
    // timeout anywhere in the product (fix 2, ruling 0044).
    let mut events = client
        .subscribe(vec![EventSubscription::PaneUpdated {
            pane_id: pane_a.pane_id.clone(),
        }])
        .expect("events.subscribe");
    client
        .send_input(&pane_a.pane_id, "echo wirk-live-sweep\n")
        .expect("pane.send_text (the harmless echo)");
    // Blocks with no timeout (fix 2, ruling 0044: the product itself
    // sets none on a subscription connection any more) until Herdr
    // pushes the event the `send_input` above should trigger.
    let first_event = events
        .next()
        .expect("events.subscribe: the subscription ended before any event arrived");
    first_event.expect("events.subscribe: pushed line was not a well-formed HerdrEvent");
    drop(events);

    // ---- sequential subscriptions on a pane with output flowing ------
    //
    // Fix 3's own scenario (0028 tried step 3): the crash there needed
    // a *second* subscribe against a pane that was producing output,
    // which the one-subscribe step above cannot reach. `pane_b` is put
    // to work writing continuously, then three subscriptions are opened
    // in sequence — the third after a `pane.split` changes the session
    // — and each must ack and deliver.
    //
    // What the server's source says about a second subscription on a
    // busy pane (`refs/herdr` `0f8ad12`): nothing closes or renames it.
    // Each connection gets its own `stream_subscriptions` call with its
    // own `ActiveSubscription` set (`src/api/server.rs:689-751`), the
    // ack is `SuccessResponse { id: request_id }` verbatim
    // (`:722-733`), and per-pane subscriptions share only the app
    // channel their setup probe uses (`src/api/subscriptions.rs:207`,
    // `dispatch_to_app_with_timeout`, 5 s). The one thing that reaches
    // the client with a *derived* id is a setup-probe **error**
    // (`:709-717`), which is why this step asserts on real acks: an
    // `Ok(_)` from `subscribe` already means the ack id matched the
    // request id exactly (`socket.rs::subscribe_impl`), and an error
    // would name the failing subscription instead.
    client
        .send_input(
            &pane_b.pane_id,
            "yes wirk-live-sweep-output | head -c 200000\n",
        )
        .expect("pane.send_text (continuous output on pane_b)");

    let busy_subscriptions = || {
        vec![
            EventSubscription::PaneAgentStatusChanged {
                pane_id: pane_b.pane_id.clone(),
            },
            EventSubscription::PaneUpdated {
                pane_id: pane_b.pane_id.clone(),
            },
        ]
    };

    let mut first = client
        .subscribe(busy_subscriptions())
        .expect("events.subscribe #1 on a pane with output flowing: ack must match");
    first
        .next()
        .expect("events.subscribe #1: no event arrived within the read timeout")
        .expect("events.subscribe #1: pushed line was not a well-formed HerdrEvent");
    drop(first);

    let mut second = client
        .subscribe(busy_subscriptions())
        .expect("events.subscribe #2 on the same busy pane: ack must match");
    second
        .next()
        .expect("events.subscribe #2: no event arrived within the read timeout")
        .expect("events.subscribe #2: pushed line was not a well-formed HerdrEvent");
    drop(second);

    // A third, after the session changes under it (`pane.split`).
    let pane_c: PaneInfo = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id.clone()),
            target_pane_id: Some(pane_b.pane_id.clone()),
            direction: SplitDirection::Down,
            cwd: workspace_dir.path().to_path_buf(),
            env: BTreeMap::new(),
        })
        .expect("pane.split (a third pane, between subscriptions)");
    let mut third = client
        .subscribe(busy_subscriptions())
        .expect("events.subscribe #3, after a pane.split: ack must match");
    third
        .next()
        .expect("events.subscribe #3: no event arrived within the read timeout")
        .expect("events.subscribe #3: pushed line was not a well-formed HerdrEvent");
    drop(third);

    // agent.start: the genuine launch-order call, on pane_a. Expected
    // to succeed (a real Claude agent starts) — asserted loosely
    // (success or a recognized business error, e.g. a box with no
    // `claude` binary on PATH) so this sweep still reports cleanly on
    // a session where that precondition differs, without ever masking
    // a schema-shape defect as a false pass.
    assert_ok_or_business_error(
        "agent.start",
        client.start_agent(StartAgent {
            pane_id: pane_a.pane_id.clone(),
            kind: "claude".to_string(),
            name: "wirk-live-sweep".to_string(),
            args: vec!["--model".to_string(), "sonnet".to_string()],
            timeout_ms: Some(10_000),
        }),
    );

    // ---- the rest of the trait ---------------------------------------

    assert_ok_or_business_error(
        "workspace.report_metadata",
        client.report_metadata(ReportMetadata {
            pane_id: None,
            workspace_id: Some(ws.workspace_id.clone()),
            source: "wirk-live-sweep".to_string(),
            tokens: Some(json!({"input": 1, "output": 1})),
            title: None,
        }),
    );
    assert_ok_or_business_error(
        "pane.report_metadata",
        client.report_metadata(ReportMetadata {
            pane_id: Some(pane_b.pane_id.clone()),
            workspace_id: None,
            source: "wirk-live-sweep".to_string(),
            tokens: None,
            title: Some("wirk live sweep".to_string()),
        }),
    );
    assert_ok_or_business_error(
        "notification.show",
        client.notify(Notify {
            title: "wirk live sweep".to_string(),
            body: "exercising every method (fix 2)".to_string(),
        }),
    );
    assert_ok_or_business_error(
        "pane.focus",
        client.focus_pane(FocusPane {
            pane_id: pane_a.pane_id.clone(),
        }),
    );

    // tab.create: real, schema-defined, no HerdrClient wrapper — sent
    // raw (module doc comment).
    let tab_reply = raw_call(
        &socket_path,
        "tab.create",
        json!({"workspace_id": ws.workspace_id}),
    );
    assert_raw_ok_or_business_error("tab.create", &tab_reply);

    // worktree.open / worktree.remove, on a fresh git repo tempdir.
    let repo_dir = tempdir().expect("worktree repo tempdir");
    git(repo_dir.path(), &["init", "-q", "-b", "main"]);
    git(
        repo_dir.path(),
        &["config", "user.email", "wirk-live-sweep@example.com"],
    );
    git(repo_dir.path(), &["config", "user.name", "wirk live sweep"]);
    std::fs::write(repo_dir.path().join("a.txt"), "one\n").expect("write a.txt");
    git(repo_dir.path(), &["add", "a.txt"]);
    git(repo_dir.path(), &["commit", "-q", "-m", "first"]);
    assert_ok_or_business_error(
        "worktree.open",
        client.open_worktree(OpenWorktree {
            path: repo_dir.path().to_path_buf(),
            workspace_id: Some(ws.workspace_id.clone()),
        }),
    );
    assert_ok_or_business_error(
        "worktree.remove",
        client.remove_worktree(RemoveWorktree {
            workspace_id: ws.workspace_id.clone(),
            force: Some(true),
        }),
    );

    // agent.prompt / agent.wait / agent.send_keys / pane.release_agent
    // / pane.report_agent / pane.report_agent_session, against pane_b
    // — never given `agent.start`, so genuinely "a pane with no
    // agent". Each must come back a well-formed success or a business
    // error with a code (`agent_not_found`/`agent_not_ready` are the
    // schema's own names for exactly this precondition) — never a
    // transport error or a raw `invalid_request` (fix 2's own
    // finding).
    assert_ok_or_business_error(
        "agent.prompt",
        client.prompt_agent(PromptAgent {
            target: pane_b.pane_id.clone(),
            text: "wirk-live-sweep: this pane has no agent".to_string(),
        }),
    );
    assert_ok_or_business_error(
        "agent.wait",
        client.wait_agent(&pane_b.pane_id, AgentStatus::Working, 1_000),
    );
    assert_ok_or_business_error(
        "agent.send_keys",
        client.send_keys(SendKeys {
            target: pane_b.pane_id.clone(),
            keys: vec!["Enter".to_string()],
        }),
    );
    assert_ok_or_business_error(
        "pane.release_agent",
        client.release_agent(ReleaseAgent {
            pane_id: pane_b.pane_id.clone(),
            agent: "claude".to_string(),
            source: Some("wirk-live-sweep".to_string()),
        }),
    );
    assert_ok_or_business_error(
        "pane.report_agent",
        client.report_agent(ReportAgent {
            pane_id: pane_b.pane_id.clone(),
            source: "wirk-live-sweep".to_string(),
            agent: "claude".to_string(),
            state: "working".to_string(),
            seq: Some(1),
        }),
    );
    assert_ok_or_business_error(
        "pane.report_agent_session",
        client.report_agent_session(ReportAgentSession {
            pane_id: pane_b.pane_id.clone(),
            source: "wirk-live-sweep".to_string(),
            agent: "claude".to_string(),
            agent_session_id: Some("wirk-live-sweep-session".to_string()),
            session_start_source: Some("wirk-live-sweep".to_string()),
            seq: Some(1),
        }),
    );

    // ---- teardown: close both panes, then the workspace --------------

    assert_ok_or_business_error("pane.close (a)", client.close_pane(&pane_a.pane_id));
    assert_ok_or_business_error("pane.close (b)", client.close_pane(&pane_b.pane_id));
    assert_ok_or_business_error("pane.close (c)", client.close_pane(&pane_c.pane_id));
    client
        .close_workspace(CloseWorkspace {
            workspace_id: ws.workspace_id,
        })
        .expect("workspace.close");
}
