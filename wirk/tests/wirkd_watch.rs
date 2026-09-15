//! `wirkd watch` (item B, ruling 0044): one NDJSON `Event` line per
//! journal append of the named Work, starting with what is already
//! there. Drives a real `wirk wirkd start` child process (the same
//! discipline `wirkd_process.rs` uses) and dials it with `wirkd::
//! client::watch` directly — the client side is a plain library call
//! over the real socket, never a fake server (0040 D127).

#[path = "support/route_fixture.rs"]
mod route_fixture;
use wirk::wirkd;

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
            parent: None,
            execution_repo: None,
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
        let events =
            wirkd::client::watch(&socket, WatchPayload::admin(work_id)).expect("watch dials");
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
        let outcome = match wirkd::client::watch(&socket, WatchPayload::admin(work_id)) {
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
        wirk_cli()
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

    let stop = wirk_cli()
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
        wirk_cli()
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

    let stop = wirk_cli()
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
        wirk_cli()
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

    let stop = wirk_cli()
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
        wirk_cli()
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

    let stop = wirk_cli()
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
        wirk_cli()
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

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    drop(wirkd_child);
}

/// Ruling 0425 F2: `WatchPayload::work_id` became `Option<WorkId>` to
/// carry the estate-wide stream (ruling 0394), so
/// `{"verb":"watch","payload":{"admin":false}}` now deserializes where
/// it used to fail as JSON before that field existed. `handle_connection`
/// only branches to the estate handler when `admin && work_id.is_none()`,
/// so that payload used to fall through to `handle_watch_connection`,
/// which unwrapped `work_id` with `.expect(...)` — a reachable panic of
/// the connection thread, silent to the caller as a bare `EOF` (not
/// reachable from the CLI, which never builds this payload; reachable by
/// any socket peer). Constructs the malformed payload directly, since no
/// `WatchPayload` constructor produces this shape.
#[test]
fn non_admin_watch_naming_no_work_is_refused_not_panicked() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);

    let malformed = WatchPayload {
        work_id: None,
        requester: None,
        admin: false,
    };
    let (tx, rx) = mpsc::channel();
    let socket = pointer.socket.clone();
    std::thread::spawn(move || {
        let outcome = match wirkd::client::watch(&socket, malformed) {
            Ok(mut events) => events.next().map(|r| r.map_err(|err| err.to_string())),
            Err(err) => Some(Err(err.to_string())),
        };
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Some(Err(msg))) => {
            assert!(
                msg.contains("BadRequest"),
                "expected a labelled BadRequest refusal for a non-administrative watch \
                 naming no work, got: {msg}"
            );
        }
        other => panic!(
            "expected the malformed watch to be refused with a labelled BadRequest, not to \
             panic its connection thread, hang, or silently close, got: {other:?}"
        ),
    }

    // The daemon must survive the malformed request and keep answering —
    // proof the connection thread returned a reply instead of panicking.
    let (work_id, _run_id) = submit(
        &estate,
        &pointer.socket,
        "post-malformed-watch liveness check",
    );
    assert!(estate.join("works").join(&work_id.0).exists());

    let stop = wirk_cli()
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
        wirk_cli()
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

    let stop = wirk_cli()
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
    let mut child = wirk_cli()
        // The operator's own stream (ruling 0117), not the runner's
        // inherited actor context.
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
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

