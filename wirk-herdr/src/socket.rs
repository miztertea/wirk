//! `SocketClient`: a live `HerdrClient` over Herdr's Unix domain socket
//! (item 4, W1; `knowledge/work/p1-herdr-executor/orient/client.md`).
//!
//! Framing is NDJSON: one `{"id","method","params"}` line out, one
//! `{"id","result"}`/`{"id","error":{code,message}}` line back
//! (`socket-api.mdx:653-663`, vendored
//! `tests/fixtures/herdr-schema-0.8.2-p20.json`). Two connection kinds,
//! revised by the tried step's live finding (0028 D93; `knowledge/
//! work/p1-herdr-executor/tried/RESULT.md`): a live Herdr 0.8.2 server
//! closes a plain (non-subscription) request connection after exactly
//! one reply — the server's own per-connection handler dispatches one
//! request from `handle_connection_with_stop`'s non-streaming match arm
//! (`refs/herdr` `0f8ad12` `src/api/server.rs:274-301`), writes the one
//! reply, and returns, ending that connection; nothing loops the socket
//! back to read a second line. `socket-api.mdx:668` states the
//! exception explicitly: "Event subscriptions keep the connection open
//! after the initial response" — implying, and this fix now matches,
//! that anything else does not. So: one **fresh** connection per
//! `call()` (connect, write one line, read one reply, close — no
//! connection is held or reused across requests); one **subscription**
//! connection per `subscribe()` call, kept open as before, with its own
//! reader thread pushing decoded `HerdrEvent`s through an `mpsc`
//! channel.
//!
//! Request-struct field names are proven to conform to the vendored
//! schema's `request/$defs/<Method>Params` defs by
//! `tests/schema.rs`'s extension (the same `dummy_value`/`resolve_ref`
//! machinery already used for `HerdrEvent`, R2); this module is written
//! by hand against that same fixture (R1: the structs in `lib.rs` stay
//! plain — this module owns their wire shape, not a derive on them,
//! per this item's allow-list).
//!
//! `HerdrClient::*`'s results are **tagged** on the wire
//! (`success_response/$defs/ResponseResult`'s `oneOf`, keyed by a
//! `type` const) — e.g. `pane.split`'s reply is
//! `{"type":"pane_info","pane":{...}}`, not a bare `PaneInfo` — verified
//! against `refs/herdr` `0f8ad12`'s own `encode_success(id,
//! ResponseResult::...)` call sites (the schema fixture names the shape;
//! the pinned source names which call site produces it for each
//! method). `extract`/`expect_type` below unwrap that tag.
//!
//! **Subscriptions, per the server's own source** (fix 3, 0028 tried
//! step 3's finding). `refs/herdr` `0f8ad12`
//! `src/api/server.rs::stream_subscriptions` builds each requested
//! subscription first (`:699-720`) and only then writes the ack. So a
//! subscription connection's first line is one of exactly two things:
//!
//! * **the ack**, `{"id": <the request's own id>, "result":
//!   {"type":"subscription_started"}}` — `server.rs:722-733` writes
//!   `SuccessResponse { id: request_id, .. }`, the id **verbatim**,
//!   never suffixed. An ack id is therefore matched exactly, never by
//!   prefix.
//! * **a setup error**, written and the stream then closed
//!   (`server.rs:709-717`). Building a per-pane subscription
//!   (`pane.agent_status_changed`, `pane.output_matched`,
//!   `pane.scroll_changed`) first probes the pane with an internal
//!   `pane.get`/`pane.read` whose request id is
//!   `"{request_id}:sub:{index}:probe"` (`src/api/subscriptions.rs:186,
//!   207, 238`); when that probe fails, the `ErrorResponse` it carries
//!   — the *probe's* id, not the request's — is what reaches the
//!   client. That is precisely what tried step 3 saw
//!   (`{"id":"wirk-sub-1:sub:0:probe"}`), and it is an error reply, not
//!   a mangled ack: this client now reads the `error` field first (as
//!   `call` already did since fix 2) and surfaces the server's real
//!   `code`/`message`, naming the subscription index the probe belongs
//!   to.
//!
//! Pushed lines after the ack come in **two envelope shapes**, and the
//! outer `event` field is what distinguishes them (fixture
//! `herdr-schema-0.8.2-p20.json`, schemas `event` and
//! `subscription_event`):
//!
//! * a plain event kind (`pane.updated`, `pane.created`, …) arrives as
//!   `EventEnvelope`: `event` is the **underscored** kind
//!   (`"pane_updated"`) and `data` carries its own `"type"` tag.
//! * a per-pane derived subscription arrives as
//!   `SubscriptionEventEnvelope`: `event` is the **dotted** kind
//!   (`"pane.agent_status_changed"`) and `data` is untagged — the
//!   fixture's `SubscriptionEventData` is an `anyOf` of bare event
//!   structs with no `type` property at all
//!   (`src/api/schema/events.rs:377-389`, `#[serde(untagged)]`).
//!
//! `parse_pushed_event` handles both: a dotted `event` names the tag
//! `data` is missing, so it is supplied before decoding. Decoding a
//! `SubscriptionEventEnvelope` as if it were an `EventEnvelope` — what
//! this client did before fix 3 — fails on every
//! `pane.agent_status_changed` the run loop exists to read.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{
    AgentStatus, Bearing, CloseWorkspace, CreateWorkspace, EventSubscription, FocusPane,
    HerdrClient, HerdrError, HerdrEvent, Notify, OpenWorktree, PaneInfo, PromptAgent, ReleaseAgent,
    RemoveWorktree, ReportAgent, ReportAgentSession, ReportMetadata, SendKeys, Snapshot, SplitPane,
    StartAgent, WorkspaceInfo, WorktreeInfo,
};

