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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use wirk_core::{
    Access, ActorWorld, ArtifactReceipt, ArtifactRef, ArtifactSpec, AttemptHolder,
    AuthoredSelection, Boundary, Claim, ClaimId, ClaimKind, ClaimRefusal, ClaimVerdict,
    DeterministicWorld, Event, EventKind, ExecutionTriple, FailureCause, Journal, JournalError,
    LaunchAttempt, OutcomeReceipt, OutputContract, ParentBinding, RepositoryBinding, Route,
    RouteId, Run, RunId, RunState, SourceBasis, Timestamp, WaypointDefinition, WaypointId,
    WaypointKind, WorkId, WorkState, World, WorldHash, ancestor_chain, find_definition,
    first_dfs_leaf, flatten_leaves, fold, load_route, validate_claim,
};

use super::boundary;
use super::{
    CancelPayload, ClaimPayload, ErrorDetail, FailPayload, RecordPayload, Reply, Request,
    RetryPayload, StatusPayload, SubmitPayload, Verb, WirkdPointer, WorkFailPayload,
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

/// Per-estate state: one `Arc<Mutex<Journal>>` per submitted Work, keyed by
/// `WorkId`, opened by submit or on a later read of its existing journal
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
    /// P3 W3: one Atlas owner for this daemon's one canonical estate
    /// (`estate_root`, already canonicalized before this state is
    /// built) — `wirk_atlas::AtlasStore` is itself a single-writer,
    /// estate-local catalog (its own module doc); no other component
    /// opens a second handle on the same `<estate_root>/atlas/`.
    atlas: Mutex<wirk_atlas::AtlasStore>,
    /// This estate's own continuation-signing secret (ruling 0095;
    /// W3-SECOND-CORRECTION.md item 1) — 32 bytes from the kernel CSPRNG,
    /// created once at daemon start under `<estate_root>/.wirk/` mode
    /// 0600 and re-read on every later start. It is a *daemon* secret,
    /// not Atlas catalog state, which is why it lives beside the pointer
    /// file rather than under `estate/atlas/`: a query must never create
    /// Atlas state (W3-CORRECTION.md item 3), and a fresh estate must
    /// still answer a search without a catalog appearing.
    ///
    /// A continuation token is only an *answer receipt* if this daemon
    /// actually issued it. Nothing in a token's plaintext is secret — a
    /// caller can read every field of its own answer — so a checksum over
    /// those fields is recomputable by the caller and proves nothing (0095:
    /// "do not mistake a client-recomputable checksum for authenticity").
    /// A MAC under a key the caller never sees is what makes "this is a
    /// generation vector I captured for you" checkable. Persisted, not
    /// in-memory, because a continuation must survive `wirkd` restart.
    continuation_key: [u8; 32],
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
    let persisted = journal.append(event)?;
    let mut watchers = state
        .watchers
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(senders) = watchers.get_mut(work_id) {
        senders.retain(|tx| tx.send(persisted.clone()).is_ok());
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

    let estate_root = std::fs::canonicalize(&estate_root).map_err(|source| WirkdError::Bind {
        socket: socket_path.clone(),
        source,
    })?;
    // P3 W3: the canonical estate scope Atlas checks every membership
    // against is this same canonicalized root — the filesystem identity
    // *is* the estate identity for this increment (BUILD-BRIEF.md:
    // "Until [a foundation EstateId] lands, APIs accept an opaque
    // estate scope from wirkd").
    let atlas = wirk_atlas::AtlasStore::open(&estate_root, estate_root.display().to_string())
        .map_err(|source| WirkdError::Bind {
            socket: socket_path.clone(),
            source: io::Error::other(source.to_string()),
        })?;
    let continuation_key =
        load_or_create_continuation_key(&wirk_dir).map_err(|source| WirkdError::Bind {
            socket: socket_path.clone(),
            source,
        })?;
    let state = Arc::new(WirkdState {
        estate_root,
        journals: Mutex::new(HashMap::new()),
        watchers: Mutex::new(HashMap::new()),
        atlas: Mutex::new(atlas),
        continuation_key,
    });

    // W5 (0035 D110): before this listener starts accepting
    // connections, re-adopt any docker containers a prior, killed
    // `wirkd` left running (module doc above `recover_docker_runs`).
    recover_docker_runs(&state);

    // W-A (§3.2): before this listener starts accepting connections,
    // re-evaluate any Work left `Waiting` by a crash between a child's
    // own completing Claim and this Work's `StageClosed` (module doc,
    // `reevaluate_waiting_works`).
    reevaluate_waiting_works(&state);

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
/// P3 native launch attempt admission: the connected client's own
/// process, as the kernel reports it for this socket's peer
/// (`SO_PEERCRED`). No client-declared pid is involved anywhere: a
/// client never sends its identity, so it cannot misstate it. This is
/// exclusion between cooperating invocations, not authentication —
/// anything that can write this estate's journal directly bypasses
/// wirkd entirely, and always could.
///
/// R3 fails here: `std::os::unix::net::UnixStream::peer_cred` is still
/// unstable (`peer_credentials_unix_socket`, rust-lang#42839) on this
/// workspace's pinned toolchain. R5 takes it instead — `libc`, already
/// this crate's direct dependency for `ChildExecutor`'s own `prctl`,
/// used the ordinary way rather than adopting a new crate for one
/// `getsockopt`.
///
/// `start_token` pins *which* process that pid is (`process_start_token`),
/// read here at admission time rather than trusted later, so a
/// recycled pid never reads as the original holder still running.
/// `None` when the kernel gives no pid for this peer: the caller then
/// refuses to admit an attempt rather than admitting one it cannot
/// name.
fn peer_holder(stream: &UnixStream) -> Option<AttemptHolder> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a plain `getsockopt(2)` on a socket this thread owns and
    // keeps alive across the call. `optval` points at a live, correctly
    // sized `ucred`, `optlen` at a `socklen_t` holding that size, and
    // the kernel writes at most that many bytes; the result is read
    // only after checking both the return code and the length the
    // kernel wrote back.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 || len as usize != std::mem::size_of::<libc::ucred>() {
        return None;
    }
    let pid = u32::try_from(cred.pid).ok().filter(|pid| *pid != 0)?;
    Some(AttemptHolder {
        pid,
        start_token: process_start_token(pid),
    })
}

/// The two fields of `/proc/<pid>/stat` an admitted holder is judged
/// by, read in one go from one read of one file so they always
/// describe the same instant:
///
/// * `state` (field 3) — the kernel's own answer to "is this process
///   still a running process at all". `Z` is the one that matters
///   here: dead, but not yet reaped by its parent, so still in `/proc`.
/// * `starttime` (field 22, clock ticks since boot) — the kernel's own
///   tiebreaker for pid reuse, read as an opaque token and never
///   interpreted as a time.
///
/// Parsed from after the **last** `)` because field 2 (`comm`) may
/// itself contain spaces and parentheses; the first field after it is
/// `state`, and `starttime` is the 19th after that. `None` on any
/// platform or condition where it cannot be read, which `holder_state`
/// treats as unverifiable rather than as either alive or gone.
fn process_stat(pid: u32) -> Option<(char, String)> {
    process_stat_at(&PathBuf::from(format!("/proc/{pid}/stat")))
}

/// `process_stat`'s parse, taking the stat file's path rather than a
/// pid — the seam a test drives with a real, injectable read failure
/// (L2: the outer `None` arm below `holder_state` treats as
/// unverifiable) instead of a live `/proc/<pid>` race. R3/R4: the same
/// `std::fs::read_to_string` call, only its path is now a parameter.
fn process_stat_at(path: &Path) -> Option<(char, String)> {
    let stat = std::fs::read_to_string(path).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let start_token = fields.nth(18)?.to_string();
    Some((state, start_token))
}

fn process_start_token(pid: u32) -> Option<String> {
    process_stat(pid).map(|(_, start_token)| start_token)
}

/// What the kernel says about a previously admitted attempt's holder.
/// Nothing here is a lease, a timeout or a heartbeat: the holder's own
/// process *is* the marker, so a holder that dies releases its attempt
/// by dying, and nothing a crashed holder leaves behind can trap a
/// Run — including its own corpse, which is what `state` is read for
/// (the review's Z1).
enum HolderState {
    /// That pid is a live process and is the same process wirkd
    /// admitted. *Live*, not *runnable*: a stopped holder (`T`) is
    /// alive, can be continued, and keeps what it holds.
    Live,
    /// No such process; or that pid is now a different process; or the
    /// process is there but dead — a zombie (`Z`) its parent has not
    /// reaped, which is exactly what a crashed `wirk run` leaves
    /// behind under a supervisor that does not `wait()`. A dead
    /// process cannot drive an agent, answer Herdr or write a journal,
    /// so it holds nothing.
    Gone,
    /// The kernel would not say (no `/proc`, or the holder was
    /// admitted without a start token and its pid is in use now).
    /// Refused like `Live` — wirkd will not replace an owner it cannot
    /// establish is gone — and reported as exactly that, so the
    /// operator sees a pid to check rather than an unexplained
    /// refusal. `wirk work retry`, which opens a *new* Run, remains
    /// available and takes no attempt from this one.
    Unverifiable,
}

fn holder_state(holder: &AttemptHolder) -> HolderState {
    if holder.pid == 0 {
        return HolderState::Gone;
    }
    holder_state_at(&PathBuf::from(format!("/proc/{}", holder.pid)), holder)
}

/// `holder_state`'s two syscalls, taking the `/proc/<pid>` directory as
/// a parameter — the seam L2's test drives with a real directory that
/// is not `/proc/<pid>` at all, so "the directory exists but its
/// `stat` entry cannot be read" is a real, constructed filesystem
/// failure rather than a live race against a pid's exit-and-reap
/// window. `holder_state` is the only caller in the running server; a
/// test calling this directly reaches the exact same two syscalls
/// (`metadata`, then `process_stat_at`) it would reach through a pid.
fn holder_state_at(proc_dir: &Path, holder: &AttemptHolder) -> HolderState {
    match std::fs::metadata(proc_dir) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => HolderState::Gone,
        Err(_) => HolderState::Unverifiable,
        // The pid is in `/proc`. That alone does not make it a running
        // process, so ask the kernel what it *is* before asking
        // whether it is the one that was admitted.
        Ok(_) => match process_stat_at(&proc_dir.join("stat")) {
            // Dead: a zombie waiting on its parent, or (`X`/`x`) on
            // its way out of the table. A pid names one process at a
            // time, so whether this corpse is the admitted holder or a
            // later occupant of its pid, the holder is not running —
            // the start token cannot change that answer, and there is
            // no uncertainty left for it to resolve.
            Some(('Z' | 'X' | 'x', _)) => HolderState::Gone,
            Some((_, current)) => match &holder.start_token {
                Some(admitted) if admitted == &current => HolderState::Live,
                Some(_) => HolderState::Gone,
                None => HolderState::Unverifiable,
            },
            // In `/proc` a moment ago, unreadable now: it may have
            // exited between the two reads, or this may be a platform
            // that does not answer. Not shown to be gone.
            None => HolderState::Unverifiable,
        },
    }
}

/// The whole of the attempt-admission rule, as one decision over
/// values: what this Run already has (`current`), who is asking
/// (`peer`), and where they would launch (`destination`). `Ok(())`
/// admits and supersedes; `Err` is the refusal text, which names both
/// sides so the client's own printed error explains itself.
///
/// Pure and kernel-backed: the only outside fact it reads is whether
/// the previous holder's process is still running (`holder_state`),
/// which is what makes it testable against real processes rather than
/// against a fake of one.
fn admit_launch_attempt(
    current: Option<&LaunchAttempt>,
    peer: &AttemptHolder,
    destination: &str,
) -> Result<(), String> {
    let Some(current) = current else {
        return Ok(());
    };
    if current.destination != destination {
        return Err(format!(
            "this Run's launch is bound to Herdr destination {}; this invocation is connected \
             to {destination} — an agent this Run may already have started is neither visible \
             nor name-colliding there, so launching from here could start a second one",
            current.destination
        ));
    }
    if current.holder.pid == peer.pid {
        return Ok(());
    }
    match holder_state(&current.holder) {
        HolderState::Gone => Ok(()),
        HolderState::Live => Err(format!(
            "this Run's launch attempt is held by process {}, which is still running",
            current.holder.pid
        )),
        HolderState::Unverifiable => Err(format!(
            "this Run's launch attempt is held by process {}, and wirkd cannot establish from \
             the kernel whether it is still running; it will not replace an owner it cannot \
             show is gone",
            current.holder.pid
        )),
    }
}

