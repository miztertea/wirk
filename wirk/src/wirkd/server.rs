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
//! `submit` requires `--route <name or path>` and loads it via
//! `wirk_core::load_route` (p2-route-files W2, build-brief.md §7.3),
//! except the Route-less ad hoc `--kind deterministic --command
//! <argv...>` single-Waypoint Work, which synthesizes its own
//! `WaypointDefinition` instead. Either way the full Route content is
//! journaled onto `WorkSubmitted.waypoint_defs` (§7.1): every later
//! reader — the World reserved here, `handle_claim`'s validation and
//! auto-advance — takes a Waypoint's kind/intent/command/outputs from
//! there, never by loading the file a second time or by a hardcoded
//! lookup. `claim` locates the Work's journal
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

use std::collections::{BTreeMap, HashMap, HashSet};
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
    Access, ActorWorld, ArtifactRef, ArtifactSpec, Boundary, Claim, ClaimId, ClaimKind,
    ClaimRefusal, ClaimVerdict, DeterministicWorld, Event, EventKind, ExecutionTriple,
    FailureCause, Journal, JournalError, OutputContract, Route, RouteId, Run, RunId, RunState,
    Timestamp, WaypointDefinition, WaypointId, WaypointKind, WorkId, WorkState, World, WorldHash,
    fold, load_route, validate_claim,
};