/// Every wire `method` name `SocketClient` sends, `ping` and
/// `events.subscribe` included alongside the `HerdrClient` trait
/// verbs — the single source `tests/schema.rs`'s conformance test
/// checks itself against (`method_set_matches_the_conformance_test`),
/// so a new `self.call(...)`/`events.subscribe` site added without
/// updating this const fails that test rather than going uncovered
/// (this item's fix).
pub const METHODS: &[&str] = &[
    "ping",
    "workspace.create",
    "pane.split",
    "worktree.open",
    "worktree.remove",
    "pane.send_text",
    "agent.start",
    "agent.prompt",
    "agent.wait",
    "pane.get",
    "agent.get",
    "agent.list",
    "agent.send_keys",
    "pane.release_agent",
    "pane.close",
    "workspace.close",
    "session.snapshot",
    "pane.report_agent_session",
    "pane.report_agent",
    "pane.report_metadata",
    "workspace.report_metadata",
    "notification.show",
    "pane.focus",
    "events.subscribe",
];

/// One request connection's two halves: a write half and a `BufReader`
/// read half, split from one `UnixStream` via `try_clone` (R3 — the
/// same fd, two independent handles, std's documented way to get a
/// half-duplex pair without extra synchronization on read vs write).
/// Dialed fresh for every `call()` and every `subscribe()` — never
/// held across requests (module doc comment: the live server closes a
/// plain request connection after one reply).
struct RequestConn {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

/// A live `HerdrClient` over `std::os::unix::net::UnixStream`
/// (client.md §1–§2; R3, no async runtime — `HerdrClient: Send + Sync`
/// is deliberately synchronous, 0023 D83). Holds no connection itself:
/// `call()` dials, sends, reads, and drops one fresh `UnixStream` per
/// request (module doc comment).
pub struct SocketClient {
    socket_path: PathBuf,
    next_id: AtomicU64,
}

impl SocketClient {
    /// Validates the socket is dialable now (fail fast, matching the
    /// prior eager-connect behavior callers such as `wirk run` rely on
    /// for an immediate error), then drops that probe connection — it
    /// is never reused; every `call()` dials its own, per request.
    /// Ruling 0044 (fix 2): no read timeout is set on any connection
    /// this client dials — a request read blocks until the reply
    /// arrives or the connection closes; `EOF`/a read error *is* the
    /// transport failure, not a timeout standing in for one.
    pub fn connect(socket_path: PathBuf) -> Result<Self, HerdrError> {
        drop(dial(&socket_path)?);
        Ok(Self {
            socket_path,
            next_id: AtomicU64::new(1),
        })
    }

