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
//! `tab.create` and `pane.wait_for_output` (real, schema-defined
//! methods with no `HerdrClient` wrapper — sent here as raw NDJSON
//! lines, `raw_call`, the same framing `SocketClient::call` uses, R1:
//! no trait method exists to reuse, and adding one to the product for
//! a test's own use is out of this item's scope).
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
//! Fix 3 adds one step to that: with a real writer running on `pane_b`,
//! three `events.subscribe` calls in sequence — the third after a
//! `pane.split` — each acked and each delivering the event this test
//! caused. That is the combination tried step 3 crashed on and no
//! earlier test could reach (one subscribe, idle pane); `pane_c`, split
//! between the second and third, is closed with the others at teardown.
//!
//! **What actually pushes `PaneUpdated`, and why this step used to
//! hang.** It sent `yes wirk-live-sweep-output | head -c 200000` to
//! `pane_b` and then subscribed, on the assumption that a pane
//! *producing output* pushes `PaneUpdated`. It blocked forever at
//! `first.next()`, deterministically, 3 of 3
//! (`index-retirement-order-verify/raw/21`–`raw/25`), and the burst
//! having already finished was only the visible half of it.
//!
//! Read at the server instead of assumed (`refs/herdr` `0f8ad12`,
//! herdr 0.9.0): **pane output emits no event at all.** Every
//! `emit_pane_updated` call site is a state change of the pane's
//! *record*, not of its screen — a metadata token expiring
//! (`app/runtime.rs:66`), an agent name or managed-agent reconciliation
//! (`app/agents.rs:60,136`), a terminal **title** whose stripped form
//! changed (`app/terminal_titles.rs:73`), an agent-status update
//! (`app/api.rs:632`), and `pane.report_metadata` when the tokens it
//! carries actually change (`app/api/panes.rs:1756`, `token_changed`).
//! A flood of characters through a pty is none of those. So no burst,
//! however long, was ever going to satisfy that `next()`, and "make the
//! output last longer" would have been a fix to the wrong thing.
//! Re-executed here against the installed herdr 0.9.0, subscription
//! open first: **0 events in 3 s while `pane_b` was actively writing**
//! (`live-sweep-causality-correct/raw/P1.log` step `[E]`).
//!
//! What causes each tested event is the last of those: one real
//! `pane.report_metadata` call per subscription, carrying a token value
//! this test changes each round, made **after** `subscribe` has
//! returned — which it does only after the server's own
//! `subscription_started` ack (`socket.rs::subscribe_impl`).
//!
//! **Causing an event is not enough, and this file used to stop
//! there.** Three findings against the previous version (ruling 0133's
//! L1/L2/L3), each executed against the installed herdr 0.9.0 with no
//! wirk code in the path (`live-sweep-causality-correct/raw/P1.log`)
//! and each closed here:
//!
//! * **L1 — the first subscription, on `pane_a`, caused nothing at
//!   all.** Its cause was `pane.send_text` of an `echo`, and a pane's
//!   output pushes no event: measured, 0 caused events in 8 s with the
//!   subscription already open (`raw/P1.log` step `[A]`). What
//!   satisfied it was one of the uncaused terminal **title** events
//!   every freshly split pane's shell emits 0.10–0.30 s after its pane
//!   is created, carrying `tokens: null` — won on shell startup
//!   latency, and lost the moment those events landed before the ack.
//!   It now causes its own `pane.report_metadata` on `pane_a`, after
//!   the ack, exactly as the three `pane_b` rounds do (`[B]`: one
//!   `pane_updated`, naming `pane_a`, carrying the token).
//! * **L1/L3 — no wait checked what it had received.**
//!   `next_event_within` took the first line off the stream and
//!   asserted only that it parsed, so *any* event on *any* pane
//!   counted. `next_expected_pane_updated_within` replaces it: it
//!   consumes and prints the events that are not the one expected, and
//!   returns only a `PaneUpdated` whose `pane.pane_id` is this round's
//!   pane **and** whose `pane.tokens` carry this round's own
//!   `ROUND_TOKEN_KEY` value. The token is the identity precisely
//!   because `token_changed` is the server's own emission condition.
//!   And each of the four rounds proves that refusal *in band* rather
//!   than trusting a race: before its real cause it causes two decoys
//!   on the same stream — one on a pane the round is not about, one on
//!   the round's own pane with the wrong token value — and then asserts
//!   that both were seen and refused before the caused event arrived.
//!   So "an unrelated event cannot satisfy this wait" is measured every
//!   run, in both halves of the identity, and not only when the
//!   uncaused title events happen to be in flight.
//! * **L3 — that filtering has to be the test's own, because the wire
//!   has no pane filter for this event.** `subscription_json` attaches
//!   `pane_id` only to `pane.agent_status_changed`,
//!   `pane.output_matched` and `pane.scroll_changed`
//!   (`socket.rs:505-515`), which is correct: the server's schema for a
//!   `pane.updated` subscription takes `type` and nothing else (`herdr
//!   api schema --json`). `EventSubscription::PaneUpdated { pane_id }`
//!   is therefore **session-wide** on the wire and its `pane_id` never
//!   leaves the client. No pane filter is invented here to paper over
//!   that; the identity is checked on the delivered event, which is the
//!   only place the wire allows it to be checked.
//!
//! **L2 — and the busy pane is now arranged rather than assumed.** The
//! finite burst it was downgraded to (`seq 1 20000 | sed …`) finished
//! 0.05 s after `send_text` returned, so the three subscriptions written
//! for "the same busy pane" ran against a pane that had produced output
//! and stopped. `pane_b` now runs a **bounded, paced** writer — 120
//! lines at 0.25 s, ~30 s of real output through a real pty, ending on
//! its own and killed with its pane at teardown either way, so nothing
//! floods and nothing outlives the test. That it is still producing is
//! **measured, never assumed or slept for**: the pane's own visible text
//! is read through `pane.read` (a real `HerdrClient` method, R2) before
//! each subscription and again after each delivery, the server's own
//! `pane.wait_for_output` (R5 — a real schema method that blocks until
//! the line exists, rather than a hand-rolled poll) bounds the wait for
//! a strictly later line, and each round asserts that the line counter
//! really advanced across it. No sleep decides anything, no flood runs
//! forever, no live path is skipped, and the product gains no timeout of
//! any kind.

