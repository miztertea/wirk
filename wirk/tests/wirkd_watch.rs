//! `wirkd watch` (item B, ruling 0044): one NDJSON `Event` line per
//! journal append of the named Work, starting with what is already
//! there. Drives a real `wirk wirkd start` child process (the same
//! discipline `wirkd_process.rs` uses) and dials it with `wirkd::
//! client::watch` directly — the client side is a plain library call
//! over the real socket, never a fake server (0040 D127).

#[path = "support/route_fixture.rs"]
mod route_fixture;
#[path = "../src/wirkd/mod.rs"]
mod wirkd;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wirkd::{Reply, Request, SubmitPayload, WatchPayload, WirkdPointer};

use wirk_core::{Event, EventKind, RepositoryBinding, RunId, WorkId};

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
            "wirkd pointer file never appeared (readable) at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Submits a Work directly over the socket (`submit`'s own verb, R6 —
/// no CLI round trip needed for what this file tests) and returns its
/// `WorkId`. `--route` is required now (p2-route-files W2), so the
/// estate's own copy of the canonical `smoke.json` fixture is installed
/// first — this file's own tests are about `watch`, not Route content.
fn submit(estate: &Path, socket: &Path, intent: &str) -> (WorkId, RunId) {
    route_fixture::install_route_fixture(estate, "smoke");
    let reply = wirkd::client::call(
        socket,
        &Request::submit(SubmitPayload {
            intent: intent.to_string(),
            repositories: vec![RepositoryBinding {
                name: "demo".to_string(),
                access: wirk_core::Access::Write,
            }],
            base_ref: "main".to_string(),
            source_basis: None,
            kind: None,
            command: None,
            repo_path: None,
            route: Some("smoke".to_string()),
        }),
    )
    .expect("submit call reaches wirkd");
    match reply {
        Reply::Ok { result, .. } => (
            WorkId(
                result["work_id"]
                    .as_str()
                    .expect("submit result carries work_id")
                    .to_string(),
            ),
            RunId(
                result["run_id"]
                    .as_str()
                    .expect("submit result carries run_id")
                    .to_string(),
            ),
        ),
        Reply::Err { error, .. } => panic!("submit refused: {} {}", error.code, error.message),
    }
}

/// Records one more `EventKind` onto `work_id`'s journal via the
/// `record` verb — the same write path `RunLoop` itself uses.
fn record(socket: &Path, work_id: &WorkId, run_id: &RunId, kind: EventKind) {
    let reply = wirkd::client::call(
        socket,
        &Request::record(wirkd::RecordPayload {
            work_id: work_id.clone(),
            run: Some(run_id.clone()),
            kind,
        }),
    )
    .expect("record call reaches wirkd");
    match reply {
        Reply::Ok { .. } => {}
        Reply::Err { error, .. } => panic!("record refused: {} {}", error.code, error.message),
    }
}

/// Opens `watch` on its own thread (the call blocks — module doc,
/// ruling 0044), forwarding every `Event` it reads into an `mpsc`
/// channel the test polls; the `JoinHandle` is returned so a test that
/// wants to prove `EOF` can join it directly instead of only reading
/// the channel closing.
fn spawn_watch(
    socket: &Path,
    work_id: &WorkId,
) -> (mpsc::Receiver<Event>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let socket = socket.to_path_buf();
    let work_id = work_id.clone();
    let handle = std::thread::spawn(move || {
        let events = wirkd::client::watch(&socket, WatchPayload { work_id }).expect("watch dials");
        for event in events {
            match event {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Err(_) => return, // EOF or a transport error: the stream ended
            }
        }
    });
    (rx, handle)
}

/// A test's own termination bound (never a product one — the owner's
/// ruling of 2026-09-02 §3): the next `Event` off `rx`, or a panic
/// naming what was never observed.
fn recv_event(rx: &mpsc::Receiver<Event>, what: &str) -> Event {
    rx.recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("never observed: {what}"))
}

