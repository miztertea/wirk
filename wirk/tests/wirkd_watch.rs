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

use wirk_core::{Event, EventKind, RepositoryBinding, WorkId};

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
fn submit(estate: &Path, socket: &Path, intent: &str) -> WorkId {
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
            kind: None,
            command: None,
            repo_path: None,
            route: Some("smoke".to_string()),
        }),
    )
    .expect("submit call reaches wirkd");
    match reply {
        Reply::Ok { result, .. } => WorkId(
            result["work_id"]
                .as_str()
                .expect("submit result carries work_id")
                .to_string(),
        ),
        Reply::Err { error, .. } => panic!("submit refused: {} {}", error.code, error.message),
    }
}

/// Records one more `EventKind` onto `work_id`'s journal via the
/// `record` verb — the same write path `RunLoop` itself uses.
fn record(socket: &Path, work_id: &WorkId, kind: EventKind) {
    let reply = wirkd::client::call(
        socket,
        &Request::record(wirkd::RecordPayload {
            work_id: work_id.clone(),
            run: None,
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
    let work_id = submit(&estate, &pointer.socket, "watch test");

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
    record(
        &pointer.socket,
        &work_id,
        EventKind::LifecycleObserved {
            status: "Working".to_string(),
        },
    );
    // Drain whatever `submit` itself wrote (WaypointReserved, RunOpened)
    // before the live one this call just appended.
    let mut saw_live = false;
    for _ in 0..8 {
        let event = recv_event(&rx_before, "the live LifecycleObserved append");
        if matches!(&event.kind, EventKind::LifecycleObserved { status } if status == "Working") {
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
        let event = recv_event(&rx_after, "the earlier live LifecycleObserved, replayed");
        if matches!(&event.kind, EventKind::LifecycleObserved { status } if status == "Working") {
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
    let work_a = submit(&estate, &pointer.socket, "work a");
    let work_b = submit(&estate, &pointer.socket, "work b");

    let (rx_a, _handle_a) = spawn_watch(&pointer.socket, &work_a);
    // Drain work_a's own submit-time events first.
    loop {
        let event = recv_event(&rx_a, "work_a's own submit-time events");
        if matches!(event.kind, EventKind::RunOpened { .. }) {
            break;
        }
    }

    record(
        &pointer.socket,
        &work_b,
        EventKind::LifecycleObserved {
            status: "should-never-reach-a".to_string(),
        },
    );
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
    let work_id = submit(&estate, &pointer.socket, "watch eof test");

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