use super::boundary;
use super::{
    ClaimPayload, ErrorDetail, FailPayload, RecordPayload, Reply, Request, RetryPayload,
    StatusPayload, SubmitPayload, Verb, WirkdPointer, WorkFailPayload,
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

/// p2-route-files W2 (build-brief.md §7.1, format.md §4): every Work's
/// own `WaypointDefinition`s, as `WorkSubmitted.waypoint_defs` journaled
/// them at submit — a loaded Route file's own Waypoints, or the ad hoc
/// deterministic path's single synthesized one (`handle_submit`, below;
/// no submit leaves this empty any more). `handle_claim`'s validation
/// and auto-advance both read a Waypoint's kind/intent/command/outputs
/// from here, never by re-reading the Route file, so an edit to the
/// file after submit cannot change what a Work already reserved (§7.1's
/// own test). Empty only for a journal line written before this field
/// existed (`old_worksubmitted_without_waypoint_defs_field_still_folds`,
/// this campaign's own evidence, none in production).
fn waypoint_defs_for(events: &[Event]) -> Vec<WaypointDefinition> {
    events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::WorkSubmitted { waypoint_defs, .. } => Some(waypoint_defs.clone()),
            _ => None,
        })
        .unwrap_or_default()
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

    // W5 (0035 D110): before this listener starts accepting
    // connections, re-adopt any docker containers a prior, killed
    // `wirkd` left running (module doc above `recover_docker_runs`).
    recover_docker_runs(&state);

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
fn managed_container_names() -> HashSet<String> {
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

// ---- W5: the docker recovery sweep (0035 D110) ------------------------
//
// Run once at `run()`'s own startup, before the listener's `.incoming()`
// loop starts accepting connections (`orient/two-works.md` §2): a
// `wirkd` that was `SIGKILL`ed with a docker Run still live leaves the
// container running under `dockerd` regardless (nothing in the docker
// executor ties the container's life to `wirkd`'s own pid, module doc
// `executors/docker.rs`) — what breaks is only the path back to the
// journal. At restart there are exactly two states for an open
// Deterministic Run's container: still known to the daemon (running,
// since every container launches with `--rm` and the daemon removes it
// the instant it exits — module doc `executors/docker.rs`, confirmed by
// this wave's own probe below), or entirely gone. Re-adopt the first,
// journal `RunVanished` for the second — nothing between.

/// One open Deterministic Run read from a Work's journal, paired with
/// the World reserved for its Waypoint. Pure and docker-free (R2: same
/// journal-walk shape `remove_owned_containers` already uses) so the
/// unit test can feed it a throwaway `works/` directory with no daemon
/// at all.
pub(crate) fn open_deterministic_runs(
    estate_root: &Path,
) -> Vec<(WorkId, RunId, DeterministicWorld)> {
    let works_dir = estate_root.join("works");
    let Ok(entries) = std::fs::read_dir(&works_dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
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
        let Some(work_id) = events.first().map(|event| event.work.clone()) else {
            continue;
        };
        for event in &events {
            let EventKind::RunOpened { run: run_id, .. } = &event.kind else {
                continue;
            };
            let Some(run) = find_run(&events, run_id) else {
                continue;
            };
            if !matches!(run.state, RunState::Open) {
                continue;
            }
            if let Some(World::Deterministic(det)) = world_for_waypoint(&events, &run.waypoint) {
                found.push((work_id.clone(), run_id.clone(), det));
            }
        }
    }
    found
}

/// One matched outcome for an open Deterministic Run against the
/// daemon's own `io.wirk.managed` listing.
#[derive(Debug)]
pub(crate) enum RunMatch {
    /// `wirk-<run_id>` is still known to the daemon: re-adopt it.
    Reattach {
        work_id: WorkId,
        run_id: RunId,
        world: DeterministicWorld,
        container_name: String,
    },
    /// `wirk-<run_id>` is gone: removed by its own `--rm` after
    /// exiting, or never a docker Run at all (indistinguishable from
    /// the journal alone — both mean the same thing here, and a late
    /// Claim from a still-alive non-docker executor is honored
    /// regardless, `Run::apply`'s own Vanished-to-Claimed path, D9#5).
    Vanished { work_id: WorkId, run_id: RunId },
}

/// Matches `open_runs` (from `open_deterministic_runs`) against
/// `managed` (from `managed_container_names`, injected here so the unit
/// test needs no daemon): one `RunMatch` per open Run, plus the names
/// in `managed` matched to none of them — a labelled container whose
/// Run is not open in any journal, left alone by the caller.
pub(crate) fn match_docker_runs(
    open_runs: Vec<(WorkId, RunId, DeterministicWorld)>,
    managed: &HashSet<String>,
) -> (Vec<RunMatch>, Vec<String>) {
    let mut matches = Vec::new();
    let mut named = HashSet::new();
    for (work_id, run_id, world) in open_runs {
        let container_name = format!("wirk-{}", run_id.0);
        named.insert(container_name.clone());
        if managed.contains(&container_name) {
            matches.push(RunMatch::Reattach {
                work_id,
                run_id,
                world,
                container_name,
            });
        } else {
            matches.push(RunMatch::Vanished { work_id, run_id });
        }
    }
    let unmatched = managed
        .iter()
        .filter(|name| !named.contains(*name))
        .cloned()
        .collect();
    (matches, unmatched)
}

/// Called once from `run()`, before the listener starts accepting
/// connections. Best-effort throughout, same discipline as
/// `remove_owned_containers`: an empty estate or no `docker` binary
/// mean nothing to recover, not an error.
fn recover_docker_runs(state: &Arc<WirkdState>) {
    let open_runs = open_deterministic_runs(&state.estate_root);
    if open_runs.is_empty() {
        return;
    }
    let managed = managed_container_names();
    let (matches, unmatched) = match_docker_runs(open_runs, &managed);
    for name in unmatched {
        eprintln!(
            "wirkd: labelled container {name} matches no open Run in any journal, left alone"
        );
    }
    for m in matches {
        match m {
            RunMatch::Vanished { work_id, run_id } => {
                handle_record(
                    state,
                    RecordPayload {
                        work_id,
                        run: Some(run_id),
                        kind: EventKind::RunVanished,
                    },
                );
            }
            RunMatch::Reattach {
                work_id,
                run_id,
                world,
                container_name,
            } => {
                let state = Arc::clone(state);
                std::thread::spawn(move || {
                    reattach_docker_run(&state, work_id, run_id, world, container_name)
                });
            }
        }
    }
}

/// Blocks on `docker wait <container_name>` (R4; ruling 0044: no
/// timer, no poll — the container's own exit is the state waited on)
/// then files the Run's outcome the way `DockerExecutor::finish_exit`
/// does for a Run it launched itself: a clean exit's declared artifacts
/// as a validated Claim, a non-zero exit's code and best-effort log
/// tail as a `RunFailed` — both through the narrower exit-code-only
/// path (`orient/two-works.md` §2 J0), reusing `handle_claim`/
/// `handle_fail` directly since this call already runs inside `wirkd`
/// and holds the same `state` those handlers take, no socket round
/// trip needed. Re-checks the Run is still `Open` right before filing:
/// the original `wirk run-deterministic` process that launched this
/// container may still be alive, attached via its own `docker start
/// -a`, and may have already filed the outcome itself once `wirkd`
/// came back up — filing again would be a harmless but redundant
/// second `ClaimRecorded`/`RunFailed`, avoided here instead.
fn reattach_docker_run(
    state: &Arc<WirkdState>,
    work_id: WorkId,
    run_id: RunId,
    world: DeterministicWorld,
    container_name: String,
) {
    let wait_output = Command::new("docker")
        .arg("wait")
        .arg(&container_name)
        .output();
    let exit_code = match wait_output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<i32>()
            .unwrap_or(-1),
        _ => -1,
    };

    let still_open = journal_for(state, &work_id).ok().is_some_and(|journal| {
        let journal = journal.lock().unwrap_or_else(|p| p.into_inner());
        journal
            .replay()
            .ok()
            .and_then(|events| find_run(&events, &run_id))
            .is_some_and(|run| matches!(run.state, RunState::Open))
    });
    if !still_open {
        eprintln!(
            "wirkd: {container_name} exited but Run {} is no longer Open, not re-filing",
            run_id.0
        );
        return;
    }

    let triple = ExecutionTriple {
        estate_root: state.estate_root.display().to_string(),
        work_id: work_id.clone(),
        run_id: run_id.clone(),
    };

    if exit_code == 0 {
        let artifacts: BTreeMap<String, String> = world
            .expected_artifacts
            .0
            .iter()
            .map(|spec| {
                (
                    spec.name.clone(),
                    world.cwd.join(&spec.name).display().to_string(),
                )
            })
            .collect();
        handle_claim(
            state,
            ClaimPayload {
                triple,
                kind: ClaimKind::Done,
                artifacts,
            },
        );
        return;
    }

    // Best-effort only: the container may already be gone by the time
    // this runs (`--rm` races the daemon's own removal, same hazard
    // `executors/docker.rs`'s own module doc names for a post-hoc
    // `docker logs`) — an unreadable log is not itself evidence of
    // anything, so a failed read leaves `detail` empty rather than
    // failing the whole outcome.
    let detail = Command::new("docker")
        .arg("logs")
        .arg(&container_name)
        .output()
        .ok()
        .map(|out| {
            let mut combined = out.stdout;
            combined.extend_from_slice(&out.stderr);
            let start = combined.len().saturating_sub(4096);
            String::from_utf8_lossy(&combined[start..]).into_owned()
        });
    handle_fail(
        state,
        FailPayload {
            triple,
            status: Some(exit_code.to_string()),
            detail,
        },
    );
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
        Verb::Retry => match serde_json::from_value::<RetryPayload>(request.payload.clone()) {
            Ok(payload) => Outcome::Reply(handle_retry(state, payload)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::WorkFail => {
            match serde_json::from_value::<WorkFailPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_workfail(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
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

/// p2-route-files (format.md §2): a `--route` value is path-like when
/// it contains `/` or ends `.json`; `resolve_route_path` (below) is the
/// only caller.
fn is_route_path(spec: &str) -> bool {
    spec.contains('/') || spec.ends_with(".json")
}

/// p2-route-files W2 (format.md §2, build-brief.md §7.3): a path-like
/// `--route` value (`is_route_path`) resolves cwd-relative or absolute,
/// same as any other path `std::fs` opens; a bare name resolves against
/// the estate's own `routes/` directory — `--route proving`/`smoke` now
/// name `<estate_root>/routes/proving.json`/`smoke.json`, never a
/// hardcoded Route in this binary.
fn resolve_route_path(estate_root: &Path, spec: &str) -> PathBuf {
    if is_route_path(spec) {
        PathBuf::from(spec)
    } else {
        estate_root.join("routes").join(format!("{spec}.json"))
    }
}

fn handle_submit(state: &Arc<WirkdState>, payload: SubmitPayload) -> Reply {
    let work_id = WorkId(mint_id("work"));
    let run_id = RunId(mint_id("run"));

    // W3 (build-brief.md §3 W3): `--kind deterministic --command
    // <argv...>` is the one submit shape that carries no Route at all
    // (build-brief.md §7.3) — every other submit, `--kind actor
    // --repo-path <path>` or bare, must name `--route`.
    let deterministic = payload.kind.as_deref() == Some("deterministic");
    if deterministic && payload.command.as_ref().is_none_or(|c| c.is_empty()) {
        return err_reply(
            "BadRequest",
            "work submit --kind deterministic requires a non-empty --command",
        );
    }

    // p2-route-files W2 (build-brief.md §7.1, §7.3): `route` is `None`
    // only for the ad hoc deterministic path above, whose own single
    // Waypoint is synthesized below instead of loaded from a file —
    // either way `waypoint_defs` ends up with the full, ordered
    // Waypoint definitions this Work reserves, read once here and never
    // reloaded (auto-advance and validation both read it back off the
    // journal, `waypoint_defs_for`).
    let (route, waypoint_defs): (Option<Route>, Vec<WaypointDefinition>) = if deterministic {
        (
            None,
            vec![WaypointDefinition {
                id: WaypointId("ad-hoc/wp-1".to_string()),
                kind: WaypointKind::Deterministic,
                declared_outputs: vec![ArtifactSpec {
                    name: "report.md".to_string(),
                    required: true,
                }],
                intent: None,
                command: payload.command.clone(),
                boundary: Boundary(Vec::new()),
            }],
        )
    } else {
        let Some(spec) = payload.route.as_deref() else {
            return err_reply(
                "BadRequest",
                "--route is required unless --kind deterministic --command is used",
            );
        };
        match load_route(&resolve_route_path(&state.estate_root, spec)) {
            Ok(route) => {
                let defs = route.waypoints.clone();
                (Some(route), defs)
            }
            Err(err) => return err_reply("RouteError", &err.to_string()),
        }
    };
    let route_id = route
        .as_ref()
        .map(|r| r.id.clone())
        .unwrap_or_else(|| RouteId("ad-hoc".to_string()));
    let first_def = waypoint_defs[0].clone();
    let waypoint_id = first_def.id.clone();
    let all_waypoints: Vec<WaypointId> = waypoint_defs.iter().map(|w| w.id.clone()).collect();

    let triple = ExecutionTriple {
        estate_root: state.estate_root.display().to_string(),
        work_id: work_id.clone(),
        run_id: run_id.clone(),
    };
    let output_contract = OutputContract(first_def.declared_outputs.clone());
    let branch = format!("wirk/{}", work_id.0);

    // The reserved World's own kind follows the *Route's* first
    // Waypoint (`first_def.kind`), not `payload.kind` directly — a
    // Route file's own authored order decides what gets reserved first
    // (build-brief.md §7.3's own "advances in the file's order", the
    // same rule auto-advance already applies to every later Waypoint,
    // `next_def.kind` below).
    let world = match first_def.kind {
        WaypointKind::Deterministic => World::Deterministic(DeterministicWorld {
            command: first_def.command.clone().unwrap_or_default(),
            base_sha: payload.base_ref.clone(),
            cwd: state.estate_root.clone(),
            env: BTreeMap::new(),
            expected_artifacts: output_contract,
        }),
        WaypointKind::Actor if payload.kind.as_deref() == Some("actor") => {
            let Some(repo_path) = payload.repo_path.clone() else {
                return err_reply("BadRequest", "--repo-path is required for --kind actor");
            };
            // Issue 285: resolve `base_ref` to a commit SHA with git at
            // submit time, so the World reserved here — not the
            // worktree `wirk run` creates later — is what pins the
            // base. An empty or unresolvable ref refuses submit rather
            // than reserving a World whose base can never be honoured.
            let base_sha = match resolve_git_sha(&repo_path, &payload.base_ref) {
                Ok(sha) => sha,
                Err(detail) => return err_reply("GitError", &detail),
            };
            World::Actor(ActorWorld {
                repository: repo_path.clone(),
                // Empty until `wirk run` creates the worktree and
                // records the update (`handle_record`, `RecordPayload`'s
                // doc comment): the World is reserved before any
                // worktree exists.
                worktree_path: PathBuf::new(),
                branch,
                base_sha,
                triple,
                // p2-route-files W2 (`--intent` removed, J1): the
                // Waypoint's own authored intent, never the submit
                // line's.
                intent: first_def.intent.clone().unwrap_or_default(),
                output_contract,
                // P2.4 W1 (build-brief.md §8 amendment 1): the World's
                // boundary is the Route-authored Waypoint's own globs,
                // not the repository path — `repo_path` stays only the
                // `repository` field above. `WorldHash::of` already
                // hashes `actor.boundary.0` (0029 D95, landed before
                // this item); only the value fed into it changes here.
                boundary: first_def.boundary.clone(),
            })
        }
        WaypointKind::Actor => {
            let repository = payload
                .repositories
                .first()
                .map(|binding| binding.name.clone())
                .unwrap_or_else(|| route_id.0.clone());
            World::Actor(ActorWorld {
                repository,
                worktree_path: state.estate_root.clone(),
                branch,
                base_sha: payload.base_ref.clone(),
                triple,
                intent: first_def.intent.clone().unwrap_or_default(),
                output_contract,
                // P2.4 W1 (build-brief.md §8 amendment 1): same fix as
                // the sibling arm above — the Route's own globs, not a
                // hardcoded empty boundary.
                boundary: first_def.boundary.clone(),
            })
        }
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
            // p2-route-files W2 (build-brief.md §7.1): the full,
            // ordered Waypoint definitions this Work reserves — a
            // loaded Route file's own Waypoints, or the ad hoc
            // deterministic path's single synthesized one — journaled
            // whole so `handle_claim`'s validation and auto-advance
            // never re-read the file (or reconstruct a fallback) again.
            waypoint_defs,
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

/// Whether `worktree_path.join(artifact_path)` (`server.rs`'s own join,
/// used both by the artifact-exists check below and by the boundary
/// diff's own membership test) would land outside `worktree_path` —
/// lexical only, no filesystem read, so an artifact path that does not
/// exist yet is still answerable (P2.4 W1, `orient/refuse.md` §4).
///
/// Handles both directions correctly: an *absolute* `artifact_path`
/// already inside `worktree_path` (the Deterministic executor's own
/// shape, `executors/child.rs`: `cwd.join(&spec.name)` display()-
/// formatted, where `cwd == worktree_path`) is not an escape; a
/// *relative* `artifact_path` with enough `..` segments to walk back
/// out of `worktree_path`, or an absolute path naming somewhere else
/// entirely, is. `Path::join` alone cannot tell the two apart (an
/// absolute second argument replaces the first outright, and neither
/// `starts_with` nor `..` resolves without normalizing first) — this
/// builds the same joined path `server.rs:956`'s own `.join()` builds,
/// then collapses `.`/`..` components against it (never touching the
/// filesystem) before comparing prefixes.
fn artifact_join_escapes(worktree_path: &Path, artifact_path: &str) -> bool {
    artifact_relative_to_worktree(worktree_path, artifact_path).is_none()
}

/// The lexical join of `artifact_path` against `worktree_path`
/// (`artifact_join_escapes`'s own normalize), expressed relative to
/// `worktree_path` — `None` when the join escapes. W6 (P2.4 W1 follow-
/// up): the boundary diff's own membership test just below compares
/// each declared artifact against `changed_paths`' output, which is
/// always worktree-relative (`git diff --name-only`/`git status
/// --porcelain`, `wirk-herdr::git::changed_paths`) — an *absolute*
/// declared artifact path (the Docker/child executors' own shape,
/// `cwd.join(&spec.name)` display()-formatted) never matched that set
/// by raw string equality even when it named the exact file `git`
/// reported, so a Route's own declared output written as an absolute
/// path self-refused `OutOfBoundary` on itself. Canonicalizing both
/// sides once here (a lexical resolve of the join, R3 — no filesystem
/// read, so an artifact that does not exist yet is still answerable)
/// and comparing worktree-relative forms throughout fixes both the
/// escape check and this membership test with one shared computation.
fn artifact_relative_to_worktree(worktree_path: &Path, artifact_path: &str) -> Option<PathBuf> {
    let candidate = Path::new(artifact_path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        worktree_path.join(candidate)
    };
    let mut normalized: Vec<std::path::Component> = Vec::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    let normalized: PathBuf = normalized.into_iter().collect();
    normalized
        .strip_prefix(worktree_path)
        .ok()
        .map(PathBuf::from)
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

    // p2-route-files W2 (build-brief.md §7.1): every submit journals
    // the full Waypoint definitions it reserves onto
    // `WorkSubmitted.waypoint_defs` (`handle_submit`, above) — this is
    // the only place a Waypoint's kind/intent/command/outputs come
    // from, never a hardcoded lookup and never a second read of the
    // Route file. A Run whose Waypoint has no matching definition is a
    // data-integrity problem no valid submit can produce (every submit
    // path populates `waypoint_defs` unconditionally), so it is refused
    // the same way a mismatched triple is (D9#4's own reasoning) rather
    // than guessing at a fallback shape.
    let journaled_defs = waypoint_defs_for(&events);
    let Some(waypoint) = journaled_defs
        .iter()
        .find(|def| def.id == run.waypoint)
        .cloned()
    else {
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
    let mut verdict = validate_claim(&waypoint, &run, &claim);

    if matches!(verdict, ClaimVerdict::Validated)
        && let Some(worktree_path) = worktree_path_for_run(&events, &run_id)
    {
        // P2.4 W1 (build-brief.md §8 amendment 1; `orient/refuse.md`
        // §4): a declared artifact path escaping the worktree join
        // just below (`worktree_path.join(&artifact.path)`) is refused
        // before that join is ever taken — checked first so an
        // escaping path is never asked whether it "exists" at the
        // escaped location, which would otherwise read as an unrelated
        // `MissingArtifact`.
        //
        // Not a bare "contains `..` or is absolute" string test
        // (`refuse.md`'s own first cut, tried and reverted — probed
        // against `child_executor.rs::d5_1_true_completes_by_claim`,
        // 0040's real service, not a hypothesis): a Deterministic
        // Waypoint's own executor (`executors/child.rs`) always claims
        // an *absolute* artifact path, `cwd.join(&spec.name)`
        // display()-formatted — legitimate, since `cwd` there equals
        // `worktree_path` itself. Rejecting every absolute path
        // refused that real, correct Claim `OutOfBoundary`. The actual
        // question `server.rs:956`'s join needs answered is narrower:
        // does the join land inside `worktree_path`, not whether the
        // string looks suspicious — `artifact_join_escapes` answers
        // that directly, lexically (no filesystem read, the artifact
        // need not exist yet).
        if let Some(escaping) = claim
            .artifacts
            .iter()
            .find(|a| artifact_join_escapes(&worktree_path, &a.path))
        {
            verdict = ClaimVerdict::Refused(ClaimRefusal::OutOfBoundary(escaping.path.clone()));
        }

        if matches!(verdict, ClaimVerdict::Validated) {
            for artifact in &claim.artifacts {
                if !worktree_path.join(&artifact.path).exists() {
                    verdict =
                        ClaimVerdict::Refused(ClaimRefusal::MissingArtifact(artifact.name.clone()));
                    break;
                }
            }
        }

        // P2.4 W1: the worktree's own changes since the World's
        // `base_sha`, outside the Waypoint's Route-authored `boundary`,
        // refuse the Claim `OutOfBoundary` naming them — `worktree_path`
        // above already covers a Deterministic Waypoint's `cwd` too
        // (`worktree_path_for_run`'s own doc), so this one call site
        // enforces both kinds (`orient/check.md` "Item 5").
        if matches!(verdict, ClaimVerdict::Validated) {
            let base_sha = world_for_waypoint(&events, &run.waypoint)
                .map(|world| match world {
                    World::Actor(actor) => actor.base_sha,
                    World::Deterministic(deterministic) => deterministic.base_sha,
                })
                .unwrap_or_default();
            // A worktree the diff cannot read folds to "nothing
            // observed" rather than a hard failure — same reasoning as
            // `git.rs::fingerprint`'s own doc comment: unreadable is not
            // itself evidence of an out-of-boundary write.
            if let Ok(changed) = wirk_herdr::git::changed_paths(&worktree_path, &base_sha) {
                // Each declared artifact in its worktree-relative form
                // (W6 above): an absolute declared path — the
                // Docker/child executors' own shape — now matches
                // `changed`'s worktree-relative entries the same way a
                // relative declared path always has. Every artifact
                // here already passed the escape guard above (verdict
                // is still `Validated`), so `strip_prefix` never fails;
                // `unwrap_or_default` only guards a defensive fallback.
                let declared: std::collections::BTreeSet<String> = claim
                    .artifacts
                    .iter()
                    .map(|a| {
                        artifact_relative_to_worktree(&worktree_path, &a.path)
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                // P2.4 W2 (build-brief.md §3 W2; refuse.md §2): a Work
                // whose one repository binding is `Access::Read`
                // refuses any changed path at all, whatever the
                // Waypoint's globs say — `work.repositories.first()`
                // per orient's own read (a single-binding case; the
                // name/path match against `ActorWorld.repository` is
                // P2.5's question, carried, not answered here).
                let is_read_binding = work
                    .repositories
                    .first()
                    .is_some_and(|binding| binding.access == Access::Read);
                let offending: Vec<&String> = changed
                    .iter()
                    .filter(|p| !declared.contains(p.as_str()))
                    .filter(|p| is_read_binding || !boundary::allows(&waypoint.boundary, p))
                    .collect();
                if !offending.is_empty() {
                    let joined = offending
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    verdict = ClaimVerdict::Refused(ClaimRefusal::OutOfBoundary(joined));
                }
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
            && let Some(next_def) = journaled_defs.iter().find(|def| &def.id == next_id)
        {
            let prior_world = world_for_waypoint(&events, &run.waypoint);
            let cwd = worktree_path_for_run(&events, &run_id)
                .unwrap_or_else(|| state.estate_root.clone());
            let base_sha = prior_world
                .as_ref()
                .map(|world| match world {
                    World::Actor(actor) => actor.base_sha.clone(),
                    World::Deterministic(deterministic) => deterministic.base_sha.clone(),
                })
                .unwrap_or_default();
            // Wave 1 (P2.6, orient/route.md §3): minted before the match,
            // not after — an Actor World's `triple` needs the new Run's
            // own id (`ExecutionTriple` names the Run it belongs to); a
            // Deterministic World carries no triple and never hit this
            // ordering requirement, which is presumably why it was built
            // first.
            let next_run_id = RunId(mint_id("run"));
            let next_world = match next_def.kind {
                // p2-route-files W2 (build-brief.md §7.1): the next
                // Waypoint's own journaled definition carries its
                // command directly — `load_route` already refused a
                // Deterministic Waypoint with no command at submit
                // (`RouteError::DeterministicMissingCommand`), so this
                // is always `Some` for a Route a Work could ever have
                // been submitted against.
                WaypointKind::Deterministic => Some(World::Deterministic(DeterministicWorld {
                    command: next_def.command.clone().unwrap_or_default(),
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
                // Wave 1 (P2.6, orient/route.md §3): the same treatment
                // as the `Deterministic` arm above — a World reserved for
                // the next Waypoint, on the *same* worktree the Work's
                // prior Run already carries (`cwd` above, read back via
                // `worktree_path_for_run` regardless of the prior
                // Waypoint's own kind, so this covers both Actor→Actor
                // and Deterministic→Actor). `repository`/`branch` come
                // from the prior World when it was itself an Actor (the
                // common case); when the prior Waypoint was Deterministic
                // (a World shape that carries neither field) they fall
                // back to the Work's own repository binding and the one
                // branch this whole Work shares, the same
                // `format!("wirk/{}", ...)` `handle_submit` cuts once for
                // every Waypoint (one worktree per Work, never a second).
                WaypointKind::Actor => {
                    let (repository, branch) = match &prior_world {
                        Some(World::Actor(actor)) => {
                            (actor.repository.clone(), actor.branch.clone())
                        }
                        _ => (
                            work.repositories
                                .first()
                                .map(|binding| binding.name.clone())
                                .unwrap_or_default(),
                            format!("wirk/{}", work_id.0),
                        ),
                    };
                    Some(World::Actor(ActorWorld {
                        repository,
                        worktree_path: cwd,
                        branch,
                        base_sha,
                        triple: ExecutionTriple {
                            estate_root: state.estate_root.display().to_string(),
                            work_id: work_id.clone(),
                            run_id: next_run_id.clone(),
                        },
                        intent: next_def.intent.clone().unwrap_or_default(),
                        output_contract: OutputContract(next_def.declared_outputs.clone()),
                        boundary: next_def.boundary.clone(),
                    }))
                }
            };
            if let Some(next_world) = next_world {
                // W2b (land finding 2026-09-05, `w2b/BUILD.md`): the
                // reserved Actor World's triple is read from the Run
                // this advance actually opens — `next_run_id`, the one
                // value both the triple (above) and `RunOpened` (below)
                // are cloned from. Checked here, not merely assumed, so
                // a future edit that clones the wrong `RunId` (the
                // *prior* Run's — the exact mistake VERIFY.md's probe
                // (c) and `actor_then_actor_auto_advance_reserves_a_
                // world_for_the_second_actor` both pin) fails loudly
                // here rather than shipping a pane whose `WIRK_RUN_ID`
                // claims against the wrong Run.
                if let World::Actor(actor) = &next_world {
                    debug_assert_eq!(
                        actor.triple.run_id, next_run_id,
                        "the reserved Actor World's triple must carry the Run this advance opens, not a prior one"
                    );
                }
                let world_hash = WorldHash::of(&next_world);
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

    // P2.3 W1 (states.md §2): why the Work is (or last was) NeedsInput
    // — additive, absent when `needs_input` is `None` so an old caller
    // reading only the fields above is unaffected.
    if let Some(cause) = &work.needs_input {
        result["needs_input"] = json!({
            "run": cause.run.0,
            "reason": cause.reason,
            "detail": cause.detail,
        });
    }

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

/// P2.3 W2 (decide.md §1): the human's "try again" verb. Refuses
/// `NotNeedsInput` unless the folded Work is `NeedsInput` (no journal
/// write on refusal, mirrors 0046 D139's malformed-Route refusal);
/// `TripleMismatch` if the triple's `run_id` names no `RunOpened`
/// (D9#4, same check `claim`/`fail` make). Appends **only** a fresh
/// `RunOpened{run: <new RunId>, waypoint, attempt: 1, world_hash}` on
/// the failed Run's own Waypoint — no new `WaypointReserved`:
/// `world_for_waypoint` (the same "last `WaypointReserved` wins"
/// lookup `handle_status` uses) reads the World already reserved
/// there, so a Waypoint reserved twice since the failure retries
/// against the *second* World, never a stale one recomputed here
/// (decide.md §5's hazard). No count is invented: the human's choice is
/// the one `RunOpened`, D134 bars a count, not the event itself.
/// `fold`'s own `RunOpened` arm clears `NeedsInput` back to `Active` on
/// replay — this handler never sets `Work.state` itself, the journal
/// is the only truth (D9#1).
fn handle_retry(state: &Arc<WirkdState>, payload: RetryPayload) -> Reply {
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
    if events.is_empty() {
        return err_reply("NotFound", "no such work");
    }
    let work = fold(&events);
    if !matches!(work.state, WorkState::NeedsInput) {
        return err_reply("NotNeedsInput", "retry refused: the Work is not NeedsInput");
    }

    let Some(run) = find_run(&events, &run_id) else {
        return err_reply(
            "TripleMismatch",
            "the run id does not match any Run opened for this Work",
        );
    };

    let Some(world) = world_for_waypoint(&events, &run.waypoint) else {
        return err_reply(
            "JournalError",
            "no World is reserved for this Run's Waypoint",
        );
    };
    let world_hash = WorldHash::of(&world);
    let new_run_id = RunId(mint_id("run"));
    let event = new_event(
        &work_id,
        Some(new_run_id.clone()),
        EventKind::RunOpened {
            run: new_run_id.clone(),
            waypoint: run.waypoint.clone(),
            attempt: 1,
            world_hash,
        },
    );
    if let Err(err) = append_event(state, &mut journal, &work_id, &event) {
        return err_reply("JournalError", &err.to_string());
    }
    ok_reply(json!({"old_run_id": run_id.0, "new_run_id": new_run_id.0}))
}

/// P2.3 W2 (decide.md §1): the human's "give up" verb. Same refusal as
/// `retry` — `NotNeedsInput` unless the Work is `NeedsInput`, no
/// journal write — then appends `WorkFailed{cause}` with the reason
/// carried verbatim (0033 D102: explicit, never inferred from a
/// `RunFailed`); `fold`'s existing `WorkFailed` arm sets `Failed`,
/// terminal, so a second `workfail` call on the same Work is refused
/// too — the same guard, now reading `Failed` instead of `NeedsInput`.
fn handle_workfail(state: &Arc<WirkdState>, payload: WorkFailPayload) -> Reply {
    let work_id = payload.work_id.clone();

    let journal = match journal_for(state, &work_id) {
        Ok(journal) => journal,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    let events = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    if events.is_empty() {
        return err_reply("NotFound", "no such work");
    }
    let work = fold(&events);
    if !matches!(work.state, WorkState::NeedsInput) {
        return err_reply("NotNeedsInput", "fail refused: the Work is not NeedsInput");
    }

    let cause = FailureCause {
        status: None,
        request_id: None,
        at: now_ts(),
        detail: Some(payload.reason),
    };
    let event = new_event(&work_id, None, EventKind::WorkFailed { cause });
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
                waypoint_defs: Vec::new(),
            },
        }
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

    /// p2-route-files W2 (format.md §2): a bare name resolves against
    /// the estate's own `routes/` directory; a path-like value (a `/`
    /// or a `.json` suffix) resolves as the literal path, unchanged.
    #[test]
    fn resolve_route_path_bare_name_vs_path() {
        let estate = Path::new("/tmp/some-estate");
        assert_eq!(
            resolve_route_path(estate, "proving"),
            estate.join("routes").join("proving.json")
        );
        assert_eq!(
            resolve_route_path(estate, "./my-route.json"),
            PathBuf::from("./my-route.json")
        );
        assert_eq!(
            resolve_route_path(estate, "/abs/route.json"),
            PathBuf::from("/abs/route.json")
        );
    }
}