    /// The next request id, from the one monotonic counter every
    /// request on this client draws from — `events.subscribe` included
    /// (fix 3). Before this, `subscribe_impl` hardcoded the literal
    /// `"wirk-sub-1"` on every call, so two subscriptions in one run
    /// shared an id and the server's own log could not tell them apart
    /// (`tried/RESULT.md`, run 3: two `request_id="wirk-sub-1"` lines,
    /// 96 ms apart). Ids are per-client, not global: correlation is
    /// per-connection, and every connection this client opens is its
    /// own.
    fn next_request_id(&self) -> String {
        format!("wirk-{}", self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// `ping` has no `HerdrClient` trait row (it is a connectivity
    /// check, not an executor verb) but is the natural round-trip probe
    /// for this client, so it is a plain inherent method (R6).
    pub fn ping(&self) -> Result<(), HerdrError> {
        let result = self.call("ping", json!({}))?;
        expect_type(&result, "pong")
    }

    // ---- wire plumbing ----------------------------------------------

    /// Dials a fresh connection, writes one request line, reads one
    /// reply line, then drops the connection (module doc comment: the
    /// live server closes a plain request connection after one reply,
    /// so this client never tries to reuse one). io/serde errors fold
    /// to `Transport`; an `{"error":{code,message}}` reply — checked
    /// *before* the id, since a schema-rejected request's reply never
    /// echoes the request's own id (fix 2) — maps per `map_error` (§3);
    /// only a reply with no `error` field has its `id` matched against
    /// the request's, an id mismatch there folding to `Transport`.
    fn call<P: Serialize>(&self, method: &str, params: P) -> Result<Value, HerdrError> {
        let id = self.next_request_id();
        let params_value = serde_json::to_value(params)
            .map_err(|e| transport(format!("encoding {method} params: {e}")))?;
        let request = json!({"id": id, "method": method, "params": params_value});
        let mut line = serde_json::to_string(&request)
            .map_err(|e| transport(format!("encoding {method} request: {e}")))?;
        line.push('\n');

        let mut conn = dial(&self.socket_path)?;
        conn.writer
            .write_all(line.as_bytes())
            .map_err(|e| transport(format!("writing {method}: {e}")))?;
        conn.writer
            .flush()
            .map_err(|e| transport(format!("flushing {method}: {e}")))?;

        let mut raw = String::new();
        let n = conn
            .reader
            .read_line(&mut raw)
            .map_err(|e| transport(format!("reading {method} reply: {e}")))?;
        if n == 0 {
            return Err(transport(format!(
                "{method}: connection closed before a reply"
            )));
        }

        let reply: Value = serde_json::from_str(raw.trim_end())
            .map_err(|e| transport(format!("malformed {method} reply: {e}")))?;

        // Error before id (fix 2, 0028 tried step 2's second finding):
        // a live server that rejects the request body outright (e.g.
        // schema-invalid params) never gets far enough to read the
        // request's own `id`, and replies `{"id":"","error":{...}}` —
        // a well-formed, correctly-routed business error, not a
        // transport fault. Checking `id` first turned that into a
        // misleading "reply id does not match" transport error,
        // discarding the real `code`/`message` underneath
        // (`RunFailed`'s journaled detail showed only the id-mismatch
        // text). The id check below applies only to a reply with no
        // `error` field — a success reply, where a mismatched id *is*
        // a genuine transport-level correlation failure.
        if let Some(error) = reply.get("error") {
            let code = error.get("code").and_then(Value::as_str).unwrap_or("");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            return Err(map_error(code, message));
        }

        let reply_id = reply.get("id").and_then(Value::as_str).unwrap_or("");
        if reply_id != id {
            return Err(transport(format!(
                "{method}: reply id {reply_id:?} does not match request id {id:?}"
            )));
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| transport(format!("{method}: reply has neither result nor error")))
    }

    /// Opens a *new* connection (client.md §1: never shared with the
    /// request connection), sends `events.subscribe`, blocks for the
    /// ack via the same `{id,result}`/`{id,error}` decode `call` uses,
    /// then hands the connection to a reader thread and returns the
    /// receiving half as the iterator the trait requires.
    fn subscribe_impl(
        &self,
        subs: Vec<EventSubscription>,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>, HerdrError> {
        let conn = dial(&self.socket_path)?;
        let RequestConn {
            mut writer,
            mut reader,
        } = conn;

        // The dotted names, in the order the server will index them
        // (`server.rs:699` enumerates `params.subscriptions`), so a
        // setup-probe failure can be reported against the subscription
        // it actually belongs to.
        let labels: Vec<&'static str> = subs.iter().map(EventSubscription::as_str).collect();

        let id = self.next_request_id();
        let request = json!({
            "id": id,
            "method": "events.subscribe",
            "params": params::events_subscribe(&subs),
        });
        let mut line = serde_json::to_string(&request)
            .map_err(|e| transport(format!("encoding events.subscribe: {e}")))?;
        line.push('\n');
        writer
            .write_all(line.as_bytes())
            .map_err(|e| transport(format!("writing events.subscribe: {e}")))?;
        writer
            .flush()
            .map_err(|e| transport(format!("flushing events.subscribe: {e}")))?;

        let mut ack_raw = String::new();
        let n = reader
            .read_line(&mut ack_raw)
            .map_err(|e| transport(format!("reading events.subscribe ack: {e}")))?;
        if n == 0 {
            return Err(transport(
                "events.subscribe: connection closed before an ack",
            ));
        }
        let ack: Value = serde_json::from_str(ack_raw.trim_end())
            .map_err(|e| transport(format!("malformed events.subscribe ack: {e}")))?;
        let ack_id = ack.get("id").and_then(Value::as_str).unwrap_or("");

        // Error before id, the same order `call` uses (fix 2) and the
        // only order the server's own two first-line shapes admit
        // (module doc comment): a subscription whose setup fails is
        // answered with an `ErrorResponse` and the stream is closed
        // (`server.rs:709-717`), and when the failure came from a
        // per-pane subscription's setup probe that response carries the
        // *probe's* id, `"<request id>:sub:<index>:probe"`
        // (`subscriptions.rs:186,207,238`). Checking the id first turned
        // that into "ack id does not match", discarding the server's own
        // code and message — tried step 3's crash, whose real cause was
        // a `pane.get` probe on an id that was not a pane id at all.
        if let Some(error) = ack.get("error") {
            let code = error.get("code").and_then(Value::as_str).unwrap_or("");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // The probe id is used only to *name* which subscription
            // failed; it never makes an error into an accepted ack.
            let message = match subscription_probe_index(ack_id, &id) {
                Some(index) => {
                    let named = labels
                        .get(index)
                        .map(|name| format!(" ({name})"))
                        .unwrap_or_default();
                    format!("events.subscribe: subscription {index}{named} setup probe: {message}")
                }
                None => format!("events.subscribe: {message}"),
            };
            return Err(map_error(code, message));
        }

        // A success ack echoes the request id verbatim
        // (`server.rs:722-733` writes `SuccessResponse { id: request_id
        // }`), so this stays an exact match — no prefix or suffix
        // tolerance: the only ids the server ever derives from ours
        // belong to error responses, handled above.
        if ack_id != id {
            return Err(transport(format!(
                "events.subscribe: ack id {ack_id:?} does not match request id {id:?}"
            )));
        }
        // The ack itself is not surfaced to the caller (client.md §1: it
        // only confirms the subscription started); a missing "result" on
        // an otherwise-non-error reply is still a protocol violation.
        match ack.get("result").and_then(|r| r.get("type")) {
            Some(Value::String(ty)) if ty == "subscription_started" => {}
            _ => {
                return Err(transport(format!(
                    "events.subscribe: ack is not a subscription_started result: {ack}"
                )));
            }
        }

        // Ruling 0044 (fix 2): no read timeout on this connection — the
        // reader thread blocks on `read_line` until Herdr pushes a line
        // or the connection closes. `Ok(0)` (`EOF`) ends the iterator
        // with no further message: `RunLoop`'s own forwarding thread
        // (`run_loop.rs::spawn_herdr_reader`) reads that as the stream
        // ending and treats it as "Herdr is gone" — a closed stream
        // *is* the observation, never a case a timeout has to invent.
        let (tx, rx) = mpsc::channel::<Result<HerdrEvent, HerdrError>>();
        std::thread::spawn(move || {
            let mut raw = String::new();
            loop {
                raw.clear();
                match reader.read_line(&mut raw) {
                    Ok(0) => break, // EOF: iterator ends, no reconnect here (caller's job)
                    Ok(_) => {
                        let parsed = parse_pushed_event(raw.trim_end());
                        if tx.send(parsed).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(transport(format!("subscription read: {e}"))));
                        break;
                    }
                }
            }
        });
        Ok(Box::new(rx.into_iter()))
    }
}

/// Dials `socket_path`, splitting one `UnixStream` into an independent
/// write handle and a `BufReader`-wrapped read handle via `try_clone`
/// (R3: both share the underlying fd; std's documented pattern for a
/// half-duplex pair with no extra locking between read and write). No
/// read timeout is set (ruling 0044): every read on either half blocks
/// until data arrives or the peer closes the connection.
fn dial(socket_path: &Path) -> Result<RequestConn, HerdrError> {
    let writer = UnixStream::connect(socket_path)
        .map_err(|e| transport(format!("connecting to {}: {e}", socket_path.display())))?;
    let reader_stream = writer
        .try_clone()
        .map_err(|e| transport(format!("cloning socket handle: {e}")))?;
    Ok(RequestConn {
        writer,
        reader: BufReader::new(reader_stream),
    })
}

/// Herdr's own id for a subscription's setup probe,
/// `"<request id>:sub:<index>:probe"` (`refs/herdr` `0f8ad12`
/// `src/api/subscriptions.rs:186,207,238` — `format!("{request_id}:sub:
/// {index}:probe")`, built for the internal `pane.get`/`pane.read` a
/// per-pane subscription probes with before it is accepted). Returns
/// the subscription index when `ack_id` is that id for `request_id`.
/// Used *only* to name which subscription an error reply belongs to:
/// an ack is still matched exactly (`subscribe_impl`).
fn subscription_probe_index(ack_id: &str, request_id: &str) -> Option<usize> {
    ack_id
        .strip_prefix(request_id)?
        .strip_prefix(":sub:")?
        .strip_suffix(":probe")?
        .parse()
        .ok()
}

/// Unwraps a pushed line's `{"event": ..., "data": {...}}` envelope
/// (client.md §1: `HerdrEvent` deserializes from `data`, not the outer
/// envelope) and decodes `data` as a `HerdrEvent`.
///
/// Two envelopes arrive on a subscription connection and the outer
/// `event` field tells them apart (module doc comment; fixture
/// `herdr-schema-0.8.2-p20.json`, schemas `event` vs
/// `subscription_event`):
///
/// * `EventEnvelope` — `event` is the underscored kind and `data`
///   carries its own `"type"` tag, which `HerdrEvent`'s
///   `#[serde(tag = "type")]` reads directly.
/// * `SubscriptionEventEnvelope` — `event` is the **dotted** kind and
///   `data` is untagged (`SubscriptionEventData` is an `anyOf` of bare
///   structs; `src/api/schema/events.rs:383-389`). The dotted kind is
///   the tag `data` is missing, so it is supplied here, underscored, to
///   the same `HerdrEvent` decode. `pane.output_matched` and
///   `pane.scroll_changed` have no `HerdrEvent` variant (nothing in
///   wirk subscribes to them, R1) and so surface as a named decode
///   error rather than being silently dropped.
///
/// A malformed line, a missing `data`, or a `data` shape `HerdrEvent`
/// rejects all fold to `Transport` — never a panic (this item's
/// "malformed line" probe).
fn parse_pushed_event(line: &str) -> Result<HerdrEvent, HerdrError> {
    let envelope: Value =
        serde_json::from_str(line).map_err(|e| transport(format!("malformed pushed line: {e}")))?;
    let kind = envelope
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut data = envelope
        .get("data")
        .cloned()
        .ok_or_else(|| transport("pushed line has no \"data\" field"))?;
    if kind.contains('.')
        && let Some(object) = data.as_object_mut()
    {
        object
            .entry("type")
            .or_insert_with(|| Value::String(kind.replace('.', "_")));
    }
    serde_json::from_value(data).map_err(|e| transport(format!("malformed pushed event: {e}")))
}

/// The dotted wire `Subscription` object for one `EventSubscription`
/// (schema `request/$defs/Subscription`'s `oneOf`, keyed by `type`;
/// D51's dotted form). Only `pane.agent_status_changed`,
/// `pane.output_matched`, and `pane.scroll_changed` carry a `pane_id`
/// filter; every other variant is `{"type": "..."}` alone.
/// `pane.output_matched`'s schema also names `source`/`match` as
/// required — `EventSubscription::PaneOutputMatched` (landed in
/// `lib.rs`, out of this item's allow-list) carries neither; unused by
/// this item's own subscriptions (only `PaneAgentStatusChanged` is
/// sent, per `HerdrExecutor::launch`), noted rather than silently
/// fixed.
fn subscription_json(sub: &EventSubscription) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), json!(sub.as_str()));
    if let EventSubscription::PaneAgentStatusChanged { pane_id }
    | EventSubscription::PaneOutputMatched { pane_id }
    | EventSubscription::PaneScrollChanged { pane_id } = sub
    {
        obj.insert("pane_id".to_string(), json!(pane_id));
    }
    Value::Object(obj)
}

