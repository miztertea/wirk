//! wirkd server loop: listener, pointer file, verb dispatch, one
//! `Mutex<Journal>` per Work (W3, `orient/transport.md` §3-5, §6;
//! `orient/build-brief.md` §3 W3). `run` binds the Unix domain socket,
//! writes the pointer file (0022 D79) once the listener is bound, then
//! accepts connections thread-per-connection (R3 `std::thread`); each
//! connection reads one NDJSON request line, dispatches by `Verb`, and
//! writes one NDJSON reply line. `WirkdState::journals` holds one
//! `Arc<Mutex<Journal>>` per `WorkId`, opened on first touch: the outer
//! `Mutex<HashMap<..>>` is held only long enough to fetch-or-insert that
//! entry, never across an append — two different Works' journals are
//! never serialized behind one lock (transport.md §5, sergeant issues
//! 334/358 answered by construction).
//!
//! `submit` constructs one hardcoded "smoke" `Route` in code (R6, no
//! Route-authoring format built yet): `RouteId("smoke")`, one
//! `WaypointDefinition { kind: Actor, declared_outputs: [{name:
//! "report.md", required: true}] }`. `claim` locates the Work's journal
//! by the triple's `work_id`, folds it to find the named `Run` (a
//! `RunId` with no matching `RunOpened` is `Refused(TripleMismatch)`,
//! recorded not honored — D9#4), then runs `validate_claim`, then two
//! checks the stub signature cannot reach itself (`orient/validate.md`
//! §3): the triple's `work_id` against the folded `Work.id`, and, when
//! the reserved `World` carries a `worktree_path`, that each claimed
//! artifact's path exists on disk beneath it (build-brief amendment 3).
//!
//! W3 (`orient/build-brief.md` §3 W3): `submit`'s `SubmitPayload.kind ==
//! Some("deterministic")` reserves a `World::Deterministic` instead of
//! the always-`Actor` World every earlier wave built, from `--command`
//! (`wirk work submit --kind deterministic --command <argv...>`);
//! `status` grows `run_id`/`attempt`/`world_hash`/`run_state`/`world`
//! fields alongside the ones W3 (item 3) already returned, so `wirk
//! run-deterministic` can read the reserved World back without
//! recompiling it (`orient/child.md` §7 item 2: "wirkd's own
//! Route-runner owns the loop"; the Route-runner itself, `run-
//! deterministic`, is a separate `wirk` invocation in the `wirk` bin,
//! not code living inside this server). `fail` is the new verb that
//! process uses to journal a `RunFailed` for a local executor failure
//! it has no other way to write to the journal (`FailPayload`'s own
//! doc).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use wirk_core::{
    ActorWorld, ArtifactRef, ArtifactSpec, Boundary, Claim, ClaimId, ClaimKind, ClaimRefusal,
    ClaimVerdict, DeterministicWorld, Event, EventKind, ExecutionTriple, FailureCause, Journal,
    JournalError, OutputContract, Route, RouteId, Run, RunId, RunState, Timestamp,
    WaypointDefinition, WaypointId, WaypointKind, WorkId, WorkState, World, WorldHash, fold,
    validate_claim,
};

use super::{
    ClaimPayload, ErrorDetail, FailPayload, RecordPayload, Reply, Request, StatusPayload,
    SubmitPayload, Verb, WirkdPointer,
};

/// Envelope reply plus what the server does after writing it: `stop`
/// removes the pointer/socket files and exits the process, everything
/// else keeps serving.
enum Outcome {
    Reply(Reply),
    Stop(Reply),
}

/// The wire protocol's own version, carried in `ping`'s reply and the
/// pointer file (transport.md §2-3).
const PROTOCOL_VERSION: u32 = 1;

/// The hardcoded "smoke" Route's single Waypoint (R6, build-brief.md §3
/// W3): every `submit` reserves this one Waypoint, every `claim`
/// validates against it. A Route-authoring format is not this item's.
fn smoke_waypoint(id: WaypointId) -> WaypointDefinition {
    WaypointDefinition {
        id,
        kind: WaypointKind::Actor,
        declared_outputs: vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }],
    }
}

/// The hardcoded "proving" Route (item 8, `orient/route.md` §1, R6):
/// two Waypoints, wp-1 Actor (writes `report.md`), wp-2 Deterministic
/// (counts its lines into `summary.md`) — a second smoke-shaped Route,
/// named as scaffolding beside `smoke_waypoint`, same footing (0034
/// D107 "holds until a Route file format exists"). Selected by
/// `SubmitPayload.route == Some("proving")`; `handle_claim`'s
/// auto-advance reserves wp-2 once wp-1's Claim is Validated.
fn proving_route() -> Route {
    Route {
        id: RouteId("proving".to_string()),
        waypoints: vec![
            WaypointDefinition {
                id: WaypointId("proving/wp-1".to_string()),
                kind: WaypointKind::Actor,
                declared_outputs: vec![ArtifactSpec {
                    name: "report.md".to_string(),
                    required: true,
                }],
            },
            WaypointDefinition {
                id: WaypointId("proving/wp-2".to_string()),
                kind: WaypointKind::Deterministic,
                declared_outputs: vec![ArtifactSpec {
                    name: "summary.md".to_string(),
                    required: true,
                }],
            },
        ],
    }
}

/// wp-2's fixed command (R6, `orient/route.md` §1): hardcoded alongside
/// the Route itself, since `WaypointDefinition` carries no command
/// field — that lives on `DeterministicWorld`, built once at
/// reservation, not on the Route (no core-type change this item).
/// Redirect form so `summary.md` holds only the line count, not
/// `report.md`'s own name.
const PROVING_WP2_COMMAND: [&str; 3] = ["sh", "-c", "wc -l < report.md > summary.md"];

/// Looks up the hardcoded Route named by `id` (R6, `orient/route.md`
/// §2): `"proving"` selects the two-Waypoint proving Route above,
/// anything else (including `"smoke"`, the default) the original
/// one-Waypoint smoke Route. `handle_claim`'s validation and
/// auto-advance both key off this instead of calling `smoke_waypoint`
/// directly, so a Work submitted on a non-smoke Route is validated
/// against its own Waypoints, not the smoke one.
fn route_definition(id: &RouteId) -> Route {
    if id.0 == "proving" {
        return proving_route();
    }
    Route {
        id: RouteId("smoke".to_string()),
        waypoints: vec![smoke_waypoint(WaypointId("smoke/wp-1".to_string()))],
    }
}