#[path = "support/live_herdr.rs"]
mod live_herdr;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;

use std::sync::mpsc;

use serde_json::{Value, json};
use tempfile::tempdir;

use wirk_herdr::{
    AgentStatus, Bearing, CloseWorkspace, CreateWorkspace, EventSubscription, FocusPane,
    HerdrClient, HerdrError, HerdrEvent, Notify, OpenWorktree, PaneInfo, PromptAgent, ReleaseAgent,
    RemoveWorktree, ReportAgent, ReportAgentSession, ReportMetadata, SendKeys, SplitDirection,
    SplitPane, StartAgent,
};

use std::time::{Duration, Instant};

/// This test file's own termination bound for its one raw, hand-framed
/// socket call (`raw_call` — no `HerdrClient` method wraps `tab.create`,
/// module doc): a live-run test may carry a bound whose exhaustion is
/// reported as "never observed" (the owner's ruling of 2026-09-02 §3;
/// ruling 0044's own exception), never a verdict about the product,
/// which itself sets no read timeout anywhere any more (fix 2).
///
/// **It applies to this one connection and nothing else.** This
/// constant's doc used to claim it was "the read timeout applied to
/// every request connection this test's client dials" and, "via the
/// reader thread `subscribe` starts", the bound on how long the
/// `events.subscribe` step waits for a pushed event. Both halves are
/// false and were read off nothing: `SocketClient`'s own `dial` sets no
/// read timeout on any connection it opens (`socket.rs::dial`, ruling
/// 0044), and `subscribe_impl` says so in as many words before it hands
/// the connection to its reader thread. The only `set_read_timeout` in
/// all of `wirk-herdr` is `raw_call`'s, six lines below. What bounds a
/// subscription wait in this file is `next_caused_pane_updated_within`,
/// and it is the test's own, on the test's own thread.
const RAW_CALL_READ_TIMEOUT: Duration = Duration::from_secs(20);