/// Dials `watch` on its own thread and reads exactly one line, bounded
/// by `timeout` — a test's own termination bound (never a product
/// one), since a refused-but-not-yet-fixed `watch` can otherwise block
/// forever on `Receiver::recv` for a Work no `submit` ever created
/// (`handle_watch_connection`'s own no-timeout contract, ruling 0044).
/// `None` means the bound expired with no reply at all — itself a
/// meaningful (and, before this wave's correction, expected) result,
/// never a panic, so the caller can assert on it directly.
fn watch_first_bounded(
    socket: &Path,
    work_id: WorkId,
    timeout: Duration,
) -> Option<Result<Event, String>> {
    let (tx, rx) = mpsc::channel();
    let socket = socket.to_path_buf();
    std::thread::spawn(move || {
        let outcome = match wirkd::client::watch(&socket, WatchPayload { work_id }) {
            Ok(mut events) => events.next().map(|r| r.map_err(|err| err.to_string())),
            Err(err) => Some(Err(err.to_string())),
        };
        let _ = tx.send(outcome);
    });
    rx.recv_timeout(timeout).unwrap_or(None)
}

/// (8a/8b) A client connected **before** an append receives it; a
/// client connecting **after** receives the earlier lines first.
#[test]
fn a_watcher_sees_events_before_and_after_it_dials() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let mut wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_id, run_id) = submit(&estate, &pointer.socket, "watch test");

    // 8a: dial before any further append — the watcher must at least
    // see everything `submit` itself already journaled.
    let (rx_before, _handle_before) = spawn_watch(&pointer.socket, &work_id);
    let first = recv_event(
        &rx_before,
        "the WorkSubmitted event submit already journaled",
    );
    assert!(
        matches!(first.kind, EventKind::WorkSubmitted { .. }),
        "expected WorkSubmitted first, got {:?}",
        first.kind
    );

    // A further append, live, while `rx_before` is already connected.
    record(&pointer.socket, &work_id, &run_id, EventKind::RunVanished);
    // Drain whatever `submit` itself wrote (WaypointReserved, RunOpened)
    // before the live one this call just appended.
    let mut saw_live = false;
    for _ in 0..8 {
        let event = recv_event(&rx_before, "the live RunVanished append");
        if matches!(&event.kind, EventKind::RunVanished) {
            saw_live = true;
            break;
        }
    }
    assert!(saw_live, "the watcher dialed before the append must see it");

    // 8b: a second watcher, dialed *after* every append above, must see
    // the same events already present, starting from the beginning.
    let (rx_after, _handle_after) = spawn_watch(&pointer.socket, &work_id);
    let first_after = recv_event(&rx_after, "the earlier WorkSubmitted, replayed");
    assert!(
        matches!(first_after.kind, EventKind::WorkSubmitted { .. }),
        "a late watcher must still see the earlier lines first, got {:?}",
        first_after.kind
    );
    let mut saw_live_after = false;
    for _ in 0..8 {
        let event = recv_event(&rx_after, "the earlier live RunVanished, replayed");
        if matches!(&event.kind, EventKind::RunVanished) {
            saw_live_after = true;
            break;
        }
    }
    assert!(
        saw_live_after,
        "a late watcher must see the already-appended live event too"
    );

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    let _ = wirkd_child.0.wait();
}

/// (8c) A second Work's appends are never delivered to the first
/// Work's watcher.
#[test]
fn a_second_works_appends_are_not_delivered_to_the_first_works_watcher() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let mut wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_a, _run_a) = submit(&estate, &pointer.socket, "work a");
    let (work_b, run_b) = submit(&estate, &pointer.socket, "work b");

    let (rx_a, _handle_a) = spawn_watch(&pointer.socket, &work_a);
    // Drain work_a's own submit-time events first.
    loop {
        let event = recv_event(&rx_a, "work_a's own submit-time events");
        if matches!(event.kind, EventKind::RunOpened { .. }) {
            break;
        }
    }

    record(&pointer.socket, &work_b, &run_b, EventKind::RunVanished);
    // work_a's own watcher must not receive anything more within a
    // bounded wait — a genuine cross-Work leak would show up as this
    // `recv_timeout` succeeding instead of timing out.
    match rx_a.recv_timeout(Duration::from_millis(500)) {
        Ok(event) => panic!("work_a's watcher received a foreign event: {event:?}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("work_a's watch connection ended unexpectedly")
        }
    }

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    let _ = wirkd_child.0.wait();
}