/// `spawn_cli_watch` with stderr piped too (one line at a time into its own
/// `mpsc`), so a test can wait on the estate stream's real subscription
/// barrier — the `subscribed` line the CLI prints once the daemon has
/// confirmed the dial (ruling 0404 F2) — instead of a sleep.
fn spawn_cli_watch_err(
    estate: &Path,
) -> (
    std::process::Child,
    mpsc::Receiver<String>,
    mpsc::Receiver<String>,
) {
    let mut child = wirk_cli()
        // The operator's own stream (ruling 0117), not the runner's
        // inherited actor context.
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
        .args(["wirkd", "watch", "--estate"])
        .arg(estate)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wirk wirkd watch");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if out_tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines() {
            match line {
                Ok(line) => {
                    if err_tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    (child, out_rx, err_rx)
}

/// Ruling 0394: the operator's unfiltered estate watch is dialed once and
/// must keep receiving durable events for Works submitted *after* the dial
/// — the per-Work discovery walk names only the Works present when it
/// lists the estate, so it can never see a Work that does not exist yet.
/// Red before this correction: a Work submitted after the dial is named by
/// no reader thread, so its `WorkSubmitted` never reaches the already-open
/// stream (the bounded `recv_line` below is the test's own termination
/// bound, not a product one — the required line is a positive event, and it
/// must arrive for the fixed stream).
#[test]
fn estate_watch_streams_a_work_submitted_after_the_dial() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_a, _run_a) = submit(&estate, &pointer.socket, "estate watch: existing work");

    // The operator's unfiltered stream, dialed while work_a exists. The
    // dial is proven live once it has replayed work_a's own journaled
    // events — no sleep, the readiness is the event itself.
    let (mut cli, rx) = spawn_cli_watch(&estate);
    let mut saw_work_a = false;
    for _ in 0..16 {
        let line = recv_line(&rx, "work_a's replayed WorkSubmitted");
        if line.starts_with(&format!("{} ", work_a.0)) && line.contains("WorkSubmitted") {
            saw_work_a = true;
            break;
        }
    }
    assert!(
        saw_work_a,
        "the estate stream must replay work_a's already-journaled events"
    );

    // The Work under test: submitted after the dial above.
    let (work_b, _run_b) = submit(&estate, &pointer.socket, "estate watch: new work");

    // Its `WorkSubmitted` must arrive on the same already-open stream, live.
    let mut saw_work_b = false;
    for _ in 0..16 {
        let line = recv_line(&rx, "work_b's WorkSubmitted, appended after the dial");
        if line.starts_with(&format!("{} ", work_b.0)) && line.contains("WorkSubmitted") {
            saw_work_b = true;
            break;
        }
    }
    assert!(
        saw_work_b,
        "a Work submitted after the dial must stream to the already-open estate watch (ruling 0394)"
    );

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());

    let status = cli.wait().expect("watch exits on its own");
    assert!(
        status.success(),
        "an unrefused estate stream ends clean on stop, got: {status:?}"
    );
    drop(wirkd_child);
}

/// Ruling 0394: an estate watch dialed on an estate with no Work yet stays
/// open and receives the first Work's events. Red before this correction:
/// the per-Work walk found no Work, so the command refused the empty estate
/// (exit 2) and the stream never opened at all — the first Work submitted
/// afterwards reaches nothing.
#[test]
fn estate_watch_streams_the_first_work_on_an_empty_estate() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);

    // No Work exists yet: the operator's stream still opens and stays open.
    // The barrier is the daemon's own confirmation, observed on the stream
    // (ruling 0404 F2): the CLI reports `subscribed` once the daemon has
    // registered this dial, which is what makes the first append below
    // guaranteed to reach it — a real connection/subscription state, not a
    // sleep that merely hopes the dial is up.
    let (mut cli, rx, err_rx) = spawn_cli_watch_err(&estate);
    let mut subscribed = false;
    for _ in 0..64 {
        let line = recv_line(&err_rx, "the estate stream's subscription barrier");
        if line.contains("subscribed") {
            subscribed = true;
            break;
        }
    }
    assert!(
        subscribed,
        "the estate stream must report its own subscription before the first Work is submitted (ruling 0404)"
    );

    // The estate's first Work, created after the barrier above.
    let (work_a, _run_a) = submit(&estate, &pointer.socket, "estate watch: first work");

    let mut saw_work_a = false;
    for _ in 0..16 {
        let line = recv_line(&rx, "work_a's WorkSubmitted, the estate's first Work");
        if line.starts_with(&format!("{} ", work_a.0)) && line.contains("WorkSubmitted") {
            saw_work_a = true;
            break;
        }
    }
    assert!(
        saw_work_a,
        "the first Work on an estate must stream to a watch dialed before it existed (ruling 0394)"
    );

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());

    let status = cli.wait().expect("watch exits on its own");
    assert!(
        status.success(),
        "an unrefused estate stream ends clean on stop, got: {status:?}"
    );
    drop(wirkd_child);
}