/// This test's own bound on waiting for one pushed event, and its own
/// release of the subscription that was waiting.
///
/// **Test-only, and nothing like a poll or a product timeout.** The
/// product sets no read timeout on a subscription connection and must
/// not (ruling 0044, fix 2): a closed stream *is* the observation, and
/// inventing an elapsed time is what that ruling forbids. But a test
/// that blocks forever when its expected event never arrives is not a
/// test — it is an unattended `cargo test --workspace` that never
/// finishes, which is what this file did. So the bound lives here, on
/// the test's side of the iterator, and its exhaustion is reported as
/// "no event was ever observed" — a failure of this run, never a
/// verdict about the product (ruling 0044 D134: "a bound on a wait is
/// allowed and named as a bound").
const EVENT_WAIT_BOUND: Duration = Duration::from_secs(60);

/// The metadata token key every event this file causes is identified
/// by, and the reason a token is the identity at all: the server emits
/// `PaneUpdated` from `pane.report_metadata` **only** when the tokens
/// actually changed (`refs/herdr` `0f8ad12`, `app/api/panes.rs:1756`,
/// `token_changed`), so a `PaneUpdated` carrying this key at this
/// round's value can only be the one this round caused. Its shape is
/// the server's own (`^[A-Za-z0-9_-]{1,32}$`, `PaneReportMetadataParams`
/// in `herdr api schema --json`).
const ROUND_TOKEN_KEY: &str = "wirk-live-sweep-round";

/// The prefix of every line `pane_b`'s writer emits. Deliberately not a
/// substring of the writer command itself as the shell echoes it: the
/// command contains the literal `$i`, so the counter regex below can
/// only ever match real *output*, never the command line that produced
/// it (measured — a first draft of this used a completion marker that
/// was a literal in the command, and `pane.read` "saw" it before the
/// writer had written a line, `raw/P1.log` step `[C]`).
const OUTPUT_LINE_PREFIX: &str = "wirk-live-sweep-output ";

/// How many lines `pane_b`'s writer emits, and how far apart. Bounded
/// on purpose (L2): 120 lines at 0.25 s is ~30 s of genuinely
/// continuous output — comfortably longer than the three rounds that
/// must run against a busy pane, and finite, so nothing floods the pty
/// and the writer ends on its own even if teardown never ran. Teardown
/// closes the pane anyway, which takes its shell with it.
const OUTPUT_LINES: u64 = 120;
const OUTPUT_INTERVAL_SECONDS: &str = "0.25";

/// This test's own bound on the server's own `pane.wait_for_output`
/// (`timeout_ms`), which is what makes "the pane is still producing
/// output" a measurement rather than a sleep: the server blocks until
/// the line exists or this elapses, and its exhaustion fails this run
/// rather than saying anything about the product. Kept below
/// `RAW_CALL_READ_TIMEOUT`, since that is the connection the call is
/// made on.
const OUTPUT_WAIT_BOUND_MS: u64 = 15_000;

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

/// One line of evidence per event this test did not ask for, printed
/// rather than silently dropped: which pane it named and what tokens it
/// carried, so a run's own output shows that the uncaused startup and
/// title events really do arrive and really are refused.
fn describe_event(event: &HerdrEvent) -> String {
    match event {
        HerdrEvent::PaneUpdated { pane } => format!(
            "pane_updated pane={} revision={} tokens={:?} title={:?}",
            pane.pane_id, pane.revision, pane.tokens, pane.terminal_title_stripped
        ),
        HerdrEvent::PaneAgentStatusChanged {
            pane_id,
            agent_status,
            ..
        } => format!("pane_agent_status_changed pane={pane_id} status={agent_status:?}"),
        HerdrEvent::PaneCreated { pane } => format!("pane_created pane={}", pane.pane_id),
        other => format!("{other:?}"),
    }
}