/// The ordered Waypoint ids `WorkSubmitted` named for this Work — the
/// same field `wirk_core::fold`'s own reducer reads into its private
/// `route_waypoints` local (`wirk-core/src/lib.rs`), inlined here
/// because `server.rs` only has raw `events`, not `fold`'s locals
/// (`orient/route.md` §2, R6). Empty when no `WorkSubmitted` is present
/// (a fabricated/stale journal — never this server's own writes).
fn route_waypoints(events: &[Event]) -> Vec<WaypointId> {
    events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::WorkSubmitted { waypoints, .. } => Some(waypoints.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The submitted `--command` argv this Work's `WorkSubmitted` carried
/// (`orient/route.md` §2/§5, R2 shape of `route_waypoints` above):
/// `None` when the submit carried no `--command` (or no `WorkSubmitted`
/// is present at all) — `handle_claim`'s auto-advance falls back to
/// `PROVING_WP2_COMMAND` itself in that case, kept out of this function
/// so it stays a pure read of the journal, same shape as
/// `route_waypoints`.
fn wp2_command_for(events: &[Event]) -> Option<Vec<String>> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::WorkSubmitted { wp2_command, .. } => wp2_command.clone(),
        _ => None,
    })
}

/// Everything wirkd can fail at during startup — bind, pointer write, or
/// opening the first Journal it touches. Reported on stderr with cause
/// and detail, exit 2 (issue 275) — the caller (`main.rs`) does the
/// printing; this type only carries what happened.
#[derive(Debug)]
pub enum WirkdError {
    Bind { socket: PathBuf, source: io::Error },
    Pointer { path: PathBuf, source: io::Error },
    Journal(JournalError),
}

impl fmt::Display for WirkdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WirkdError::Bind { socket, source } => {
                write!(
                    f,
                    "cause: bind socket; detail: {}: {source}",
                    socket.display()
                )
            }
            WirkdError::Pointer { path, source } => {
                write!(
                    f,
                    "cause: write pointer; detail: {}: {source}",
                    path.display()
                )
            }
            WirkdError::Journal(source) => write!(f, "cause: open journal; detail: {source}"),
        }
    }
}

impl std::error::Error for WirkdError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WirkdError::Bind { source, .. } | WirkdError::Pointer { source, .. } => Some(source),
            WirkdError::Journal(source) => Some(source),
        }
    }
}

impl From<JournalError> for WirkdError {
    fn from(err: JournalError) -> Self {
        WirkdError::Journal(err)
    }
}

/// Per-estate state: one `Arc<Mutex<Journal>>` per Work, keyed by
/// `WorkId`, opened the first time `submit` or `claim` touches it
/// (transport.md §5).
struct WirkdState {
    estate_root: PathBuf,
    journals: Mutex<HashMap<WorkId, Arc<Mutex<Journal>>>>,
    /// One `Sender<Event>` per live `watch` connection on a Work (item B,
    /// ruling 0044): registered under the Work's own `Mutex<Journal>`
    /// (`handle_watch_connection`, so a watcher dialing in never misses
    /// an event appended between its replay and its registration), fed
    /// by `append_event` under that same lock (so notification is
    /// ordered with the append it announces, never racing a second
    /// concurrent writer). A dead receiver (the client hung up, or its
    /// thread's write to the socket failed) is pruned lazily, the next
    /// time `append_event` tries to send to it and gets `Err` — no
    /// separate deregistration path, no timer.
    watchers: Mutex<HashMap<WorkId, Vec<std::sync::mpsc::Sender<Event>>>>,
}

/// Appends `event` to `journal`, then hands a clone to every live
/// `watch` connection on `work_id`, pruning any whose receiver is gone
/// (module doc on `WirkdState::watchers`). Notification happens while
/// `journal`'s own lock (the caller's `MutexGuard`) is still held, so a
/// watch connection that registers between two calls to this function
/// either sees the earlier event in its own replay or is registered in
/// time to receive this one — never neither (item B: "a client
/// connected before an append receives it; a client connecting after
/// receives the earlier lines first").
fn append_event(
    state: &Arc<WirkdState>,
    journal: &mut Journal,
    work_id: &WorkId,
    event: &Event,
) -> Result<(), JournalError> {
    journal.append(event)?;
    let mut watchers = state
        .watchers
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(senders) = watchers.get_mut(work_id) {
        senders.retain(|tx| tx.send(event.clone()).is_ok());
    }
    Ok(())
}

/// Binds `estate_root/.wirk/wirkd.sock`, writes the pointer file, then
/// serves forever, one thread per connection. Returns only on a bind or
/// pointer-write failure — the accept loop itself never returns short of
/// a `stop` request, which exits the process directly (transport.md §2:
/// "wirkd removes the pointer and socket file and exits after the reply
/// is flushed").
pub fn run(estate_root: PathBuf) -> Result<(), WirkdError> {
    let wirk_dir = estate_root.join(".wirk");
    std::fs::create_dir_all(&wirk_dir).map_err(|source| WirkdError::Bind {
        socket: wirk_dir.join("wirkd.sock"),
        source,
    })?;
    let socket_path = wirk_dir.join("wirkd.sock");
    let listener = bind_socket(&socket_path).map_err(|source| WirkdError::Bind {
        socket: socket_path.clone(),
        source,
    })?;
    write_pointer(&estate_root, &socket_path, std::process::id())?;

    let state = Arc::new(WirkdState {
        estate_root,
        journals: Mutex::new(HashMap::new()),
        watchers: Mutex::new(HashMap::new()),
    });

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        let socket_path = socket_path.clone();
        std::thread::spawn(move || handle_connection(stream, &state, &socket_path));
    }
    Ok(())
}

/// A socket file left over from a wirkd that did not shut down cleanly
/// refuses `bind` with `AddrInUse`; if nothing accepts a connection at
/// that path, it is stale and removed before the real `bind` (0032 D99:
/// no supervisor, so this is wirkd's own startup housekeeping, not a
/// liveness protocol).
fn bind_socket(socket_path: &Path) -> io::Result<UnixListener> {
    if socket_path.exists() && UnixStream::connect(socket_path).is_err() {
        let _ = std::fs::remove_file(socket_path);
    }
    UnixListener::bind(socket_path)
}

/// Writes `<estate_root>/.wirk/wirkd.json` atomically (temp file then
/// rename), owner-only (0600), and a copy to
/// `$HERDR_PLUGIN_STATE_DIR/wirkd.json` when that variable is set (D79).
/// Called only after the listener is already bound (transport.md §3:
/// "before wirkd does anything else observable").
fn write_pointer(estate_root: &Path, socket: &Path, pid: u32) -> Result<(), WirkdError> {
    let pointer = WirkdPointer {
        schema: "wirkd.pointer/v1".to_string(),
        socket: socket.to_path_buf(),
        pid,
        protocol_version: PROTOCOL_VERSION,
    };
    let bytes = serde_json::to_vec(&pointer).expect("WirkdPointer always serializes");

    let dir = estate_root.join(".wirk");
    write_pointer_copy(&dir.join("wirkd.json"), &bytes)?;

    if let Ok(plugin_dir) = std::env::var("HERDR_PLUGIN_STATE_DIR") {
        let plugin_dir = PathBuf::from(plugin_dir);
        if std::fs::create_dir_all(&plugin_dir).is_ok() {
            let _ = write_pointer_copy(&plugin_dir.join("wirkd.json"), &bytes);
        }
    }
    Ok(())
}

