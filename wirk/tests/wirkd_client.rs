//! Fake-server tests for the wirkd wire protocol and client (W2, issue
//! 287; `orient/transport.md` §2, §4). No real wirkd process runs here
//! — a scripted fake server on a `UnixListener` stands in for it, so
//! this proves `client::call`/`client::locate` against the envelope
//! shape without needing W3's listener loop.
//!
//! `wirk` has no `lib.rs` (bin-only, `wirk/tests/claim.rs` and
//! `journal_demo.rs` both drive the built binary as a subprocess
//! instead); the wire types this test needs to call directly —
//! `Request`, `Reply`, `client::call`, `client::locate` — are compiled
//! into *this* test binary's own crate root via `#[path]`, the
//! ordinary way to unit-test a bin crate's internals without adding a
//! library target purely for tests.

#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::thread::{self, JoinHandle};

use wirkd::client::{self, ClientError};
use wirkd::{Reply, Request, Verb, WirkdPointer};

/// Binds a `UnixListener` at `socket_path` (so the socket exists and is
/// already accepting connections before this returns — a client can
/// dial immediately, no poll or sleep needed for readiness), then
/// spawns a thread that accepts exactly one connection, reads one
/// NDJSON request line, writes `scripted_reply` followed by `\n`, and
/// returns the request line it read (trimmed) for the caller to
/// inspect.
fn spawn_fake_server(socket_path: &Path, scripted_reply: &'static str) -> JoinHandle<String> {
    let listener = UnixListener::bind(socket_path).expect("bind fake wirkd socket");
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept one connection");
        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .expect("read one request line");

        let mut writer = &stream;
        writer
            .write_all(scripted_reply.as_bytes())
            .expect("write scripted reply");
        writer.write_all(b"\n").expect("write trailing newline");

        request_line.trim_end_matches(['\n', '\r']).to_string()
    })
}

#[test]
fn ping_round_trips_through_the_fake_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("wirkd.sock");
    let scripted = r#"{"ok":true,"result":{"protocol_version":1,"pid":4821}}"#;
    let handle = spawn_fake_server(&socket_path, scripted);

    let reply = client::call(&socket_path, &Request::ping()).expect("call succeeds");
    match reply {
        Reply::Ok { ok, result } => {
            assert!(ok);
            assert_eq!(result["protocol_version"], 1);
            assert_eq!(result["pid"], 4821);
        }
        Reply::Err { .. } => panic!("expected an ok reply, got an error reply"),
    }

    let request_line = handle.join().expect("fake server thread");
    let parsed: Request = serde_json::from_str(&request_line).expect("request line is valid JSON");
    assert_eq!(parsed.verb, Verb::Ping);
    assert_eq!(parsed.payload, serde_json::json!({}));
}

#[test]
fn an_ok_false_reply_surfaces_as_the_error_variant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("wirkd.sock");
    let scripted = r#"{"ok":false,"error":{"code":"MissingArtifact","message":"report.md"}}"#;
    let _handle = spawn_fake_server(&socket_path, scripted);

    let reply = client::call(&socket_path, &Request::ping())
        .expect("the transport succeeds; the refusal is in the reply, not a transport error");
    match reply {
        Reply::Err { ok, error } => {
            assert!(!ok);
            assert_eq!(error.code, "MissingArtifact");
            assert_eq!(error.message, "report.md");
            assert!(error.detail.is_none());
        }
        Reply::Ok { .. } => panic!("expected an error reply, got an ok reply"),
    }
}

#[test]
fn a_malformed_reply_line_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("wirkd.sock");
    let scripted = "not json at all";
    let _handle = spawn_fake_server(&socket_path, scripted);

    let result = client::call(&socket_path, &Request::ping());
    match result {
        Err(ClientError::MalformedReply(reason)) => {
            assert!(reason.contains("not json at all"));
        }
        other => panic!("expected ClientError::MalformedReply, got {other:?}"),
    }
}

#[test]
fn a_missing_pointer_file_is_a_distinct_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    // No `.wirk/wirkd.json` written under `dir` — wirkd was never
    // started here.
    let result = client::locate(dir.path());
    match result {
        Err(ClientError::PointerNotFound(path)) => {
            assert_eq!(path, dir.path().join(".wirk").join("wirkd.json"));
        }
        other => panic!("expected ClientError::PointerNotFound, got {other:?}"),
    }
}

#[test]
fn locate_reads_a_pointer_file_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let wirk_dir = dir.path().join(".wirk");
    std::fs::create_dir_all(&wirk_dir).expect("create .wirk dir");
    let pointer = WirkdPointer {
        schema: "wirkd.pointer/v1".to_string(),
        socket: wirk_dir.join("wirkd.sock"),
        pid: 4821,
        protocol_version: 1,
    };
    std::fs::write(
        wirk_dir.join("wirkd.json"),
        serde_json::to_vec(&pointer).expect("serialize pointer"),
    )
    .expect("write pointer file");

    let located = client::locate(dir.path()).expect("locate succeeds");
    assert_eq!(located.schema, "wirkd.pointer/v1");
    assert_eq!(located.socket, wirk_dir.join("wirkd.sock"));
    assert_eq!(located.pid, 4821);
    assert_eq!(located.protocol_version, 1);
}

/// Golden test (BRIEF outcome): the serialised `Request` and `Reply`
/// for `ping` match a literal expected string — pins the wire shape
/// itself, not just that round-tripping works.
#[test]
fn ping_request_and_reply_match_the_golden_envelope() {
    let request_json = serde_json::to_string(&Request::ping()).expect("serialize request");
    assert_eq!(request_json, r#"{"verb":"ping","payload":{}}"#);

    // `Reply::Ok.result` is a `serde_json::Value`; without the
    // `preserve_order` feature (not adopted, R1: nothing else needs
    // ordered JSON maps) its object keys serialize in `BTreeMap`
    // order, alphabetical here — the golden string reflects that, not
    // request-line order.
    let reply: Reply =
        serde_json::from_str(r#"{"ok":true,"result":{"protocol_version":1,"pid":4821}}"#)
            .expect("parse golden reply");
    let reply_json = serde_json::to_string(&reply).expect("serialize reply");
    assert_eq!(
        reply_json,
        r#"{"ok":true,"result":{"pid":4821,"protocol_version":1}}"#
    );
}