/// Whether a pushed event is the one a round caused: a `PaneUpdated`
/// naming `pane_id`, whose metadata tokens carry `ROUND_TOKEN_KEY` at
/// exactly `round`.
///
/// Both halves are load-bearing and neither is redundant.
/// `EventSubscription::PaneUpdated` carries no pane filter on the wire
/// (module doc, L3), so every pane's `PaneUpdated` arrives here and the
/// pane must be checked; and a pane's own uncaused terminal-title
/// events name the right pane while carrying `tokens: null`, so the
/// token must be checked too. Only `pane.report_metadata` with a
/// changed token produces both at once, and only this test sends that.
fn is_caused_pane_updated(event: &HerdrEvent, pane_id: &str, round: &str) -> bool {
    match event {
        HerdrEvent::PaneUpdated { pane } => {
            pane.pane_id == pane_id
                && pane
                    .tokens
                    .as_ref()
                    .and_then(|tokens| tokens.get(ROUND_TOKEN_KEY))
                    .is_some_and(|value| value == round)
        }
        _ => false,
    }
}

/// Waits for the one event this round caused, within this test's own
/// bound, ignoring (and reporting) every event that is not it — then
/// releases the subscription either way (`EVENT_WAIT_BOUND`).
///
/// **This replaces a wait that accepted anything.** The previous
/// `next_event_within` returned the first line off the stream and
/// asserted only that it parsed as a `HerdrEvent`, which the uncaused
/// terminal-title events every freshly split pane emits satisfy
/// perfectly well — measured, and the reason the `pane_a` step passed
/// on a race rather than on its own cause (module doc, L1). Here the
/// only thing that ends the wait successfully is
/// `is_caused_pane_updated`: this round's pane and this round's token.
/// The count of events refused on the way is returned with it, so a
/// caller that arranged refusals can assert they really happened.
///
/// The iterator is *moved in*, so returning from this function is the
/// `drop` the caller used to write by hand: the receiving half of this
/// function's channel goes away, the forwarding thread's next `send`
/// fails and it breaks, dropping the client's iterator with it, whose
/// own receiver going away then breaks the client's reader thread out
/// of its loop and closes the connection.
///
/// **When the bound is exhausted, the real resources still go.** This
/// function's own thread is blocked on `rx.recv_timeout`'s counterpart,
/// a `send` into a channel nobody is reading; the thread actually
/// inside an untimed `read_line` is the *client's* reader thread, which
/// is the product's contract and correctly has no timeout, so nothing
/// here can interrupt it either. The panic below unwinds the test
/// thread into `LiveHerdrSession::drop`, which closes every workspace
/// and then stops and deletes the throwaway session; the server exits,
/// the kernel closes the subscription connection, `read_line` returns
/// `Ok(0)`, the client's reader thread ends and drops its sender, and
/// this function's thread ends with the `recv` that then fails. Both
/// threads go, and they go before the process does, not because of it
/// (executed: the EOF arrives 0.33 s after `herdr session stop`,
/// `index-sync-and-live-sweep-verify/VERDICT.md`). No workspace, pane,
/// session or connection is left behind, and no bound of any kind is
/// added to the product.
fn next_caused_pane_updated_within(
    label: &str,
    events: Box<dyn Iterator<Item = Result<HerdrEvent, HerdrError>> + Send>,
    pane_id: &str,
    round: &str,
) -> (HerdrEvent, usize) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for event in events {
            if tx.send(event).is_err() {
                break; // receiver dropped: the caller is done with this subscription
            }
        }
    });
    let deadline = Instant::now() + EVENT_WAIT_BOUND;
    let mut ignored = 0usize;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Ok(event)) => {
                if is_caused_pane_updated(&event, pane_id, round) {
                    eprintln!(
                        "live_sweep: {label}: delivered the caused event after ignoring \
                         {ignored} unrelated: {}",
                        describe_event(&event)
                    );
                    return (event, ignored);
                }
                ignored += 1;
                eprintln!(
                    "live_sweep: {label}: IGNORED (not pane {pane_id} with \
                     {ROUND_TOKEN_KEY}={round}): {}",
                    describe_event(&event)
                );
            }
            Ok(Err(error)) => {
                panic!("{label}: pushed line was not a well-formed HerdrEvent: {error:?}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
                "{label}: the subscription ended after {ignored} unrelated event(s) and \
                 before the one this round caused (pane {pane_id}, {ROUND_TOKEN_KEY}={round})"
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "{label}: no event for pane {pane_id} carrying {ROUND_TOKEN_KEY}={round} was \
                 ever observed within {EVENT_WAIT_BOUND:?} ({ignored} unrelated event(s) were \
                 seen and refused) — the bound is this test's own and its exhaustion is \
                 \"never observed\", not a verdict about the server"
            ),
        }
    }
}