/// (8d) wirkd stopping ends the stream (`EOF`) for the client.
#[test]
fn wirkd_stopping_ends_the_watch_stream_for_the_client() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_id, _run_id) = submit(&estate, &pointer.socket, "watch eof test");

    let (rx, handle) = spawn_watch(&pointer.socket, &work_id);
    let _ = recv_event(&rx, "the WorkSubmitted event submit already journaled");

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());

    // `EOF` closes the channel (`spawn_watch`'s reader returns once its
    // `events` iterator ends), which a bounded `recv_timeout` loop
    // observes as `Disconnected` — a test's own termination bound
    // (never a product one), not a `.join()` that could hang the suite
    // outright on a real regression.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(_) => {} // a stray late event: keep draining
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                assert!(
                    Instant::now() < deadline,
                    "the watch stream never closed after wirkd stop"
                );
            }
        }
    }
    let _ = handle.join();

    // `KillOnDrop` still runs at scope end (best-effort double-stop is
    // harmless: the process is already gone).
    drop(wirkd_child);
}

/// P3 foundation correction (0069, `foundation-verify/VERDICT.md`
/// finding 2): `watch` must never create a journal for a Work that was
/// never submitted — only `submit` may (0067). Red before this wave:
/// `handle_watch_connection` called `create_journal_for`, whose
/// `Journal::open` does `create_dir_all` plus `OpenOptions::create(true)`,
/// materializing a real, empty `works/<id>/journal.ndjson` for any
/// caller-supplied id.
#[test]
fn watch_of_unsubmitted_work_creates_no_journal_and_is_refused() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);

    let unknown = WorkId("work-never-submitted".to_string());
    match watch_first_bounded(&pointer.socket, unknown.clone(), Duration::from_secs(10)) {
        Some(Err(msg)) => {
            assert!(
                msg.contains("NotFound"),
                "expected a NotFound refusal for an unknown Work, got: {msg}"
            );
        }
        other => panic!(
            "expected watch of an unknown Work to be refused promptly, got: {other:?} \
             (None means the daemon never replied within the bound — the Work's empty \
             journal was created and the connection blocked on further events that \
             never came)"
        ),
    }

    assert!(
        !estate.join("works").join(&unknown.0).exists(),
        "watch must never materialize a journal for a Work that was never submitted"
    );

    // The daemon must still be alive and answering other calls after
    // the refusal — a real submit still works.
    let (work_id, _run_id) = submit(&estate, &pointer.socket, "post-refusal liveness check");
    assert!(
        estate.join("works").join(&work_id.0).exists(),
        "the daemon must still accept ordinary work after refusing an unknown watch"
    );

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    drop(wirkd_child);
}

/// P3 foundation correction (0069, `foundation-verify/VERDICT.md`
/// finding 3): a path-like Work id must be refused, not panic the
/// connection thread. Red before this wave: `create_journal_for`
/// resolved the id through `work_journal_dir(...).expect(
/// "daemon-minted WorkId is a path component")`, and `watch` reached
/// that call with a caller-supplied id — externally reachable, silent
/// exit 0 for the client, a panic message on the daemon's stderr.
#[test]
fn watch_of_path_like_work_id_is_refused_not_panicked() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);

    let escaping = WorkId("../../escaped-by-watch".to_string());
    match watch_first_bounded(&pointer.socket, escaping.clone(), Duration::from_secs(10)) {
        Some(Err(msg)) => {
            assert!(
                msg.contains("NotFound"),
                "expected a NotFound refusal for a path-like Work id, got: {msg}"
            );
        }
        other => panic!(
            "expected watch of a path-like Work id to be refused, not to panic, hang, or \
             silently close, got: {other:?}"
        ),
    }

    assert!(
        !estate.join("works").join("escaped-by-watch").exists(),
        "a path-like id must never escape the works/ namespace"
    );

    // The daemon must survive the request and keep answering — proof
    // the connection thread returned a reply instead of panicking.
    let (work_id, _run_id) = submit(&estate, &pointer.socket, "post-panic liveness check");
    assert!(estate.join("works").join(&work_id.0).exists());

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    drop(wirkd_child);
}