/// The other half of the same ownership: who may state what happened to
/// a Run whose attempt is held. `Ok(())` for the holder itself, for a
/// Run no attempt governs, and for wirkd's own internal recovery path
/// (`peer` is `None` there, since no client is on the other end).
fn admit_outcome_record(
    current: Option<&LaunchAttempt>,
    peer: Option<&AttemptHolder>,
    kind: &EventKind,
) -> Result<(), String> {
    if !matches!(
        kind,
        EventKind::RunLaunched { .. }
            | EventKind::RunFailed { .. }
            | EventKind::RunVanished
            | EventKind::LifecycleObserved { .. }
    ) {
        return Ok(());
    }
    let (Some(current), Some(peer)) = (current, peer) else {
        return Ok(());
    };
    if current.holder.pid == peer.pid {
        return Ok(());
    }
    Err(format!(
        "this Run's launch attempt is held by process {}; this process ({}) no longer owns it \
         and may not record its outcome",
        current.holder.pid, peer.pid
    ))
}

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

    // P3 native launch attempt admission: who is on the other end of
    // this connection, as the kernel reports it — read once, here,
    // where the socket still exists (`peer_holder`).
    let peer = peer_holder(&stream);
    let outcome = dispatch(&request, state, peer.as_ref());

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
            if let Ok(RunBinding {
                world: World::Deterministic(det),
                ..
            }) = resolve_run_binding(&events, estate_root, &work_id, run_id)
            {
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
                    // wirkd's own startup recovery, not a client: no
                    // peer process exists to name, and no Actor launch
                    // attempt governs a Deterministic Run.
                    None,
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

    let still_open = journal_for(state, &work_id)
        .ok()
        .flatten()
        .is_some_and(|journal| {
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

fn dispatch(request: &Request, state: &Arc<WirkdState>, peer: Option<&AttemptHolder>) -> Outcome {
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
            Ok(payload) => Outcome::Reply(handle_record(state, payload, peer)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::Cancel => match serde_json::from_value::<CancelPayload>(request.payload.clone()) {
            Ok(payload) => Outcome::Reply(handle_cancel(state, payload)),
            Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
        },
        Verb::AtlasAcquire => {
            match serde_json::from_value::<super::AtlasAcquirePayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_acquire(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasRefresh => {
            match serde_json::from_value::<super::AtlasRefreshPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_refresh(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasPublish => {
            match serde_json::from_value::<super::AtlasPublishPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_publish(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasSemanticBuild => {
            match serde_json::from_value::<super::AtlasSemanticBuildPayload>(
                request.payload.clone(),
            ) {
                Ok(payload) => Outcome::Reply(handle_atlas_semantic_build(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasSemanticSelect => {
            match serde_json::from_value::<super::AtlasSemanticSelectPayload>(
                request.payload.clone(),
            ) {
                Ok(payload) => Outcome::Reply(handle_atlas_semantic_select(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasStatus => {
            match serde_json::from_value::<super::AtlasStatusPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_status(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasSearch => {
            match serde_json::from_value::<super::AtlasSearchPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_search(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasResolve => {
            match serde_json::from_value::<super::AtlasResolvePayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_resolve(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasRelate => {
            match serde_json::from_value::<super::AtlasRelatePayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_relate(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
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
        Ok(Some(journal)) => journal,
        Ok(None) => {
            write_one_reply(&stream, &err_reply("NotFound", "no such work"));
            return;
        }
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
                leaves: Vec::new(),
                required_child_outcomes: Vec::new(),
                selection: None,
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
    // W-A (§3.1): the flattened DFS-ordered executable leaves, whatever
    // the tree's own nesting — a flat Route (no `Container` nodes)
    // flattens to itself unchanged (`old_flat_route_journal_folds_
    // identically`).
    let all_waypoints: Vec<WaypointId> = flatten_leaves(&waypoint_defs);
    let Some(waypoint_id) = all_waypoints.first().cloned() else {
        return err_reply("RouteError", "route has no executable waypoints");
    };
    let Some(first_def) = find_definition(&waypoint_defs, &waypoint_id).cloned() else {
        return err_reply(
            "RouteError",
            "route names no definition for its own first waypoint",
        );
    };

    let triple = ExecutionTriple {
        estate_root: state.estate_root.display().to_string(),
        work_id: work_id.clone(),
        run_id: run_id.clone(),
    };
    let output_contract = OutputContract(first_def.declared_outputs.clone());
    let branch = format!("wirk/{}", work_id.0);

    // P3 W3 (ruling 0090): resolved once, before any World is built, so
    // every arm below (and the child-spawn identity check further down)
    // reads the same name — never a bare `repositories.first()`.
    let execution_repo_name =
        match resolve_execution_repo(&payload.repositories, payload.execution_repo.as_deref()) {
            Ok(name) => name,
            Err((code, message)) => return err_reply(code, &message),
        };
    // Populated only where a real checkout (`repo_path`) exists to
    // verify at submit time (the Deterministic-Git and immediate-Actor
    // arms below); the bare Actor arm materializes its worktree later
    // via `wirk run`, with nothing yet to canonicalize here.
    let mut execution_identity: Option<String> = None;

    // The reserved World's own kind follows the *Route's* first
    // Waypoint (`first_def.kind`), not `payload.kind` directly — a
    // Route file's own authored order decides what gets reserved first
    // (build-brief.md §7.3's own "advances in the file's order", the
    // same rule auto-advance already applies to every later Waypoint,
    // `next_def.kind` below).
    let world = match first_def.kind {
        WaypointKind::Deterministic => {
            let requested_basis =
                payload
                    .source_basis
                    .clone()
                    .unwrap_or_else(|| SourceBasis::OutputOnly {
                        reference: payload.base_ref.clone(),
                    });
            if matches!(requested_basis, SourceBasis::OutputOnly { .. })
                && (payload
                    .repositories
                    .iter()
                    .any(|binding| binding.access == Access::Read)
                    || !first_def.boundary.0.is_empty())
            {
                return err_reply(
                    "IncompatibleSourceBasis",
                    "output-only execution cannot satisfy repository Read or Git boundary inspection",
                );
            }
            let (base_sha, source_basis, cwd) = match requested_basis {
                SourceBasis::Git { base } => {
                    let Some(repo_path) = payload.repo_path.clone() else {
                        return err_reply(
                            "BadRequest",
                            "deterministic Git inspection requires --repo-path <checkout>",
                        );
                    };
                    let verified = match resolve_git_sha(&repo_path, &base) {
                        Ok(sha) => sha,
                        Err(detail) => return err_reply("GitError", &detail),
                    };
                    if execution_repo_name.is_some() {
                        execution_identity = match canonical_repository_identity(&repo_path) {
                            Ok(identity) => Some(identity),
                            Err(detail) => return err_reply("GitError", &detail),
                        };
                    }
                    (
                        verified.clone(),
                        SourceBasis::Git { base: verified },
                        PathBuf::from(repo_path),
                    )
                }
                SourceBasis::OutputOnly { reference } => (
                    reference.clone(),
                    SourceBasis::OutputOnly { reference },
                    state.estate_root.clone(),
                ),
                SourceBasis::Unknown => {
                    return err_reply(
                        "BadRequest",
                        "new submissions cannot use an unknown source basis",
                    );
                }
            };
            World::Deterministic(DeterministicWorld {
                command: first_def.command.clone().unwrap_or_default(),
                base_sha,
                source_basis,
                cwd,
                env: BTreeMap::new(),
                expected_artifacts: output_contract,
            })
        }
        WaypointKind::Actor if payload.kind.as_deref() == Some("actor") => {
            if matches!(payload.source_basis, Some(SourceBasis::OutputOnly { .. })) {
                return err_reply(
                    "IncompatibleSourceBasis",
                    "actor execution requires a Git basis",
                );
            }
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
            if execution_repo_name.is_some() {
                execution_identity = match canonical_repository_identity(&repo_path) {
                    Ok(identity) => Some(identity),
                    Err(detail) => return err_reply("GitError", &detail),
                };
            }
            World::Actor(ActorWorld {
                repository: repo_path.clone(),
                // Empty until `wirk run` creates the worktree and
                // records the update (`handle_record`, `RecordPayload`'s
                // doc comment): the World is reserved before any
                // worktree exists.
                worktree_path: PathBuf::new(),
                branch,
                source_basis: SourceBasis::Git {
                    base: base_sha.clone(),
                },
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
            // P3 W3 (ruling 0090): the resolved execution binding's
            // name, never `repositories.first()` — a Work declaring
            // more than one `--repo` binding without saying which is
            // execution already refused above, so this arm only ever
            // sees an unambiguous name (or none, the legacy no-binding
            // fallback preserved verbatim).
            let repository = execution_repo_name
                .clone()
                .unwrap_or_else(|| route_id.0.clone());
            World::Actor(ActorWorld {
                repository,
                worktree_path: state.estate_root.clone(),
                branch,
                base_sha: payload.base_ref.clone(),
                source_basis: SourceBasis::Unknown,
                triple,
                intent: first_def.intent.clone().unwrap_or_default(),
                output_contract,
                // P2.4 W1 (build-brief.md §8 amendment 1): same fix as
                // the sibling arm above — the Route's own globs, not a
                // hardcoded empty boundary.
                boundary: first_def.boundary.clone(),
            })
        }
        // `waypoint_id` is `all_waypoints[0]`, drawn from `flatten_leaves`
        // (§3.1) — it can never resolve to a `Container` definition.
        WaypointKind::Container => {
            unreachable!("the flattened waypoint sequence names only executable leaves")
        }
    };
    let world_hash = WorldHash::of(&world);

    // W-A (§3.3): a child submission is checked and, if admitted,
    // journaled on the *parent's* journal (`ChildWorkSpawned`) before
    // this Work's own journal is created at all — "the reverse order
    // was rejected because it could produce a creditable child the
    // parent never recorded."
    // W-A correction (F4): `spawn_child_on_parent` resolves (or checks)
    // the container activation the child serves and hands it back, so
    // the child's *own* `WorkSubmitted.parent` records the same exact
    // generation the parent's `ChildWorkSpawned` does — the two-sided
    // binding both halves are later checked against.
    let mut recorded_parent = payload.parent.clone();
    if let Some(parent) = &payload.parent {
        match spawn_child_on_parent(
            state,
            parent,
            &work_id,
            &payload.repositories,
            execution_repo_name.as_deref(),
            execution_identity.as_deref(),
        ) {
            Ok(attempt) => {
                if let Some(binding) = recorded_parent.as_mut() {
                    binding.attempt = Some(attempt);
                }
            }
            Err((code, message)) => return err_reply(code, &message),
        }
    }

    let journal = match create_journal_for(state, &work_id) {
        Ok(journal) => journal,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());

    // W-A (§3.1): explicit journaled identity for every container this
    // first reservation newly enters (BUILD-AMENDMENTS.md: "name it and
    // journal it"), outermost first.
    let entering: Vec<WaypointId> = entering_ancestors(&waypoint_defs, &waypoint_id);

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
            parent: recorded_parent,
            execution_repo: execution_repo_name,
            execution_identity,
        },
    );
    if let Err(err) = append_event(state, &mut journal, &work_id, &submitted) {
        return err_reply("JournalError", &err.to_string());
    }
    for waypoint in &entering {
        let activated = new_event(
            &work_id,
            None,
            EventKind::ContainerActivated {
                waypoint: waypoint.clone(),
                attempt: 1,
            },
        );
        if let Err(err) = append_event(state, &mut journal, &work_id, &activated) {
            return err_reply("JournalError", &err.to_string());
        }
    }
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
    for event in [reserved, opened] {
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

/// W-A (§3.3): validates a child submission's `ParentBinding` against
/// the parent's own journal and, if admitted, appends `ChildWorkSpawned`
/// there. Checked, in order: the parent exists in this estate and is
/// not terminal; `waypoint` names a `Container` in the parent's own
/// `waypoint_defs` that declares `role` in `required_child_outcomes`;
/// `run` is the current, unsuperseded Run of the leaf that requested it
/// (found by walking up from that leaf's own ancestor chain to
/// `waypoint`, so a leaf nested arbitrarily deep under the named
/// container may request its role — `ChildParentMismatch` otherwise);
/// every one of `child_repositories` is bound no wider than the
/// parent's own binding of the same name (`ChildExceedsParentBinding`);
/// whenever a binding names the *parent's own* execution repository,
/// the child's and the parent's own real, wirkd-verified canonical
/// repository identities must also agree — always, never a Route
/// opt-in (ruling 0090, corrected by 0092 after a first draft of this
/// function gated the check behind a Route field that defaulted off:
/// that preserved the demonstrated defect as default behavior, which
/// is not what 0090 asked for). This is what
/// `loop-a-native-verify/VERDICT.md` §4 found missing: a same-named,
/// disconnected throwaway repository was admitted under the parent's
/// own alias with no check that it was actually the parent's
/// repository. A child's own, legitimately distinct output repository
/// must be admitted under a name the parent explicitly declared for
/// that purpose, never by reusing the parent's own execution alias.
/// Estate boundary is automatic: a `WorkId` from another estate simply
/// resolves to no journal here (`journal_for`'s own containment).
fn spawn_child_on_parent(
    state: &Arc<WirkdState>,
    parent: &ParentBinding,
    child_id: &WorkId,
    child_repositories: &[RepositoryBinding],
    child_execution_repo: Option<&str>,
    child_execution_identity: Option<&str>,
) -> Result<u32, (&'static str, String)> {
    let journal = journal_for(state, &parent.work)
        .map_err(|err| ("JournalError", err.to_string()))?
        .ok_or_else(|| ("ChildParentMismatch", "no such parent Work".to_string()))?;
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    let events = journal
        .replay()
        .map_err(|err| ("JournalError", err.to_string()))?;
    if events.is_empty() {
        return Err(("ChildParentMismatch", "no such parent Work".to_string()));
    }
    let parent_work = fold(&events);
    if parent_work.state.is_terminal() {
        return Err((
            "ChildParentMismatch",
            "the parent Work is already terminal".to_string(),
        ));
    }
    let parent_defs = waypoint_defs_for(&events);
    let Some(container_def) = find_definition(&parent_defs, &parent.waypoint) else {
        return Err((
            "ChildParentMismatch",
            "the parent names no such Waypoint".to_string(),
        ));
    };
    if !matches!(container_def.kind, WaypointKind::Container)
        || !container_def
            .required_child_outcomes
            .iter()
            .any(|spec| spec.role == parent.role)
    {
        return Err((
            "ChildParentMismatch",
            "the named Waypoint is not a container declaring this role".to_string(),
        ));
    }
    let Some(requesting_run) = find_run(&events, &parent.run) else {
        return Err((
            "ChildParentMismatch",
            "the parent names no such Run".to_string(),
        ));
    };
    if !ancestor_chain(&parent_defs, &requesting_run.waypoint).contains(&parent.waypoint) {
        return Err((
            "ChildParentMismatch",
            "the named Run's own Waypoint is not nested under the named container".to_string(),
        ));
    }
    if latest_run_for_waypoint(&events, &requesting_run.waypoint).map(|entry| entry.0)
        != Some(parent.run.clone())
    {
        return Err((
            "ChildParentMismatch",
            "the named Run has been superseded by a retry".to_string(),
        ));
    }
    for binding in child_repositories {
        let Some(parent_binding) = parent_work
            .repositories
            .iter()
            .find(|p| p.name == binding.name)
        else {
            return Err((
                "ChildExceedsParentBinding",
                format!(
                    "repository {} is not bound by the parent Work",
                    binding.name
                ),
            ));
        };
        let within = match binding.access {
            Access::Read => matches!(parent_binding.access, Access::Read | Access::Write),
            Access::Write => matches!(parent_binding.access, Access::Write),
        };
        if !within {
            return Err((
                "ChildExceedsParentBinding",
                format!(
                    "repository {} exceeds the parent's own binding",
                    binding.name
                ),
            ));
        }
        // Ruling 0090/0092: name/access alone cannot tell a child
        // genuinely inheriting the parent's own repository from one
        // bound only to a same-named, disconnected throwaway repository
        // (`loop-a-native-verify/VERDICT.md` §4) — and this is not a
        // Route opt-in (0092: an opt-in that defaults off preserves the
        // demonstrated defect as default behavior). Whenever this
        // binding's name *is* the parent's own resolved execution
        // repository, a child that also declares this same name as its
        // own execution repository is inheriting that specific
        // repository's authority, not merely a permission label —
        // wirkd verifies the two checkouts are actually the same
        // repository before admitting it. A child that wants its own,
        // legitimately distinct output repository must be admitted
        // under a name the parent does *not* already use for its own
        // execution checkout (a separate binding the parent explicitly
        // declared for that purpose); reusing the parent's own
        // execution alias for a different repository is exactly the
        // defect this closes, never a supported shape.
        if parent_work.execution_repo.as_deref() == Some(binding.name.as_str()) {
            if Some(binding.name.as_str()) != child_execution_repo {
                return Err((
                    "ChildExceedsParentBinding",
                    format!(
                        "repository {} is the parent's own execution repository; the child must declare it as its own execution repository too, not merely list it",
                        binding.name
                    ),
                ));
            }
            let Some(parent_identity) = parent_work.execution_identity.as_deref() else {
                return Err((
                    "ChildExceedsParentBinding",
                    format!(
                        "repository {} cannot be verified against the parent's own unverified execution binding",
                        binding.name
                    ),
                ));
            };
            let Some(child_identity) = child_execution_identity else {
                return Err((
                    "ChildExceedsParentBinding",
                    format!(
                        "repository {} has no resolvable execution identity for the child",
                        binding.name
                    ),
                ));
            };
            if child_identity != parent_identity {
                return Err((
                    "ChildExceedsParentBinding",
                    format!(
                        "repository {} does not resolve to the parent's own repository",
                        binding.name
                    ),
                ));
            }
        }
    }

    // W-A correction (F4): the child serves one *generation* of the
    // container, not the container in the abstract. A submission may
    // name it explicitly (and is refused if that generation is not the
    // current one — a stale request cannot be admitted, let alone
    // credited), or leave it open and take the current one.
    let current_attempt = container_attempt(&events, &parent.waypoint);
    if let Some(named) = parent.attempt
        && named != current_attempt
    {
        return Err((
            "ChildParentMismatch",
            format!(
                "the named container activation {named} is not {}'s current activation {current_attempt}",
                parent.waypoint.0
            ),
        ));
    }

    let spawned = new_event(
        &parent.work,
        Some(parent.run.clone()),
        EventKind::ChildWorkSpawned {
            role: parent.role.clone(),
            child: child_id.clone(),
            waypoint: parent.waypoint.clone(),
            attempt: current_attempt,
            run: parent.run.clone(),
        },
    );
    append_event(state, &mut journal, &parent.work, &spawned)
        .map_err(|err| ("JournalError", err.to_string()))?;
    Ok(current_attempt)
}

/// The container ids `waypoint` newly enters — every ancestor (§3.1)
/// for which `waypoint` is the DFS-first leaf of that ancestor's own
/// subtree — outermost first, the order `ContainerActivated` is
/// journaled in.
fn entering_ancestors(tree: &[WaypointDefinition], waypoint: &WaypointId) -> Vec<WaypointId> {
    let mut chain = ancestor_chain(tree, waypoint);
    chain.retain(|ancestor| {
        find_definition(tree, ancestor).and_then(first_dfs_leaf) == Some(waypoint)
    });
    chain.reverse();
    chain
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

/// P3 W3 (ruling 0090): resolves which declared `--repo` binding is the
/// Work's actual execution/write checkout. `requested` must name a real
/// binding when given; when omitted, a single binding is unambiguous
/// (the legacy submit line keeps working unchanged) but more than one
/// binding is refused rather than silently taking `repositories[0]` —
/// the defect this whole correction exists to remove
/// (`is_read_binding`'s and the bare Actor World's own former `.first()`
/// reads, both replaced by this resolved name).
fn resolve_execution_repo(
    repositories: &[RepositoryBinding],
    requested: Option<&str>,
) -> Result<Option<String>, (&'static str, String)> {
    match requested {
        Some(name) => {
            if repositories.iter().any(|binding| binding.name == name) {
                Ok(Some(name.to_string()))
            } else {
                Err((
                    "UnknownExecutionRepository",
                    format!("--execution-repo {name} names no --repo binding on this submission"),
                ))
            }
        }
        None => match repositories.len() {
            0 => Ok(None),
            1 => Ok(Some(repositories[0].name.clone())),
            _ => Err((
                "AmbiguousExecutionRepository",
                "more than one --repo binding is declared; --execution-repo <name> must say \
                 which one is the execution checkout"
                    .to_string(),
            )),
        },
    }
}

/// P3 W3 (ruling 0090): the real, wirkd-verified identity of the
/// repository backing an execution checkout — `git rev-parse
/// --path-format=absolute --git-common-dir`, canonicalized. Two
/// worktrees of the same repository (`git worktree add`) share one
/// common Git directory and therefore resolve identically here; two
/// unrelated repositories that merely happen to share a `--repo` alias
/// or a same-named on-disk directory do not. This is the check
/// `loop-a-native-verify/VERDICT.md` §4 found missing: admission by
/// binding name/access alone could not tell a child genuinely bound to
/// the parent's own repository from one bound only to a same-named,
/// disconnected throwaway repository.
fn canonical_repository_identity(repo_path: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git -C {repo_path} rev-parse --git-common-dir failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    std::fs::canonicalize(&raw)
        .map(|p| p.display().to_string())
        .map_err(|err| format!("could not canonicalize resolved Git common directory {raw}: {err}"))
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
fn handle_record(
    state: &Arc<WirkdState>,
    payload: RecordPayload,
    peer: Option<&AttemptHolder>,
) -> Reply {
    if matches!(
        payload.kind,
        EventKind::WorkSubmitted { .. }
            | EventKind::RunOpened { .. }
            | EventKind::ClaimFiled { .. }
            | EventKind::ClaimRecorded { .. }
            | EventKind::WorkFailed { .. }
            | EventKind::WorkCanceled { .. }
            | EventKind::ContainerActivated { .. }
            | EventKind::StageHeld { .. }
            | EventKind::StageClosed { .. }
            | EventKind::ChildWorkSpawned { .. }
    ) {
        return err_reply(
            "Forbidden",
            "this transition is owned by wirkd or the operator, never by record",
        );
    }

    let journal = match journal_for(state, &payload.work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
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
    let Some(run_id) = payload.run.as_ref() else {
        return err_reply(
            "InvalidTransition",
            "record observations must name an existing Run",
        );
    };
    let Some(run) = find_run(&events, run_id) else {
        return err_reply("InvalidTransition", "record names an unknown Run");
    };
    if fold(&events).state.is_terminal()
        || !matches!(run.state, RunState::Open)
        || latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0)
            != Some(run_id.clone())
    {
        return err_reply(
            "InvalidTransition",
            "record does not target the current open Run",
        );
    }

    // P3 native launch attempt admission: a replaced owner goes quiet.
    // Once some invocation holds this Run's launch attempt, only that
    // process may state what happened to the launch or what the pane
    // is doing — a superseded one cannot publish a competing
    // `RunFailed`, cannot append lifecycle observations to the Run it
    // no longer drives, and learns it was replaced from this refusal
    // (`RunLoop` stops driving on it rather than keeping a pane it no
    // longer owns prompted).
    if let Err(refusal) = admit_outcome_record(run.launch_attempt.as_ref(), peer, &payload.kind) {
        return err_reply("InvalidTransition", &refusal);
    }

    let kind = match payload.kind {
        EventKind::WorktreeCreated { repo, base_sha } => {
            let binding =
                match resolve_run_binding(&events, &state.estate_root, &payload.work_id, run_id) {
                    Ok(binding) => binding,
                    Err(reason) => return err_reply("ValidationUnavailable", &reason),
                };
            let World::Actor(actor) = binding.world else {
                return err_reply("InvalidTransition", "only an Actor Run creates a worktree");
            };
            if binding.materialized
                || events.iter().any(|event| {
                    event.run.as_ref() == Some(run_id)
                        && matches!(event.kind, EventKind::WorktreeCreated { .. })
                })
                || actor.repository != repo
                || actor.base_sha != base_sha
                || resolve_git_sha(&repo, &base_sha).as_deref() != Ok(base_sha.as_str())
            {
                return err_reply(
                    "InvalidTransition",
                    "WorktreeCreated does not match this Run's unmaterialized Actor binding",
                );
            }
            EventKind::WorktreeCreated { repo, base_sha }
        }
        EventKind::WaypointReserved {
            waypoint,
            world_hash,
            world,
        } => {
            let Some(previous) = events.last() else {
                return err_reply(
                    "InvalidTransition",
                    "materialization has no preceding event",
                );
            };
            let EventKind::WorktreeCreated { repo, base_sha } = &previous.kind else {
                return err_reply(
                    "InvalidTransition",
                    "Actor materialization must immediately follow WorktreeCreated",
                );
            };
            if previous.run.as_ref() != Some(run_id) || waypoint != run.waypoint {
                return err_reply("InvalidTransition", "materialization names a different Run");
            }
            let binding =
                match resolve_run_binding(&events, &state.estate_root, &payload.work_id, run_id) {
                    Ok(binding) => binding,
                    Err(reason) => return err_reply("ValidationUnavailable", &reason),
                };
            let World::Actor(initial) = binding.world else {
                return err_reply(
                    "InvalidTransition",
                    "only an Actor World can be materialized",
                );
            };
            let World::Actor(updated) = &world else {
                return err_reply("InvalidTransition", "materialization changed World kind");
            };
            let mut expected = initial.clone();
            expected.worktree_path = updated.worktree_path.clone();
            if binding.materialized
                || expected != *updated
                || updated.worktree_path.as_os_str().is_empty()
                || !paths_equal(
                    &state.estate_root.join("worktrees").join(&payload.work_id.0),
                    &updated.worktree_path,
                )
                || repo != &updated.repository
                || base_sha != &updated.base_sha
                || world_hash != run.world_hash
                || WorldHash::of(&world) != world_hash
            {
                return err_reply(
                    "InvalidTransition",
                    "materialized World does not match its Run",
                );
            }
            EventKind::WaypointReserved {
                waypoint,
                world_hash,
                world,
            }
        }
        // P3 native launch selection D1: the pre-launch admission
        // itself. At most one per Run, checked against the Run's own
        // replayed journal while this handler holds the journal lock —
        // that lock, not a git worktree lock and not Herdr's agent-name
        // uniqueness, is what makes two concurrent `wirk run`
        // invocations for one Run resolve to exactly one admitted
        // request. The loser is refused here, before it has called
        // Herdr at all, so it has nothing to relaunch and nothing to
        // journal against the winner's Run.
        EventKind::RunLaunchRequested {
            run: inner,
            actor_kind,
            selection,
        } => {
            if &inner != run_id
                || events.iter().any(|event| {
                    event.run.as_ref() == Some(run_id)
                        && matches!(event.kind, EventKind::RunLaunchRequested { .. })
                })
            {
                return err_reply(
                    "InvalidTransition",
                    "this Run's launch request is already bound",
                );
            }
            match resolve_run_binding(&events, &state.estate_root, &payload.work_id, run_id) {
                Ok(binding) if binding.materialized && matches!(binding.world, World::Actor(_)) => {
                }
                Ok(_) => return err_reply("InvalidTransition", "Actor Run is not materialized"),
                Err(reason) => return err_reply("ValidationUnavailable", &reason),
            }
            EventKind::RunLaunchRequested {
                run: inner,
                actor_kind,
                selection,
            }
        }
        // P3 native launch attempt admission (the independent review's
        // N1): the *attempt*, admitted the same way and in the same
        // place the request is. Admitting the request once is not
        // enough — once it is bound, a duplicate invocation and a
        // recovery invocation both used to fall straight through to
        // `agent.start`, with only Herdr's own agent-name uniqueness
        // between them, which is exactly the incidental guard this
        // contract may not lean on (and which does not exist at all
        // across two Herdr sessions).
        //
        // Three refusals, all under this handler's journal lock:
        // the destination this Run's launch is bound to; a holder the
        // kernel still reports running; and a client wirkd cannot
        // name. Everything else is admitted and supersedes — a holder
        // that died releases its attempt by dying, so there is no
        // marker to leak and no valid Run to trap.
        EventKind::RunLaunchAttempted {
            run: inner,
            destination,
            ..
        } => {
            if &inner != run_id {
                return err_reply("InvalidTransition", "launch attempt names a different Run");
            }
            let Some(peer) = peer else {
                return err_reply(
                    "Forbidden",
                    "wirkd could not identify this client's process; a launch attempt is \
                     admitted only to a process it can name",
                );
            };
            if !events.iter().any(|event| {
                event.run.as_ref() == Some(run_id)
                    && matches!(event.kind, EventKind::RunLaunchRequested { .. })
            }) {
                return err_reply(
                    "InvalidTransition",
                    "a launch attempt precedes this Run's admitted launch request",
                );
            }
            if let Err(refusal) =
                admit_launch_attempt(run.launch_attempt.as_ref(), peer, &destination)
            {
                return err_reply("InvalidTransition", &refusal);
            }
            EventKind::RunLaunchAttempted {
                run: inner,
                destination,
                holder: peer.clone(),
            }
        }
        EventKind::RunLaunched {
            run: inner,
            actor_kind,
            selection,
            launch_argv,
        } => {
            // P3 native launch selection: this duplicate check is
            // already the server-side half of "a repeated invocation
            // must not silently alter an already fixed Run launch"
            // (PREPARATION-ADJUDICATION.md point 3) — at most one
            // `RunLaunched` is ever accepted per Run, whatever
            // `selection`/`launch_argv` it carries, so a second
            // resolved request (even one identical to the first) is
            // refused here exactly as a second `actor_kind` always was.
            // `wirk run`'s own client-side check (`executor.rs`) is what
            // stops the *live relaunch* from ever happening in the first
            // place; this is the durable record's own backstop.
            if &inner != run_id
                || events.iter().any(|event| {
                    event.run.as_ref() == Some(run_id)
                        && matches!(event.kind, EventKind::RunLaunched { .. })
                })
            {
                return err_reply(
                    "InvalidTransition",
                    "RunLaunched is mismatched or duplicate",
                );
            }
            match resolve_run_binding(&events, &state.estate_root, &payload.work_id, run_id) {
                Ok(binding) if binding.materialized && matches!(binding.world, World::Actor(_)) => {
                }
                Ok(_) => return err_reply("InvalidTransition", "Actor Run is not materialized"),
                Err(reason) => return err_reply("ValidationUnavailable", &reason),
            }
            // D1's other half: a launch result may only ever state the
            // request that was already admitted for this Run. Without
            // this, the pre-launch binding would be advisory — a caller
            // could bind one selection and then report another.
            let bound = events.iter().find_map(|event| match &event.kind {
                EventKind::RunLaunchRequested {
                    run: bound_run,
                    actor_kind,
                    selection,
                } if bound_run == run_id && event.run.as_ref() == Some(run_id) => {
                    Some((actor_kind.clone(), selection.clone()))
                }
                _ => None,
            });
            match bound {
                None => {
                    return err_reply(
                        "InvalidTransition",
                        "RunLaunched without an admitted RunLaunchRequested",
                    );
                }
                Some((bound_kind, bound_selection))
                    if bound_kind != actor_kind || bound_selection != selection =>
                {
                    return err_reply(
                        "InvalidTransition",
                        "RunLaunched does not match this Run's admitted launch request",
                    );
                }
                Some(_) => {}
            }
            EventKind::RunLaunched {
                run: inner,
                actor_kind,
                selection,
                launch_argv,
            }
        }
        EventKind::LifecycleObserved { status, detail } => {
            // D1: an admitted launch request is enough. A launch whose
            // reply was lost still really started an agent, and the
            // reconciliation that discovers it (`RunLoop::launch`) has
            // to be able to say so.
            let launched = events.iter().any(|event| {
                event.run.as_ref() == Some(run_id)
                    && matches!(
                        &event.kind,
                        EventKind::RunLaunched { run: inner, .. }
                            | EventKind::RunLaunchRequested { run: inner, .. }
                            if inner == run_id
                    )
            });
            if !launched {
                return err_reply("InvalidTransition", "lifecycle observation precedes launch");
            }
            EventKind::LifecycleObserved { status, detail }
        }
        EventKind::RunFailed { mut cause } => {
            cause.at = now_ts();
            EventKind::RunFailed { cause }
        }
        EventKind::RunVanished => EventKind::RunVanished,
        EventKind::WorkSubmitted { .. }
        | EventKind::RunOpened { .. }
        | EventKind::ClaimFiled { .. }
        | EventKind::ClaimRecorded { .. }
        | EventKind::WorkFailed { .. }
        | EventKind::WorkCanceled { .. }
        | EventKind::ContainerActivated { .. }
        | EventKind::StageHeld { .. }
        | EventKind::StageClosed { .. }
        | EventKind::ChildWorkSpawned { .. } => unreachable!(),
    };
    let event = new_event(&payload.work_id, Some(run_id.clone()), kind);
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

/// Canonical containment after the lexical and existence checks.  A failed
/// inspection is deliberately distinct from a successful proof that a path
/// escapes (0067): it cannot be turned into `OutOfBoundary` evidence.
fn artifact_canonical_containment(
    worktree_path: &Path,
    artifact_path: &str,
) -> Result<bool, String> {
    let joined = if Path::new(artifact_path).is_absolute() {
        PathBuf::from(artifact_path)
    } else {
        worktree_path.join(artifact_path)
    };
    let canonical_root = std::fs::canonicalize(worktree_path).map_err(|err| {
        format!(
            "cannot inspect canonical worktree {}: {err}",
            worktree_path.display()
        )
    })?;
    let canonical_artifact = std::fs::canonicalize(&joined).map_err(|err| {
        format!(
            "cannot inspect canonical artifact {}: {err}",
            joined.display()
        )
    })?;
    Ok(!canonical_artifact.starts_with(&canonical_root))
}

fn handle_claim(state: &Arc<WirkdState>, payload: ClaimPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();

    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the claim's estate root does not identify this daemon's estate",
        );
    }

    let journal = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
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
            ClaimOutcome {
                claim_id,
                claim_kind: payload.kind,
                verdict: ClaimVerdict::Refused(ClaimRefusal::TripleMismatch),
                artifacts: Vec::new(),
            },
        );
    };

    // The Work this Run belongs to: `events` is guaranteed non-empty
    // here (finding a `RunOpened` above required a `WorkSubmitted`
    // first — `fold`'s own precondition), so this never hits the
    // "no WorkSubmitted event" panic.
    let work = fold(&events);
    // W-A correction (minor finding): a Claim against an already
    // terminal Work is refused *before* anything is appended. The
    // pre-correction path journaled `ClaimFiled`/`ClaimRecorded`
    // against a `Completed`/`Failed`/`Canceled` Work — false progress
    // on a record that is supposed to be closed. This is not the
    // "late but valid claim" case d9_5 protects (that is about a Run's
    // own state on a live Work); the Work itself is over.
    if work.state.is_terminal() {
        return err_reply(
            "WorkTerminal",
            "the Work is already terminal: no further Claim can be filed against it",
        );
    }
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
            ClaimOutcome {
                claim_id,
                claim_kind: payload.kind,
                verdict: ClaimVerdict::Refused(ClaimRefusal::TripleMismatch),
                artifacts: Vec::new(),
            },
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
    let Some(waypoint) = find_definition(&journaled_defs, &run.waypoint).cloned() else {
        return record_and_reply(
            state,
            &mut journal,
            &work_id,
            &run_id,
            ClaimOutcome {
                claim_id,
                claim_kind: payload.kind,
                verdict: ClaimVerdict::Refused(ClaimRefusal::TripleMismatch),
                artifacts: Vec::new(),
            },
        );
    };
    let binding = resolve_run_binding(&events, &state.estate_root, &work_id, &run_id);
    let mut verdict = match (&payload.kind, &binding) {
        (_, Err(reason)) => {
            ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(reason.clone()))
        }
        (ClaimKind::Done, Ok(binding))
            if matches!(binding.world, World::Actor(_)) && !binding.materialized =>
        {
            ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(
                "Actor checkout has not been materialized for this Run".to_string(),
            ))
        }
        _ => validate_claim(&waypoint, &run, &claim),
    };

    if matches!(verdict, ClaimVerdict::Validated)
        && let Ok(binding) = &binding
    {
        let worktree_path = match &binding.world {
            World::Actor(actor) => actor.worktree_path.clone(),
            World::Deterministic(deterministic) => deterministic.cwd.clone(),
        };
        // 0069 correction (REVERIFY-ASSESSMENT.md's named
        // pre-materialization limit): an Actor Run whose checkout has
        // not yet materialized has no real worktree to inspect at all
        // — `worktree_path` above is empty (`ActorWorld`'s own doc). A
        // Claim naming any artifact refuses `ValidationUnavailable`
        // with that reason directly, before any of the escape/
        // existence/canonical checks below ever run: every one of them
        // joins a (possibly relative) artifact path against
        // `worktree_path`, and an empty `worktree_path` lets a relative
        // join resolve, once a real filesystem syscall (`.exists()`,
        // `canonicalize`) touches it, against the *daemon's own*
        // current working directory — an unrelated file sitting there
        // must never be able to change the outcome. A Done claim never
        // reaches this arm unmaterialized (the earlier verdict match
        // already refuses it first); an artifact-free Question has
        // nothing here to inspect and is unaffected.
        if !binding.materialized && !claim.artifacts.is_empty() {
            verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(
                "Actor checkout has not been materialized for this Run; no worktree is available to inspect the claimed artifacts".to_string(),
            ));
        } else {
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
                        verdict = ClaimVerdict::Refused(ClaimRefusal::MissingArtifact(
                            artifact.name.clone(),
                        ));
                        break;
                    }
                }
            }

            if matches!(verdict, ClaimVerdict::Validated) {
                for artifact in &claim.artifacts {
                    match artifact_canonical_containment(&worktree_path, &artifact.path) {
                        Ok(true) => {
                            verdict = ClaimVerdict::Refused(ClaimRefusal::OutOfBoundary(
                                artifact.path.clone(),
                            ));
                            break;
                        }
                        Ok(false) => {}
                        Err(detail) => {
                            verdict =
                                ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(detail));
                            break;
                        }
                    }
                }
            }

            // P2.4 W1: the worktree's own changes since the World's
            // `base_sha`, outside the Waypoint's Route-authored `boundary`,
            // refuse the Claim `OutOfBoundary` naming them — `worktree_path`
            // above already covers a Deterministic Waypoint's `cwd` too
            // (`worktree_path_for_run`'s own doc), so this one call site
            // enforces both kinds (`orient/check.md` "Item 5"). Gated on
            // `binding.materialized` (0069 correction): an unmaterialized
            // Actor's `worktree_path` is empty — nothing exists yet to
            // diff against — so a Question filed before materialization
            // (legitimately Validated, no artifacts to check above) must
            // not be sent through a git diff against an empty path. A Done
            // claim never reaches this arm unmaterialized: the verdict
            // match above already refuses it first.
            if matches!(verdict, ClaimVerdict::Validated) && binding.materialized {
                let base_sha = match &binding.world {
                    World::Actor(actor) => actor.base_sha.clone(),
                    World::Deterministic(deterministic) => deterministic.base_sha.clone(),
                };
                let changed = match binding.world.source_basis() {
                    SourceBasis::Git { .. } => {
                        match wirk_herdr::git::changed_paths(&worktree_path, &base_sha) {
                            Ok(changed) => Some(changed),
                            Err(err) => {
                                verdict = ClaimVerdict::Refused(
                                    ClaimRefusal::ValidationUnavailable(err.to_string()),
                                );
                                None
                            }
                        }
                    }
                    SourceBasis::OutputOnly { .. } => None,
                    SourceBasis::Unknown => {
                        verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(
                            "source basis is unknown".to_string(),
                        ));
                        None
                    }
                };
                if let Some(changed) = changed {
                    // Each declared artifact in its worktree-relative form
                    // (W6 above): an absolute declared path — the
                    // Docker/child executors' own shape — now matches
                    // `changed`'s worktree-relative entries the same way a
                    // relative declared path always has. Every artifact
                    // here already passed the escape guard above (verdict
                    // is still `Validated`), so `strip_prefix` never fails;
                    // `unwrap_or_default` only guards a defensive fallback.
                    let mut declared: std::collections::BTreeSet<String> = claim
                        .artifacts
                        .iter()
                        .map(|a| {
                            artifact_relative_to_worktree(&worktree_path, &a.path)
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        })
                        .collect();
                    // W4 (P2.6 run 3, rerun3's `orient.md`-vs-`build`
                    // finding): a Route's own earlier Waypoints (of this
                    // same Work, in the journaled Route order) already left
                    // their own declared outputs sitting untracked in the
                    // shared worktree — `orient.md` for `orient`, before
                    // `build` ever runs. Excluded from `offending` the same
                    // way this Claim's *own* declared artifacts already are
                    // just above (0050): a Waypoint's boundary names what
                    // *it* may write, never a refusal of evidence a prior,
                    // already-Claimed Waypoint of the same Work left behind.
                    // A later Waypoint that legitimately edits an earlier
                    // one's output is unaffected — only the *name itself*
                    // is excluded from `offending`, not from `boundary`'s
                    // own glob check, so a later Waypoint whose own boundary
                    // covers that path can still declare and reclaim it.
                    // W-A (§3.1): walked via `find_definition` against
                    // the full (possibly nested) tree, not a top-level
                    // scan — a nested leaf's own id never appears as a
                    // top-level `journaled_defs` entry, only its
                    // ancestor container's does, so a flat iteration
                    // over `journaled_defs` alone would silently exclude
                    // nothing for any leaf inside a container.
                    // W-A correction (F1/F2, reopen): "already left
                    // behind by another Waypoint of this same Work" is
                    // not a *route-position* fact once a stage can be
                    // reopened. Re-running an earlier leaf finds a
                    // later leaf's already-validated output sitting in
                    // the shared checkout, and refusing it
                    // `OutOfBoundary` would make the correction path
                    // unusable. The honest test is whether some other
                    // leaf of this Work actually claimed that output:
                    // every leaf before this one in Route order (which
                    // can only have been reached through a validated
                    // Claim) plus every leaf that currently holds one.
                    let route_order = route_waypoints(&events);
                    let mut settled: Vec<&WaypointId> = Vec::new();
                    if let Some(pos) = route_order.iter().position(|w| w == &waypoint.id) {
                        settled.extend(&route_order[..pos]);
                    }
                    for leaf_id in &route_order {
                        if leaf_id == &waypoint.id {
                            continue;
                        }
                        if latest_run_for_waypoint(&events, leaf_id)
                            .and_then(|(run_id, ..)| find_run(&events, &run_id))
                            .is_some_and(|run| matches!(run.state, RunState::Claimed(_)))
                        {
                            settled.push(leaf_id);
                        }
                    }
                    for leaf_id in settled {
                        if let Some(def) = find_definition(&journaled_defs, leaf_id) {
                            for output in &def.declared_outputs {
                                declared.insert(output.name.clone());
                            }
                        }
                    }
                    // P2.4 W2 (build-brief.md §3 W2; refuse.md §2): a Work
                    // whose *execution* repository binding is
                    // `Access::Read` refuses any changed path at all,
                    // whatever the Waypoint's globs say. P3 W3 (ruling
                    // 0090): resolved by `Work.execution_repo` — the same
                    // name `resolve_execution_repo` fixed at submit time
                    // for every Waypoint kind and every submit shape
                    // (`handle_submit`'s own doc) — never
                    // `repositories.first()` and never `ActorWorld.
                    // repository` (which is a bare path, not a binding
                    // name, for the immediate `--kind actor --repo-path`
                    // submit shape, so matching against it would silently
                    // miss and fall back to `.first()` for exactly the
                    // shape real actor Runs use). A multi-source Work
                    // whose execution repository happens to be listed
                    // second must refuse identically to one where it is
                    // listed first, and a readable evidence source's own
                    // access must never stand in for it. Falls back to
                    // `.first()` only for a pre-correction journal or a
                    // Work with no resolvable name, preserving the legacy
                    // single-binding read.
                    let is_read_binding = work
                        .execution_repo
                        .as_deref()
                        .and_then(|name| {
                            work.repositories
                                .iter()
                                .find(|candidate| candidate.name == name)
                        })
                        .or_else(|| work.repositories.first())
                        .is_some_and(|binding| binding.access == Access::Read);
                    // 0050 D150: "A Read repository binding refuses any
                    // change at all" — no exception for a declared
                    // artifact. The declared-artifact exclusion is a
                    // Write-binding boundary concept (a Waypoint's own
                    // output does not count against its own boundary); it
                    // must never let a Read binding's absolute rule get
                    // bypassed just because the changed path happens to be
                    // named as this Claim's (or an earlier Waypoint's)
                    // declared output (`read-artifact-probe/ASSESSMENT.md`,
                    // an executed defect: a modified or brand-new declared
                    // artifact both wrongly Validated under Read).
                    let offending: Vec<&String> = changed
                        .iter()
                        .filter(|p| is_read_binding || !declared.contains(p.as_str()))
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
    }

    // W-A correction (F3): capture the content identity of the exact
    // artifacts that validated, bound to this Claim/Run and to the
    // canonical path inside the Run's own checkout. Recorded on
    // `ClaimRecorded`, which is what every later receipt and every
    // historical inspection reads — so a rewrite of that path after
    // validation is *detectable* rather than silently re-attributed.
    // A file that validated a moment ago but cannot be read now is an
    // explicit `ValidationUnavailable`, never a receipt with no digest.
    let mut artifact_receipts: Vec<ArtifactReceipt> = Vec::new();
    if matches!(verdict, ClaimVerdict::Validated)
        && let Ok(binding) = &binding
        && binding.materialized
    {
        let worktree_path = match &binding.world {
            World::Actor(actor) => actor.worktree_path.clone(),
            World::Deterministic(deterministic) => deterministic.cwd.clone(),
        };
        for artifact in &claim.artifacts {
            let resolved = worktree_path.join(&artifact.path);
            let Some(digest) = ArtifactReceipt::digest_of(&resolved) else {
                verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(format!(
                    "the claimed artifact {} could not be read to record its content identity",
                    artifact.name
                )));
                artifact_receipts.clear();
                break;
            };
            artifact_receipts.push(ArtifactReceipt {
                name: artifact.name.clone(),
                path: artifact_relative_to_worktree(&worktree_path, &artifact.path)
                    .map(|relative| relative.to_string_lossy().into_owned())
                    .unwrap_or_else(|| artifact.path.clone()),
                digest,
            });
        }
    }

    let claim_kind = payload.kind.clone();
    let reply = record_and_reply(
        state,
        &mut journal,
        &work_id,
        &run_id,
        ClaimOutcome {
            claim_id,
            claim_kind: payload.kind,
            verdict: verdict.clone(),
            artifacts: artifact_receipts,
        },
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

        // W-A (§3.2): before advancing to the next leaf (or letting
        // `fold`'s own last-waypoint rule complete the Work), close
        // every container this claim's leaf is the last direct child
        // of, innermost first, cascading outward through
        // `close_cascade`. A Held container stops the whole advance —
        // the Work becomes `Waiting`, nothing further is reserved.
        if let Some(container_id) = innermost_closing_container(&journaled_defs, &run.waypoint) {
            match close_cascade(state, &work_id, &mut journal, &journaled_defs, container_id) {
                Ok(CascadeOutcome::Held) => return reply,
                Ok(CascadeOutcome::Closed(_)) => {}
                Err(err) => return err_reply("JournalError", &err.to_string()),
            }
        }

        // W-A (§3.3): if this Work is itself a child and the cascade
        // above just completed it, re-evaluate the parent's own held
        // container now that a fresh, current-Run-bound receipt is
        // available — never inferred from `WorkState` alone
        // (`reevaluate_parent`/`evaluate_closure` re-derive it from the
        // parent's own journal facts). A completed Work has no next
        // leaf to reserve (`is_last` is necessarily true whenever this
        // fires), so this returns directly — critically, only *after*
        // dropping this Work's own journal lock: `reevaluate_parent`'s
        // own cascade may need to read this same Work's journal again
        // (as the child being credited), and `Mutex` is not reentrant.
        let events_now = match journal.replay() {
            Ok(events) => events,
            Err(err) => return err_reply("JournalError", &err.to_string()),
        };
        let this_work = fold(&events_now);
        if matches!(this_work.state, WorkState::Completed)
            && let Some(parent) = this_work.parent.clone()
        {
            drop(journal);
            if let Err(err) = reevaluate_parent(state, &parent) {
                return err_reply("JournalError", &err.to_string());
            }
            return reply;
        }

        if !is_last
            && let Err((code, message)) = reserve_next_leaf(
                state,
                &work_id,
                &mut journal,
                &journaled_defs,
                &run.waypoint,
            )
        {
            return err_reply(code, &message);
        }
    }

    reply
}

/// Reserves the next leaf after `after_leaf` in this Work's own
/// flattened Route order — the World, its hash, and the fresh `RunOpened`
/// — journaling the `ContainerActivated` identity for every container
/// the reservation newly enters first.
///
/// Extracted from `handle_claim`'s own auto-advance (W-A correction):
/// a container can now also close *without* a Claim on this journal at
/// all (a required child Work completing elsewhere, re-evaluated by
/// `reevaluate_parent`), and a Route that continues past that container
/// has to advance then too. Before the extraction the Work was left
/// Active with its closed container as `current_waypoint` and no open
/// Run at all — nothing to claim, nothing to retry.
fn reserve_next_leaf(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    journal: &mut Journal,
    journaled_defs: &[WaypointDefinition],
    after_leaf: &WaypointId,
) -> Result<(), (&'static str, String)> {
    let events = journal
        .replay()
        .map_err(|err| ("JournalError", err.to_string()))?;
    let waypoints = route_waypoints(&events);
    let prior_binding = match latest_run_for_waypoint(&events, after_leaf) {
        Some((run_id, ..)) => resolve_run_binding(&events, &state.estate_root, work_id, &run_id),
        None => Err("the leaf just closed has no Run".to_string()),
    };
    if let Some(pos) = waypoints.iter().position(|w| w == after_leaf)
        && let Some(next_id) = waypoints.get(pos + 1)
        && let Some(next_def) = find_definition(journaled_defs, next_id)
    {
        let prior_world = prior_binding
            .as_ref()
            .ok()
            .map(|binding| binding.world.clone());
        let cwd = prior_world
            .as_ref()
            .map(|world| match world {
                World::Actor(actor) => actor.worktree_path.clone(),
                World::Deterministic(deterministic) => deterministic.cwd.clone(),
            })
            .unwrap_or_else(|| state.estate_root.clone());
        // W4 (P2.6 run 3, rerun3's `verify`-vs-`build` finding): the
        // *next* Waypoint's boundary check (above, this function) diffs
        // the worktree against whatever `base_sha` its own reserved
        // World carries — carrying the *prior* Waypoint's `base_sha`
        // forward unchanged (the pre-W4 behaviour) means every
        // already-Claimed change the prior Waypoint itself just made
        // reads as out-of-boundary for the one after it. The next
        // Waypoint's Run has not started yet, so "the worktree as it
        // stood when its Run started" is the branch tip right now, in
        // this same worktree — read fresh with git (`resolve_git_sha`,
        // R2, the same call `handle_submit` already makes to pin a
        // Work's original base) rather than carried from the prior
        // World's own field. A worktree git cannot read from (the
        // no-repo-path test harness's own bare-estate `cwd`) falls back
        // to the prior World's `base_sha` unchanged — today's behaviour,
        // never a hard failure of auto-advance itself. The Work's
        // *original* base stays exactly where `handle_submit` already
        // put it, on the first Waypoint's own reserved World — nothing
        // here touches that.
        let prior_base_sha = || {
            prior_world
                .as_ref()
                .map(|world| match world {
                    World::Actor(actor) => actor.base_sha.clone(),
                    World::Deterministic(deterministic) => deterministic.base_sha.clone(),
                })
                .unwrap_or_default()
        };
        let prior_basis = prior_world
            .as_ref()
            .map(|world| world.source_basis().clone())
            .unwrap_or_default();
        let base_sha = match &prior_basis {
            SourceBasis::Git { .. } => match resolve_git_sha(&cwd.display().to_string(), "HEAD") {
                Ok(base) => base,
                Err(detail) => return Err(("ValidationUnavailable", detail)),
            },
            SourceBasis::OutputOnly { reference } => reference.clone(),
            SourceBasis::Unknown => prior_base_sha(),
        };
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
                base_sha: base_sha.clone(),
                source_basis: match &prior_basis {
                    SourceBasis::Git { .. } => SourceBasis::Git {
                        base: base_sha.clone(),
                    },
                    SourceBasis::OutputOnly { reference } => SourceBasis::OutputOnly {
                        reference: reference.clone(),
                    },
                    SourceBasis::Unknown => SourceBasis::Unknown,
                },
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
                if !matches!(prior_basis, SourceBasis::Git { .. }) {
                    return Err((
                        "IncompatibleSourceBasis",
                        "an Actor stage cannot inherit an output-only or unknown source basis"
                            .to_string(),
                    ));
                }
                let (repository, branch) = match &prior_world {
                    Some(World::Actor(actor)) => (actor.repository.clone(), actor.branch.clone()),
                    // A deterministic Git World carries its verified
                    // checkout in `cwd`. The logical repository binding
                    // name is not a path and therefore cannot support the
                    // Actor stage's later Git validation or retry.
                    Some(World::Deterministic(deterministic)) => (
                        deterministic.cwd.display().to_string(),
                        format!("wirk/{}", work_id.0),
                    ),
                    None => (String::new(), format!("wirk/{}", work_id.0)),
                };
                Some(World::Actor(ActorWorld {
                    repository,
                    worktree_path: cwd,
                    branch,
                    source_basis: SourceBasis::Git {
                        base: base_sha.clone(),
                    },
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
            // `waypoints` (`route_waypoints`) names only executable
            // leaves (`flatten_leaves`, §3.1) — `next_id` can never
            // resolve to a `Container` definition.
            WaypointKind::Container => {
                unreachable!("the flattened waypoint sequence names only executable leaves")
            }
        };
        if let Some(next_world) = next_world {
            // W-A (§3.1): explicit journaled identity for every
            // container this reservation newly enters, outermost
            // first, before the reservation itself.
            // W-A correction (F1/F2): entering a container is a new
            // *generation* of it — re-entering one that already
            // closed (after a reopen upstream re-ran the Route
            // through it) mints the next attempt, which is what
            // invalidates its earlier `StageClosed` for every
            // ancestor that would otherwise have credited it.
            let events_for_attempt = journal
                .replay()
                .map_err(|err| ("JournalError", err.to_string()))?;
            for waypoint in entering_ancestors(journaled_defs, next_id) {
                let attempt = next_container_attempt(&events_for_attempt, &waypoint);
                let activated = new_event(
                    work_id,
                    None,
                    EventKind::ContainerActivated { waypoint, attempt },
                );
                append_event(state, journal, work_id, &activated)
                    .map_err(|err| ("JournalError", err.to_string()))?;
            }
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
                work_id,
                None,
                EventKind::WaypointReserved {
                    waypoint: next_id.clone(),
                    world_hash: world_hash.clone(),
                    world: next_world,
                },
            );
            let opened = new_event(
                work_id,
                Some(next_run_id.clone()),
                EventKind::RunOpened {
                    run: next_run_id,
                    waypoint: next_id.clone(),
                    attempt: 1,
                    world_hash,
                },
            );
            for event in [reserved, opened] {
                append_event(state, journal, work_id, &event)
                    .map_err(|err| ("JournalError", err.to_string()))?;
            }
        }
    }
    Ok(())
}

/// One Claim's recorded outcome: its minted id, its verb, the verdict
/// it drew, and (W-A correction, F3) the content identity of the
/// artifacts it was validated against. One struct rather than four
/// positional arguments — `record_and_reply` is the only caller shape,
/// and the four always travel together.
struct ClaimOutcome {
    claim_id: ClaimId,
    claim_kind: ClaimKind,
    verdict: ClaimVerdict,
    artifacts: Vec<ArtifactReceipt>,
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
    outcome: ClaimOutcome,
) -> Reply {
    let ClaimOutcome {
        claim_id,
        claim_kind,
        verdict,
        artifacts,
    } = outcome;
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
            artifacts,
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
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
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
    let waypoint_defs = waypoint_defs_for(&events);

    let runs: Vec<Value> = all_run_ids(&events)
        .into_iter()
        .filter_map(|run_id| {
            let run = find_run(&events, &run_id)?;
            let binding = resolve_run_binding(&events, &state.estate_root, &work.id, &run_id);
            let (world, world_binding) = binding_status(binding);
            // P3 native launch selection (BUILD-BRIEF.md item 1): the
            // Route-authored default for this Run's own Waypoint —
            // `wirk run`'s own precedence layer (CLI-explicit > this >
            // the harness's native default), read from the same
            // `WorkSubmitted.waypoint_defs` every other reader of a
            // Waypoint's authored shape already uses (R2), never
            // re-read from a Route file that may have changed since
            // submit.
            let selection: Option<AuthoredSelection> =
                find_definition(&waypoint_defs, &run.waypoint)
                    .and_then(|def| def.selection.clone());
            Some(json!({
                "run": serde_json::to_value(&run).ok()?,
                "world": world,
                "world_binding": world_binding,
                "selection": selection,
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
    // W-A (§3.2, §3.3): additive, same convention — absent when the
    // Work was never held or is not a child, so an old caller reading
    // only the original fields is unaffected.
    if let Some(held) = &work.held {
        result["held"] = json!({
            "waypoint": held.waypoint.0,
            "attempt": held.attempt,
            "missing": held.missing,
        });
    }
    if let Some(parent) = &work.parent {
        result["parent"] = json!({
            "work": parent.work.0,
            "waypoint": parent.waypoint.0,
            "attempt": parent.attempt_or_first(),
            "run": parent.run.0,
            "role": parent.role,
        });
    }
    // W-A correction (F1/F2): every container's current activation, so
    // one generation of a nested stage is distinguishable from the next
    // over the public verb, not only in the raw journal.
    if !work.activations.is_empty() {
        result["activations"] = Value::Array(
            work.activations
                .iter()
                .map(|entry| json!({"waypoint": entry.waypoint.0, "attempt": entry.attempt}))
                .collect(),
        );
    }
    // W-A correction (F3): the artifact evidence this Work's validated
    // Claims actually rest on, each answered against the content
    // identity recorded at validation — `available: false` with an
    // explicit reason when the bytes changed or the file is gone,
    // rather than a path that silently reads as whatever is there now.
    result["evidence"] = Value::Array(claim_evidence(&events));
    if let Some(cause) = &work.needs_input {
        result["needs_input"] = json!({
            "run": cause.run.0,
            "reason": cause.reason,
            "detail": cause.detail,
        });
    }

    // W-A (§3.2): a held container itself has no Run — `current_waypoint`
    // names the container, not a leaf, so `latest_run_for_waypoint`
    // alone finds nothing. Fall back to the most recently opened Run
    // among the container's own descendant leaves: the run `wirk work
    // retry`'s CLI (and a human reading `status`) means by "the current
    // run to retry" for a held container.
    let effective_run = work.current_waypoint.as_ref().and_then(|waypoint| {
        latest_run_for_waypoint(&events, waypoint).or_else(|| {
            let defs = waypoint_defs_for(&events);
            let container = find_definition(&defs, waypoint)?;
            let leaves: HashSet<WaypointId> = flatten_leaves(std::slice::from_ref(container))
                .into_iter()
                .collect();
            events.iter().rev().find_map(|event| match &event.kind {
                EventKind::RunOpened {
                    run,
                    waypoint: wp,
                    attempt,
                    world_hash,
                } if leaves.contains(wp) => Some((run.clone(), *attempt, world_hash.clone())),
                _ => None,
            })
        })
    });
    if let Some((run_id, attempt, world_hash)) = effective_run {
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
        let binding = resolve_run_binding(&events, &state.estate_root, &work.id, &run_id);
        let (world, world_binding) = binding_status(binding);
        result["world"] = world;
        result["world_binding"] = world_binding;
    }

    ok_reply(result)
}

/// W-A correction (F3): one entry per validated Claim that recorded
/// artifact receipts, each artifact re-checked against its recorded
/// digest right now. `available` is the whole point: an artifact whose
/// bytes changed since validation, or that is gone, is reported
/// explicitly unavailable with the reason — never silently credited,
/// and never re-hashed into a new "current" digest that would erase
/// what actually validated. Historical entries stay inspectable
/// (BUILD-AMENDMENTS.md: "Snapshot needed bytes or resolve against the
/// recorded digest and return explicit unavailable after change").
fn claim_evidence(events: &[Event]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| {
            let EventKind::ClaimRecorded {
                claim,
                verdict: ClaimVerdict::Validated,
                artifacts,
                ..
            } = &event.kind
            else {
                return None;
            };
            if artifacts.is_empty() {
                return None;
            }
            let run_id = event.run.clone();
            let waypoint = run_id
                .as_ref()
                .and_then(|run| find_run(events, run))
                .map(|run| run.waypoint.0);
            let worktree = run_id
                .as_ref()
                .and_then(|run| worktree_path_for_run(events, run));
            let entries: Vec<Value> = artifacts
                .iter()
                .map(|artifact| {
                    let (available, reason) = match &worktree {
                        // A pre-correction receipt recorded a name and
                        // nothing else: inspectable, but never
                        // reportable as evidence that still holds.
                        _ if artifact.digest.is_empty() => (false, Some("unrecorded")),
                        None => (false, Some("unresolved")),
                        Some(worktree) => {
                            let path = worktree.join(&artifact.path);
                            match ArtifactReceipt::digest_of(&path) {
                                None => (false, Some("absent")),
                                Some(now) if now == artifact.digest => (true, None),
                                Some(_) => (false, Some("changed")),
                            }
                        }
                    };
                    json!({
                        "name": artifact.name,
                        "path": artifact.path,
                        "digest": artifact.digest,
                        "available": available,
                        "reason": reason,
                    })
                })
                .collect();
            Some(json!({
                "claim": claim.0,
                "run": run_id.map(|run| run.0),
                "waypoint": waypoint,
                "artifacts": entries,
            }))
        })
        .collect()
}

fn binding_status(binding: Result<RunBinding, String>) -> (Value, Value) {
    match binding {
        Ok(binding) => (
            serde_json::to_value(&binding.world).expect("World always serializes"),
            json!({
                "state": "resolved",
                "inspection": binding.inspection_name(),
                "materialized": binding.materialized,
                "legacy_basis": binding.legacy_basis,
            }),
        ),
        Err(reason) => (
            Value::Null,
            json!({"state": "unavailable", "reason": reason}),
        ),
    }
}

/// `run-deterministic`'s own verb (module doc, `FailPayload`): journals
/// a `RunFailed { cause }` for the triple's `run_id`, refusing
/// (`TripleMismatch`) the same way `claim` does (D9#4) when no
/// `RunOpened` in this Work's journal names it — this call never
/// invents a Run, only records a fact about one that already exists.
fn handle_fail(state: &Arc<WirkdState>, payload: FailPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();

    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the failure's estate root does not identify this daemon's estate",
        );
    }

    let journal = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    let events = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let Some(run) = find_run(&events, &run_id) else {
        return err_reply(
            "TripleMismatch",
            "the run id does not match any Run opened for this Work",
        );
    };
    if !matches!(run.state, RunState::Open)
        || latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0)
            != Some(run_id.clone())
        || fold(&events).state.is_terminal()
    {
        return err_reply(
            "InvalidTransition",
            "failure does not target the current open Run",
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
/// (D9#4, same check `claim`/`fail` make).
///
/// P2.6 W3 (rerun findings, `knowledge/evidence/p2-build-wave-2026-09-05/
/// rerun/`; ruling 0052): this used to reuse the Waypoint's
/// already-reserved World verbatim, so an `ActorWorld.triple.run_id`
/// still named whichever Run *first* reserved it — a retried Run's
/// pane launch then collided `agent_name_taken` on that stale Run's
/// still-alive pane (`03-orient.log` lines 21-23), the exact defect
/// class Wave 1 fixed for auto-advance's `next_world` (`handle_claim`)
/// on this sibling code path Wave 1 never touched. Fixed the same way:
/// a fresh World is minted before the new `RunOpened`, carrying the new
/// Run's own triple.
///
/// W4 (P2.6 run 3): the "`Deterministic` has no triple, so nothing is
/// re-reserved for it" half of that fix was only half right — a
/// `Deterministic` World's own `base_sha` can go just as stale as an
/// `Actor` World's `triple.run_id` did (`03-orient.log`'s
/// `build-wave/verify` finding: a retry that reuses the prior World
/// verbatim reuses its stale `base_sha` too, so an `OutOfBoundary`
/// refusal caused by that staleness recurs identically on every retry,
/// with no path to self-correct). Both arms now mint a fresh World —
/// `Actor` a fresh triple *and* `base_sha`, `Deterministic` a fresh
/// `base_sha` alone — read from the worktree with git at this retry's
/// own start, the same call auto-advance's `next_world` makes
/// (`resolve_git_sha`, R2); a worktree git cannot read from falls back
/// to the prior World's own `base_sha`, never a hard failure. The old
/// Run is also marked `RunFailed{status: "retried"}` here: a Claim refused
/// leaves a Run `Open` (D9#3, `Run::apply`), so without this a Work
/// that had two Runs — the refused one and the retry — would still
/// read as having two `Open` Runs, and a caller that picks "the" open
/// Run to drive (`wirk/src/executor.rs::fetch_open_run`) could still
/// find the abandoned one first and collide on its pane name exactly as
/// the rerun did live. No count is invented: the human's one retry verb
/// is the one `RunOpened` (D134 bars a count, not the event itself).
/// `fold`'s own `RunOpened` arm clears `NeedsInput` back to `Active` on
/// replay — this handler never sets `Work.state` itself, the journal
/// is the only truth (D9#1).
fn handle_retry(state: &Arc<WirkdState>, payload: RetryPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();

    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the retry's estate root does not identify this daemon's estate",
        );
    }

    let journal = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
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
    let waypoint_defs = waypoint_defs_for(&events);
    let Some(run) = find_run(&events, &run_id) else {
        return err_reply(
            "TripleMismatch",
            "the run id does not match any Run opened for this Work",
        );
    };
    match work.state {
        WorkState::NeedsInput => {
            if work.needs_input.as_ref().map(|cause| &cause.run) != Some(&run_id) {
                return err_reply(
                    "TripleMismatch",
                    "retry must name the Run that placed this Work in NeedsInput",
                );
            }
            if work.current_waypoint.as_ref() != Some(&run.waypoint) {
                return err_reply(
                    "TripleMismatch",
                    "retry must name the current unsuperseded Run",
                );
            }
        }
        // W-A (§3.2 amendment, BUILD-AMENDMENTS.md): a held container's
        // own missing/invalid leaf output needs a usable correction
        // path even though the Work is `Waiting`, not `NeedsInput` — a
        // retry naming the *current* Run of any leaf nested under the
        // held container is accepted; the leaf need not itself have
        // failed (its own Claim may have validated cleanly while the
        // *container's* own requirement — a required artifact name, or
        // a since-superseded child receipt — went unmet).
        WorkState::Waiting => {
            let Some(held) = &work.held else {
                return err_reply("NotNeedsInput", "retry refused: the Work is not held");
            };
            if !ancestor_chain(&waypoint_defs, &run.waypoint).contains(&held.waypoint) {
                return err_reply(
                    "TripleMismatch",
                    "retry must name a leaf nested under the held container",
                );
            }
        }
        _ => {
            return err_reply(
                "NotNeedsInput",
                "retry refused: the Work is neither NeedsInput nor Waiting",
            );
        }
    }
    if latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0) != Some(run_id.clone())
    {
        return err_reply(
            "TripleMismatch",
            "retry must name the current unsuperseded Run",
        );
    }

    let binding = match resolve_run_binding(&events, &state.estate_root, &work_id, &run_id) {
        Ok(binding) => binding,
        Err(reason) => return err_reply("ValidationUnavailable", &reason),
    };
    let prior_world = binding.world;

    let new_run_id = RunId(mint_id("run"));

    let fresh_world = match &prior_world {
        World::Actor(actor) => {
            let fresh_base_sha = if binding.materialized {
                match resolve_git_sha(&actor.worktree_path.display().to_string(), "HEAD") {
                    Ok(base) => base,
                    Err(detail) => return err_reply("ValidationUnavailable", &detail),
                }
            } else {
                actor.base_sha.clone()
            };
            World::Actor(ActorWorld {
                worktree_path: if binding.materialized {
                    actor.worktree_path.clone()
                } else {
                    PathBuf::new()
                },
                base_sha: fresh_base_sha.clone(),
                source_basis: SourceBasis::Git {
                    base: fresh_base_sha,
                },
                triple: ExecutionTriple {
                    estate_root: state.estate_root.display().to_string(),
                    work_id: work_id.clone(),
                    run_id: new_run_id.clone(),
                },
                ..actor.clone()
            })
        }
        World::Deterministic(deterministic) => {
            let (fresh_base_sha, source_basis) = match &deterministic.source_basis {
                SourceBasis::Git { .. } => {
                    let base =
                        match resolve_git_sha(&deterministic.cwd.display().to_string(), "HEAD") {
                            Ok(base) => base,
                            Err(detail) => return err_reply("ValidationUnavailable", &detail),
                        };
                    (base.clone(), SourceBasis::Git { base })
                }
                SourceBasis::OutputOnly { reference } => (
                    reference.clone(),
                    SourceBasis::OutputOnly {
                        reference: reference.clone(),
                    },
                ),
                SourceBasis::Unknown => {
                    return err_reply("ValidationUnavailable", "source basis is unknown");
                }
            };
            World::Deterministic(DeterministicWorld {
                base_sha: fresh_base_sha,
                source_basis,
                ..deterministic.clone()
            })
        }
    };
    // W-A correction (F1/F2): retrying a leaf *reopens* every ancestor
    // container whose own current activation had already closed —
    // outermost first, each with a fresh monotonic attempt, journaled
    // before the reservation the retry itself makes. This is the usable
    // reopen path the amendments require ("Reopening invalidates
    // affected aggregate closure and cannot reuse superseded attempt
    // evidence"): a closed generation's `StageClosed` no longer answers
    // for the new one, so every ancestor must see the corrected stage
    // actually re-executed before it can close again. An ancestor that
    // is merely *held* is left on its own activation — its closure
    // never happened, so there is nothing to invalidate, and its own
    // already-valid child receipts stay valid.
    let mut reopening = ancestor_chain(&waypoint_defs, &run.waypoint);
    reopening.reverse();
    for ancestor in reopening {
        let attempt = container_attempt(&events, &ancestor);
        if !matches!(
            stage_outcome_at(&events, &ancestor, attempt),
            Some(StageOutcomeRef::Closed(_))
        ) {
            continue;
        }
        let activated = new_event(
            &work_id,
            None,
            EventKind::ContainerActivated {
                waypoint: ancestor.clone(),
                attempt: next_container_attempt(&events, &ancestor),
            },
        );
        if let Err(err) = append_event(state, &mut journal, &work_id, &activated) {
            return err_reply("JournalError", &err.to_string());
        }
    }

    let world_hash = WorldHash::of(&fresh_world);
    let reserved = new_event(
        &work_id,
        None,
        EventKind::WaypointReserved {
            waypoint: run.waypoint.clone(),
            world_hash: world_hash.clone(),
            world: fresh_world,
        },
    );
    if let Err(err) = append_event(state, &mut journal, &work_id, &reserved) {
        return err_reply("JournalError", &err.to_string());
    }

    let superseded = new_event(
        &work_id,
        Some(run_id.clone()),
        EventKind::RunFailed {
            cause: FailureCause {
                status: Some("retried".to_string()),
                request_id: None,
                at: now_ts(),
                detail: Some(format!("superseded by retry {}", new_run_id.0)),
            },
        },
    );
    if let Err(err) = append_event(state, &mut journal, &work_id, &superseded) {
        return err_reply("JournalError", &err.to_string());
    }

    let event = new_event(
        &work_id,
        Some(new_run_id.clone()),
        EventKind::RunOpened {
            run: new_run_id.clone(),
            waypoint: run.waypoint.clone(),
            attempt: run.attempt + 1,
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
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
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

#[derive(Debug, Clone)]
struct RunBinding {
    world: World,
    materialized: bool,
    legacy_basis: bool,
}

impl RunBinding {
    fn inspection_name(&self) -> &'static str {
        match self.world.source_basis() {
            SourceBasis::Git { .. } => "git",
            SourceBasis::OutputOnly { .. } => "output_only",
            SourceBasis::Unknown => "unknown",
        }
    }
}

/// Resolves exactly the reservation consumed by one RunOpened and its only
/// legal Actor materialization. Journal order and full structured equality
/// are the authority; matching hashes alone never associate a World to a Run.
fn resolve_run_binding(
    events: &[Event],
    estate_root: &Path,
    work_id: &WorkId,
    run_id: &RunId,
) -> Result<RunBinding, String> {
    let openings: Vec<(usize, &Event, &WaypointId, &WorldHash)> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match &event.kind {
            EventKind::RunOpened {
                run,
                waypoint,
                world_hash,
                ..
            } if run == run_id => Some((index, event, waypoint, world_hash)),
            _ => None,
        })
        .collect();
    let [(open_index, opened, waypoint, opened_hash)] = openings.as_slice() else {
        return Err(if openings.is_empty() {
            "no RunOpened names this Run".to_string()
        } else {
            "duplicate RunOpened events name this Run".to_string()
        });
    };
    if opened.work != *work_id || opened.run.as_ref() != Some(run_id) {
        return Err("RunOpened carries mismatched Work or Event.run identity".to_string());
    }

    let reservation_index = if *open_index >= 1
        && matches!(
            events[*open_index - 1].kind,
            EventKind::WaypointReserved { .. }
        ) {
        *open_index - 1
    } else if *open_index >= 2
        && matches!(
            events[*open_index - 2].kind,
            EventKind::WaypointReserved { .. }
        )
        && matches!(
            &events[*open_index - 1].kind,
            EventKind::RunFailed { cause } if cause.status.as_deref() == Some("retried")
        )
        && events[*open_index - 1].run.as_ref() != Some(run_id)
    {
        *open_index - 2
    } else {
        return Err("RunOpened is not adjacent to a legal opening reservation".to_string());
    };
    let reservation = &events[reservation_index];
    let EventKind::WaypointReserved {
        waypoint: reserved_waypoint,
        world_hash: reserved_hash,
        world,
    } = &reservation.kind
    else {
        unreachable!()
    };
    if reservation.work != *work_id
        || reservation.run.is_some()
        || reserved_waypoint != *waypoint
        || reserved_hash != *opened_hash
        || WorldHash::of(world) != *reserved_hash
    {
        return Err("opening reservation does not match RunOpened".to_string());
    }

    let mut resolved = world.clone();
    let mut legacy_basis = false;
    match &mut resolved {
        World::Actor(actor) => {
            if actor.triple.work_id != *work_id
                || actor.triple.run_id != *run_id
                || !estate_roots_equal(estate_root, &actor.triple.estate_root)
            {
                return Err(
                    "Actor World triple does not name this estate, Work, and Run".to_string(),
                );
            }
            match &actor.source_basis {
                SourceBasis::Git { base } if base == &actor.base_sha => {}
                SourceBasis::Unknown => {
                    // The canonical Actor reservation/open writer resolves the
                    // submitted revision before writing and carries the exact
                    // triple. Preserve that old writer sequence as Git without
                    // rewriting its journal or hash.
                    actor.source_basis = SourceBasis::Git {
                        base: actor.base_sha.clone(),
                    };
                    legacy_basis = true;
                }
                _ => return Err("Actor World has an incompatible source basis".to_string()),
            }
        }
        World::Deterministic(det) => match &det.source_basis {
            SourceBasis::Git { base } if base == &det.base_sha => {}
            SourceBasis::OutputOnly { reference } if reference == &det.base_sha => {}
            SourceBasis::Unknown => {
                return Err("legacy Deterministic World has no provable source basis".to_string());
            }
            _ => return Err("Deterministic World source basis disagrees with base_sha".to_string()),
        },
    }

    let World::Actor(initial_actor) = &resolved else {
        return Ok(RunBinding {
            world: resolved,
            materialized: true,
            legacy_basis,
        });
    };
    if !initial_actor.worktree_path.as_os_str().is_empty() {
        return Ok(RunBinding {
            world: resolved,
            materialized: true,
            legacy_basis,
        });
    }

    let expected_path = estate_root.join("worktrees").join(&work_id.0);
    let mut materialized: Option<World> = None;
    let launch_index = events.iter().enumerate().find_map(|(index, event)| {
        matches!(
            &event.kind,
            EventKind::RunLaunched { run, .. }
                if event.run.as_ref() == Some(run_id) && run == run_id
        )
        .then_some(index)
    });
    for index in (*open_index + 1)..events.len().saturating_sub(1) {
        if launch_index.is_some_and(|launch| index >= launch) {
            break;
        }
        let created = &events[index];
        let updated = &events[index + 1];
        let EventKind::WorktreeCreated { repo, base_sha } = &created.kind else {
            continue;
        };
        let EventKind::WaypointReserved {
            waypoint: updated_waypoint,
            world_hash: updated_hash,
            world: updated_world,
        } = &updated.kind
        else {
            continue;
        };
        if created.work != *work_id
            || created.run.as_ref() != Some(run_id)
            || updated.work != *work_id
            || !matches!(updated.run.as_ref(), Some(run) if run == run_id) && updated.run.is_some()
            || updated_waypoint != *waypoint
            || updated_hash != *opened_hash
            || WorldHash::of(updated_world) != *updated_hash
        {
            continue;
        }
        let World::Actor(updated_actor) = updated_world else {
            continue;
        };
        let mut expected_actor = initial_actor.clone();
        expected_actor.worktree_path = updated_actor.worktree_path.clone();
        // Legacy materialization stored Unknown even though the validated
        // Actor opening sequence proves Git. Compare its historical value,
        // then expose the effective proven tag in the resolved World.
        if legacy_basis {
            expected_actor.source_basis = SourceBasis::Unknown;
        }
        if repo != &initial_actor.repository
            || base_sha != &initial_actor.base_sha
            || expected_actor != *updated_actor
            || !paths_equal(&expected_path, &updated_actor.worktree_path)
        {
            continue;
        }
        if materialized.is_some() {
            return Err("multiple Actor materializations fit this Run".to_string());
        }
        let mut effective = updated_actor.clone();
        if legacy_basis {
            effective.source_basis = SourceBasis::Git {
                base: effective.base_sha.clone(),
            };
        }
        materialized = Some(World::Actor(effective));
    }
    let is_materialized = materialized.is_some();
    Ok(RunBinding {
        world: materialized.unwrap_or(resolved),
        materialized: is_materialized,
        legacy_basis,
    })
}

fn paths_equal(expected: &Path, actual: &Path) -> bool {
    match (
        std::fs::canonicalize(expected),
        std::fs::canonicalize(actual),
    ) {
        (Ok(expected), Ok(actual)) => expected == actual,
        _ => expected == actual,
    }
}

fn estate_roots_equal(expected: &Path, actual: &str) -> bool {
    std::fs::canonicalize(actual).is_ok_and(|actual| actual == expected)
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

/// Returns the only valid directory for a Work id.  External verbs must not
/// let a path-like id escape the estate's `works/` namespace.
fn work_journal_dir(state: &Arc<WirkdState>, work_id: &WorkId) -> Option<PathBuf> {
    let mut components = Path::new(&work_id.0).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => {
            Some(state.estate_root.join("works").join(&work_id.0))
        }
        _ => None,
    }
}

/// Fetches an existing submitted Work's journal.  This is intentionally not a
/// creation path: status, watch, claim, record, failure and retry may observe
/// a Work, but only submit gives one a journal (0067).
fn journal_for(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
) -> Result<Option<Arc<Mutex<Journal>>>, JournalError> {
    let mut journals = state
        .journals
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(existing) = journals.get(work_id) {
        return Ok(Some(Arc::clone(existing)));
    }
    let Some(dir) = work_journal_dir(state, work_id) else {
        return Ok(None);
    };
    if !dir.join("journal.ndjson").is_file() {
        return Ok(None);
    }
    let journal = Journal::open(dir)?;
    let journal = Arc::new(Mutex::new(journal));
    journals.insert(work_id.clone(), Arc::clone(&journal));
    Ok(Some(journal))
}

/// The submit-only journal creation path.  The id is daemon-minted, but it
/// still goes through the same containment guard so future callers cannot
/// accidentally bypass it.
fn create_journal_for(
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
    let dir = work_journal_dir(state, work_id).expect("daemon-minted WorkId is a path component");
    let journal = Arc::new(Mutex::new(Journal::open(dir)?));
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
                // P3 native launch selection: same seed-then-fold as
                // `kind` above — `RunLaunched` moves these via `Run::apply`.
                selection: wirk_core::ActorSelection::default(),
                launched: false,
                launch_requested: false,
                launch_attempt: None,
                launch_argv: Vec::new(),
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
        ClaimRefusal::ValidationUnavailable(detail) => ("ValidationUnavailable", detail.clone()),
        ClaimRefusal::AlreadyClaimed => {
            ("AlreadyClaimed", "the Run is already Claimed".to_string())
        }
    };
    err_reply(code, &message)
}

// ---- W-A: nested-stage closure evaluation (§3.2-3.3) -------------------

/// The container `leaf_id` newly completes (as its last direct child),
/// if any — the entry point `close_cascade` walks outward from.
/// `close_cascade` itself decides whether to continue past it; this
/// only answers the *first* step.
fn innermost_closing_container(
    tree: &[WaypointDefinition],
    leaf_id: &WaypointId,
) -> Option<WaypointId> {
    let chain = ancestor_chain(tree, leaf_id);
    let immediate = chain.first()?;
    let def = find_definition(tree, immediate)?;
    is_last_direct_child(def, leaf_id).then(|| immediate.clone())
}

/// `is_last_direct_child`'s cousin for a container's own outward
/// direction: re-exported locally since `wirk_core::is_last_direct_child`
/// takes the parent definition, not the tree — kept here rather than in
/// `wirk-core` since only `close_cascade`'s own cascade needs it walked
/// this way.
fn is_last_direct_child(container: &WaypointDefinition, id: &WaypointId) -> bool {
    container.leaves.last().map(|d| &d.id) == Some(id)
}

/// What one `close_cascade` walk did (W-A correction): stopped on a
/// hold, or closed up to and including one outermost container.
enum CascadeOutcome {
    Held,
    Closed(WaypointId),
}

/// One container's closure outcome (§3.2): closed with exact receipts,
/// or held with the unmet requirement names.
enum ClosureOutcome {
    Closed(Vec<OutcomeReceipt>),
    Held(Vec<String>),
}

/// The most recently journaled outcome for container `id` — the *last*
/// `StageClosed`/`StageHeld` naming it wins (re-evaluation may append a
/// fresh one after an earlier hold), mirroring `world_for_waypoint`'s
/// own "last one wins" rule.
enum StageOutcomeRef {
    Closed(Vec<OutcomeReceipt>),
    Held(Vec<String>),
}

/// The container activation currently in force for `id` — the last
/// `ContainerActivated` naming it (W-A correction, F1/F2). A container
/// no activation names at all (a pre-correction journal, or an old flat
/// Route with no containers) reads as the first generation, so every
/// comparison below stays total.
fn container_attempt(events: &[Event], id: &WaypointId) -> u32 {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ContainerActivated { waypoint, attempt } if waypoint == id => Some(*attempt),
            _ => None,
        })
        .unwrap_or(1)
}

/// The next activation to mint for `id`: strictly monotonic per
/// container, from the journal alone (W-A correction, F1/F2).
fn next_container_attempt(events: &[Event], id: &WaypointId) -> u32 {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContainerActivated { waypoint, attempt } if waypoint == id => Some(*attempt),
            _ => None,
        })
        .max()
        .map_or(1, |highest| highest + 1)
}

/// The most recently journaled outcome for `id` **within one
/// activation** (W-A correction, F1/F2): a `StageClosed` from a
/// superseded generation is history, never current credit. This is the
/// replacement for the pre-correction "latest `StageClosed` wins"
/// shortcut that let a reopened stage's earlier close still satisfy its
/// parent.
fn stage_outcome_at(events: &[Event], id: &WaypointId, attempt: u32) -> Option<StageOutcomeRef> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::StageClosed {
            waypoint,
            attempt: at,
            receipts,
        } if waypoint == id && *at == attempt => Some(StageOutcomeRef::Closed(receipts.clone())),
        EventKind::StageHeld {
            waypoint,
            attempt: at,
            missing,
        } if waypoint == id && *at == attempt => Some(StageOutcomeRef::Held(missing.clone())),
        _ => None,
    })
}

/// `id`'s outcome for the activation currently in force.
fn current_stage_outcome(events: &[Event], id: &WaypointId) -> Option<StageOutcomeRef> {
    stage_outcome_at(events, id, container_attempt(events, id))
}

/// The artifact receipts recorded for `claim` at validation (W-A
/// correction, F3) — the only source a closure receipt's artifacts come
/// from. Empty for a pre-correction journal, whose closure then holds
/// on its declared outputs rather than inventing content identity it
/// never recorded.
fn claim_artifact_receipts(events: &[Event], claim_id: &ClaimId) -> Vec<ArtifactReceipt> {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ClaimRecorded {
                claim, artifacts, ..
            } if claim == claim_id => Some(artifacts.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Recursively gathers every `Leaf` receipt's artifact names out of
/// `receipts`, descending through nested `Container` receipts — an
/// outer container's own `declared_outputs` are satisfied by a leaf at
/// any depth in its subtree, aggregated through each level's own
/// closure receipt rather than re-derived from raw Claims each time
/// (W-A-BUILD.md: "aggregate recursively, never by last flattened leaf
/// alone").
fn collect_names(receipts: &[OutcomeReceipt], out: &mut BTreeSet<String>) {
    for receipt in receipts {
        match receipt {
            OutcomeReceipt::Leaf { artifacts, .. } => {
                out.extend(artifacts.iter().map(|artifact| artifact.name.clone()));
            }
            OutcomeReceipt::Container { receipts, .. } => collect_names(receipts, out),
            OutcomeReceipt::Child { .. } => {}
        }
    }
}

/// Evaluates whether container `container_id` (found in `tree`) can
/// close, from `events` alone — never from `WorkState`. Each direct
/// child contributes a receipt when it has current, valid evidence: an
/// executable leaf whose *current* (`latest_run_for_waypoint`) Run is
/// `Claimed` (which, since `Run::apply` only reaches `Claimed` via a
/// Validated Done verdict, already proves that leaf's own required
/// declared outputs were present); a nested container whose own latest
/// outcome is `StageClosed` (a stale one superseded by a later
/// `StageHeld` does not count). `declared_outputs{required}` are then
/// checked against the union of every contributing leaf's own required
/// output names, recursively aggregated; `required_child_outcomes` are
/// checked via `valid_child_receipt`, which itself refuses a receipt
/// bound to a superseded Run (a retried leaf cannot reuse an earlier
/// attempt's child).
fn evaluate_closure(
    state: &Arc<WirkdState>,
    events: &[Event],
    tree: &[WaypointDefinition],
    container_id: &WaypointId,
) -> ClosureOutcome {
    let Some(def) = find_definition(tree, container_id) else {
        return ClosureOutcome::Held(vec![format!("container {} not found", container_id.0)]);
    };

    let mut receipts = Vec::new();
    let mut produced: BTreeSet<String> = BTreeSet::new();
    let mut missing = Vec::new();

    for child in &def.leaves {
        match child.kind {
            // W-A correction (F1/F2): a nested container counts only
            // when *its own current activation* closed. A reopened
            // sub-container has no `StageClosed` for its new generation
            // until it is re-executed, so its earlier close can never
            // be borrowed to satisfy this parent.
            WaypointKind::Container => match current_stage_outcome(events, &child.id) {
                Some(StageOutcomeRef::Closed(child_receipts)) => {
                    collect_names(&child_receipts, &mut produced);
                    receipts.push(OutcomeReceipt::Container {
                        waypoint: child.id.clone(),
                        receipts: child_receipts,
                    });
                }
                _ => missing.push(format!("container {} not closed", child.id.0)),
            },
            WaypointKind::Actor | WaypointKind::Deterministic => {
                match latest_run_for_waypoint(events, &child.id)
                    .and_then(|(run_id, ..)| find_run(events, &run_id).map(|run| (run_id, run)))
                {
                    // W-A correction (F3): the receipt's artifacts are
                    // the ones this Claim was *validated against*, with
                    // the content identity recorded then — never the
                    // Route's declared names re-derived against a
                    // mutable path.
                    Some((run_id, run)) => match &run.state {
                        RunState::Claimed(claim_id) => {
                            let artifacts = claim_artifact_receipts(events, claim_id);
                            produced.extend(artifacts.iter().map(|a| a.name.clone()));
                            receipts.push(OutcomeReceipt::Leaf {
                                waypoint: child.id.clone(),
                                run: run_id,
                                claim: claim_id.clone(),
                                artifacts,
                            });
                        }
                        // W-A correction (F1): a container never closes
                        // over a leaf whose current execution is still
                        // open (or failed) — the pre-correction code
                        // simply contributed no receipt, which let a
                        // leaf with no required output of its own pass
                        // silently while its Run stayed open.
                        _ => missing.push(format!(
                            "waypoint {} has no current validated Run",
                            child.id.0
                        )),
                    },
                    None => missing.push(format!("waypoint {} has not run", child.id.0)),
                }
            }
        }
    }

    for spec in &def.declared_outputs {
        if spec.required && !produced.contains(&spec.name) {
            missing.push(spec.name.clone());
        }
    }

    for role in &def.required_child_outcomes {
        if !role.required {
            continue;
        }
        match valid_child_receipt(state, events, container_id, &role.role) {
            Some(receipt) => receipts.push(receipt),
            None => missing.push(format!("child role {}", role.role)),
        }
    }

    if missing.is_empty() {
        ClosureOutcome::Closed(receipts)
    } else {
        ClosureOutcome::Held(missing)
    }
}

/// A valid `Child` receipt for `role` on `container_id`, from `events`
/// alone (§3.3): the *most recent* `ChildWorkSpawned` naming this
/// `(container_id, role)` whose own `run` is still the current,
/// unsuperseded Run of the leaf that requested it (a retried requesting
/// leaf mints a fresh Run, so an earlier spawn's `run` no longer
/// matches `latest_run_for_waypoint` — its receipt is never reused,
/// `retried_parent_leaf_cannot_reuse_earlier_attempts_child_receipt`),
/// and whose named child Work has its own journal
/// (`dangling_spawn_without_child_journal_is_missing_not_credited`
/// otherwise) and is `Completed` (never `Canceled`, `Failed`, or simply
/// open).
fn valid_child_receipt(
    state: &Arc<WirkdState>,
    events: &[Event],
    container_id: &WaypointId,
    role: &str,
) -> Option<OutcomeReceipt> {
    let parent_work_id = events.first().map(|event| event.work.clone())?;
    let current_attempt = container_attempt(events, container_id);
    events.iter().rev().find_map(|event| {
        let EventKind::ChildWorkSpawned {
            role: spawned_role,
            child,
            waypoint,
            attempt,
            run,
        } = &event.kind
        else {
            return None;
        };
        if spawned_role != role || waypoint != container_id {
            return None;
        }
        // W-A correction (F1/F2): the spawn served one generation of
        // this container. A reopened container's earlier child outcome
        // is superseded evidence, not current credit.
        if *attempt != current_attempt {
            return None;
        }
        let requester = find_run(events, run)?;
        if latest_run_for_waypoint(events, &requester.waypoint)
            .map(|entry| entry.0)
            .as_ref()
            != Some(run)
        {
            return None;
        }
        // W-A correction (F4): the binding is checked from both sides —
        // the parent's own spawn record *and* the child's own recorded
        // parent. A Work that never named this parent, waypoint,
        // activation, requesting Run and role cannot be a receipt,
        // however completed it is.
        let expected = ParentBinding {
            work: parent_work_id.clone(),
            waypoint: container_id.clone(),
            attempt: Some(*attempt),
            run: run.clone(),
            role: role.to_string(),
        };
        let (claim, world_hash) = child_work_completed_receipt(state, child, &expected)?;
        Some(OutcomeReceipt::Child {
            role: role.to_string(),
            child: child.clone(),
            parent_run: run.clone(),
            claim,
            world_hash,
        })
    })
}

/// The child Work's own closing `ClaimId`/`WorldHash`, when — and only
/// when — its journal exists and it folds `Completed` (never inferred
/// from a partial replay or a dangling spawn with no journal at all).
fn child_work_completed_receipt(
    state: &Arc<WirkdState>,
    child: &WorkId,
    expected: &ParentBinding,
) -> Option<(ClaimId, WorldHash)> {
    let journal = journal_for(state, child).ok().flatten()?;
    let events = {
        let journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
        journal.replay().ok()?
    };
    if events.is_empty() {
        return None;
    }
    let work = fold(&events);
    if !matches!(work.state, WorkState::Completed) {
        return None;
    }
    // W-A correction (F4): the child's own half of the binding.
    let bound = work.parent.as_ref().is_some_and(|own| {
        own.work == expected.work
            && own.waypoint == expected.waypoint
            && own.run == expected.run
            && own.role == expected.role
            && own.attempt_or_first() == expected.attempt_or_first()
    });
    if !bound {
        return None;
    }
    let waypoints = route_waypoints(&events);
    let last_leaf = waypoints.last()?;
    let (run_id, ..) = latest_run_for_waypoint(&events, last_leaf)?;
    let run = find_run(&events, &run_id)?;
    let RunState::Claimed(claim_id) = run.state else {
        return None;
    };
    let world_hash = world_for_waypoint(&events, last_leaf).map(|world| WorldHash::of(&world))?;
    Some((claim_id, world_hash))
}

/// Evaluates `current`'s closure and, on success, walks outward through
/// its own ancestor chain as far as each successive container is also
/// its parent's last direct child — journaling one `StageClosed` per
/// level closed, or one final `StageHeld` where the cascade stops.
/// Returns `CascadeOutcome::Held` when the cascade stopped on a hold
/// (the caller must not advance the Work any further), or
/// `CascadeOutcome::Closed(outermost)` naming the outermost container
/// it actually closed — which is the container the Route continues
/// *after*, and so what a caller advancing the Work must ask about.
fn close_cascade(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    journal: &mut Journal,
    tree: &[WaypointDefinition],
    mut current: WaypointId,
) -> Result<CascadeOutcome, JournalError> {
    loop {
        let events = journal.replay()?;
        let attempt = container_attempt(&events, &current);
        match evaluate_closure(state, &events, tree, &current) {
            ClosureOutcome::Closed(receipts) => {
                let closed = new_event(
                    work_id,
                    None,
                    EventKind::StageClosed {
                        waypoint: current.clone(),
                        attempt,
                        receipts,
                    },
                );
                append_event(state, journal, work_id, &closed)?;
                let ancestors = ancestor_chain(tree, &current);
                match ancestors.first() {
                    Some(parent_id)
                        if find_definition(tree, parent_id)
                            .is_some_and(|parent| is_last_direct_child(parent, &current)) =>
                    {
                        current = parent_id.clone();
                    }
                    _ => return Ok(CascadeOutcome::Closed(current)),
                }
            }
            ClosureOutcome::Held(missing) => {
                // W-A correction (minor finding): an unchanged hold on
                // the same activation is already journaled — a restart
                // sweep or a second re-evaluation re-deriving the same
                // answer is *idempotent recovery*, not a new fact, and
                // must not grow the journal by one line per restart.
                // A hold whose reason changed (or one on a fresh
                // activation) is a new fact and is appended.
                let work_now = fold(&events);
                if matches!(
                    stage_outcome_at(&events, &current, attempt),
                    Some(StageOutcomeRef::Held(ref already)) if already == &missing
                ) && matches!(work_now.state, WorkState::Waiting)
                    && work_now.held.as_ref().is_some_and(|held| {
                        held.waypoint == current
                            && held.attempt == attempt
                            && held.missing == missing
                    })
                {
                    return Ok(CascadeOutcome::Held);
                }
                let held = new_event(
                    work_id,
                    None,
                    EventKind::StageHeld {
                        waypoint: current.clone(),
                        attempt,
                        missing,
                    },
                );
                append_event(state, journal, work_id, &held)?;
                return Ok(CascadeOutcome::Held);
            }
        }
    }
}

/// Re-evaluates `parent.work`'s held container (`parent.waypoint`),
/// triggered by an external event on a *different* Work's journal — a
/// spawned child validating Done or being canceled (module callers), or
/// the startup sweep over every `Waiting` Work
/// (`reevaluate_waiting_works`). A no-op, not an error, when the parent
/// Work no longer exists, is already terminal, or its named container
/// is not currently held (idempotent: re-evaluating an already-closed
/// container must never re-open or duplicate its receipts).
fn reevaluate_parent(state: &Arc<WirkdState>, parent: &ParentBinding) -> Result<(), JournalError> {
    let Some(journal) = journal_for(state, &parent.work)? else {
        return Ok(());
    };
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    let events = journal.replay()?;
    if events.is_empty() || fold(&events).state.is_terminal() {
        return Ok(());
    }
    // W-A correction (F1/F2): "currently held" is asked of the
    // activation in force, so a container reopened after its close is
    // never re-closed here from a superseded generation's evidence.
    if !matches!(
        current_stage_outcome(&events, &parent.waypoint),
        Some(StageOutcomeRef::Held(_))
    ) {
        return Ok(());
    }
    let defs = waypoint_defs_for(&events);
    let closed = match close_cascade(
        state,
        &parent.work,
        &mut journal,
        &defs,
        parent.waypoint.clone(),
    )? {
        CascadeOutcome::Held => return Ok(()),
        CascadeOutcome::Closed(outermost) => outermost,
    };
    // W-A correction: the cascade closed this container (and possibly
    // outer ones) without any Claim on *this* journal — the child that
    // satisfied it completed elsewhere. `handle_claim`'s auto-advance
    // never ran, so if the Route continues past the outermost container
    // just closed, reserve its next leaf here; otherwise the Work sits
    // `Active` on a closed container with no open Run and nothing to
    // claim or retry.
    if let Some(container) = find_definition(&defs, &closed)
        && let Some(last_leaf) = flatten_leaves(std::slice::from_ref(container))
            .last()
            .cloned()
        && let Err((code, message)) =
            reserve_next_leaf(state, &parent.work, &mut journal, &defs, &last_leaf)
    {
        eprintln!(
            "wirkd: advancing {} past its closed container failed: {code} {message}",
            parent.work.0
        );
    }
    let events_now = journal.replay()?;
    let parent_work = fold(&events_now);
    let grandparent = parent_work.parent.clone();
    drop(journal);
    if matches!(parent_work.state, WorkState::Completed)
        && let Some(grandparent) = grandparent
    {
        reevaluate_parent(state, &grandparent)?;
    }
    Ok(())
}

/// Startup sweep (W-A, §3.2, mirroring `recover_docker_runs`'s own
/// once-at-startup convention): every Work whose folded state is
/// currently `Waiting` gets its held container re-evaluated once, so a
/// crash between a child's own completing Claim and this Work's
/// `StageClosed` is repaired on restart without any external trigger.
/// Idempotent: a container already closed (or held for the same reason)
/// is left alone by `reevaluate_parent`'s own guard.
fn reevaluate_waiting_works(state: &Arc<WirkdState>) {
    let works_dir = state.estate_root.join("works");
    let Ok(entries) = std::fs::read_dir(&works_dir) else {
        return;
    };
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
        if events.is_empty() {
            continue;
        }
        let work = fold(&events);
        if !matches!(work.state, WorkState::Waiting) {
            continue;
        }
        let Some(held) = work.held.clone() else {
            continue;
        };
        let parent = ParentBinding {
            work: work.id.clone(),
            waypoint: held.waypoint,
            // The sweep re-evaluates one *held container*; the Run/role
            // fields of this binding are only `reevaluate_parent`'s
            // addressing, never evidence (it re-derives every receipt
            // from the parent's own journal). The activation likewise
            // comes from the journal there, not from here.
            attempt: Some(held.attempt),
            run: RunId(String::new()),
            role: String::new(),
        };
        if let Err(err) = reevaluate_parent(state, &parent) {
            eprintln!(
                "wirkd: startup re-evaluation of held Work {} failed: {err}",
                work.id.0
            );
        }
    }
}

// ---- W-A: cancel and cascade (§3.4) ------------------------------------

/// `wirk work cancel`: refuses `OpenChild` when a spawned, non-terminal
/// child exists and `--cascade` was not given (no journal write on
/// refusal, same shape as `retry`'s `NotNeedsInput`); with `--cascade`,
/// cancels every open child first (recursively, attributing each with
/// `caused_by`), then this Work. A terminal Work is a no-op success
/// (idempotent: a crash mid-cascade is repaired by re-running the verb).
fn handle_cancel(state: &Arc<WirkdState>, payload: CancelPayload) -> Reply {
    match cancel_work(
        state,
        &payload.work_id,
        payload.cascade,
        payload.reason,
        None,
    ) {
        Ok(()) => ok_reply(json!({})),
        Err((code, message)) => err_reply(code, &message),
    }
}

/// The recursive worker behind `handle_cancel`: `caused_by` is `None`
/// for the explicitly named target of the verb, `Some(parent)` for a
/// cascade step.
fn cancel_work(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    cascade: bool,
    reason: Option<String>,
    caused_by: Option<WorkId>,
) -> Result<(), (&'static str, String)> {
    let journal = journal_for(state, work_id)
        .map_err(|err| ("JournalError", err.to_string()))?
        .ok_or_else(|| ("NotFound", "no such work".to_string()))?;
    let events = {
        let journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
        journal
            .replay()
            .map_err(|err| ("JournalError", err.to_string()))?
    };
    if events.is_empty() {
        return Err(("NotFound", "no such work".to_string()));
    }
    let work = fold(&events);
    if work.state.is_terminal() {
        return Ok(());
    }

    let open_children: Vec<WorkId> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ChildWorkSpawned { child, .. } => Some(child.clone()),
            _ => None,
        })
        .filter(|child| {
            journal_for(state, child)
                .ok()
                .flatten()
                .and_then(|journal| {
                    let journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
                    journal.replay().ok()
                })
                .is_some_and(|events| !events.is_empty() && !fold(&events).state.is_terminal())
        })
        .collect();

    if !open_children.is_empty() {
        if !cascade {
            return Err((
                "OpenChild",
                format!(
                    "work {} has an open child {} (retry with --cascade)",
                    work_id.0, open_children[0].0
                ),
            ));
        }
        for child in &open_children {
            cancel_work(state, child, true, None, Some(work_id.clone()))?;
        }
    }

    let journal = journal_for(state, work_id)
        .map_err(|err| ("JournalError", err.to_string()))?
        .ok_or_else(|| ("NotFound", "no such work".to_string()))?;
    let mut journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
    // Re-check terminality under this Work's own lock: a concurrent
    // cancel of the same Work between the read above and this write is
    // harmless (idempotent no-op), never a double `WorkCanceled`.
    let events = journal
        .replay()
        .map_err(|err| ("JournalError", err.to_string()))?;
    if fold(&events).state.is_terminal() {
        return Ok(());
    }
    let event = new_event(work_id, None, EventKind::WorkCanceled { reason, caused_by });
    append_event(state, &mut journal, work_id, &event)
        .map_err(|err| ("JournalError", err.to_string()))?;
    drop(journal);

    // A canceled child never satisfies a receipt; the parent's own
    // container re-evaluates to `StageHeld` for that role, exactly as
    // it would for any other missing/invalid child completion.
    if let Some(parent) = work.parent
        && let Err(err) = reevaluate_parent(state, &parent)
    {
        return Err(("JournalError", err.to_string()));
    }
    Ok(())
}

// ---- Atlas (P3 W3, BUILD-BRIEF.md "Public surface") ----------------------

/// An `ExactCoordinate` travels the wire as hex-encoded JSON bytes (R3/
/// R6: stdlib only, no new dependency for a base64 crate) — argv-safe
/// and line-safe, since a Git pathname is raw bytes, never guaranteed
/// text. `decode_coordinate` is the inverse, used by every verb that
/// accepts a caller-supplied coordinate.
fn encode_coordinate(coordinate: &wirk_atlas::ExactCoordinate) -> String {
    let bytes = serde_json::to_vec(coordinate).expect("ExactCoordinate always serializes");
    hex_encode(&bytes)
}

fn decode_coordinate(encoded: &str) -> Result<wirk_atlas::ExactCoordinate, String> {
    let bytes = hex_decode(encoded)?;
    serde_json::from_slice(&bytes).map_err(|err| format!("malformed coordinate: {err}"))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("odd-length coordinate".to_string());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16)
                .map_err(|_| "invalid coordinate hex".to_string())
        })
        .collect()
}

fn membership_json(membership: &wirk_atlas::Membership) -> Value {
    json!({
        "id": membership.id.0,
        "estate": membership.estate.0,
        "alias": membership.alias,
        "source": membership.source.0,
        "locator": membership.locator,
        "requested_ref": membership.requested_ref,
    })
}

fn generation_json(generation: &wirk_atlas::SourceGeneration) -> Value {
    let mut coverage = std::collections::BTreeMap::from([
        ("indexed", 0u64),
        ("excluded", 0u64),
        ("unsupported", 0u64),
        ("unavailable", 0u64),
        ("error", 0u64),
    ]);
    for resource in &generation.resources {
        let key = match resource.disposition {
            wirk_atlas::CoverageDisposition::Indexed => "indexed",
            wirk_atlas::CoverageDisposition::Excluded => "excluded",
            wirk_atlas::CoverageDisposition::Unsupported => "unsupported",
            wirk_atlas::CoverageDisposition::Unavailable => "unavailable",
            wirk_atlas::CoverageDisposition::Error => "error",
        };
        *coverage.get_mut(key).expect("all five keys pre-seeded") += 1;
    }
    json!({
        "generation": generation.id.0,
        "source": generation.source.0,
        "revision": generation.revision,
        "content": generation.content,
        "extractor_set": generation.extractor_set,
        "acquisition_policy": generation.acquisition_policy,
        "coverage": {
            "indexed": coverage["indexed"],
            "excluded": coverage["excluded"],
            "unsupported": coverage["unsupported"],
            "unavailable": coverage["unavailable"],
            "error": coverage["error"],
            "total": generation.resources.len(),
        },
    })
}

/// `handle_atlas_acquire`: registers `source` (on first use) against
/// `repository`, then stages an acquisition at `revision` — creation
/// only, never a query's side effect (BUILD-BRIEF.md: "Query and exact
/// resolution cannot create, refresh, fetch, embed or repair stores").
fn handle_atlas_acquire(state: &Arc<WirkdState>, payload: super::AtlasAcquirePayload) -> Reply {
    let mut atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let membership =
        match atlas.register_git(&payload.source, &payload.repository, &payload.revision) {
            Ok(membership) => membership,
            Err(err) => return err_reply("AtlasError", &err.to_string()),
        };
    acquire_reply(&mut atlas, &membership, &payload.revision)
}

/// `handle_atlas_refresh`: reuses `source`'s existing registration and
/// membership (`UnknownSource` if none exists yet — refresh never
/// creates a registration, only `acquire` does); stages a candidate
/// generation without publishing it.
fn handle_atlas_refresh(state: &Arc<WirkdState>, payload: super::AtlasRefreshPayload) -> Reply {
    let mut atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some(membership) = atlas
        .memberships()
        .find(|membership| membership.alias == payload.source)
        .cloned()
    else {
        return err_reply(
            "UnknownSource",
            &format!("no registered source named {}", payload.source),
        );
    };
    acquire_reply(&mut atlas, &membership, &payload.revision)
}

fn acquire_reply(
    atlas: &mut wirk_atlas::AtlasStore,
    membership: &wirk_atlas::Membership,
    revision: &str,
) -> Reply {
    match atlas.acquire(membership, revision, wirk_atlas::ExtractorPolicy::default()) {
        Ok(wirk_atlas::AcquireOutcome::Staged(generation)) => ok_reply(json!({
            "membership": membership_json(membership),
            "outcome": "staged",
            "generation": generation_json(&generation),
        })),
        Ok(wirk_atlas::AcquireOutcome::Unavailable(detail)) => ok_reply(json!({
            "membership": membership_json(membership),
            "outcome": "unavailable",
            "detail": detail,
        })),
        Err(err) => err_reply("AtlasError", &err.to_string()),
    }
}

/// `handle_atlas_publish`: atomically advances `source`'s published
/// generation to the already-staged `generation` — a second, explicit
/// operation from `acquire`/`refresh` (BUILD-BRIEF.md: "An immutable
/// staged generation is unreadable to queries until a separate catalog
/// publication names it").
fn handle_atlas_publish(state: &Arc<WirkdState>, payload: super::AtlasPublishPayload) -> Reply {
    let mut atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some(membership) = atlas
        .memberships()
        .find(|membership| membership.alias == payload.source)
        .cloned()
    else {
        return err_reply(
            "UnknownSource",
            &format!("no registered source named {}", payload.source),
        );
    };
    let generation = wirk_atlas::GenerationId(payload.generation.clone());
    match atlas.publish(&membership, &generation) {
        Ok(()) => ok_reply(json!({
            "membership": membership_json(&membership),
            "generation": payload.generation,
            "publication_revision": atlas.publication_revision(),
        })),
        Err(err) => err_reply("AtlasError", &err.to_string()),
    }
}

/// `handle_atlas_status`: every registered source (or the one named),
/// its currently published generation and coverage summary, and its
/// recent acquisition attempts — read-only, creates nothing (a fresh
/// estate or an unregistered source name reports emptily rather than
/// fabricating a generation).
///
/// P3 W3 correction (ruling 0093, W3-CORRECTION.md item 3): `registered`
/// used to read `true` on a totally fresh estate whenever no specific
/// `--source` was named (`!sources.is_empty() || payload.source.is_none()`
/// — vacuously true for an empty catalog) — a fresh estate now reports
/// `sources_total: 0` unambiguously instead. `registered` is reported
/// only when a specific `--source` was named, meaning exactly "this name
/// exists in the catalog".
///
/// Item 3 also requires `--work` scoping: when given, a source this
/// Work's own journaled bindings do not admit is dropped entirely,
/// never disclosing its locator, revision or generation (VERDICT.md
/// L4). Omitting `--work` remains estate-wide catalog administration —
/// an explicit, distinct capability from Work-scoped retrieval, not a
/// bug to close by removing it.
fn handle_atlas_status(state: &Arc<WirkdState>, payload: super::AtlasStatusPayload) -> Reply {
    let scope = match resolve_query_scope(state, &payload.work) {
        Ok(scope) => scope,
        Err(reply) => return reply,
    };
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Ruling 0095 (correction-verify VERDICT §5 R3): under a Work scope,
    // both of these count only what this scope actually admits. Counting
    // — or answering `registered` over — every alias in the catalog made
    // a Work-scoped call an existence oracle for aliases the same scope
    // refuses to search: `--source secret` answered `registered: true`
    // for a denied alias and `false` for a name that does not exist. No
    // locator, revision or generation leaked, but the alias itself did.
    // The estate-wide administrative call (`payload.work` absent) is
    // unchanged and still discloses everything, exactly as 0093 requires
    // the explicit administration surface to.
    let scoped = payload.work.is_some();
    let mut sources_total = 0usize;
    let mut sources = Vec::new();
    let mut name_found = false;
    for membership in atlas.memberships() {
        let admitted = admitted_membership_for(&atlas, &scope, &membership.id).is_some();
        if !scoped || admitted {
            sources_total += 1;
        }
        if let Some(wanted) = &payload.source
            && &membership.alias != wanted
        {
            continue;
        }
        if !scoped || admitted {
            name_found = true;
        }
        if !admitted {
            continue;
        }
        let current = match atlas.current(membership) {
            Ok(generation) => generation,
            Err(err) => return err_reply("AtlasError", &err.to_string()),
        };
        let attempts: Vec<Value> = atlas
            .attempts()
            .iter()
            .filter(|attempt| attempt.membership == membership.id)
            .map(|attempt| {
                json!({
                    "at_unix_millis": attempt.at_unix_millis,
                    "requested_ref": attempt.requested_ref,
                    "outcome": attempt.outcome,
                    "generation": attempt.generation.as_ref().map(|g| g.0.clone()),
                    "diagnostic": attempt.diagnostic,
                })
            })
            .collect();
        let semantic = match semantic_status_record(&atlas, membership) {
            Ok(semantic) => semantic,
            Err(reply) => return reply,
        };
        sources.push(json!({
            "membership": membership_json(membership),
            "published_generation": current.as_ref().map(generation_json),
            "recent_attempts": attempts,
            "semantic": semantic,
        }));
    }
    let mut result = json!({
        "publication_revision": atlas.publication_revision(),
        "sources_total": sources_total,
        "sources": sources,
        "work_scoped": payload.work.is_some(),
    });
    if let Some(wanted) = &payload.source {
        if scoped {
            // Deliberately one answer for both "denied" and "no such
            // alias": under a Work scope the only honest thing to say is
            // whether *this scope* can see it.
            result["admitted"] = json!(name_found);
        } else {
            result["registered"] = json!(name_found);
        }
        result["source"] = json!(wanted);
    }
    ok_reply(result)
}

// ---- Atlas semantic editions (P3 W4 A, W4-PUBLIC-LIFECYCLE-BUILD.md) -----

fn configured_path_json(path: &wirk_atlas::ConfiguredPath) -> Value {
    json!({
        "configured": path.configured,
        "canonical": path.canonical,
        "digest": path.digest,
        "byte_len": path.byte_len,
        "file_count": path.file_count,
    })
}

/// The complete public identity of one edition. Every field is what was
/// actually measured — recipe equality is not output identity (0078), so
/// the vector and mapping digests are first-class here, not a footnote.
fn edition_json(edition: &wirk_atlas::SemanticEdition) -> Value {
    json!({
        "edition": edition.id.0,
        // Which identity scheme this record's id was computed under. A
        // `v1` edition predates the complete-argv and backend-environment
        // bindings and says so rather than implying them.
        "identity": edition.identity,
        "estate": edition.estate.0,
        "membership": edition.membership.0,
        "source": edition.source.0,
        "generation": edition.generation.0,
        "revision": edition.generation_revision,
        "content": edition.generation_content,
        "acquisition_policy": edition.acquisition_policy,
        "chunker": {
            "extractor_set": edition.chunker.extractor_set,
            "unitizer": edition.chunker.unitizer,
        },
        "model": {
            "consumed": configured_path_json(&edition.model.consumed),
            "reported_path": edition.model.reported_path,
            "reported_digest": edition.model.reported_digest,
        },
        "backend": {
            "protocol": edition.backend.protocol,
            "program": configured_path_json(&edition.backend.program),
            "arguments": edition.backend.arguments.iter().map(configured_path_json).collect::<Vec<_>>(),
            "argv": edition.backend.argv.iter().map(backend_argument_json).collect::<Vec<_>>(),
            "reported": edition.backend.reported,
            "environment": backend_environment_json(&edition.backend.environment),
        },
        "vectors": {
            "format": edition.vectors.format,
            "file": edition.vectors.file,
            "rows": edition.vectors.rows,
            "dimensions": edition.vectors.dimensions,
            "byte_len": edition.vectors.byte_len,
            "digest": edition.vectors.digest,
        },
        "mapping": {
            "file": edition.mapping.file,
            "rows": edition.mapping.rows,
            "byte_len": edition.mapping.byte_len,
            "digest": edition.mapping.digest,
        },
        "producer": {
            "producer": edition.producer.producer,
            "built_at_unix_millis": edition.producer.built_at_unix_millis,
        },
    })
}

fn backend_argument_json(argument: &wirk_atlas::BackendArgument) -> Value {
    match argument {
        wirk_atlas::BackendArgument::Literal { value } => {
            json!({"kind": "literal", "value": value})
        }
        wirk_atlas::BackendArgument::File { value, file } => {
            json!({"kind": "file", "value": value, "file": configured_path_json(file)})
        }
    }
}

fn unavailable_entries_json(entries: &[wirk_atlas::UnavailableEntry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|entry| json!({"name": entry.name, "reason": entry.reason}))
            .collect(),
    )
}

/// `unmeasured` is the honest reading of every record written before
/// loaded modules were measured. It is deliberately not `complete`: those
/// builds never looked, and saying otherwise would mint coverage they
/// never had.
fn environment_coverage_json(coverage: &wirk_atlas::EnvironmentCoverage) -> Value {
    match coverage {
        wirk_atlas::EnvironmentCoverage::Unmeasured => json!({"state": "unmeasured"}),
        wirk_atlas::EnvironmentCoverage::Complete => json!({"state": "complete"}),
        wirk_atlas::EnvironmentCoverage::Partial(detail) => {
            json!({"state": "partial", "detail": detail})
        }
    }
}

/// `unreported` is a first-class answer, not a missing field: a backend
/// that cannot enumerate its own environment produces an edition whose
/// implementation provenance is honestly absent, which a reader must be
/// able to tell apart from one that was measured
/// (`W4-LIFECYCLE-CORRECTION.md` item 3).
///
/// `W4-PRODUCER-PROVENANCE-CORRECTION.md` item 2 adds a second axis to the
/// same rule: among the editions that *did* report, one that measured
/// every loaded module and one that could not must not render alike. The
/// state a reader sees is therefore the coverage — `complete`, `partial`
/// with the reason, `unmeasured` for a record written before loaded
/// modules were measured at all — and never a bare "reported".
fn backend_environment_json(environment: &wirk_atlas::BackendEnvironment) -> Value {
    match environment {
        wirk_atlas::BackendEnvironment::Unreported => json!({"state": "unreported"}),
        wirk_atlas::BackendEnvironment::Reported(identity) => json!({
            "state": "reported",
            "coverage": environment_coverage_json(&identity.coverage),
            "scope": identity.scope,
            "kind": identity.kind,
            "root": identity.root,
            "runtime": identity.runtime,
            "executable": identity.executable,
            "digest": identity.digest,
            "modules_total": identity.modules.len(),
            "modules": identity.modules.iter().map(|module| json!({
                "name": module.name,
                "origin": module.origin,
                "path": module.path,
                "digest": module.digest,
                "byte_len": module.byte_len,
                "attribution": match &module.attribution {
                    wirk_atlas::ModuleAttribution::Declared(name) =>
                        json!({"state": "declared", "distribution": name}),
                    wirk_atlas::ModuleAttribution::Undeclared(detail) =>
                        json!({"state": "undeclared", "detail": detail}),
                },
            })).collect::<Vec<_>>(),
            "undescribed_distributions": unavailable_entries_json(
                &identity.undescribed_distributions),
            "unmeasured_modules": unavailable_entries_json(&identity.unmeasured_modules),
            "distributions": identity.distributions.iter().map(|distribution| json!({
                "name": distribution.name,
                "version": distribution.version,
                "metadata_path": distribution.metadata_path,
                "record_digest": distribution.record_digest,
                "metadata_digest": distribution.metadata_digest,
                "declared_files": distribution.declared_files,
                "declared_byte_len": distribution.declared_byte_len,
                "files_checked": distribution.files_checked,
                "files_missing": distribution.files_missing,
                "files_mismatched": distribution.files_mismatched,
            })).collect::<Vec<_>>(),
        }),
    }
}

/// `verified` here means "these bytes are the bytes this record commits
/// to", and nothing else. It deliberately says nothing about retrieval:
/// W4 A builds and selects editions, and no query reads them yet.
fn verification_json(verification: &wirk_atlas::SemanticVerification) -> Value {
    match verification {
        wirk_atlas::SemanticVerification::Verified => json!({"state": "verified"}),
        wirk_atlas::SemanticVerification::Missing(detail) => {
            json!({"state": "missing", "detail": detail})
        }
        wirk_atlas::SemanticVerification::Corrupt(detail) => {
            json!({"state": "corrupt", "detail": detail})
        }
        wirk_atlas::SemanticVerification::Unavailable(detail) => {
            json!({"state": "unavailable", "detail": detail})
        }
    }
}

fn membership_by_alias(
    atlas: &wirk_atlas::AtlasStore,
    alias: &str,
) -> Option<wirk_atlas::Membership> {
    atlas
        .memberships()
        .find(|membership| membership.alias == alias)
        .cloned()
}

/// `handle_atlas_semantic_build`: stage one immutable semantic edition.
/// Creation only — never a query's side effect, never implicit, and never
/// a publication: the reply's `outcome` is `staged`, and no reader
/// consults the result until `atlas semantic select` names it.
fn handle_atlas_semantic_build(
    state: &Arc<WirkdState>,
    payload: super::AtlasSemanticBuildPayload,
) -> Reply {
    let mut atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some(membership) = membership_by_alias(&atlas, &payload.source) else {
        return err_reply(
            "UnknownSource",
            &format!("no registered source named {}", payload.source),
        );
    };
    let config = wirk_atlas::SemanticBuildConfig {
        backend: std::path::PathBuf::from(&payload.backend),
        backend_args: payload.backend_args.clone(),
        model: std::path::PathBuf::from(&payload.model),
        // The producer of a product build is this daemon's own verb, at
        // this protocol version. 0089 forbids minting producer proof for
        // vectors this product did not create; it always knows its own.
        producer: format!("wirkd/atlas-semantic-build/{PROTOCOL_VERSION}"),
    };
    let generation = wirk_atlas::GenerationId(payload.generation.clone());
    match atlas.build_semantic(&membership, &generation, &config) {
        Ok(wirk_atlas::SemanticBuildOutcome::Staged(edition)) => ok_reply(json!({
            "membership": membership_json(&membership),
            "outcome": "staged",
            "edition": edition_json(&edition),
        })),
        Ok(wirk_atlas::SemanticBuildOutcome::Refused(reason)) => ok_reply(json!({
            "membership": membership_json(&membership),
            "outcome": "refused",
            "detail": reason,
        })),
        Err(err) => err_reply("AtlasError", &err.to_string()),
    }
}

/// `handle_atlas_semantic_select`: the separate, atomic publication step.
/// The store re-verifies the edition's own bytes and its exact committed
/// coordinates before any catalog write, so a refused selection leaves
/// the previously selected edition exactly as it was.
fn handle_atlas_semantic_select(
    state: &Arc<WirkdState>,
    payload: super::AtlasSemanticSelectPayload,
) -> Reply {
    let mut atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some(membership) = membership_by_alias(&atlas, &payload.source) else {
        return err_reply(
            "UnknownSource",
            &format!("no registered source named {}", payload.source),
        );
    };
    let previous = atlas.selected_semantic(&membership);
    let edition = wirk_atlas::EditionId(payload.edition.clone());
    match atlas.select_semantic(&membership, &edition) {
        Ok(Ok(edition)) => ok_reply(json!({
            "membership": membership_json(&membership),
            "outcome": "selected",
            "edition": edition_json(&edition),
            "publication_revision": atlas.publication_revision(),
        })),
        Ok(Err(reason)) => ok_reply(json!({
            "membership": membership_json(&membership),
            "outcome": "refused",
            "detail": reason,
            // Named explicitly so a failed replacement is legible as
            // "the old one still stands", not as an unknown state.
            "selected": previous.map(|id| id.0),
            "publication_revision": atlas.publication_revision(),
        })),
        Err(err) => err_reply("AtlasError", &err.to_string()),
    }
}

/// This source's semantic record for `atlas status`: every edition on
/// disk, which one is selected, and what each one's bytes actually verify
/// as right now. Under a `--work` scope this is only ever reached for a
/// membership that scope already admits, so it discloses nothing the
/// caller could not already see.
fn semantic_status_record(
    atlas: &wirk_atlas::AtlasStore,
    membership: &wirk_atlas::Membership,
) -> Result<Value, Reply> {
    let editions = match atlas.semantic_editions(membership) {
        Ok(editions) => editions,
        Err(err) => return Err(err_reply("AtlasError", &err.to_string())),
    };
    let selected = atlas.selected_semantic(membership);
    let rendered: Vec<Value> = editions
        .iter()
        .map(|state| {
            let mut value = edition_json(&state.edition);
            value["state"] = json!(if state.selected { "selected" } else { "staged" });
            value["verification"] = verification_json(&state.verification);
            // Retained-and-intact is not the same as
            // describes-what-this-source-publishes
            // (`W4-LIFECYCLE-CORRECTION.md` item 1). Both are reported,
            // per edition, and neither is inferred from the other.
            value["current"] = json!(state.current);
            value
        })
        .collect();
    // `selected_available` now means what a caller reading only the
    // summary would take it to mean: this selection is usable right now.
    // A selection whose generation is superseded, whose bytes do not
    // verify, or whose record cannot be read, is `false` with the reason
    // beside it — never `true` because a record with that id exists.
    let availability = match atlas.semantic_availability(membership) {
        Ok(availability) => availability,
        Err(err) => return Err(err_reply("AtlasError", &err.to_string())),
    };
    let mut availability_json = json!({"state": availability.label()});
    if let Some(detail) = availability.detail() {
        availability_json["detail"] = json!(detail);
    }
    Ok(json!({
        "selected": selected.as_ref().map(|id| id.0.clone()),
        "selected_available": availability.selected_available(),
        "availability": availability_json,
        "editions_total": rendered.len(),
        "editions": rendered,
    }))
}

/// Estate-wide orientation (`payload.work` absent) or a real Work's own
/// journaled `repositories` (present) — the complete admission scope,
/// never a client-supplied replacement grant set (BUILD-AMENDMENTS.md:
/// "A Work-scoped query must derive its complete read admission from
/// the journaled Work, not accept a replacement grant set from the
/// client").
fn resolve_query_scope(
    state: &Arc<WirkdState>,
    work: &Option<WorkId>,
) -> Result<wirk_atlas::QueryScope, Reply> {
    let Some(work_id) = work else {
        return Ok(wirk_atlas::QueryScope::EstateOrientation);
    };
    let events = match journal_for(state, work_id) {
        Ok(Some(journal)) => {
            let journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
            match journal.replay() {
                Ok(events) => events,
                Err(err) => return Err(err_reply("JournalError", &err.to_string())),
            }
        }
        Ok(None) => return Err(err_reply("NotFound", "no such work")),
        Err(err) => return Err(err_reply("JournalError", &err.to_string())),
    };
    if events.is_empty() {
        return Err(err_reply("NotFound", "no such work"));
    }
    Ok(wirk_atlas::QueryScope::Work(fold(&events).repositories))
}

/// Re-derives `wirk_atlas::admission::admit`'s own single rule (that
/// function is private to its crate; W3 is a trusted caller across the
/// crate boundary, not a reason to widen its public surface) rather
/// than admitting on the daemon's own separate judgment: a membership
/// is admissible under `QueryScope::EstateOrientation` unconditionally,
/// or under `QueryScope::Work(grants)` only if some grant names its
/// alias.
fn admitted_membership_for(
    atlas: &wirk_atlas::AtlasStore,
    scope: &wirk_atlas::QueryScope,
    id: &wirk_atlas::MembershipId,
) -> Option<wirk_atlas::Membership> {
    let membership = atlas.memberships().find(|member| &member.id == id)?.clone();
    match scope {
        wirk_atlas::QueryScope::EstateOrientation => Some(membership),
        wirk_atlas::QueryScope::Work(grants) => grants
            .iter()
            .any(|grant| grant.name == membership.alias)
            .then_some(membership),
    }
}

fn evidence_hit_json(hit: &wirk_atlas::EvidenceHit) -> Value {
    json!({
        "coordinate": encode_coordinate(&hit.coordinate),
        "estate": hit.coordinate.estate.0,
        "source": hit.coordinate.source.0,
        "generation": hit.coordinate.generation.0,
        // P3 W3 correction (ruling 0093, W3-CORRECTION.md item 4;
        // VERDICT.md L3): BUILD-BRIEF.md's own decisive assertion is
        // "every hit names estate, source, generation,
        // revision/content/extractor identities" — these three were
        // previously only recoverable by joining against a separate
        // `atlas status` call.
        "revision": hit.generation_identity.revision,
        "content": hit.generation_identity.content,
        "extractor_set": hit.generation_identity.extractor_set,
        "path": String::from_utf8_lossy(&hit.coordinate.path),
        "line_start": hit.coordinate.line_start,
        "line_end": hit.coordinate.line_end,
        "score": hit.score,
        "snippet": hit.snippet,
    })
}

fn budget_json(budget: &wirk_atlas::AnswerBudget) -> Value {
    json!({
        "limit": budget.limit,
        "offset": budget.offset,
        "total_candidates": budget.total_candidates,
        "returned": budget.returned,
    })
}

fn semantic_status_json(status: &wirk_atlas::SemanticStatus) -> Value {
    match status {
        wirk_atlas::SemanticStatus::Applied => json!({"status": "applied"}),
        wirk_atlas::SemanticStatus::Unavailable(reason) => {
            json!({"status": "unavailable", "reason": reason})
        }
        wirk_atlas::SemanticStatus::Disabled => json!({"status": "disabled"}),
    }
}

fn coverage_json(coverage: &wirk_atlas::AnswerCoverage) -> Value {
    json!({
        "no_match": coverage.no_match,
        "partial": coverage.partial,
        "source_unavailable": coverage.source_unavailable,
        "generation_unavailable": coverage.generation_unavailable,
        "unsupported_family": coverage.unsupported_family,
        // P3 W3 correction (ruling 0093, W3-CORRECTION.md item 3;
        // VERDICT.md M1/L1/L2): distinct from `no_match` — the scope
        // admitted nothing to search (`denied`) or the addressed estate
        // (or named source) has no registered membership at all
        // (`no_sources`, the fresh-estate case) are both "never
        // searched", never a positive assertion of absence.
        "denied": coverage.denied,
        "no_sources": coverage.no_sources,
        // Ruling 0095: end of *this answer's* pages, reported in its own
        // right so it is never told as "the corpus held nothing" beside a
        // `budget.total_candidates` that says otherwise.
        "spent": coverage.spent,
        "complete": coverage.is_complete(),
    })
}

/// One `search` answer's own continuation token (ruling 0093,
/// W3-CORRECTION.md item 1): the exact generation vector it read plus
/// the request that produced it and the next page offset — hex-encoded
/// JSON, the same convention `encode_coordinate` uses. Every field of
/// the *request* that produced an answer is captured so a later
/// `--continue` can be checked against the caller's own restated
/// request rather than trusted blind; only `offset`/`generations`
/// actually drive re-execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
struct ContinuationToken {
    work: Option<String>,
    query: String,
    source: Option<String>,
    families: Vec<String>,
    semantic: Option<String>,
    limit: usize,
    offset: usize,
    generations: Vec<(String, String)>,
}

/// Reads `<wirk_dir>/continuation-key`, creating it from the kernel
/// CSPRNG on first use (`WirkdState::continuation_key`'s own doc for why
/// it lives here and not in the Atlas estate). R4: `/dev/urandom` is the
/// platform's own CSPRNG, read the ordinary way — no dependency, no
/// hand-rolled seeding. Mode 0600 is set before any byte is written, so
/// the secret is never briefly world-readable.
fn load_or_create_continuation_key(wirk_dir: &Path) -> io::Result<[u8; 32]> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt;
    let path = wirk_dir.join("continuation-key");
    if let Ok(existing) = std::fs::read(&path)
        && existing.len() == 32
    {
        let mut key = [0u8; 32];
        key.copy_from_slice(&existing);
        return Ok(key);
    }
    let mut key = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut key)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&key)?;
    file.sync_all()?;
    Ok(key)
}

/// HMAC-SHA256 over one continuation token's exact serialized bytes.
/// R5: the `hmac` crate's own construction over the `sha2` this
/// workspace already depends on, not a hand-rolled keyed hash.
fn continuation_tag(key: &[u8; 32], body: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac =
        Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(body);
    mac.finalize().into_bytes().to_vec()
}

/// `<hex of the token's JSON>.<hex of its HMAC>` — the tag covers the
/// exact bytes transmitted, so there is no canonicalization gap between
/// what was signed and what is later verified.
fn encode_continuation(key: &[u8; 32], token: &ContinuationToken) -> String {
    let body = serde_json::to_vec(token).expect("ContinuationToken always serializes");
    let tag = continuation_tag(key, &body);
    format!("{}.{}", hex_encode(&body), hex_encode(&tag))
}

/// Why the tag is checked *before* the token is interpreted at all: the
/// fields a continuation carries (`generations`, `offset`) are the ones
/// that decide which immutable blobs `wirk_atlas::search` will read, and
/// every one of them is visible to any caller who has ever received one
/// answer. Ruling 0095: "A token assembled by a client from known
/// metadata must not masquerade as a previously captured answer."
/// `verify_slice` is the `hmac` crate's own constant-time comparison.
fn decode_continuation(key: &[u8; 32], encoded: &str) -> Result<ContinuationToken, Reply> {
    use hmac::{Hmac, KeyInit, Mac};
    let Some((body_hex, tag_hex)) = encoded.split_once('.') else {
        return Err(err_reply(
            "MalformedContinuation",
            "a continuation token is <body>.<tag>; this one carries no authenticity tag",
        ));
    };
    let body =
        hex_decode(body_hex).map_err(|detail| err_reply("MalformedContinuation", &detail))?;
    let tag = hex_decode(tag_hex).map_err(|detail| err_reply("MalformedContinuation", &detail))?;
    let mut mac =
        Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(&body);
    if mac.verify_slice(&tag).is_err() {
        return Err(err_reply(
            "ForgedContinuation",
            "this continuation token was not issued by this estate for a real answer",
        ));
    }
    serde_json::from_slice(&body).map_err(|err| {
        err_reply(
            "MalformedContinuation",
            &format!("malformed continuation: {err}"),
        )
    })
}

/// `handle_atlas_search`: lexical (optionally semantic-*requested*,
/// never silently applied — W4 owns the real backend) ranked search
/// over the resolved scope's admitted, published generations.
///
/// P3 W3 correction (ruling 0093, W3-CORRECTION.md item 1): `payload.
/// continuation`, when present, must name the *same* query/scope this
/// request restates — a caller cannot swap `--work`, `--query`,
/// `--source`, `--family`, `--semantic` or `--limit` mid-continuation
/// and inherit a different answer's captured generations. The scope
/// itself is still re-derived fresh from the *current* journaled Work
/// on every call (`resolve_query_scope`), never taken from the token —
/// a continuation cannot mint authority a Work's real bindings do not
/// currently grant, even if they once did.
fn handle_atlas_search(state: &Arc<WirkdState>, payload: super::AtlasSearchPayload) -> Reply {
    let scope = match resolve_query_scope(state, &payload.work) {
        Ok(scope) => scope,
        Err(reply) => return reply,
    };
    let semantic = match payload.semantic.as_deref() {
        None | Some("disabled") => wirk_atlas::SemanticRequest::Disabled,
        Some("requested") => wirk_atlas::SemanticRequest::Requested,
        Some(other) => {
            return err_reply("BadRequest", &format!("unknown --semantic value {other}"));
        }
    };
    let mut families = Vec::new();
    for family in &payload.families {
        families.push(match family.as_str() {
            "code" => wirk_atlas::ContentFamily::Code,
            "knowledge" => wirk_atlas::ContentFamily::Knowledge,
            "config" => wirk_atlas::ContentFamily::Config,
            other => return err_reply("BadRequest", &format!("unknown content family {other}")),
        });
    }
    let limit = payload.limit.unwrap_or(10);
    let (pinned, offset) = match &payload.continuation {
        None => (None, 0),
        Some(token) => {
            let decoded = match decode_continuation(&state.continuation_key, token) {
                Ok(decoded) => decoded,
                Err(reply) => return reply,
            };
            let restated = ContinuationToken {
                work: payload.work.as_ref().map(|w| w.0.clone()),
                query: payload.query.clone(),
                source: payload.source.clone(),
                families: payload.families.clone(),
                semantic: payload.semantic.clone(),
                limit,
                offset: decoded.offset,
                generations: decoded.generations.clone(),
            };
            if decoded != restated {
                return err_reply(
                    "ContinuationMismatch",
                    "the continuation token names a different work/query/source/family/semantic/limit than this request",
                );
            }
            let pinned: BTreeMap<wirk_atlas::MembershipId, wirk_atlas::GenerationId> = decoded
                .generations
                .iter()
                .map(|(membership, generation)| {
                    (
                        wirk_atlas::MembershipId(membership.clone()),
                        wirk_atlas::GenerationId(generation.clone()),
                    )
                })
                .collect();
            (Some(pinned), decoded.offset)
        }
    };
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let request = wirk_atlas::SearchRequest {
        scope,
        requested_source: payload.source.clone(),
        query: payload.query.clone(),
        families,
        semantic,
        limit,
        pinned,
        offset,
    };
    match wirk_atlas::search(&atlas, &request) {
        Ok(answer) => {
            let continuation = ContinuationToken {
                work: payload.work.as_ref().map(|w| w.0.clone()),
                query: payload.query.clone(),
                source: payload.source.clone(),
                families: payload.families.clone(),
                semantic: payload.semantic.clone(),
                limit,
                offset: offset + answer.hits.len(),
                generations: answer
                    .generations
                    .iter()
                    .map(|(membership, generation)| (membership.0.clone(), generation.0.clone()))
                    .collect(),
            };
            ok_reply(json!({
                "publication_revision": answer.publication_revision,
                "generations": answer.generations.iter().map(|(membership, generation)| json!({
                    "membership": membership.0,
                    "generation": generation.0,
                })).collect::<Vec<_>>(),
                "admission": {"admitted": answer.admission.admitted, "denied": answer.admission.denied},
                "hits": answer.hits.iter().map(evidence_hit_json).collect::<Vec<_>>(),
                "semantic": semantic_status_json(&answer.semantic),
                "coverage": coverage_json(&answer.coverage),
                "truncated": answer.truncated,
                "budget": budget_json(&answer.budget),
                "continuation": if answer.coverage.no_sources || answer.coverage.denied {
                    None
                } else {
                    Some(encode_continuation(&state.continuation_key, &continuation))
                },
            }))
        }
        Err(err) => err_reply("AtlasError", &err.to_string()),
    }
}

/// `handle_atlas_resolve`: exact evidence-coordinate resolution.
/// `InadmissibleEstate` refuses a coordinate naming a different estate
/// before any lookup/ranking, without disclosing its path/content
/// (BUILD-BRIEF.md's decisive-scenario requirement); `Inadmissible`
/// refuses one naming a membership this scope does not grant, same
/// non-disclosure.
fn handle_atlas_resolve(state: &Arc<WirkdState>, payload: super::AtlasResolvePayload) -> Reply {
    let scope = match resolve_query_scope(state, &payload.work) {
        Ok(scope) => scope,
        Err(reply) => return reply,
    };
    let coordinate = match decode_coordinate(&payload.coordinate) {
        Ok(coordinate) => coordinate,
        Err(detail) => return err_reply("MalformedCoordinate", &detail),
    };
    if coordinate.estate.0 != state.estate_root.display().to_string() {
        return err_reply(
            "InadmissibleEstate",
            "coordinate names a different estate than this daemon's own",
        );
    }
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let Some(membership) = admitted_membership_for(&atlas, &scope, &coordinate.membership) else {
        return err_reply(
            "Inadmissible",
            "coordinate's membership is not admissible under this scope",
        );
    };
    match atlas.resolve_exact(&membership, &coordinate) {
        Ok(outcome) => ok_reply(resolve_outcome_json(outcome, &membership.locator)),
        Err(err) => err_reply("AtlasError", &err.to_string()),
    }
}

/// The full committed blob's own byte length (P3 W3 correction, ruling
/// 0093, W3-CORRECTION.md item 4; VERDICT.md L3: "no evidence budget is
/// disclosed on either search or resolve"): `resolve`'s own answer
/// already names an exact `byte_start`/`byte_end` span the caller
/// chose; this discloses how large the object it was cut from actually
/// is, so a caller can tell a deliberately narrow span from the whole
/// object. Read directly from Git (`cat-file -s`), never from the
/// bounded extracted unit — the same real-object source `resolve_exact`
/// itself already re-verified the span against.
fn blob_total_bytes(locator: &str, object_id: &str) -> Option<u64> {
    let output = Command::new("git")
        .arg("-C")
        .arg(locator)
        .args(["cat-file", "-s", object_id])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn resolve_outcome_json(outcome: wirk_atlas::ResolveOutcome, locator: &str) -> Value {
    match outcome {
        wirk_atlas::ResolveOutcome::Resolved(evidence) => json!({
            "outcome": "resolved",
            "coordinate": encode_coordinate(&evidence.coordinate),
            "path": String::from_utf8_lossy(&evidence.coordinate.path),
            "line_start": evidence.coordinate.line_start,
            "line_end": evidence.coordinate.line_end,
            "text": String::from_utf8(evidence.bytes.clone()).ok(),
            "bytes_hex": hex_encode(&evidence.bytes),
            "budget": {
                "returned_bytes": evidence.bytes.len(),
                "total_bytes": blob_total_bytes(locator, &evidence.coordinate.object_id),
            },
        }),
        wirk_atlas::ResolveOutcome::Absent => json!({"outcome": "absent"}),
        wirk_atlas::ResolveOutcome::Excluded(detail) => {
            json!({"outcome": "excluded", "detail": detail})
        }
        wirk_atlas::ResolveOutcome::Unsupported(detail) => {
            json!({"outcome": "unsupported", "detail": detail})
        }
        wirk_atlas::ResolveOutcome::Unavailable(detail) => {
            json!({"outcome": "unavailable", "detail": detail})
        }
    }
}

/// The producing action an assertion is attributed to: one Run that is
/// *executing right now* under this Work, and the World it was reserved
/// against.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProducingAction {
    run: RunId,
    waypoint: WaypointId,
    world_hash: WorldHash,
}

/// Why a *current open* Run and not the Work's latest validated Claim
/// (ruling 0095, superseding this function's ruling-0093 predecessor
/// `latest_validated_claim`).
///
/// A validated Claim is a real journal fact, but it is the receipt of
/// some *earlier* action: it says this Work once produced something the
/// estate validated, and nothing at all about the assertion now being
/// recorded. Two consequences the correction-verify VERDICT demonstrated
/// on the previous code: a Claim over an unrelated artifact, produced by
/// a route with no relation to Atlas, authorized an assertion between two
/// unrelated coordinates (§5 R2); and, because a Work could not assert
/// until it had already claimed, an actor that must *report* its own
/// assertion had to mutate an artifact it had already claimed, leaving a
/// permanent digest mismatch in the estate (§5 R1). Ruling 0095 names
/// the lifecycle instead: assert during the work, report the outcome,
/// then Claim the finished report once.
///
/// So the producing action is the Work's current `RunOpened` that is
/// still `RunState::Open` after every journaled event is applied
/// (`wirk_core::Run::apply`, R2 — the same reducer every other reader
/// uses), on a Work that is not itself terminal. A retried, failed,
/// vanished or already-claimed Run is not an action anything is
/// currently being produced by.
///
/// This is accountability under the local execution contract, exactly as
/// 0093/0095 frame it: it attributes the assertion to a real, journaled,
/// currently-executing action of a real Work. It is not an
/// authentication guarantee — the socket contract provides none — and it
/// is not a claim that the assertion is true.
fn current_producing_action(events: &[Event]) -> Result<ProducingAction, Reply> {
    use wirk_core::{RunState, WorkState};

    let work = fold(events);
    match work.state {
        WorkState::Completed | WorkState::Failed | WorkState::Canceled => {
            return Err(err_reply(
                "NoAdmittedProducingAction",
                "this Work is terminal; a completed Work's past Claims are history, not a producing action for a new assertion",
            ));
        }
        _ => {}
    }

    let mut open: Option<ProducingAction> = None;
    let mut any_run = false;
    for event in events {
        let EventKind::RunOpened {
            run,
            waypoint,
            world_hash,
            ..
        } = &event.kind
        else {
            continue;
        };
        any_run = true;
        let mut folded = Run {
            id: run.clone(),
            waypoint: waypoint.clone(),
            attempt: 0,
            world_hash: world_hash.clone(),
            state: RunState::Open,
            kind: Default::default(),
            selection: Default::default(),
            launched: false,
            launch_requested: false,
            launch_argv: Vec::new(),
            launch_attempt: None,
        };
        for later in events {
            folded.apply(later);
        }
        if matches!(folded.state, RunState::Open) {
            open = Some(ProducingAction {
                run: run.clone(),
                waypoint: waypoint.clone(),
                world_hash: world_hash.clone(),
            });
        }
    }
    match open {
        Some(action) => Ok(action),
        None if any_run => Err(err_reply(
            "NoAdmittedProducingAction",
            "every Run this Work opened is terminal (claimed, failed, vanished or superseded by retry); a spent action does not produce a new assertion",
        )),
        None => Err(err_reply(
            "NoAdmittedProducingAction",
            "this Work has never opened a Run; a retrieved Work id alone is not a producing action",
        )),
    }
}

/// `handle_atlas_relate`: admits one evidenced `GovernedBy` relationship.
///
/// P3 W3 correction (ruling 0093, W3-CORRECTION.md item 2): the review's
/// recommended fix — requiring `Access::Write` on the relationship's own
/// source endpoints — is rejected by ruling 0077 (a Read knowledge
/// source must remain usable in a real investigation; an all-Read
/// source set does not by itself prove an unauthorized producer). The
/// actual gap VERDICT.md M2 found is different: `resolve_query_scope`
/// only proves the caller supplied a real `WorkId` it can retrieve —
/// not that this Work has ever *done* anything. `work` is required, and
/// the producer identity is derived from the journal, never from a
/// client-supplied producer string and never from a bare `work:<id>`
/// formatted from retrieval alone.
///
/// P3 W3 second correction (ruling 0095, W3-SECOND-CORRECTION.md item
/// 3): that producer is now the Work's *current* admitted Run and the
/// World it was reserved against — `current_producing_action`, whose own
/// doc records why a historical validated Claim is not it. `payload.run`
/// and `payload.world`, when the caller states them, are checked against
/// that current action rather than believed: a stale, retried, foreign
/// or invented receipt is refused `ProducingActionMismatch`, and a Work
/// whose Runs are all spent is refused `NoAdmittedProducingAction`. A
/// Read-only grant set still asserts perfectly well (0077); the
/// assertion still mutates nothing in any source repository; and it
/// remains an attributed assertion, not settlement, Application or
/// semantic truth.
fn handle_atlas_relate(state: &Arc<WirkdState>, payload: super::AtlasRelatePayload) -> Reply {
    let scope = match resolve_query_scope(state, &Some(payload.work.clone())) {
        Ok(scope) => scope,
        Err(reply) => return reply,
    };
    let events = match journal_for(state, &payload.work) {
        Ok(Some(journal)) => {
            let journal = journal.lock().unwrap_or_else(|poison| poison.into_inner());
            match journal.replay() {
                Ok(events) => events,
                Err(err) => return err_reply("JournalError", &err.to_string()),
            }
        }
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let action = match current_producing_action(&events) {
        Ok(action) => action,
        Err(reply) => return reply,
    };
    if let Some(stated) = &payload.run
        && stated != &action.run.0
    {
        return err_reply(
            "ProducingActionMismatch",
            "the stated run is not this Work's current producing action",
        );
    }
    if let Some(stated) = &payload.world
        && stated != &action.world_hash.0
    {
        return err_reply(
            "ProducingActionMismatch",
            "the stated world is not the World this Work's current producing action was reserved against",
        );
    }
    let kind = match payload.kind.as_str() {
        "governed_by" => wirk_atlas::RelationshipKind::GovernedBy,
        other => return err_reply("BadRequest", &format!("unknown relationship kind {other}")),
    };
    let from = match decode_coordinate(&payload.from) {
        Ok(coordinate) => coordinate,
        Err(detail) => return err_reply("MalformedCoordinate", &detail),
    };
    let to = match decode_coordinate(&payload.to) {
        Ok(coordinate) => coordinate,
        Err(detail) => return err_reply("MalformedCoordinate", &detail),
    };
    let mut evidence = Vec::new();
    for encoded in &payload.evidence {
        match decode_coordinate(encoded) {
            Ok(coordinate) => evidence.push(coordinate),
            Err(detail) => return err_reply("MalformedCoordinate", &detail),
        }
    }
    let mut atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let producer = format!(
        "explicit-admission/v1/work:{}/run:{}/world:{}",
        payload.work.0, action.run.0, action.world_hash.0
    );
    match wirk_atlas::admit_relationship(
        &mut atlas, &scope, None, kind, from, to, evidence, &producer,
    ) {
        Ok(relationship) => ok_reply(json!({
            "id": relationship.id.0,
            "kind": "governed_by",
            "from": encode_coordinate(&relationship.from),
            "to": encode_coordinate(&relationship.to),
            "evidence": relationship.evidence.iter().map(encode_coordinate).collect::<Vec<_>>(),
            "producer": relationship.producer,
            // The exact receipt an actor puts in its own final report —
            // recorded during the work, so the report it later Claims
            // stays byte-identical to what was claimed (ruling 0095).
            "producing_action": {
                "work": payload.work.0,
                "run": action.run.0,
                "waypoint": action.waypoint.0,
                "world": action.world_hash.0,
            },
            "published_at_unix_millis": relationship.published_at_unix_millis,
        })),
        Err(err) => err_reply("AtlasRelationshipError", &err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

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
                parent: None,
                execution_repo: None,
                execution_identity: None,
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

    // ---- P3 native launch attempt admission (the review's N1) --------
    //
    // Every holder below is a real process this test can name: this
    // test's own, or a child it spawned and reaped. No fake stands in
    // for the kernel — "is that process still running" is the whole
    // mechanism, so faking it would pin nothing (0040).

    /// A holder wirkd itself admitted: the running process, with the
    /// start token the kernel reports for it right now.
    fn live_holder() -> AttemptHolder {
        let pid = std::process::id();
        AttemptHolder {
            pid,
            start_token: process_start_token(pid),
        }
    }

    /// A pid that is definitely not running: a child spawned and
    /// reaped, so the kernel has released it. (A recycled pid is
    /// covered separately by the start-token case below, which is what
    /// makes this safe to assert.)
    fn reaped_pid() -> u32 {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    }

    fn attempt(holder: AttemptHolder, destination: &str) -> LaunchAttempt {
        LaunchAttempt {
            holder,
            destination: destination.to_string(),
        }
    }

    #[test]
    fn a_running_process_reads_live_and_a_reaped_one_reads_gone() {
        assert!(matches!(holder_state(&live_holder()), HolderState::Live));
        let gone = AttemptHolder {
            pid: reaped_pid(),
            start_token: Some("1".to_string()),
        };
        assert!(matches!(holder_state(&gone), HolderState::Gone));
    }

    /// Poll `/proc/<pid>/stat` until the kernel reports the state this
    /// test is waiting for. Real states of real processes: nothing here
    /// simulates the kernel. Returns rather than panicking, so a
    /// caller always reaches its own cleanup — a test that leaves a
    /// stopped child behind hangs the whole run.
    fn wait_for_state(pid: u32, want: char) -> bool {
        for _ in 0..500 {
            if process_stat(pid).is_some_and(|(state, _)| state == want) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    fn signal_process(pid: u32, signal: libc::c_int) {
        let pid = libc::pid_t::try_from(pid).expect("a pid fits pid_t");
        // SAFETY: a plain `kill(2)` with a pid this test spawned itself
        // and has not reaped, so the pid still names that process and
        // cannot have been recycled under us.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0, "kill({pid}) failed");
    }

    /// The review's Z1, as a rule. A child this test kills and
    /// deliberately never reaps is *dead* — it cannot drive an agent,
    /// answer Herdr or write a journal — but `/proc/<pid>` still exists
    /// and its start token is unchanged, which is precisely what a
    /// crashed `wirk run` leaves behind under any parent that does not
    /// `wait()`. It must read `Gone`, and it must hold nothing.
    #[test]
    fn a_dead_but_unreaped_holder_is_gone_and_traps_nothing() {
        let mut child = Command::new("true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        let holder = AttemptHolder {
            pid,
            start_token: process_start_token(pid),
        };

        // Everything is read while the corpse is still unreaped, and
        // the child is reaped before a single assertion runs.
        let is_zombie = wait_for_state(pid, 'Z');
        let still_in_proc = std::fs::metadata(format!("/proc/{pid}")).is_ok();
        let token_now = process_start_token(pid);
        let state = holder_state(&holder);
        let admitted = admit_launch_attempt(
            Some(&attempt(holder.clone(), "/run/herdr.sock")),
            &live_holder(),
            "/run/herdr.sock",
        );
        // The kernel says dead; there is nothing left to be uncertain
        // about, so this holds even when no start token was ever read.
        let untokened = holder_state(&AttemptHolder {
            pid,
            start_token: None,
        });
        child.wait().expect("reap");

        assert!(is_zombie, "the child never became a zombie");
        assert!(
            still_in_proc,
            "the whole point of this case: a zombie is still in /proc"
        );
        assert_eq!(
            holder.start_token, token_now,
            "and its start token is unchanged, so the token cannot tell us it died"
        );
        assert!(
            matches!(state, HolderState::Gone),
            "a dead holder is gone, reaped or not"
        );
        assert!(
            admitted.is_ok(),
            "a crashed invocation must not trap a valid Run by going unreaped"
        );
        assert!(matches!(untokened, HolderState::Gone));
    }

    /// The other side of that distinction, and the reason it is a state
    /// and not just "did it exit": a *stopped* process is alive. It can
    /// be continued, and it may still be holding a Herdr pane open, so
    /// it keeps what it holds.
    #[test]
    fn a_stopped_holder_is_still_live_and_keeps_its_run() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let holder = AttemptHolder {
            pid,
            start_token: process_start_token(pid),
        };
        signal_process(pid, libc::SIGSTOP);

        // Read while it is stopped, then put it back and reap it
        // *before* asserting: a `SIGSTOP`ped child that outlives a
        // failing test never exits and the whole run hangs on it.
        let is_stopped = wait_for_state(pid, 'T');
        let state = holder_state(&holder);
        let admitted = admit_launch_attempt(
            Some(&attempt(holder, "/run/herdr.sock")),
            &live_holder(),
            "/run/herdr.sock",
        );
        signal_process(pid, libc::SIGCONT);
        child.kill().expect("kill");
        child.wait().expect("reap");

        assert!(is_stopped, "the child never stopped");
        assert!(matches!(state, HolderState::Live), "stopped is not dead");
        let refusal = admitted.expect_err("a stopped holder still owns its attempt");
        assert!(refusal.contains("still running"), "{refusal}");
    }

    /// The pid-reuse case, which is why the start token exists at all:
    /// this very pid is running, but it is not the process that was
    /// admitted, so the attempt it held is gone.
    #[test]
    fn a_recycled_pid_is_gone_not_live() {
        let recycled = AttemptHolder {
            pid: std::process::id(),
            start_token: Some("this-is-not-the-token-the-kernel-reports".to_string()),
        };
        assert!(matches!(holder_state(&recycled), HolderState::Gone));
    }

    /// A holder wirkd admitted without ever reading a start token, whose
    /// pid is in use now, cannot be shown to be gone — and wirkd will
    /// not replace an owner it cannot show is gone.
    #[test]
    fn a_holder_with_no_start_token_whose_pid_is_in_use_is_unverifiable() {
        let unknown = AttemptHolder {
            pid: std::process::id(),
            start_token: None,
        };
        assert!(matches!(holder_state(&unknown), HolderState::Unverifiable));
    }

    /// L2 (`native-launch-zombie-verify/VERDICT.md` §8): the outer
    /// `None` arm below `holder_state_at`'s `process_stat_at` call —
    /// "`/proc/<pid>` exists, but its `stat` entry cannot be read" —
    /// was unpinned by any test; flipping it to `Gone` left all 711
    /// tests passing. Reached here by a real, non-racy filesystem
    /// failure at the seam `holder_state_at` takes as a parameter: a
    /// directory that exists (so `metadata` succeeds, taking the
    /// `Ok(_)` arm) whose `stat` entry is itself a directory, so
    /// `std::fs::read_to_string` on it fails for real — no live pid,
    /// no exit-and-reap race, nothing stubbed. A `wirk work retry`
    /// remains available regardless (`HolderState::Unverifiable`'s own
    /// doc comment); this test is only about which state is reported.
    #[test]
    fn an_unreadable_stat_after_metadata_succeeds_is_unverifiable_not_gone() {
        let proc_dir = tempfile::tempdir().expect("proc_dir tempdir");
        std::fs::create_dir(proc_dir.path().join("stat")).expect("stat entry is a directory");

        let holder = AttemptHolder {
            pid: 1,
            start_token: Some("123".to_string()),
        };
        assert!(
            matches!(
                holder_state_at(proc_dir.path(), &holder),
                HolderState::Unverifiable
            ),
            "an unreadable stat file must never be reported as Gone — a read \
             failure is not proof of death"
        );
    }

    /// N1 itself, as a rule: a Run whose attempt is held by a process
    /// that is still running admits no second attempt — not a duplicate
    /// invocation, not a recovery. Nothing about this depends on Herdr's
    /// agent-name uniqueness.
    #[test]
    fn a_second_attempt_is_refused_while_its_holder_is_running() {
        let held = attempt(live_holder(), "/run/herdr.sock");
        let other = AttemptHolder {
            pid: std::process::id() + 1,
            start_token: Some("t".to_string()),
        };
        let refusal = admit_launch_attempt(Some(&held), &other, "/run/herdr.sock")
            .expect_err("a live holder owns it");
        assert!(refusal.contains("still running"), "{refusal}");
    }

    /// And the other side of the same rule: nothing is trapped. The
    /// holder's own process *is* the marker, so a holder that died
    /// releases what it held by dying — there is no lease to expire and
    /// no marker to clear by hand.
    #[test]
    fn an_attempt_whose_holder_died_is_superseded_not_stuck() {
        let orphaned = attempt(
            AttemptHolder {
                pid: reaped_pid(),
                start_token: Some("1".to_string()),
            },
            "/run/herdr.sock",
        );
        assert!(
            admit_launch_attempt(Some(&orphaned), &live_holder(), "/run/herdr.sock").is_ok(),
            "a dead holder must never keep a valid Run from being recovered"
        );
    }

    /// The same process asking again is the same owner, not a rival —
    /// a Run this invocation already holds stays holdable across the
    /// several records one launch makes.
    #[test]
    fn the_current_holder_may_re_attempt_its_own_run() {
        let held = attempt(live_holder(), "/run/herdr.sock");
        assert!(admit_launch_attempt(Some(&held), &live_holder(), "/run/herdr.sock").is_ok());
    }

    /// The destination binding: the review's own unexecuted inference
    /// was that two Herdr sessions would not collide on agent name. A
    /// Run's launch is bound to the Herdr it was attempted on, so a
    /// recovery pointed somewhere else is refused *before* it can turn
    /// an uncertain launch into a second real one — and the refusal is
    /// about the destination, not about the holder, so it stands even
    /// when the previous holder is long gone.
    #[test]
    fn a_second_herdr_destination_is_refused_even_when_the_holder_is_gone() {
        let elsewhere = attempt(
            AttemptHolder {
                pid: reaped_pid(),
                start_token: Some("1".to_string()),
            },
            "/run/herdr-one.sock",
        );
        let refusal = admit_launch_attempt(Some(&elsewhere), &live_holder(), "/run/herdr-two.sock")
            .expect_err("a different Herdr cannot see the first one's agent");
        assert!(refusal.contains("herdr-one.sock"), "{refusal}");
        assert!(refusal.contains("herdr-two.sock"), "{refusal}");
    }

    /// A Run nobody has attempted yet — and every journal written
    /// before attempts existed, which folds to exactly the same
    /// `None` — admits the first attempt without any of this.
    #[test]
    fn a_run_with_no_admitted_attempt_admits_the_first_one() {
        assert!(admit_launch_attempt(None, &live_holder(), "/run/herdr.sock").is_ok());
    }

    /// A stale owner goes quiet: after replacement it may not publish a
    /// competing failure, nor keep appending observations to a Run it
    /// no longer drives.
    #[test]
    fn a_replaced_owner_may_not_record_this_runs_outcome() {
        let held = attempt(live_holder(), "/run/herdr.sock");
        let stale = AttemptHolder {
            pid: std::process::id() + 1,
            start_token: Some("t".to_string()),
        };
        for kind in [
            EventKind::RunFailed {
                cause: FailureCause {
                    status: None,
                    request_id: None,
                    at: Timestamp(0),
                    detail: Some("I think it failed".to_string()),
                },
            },
            EventKind::LifecycleObserved {
                status: "idle".to_string(),
                detail: None,
            },
            EventKind::RunVanished,
        ] {
            let refusal = admit_outcome_record(Some(&held), Some(&stale), &kind)
                .expect_err("a replaced owner is refused");
            assert!(refusal.contains("no longer owns it"), "{refusal}");
        }
        assert!(
            admit_outcome_record(Some(&held), Some(&live_holder()), &EventKind::RunVanished)
                .is_ok(),
            "the holder itself still records freely"
        );
    }

    /// Ownership governs the launch outcome, not the journal at large:
    /// a Run that nobody has attempted, and wirkd's own internal
    /// recovery path (no peer at all), are both unaffected.
    #[test]
    fn outcome_ownership_only_applies_where_an_attempt_exists() {
        let held = attempt(live_holder(), "/run/herdr.sock");
        assert!(admit_outcome_record(None, Some(&live_holder()), &EventKind::RunVanished).is_ok());
        assert!(admit_outcome_record(Some(&held), None, &EventKind::RunVanished).is_ok());
        assert!(
            admit_outcome_record(
                Some(&held),
                Some(&AttemptHolder {
                    pid: std::process::id() + 1,
                    start_token: None,
                }),
                &EventKind::WorktreeCreated {
                    repo: "r".to_string(),
                    base_sha: "s".to_string(),
                },
            )
            .is_ok(),
            "materialization is not a launch outcome"
        );
    }
}