// ---- result-tag unwrapping ------------------------------------------

/// Confirms a tagged result's `"type"` equals `expected`, discarding
/// the rest — for verbs whose success carries no field the trait needs
/// back (`Result<(), HerdrError>` rows).
fn expect_type(result: &Value, expected: &str) -> Result<(), HerdrError> {
    let ty = result.get("type").and_then(Value::as_str).unwrap_or("");
    if ty != expected {
        return Err(transport(format!(
            "expected result type {expected:?}, got {ty:?}: {result}"
        )));
    }
    Ok(())
}

/// Confirms a tagged result's `"type"` equals `expected`, then decodes
/// its `field` into `T` (`success_response/$defs/ResponseResult`'s
/// `oneOf`, e.g. `{"type":"pane_info","pane":{...}}`).
fn extract<T: DeserializeOwned>(
    result: Value,
    expected: &str,
    field: &str,
) -> Result<T, HerdrError> {
    let ty = result
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ty != expected {
        return Err(transport(format!(
            "expected result type {expected:?}, got {ty:?}: {result}"
        )));
    }
    let inner = result
        .get(field)
        .cloned()
        .ok_or_else(|| transport(format!("result {expected:?} has no {field:?} field")))?;
    serde_json::from_value(inner)
        .map_err(|e| transport(format!("result {expected:?}.{field}: {e}")))
}