fn write_pointer_copy(path: &Path, bytes: &[u8]) -> Result<(), WirkdError> {
    let tmp = path.with_extension("json.tmp");
    let write = |tmp: &Path| -> io::Result<()> {
        std::fs::write(tmp, bytes)?;
        std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(tmp, path)
    };
    write(&tmp).map_err(|source| WirkdError::Pointer {
        path: path.to_path_buf(),
        source,
    })
}

/// Removes the pointer file(s) and the socket on a clean `stop`
/// (transport.md §3).
fn remove_pointer_and_socket(estate_root: &Path, socket_path: &Path) {
    let _ = std::fs::remove_file(estate_root.join(".wirk").join("wirkd.json"));
    if let Ok(plugin_dir) = std::env::var("HERDR_PLUGIN_STATE_DIR") {
        let _ = std::fs::remove_file(PathBuf::from(plugin_dir).join("wirkd.json"));
    }
    let _ = std::fs::remove_file(socket_path);
}

/// Reads one NDJSON request line, dispatches it, writes one NDJSON
/// reply line. A malformed request line gets a `BadRequest` error reply
/// rather than dropping the connection silently. `stop` writes its
/// reply, flushes, then removes the pointer/socket and exits the whole
/// process (transport.md §2).
fn handle_connection(stream: UnixStream, state: &Arc<WirkdState>, socket_path: &Path) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }

    let request = match serde_json::from_str::<Request>(line.trim_end()) {
        Ok(request) => request,
        Err(err) => {
            write_one_reply(&stream, &err_reply("BadRequest", &err.to_string()));
            return;
        }
    };

    // `watch` (item B) is a long-lived, many-lines-out connection, not
    // the one-request-one-reply shape every other verb uses — it never
    // returns an `Outcome`, and ends only when the client hangs up or
    // this process exits (ruling 0044: no read timeout, no poll).
    if request.verb == Verb::Watch {
        match serde_json::from_value::<super::WatchPayload>(request.payload) {
            Ok(payload) => handle_watch_connection(stream, state, payload),
            Err(err) => write_one_reply(&stream, &err_reply("BadRequest", &err.to_string())),
        }
        return;
    }

    let outcome = dispatch(&request, state);

    let reply = match &outcome {
        Outcome::Reply(reply) | Outcome::Stop(reply) => reply,
    };
    let mut bytes = serde_json::to_vec(reply).expect("Reply always serializes");
    bytes.push(b'\n');
    let mut writer = &stream;
    let _ = writer.write_all(&bytes);
    let _ = writer.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);

    if matches!(outcome, Outcome::Stop(_)) {
        remove_owned_containers(&state.estate_root);
        remove_pointer_and_socket(&state.estate_root, socket_path);
        std::process::exit(0);
    }
}

/// `docker rm -f`s every `io.wirk.managed` container whose run id
/// appears in one of this estate's Work journals (W3, build-brief
/// outcome: "wirkd stop removes any io.wirk.managed containers whose
/// run ids appear in its journals"). Exact-owned, the same discipline
/// `DockerExecutor::remove_owned` uses (`orient/docker.md` §1, §4):
/// never a blind `docker rm` by a derived name alone — `managed_
/// container_names` below is checked first, so only a container the
/// daemon itself reports as `io.wirk.managed=true` is ever touched.
/// Best-effort throughout: no `docker` binary, no daemon, or an empty
/// estate (no `works/` directory yet) all mean nothing to remove, not
/// an error — a clean `stop` must not fail because docker is absent
/// from a box that never ran a `DockerExecutor` Run at all. This is a
/// separate scan from any live `DockerExecutor`'s own in-memory `runs`
/// map (`docker.rs`, out of this wave's allow-list): those only track
/// Runs launched by *that* process's own instance, never the ones a
/// separate, already-exited `wirk run-deterministic` invocation
/// launched — journal-derived is the only owner-agnostic source wirkd
/// itself has (`orient/docker.md` §4: "the name is journaled, not only
/// held in memory").
fn remove_owned_containers(estate_root: &Path) {
    let works_dir = estate_root.join("works");
    let Ok(entries) = std::fs::read_dir(&works_dir) else {
        return;
    };
    let mut run_ids: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(journal) = Journal::open(&dir) else {
            continue;
        };
        let Ok(events) = journal.replay() else {
            continue;
        };
        for event in &events {
            if let EventKind::RunOpened { run, .. } = &event.kind {
                run_ids.push(run.0.clone());
            }
        }
    }
    if run_ids.is_empty() {
        return;
    }
    let managed = managed_container_names();
    for run_id in run_ids {
        let name = format!("wirk-{run_id}");
        if managed.contains(&name) {
            let _ = std::process::Command::new("docker")
                .arg("rm")
                .arg("-f")
                .arg(&name)
                .output();
        }
    }
}

/// `docker ps -a --filter label=io.wirk.managed=true --format
/// '{{.Names}}'`, one name per line — an empty set (no `docker`
/// binary, no daemon reachable, or genuinely none managed) is not an
/// error here, just nothing to remove.
fn managed_container_names() -> std::collections::HashSet<String> {
    let output = std::process::Command::new("docker")
        .arg("ps")
        .arg("-a")
        .arg("--filter")
        .arg("label=io.wirk.managed=true")
        .arg("--format")
        .arg("{{.Names}}")
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        _ => Default::default(),
    }
}