/// P3 foundation correction (0069, `foundation-verify/VERDICT.md`
/// "What is established"): a live-streamed `Event`'s own `EventId` is
/// nonempty and equals the id the journal actually persisted for it.
/// Already true of the candidate, but until now green only in an
/// independent probe, never in the suite (`identity_binding`'s count
/// held at 16 across the reconciliation) — pinned here so a future
/// edit cannot silently regress it.
#[test]
fn live_streamed_event_id_matches_the_persisted_event() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_id, run_id) = submit(&estate, &pointer.socket, "event id test");

    let (rx, _handle) = spawn_watch(&pointer.socket, &work_id);
    let first = recv_event(&rx, "the WorkSubmitted event submit already journaled");
    assert!(matches!(first.kind, EventKind::WorkSubmitted { .. }));

    record(&pointer.socket, &work_id, &run_id, EventKind::RunVanished);

    let mut live = None;
    for _ in 0..8 {
        let event = recv_event(&rx, "the live RunVanished append");
        if matches!(&event.kind, EventKind::RunVanished) {
            live = Some(event);
            break;
        }
    }
    let live = live.expect("the watcher must observe the live RunVanished append");
    assert!(
        !live.id.0.is_empty(),
        "a streamed EventId must not be empty"
    );

    // The journal persists each event inside a `{"seq", "event"}`
    // envelope (`Journal::append`'s own doc) — a private shape this
    // crate does not export, so the persisted id is read as raw JSON
    // rather than deserialized into `Event` directly.
    let journal_path = estate.join("works").join(&work_id.0).join("journal.ndjson");
    let contents = fs::read_to_string(&journal_path).expect("read persisted journal");
    let last_line = contents
        .lines()
        .last()
        .expect("journal has at least the RunVanished just appended");
    let envelope: serde_json::Value =
        serde_json::from_str(last_line).expect("parse persisted envelope");
    let persisted_id = envelope["event"]["id"]
        .as_str()
        .expect("persisted event carries an id");
    assert_eq!(
        persisted_id, live.id.0,
        "the streamed EventId must equal the journal's own persisted id"
    );

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    drop(wirkd_child);
}

/// Spawns the real `wirk wirkd watch --estate <estate>` CLI process
/// (discovery mode: every current Work) with its stdout piped one line
/// at a time into an `mpsc` channel — the same "reader thread, bounded
/// receive" shape `spawn_watch` uses for the client library directly,
/// applied here to the actual CLI binary rather than
/// `wirkd::client::watch`'s iterator.
fn spawn_cli_watch(estate: &Path) -> (std::process::Child, mpsc::Receiver<String>) {
    let mut child = Command::new(wirk_bin())
        .args(["wirkd", "watch", "--estate"])
        .arg(estate)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn wirk wirkd watch");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    (child, rx)
}

fn recv_line(rx: &mpsc::Receiver<String>, what: &str) -> String {
    rx.recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("never observed: {what}"))
}