/// Error-code mapping, client.md §3: `pane_not_found`/`agent_not_found`/
/// `workspace_not_found` to `NotFound` (0027 `lib.rs`); `agent_not_ready`
/// to `Blocked` (D52); every other code — `invalid_request` foremost —
/// to `Invalid` (fix 2: a well-formed `{"error":{...}}` reply is always
/// a business error the server parsed and answered, never a transport
/// fault; `Transport` stays reserved for socket/io/framing failures
/// where no such reply exists at all — see `call`'s doc comment).
fn map_error(code: &str, message: String) -> HerdrError {
    match code {
        "pane_not_found" | "agent_not_found" | "workspace_not_found" => {
            HerdrError::NotFound(message)
        }
        "agent_not_ready" => HerdrError::Blocked(message),
        other => HerdrError::Invalid(format!("{other}: {message}")),
    }
}

fn transport(message: impl Into<String>) -> HerdrError {
    HerdrError::Transport(message.into())
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

// ---- Request params -----------------------------------------------------
//
// One function per method's `*Params` shape, kept separate from the
// `HerdrClient` impl below so `tests/schema.rs` can call each builder
// directly against a sample struct and compare its keys to the
// vendored fixture's `request/$defs/<Method>Params.required` list
// (client.md §4: "the same fixture-driven ... method for the request
// params the client sends"). Field names are copied by hand from the
// fixture (checked at W1 time); `tests/schema.rs::request_params_
// conform_to_the_schema` is what proves it, not this comment.

pub mod params {
    use super::{
        AgentStatus, CloseWorkspace, CreateWorkspace, OpenWorktree, PromptAgent, ReleaseAgent,
        RemoveWorktree, ReportAgent, ReportAgentSession, ReportMetadata, SendKeys, SplitPane,
        StartAgent, path_str,
    };
    use crate::{FocusPane, Notify};
    use serde_json::{Value, json};

    pub fn workspace_create(req: &CreateWorkspace) -> Value {
        json!({
            "cwd": path_str(&req.cwd),
            "env": req.env,
            "label": req.label,
        })
    }

    pub fn pane_split(req: &SplitPane) -> Value {
        json!({
            "workspace_id": req.workspace_id,
            "target_pane_id": req.target_pane_id,
            "direction": req.direction,
            "cwd": path_str(&req.cwd),
            "env": req.env,
        })
    }

    pub fn worktree_open(req: &OpenWorktree) -> Value {
        json!({
            "path": path_str(&req.path),
            "workspace_id": req.workspace_id,
        })
    }

    pub fn worktree_remove(req: &RemoveWorktree) -> Value {
        json!({
            "workspace_id": req.workspace_id,
            "force": req.force,
        })
    }

    pub fn pane_send_text(pane_id: &str, text: &str) -> Value {
        json!({"pane_id": pane_id, "text": text})
    }

    pub fn agent_start(req: &StartAgent) -> Value {
        json!({
            "pane_id": req.pane_id,
            "kind": req.kind,
            "name": req.name,
            "args": req.args,
            "timeout_ms": req.timeout_ms,
        })
    }

    pub fn agent_prompt(req: &PromptAgent) -> Value {
        json!({"target": req.target, "text": req.text})
    }

    pub fn agent_wait(target: &str, until: AgentStatus, timeout_ms: u64) -> Value {
        json!({
            "target": target,
            "until": [until],
            "timeout_ms": timeout_ms,
        })
    }

    pub fn pane_get(pane_id: &str) -> Value {
        json!({"pane_id": pane_id})
    }

    pub fn agent_get(target: &str) -> Value {
        json!({"target": target})
    }

    pub fn agent_list() -> Value {
        json!({})
    }

    pub fn agent_send_keys(req: &SendKeys) -> Value {
        json!({"target": req.target, "keys": req.keys})
    }

    pub fn pane_release_agent(req: &ReleaseAgent) -> Value {
        json!({
            "pane_id": req.pane_id,
            "agent": req.agent,
            "source": req.source,
        })
    }

    pub fn pane_close(pane_id: &str) -> Value {
        json!({"pane_id": pane_id})
    }

    /// The vendored fixture's `workspace.close` params is
    /// `WorkspaceTarget` (`{workspace_id}` alone); `refs/herdr`
    /// `0f8ad12`'s live handler type (`WorkspaceCloseParams`, which
    /// adds an optional `close_group`) has moved ahead of the fixture
    /// export (checked at W1 time, both pinned to the same commit) —
    /// conformance here follows the vendored fixture per this item's
    /// brief, not the live source; `close_group` defaults false
    /// server-side either way, matching this executor's
    /// one-workspace-per-Run use (D54).
    pub fn workspace_close(req: &CloseWorkspace) -> Value {
        json!({"workspace_id": req.workspace_id})
    }

    pub fn session_snapshot() -> Value {
        json!({})
    }

    pub fn pane_report_agent_session(req: &ReportAgentSession) -> Value {
        json!({
            "pane_id": req.pane_id,
            "source": req.source,
            "agent": req.agent,
            "agent_session_id": req.agent_session_id,
            "session_start_source": req.session_start_source,
            "seq": req.seq,
        })
    }

    pub fn pane_report_agent(req: &ReportAgent) -> Value {
        json!({
            "pane_id": req.pane_id,
            "source": req.source,
            "agent": req.agent,
            "state": req.state,
            "seq": req.seq,
        })
    }

    /// `ReportMetadata` (landed `lib.rs`, out of this item's
    /// allow-list) carries both `pane_id: Option<..>` and
    /// `workspace_id: Option<..>` for what are, on the wire, two
    /// distinct methods (`pane.report_metadata` vs
    /// `workspace.report_metadata`) — dispatch on which is set (J1,
    /// local/reversible: no ruling names a precedence, and the caller
    /// only ever sets one).
    pub fn pane_report_metadata(req: &ReportMetadata, pane_id: &str) -> Value {
        json!({
            "pane_id": pane_id,
            "source": req.source,
            "title": req.title,
            "tokens": req.tokens,
        })
    }

    pub fn workspace_report_metadata(req: &ReportMetadata, workspace_id: &str) -> Value {
        json!({
            "workspace_id": workspace_id,
            "source": req.source,
            "tokens": req.tokens.clone().unwrap_or(Value::Null),
        })
    }

    pub fn notification_show(req: &Notify) -> Value {
        json!({"title": req.title, "body": req.body})
    }

    pub fn pane_focus(req: &FocusPane) -> Value {
        json!({"pane_id": req.pane_id})
    }

    /// Builds `events.subscribe`'s params the same way every other
    /// verb does (a builder over the request struct, not JSON
    /// assembled inline in `subscribe_impl`) so
    /// `tests/schema.rs::request_params_conform_to_the_schema` covers
    /// it identically (this item's fix; R2).
    pub fn events_subscribe(subs: &[crate::EventSubscription]) -> Value {
        let subscriptions: Vec<Value> = subs.iter().map(super::subscription_json).collect();
        json!({"subscriptions": subscriptions})
    }
}

// ---- HerdrClient ------------------------------------------------------

impl HerdrClient for SocketClient {
    fn create_workspace(&self, req: CreateWorkspace) -> Result<WorkspaceInfo, HerdrError> {
        let result = self.call("workspace.create", params::workspace_create(&req))?;
        extract(result, "workspace_created", "workspace")
    }

    fn split_pane(&self, req: SplitPane) -> Result<PaneInfo, HerdrError> {
        let result = self.call("pane.split", params::pane_split(&req))?;
        extract(result, "pane_info", "pane")
    }

    fn open_worktree(&self, req: OpenWorktree) -> Result<WorktreeInfo, HerdrError> {
        let result = self.call("worktree.open", params::worktree_open(&req))?;
        extract(result, "worktree_opened", "worktree")
    }

    fn remove_worktree(&self, req: RemoveWorktree) -> Result<(), HerdrError> {
        let result = self.call("worktree.remove", params::worktree_remove(&req))?;
        expect_type(&result, "worktree_removed")
    }

    fn send_input(&self, pane_id: &str, text: &str) -> Result<(), HerdrError> {
        let result = self.call("pane.send_text", params::pane_send_text(pane_id, text))?;
        expect_type(&result, "ok")
    }

    fn start_agent(&self, req: StartAgent) -> Result<(), HerdrError> {
        let result = self.call("agent.start", params::agent_start(&req))?;
        expect_type(&result, "agent_started")
    }

    fn prompt_agent(&self, req: PromptAgent) -> Result<(), HerdrError> {
        let result = self.call("agent.prompt", params::agent_prompt(&req))?;
        expect_type(&result, "agent_prompted")
    }

    fn wait_agent(
        &self,
        target: &str,
        until: AgentStatus,
        timeout_ms: u64,
    ) -> Result<AgentStatus, HerdrError> {
        let result = self.call("agent.wait", params::agent_wait(target, until, timeout_ms))?;
        let pane: PaneInfo = extract(result, "agent_info", "agent")?;
        Ok(pane.agent_status)
    }

    fn get_pane(&self, pane_id: &str) -> Result<PaneInfo, HerdrError> {
        let result = self.call("pane.get", params::pane_get(pane_id))?;
        extract(result, "pane_info", "pane")
    }

    fn get_agent(&self, target: &str) -> Result<PaneInfo, HerdrError> {
        let result = self.call("agent.get", params::agent_get(target))?;
        // `agent.get`'s reply tags "agent_info" and carries an
        // `AgentInfo` whose required-field set is identical to
        // `PaneInfo`'s (both: pane_id, terminal_id, workspace_id,
        // tab_id, focused, agent_status, revision — vendored fixture,
        // checked at W1 time); reused rather than a second near-
        // duplicate type (R1).
        extract(result, "agent_info", "agent")
    }

    fn list_agents(&self) -> Result<Vec<PaneInfo>, HerdrError> {
        let result = self.call("agent.list", params::agent_list())?;
        extract(result, "agent_list", "agents")
    }

    fn send_keys(&self, req: SendKeys) -> Result<(), HerdrError> {
        let result = self.call("agent.send_keys", params::agent_send_keys(&req))?;
        expect_type(&result, "ok")
    }

    fn release_agent(&self, req: ReleaseAgent) -> Result<(), HerdrError> {
        let result = self.call("pane.release_agent", params::pane_release_agent(&req))?;
        expect_type(&result, "ok")
    }

    fn close_pane(&self, pane_id: &str) -> Result<(), HerdrError> {
        let result = self.call("pane.close", params::pane_close(pane_id))?;
        expect_type(&result, "ok")
    }

    fn close_workspace(&self, req: CloseWorkspace) -> Result<(), HerdrError> {
        let result = self.call("workspace.close", params::workspace_close(&req))?;
        expect_type(&result, "ok")
    }

    fn snapshot(&self) -> Result<Snapshot, HerdrError> {
        #[derive(serde::Deserialize)]
        struct SessionSnapshotPanes {
            panes: Vec<PaneInfo>,
        }
        let result = self.call("session.snapshot", params::session_snapshot())?;
        let snap: SessionSnapshotPanes = extract(result, "session_snapshot", "snapshot")?;
        let workspaces = snap
            .panes
            .into_iter()
            .map(|pane| Bearing {
                workspace_id: pane.workspace_id,
                tab_id: pane.tab_id,
                pane_id: pane.pane_id,
                terminal_id: pane.terminal_id,
            })
            .collect();
        Ok(Snapshot { workspaces })
    }

    fn report_agent_session(&self, req: ReportAgentSession) -> Result<(), HerdrError> {
        let result = self.call(
            "pane.report_agent_session",
            params::pane_report_agent_session(&req),
        )?;
        expect_type(&result, "ok")
    }

    fn report_agent(&self, req: ReportAgent) -> Result<(), HerdrError> {
        let result = self.call("pane.report_agent", params::pane_report_agent(&req))?;
        expect_type(&result, "ok")
    }

    fn report_metadata(&self, req: ReportMetadata) -> Result<(), HerdrError> {
        if let Some(pane_id) = req.pane_id.clone() {
            let result = self.call(
                "pane.report_metadata",
                params::pane_report_metadata(&req, &pane_id),
            )?;
            expect_type(&result, "ok")
        } else if let Some(workspace_id) = req.workspace_id.clone() {
            let result = self.call(
                "workspace.report_metadata",
                params::workspace_report_metadata(&req, &workspace_id),
            )?;
            expect_type(&result, "ok")
        } else {
            Err(transport(
                "ReportMetadata needs a pane_id or a workspace_id",
            ))
        }
    }

    fn notify(&self, req: Notify) -> Result<(), HerdrError> {
        let result = self.call("notification.show", params::notification_show(&req))?;
        expect_type(&result, "notification_show")
    }

    fn focus_pane(&self, req: FocusPane) -> Result<(), HerdrError> {
        let result = self.call("pane.focus", params::pane_focus(&req))?;
        expect_type(&result, "pane_info")
    }

    fn subscribe(
        &self,
        subs: Vec<EventSubscription>,
    ) -> Result<Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>, HerdrError> {
        self.subscribe_impl(subs)
    }
}