fn dispatch(request: &Request, state: &Arc<WirkdState>) -> Outcome {
    match request.verb {
        Verb::Ping => Outcome::Reply(handle_ping()),
        Verb::Submit => match serde_json::from_value::<SubmitPayload>(request.payload.clone()) {
            Ok(payload) => Outcome::Reply(handle_submit(state, payload)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::Claim => match serde_json::from_value::<ClaimPayload>(request.payload.clone()) {
            Ok(payload) => Outcome::Reply(handle_claim(state, payload)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::Status => match serde_json::from_value::<StatusPayload>(request.payload.clone()) {
            Ok(payload) => Outcome::Reply(handle_status(state, payload)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::Fail => match serde_json::from_value::<FailPayload>(request.payload.clone()) {
            Ok(payload) => Outcome::Reply(handle_fail(state, payload)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::Record => match serde_json::from_value::<RecordPayload>(request.payload.clone()) {
            Ok(payload) => Outcome::Reply(handle_record(state, payload)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::Stop => Outcome::Stop(ok_reply(json!({}))),
        // `handle_connection` intercepts `watch` before ever calling
        // `dispatch` (its own long-lived, many-lines-out shape does not
        // fit `Outcome`) — this arm exists only so the match stays
        // exhaustive against a `Verb` this function is never actually
        // handed for.
        Verb::Watch => Outcome::Reply(err_reply(
            "Internal",
            "watch is not dispatched through this path",
        )),
    }
}

/// One-shot fast path for a reply that has to go out before (or instead
/// of) the normal `handle_connection` write-then-shutdown sequence — a
/// malformed `watch` payload, in practice, so far.
fn write_one_reply(stream: &UnixStream, reply: &Reply) {
    let mut bytes = serde_json::to_vec(reply).expect("Reply always serializes");
    bytes.push(b'\n');
    let mut writer = stream;
    let _ = writer.write_all(&bytes);
    let _ = writer.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// `watch` (item B): replays the Work's journal, registers this
/// connection as a watcher of it (both under the Work's own journal
/// lock, `WirkdState::watchers`' own doc comment — no append can land
/// between the replay and the registration), writes one NDJSON `Event`
/// line per already-present event, then blocks on the channel
/// (`Receiver::recv`, no timeout — ruling 0044) writing one more line
/// per event appended after that, until the client hangs up (a write
/// fails) or this process exits (the socket closes with it, an `EOF`
/// for the client — same "closed stream is the peer's death" reading
/// D134 gives Herdr's own subscription).
fn handle_watch_connection(
    stream: UnixStream,
    state: &Arc<WirkdState>,
    payload: super::WatchPayload,
) {
    let work_id = payload.work_id;
    let journal = match journal_for(state, &work_id) {
        Ok(journal) => journal,
        Err(err) => {
            write_one_reply(&stream, &err_reply("JournalError", &err.to_string()));
            return;
        }
    };

    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    let existing = {
        let journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
        let existing = match journal.replay() {
            Ok(events) => events,
            Err(err) => {
                write_one_reply(&stream, &err_reply("JournalError", &err.to_string()));
                return;
            }
        };
        // Registered while `journal`'s lock is still held (module doc):
        // `append_event` takes the same lock before it ever sends, so an
        // append cannot land between this `replay()` and this
        // registration.
        state
            .watchers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(work_id.clone())
            .or_default()
            .push(tx);
        existing
    };

    let mut writer = &stream;
    for event in &existing {
        if write_event_line(&mut writer, event).is_err() {
            return;
        }
    }
    // `rx` is dropped on every return path below, which is what makes
    // `append_event`'s next `tx.send` on this Work fail and prune this
    // dead entry (`WirkdState::watchers`' own doc comment) — no separate
    // deregistration call.
    while let Ok(event) = rx.recv() {
        if write_event_line(&mut writer, &event).is_err() {
            return;
        }
    }
    // `Err` from `recv` means every `Sender` for this Work is gone —
    // only possible if this process is shutting down (nothing else ever
    // drops the map's own copy) — the connection ends the same as a
    // client hangup: the socket simply closes when this function
    // returns.
}

/// One NDJSON line per `Event`, raw — not wrapped in the request/reply
/// `Reply` envelope (this is not a reply to anything; it is a push),
/// matching `handle_watch_connection`'s own doc.
fn write_event_line(writer: &mut &UnixStream, event: &Event) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(event).expect("Event always serializes");
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

fn handle_ping() -> Reply {
    ok_reply(json!({
        "protocol_version": PROTOCOL_VERSION,
        "pid": std::process::id(),
    }))
}

fn handle_submit(state: &Arc<WirkdState>, payload: SubmitPayload) -> Reply {
    let work_id = WorkId(mint_id("work"));
    let run_id = RunId(mint_id("run"));
    // `--route proving` selects the two-Waypoint proving Route (item 8,
    // `orient/route.md` §2); absent or any other value keeps today's
    // one-Waypoint "smoke" Route (R6, additive — every existing caller
    // that never sets `route` is unaffected). Only wp-1 is reserved and
    // opened here either way; wp-2 (proving only) is reserved by
    // `handle_claim`'s auto-advance once wp-1's Claim is Validated.
    let proving = payload.route.as_deref() == Some("proving");
    let route = if proving {
        proving_route()
    } else {
        route_definition(&RouteId("smoke".to_string()))
    };
    let route_id = route.id.clone();
    let waypoint_id = route.waypoints[0].id.clone();
    let all_waypoints: Vec<WaypointId> = route.waypoints.iter().map(|w| w.id.clone()).collect();

    let triple = ExecutionTriple {
        estate_root: state.estate_root.display().to_string(),
        work_id: work_id.clone(),
        run_id: run_id.clone(),
    };
    let output_contract = OutputContract(vec![ArtifactSpec {
        name: "report.md".to_string(),
        required: true,
    }]);
    let branch = format!("wirk/{}", work_id.0);

    // W3 (build-brief.md §3 W3, both items): `--kind deterministic
    // --command <argv...>` (item 5) reserves a `World::Deterministic`;
    // `--kind actor --repo-path <path>` (item 4) reserves an `ActorWorld`
    // with `base_ref` resolved to a commit SHA and an empty
    // `worktree_path` filled in later by `wirk run`; `kind` absent or
    // any other value keeps today's original "smoke" World unchanged —
    // `worktree_path: state.estate_root`, the raw unresolved `base_ref`.
    let deterministic = payload.kind.as_deref() == Some("deterministic");
    if deterministic && payload.command.as_ref().is_none_or(|c| c.is_empty()) {
        return err_reply(
            "BadRequest",
            "work submit --kind deterministic requires a non-empty --command",
        );
    }

    let world = if deterministic {
        World::Deterministic(DeterministicWorld {
            command: payload.command.clone().unwrap_or_default(),
            base_sha: payload.base_ref.clone(),
            cwd: state.estate_root.clone(),
            env: BTreeMap::new(),
            expected_artifacts: output_contract,
        })
    } else if payload.kind.as_deref() == Some("actor") {
        let Some(repo_path) = payload.repo_path.clone() else {
            return err_reply("BadRequest", "--repo-path is required for --kind actor");
        };
        // Issue 285: resolve `base_ref` to a commit SHA with git at
        // submit time, so the World reserved here — not the worktree
        // `wirk run` creates later — is what pins the base. An empty or
        // unresolvable ref refuses submit rather than reserving a World
        // whose base can never be honoured.
        let base_sha = match resolve_git_sha(&repo_path, &payload.base_ref) {
            Ok(sha) => sha,
            Err(detail) => return err_reply("GitError", &detail),
        };
        World::Actor(ActorWorld {
            repository: repo_path.clone(),
            // Empty until `wirk run` creates the worktree and records
            // the update (`handle_record`, `RecordPayload`'s doc
            // comment): the World is reserved before any worktree
            // exists.
            worktree_path: PathBuf::new(),
            branch,
            base_sha,
            triple,
            intent: payload.intent.clone(),
            output_contract,
            boundary: Boundary(vec![repo_path]),
        })
    } else {
        let repository = payload
            .repositories
            .first()
            .map(|binding| binding.name.clone())
            .unwrap_or_else(|| "smoke".to_string());
        World::Actor(ActorWorld {
            repository,
            worktree_path: state.estate_root.clone(),
            branch,
            base_sha: payload.base_ref.clone(),
            triple,
            intent: payload.intent.clone(),
            output_contract,
            boundary: Boundary(Vec::new()),
        })
    };
    let world_hash = WorldHash::of(&world);

    let journal = match journal_for(state, &work_id) {
        Ok(journal) => journal,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());

    let submitted = new_event(
        &work_id,
        None,
        EventKind::WorkSubmitted {
            route: route_id,
            repositories: payload.repositories,
            intent: payload.intent,
            waypoints: all_waypoints,
            // W3 (route.md §2/§8, build-brief.md §7.1): carried
            // unconditionally, not only for `--kind actor` — harmless for
            // a `--kind deterministic`/smoke submit (nothing reads it
            // back unless this Work's Route later auto-advances to a
            // Deterministic Waypoint), and avoids a second `payload.kind`
            // branch here (R6).
            wp2_command: payload.command,
        },
    );
    let reserved = new_event(
        &work_id,
        None,
        EventKind::WaypointReserved {
            waypoint: waypoint_id.clone(),
            world_hash: world_hash.clone(),
            world,
        },
    );
    let opened = new_event(
        &work_id,
        Some(run_id.clone()),
        EventKind::RunOpened {
            run: run_id.clone(),
            waypoint: waypoint_id.clone(),
            attempt: 1,
            world_hash,
        },
    );
    for event in [submitted, reserved, opened] {
        if let Err(err) = append_event(state, &mut journal, &work_id, &event) {
            return err_reply("JournalError", &err.to_string());
        }
    }

    ok_reply(json!({
        "work_id": work_id.0,
        "run_id": run_id.0,
        "waypoint": waypoint_id.0,
    }))
}

/// `git -C <repo_path> rev-parse <base_ref>` (R4: native platform CLI,
/// the same call every other git use in this estate makes — `git.rs`'s
/// own doc comment reasons identically for `wirk-herdr`'s side). Issue
/// 285: refuses submit rather than reserving a World pinned to a ref
/// git itself could not resolve.
fn resolve_git_sha(repo_path: &str, base_ref: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", base_ref])
        .output()
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git -C {repo_path} rev-parse {base_ref} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// W3: appends one `EventKind` through the same single write path
/// `submit`/`claim` use, for the journal writes `RunLoop` and `wirk
/// run` themselves need to make (`RunLaunched`, `RunFailed`,
/// `RunVanished`, `LifecycleObserved`, `WorktreeCreated`, and a
/// re-emitted `WaypointReserved` that fills in the worktree path —
/// `RecordPayload`'s doc comment). `ClaimFiled`/`ClaimRecorded` are
/// refused: those two travel only through `claim`'s own validated path
/// (build-brief.md's own "Implement wirkd's record verb... refuse
/// ClaimRecorded and ClaimFiled through it").
fn handle_record(state: &Arc<WirkdState>, payload: RecordPayload) -> Reply {
    if matches!(
        payload.kind,
        EventKind::ClaimFiled { .. } | EventKind::ClaimRecorded { .. }
    ) {
        return err_reply(
            "Forbidden",
            "ClaimFiled/ClaimRecorded are written only by the claim verb, never by record",
        );
    }

    let journal = match journal_for(state, &payload.work_id) {
        Ok(journal) => journal,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    let event = new_event(&payload.work_id, payload.run, payload.kind);
    if let Err(err) = append_event(state, &mut journal, &payload.work_id, &event) {
        return err_reply("JournalError", &err.to_string());
    }
    ok_reply(json!({}))
}

fn handle_claim(state: &Arc<WirkdState>, payload: ClaimPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();

    let journal = match journal_for(state, &work_id) {
        Ok(journal) => journal,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());

    let events = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };

    let claim_id = ClaimId(mint_id("claim"));
    let artifacts: Vec<ArtifactRef> = payload
        .artifacts
        .iter()
        .map(|(name, path)| ArtifactRef {
            name: name.clone(),
            path: path.clone(),
        })
        .collect();
    let claim = Claim {
        id: claim_id.clone(),
        run: run_id.clone(),
        triple: payload.triple.clone(),
        artifacts,
        kind: payload.kind.clone(),
    };

    // D9#4: a `RunId` with no matching `RunOpened` in this Work's
    // journal is a fabricated (or stale) triple — refused and recorded,
    // never honored, and never folded onto (validate.md §3): `fold`'s
    // own "unknown Run" rule already ignores an event naming a Run it
    // has no `RunOpened` for, so appending here is safe even though
    // nothing about the Work changes.
    let Some(run) = find_run(&events, &run_id) else {
        return record_and_reply(
            state,
            &mut journal,
            &work_id,
            &run_id,
            claim_id,
            payload.kind,
            ClaimVerdict::Refused(ClaimRefusal::TripleMismatch),
        );
    };

    // The Work this Run belongs to: `events` is guaranteed non-empty
    // here (finding a `RunOpened` above required a `WorkSubmitted`
    // first — `fold`'s own precondition), so this never hits the
    // "no WorkSubmitted event" panic.
    let work = fold(&events);
    // Defensive: the journal was located by `triple.work_id`, so this
    // can only fail if a caller mismatched estate/journal wiring, never
    // in this wave's own construction (build-brief.md §3 W3: "checks
    // the triple's work_id against the Work").
    if work.id != work_id {
        return record_and_reply(
            state,
            &mut journal,
            &work_id,
            &run_id,
            claim_id,
            payload.kind,
            ClaimVerdict::Refused(ClaimRefusal::TripleMismatch),
        );
    }

    let route = route_definition(&work.route);
    let waypoint = route
        .waypoints
        .iter()
        .find(|def| def.id == run.waypoint)
        .cloned()
        .unwrap_or_else(|| smoke_waypoint(run.waypoint.clone()));
    let mut verdict = validate_claim(&waypoint, &run, &claim);

    if matches!(verdict, ClaimVerdict::Validated)
        && let Some(worktree_path) = worktree_path_for_run(&events, &run_id)
    {
        for artifact in &claim.artifacts {
            if !worktree_path.join(&artifact.path).exists() {
                verdict =
                    ClaimVerdict::Refused(ClaimRefusal::MissingArtifact(artifact.name.clone()));
                break;
            }
        }
    }

    let claim_kind = payload.kind.clone();
    let reply = record_and_reply(
        state,
        &mut journal,
        &work_id,
        &run_id,
        claim_id,
        payload.kind,
        verdict.clone(),
    );

    // Auto-advance (item 8, `orient/route.md` §2, J1 decided in
    // `build-brief.md` §2 "Disagreement resolved"): a Validated Done
    // claim against a non-last Waypoint reserves the Route's next one,
    // inside the same journal lock the claim itself just appended
    // under — atomic with the claim, no second round trip, no race
    // with a concurrent `status` read. A Validated Question, a Refused
    // claim, or a claim on the last Waypoint advances nothing.
    if matches!(verdict, ClaimVerdict::Validated) && matches!(claim_kind, ClaimKind::Done) {
        let waypoints = route_waypoints(&events);
        let is_last = waypoints.last() == Some(&run.waypoint);
        if !is_last
            && let Some(pos) = waypoints.iter().position(|w| w == &run.waypoint)
            && let Some(next_id) = waypoints.get(pos + 1)
            && let Some(next_def) = route.waypoints.iter().find(|def| &def.id == next_id)
        {
            let cwd = worktree_path_for_run(&events, &run_id)
                .unwrap_or_else(|| state.estate_root.clone());
            let base_sha = world_for_waypoint(&events, &run.waypoint)
                .map(|world| match world {
                    World::Actor(actor) => actor.base_sha,
                    World::Deterministic(deterministic) => deterministic.base_sha,
                })
                .unwrap_or_default();
            let next_world = match next_def.kind {
                // W3 (route.md §2/§8, build-brief.md §7.1): the
                // Work's own submitted `wp2_command` (an actor-kind
                // submit's `--command`) wins when present; proving's
                // own wp-2 is the only Deterministic Waypoint any
                // hardcoded Route advances to today, so absent that,
                // `PROVING_WP2_COMMAND` (hardcoded alongside the Route
                // itself) is the fallback.
                WaypointKind::Deterministic => Some(World::Deterministic(DeterministicWorld {
                    command: wp2_command_for(&events).unwrap_or_else(|| {
                        PROVING_WP2_COMMAND.iter().map(|s| s.to_string()).collect()
                    }),
                    base_sha,
                    cwd,
                    // Every cargo the child executor runs uses the one
                    // named-kept warm cache (0030; 0039 D126), not a
                    // cold build in the worktree (build-brief.md §7.5).
                    env: BTreeMap::from([(
                        "CARGO_TARGET_DIR".to_string(),
                        "/var/tmp/wirk-target".to_string(),
                    )]),
                    expected_artifacts: OutputContract(next_def.declared_outputs.clone()),
                })),
                // No hardcoded Route advances Actor-to-Actor today
                // (R1: nothing needs it) — left unadvanced rather than
                // guessing an intent/repo_path for a second pane.
                WaypointKind::Actor => None,
            };
            if let Some(next_world) = next_world {
                let world_hash = WorldHash::of(&next_world);
                let next_run_id = RunId(mint_id("run"));
                let reserved = new_event(
                    &work_id,
                    None,
                    EventKind::WaypointReserved {
                        waypoint: next_id.clone(),
                        world_hash: world_hash.clone(),
                        world: next_world,
                    },
                );
                let opened = new_event(
                    &work_id,
                    Some(next_run_id.clone()),
                    EventKind::RunOpened {
                        run: next_run_id,
                        waypoint: next_id.clone(),
                        attempt: 1,
                        world_hash,
                    },
                );
                for event in [reserved, opened] {
                    if let Err(err) = append_event(state, &mut journal, &work_id, &event) {
                        return err_reply("JournalError", &err.to_string());
                    }
                }
            }
        }
    }

    reply
}

/// Appends `ClaimFiled` then `ClaimRecorded { verdict }` (validate.md
/// §3: filed before validation, recorded after — both always land,
/// whichever way the verdict fell), then builds the wire reply from the
/// same verdict.
fn record_and_reply(
    state: &Arc<WirkdState>,
    journal: &mut Journal,
    work_id: &WorkId,
    run_id: &RunId,
    claim_id: ClaimId,
    claim_kind: ClaimKind,
    verdict: ClaimVerdict,
) -> Reply {
    let filed = new_event(
        work_id,
        Some(run_id.clone()),
        EventKind::ClaimFiled {
            claim: claim_id.clone(),
        },
    );
    if let Err(err) = append_event(state, journal, work_id, &filed) {
        return err_reply("JournalError", &err.to_string());
    }
    let recorded = new_event(
        work_id,
        Some(run_id.clone()),
        EventKind::ClaimRecorded {
            claim: claim_id,
            claim_kind,
            verdict: verdict.clone(),
        },
    );
    if let Err(err) = append_event(state, journal, work_id, &recorded) {
        return err_reply("JournalError", &err.to_string());
    }

    match verdict {
        ClaimVerdict::Validated => ok_reply(json!({"verdict": "Validated"})),
        ClaimVerdict::Refused(refusal) => refusal_reply(&refusal),
    }
}

/// W3 (both items, build-brief.md §3 W3 / §2.2): alongside item 3's
/// original three fields (`state`, `current_waypoint`, `events`), now
/// also carries `run_id`/`attempt`/`world_hash`/`run_state` and, when
/// the Work's current Waypoint has one, the reserved `world` itself —
/// the shape `wirk run-deterministic` reads (module doc: "reads the
/// reserved World from wirkd status") — and a `"runs"` array, one entry
/// per `RunOpened` this Work's journal carries: the reconstructed `Run`
/// (state included) plus its Waypoint's most-recently reserved `World`,
/// so `wirk run` can read both the Run to drive and the World to launch
/// it with from one verb (`wirk_herdr::run_loop::WirkdApi::status`).
/// All of these are additive; an old caller reading only the first
/// three fields is unaffected.
fn handle_status(state: &Arc<WirkdState>, payload: StatusPayload) -> Reply {
    let journal = match journal_for(state, &payload.work_id) {
        Ok(journal) => journal,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    let events = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    if events.is_empty() {
        return err_reply("NotFound", "no such work");
    }
    let work = fold(&events);

    let runs: Vec<Value> = all_run_ids(&events)
        .into_iter()
        .filter_map(|run_id| {
            let run = find_run(&events, &run_id)?;
            let world = world_for_waypoint(&events, &run.waypoint);
            Some(json!({
                "run": serde_json::to_value(&run).ok()?,
                "world": world.and_then(|w| serde_json::to_value(&w).ok()),
            }))
        })
        .collect();

    let mut result = json!({
        "state": work_state_name(work.state),
        "current_waypoint": work.current_waypoint.as_ref().map(|w| w.0.clone()),
        "events": events.len(),
        "runs": runs,
    });

    if let Some(waypoint) = &work.current_waypoint
        && let Some((run_id, attempt, world_hash)) = latest_run_for_waypoint(&events, waypoint)
    {
        result["run_id"] = json!(run_id.0);
        result["attempt"] = json!(attempt);
        result["world_hash"] = json!(world_hash.0);
        if let Some(run) = find_run(&events, &run_id) {
            let (run_state, failure_status, failure_detail) = match &run.state {
                RunState::Open => ("open", None, None),
                RunState::Claimed(_) => ("claimed", None, None),
                RunState::Vanished => ("vanished", None, None),
                RunState::Failed(cause) => ("failed", cause.status.clone(), cause.detail.clone()),
            };
            result["run_state"] = json!(run_state);
            if let Some(status) = failure_status {
                result["failure_status"] = json!(status);
            }
            if let Some(detail) = failure_detail {
                result["failure_detail"] = json!(detail);
            }
        }
        if let Some(world) = world_for_waypoint(&events, waypoint) {
            result["world"] = serde_json::to_value(&world).expect("World always serializes");
        }
    }

    ok_reply(result)
}

/// `run-deterministic`'s own verb (module doc, `FailPayload`): journals
/// a `RunFailed { cause }` for the triple's `run_id`, refusing
/// (`TripleMismatch`) the same way `claim` does (D9#4) when no
/// `RunOpened` in this Work's journal names it — this call never
/// invents a Run, only records a fact about one that already exists.
fn handle_fail(state: &Arc<WirkdState>, payload: FailPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();

    let journal = match journal_for(state, &work_id) {
        Ok(journal) => journal,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    let events = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    if find_run(&events, &run_id).is_none() {
        return err_reply(
            "TripleMismatch",
            "the run id does not match any Run opened for this Work",
        );
    }

    let cause = FailureCause {
        status: payload.status,
        request_id: None,
        at: now_ts(),
        detail: payload.detail,
    };
    let event = new_event(&work_id, Some(run_id), EventKind::RunFailed { cause });
    if let Err(err) = append_event(state, &mut journal, &work_id, &event) {
        return err_reply("JournalError", &err.to_string());
    }
    ok_reply(json!({}))
}

/// Every `RunId` this Work's journal has opened, in journal order (one
/// per `RunOpened` event) — `handle_status`'s own iteration order for
/// building `"runs"`.
fn all_run_ids(events: &[Event]) -> Vec<RunId> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::RunOpened { run, .. } => Some(run.clone()),
            _ => None,
        })
        .collect()
}

/// The `World` most recently reserved for `waypoint` — the *last*
/// matching `WaypointReserved` wins, not the first (`.rev()`): `wirk
/// run` (W3) re-emits `WaypointReserved` through `record` once the
/// worktree exists, carrying the same `waypoint`/`world_hash` but a
/// filled-in `worktree_path` (`RecordPayload`'s doc comment, R1 — no
/// new event type for a World field that changed after reservation).
fn world_for_waypoint(events: &[Event], waypoint: &WaypointId) -> Option<World> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::WaypointReserved {
            waypoint: w, world, ..
        } if w == waypoint => Some(world.clone()),
        _ => None,
    })
}

/// Fetches (opening on first touch) the `Arc<Mutex<Journal>>` for
/// `work_id`, journaled at `$estate_root/works/<work_id>/journal.ndjson`
/// (0033 D101). The outer map lock is held only long enough to
/// fetch-or-insert; the returned `Arc` lets the caller lock the
/// per-Work `Mutex<Journal>` without holding the map lock across the
/// append (transport.md §5).
fn journal_for(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
) -> Result<Arc<Mutex<Journal>>, JournalError> {
    let mut journals = state
        .journals
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(existing) = journals.get(work_id) {
        return Ok(Arc::clone(existing));
    }
    let dir = state.estate_root.join("works").join(&work_id.0);
    let journal = Journal::open(dir)?;
    let journal = Arc::new(Mutex::new(journal));
    journals.insert(work_id.clone(), Arc::clone(&journal));
    Ok(journal)
}

/// Reconstructs the `Run` named `run_id` by replaying `events` in order:
/// seeds the initial state at its `RunOpened`, then folds every
/// subsequent event through `Run::apply` (which already ignores events
/// naming a different Run — `lib.rs` "An event whose `run` is not this
/// Run's id is ignored"). `None` when no `RunOpened` names `run_id` at
/// all — the fabricated/stale-triple case (D9#4).
fn find_run(events: &[Event], run_id: &RunId) -> Option<Run> {
    let mut run: Option<Run> = None;
    for event in events {
        if run.is_none()
            && let EventKind::RunOpened {
                run: opened,
                waypoint,
                attempt,
                world_hash,
            } = &event.kind
            && opened == run_id
        {
            run = Some(Run {
                id: opened.clone(),
                waypoint: waypoint.clone(),
                attempt: *attempt,
                world_hash: world_hash.clone(),
                state: RunState::Open,
                // W1 (0041 D129): `RunOpened` is journaled at submit,
                // before `--actor-kind` is chosen (that happens at
                // `wirk run` time) — seeded `Claude` here, then folded
                // to the real kind by `RunLaunched` via `Run::apply`
                // below, same event stream this loop already replays.
                kind: wirk_core::ActorKind::default(),
            });
        }
        if let Some(run) = run.as_mut() {
            run.apply(event);
        }
    }
    run
}

/// The `worktree_path` (Actor) or `cwd` (Deterministic) of the World
/// reserved for `run_id`'s Waypoint, when the journal carries one —
/// build-brief amendment 3: "wirkd checks artifact paths exist on disk
/// relative to the Run's worktree path when the World carries one, else
/// by name only". Reads the Waypoint's *most recent* World
/// (`world_for_waypoint`, R2): an Actor Run's `worktree_path` starts
/// empty at reservation and is filled in by `wirk run` (W3) once the
/// worktree exists, through a re-emitted `WaypointReserved` — the first
/// `WaypointReserved` alone would check artifacts against an empty
/// path and always refuse.
fn worktree_path_for_run(events: &[Event], run_id: &RunId) -> Option<PathBuf> {
    let waypoint_id = events.iter().find_map(|event| match &event.kind {
        EventKind::RunOpened { run, waypoint, .. } if run == run_id => Some(waypoint.clone()),
        _ => None,
    })?;
    match world_for_waypoint(events, &waypoint_id)? {
        World::Actor(actor) => Some(actor.worktree_path),
        World::Deterministic(deterministic) => Some(deterministic.cwd),
    }
}

/// The most recently opened Run for `waypoint_id` — the last
/// `RunOpened` naming it, walked in reverse so a retried Waypoint's
/// latest attempt wins (W3, `handle_status`: "reads the reserved World
/// from wirkd status" needs a `run_id`/`attempt`/`world_hash` to hand
/// back, the same three fields `RunOpened` itself carries).
fn latest_run_for_waypoint(
    events: &[Event],
    waypoint_id: &WaypointId,
) -> Option<(RunId, u32, WorldHash)> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::RunOpened {
            run,
            waypoint,
            attempt,
            world_hash,
        } if waypoint == waypoint_id => Some((run.clone(), *attempt, world_hash.clone())),
        _ => None,
    })
}

fn new_event(work_id: &WorkId, run: Option<RunId>, kind: EventKind) -> Event {
    Event {
        id: wirk_core::EventId(String::new()),
        work: work_id.clone(),
        run,
        at: now_ts(),
        kind,
    }
}

fn now_ts() -> Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Timestamp(ms as i64)
}

/// A unique-enough string id, std only (R3 over R5: nothing beyond
/// stdlib is needed for a wirkd-local identifier — the `WorkId`/`RunId`/
/// `ClaimId` newtypes place no format requirement on their `String`,
/// only the `Journal`'s own `EventId` minting uses `ulid`, which
/// `wirk-core` does not expose). Nanosecond timestamp plus a
/// process-local atomic counter: unique within one wirkd process, which
/// is the only minter of these ids.
fn mint_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos:x}-{seq:x}")
}

fn work_state_name(state: WorkState) -> &'static str {
    match state {
        WorkState::Pending => "pending",
        WorkState::Active => "active",
        WorkState::Waiting => "waiting",
        WorkState::NeedsInput => "needs_input",
        WorkState::Blocked => "blocked",
        WorkState::Completed => "completed",
        WorkState::Failed => "failed",
        WorkState::Canceled => "canceled",
    }
}

fn ok_reply(result: Value) -> Reply {
    Reply::Ok { ok: true, result }
}

fn err_reply(code: &str, message: &str) -> Reply {
    Reply::Err {
        ok: false,
        error: ErrorDetail {
            code: code.to_string(),
            message: message.to_string(),
            detail: None,
        },
    }
}

fn refusal_reply(refusal: &ClaimRefusal) -> Reply {
    let (code, message) = match refusal {
        ClaimRefusal::MissingArtifact(name) => ("MissingArtifact", name.clone()),
        ClaimRefusal::TripleMismatch => (
            "TripleMismatch",
            "the claim's run id does not match any Run opened for this Work".to_string(),
        ),
        ClaimRefusal::OutOfBoundary(what) => ("OutOfBoundary", what.clone()),
        ClaimRefusal::AlreadyClaimed => {
            ("AlreadyClaimed", "the Run is already Claimed".to_string())
        }
    };
    err_reply(code, &message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work_submitted_event(waypoints: Vec<&str>) -> Event {
        work_submitted_event_with_command(waypoints, None)
    }

    /// W3: same as `work_submitted_event`, with the submitted
    /// `wp2_command` also settable, for `wp2_command_for`'s own test.
    fn work_submitted_event_with_command(
        waypoints: Vec<&str>,
        wp2_command: Option<Vec<&str>>,
    ) -> Event {
        Event {
            id: wirk_core::EventId(String::new()),
            work: WorkId("work-1".to_string()),
            run: None,
            at: Timestamp(0),
            kind: EventKind::WorkSubmitted {
                route: RouteId("route-1".to_string()),
                repositories: Vec::new(),
                intent: "do the thing".to_string(),
                waypoints: waypoints
                    .into_iter()
                    .map(|id| WaypointId(id.to_string()))
                    .collect(),
                wp2_command: wp2_command.map(|cmd| cmd.into_iter().map(String::from).collect()),
            },
        }
    }

    /// `wp2_command_for` (`orient/route.md` §5): the submitted command
    /// wins when present, `None` when absent — `handle_claim`'s
    /// auto-advance falls the latter back to `PROVING_WP2_COMMAND`
    /// itself, not this function's job.
    #[test]
    fn wp2_command_for_reads_workssubmitted_or_falls_back_to_none() {
        let submitted = vec![work_submitted_event_with_command(
            vec!["proving/wp-1", "proving/wp-2"],
            Some(vec!["sh", "-c", "cargo test"]),
        )];
        assert_eq!(
            wp2_command_for(&submitted),
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "cargo test".to_string()
            ])
        );

        let absent = vec![work_submitted_event(vec!["proving/wp-1", "proving/wp-2"])];
        assert_eq!(wp2_command_for(&absent), None);

        assert_eq!(wp2_command_for(&[]), None);
    }

    /// `route_waypoints` reads the ordered ids straight off the Work's
    /// own `WorkSubmitted` event (`orient/route.md` §4): two for a
    /// proving-shaped submission, one for a smoke-shaped one, and empty
    /// when no `WorkSubmitted` is present at all.
    #[test]
    fn route_waypoints_reads_workssubmitted_in_order() {
        let two = vec![work_submitted_event(vec!["proving/wp-1", "proving/wp-2"])];
        assert_eq!(
            route_waypoints(&two),
            vec![
                WaypointId("proving/wp-1".to_string()),
                WaypointId("proving/wp-2".to_string()),
            ]
        );

        let one = vec![work_submitted_event(vec!["smoke/wp-1"])];
        assert_eq!(
            route_waypoints(&one),
            vec![WaypointId("smoke/wp-1".to_string())]
        );

        assert_eq!(route_waypoints(&[]), Vec::<WaypointId>::new());
    }

    /// `route_definition("proving")` names two Waypoints, wp-1 Actor
    /// then wp-2 Deterministic, in that order (`orient/route.md` §1) —
    /// any other id (including `"smoke"`) stays the original single
    /// Actor Waypoint.
    #[test]
    fn route_definition_selects_proving_or_falls_back_to_smoke() {
        let proving = route_definition(&RouteId("proving".to_string()));
        assert_eq!(proving.waypoints.len(), 2);
        assert_eq!(
            proving.waypoints[0].id,
            WaypointId("proving/wp-1".to_string())
        );
        assert_eq!(proving.waypoints[0].kind, WaypointKind::Actor);
        assert_eq!(
            proving.waypoints[1].id,
            WaypointId("proving/wp-2".to_string())
        );
        assert_eq!(proving.waypoints[1].kind, WaypointKind::Deterministic);

        let smoke = route_definition(&RouteId("smoke".to_string()));
        assert_eq!(smoke.waypoints.len(), 1);
        assert_eq!(smoke.waypoints[0].kind, WaypointKind::Actor);

        let other = route_definition(&RouteId("anything-else".to_string()));
        assert_eq!(other.waypoints.len(), 1);
    }
}
