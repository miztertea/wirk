//! Contract tests for `SocketClient` against a scripted fake socket
//! server (item 4, W1; `client.md` §4; revised for the live-server fix,
//! 0028 D93, "tried step 2026-09-04"). A `std::os::unix::net::
//! UnixListener` on a tempdir path, in a background thread, accepts a
//! connection, reads one NDJSON request line, writes back one scripted
//! reply keyed by method name, and closes the connection — matching
//! live Herdr 0.8.2's own per-connection handler, which dispatches
//! exactly one non-subscription request per connection and returns
//! (`refs/herdr` `0f8ad12` `src/api/server.rs:274-301`;
//! `socket-api.mdx:668`'s "Event subscriptions keep the connection open
//! after the initial response" names subscriptions as the one
//! exception). Modeled on `wirk-herdr`'s own in-process `fake.rs`
//! (moved to a real socket) and on Herdr's own test pattern for the
//! identical shape (a listener thread reading one line via
//! `BufReader::read_line`, writing back a literal reply, then
//! returning). No live Herdr; no sleep anywhere as a wait (issue 359) —
//! `UnixListener::bind` is synchronous, so the socket is already
//! connectable (kernel backlog queues the connection) the moment
//! `spawn_fake_server` returns, no readiness poll needed; the one
//! genuine wait, the read-timeout probe, is bounded by `SocketClient`'s
//! own configured timeout, not a test-side sleep.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wirk_herdr::{AgentStatus, HerdrClient, HerdrError, HerdrEvent, SocketClient};

/// Starts a fake Herdr socket server on a tempdir path, scripted by
/// `handler`: called once per accepted connection with that
/// connection's `BufReader`/`UnixStream` pair, each on its **own**
/// thread — `SocketClient` now dials a fresh connection per `call()`
/// plus the probe connection `connect()` dials and drops, so a single
/// test can open several connections to this fake in sequence (or, for
/// `subscribe()`, one long-lived one alongside them); a sequential
/// single-handler-thread accept loop would serialize those and could
/// deadlock a test that holds one connection open while dialing
/// another. Tests are short-lived; no teardown beyond the tempdir's own
/// `Drop`, matching `fake.rs`'s "no teardown needed for a process-local
/// fake" posture.
fn spawn_fake_server<F>(dir: &tempfile::TempDir, handler: F) -> PathBuf
where
    F: Fn(&mut BufReader<UnixStream>, &mut UnixStream) + Send + Sync + 'static,
{
    let socket_path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind fake herdr socket");
    let handler = std::sync::Arc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let handler = handler.clone();
            std::thread::spawn(move || {
                let mut writer = stream.try_clone().expect("clone stream");
                let mut reader = BufReader::new(stream);
                handler(&mut reader, &mut writer);
            });
        }
    });
    socket_path
}

fn read_request(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read request line");
    serde_json::from_str(line.trim_end()).expect("request line is valid JSON")
}

fn write_reply(writer: &mut UnixStream, id: &str, result: serde_json::Value) {
    let reply = serde_json::json!({"id": id, "result": result});
    let mut line = serde_json::to_string(&reply).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).unwrap();
    writer.flush().unwrap();
}

fn write_error(writer: &mut UnixStream, id: &str, code: &str, message: &str) {
    let reply = serde_json::json!({"id": id, "error": {"code": code, "message": message}});
    let mut line = serde_json::to_string(&reply).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).unwrap();
    writer.flush().unwrap();
}

fn sample_pane_json(pane_id: &str) -> serde_json::Value {
    serde_json::json!({
        "pane_id": pane_id,
        "terminal_id": "term1",
        "workspace_id": "ws1",
        "tab_id": "tab1",
        "focused": true,
        "agent_status": "working",
        "revision": 1,
    })
}

// ---- ping round trip --------------------------------------------------

#[test]
fn ping_round_trips_through_the_fake_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        assert_eq!(req["method"], "ping");
        write_reply(
            writer,
            req["id"].as_str().unwrap(),
            serde_json::json!({"type": "pong", "version": "0.8.2", "protocol": 20}),
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    client.ping().expect("ping succeeds");
}

// ---- error reply maps to the right HerdrError --------------------------

#[test]
fn agent_not_ready_maps_to_blocked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        write_error(
            writer,
            req["id"].as_str().unwrap(),
            "agent_not_ready",
            "claude is not ready yet",
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let err = client.get_agent("pane1").expect_err("agent not ready");
    assert!(
        matches!(err, HerdrError::Blocked(ref m) if m.contains("not ready")),
        "expected Blocked, got {err:?}"
    );
}