/// P3 (0069 correction, `FINAL-REFUSAL-CORRECTION.md` item 1): the real
/// `wirk wirkd watch --work <id>` CLI — not just the client library's
/// iterator shape — must exit nonzero when the daemon explicitly
/// refuses the named Work, and must label that refusal as a refusal,
/// never as "malformed" (a well-formed `Reply::Err` is not a wire
/// protocol violation).
#[test]
fn cli_watch_of_unknown_work_exits_nonzero_and_labels_the_refusal() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let _pointer = wait_for_pointer(&estate);

    let output = Command::new(wirk_bin())
        .args(["wirkd", "watch", "--estate"])
        .arg(&estate)
        .args(["--work", "work-never-submitted"])
        .output()
        .expect("wirk wirkd watch runs");

    assert!(
        !output.status.success(),
        "watch of an explicitly unknown Work must exit nonzero, got status: {:?}, stdout: {}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refused") && stdout.contains("NotFound"),
        "expected the refusal labeled as such, naming the daemon's actual code, got stdout: {stdout}"
    );
    assert!(
        !stdout.contains("malformed"),
        "a valid daemon refusal must never be labeled malformed, got stdout: {stdout}"
    );

    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    drop(wirkd_child);
}

/// P3 (0069 correction, `FINAL-REFUSAL-CORRECTION.md` item 1): the
/// multi-Work discovery stream must keep serving a valid Work's own
/// events, live, after a sibling Work's watch is refused — and the
/// whole command's final exit still reflects that refusal. `work_b` is
/// never submitted through this daemon at all: only its bare directory
/// is created directly under `works/`, so `wirk wirkd watch`'s own
/// discovery (a plain directory listing, `list_work_ids`) names it, but
/// the daemon has no journal for it — neither on disk nor in its own
/// in-memory cache (submitting `work_b` through this same daemon and
/// then deleting its journal file does *not* reproduce this: `submit`
/// already cached the journal handle in memory, so the daemon would
/// keep serving it from that cache regardless of the file's removal —
/// watched directly and ruled out before writing this fixture this
/// way).
#[test]
fn cli_watch_multi_work_keeps_streaming_after_a_sibling_refusal_and_exits_nonzero() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        Command::new(wirk_bin())
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_a, run_a) = submit(&estate, &pointer.socket, "valid stream");
    let work_b_id = "work-manufactured-never-submitted".to_string();
    fs::create_dir_all(estate.join("works").join(&work_b_id))
        .expect("create work_b's bare, journal-less directory");

    let (mut cli, rx) = spawn_cli_watch(&estate);

    let mut saw_work_a_submitted = false;
    let mut saw_work_b_refused = false;
    for _ in 0..16 {
        if saw_work_a_submitted && saw_work_b_refused {
            break;
        }
        let line = recv_line(&rx, "work_a's own event or work_b's refusal");
        if line.starts_with(&format!("{} ", work_a.0)) && line.contains("WorkSubmitted") {
            saw_work_a_submitted = true;
        }
        if line.starts_with(&format!("{work_b_id} refused")) {
            assert!(line.contains("NotFound"), "expected NotFound, got: {line}");
            assert!(
                !line.contains("malformed"),
                "must not be labeled malformed, got: {line}"
            );
            saw_work_b_refused = true;
        }
    }
    assert!(
        saw_work_a_submitted,
        "work_a's own stream must keep serving despite work_b's refusal"
    );
    assert!(
        saw_work_b_refused,
        "work_b's own watch must be refused and labeled as such"
    );

    // work_a's stream is still alive after work_b's refusal: a fresh
    // live append must still arrive, unkilled.
    record(&pointer.socket, &work_a, &run_a, EventKind::RunVanished);
    let mut saw_live = false;
    for _ in 0..16 {
        let line = recv_line(&rx, "work_a's live RunVanished append");
        if line.starts_with(&format!("{} ", work_a.0)) && line.contains("RunVanished") {
            saw_live = true;
            break;
        }
    }
    assert!(
        saw_live,
        "work_a's stream must keep delivering live events after the sibling refusal"
    );

    // Ending wirkd ends work_a's own stream too (EOF); the whole CLI
    // process then exits on its own — no kill, no timeout on the
    // stream itself, only this test's own bound on waiting for it.
    let stop = Command::new(wirk_bin())
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());

    let status = cli.wait().expect("wirk wirkd watch exits on its own");
    assert!(
        !status.success(),
        "the whole command's exit must reflect work_b's refusal even though work_a streamed cleanly the whole time, got: {status:?}"
    );

    drop(wirkd_child);
}