/// Ruling 0425 F3: `estate_drain`'s journal and replay errors named no
/// Work, though on a stream spanning the whole estate that is the one
/// piece of information an operator needs to locate the failure.
/// Corrupts one Work's own journal file directly — the same corruption
/// shape `journal_demo.rs`'s own corruption test builds, bypassing the
/// daemon entirely so the write is a genuine on-disk corruption, not a
/// crafted request — so the *replay* branch inside `estate_drain` fails,
/// and checks the estate stream's terminal `JournalError` names that
/// Work's own id, not just "journal"/"replay".
#[test]
fn estate_watch_journal_error_names_the_failing_work() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);

    let (work_id, _run_id) = submit(&estate, &pointer.socket, "corrupted-journal target");

    // Bypasses the daemon entirely: a hand-mangled line appended straight
    // to the file on disk, so `Journal::open` still succeeds (append mode,
    // no parse on open) and only `replay()` fails, once the estate stream
    // reaches this Work in its sorted walk of `works/`.
    let journal_path = estate.join("works").join(&work_id.0).join("journal.ndjson");
    {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&journal_path)
            .expect("open journal for corruption");
        writeln!(file, "not json").expect("write malformed line");
    }

    let events =
        wirkd::client::watch(&pointer.socket, WatchPayload::estate()).expect("estate watch dials");
    let mut terminal_err = None;
    for event in events {
        if let Err(err) = event {
            terminal_err = Some(err.to_string());
            break;
        }
    }
    let msg = terminal_err.expect(
        "the estate stream must end with a nonzero JournalError once it reaches the \
         corrupted journal, not a clean EOF or a silently empty history",
    );
    assert!(
        msg.contains("JournalError"),
        "expected a JournalError refusal, got: {msg}"
    );
    assert!(
        msg.contains(&work_id.0),
        "expected the JournalError to name the failing Work {}, got: {msg}",
        work_id.0
    );

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    drop(wirkd_child);
}