/// The highest `wirk-live-sweep-output N` this pane has on screen, read
/// off the pane itself through `pane.read` (`HerdrClient::read_pane`,
/// R2 — a real method of the surface this file sweeps, and one nothing
/// here used to exercise). `None` when the writer has not produced a
/// line yet.
///
/// The regex is a hand parse rather than a dependency (R3/R6): the
/// prefix is fixed and the tail is digits.
fn observed_output_line(client: &impl HerdrClient, pane_id: &str) -> Option<u64> {
    let text = client
        .read_pane(pane_id)
        .unwrap_or_else(|e| panic!("pane.read({pane_id}): {e:?}"));
    text.split(OUTPUT_LINE_PREFIX)
        .skip(1)
        .filter_map(|tail| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u64>().ok()
        })
        .max()
}

/// Blocks until `pane_id` has actually produced `wirk-live-sweep-output
/// <line>`, using the server's own `pane.wait_for_output` (R5: a real
/// schema method that does exactly this, bounded by its own
/// `timeout_ms` — hand-rolling a poll loop here would be the failure of
/// R5 the ladder names). No `HerdrClient` method wraps it, so it goes
/// out as a raw NDJSON line, the same way `tab.create` does.
///
/// This is the whole of L2's correction: "the pane is producing output"
/// stops being an assumption about timing and becomes a thing the
/// server confirms, in the round that needs it.
fn wait_for_output_line(socket_path: &Path, pane_id: &str, line: u64) -> String {
    let target = format!("{OUTPUT_LINE_PREFIX}{line}");
    let reply = raw_call(
        socket_path,
        "pane.wait_for_output",
        json!({
            "pane_id": pane_id,
            "source": "recent",
            "match": {"type": "substring", "value": target},
            "timeout_ms": OUTPUT_WAIT_BOUND_MS,
        }),
    );
    let matched = reply
        .get("result")
        .and_then(|result| result.get("matched_line"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "pane.wait_for_output({pane_id}, {target:?}): the pane produced no such line \
                 within {OUTPUT_WAIT_BOUND_MS}ms — this run's own bound, not a verdict about \
                 the server: {reply}"
            )
        });
    matched.to_string()
}