#[test]
fn pane_not_found_maps_to_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        write_error(
            writer,
            req["id"].as_str().unwrap(),
            "pane_not_found",
            "no such pane",
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let err = client.get_pane("gone").expect_err("pane not found");
    assert!(
        matches!(err, HerdrError::NotFound(ref m) if m.contains("no such pane")),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn an_unlisted_code_maps_to_invalid_not_transport() {
    // Fix 2: a well-formed `{"error":{code,message}}` reply is always a
    // business error the server parsed and answered — never a
    // transport fault, whatever the code (0028 tried step 2's second
    // finding: `pane.split` schema rejections were misreported as a
    // `Transport` id mismatch before this fix; `Transport` now stays
    // reserved for socket/io/framing failures with no such reply).
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        write_error(
            writer,
            req["id"].as_str().unwrap(),
            "invalid_params",
            "bad shape",
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let err = client.get_pane("pane1").expect_err("invalid_params");
    assert!(
        matches!(err, HerdrError::Invalid(ref m) if m.contains("invalid_params")),
        "expected Invalid, got {err:?}"
    );
}

#[test]
fn an_error_reply_with_a_mismatched_id_maps_to_invalid_not_a_transport_id_mismatch() {
    // The live-finding case itself (0028 tried step 2, RESULT.md):
    // a schema-rejected request never gets far enough server-side to
    // read the request's own `id`, so the reply comes back
    // `{"id":"","error":{...}}` — an empty id, mismatched against
    // whatever id the client sent. Before this fix, `call()` checked
    // `id` before `error` and reported this as a confusing transport
    // id-mismatch, discarding the real code/message. Now the error is
    // checked first, so this well-formed business error surfaces as
    // its own `Invalid` variant, id mismatch and all.
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let _req = read_request(reader);
        // ignore the request's real id; use an empty one, matching the
        // live server's own behavior on a schema-rejected request.
        write_error(
            writer,
            "",
            "invalid_request",
            "unknown variant `horizontal`",
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let err = client.get_pane("pane1").expect_err("invalid_request");
    assert!(
        matches!(err, HerdrError::Invalid(ref m) if m.contains("invalid_request") && m.contains("unknown variant")),
        "expected Invalid carrying the server's message, got {err:?}"
    );
}

// ---- subscribe stream ---------------------------------------------------

#[test]
fn subscribe_delivers_two_events_then_ends_when_the_server_closes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        assert_eq!(req["method"], "events.subscribe");
        write_reply(
            writer,
            req["id"].as_str().unwrap(),
            serde_json::json!({"type": "subscription_started"}),
        );

        let ev1 = serde_json::json!({
            "event": "pane_agent_status_changed",
            "data": {
                "type": "pane_agent_status_changed",
                "pane_id": "pane1",
                "workspace_id": "ws1",
                "agent_status": "working",
            },
        });
        let ev2 = serde_json::json!({
            "event": "pane_agent_status_changed",
            "data": {
                "type": "pane_agent_status_changed",
                "pane_id": "pane1",
                "workspace_id": "ws1",
                "agent_status": "blocked",
            },
        });
        for ev in [ev1, ev2] {
            let mut line = serde_json::to_string(&ev).unwrap();
            line.push('\n');
            writer.write_all(line.as_bytes()).unwrap();
            writer.flush().unwrap();
        }
        // Connection closes when this handler returns (writer dropped).
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let mut events = client
        .subscribe(vec![
            wirk_herdr::EventSubscription::PaneAgentStatusChanged {
                pane_id: "pane1".to_string(),
            },
        ])
        .expect("subscribe");

    let first = events.next().expect("first event").expect("not an error");
    let second = events.next().expect("second event").expect("not an error");
    assert!(matches!(
        first,
        HerdrEvent::PaneAgentStatusChanged {
            agent_status: AgentStatus::Working,
            ..
        }
    ));
    assert!(matches!(
        second,
        HerdrEvent::PaneAgentStatusChanged {
            agent_status: AgentStatus::Blocked,
            ..
        }
    ));
    assert!(events.next().is_none(), "iterator ends on server close");
}

// ---- malformed line -----------------------------------------------------

#[test]
fn a_malformed_pushed_line_is_transport_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        assert_eq!(req["method"], "events.subscribe");
        write_reply(
            writer,
            req["id"].as_str().unwrap(),
            serde_json::json!({"type": "subscription_started"}),
        );
        writer.write_all(b"not json at all\n").unwrap();
        writer.flush().unwrap();
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let mut events = client
        .subscribe(vec![
            wirk_herdr::EventSubscription::PaneAgentStatusChanged {
                pane_id: "pane1".to_string(),
            },
        ])
        .expect("subscribe");

    let first = events.next().expect("one item, the error");
    assert!(
        matches!(first, Err(HerdrError::Transport(_))),
        "expected Err(Transport), got {first:?}"
    );
}

// ---- no read timeout (ruling 0044, fix 2) ---------------------------------
//
// `SocketClient` sets no read timeout anywhere any more (D134: "wirk
// blocks on state... not a read timeout"): a request read blocks until
// the reply arrives or the connection closes, for however long that
// takes. This test proves the "blocks, does not time out early" half
// directly — a fake server that replies only after a real, deliberate
// delay of its own (the thing under test, not a wait in the test,
// mirroring item G's `sleep 2; exit 0` shape) still gets its reply, well
// past what the old 200ms/30s timeouts would ever have tolerated.

#[test]
fn a_request_blocks_until_the_server_actually_replies_no_matter_how_long() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reply_delay = Duration::from_millis(500);
    let socket_path = spawn_fake_server(&dir, move |reader, writer| {
        let request = read_request(reader);
        std::thread::sleep(reply_delay);
        let id = request["id"].as_str().unwrap_or_default();
        write_reply(
            writer,
            id,
            serde_json::json!({"type": "pane_info", "pane": sample_pane_json("pane1")}),
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let started = Instant::now();
    let pane = client.get_pane("pane1").expect("blocks, then succeeds");
    assert_eq!(pane.pane_id, "pane1");
    assert!(
        started.elapsed() >= reply_delay,
        "the call returned before the server's own delay elapsed: {:?} < {reply_delay:?}",
        started.elapsed()
    );
}

// ---- two consecutive requests against a one-reply-per-connection fake --
//
// This is the test that would have failed before this fix: the old
// `SocketClient` held one request connection for the process and
// reused it for every `call()`; a fake server that closes after one
// reply (as live Herdr 0.8.2 does, module doc comment) would have
// broken the second `get_pane` below with a broken-pipe `Transport`
// error, exactly as `tried/RESULT.md`'s live probe found. The fix
// dials fresh per call, so both requests land on their own connection
// and both succeed.

#[test]
fn two_consecutive_requests_both_succeed_against_a_one_reply_fake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        // One request, one reply, then this handler returns and the
        // connection closes — never a second line read on it.
        let req = read_request(reader);
        assert_eq!(req["method"], "pane.get");
        let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
        write_reply(
            writer,
            req["id"].as_str().unwrap(),
            serde_json::json!({"type": "pane_info", "pane": sample_pane_json(&pane_id)}),
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let first = client.get_pane("pane1").expect("first get_pane succeeds");
    assert_eq!(first.pane_id, "pane1");
    let second = client.get_pane("pane2").expect("second get_pane succeeds");
    assert_eq!(second.pane_id, "pane2");
}

// ---- id mismatch is a protocol violation, not silently accepted -------

#[test]
fn get_pane_extracts_pane_info_on_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        assert_eq!(req["method"], "pane.get");
        assert_eq!(req["params"]["pane_id"], "pane1");
        write_reply(
            writer,
            req["id"].as_str().unwrap(),
            serde_json::json!({"type": "pane_info", "pane": sample_pane_json("pane1")}),
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let pane = client.get_pane("pane1").expect("get_pane succeeds");
    assert_eq!(pane.pane_id, "pane1");
    assert_eq!(pane.agent_status, AgentStatus::Working);
}

// ---- subscriptions: ids, acks, and the two pushed envelopes -----------
//
// All three of these pin what tried step 3 found live (0028 D93;
// `knowledge/work/p1-herdr-executor/tried/RESULT.md`), against a fake
// that answers subscriptions the way `refs/herdr` `0f8ad12` does:
// `server.rs:722-733` acks with `SuccessResponse { id: request_id,
// result: subscription_started }` — the request's own id, verbatim —
// and `server.rs:709-717` answers a subscription whose setup failed
// with an `ErrorResponse` (then closes), carrying the *probe's* id,
// `"<request id>:sub:<index>:probe"`, when the failure came from the
// internal `pane.get`/`pane.read` probe a per-pane subscription is
// built with (`subscriptions.rs:186,207,238`).

/// Two subscriptions opened in sequence get **distinct** request ids
/// from the one counter, and each ack echoes its own id back (fix 3).
/// Before it, `subscribe_impl` sent the fixed literal `"wirk-sub-1"`
/// every time, so this fake — which asserts the ack it writes matches
/// the id it was sent, and that the two ids differ — could not tell
/// the two subscriptions apart at all.
#[test]
fn two_subscriptions_in_sequence_get_distinct_ids_and_both_acks_match() {
    use std::sync::Mutex;

    let dir = tempfile::tempdir().expect("tempdir");
    let seen_ids: std::sync::Arc<Mutex<Vec<String>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
    let recorder = std::sync::Arc::clone(&seen_ids);
    let socket_path = spawn_fake_server(&dir, move |reader, writer| {
        let req = read_request(reader);
        assert_eq!(req["method"], "events.subscribe");
        let id = req["id"].as_str().expect("a subscribe id").to_string();
        recorder.lock().unwrap().push(id.clone());
        // The live server's ack: the request's own id, verbatim.
        write_reply(
            writer,
            &id,
            serde_json::json!({"type": "subscription_started"}),
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let first = client
        .subscribe(vec![
            wirk_herdr::EventSubscription::PaneAgentStatusChanged {
                pane_id: "pane1".to_string(),
            },
        ])
        .expect("first subscribe acked");
    let second = client
        .subscribe(vec![wirk_herdr::EventSubscription::PaneUpdated {
            pane_id: "pane1".to_string(),
        }])
        .expect("second subscribe acked");
    // Both streams end when their handler returns; draining them is how
    // this test waits for both connections to have been served, with no
    // sleep (issue 359).
    assert!(first.count() == 0 && second.count() == 0);

    let ids = seen_ids.lock().unwrap().clone();
    assert_eq!(ids.len(), 2, "both subscribes reached the server: {ids:?}");
    assert_ne!(
        ids[0], ids[1],
        "two subscriptions must carry distinct request ids, got {ids:?}"
    );
}

/// A subscription whose server-side setup probe failed comes back as an
/// error response carrying the probe's id — the exact line tried step 3
/// crashed on (`{"id":"wirk-sub-1:sub:0:probe", ...}`). It must surface
/// as the server's own business error, naming the failing subscription,
/// never as a bogus "ack id does not match" transport fault.
#[test]
fn a_setup_probe_error_surfaces_the_servers_code_not_an_id_mismatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        assert_eq!(req["method"], "events.subscribe");
        let id = req["id"].as_str().expect("a subscribe id").to_string();
        // `ActiveSubscription::new` probed the pane and got
        // `pane_not_found`; `stream_subscriptions` writes that error
        // response and closes the stream.
        write_error(
            writer,
            &format!("{id}:sub:0:probe"),
            "pane_not_found",
            "pane not found",
        );
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    // `Box<dyn Iterator>` is not `Debug`, so the error is taken by
    // `match`, not `expect_err`.
    let Err(err) = client.subscribe(vec![
        wirk_herdr::EventSubscription::PaneAgentStatusChanged {
            pane_id: "not-a-pane-id".to_string(),
        },
    ]) else {
        panic!("a failed setup probe must not read as a subscription");
    };
    match err {
        HerdrError::NotFound(message) => {
            assert!(
                message.contains("pane not found"),
                "the server's own message must survive: {message:?}"
            );
            assert!(
                message.contains("subscription 0") && message.contains("pane.agent_status_changed"),
                "the failing subscription must be named: {message:?}"
            );
        }
        other => panic!("expected NotFound carrying the server's message, got {other:?}"),
    }
}

/// A per-pane subscription's pushed events arrive in the
/// `SubscriptionEventEnvelope` shape: dotted kind in `event`, `data`
/// untagged (fixture `herdr-schema-0.8.2-p20.json`, schema
/// `subscription_event`; `refs/herdr` `0f8ad12`
/// `src/api/schema/events.rs:377-389`). Decoding `data` alone — what
/// this client did before fix 3 — fails on every one of them, since
/// `HerdrEvent` is tagged on a `"type"` the envelope does not carry.
#[test]
fn a_subscription_event_envelope_without_a_type_tag_parses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = spawn_fake_server(&dir, |reader, writer| {
        let req = read_request(reader);
        assert_eq!(req["method"], "events.subscribe");
        write_reply(
            writer,
            req["id"].as_str().unwrap(),
            serde_json::json!({"type": "subscription_started"}),
        );
        let ev = serde_json::json!({
            "event": "pane.agent_status_changed",
            "data": {
                "pane_id": "pane1",
                "workspace_id": "ws1",
                "agent_status": "blocked",
            },
        });
        let mut line = serde_json::to_string(&ev).unwrap();
        line.push('\n');
        writer.write_all(line.as_bytes()).unwrap();
        writer.flush().unwrap();
    });

    let client = SocketClient::connect(socket_path).expect("connect");
    let mut events = client
        .subscribe(vec![
            wirk_herdr::EventSubscription::PaneAgentStatusChanged {
                pane_id: "pane1".to_string(),
            },
        ])
        .expect("subscribe");
    let first = events
        .next()
        .expect("one pushed event")
        .expect("the untagged subscription envelope must decode");
    assert!(
        matches!(
            first,
            HerdrEvent::PaneAgentStatusChanged {
                agent_status: AgentStatus::Blocked,
                ..
            }
        ),
        "got {first:?}"
    );
}