/// Spawns the real `watch` CLI with `--json`, stdout and stderr each
/// piped one line at a time into their own `mpsc` channel — the `--json`
/// contract is a whole-line contract on stdout, so the diagnostics it
/// must *not* appear in (stderr) need watching too.
fn spawn_cli_watch_json(
    estate: &Path,
    work: Option<&str>,
) -> (
    std::process::Child,
    mpsc::Receiver<String>,
    mpsc::Receiver<String>,
) {
    let mut command = wirk_cli();
    command.args(["wirkd", "watch", "--estate"]);
    command.arg(estate);
    if let Some(work) = work {
        command.args(["--work", work]);
    }
    command.arg("--json");
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch --json");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    let out_thread = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if out_tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    let err_thread = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines() {
            match line {
                Ok(line) => {
                    if err_tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    let _ = (out_thread, err_thread);
    (child, out_rx, err_rx)
}

/// The `--json` contract is a whole-line one: a stdout line that does
/// not parse as one JSON object on its own is not a parseable event, no
/// matter what it contains.
fn parse_whole_line(line: &str) -> serde_json::Value {
    serde_json::from_str(line)
        .unwrap_or_else(|err| panic!("stdout line is not one parseable Event: {err}: {line}"))
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
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let _pointer = wait_for_pointer(&estate);

    let output = wirk_cli()
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID")
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

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    drop(wirkd_child);
}

/// P3 (ruling 0394): the operator's unfiltered estate watch is one
/// stream, not a per-Work fan-out — so a bare, journal-less Work
/// directory contributes nothing to it (it is simply absent, never
/// refused), a valid Work's own events still replay and stream live, and
/// with no refusal to record the whole command exits clean. `work_b` is
/// never submitted through this daemon at all: only its bare directory is
/// created directly under `works/`, so it has no journal to replay — and
/// the estate stream has no per-Work admission to refuse, so it neither
/// errors nor contributes a line.
#[test]
fn cli_watch_estate_skips_a_journalless_sibling_and_streams_to_a_clean_exit() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let wirkd_child = KillOnDrop(
        wirk_cli()
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

    // work_a's replay must reach the one estate stream; a bare,
    // journal-less sibling must not — refused or otherwise — because the
    // estate stream has no per-Work admission to refuse.
    let mut saw_work_a_submitted = false;
    for _ in 0..16 {
        let line = recv_line(&rx, "work_a's own replayed event");
        assert!(
            !line.starts_with(&format!("{work_b_id} ")),
            "a bare, journal-less Work must not appear on the estate stream, refused or otherwise: {line}"
        );
        if line.starts_with(&format!("{} ", work_a.0)) && line.contains("WorkSubmitted") {
            saw_work_a_submitted = true;
            break;
        }
    }
    assert!(
        saw_work_a_submitted,
        "work_a's own replay must reach the estate stream"
    );

    // A live append after the dial must also reach the same stream.
    record(&pointer.socket, &work_a, &run_a, EventKind::RunVanished);
    let mut saw_live = false;
    for _ in 0..16 {
        let line = recv_line(&rx, "work_a's live RunVanished append");
        assert!(
            !line.starts_with(&format!("{work_b_id} ")),
            "a bare, journal-less Work must not appear on the estate stream, refused or otherwise: {line}"
        );
        if line.starts_with(&format!("{} ", work_a.0)) && line.contains("RunVanished") {
            saw_live = true;
            break;
        }
    }
    assert!(
        saw_live,
        "work_a's stream must keep delivering live events across the skipped sibling"
    );

    // Ending wirkd ends the estate stream (EOF); with no refusal recorded
    // anywhere, the whole command exits clean — the flip from the per-Work
    // refusal contract this correction removes.
    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());

    let status = cli.wait().expect("wirkd watch exits on its own");
    assert!(
        status.success(),
        "the estate watch must exit clean with no refusal to record, got: {status:?}"
    );

    drop(wirkd_child);
}

/// The `wirk` CLI with the *test runner's own* actor triple removed from
/// the child's environment.
///
/// `resolve_scope` reads `WIRK_ESTATE_ROOT`/`WIRK_WORK_ID`/`WIRK_RUN_ID`
/// to decide whether a call is an actor's own or an operator's, and a
/// test process inherits whatever its runner had. This suite is run from
/// inside a real actor pane often enough that an inherited triple makes
/// a fixture's administrative call against its own temp estate refuse as
/// a cross-estate read — so the fixture has to say which it is rather
/// than depend on who started it.
///
/// Sites that mean to act *as* an actor set the three back explicitly on
/// the returned command; a later `env` overrides this removal.
fn wirk_cli() -> Command {
    let mut command = Command::new(wirk_bin());
    command
        .env_remove("WIRK_ESTATE_ROOT")
        .env_remove("WIRK_WORK_ID")
        .env_remove("WIRK_RUN_ID");
    command
}

/// The advertised `--json` mode: every stdout line is one complete,
/// parseable `Event` object on its own — no `work_id` prefix, no prose —
/// so a reader can hand each line straight to its JSON parser. The
/// `work`/`run` identity the prefix used to carry is already fields of
/// the event itself. Replay starts at the first journaled event, and a
/// live append made after the dial arrives as one more such line.
#[test]
fn cli_watch_json_streams_one_parseable_event_per_line() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let mut wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_id, run_id) = submit(&estate, &pointer.socket, "json watch replay");

    let (mut cli, out, err) = spawn_cli_watch_json(&estate, Some(work_id.0.as_str()));

    // Replay: the first journaled event must be the first line, whole.
    let first = parse_whole_line(&recv_line(&out, "the first json watch line"));
    assert_eq!(
        first["kind"]["kind"].as_str(),
        Some("WorkSubmitted"),
        "the first replayed line must be the first journaled event"
    );
    assert_eq!(
        first["work"].as_str(),
        Some(work_id.0.as_str()),
        "the event must carry its own Work identity: {first}"
    );

    // A live append while the stream is open arrives as one more
    // parseable line.
    record(&pointer.socket, &work_id, &run_id, EventKind::RunVanished);
    let mut saw_live = false;
    for _ in 0..8 {
        let event = parse_whole_line(&recv_line(&out, "the live RunVanished line"));
        if event["kind"]["kind"].as_str() == Some("RunVanished") {
            assert_eq!(
                event["run"].as_str(),
                Some(run_id.0.as_str()),
                "the live event must carry its own Run identity: {event}"
            );
            saw_live = true;
            break;
        }
    }
    assert!(saw_live, "the live append must arrive as a parseable line");

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());

    let status = cli.wait().expect("watch exits on EOF");
    assert!(
        status.success(),
        "an unrefused json stream ends clean when the daemon stops, got: {status:?}"
    );
    for line in err {
        assert!(
            !line.contains("refused"),
            "an unrefused stream must not report a refusal: {line}"
        );
    }
    let _ = wirkd_child.0.wait();
    drop(wirkd_child);
}