#[test]
fn live_sweep_walks_every_socketclient_method_in_launch_order_then_the_rest() {
    let Some(session) = live_herdr::LiveHerdrSession::start(
        "live_sweep_walks_every_socketclient_method_in_launch_order_then_the_rest",
    ) else {
        // Said explicitly, so a run that took the `herdr not on PATH`
        // early return can never be read as a live sweep that passed:
        // that branch reports `ok` to `cargo test` like any other, and
        // a bare `ok` on its own says nothing about which path ran.
        eprintln!("live_sweep: SKIPPED — herdr is not on PATH; the live path was NOT entered");
        return;
    };
    eprintln!(
        "live_sweep: ENTERED the live path against throwaway session {} at {}",
        session.name(),
        session.socket_path().display()
    );
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

    // One real `pane.report_metadata` on a named pane, carrying a
    // token value that differs from the last one that pane was given —
    // the condition, and the only condition this test can create, on
    // which the server emits `PaneUpdated`
    // (`refs/herdr` `0f8ad12`, `app/api/panes.rs:1756`: `token_changed`).
    // Always called *after* the subscription it is meant for has been
    // acked, and the round value it writes is what identifies the event
    // that comes back.
    // Two events every round causes *before* its real one, on the same
    // stream and after the same ack, so that "an unrelated event cannot
    // satisfy this wait" is a thing each run demonstrates in band rather
    // than a property that happens to hold when the race is won. The
    // first names a pane the round is not about (with the right token
    // key); the second names the round's own pane with the wrong token
    // value. Both are refused by `is_caused_pane_updated`, one for each
    // half of the identity, and the round then asserts that at least two
    // events were refused before the one it caused.
    //
    // These are real `pane.report_metadata` calls on real panes, and
    // they are ordinary decoys rather than an injection: the server
    // pushes them because their tokens changed, exactly as it pushes the
    // round's own.
    let cause_pane_updated = |pane_id: &str, round: &str| {
        client
            .report_metadata(ReportMetadata {
                pane_id: Some(pane_id.to_string()),
                workspace_id: None,
                source: "wirk-live-sweep".to_string(),
                tokens: Some(json!({ROUND_TOKEN_KEY: round})),
                title: None,
            })
            .unwrap_or_else(|e| {
                panic!("pane.report_metadata (pane {pane_id}, round {round}, the cause): {e:?}")
            });
    };

    // events.subscribe: subscribe to pane_a, cause an identifiable
    // update on pane_a, then read events until the one that was caused
    // arrives — a genuine blocking read, no timeout anywhere in the
    // product (fix 2, ruling 0044).
    //
    // **This step used to cause nothing** (module doc, L1). Its cause
    // was the `pane.send_text` below and nothing else, and a pane's
    // output pushes no event at all, so what actually satisfied it was
    // an uncaused terminal-title event from a shell that had just
    // started — an event with `tokens: null`, sometimes on another pane
    // entirely. The `send_text` stays, because `pane.send_text` is part
    // of the surface this file sweeps and because keeping it makes the
    // point: it is issued here as a *non-cause*, its title event is one
    // of the ones the wait now refuses by name, and the round token is
    // what ends the wait.
    let events = client
        .subscribe(vec![EventSubscription::PaneUpdated {
            pane_id: pane_a.pane_id.clone(),
        }])
        .expect("events.subscribe");
    client
        .send_input(&pane_a.pane_id, "echo wirk-live-sweep\n")
        .expect("pane.send_text (exercised here, and deliberately not the cause)");
    // Everything below comes *after* the ack, which is the only ordering
    // that makes the event this step waits for the subscription's to
    // see. The two decoys first, then the cause.
    cause_pane_updated(&seed.pane_id, "a");
    cause_pane_updated(&pane_a.pane_id, "a-decoy");
    cause_pane_updated(&pane_a.pane_id, "a");
    // The read itself blocks with no timeout in the product (fix 2,
    // ruling 0044); the bound is the test's own and lives on the test's
    // side of the iterator (`next_caused_pane_updated_within`), as does
    // the identity check that says which event may end it.
    let (_, refused) =
        next_caused_pane_updated_within("events.subscribe", events, &pane_a.pane_id, "a");
    assert!(
        refused >= 2,
        "events.subscribe: the two decoys caused before the real event were not both seen \
         and refused ({refused} refused) — the identity check was not exercised"
    );
    eprintln!(
        "live_sweep: events.subscribe on pane_a delivered the event it caused \
         ({ROUND_TOKEN_KEY}=a on {}) after refusing {refused} unrelated",
        pane_a.pane_id
    );

    // ---- sequential subscriptions on a pane with output flowing ------
    //
    // Fix 3's own scenario (0028 tried step 3): the crash there needed
    // a *second* subscribe against a pane that was producing output,
    // which the one-subscribe step above cannot reach. A real writer
    // runs on `pane_b`, then three subscriptions are opened in sequence
    // — the third after a `pane.split` changes the session — and each
    // must ack and then deliver the event the test causes *after* the
    // ack, identified by pane and token, never one it hopes is still in
    // flight from before it.
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
    //
    // `pane_b` is put to work for real and for long enough (L2): a
    // bounded, paced writer through a real pty, running across all
    // three rounds rather than a burst that finished before the first
    // of them. It is not what causes the events below, and it never was
    // — the module doc has the server's own reason — so the fact that
    // it is *still writing* is asserted separately, round by round,
    // rather than assumed.
    let writer = format!(
        "i=1; while [ $i -le {OUTPUT_LINES} ]; do echo \"{OUTPUT_LINE_PREFIX}$i\"; \
         sleep {OUTPUT_INTERVAL_SECONDS}; i=$((i+1)); done\n"
    );
    client
        .send_input(&pane_b.pane_id, &writer)
        .expect("pane.send_text (the paced writer on pane_b)");
    // The writer has genuinely started before any of this scenario's
    // subscriptions are opened — waited for at the server, not slept
    // for.
    let started = wait_for_output_line(&socket_path, &pane_b.pane_id, 1);
    eprintln!("live_sweep: pane_b is writing: first line observed at the server: {started:?}");

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

    // One round: read where the writer has got to, subscribe, cause the
    // round's own event, wait for *that* event, then require the writer
    // to have moved on — the server's own `pane.wait_for_output` blocks
    // until a strictly later line exists, and the pane is read again so
    // the numbers on both sides of the round are recorded rather than
    // inferred. A pane that had stopped producing output fails the round
    // it was supposed to be busy for.
    let busy_round = |label: &str, subscription, round: &str| {
        let before = observed_output_line(&client, &pane_b.pane_id)
            .unwrap_or_else(|| panic!("{label}: pane_b had produced no output line before it"));
        let decoy = format!("{round}-decoy");
        cause_pane_updated(&seed.pane_id, round);
        cause_pane_updated(&pane_b.pane_id, &decoy);
        cause_pane_updated(&pane_b.pane_id, round);
        let (_, refused) =
            next_caused_pane_updated_within(label, subscription, &pane_b.pane_id, round);
        assert!(
            refused >= 2,
            "{label}: the two decoys caused before the real event were not both seen and \
             refused ({refused} refused) — the identity check was not exercised"
        );
        wait_for_output_line(&socket_path, &pane_b.pane_id, before + 1);
        let after = observed_output_line(&client, &pane_b.pane_id)
            .expect("pane_b's output cannot disappear once it has been read");
        assert!(
            after > before,
            "{label}: pane_b was supposed to be producing output across this round, and its \
             line counter did not advance ({before} -> {after})"
        );
        eprintln!(
            "live_sweep: {label} on pane_b delivered the event it caused \
             ({ROUND_TOKEN_KEY}={round}) after refusing {refused} unrelated; pane_b's output \
             advanced {before} -> {after} across the round"
        );
    };

    let first = client
        .subscribe(busy_subscriptions())
        .expect("events.subscribe #1 on a pane with output flowing: ack must match");
    busy_round("events.subscribe #1", first, "1");

    let second = client
        .subscribe(busy_subscriptions())
        .expect("events.subscribe #2 on the same busy pane: ack must match");
    busy_round("events.subscribe #2", second, "2");

    // A third, after the session changes under it (`pane.split`). The
    // new pane's own shell emits an uncaused title event of its own
    // shortly after this returns, on the same session-wide stream the
    // third subscription reads (L3) — which is exactly the kind of
    // event the wait must refuse, and does.
    let pane_c: PaneInfo = client
        .split_pane(SplitPane {
            workspace_id: Some(ws.workspace_id.clone()),
            target_pane_id: Some(pane_b.pane_id.clone()),
            direction: SplitDirection::Down,
            cwd: workspace_dir.path().to_path_buf(),
            env: BTreeMap::new(),
        })
        .expect("pane.split (a third pane, between subscriptions)");
    let third = client
        .subscribe(busy_subscriptions())
        .expect("events.subscribe #3, after a pane.split: ack must match");
    busy_round("events.subscribe #3", third, "3");

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
    eprintln!("live_sweep: COMPLETED the live path — every method above ran against a real Herdr");
}