/// In `--json` mode a refusal is not a stream line: stdout stays empty
/// for the refused Work, the refusal reaches stderr naming the daemon's
/// own code, and the exit still reflects it.
#[test]
fn cli_watch_json_of_unknown_work_refuses_on_stderr_and_prints_no_stdout() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let mut wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wirkd"),
    );
    let _pointer = wait_for_pointer(&estate);

    let output = wirk_cli()
        .args(["wirkd", "watch", "--estate"])
        .arg(&estate)
        .args(["--work", "work-never-submitted"])
        .arg("--json")
        .output()
        .expect("watch --json runs");

    assert!(
        !output.status.success(),
        "a refused json watch must exit nonzero, got: {:?}, stdout: {}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "a refused Work has no event to print; stdout must stay empty, got: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refused") && stderr.contains("NotFound"),
        "the refusal must reach stderr naming the daemon's own code, got: {stderr}"
    );
    assert!(
        !stderr.contains("malformed"),
        "a valid daemon refusal must never be labeled malformed, got: {stderr}"
    );

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());
    let _ = wirkd_child.0.wait();
    drop(wirkd_child);
}

/// P3 (ruling 0394): the operator's unfiltered estate watch, in
/// `--json` mode, is one parseable stream — not a per-Work fan-out — so
/// a bare, journal-less Work directory contributes no line to it (the
/// stream has no per-Work admission to refuse), a valid Work's own events
/// still replay and stream live, and with no refusal to record the whole
/// command exits clean. Every stdout line still parses as one event of the
/// admitted Work, because a journal-less sibling has no events to name
/// itself.
#[test]
fn cli_watch_json_estate_skips_a_journalless_sibling_and_streams_to_a_clean_exit() {
    let dir = tempfile::tempdir().expect("estate tempdir");
    let estate = dir.path().to_path_buf();
    let mut wirkd_child = KillOnDrop(
        wirk_cli()
            .args(["wirkd", "start", "--estate"])
            .arg(&estate)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn watch cli"),
    );
    let pointer = wait_for_pointer(&estate);
    let (work_a, run_a) = submit(&estate, &pointer.socket, "json valid stream");
    let work_b_id = "work-json-manufactured-never-submitted".to_string();
    fs::create_dir_all(estate.join("works").join(&work_b_id))
        .expect("create work_b's bare, journal-less directory");

    let (mut cli, out, err) = spawn_cli_watch_json(&estate, None);

    // work_a's replay must reach the one estate stream, and every line it
    // sends still parses as one event of the admitted Work — a journal-less
    // sibling has no events to name itself, so it names no Work here.
    let mut saw_work_a = false;
    for _ in 0..16 {
        if saw_work_a {
            break;
        }
        let line = recv_line(&out, "work_a's own event line");
        let event = parse_whole_line(&line);
        assert_eq!(
            event["work"].as_str(),
            Some(work_a.0.as_str()),
            "stdout must carry only the admitted Work's events, a bare sibling having none: {line}"
        );
        assert_ne!(
            event["work"].as_str(),
            Some(work_b_id.as_str()),
            "a bare, journal-less Work must not appear on the estate stream: {line}"
        );
        if event["kind"]["kind"].as_str() == Some("WorkSubmitted") {
            saw_work_a = true;
        }
    }
    assert!(
        saw_work_a,
        "work_a's own replay must reach the estate stream"
    );

    record(&pointer.socket, &work_a, &run_a, EventKind::RunVanished);
    let mut saw_live = false;
    for _ in 0..16 {
        let line = recv_line(&out, "work_a's live RunVanished line");
        let event = parse_whole_line(&line);
        if event["kind"]["kind"].as_str() == Some("RunVanished") {
            saw_live = true;
            break;
        }
    }
    assert!(
        saw_live,
        "work_a must keep delivering parseable live events across the skipped sibling"
    );

    let stop = wirk_cli()
        .args(["wirkd", "stop", "--estate"])
        .arg(&estate)
        .output()
        .expect("wirkd stop runs");
    assert!(stop.status.success());

    let status = cli.wait().expect("watch exits on its own");
    assert!(
        status.success(),
        "the estate watch must exit clean with no refusal to record, got: {status:?}"
    );

    // The estate stream has no per-Work admission to refuse, so it records
    // no refusal diagnostic at all; the stderr channel closes once the
    // process has (asserted above) exited, so this drain ends.
    for line in err {
        assert!(
            !line.contains("refused"),
            "the estate stream must not record a refusal it never makes: {line}"
        );
    }
    let _ = wirkd_child.0.wait();
    drop(wirkd_child);
}
