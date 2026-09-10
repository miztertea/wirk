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
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use wirk_core::{
    Access, ActorKind, ActorReviewProof, ActorSelection, ActorWorld, AdmittedEvidence,
    ApplicationProducer, ApplicationRef, ArtifactReceipt, ArtifactRef, ArtifactSpec,
    AssertedJudgement, AssertingAuthor, Assertion, AttemptHolder, Attribution, AuthoredSelection,
    Boundary, ChildProof, Claim, ClaimId, ClaimKind, ClaimRefusal, ClaimVerdict, ConfirmedBy,
    Decision, DeterministicWorld, DischargedRole, Event, EventId, EventKind, EvidenceOutcome,
    EvidenceRef, ExecutionTriple, FailureCause, Finding, FindingId, FindingKind, FindingRecord,
    FindingScope, FindingState, GenerationPoint, Journal, JournalError, JournalReader,
    LaunchAttempt, ObligationRef, OutcomeReceipt, OutputContract, ParentBinding, PeerIdentity,
    ReadySettlement, RelationRoute, RelationStanding, RepositoryBinding, ReviewTarget, Route,
    RouteId, Run, RunId, RunState, Settlement, SettlementAuthority, SettlementCheck,
    SettlementClass, SourceBasis, Timestamp, UnreadFields, WaypointDefinition, WaypointId,
    WaypointKind, Work, WorkId, WorkState, World, WorldHash, ancestor_chain, find_definition,
    finding_kind_name, first_dfs_leaf, flatten_leaves, fold, load_route, obligation_basis,
    validate_claim,
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
    /// How the derived Findings index stood at this daemon's **own last
    /// reconciliation attempt** (ruling 0116's first carried limit).
    ///
    /// In-memory on purpose, and correct across restart without being
    /// persisted: the only writer of `atlas/findings.ndjson` is this
    /// daemon, `run` reconciles from every journal in the estate before
    /// the listener accepts a single connection, and every mutating verb
    /// reconciles again after its own append. So a crash between a
    /// journaled event and its index row is re-detected — and repaired,
    /// or reported as unrepaired — by the next start, and no client ever
    /// reads a value this process did not itself measure. Persisting it
    /// would be worse than useless: the one failure this exists to
    /// report is an unwritable `atlas/` directory, which is exactly
    /// where a persisted health file could not be written either.
    ///
    /// It is deliberately **not** a fresh estate scan on read. Queries
    /// stay pure (W3-CORRECTION.md item 3, "a query must never create
    /// Atlas state"); they report the health this daemon last observed,
    /// and say so.
    index_health: Mutex<IndexHealth>,
    /// The order every index observation is ranked by, and nothing else:
    /// a monotonic per-daemon ticket taken where a reconciliation's
    /// **walk** begins, so that "older" and "newer" mean *when this
    /// attempt read the estate* rather than when its record happened to
    /// arrive.
    ///
    /// Ruling 0125 closed "an older outcome cannot overwrite a newer
    /// publication" at the publication boundary, by making `{append,
    /// record}` one critical section. The same class survived one step
    /// earlier, at the scan boundary, because an older walk needs no
    /// publication at all to record: once somebody else has indexed
    /// every row it was carrying, its `append_finding_rows` returns
    /// `Ok(0)` **without writing a byte**, and it then recorded
    /// `Synchronized` over a newer, known, unrepaired failure
    /// (`index-health-reverify/VERDICT.md`, reproduced four times on a
    /// real daemon under a real kernel `EACCES`).
    ///
    /// It is a counter on this daemon's own state, never a process
    /// global: one estate's ordering is not another's, and the tests
    /// that pin this run several daemons at once. It is an order, not a
    /// duration, a deadline or a budget — nothing here is paced or
    /// decided by time (ruling 0044 D134).
    index_observations: AtomicU64,
}

/// What the last reconciliation attempt found, in the terms a caller
/// needs: whether the derived index currently projects every journaled
/// row, and — when it does not — enough to act on without disclosing
/// what the caller may not see.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexProjection {
    /// No reconciliation has run in this process yet. The honest state
    /// before `run`'s startup sweep, and never reported as clean: an
    /// index nobody has checked is not an index known to be complete.
    Unreconciled,
    /// Every row this estate's journals support is in the file.
    Synchronized,
    /// The rows are visible to a fresh reader — the atomic rename
    /// succeeded — but the containing directory's `fsync` did not, so a
    /// power loss could still lose the directory entry. `AtlasStore`'s
    /// own `DurabilityUncertain` window, reported as itself rather than
    /// flattened into "behind" (a different, false fact) or into
    /// "synchronized" (the silence this closes).
    DurabilityUnconfirmed { detail: String },
    /// The index is missing rows the journals hold. `pending` is how
    /// many, or `None` when the index could not be read at all and the
    /// count is genuinely unknown.
    Behind {
        pending: Option<usize>,
        detail: String,
    },
}

/// What one attempt established about the estate's **atlas directory**
/// being on disk — the other half of an index write, and the half a
/// reader cannot check afterwards.
///
/// Every path that writes the index writes a temporary, `fsync`s it,
/// renames it over the real name and then `fsync`s the containing
/// directory (`AtlasStore::rewrite_rows`); the retirement renames
/// preserved copies and `fsync`s the same directory
/// (`retire_preserved_unreadable_indexes`). That last `fsync` is the
/// only thing anywhere that turns "a fresh reader can see it" into "a
/// machine that loses power still sees it", and it is the only thing
/// that can retire an earlier one's failure: a directory `fsync` covers
/// the entries pending in it, not just this call's.
///
/// So an attempt reports which of three things it did, and nothing
/// infers it from the projection: the projection says what the *index*
/// holds, and these three say what the *directory* is known to hold.
#[derive(Debug, Clone)]
enum DirectoryDurability {
    /// This attempt `fsync`ed the atlas directory and it returned
    /// success — so every rename and rewrite that was visible before it
    /// is on disk, including one an earlier attempt could not confirm.
    Confirmed,
    /// This attempt's own directory `fsync` failed, after its rename had
    /// already made the bytes visible to a fresh reader.
    Uncertain(String),
    /// This attempt did not write to the atlas directory at all, so it
    /// established nothing either way — a sweep that found nothing to
    /// append writes not one byte, and must not be read as confirming a
    /// directory it never opened.
    Unestablished,
}

/// What the observation that recorded a health record saw of the index
/// **file** — the other half of "is this projection complete", and the
/// half a later read can compare its own open against.
///
/// Ruling 0137: an index file that is not there answers a read with no
/// rows, and that is a truthful answer for an estate that never wrote
/// one. It is not a truthful answer beside a health record formed over a
/// file that *was* there, because then what the file held is exactly what
/// this read cannot establish. One fact, taken from what the recording
/// attempt itself established about the file — its own append's read or
/// its own publication — resolved against the same listing of `atlas/`
/// the preserved-copy question is already answered by (`recorded_backing`).
/// No second store, no count, no timer, and nothing remembered across a
/// restart that the estate itself does not still show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordedBacking {
    /// No observation has recorded here yet, or the one that did could
    /// not list the estate's atlas directory. Never evidence of anything:
    /// a read that finds no file is not entitled to call it a loss.
    Unknown,
    /// The recording observation found an index file: its own append
    /// read one or published one, or the listing at the end of it saw
    /// one.
    Present,
    /// The recording observation found none: its own append neither read
    /// nor wrote a file, and the listing did not see one either. The
    /// estate that has never published a row.
    Absent,
}

#[derive(Debug, Clone)]
struct IndexHealth {
    projection: IndexProjection,
    /// When this projection state was first entered (not the last time
    /// it was re-observed) — so a caller can tell a window that just
    /// opened from one that has been open all along.
    since: Timestamp,
    /// When a reconciliation was last attempted at all.
    last_attempt: Option<Timestamp>,
    /// Which observation this whole record came from — the ticket the
    /// recording attempt took before its walk. Everything above belongs
    /// to that one observation, including its timestamps: an attempt
    /// whose record is discarded contributes nothing at all, rather than
    /// leaving its clock behind on somebody else's outcome.
    ///
    /// `0` is "no observation has recorded here yet", which is exactly
    /// `Unreconciled`, and is older than every real ticket.
    observation: u64,
    /// Copies of an earlier standing index that could not be parsed,
    /// kept aside rather than replaced away (`preserve_unreadable_index`).
    ///
    /// Why this is on the *health* record and not only in a detail
    /// string: what those bytes held is unknowable, so no walk of the
    /// estate can establish that the index is whole again. A rebuild
    /// that proceeded over unparsable lines therefore leaves a residual
    /// uncertainty that the next ordinary reconciliation must **not**
    /// clear — its basis is the shortened file that rebuild just wrote,
    /// so it is comparing like with like and learns nothing (ruling
    /// 0130). Recorded from the estate itself on every attempt, so it
    /// survives a restart, and cleared only when an administrator says
    /// they have reviewed the preserved bytes
    /// (`--retire-preserved-index`).
    preserved: PreservedIndexCopies,
    /// Whether the observation that recorded this found an index file on
    /// disk — what its own append established about the file, resolved
    /// against the listing of `atlas/` taken at the end of it
    /// (`recorded_backing`). Read by a later *query* to tell "this
    /// estate holds no findings" from "the file this record was formed
    /// over is gone" — the two states ruling 0137 found collapsed into
    /// one silent `complete: true`.
    index_backing: RecordedBacking,
    /// A write or rename this daemon already reported as landed whose
    /// containing directory's `fsync` did not succeed, as the failing
    /// call described it.
    ///
    /// Why this is remembered rather than re-derived: unlike
    /// `preserved`, no listing of the estate can answer it. The rename
    /// *is* visible — that is what makes the window a durability
    /// question and not a missing-row question — so every later read of
    /// the estate sees a healthy directory and learns nothing about
    /// whether its entry survives a power cut. The one thing that
    /// answers it is a **later successful `fsync` of that same
    /// directory**, which is exactly what `DirectoryDurability` reports
    /// (ruling 0130's "no one-call warning that is automatically
    /// laundered into complete": before this, the retirement's own
    /// reconciliation cleared the window it had just opened, because a
    /// sweep with nothing to append writes nothing and so certifies a
    /// directory it never touched).
    ///
    /// Not a latch and not a clock: it is cleared by an ordinary
    /// successful write of the index, an administrative `--rebuild`, a
    /// retirement that renames a copy, or a restart — every one of them
    /// an `fsync` of the atlas directory this product already makes.
    unconfirmed_directory: Option<String>,
}

/// What one listing of the estate's atlas directory established about
/// preserved copies of an unreadable index.
///
/// **Two different facts, deliberately not flattened into one list of
/// strings.** Names that were listed, and — when the directory could not
/// be listed at all — why not. Flattened, the listing's own error text
/// went into `preserved_index_copies` as though it were a file name and
/// was then *counted*: an administrator was told "1 preserved copy(ies)
/// … are held" and given a name that was a sentence, and every scoped
/// requester was told that bytes "are preserved in this estate" and that
/// the wait was on someone retiring them — all of it manufactured out of
/// a permission problem on a directory, with no such file anywhere and
/// the retirement they were pointed at returning `EACCES`
/// (`index-basis-recovery-verify/raw/46`).
///
/// Both facts stop this estate certifying its projection complete: an
/// estate whose atlas directory cannot be listed has not been checked.
/// Only one of them is a claim about bytes, and only that one may be
/// said.
#[derive(Debug, Clone, Default)]
struct PreservedIndexCopies {
    /// Copies actually listed. Only ever file names.
    names: Vec<String>,
    /// Why the question could not be answered, when it could not. The
    /// underlying cause, for the administrator who can act on it.
    unknown: Option<String>,
}

impl PreservedIndexCopies {
    /// Whether this estate may not certify its projection complete.
    /// Neither held copies nor an unanswerable question authorizes it.
    fn qualifies(&self) -> bool {
        !self.names.is_empty() || self.unknown.is_some()
    }

    /// The administrative note, which says only what was established.
    fn note(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.names.is_empty() {
            parts.push(format!(
                "{} preserved copy(ies) of an index whose lines did not all parse are held in the estate's atlas directory ({}); what those lines held cannot be established by any walk of the estate, so this projection's completeness is unknown until an administrator reviews them and runs `wirk atlas findings --admin --retire-preserved-index`",
                self.names.len(),
                self.names.join("; ")
            ));
        }
        if let Some(unknown) = &self.unknown {
            parts.push(format!(
                "the estate's atlas directory could not be listed, so whether it holds preserved copies of an index whose lines did not all parse is unknown and this projection's completeness cannot be established either way: {unknown}"
            ));
        }
        parts.join("; ")
    }
}

impl IndexHealth {
    fn unreconciled() -> Self {
        Self {
            projection: IndexProjection::Unreconciled,
            since: now_ts(),
            last_attempt: None,
            observation: 0,
            preserved: PreservedIndexCopies::default(),
            index_backing: RecordedBacking::Unknown,
            unconfirmed_directory: None,
        }
    }

    /// True only for `Synchronized`. `Unreconciled` and
    /// `DurabilityUnconfirmed` are both honestly short of it: the first
    /// because nothing has checked, the second because the file's
    /// directory entry is not confirmed on disk.
    fn complete(&self) -> bool {
        matches!(self.projection, IndexProjection::Synchronized)
    }
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
    // P3 execution-recovery item 3: canonicalized *before* the socket
    // path is derived and written, not after. A relative `--estate`
    // otherwise leaves `wirk_dir`/`socket_path` relative to whatever
    // directory this process happened to start in, and that relative
    // path is what `write_pointer` durably records in
    // `.wirk/wirkd.json` — every later client resolves it against its
    // *own* cwd instead (an actor's worktree, not the estate root),
    // failing with `ENOENT` (MECHANISM-REPORT.md qualification 4, third
    // bullet). `wirk_dir`'s directory already exists (`create_dir_all`
    // just above), so canonicalizing this early is exactly as valid as
    // the canonicalization this function already performed later for
    // the same root — reused here, once, at the point that actually
    // matters.
    let estate_root = std::fs::canonicalize(&estate_root).map_err(|source| WirkdError::Bind {
        socket: wirk_dir.join("wirkd.sock"),
        source,
    })?;
    let wirk_dir = estate_root.join(".wirk");
    let socket_path = wirk_dir.join("wirkd.sock");
    let listener = bind_socket(&socket_path).map_err(|source| WirkdError::Bind {
        socket: socket_path.clone(),
        source,
    })?;
    write_pointer(&estate_root, &socket_path, std::process::id())?;
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
        index_health: Mutex::new(IndexHealth::unreconciled()),
        index_observations: AtomicU64::new(0),
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

    // W-B (§6): every eligible journal, terminal included, before this
    // listener starts accepting connections — a crash between a
    // completed Work's last event and its settlement, or between a
    // journaled `FindingSettled`/`FindingAsserted`/`FindingApplied` and
    // its index row, is repaired here rather than left missing
    // indefinitely (the terminal design's own defect: a
    // non-terminal-or-recently-terminal filter would have preserved it).
    // Settlement runs first (canonical), the index reconciliation second,
    // so a settlement this very sweep just minted is indexed in the same
    // pass.
    settle_ready_findings(&state);
    reconcile_findings_index(&state);

    // W-C1 (BUILD.md §5.1): a crash between a projection's temp write
    // and its rename leaves a `.tmp-` file that nothing can ever
    // reference — the reference names the *renamed* path, so an
    // un-renamed temp is unreachable by construction. Swept here, at
    // startup, before the listener accepts a connection, which is the
    // one moment this daemon is provably the only writer of these
    // directories. Nothing else is swept: a *renamed* projection file
    // that no event names is left exactly where it is, permanently. Age
    // is not evidence of orphanhood (ruling 0124: no mtime heuristic
    // deletes an unreferenced projection artifact), and harmless residue
    // is cheaper than a wrong deletion.
    sweep_projection_temporaries(&state);

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
    let holder = peer_holder(&stream);

    // W-B (§2.3, A2): `UnixStream::peer_cred()` is attribution, never
    // authentication — the same OS uid runs both an honest human
    // terminal and an actor's shell (probe A/B, `loop-b-prepare-correct`).
    // Recorded on every `Assertion`/`Attribution::Asserted`, never a gate;
    // an unreadable credential (a platform without `SO_PEERCRED`, in
    // practice never this estate's own Linux/macOS boxes) reads as the
    // unprivileged `0/0` rather than failing the request.
    let peer = peer_credentials(&stream).unwrap_or(PeerIdentity { uid: 0, gid: 0 });
    let outcome = dispatch(&request, state, holder.as_ref(), peer);

    // P3 execution-recovery item 3: for `stop`, the pointer/socket
    // cleanup runs *before* the reply is sent, not after — the prior
    // order let `wirk wirkd stop` return to its caller while this
    // thread still owned an unremoved `.wirk/wirkd.sock`, so an
    // immediate `start` right after a truthful "stopped" reply could
    // still find the path occupied and fail `EADDRINUSE`
    // (MECHANISM-REPORT.md qualification 4, second bullet). Unlinking
    // the socket path here does not disturb this already-`accept`ed
    // connection — a Unix domain socket's peer is the open file
    // descriptor, not the pathname — so the reply below still reaches
    // this same client. This makes the reply truthful (nothing is
    // reachable at this estate's socket by the time it is sent) rather
    // than adding an arbitrary sleep anywhere.
    if matches!(outcome, Outcome::Stop(_)) {
        remove_owned_containers(&state.estate_root);
        remove_pointer_and_socket(&state.estate_root, socket_path);
    }

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
        // Pure discovery: read the journal that is there, create
        // nothing, and read one an operator left read-only
        // (`discovery_events`). The mutation this sweep decides on
        // still goes through the one write path below.
        let Some(events) = discovery_events(&dir) else {
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
        // Pure discovery: read the journal that is there, create
        // nothing, and read one an operator left read-only
        // (`discovery_events`). The mutation this sweep decides on
        // still goes through the one write path below.
        let Some(events) = discovery_events(&dir) else {
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
            let journal = lock_journal(&journal);
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
                // A Deterministic executor writes into its own `cwd`,
                // which is a real checkout it holds Write on: it has no
                // reason to reach for the managed area (ruling 0145).
                outputs: Default::default(),
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

fn dispatch(
    request: &Request,
    state: &Arc<WirkdState>,
    holder: Option<&AttemptHolder>,
    peer: PeerIdentity,
) -> Outcome {
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
            Ok(payload) => Outcome::Reply(handle_record(state, payload, holder)),
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
        Verb::FindingRaise => {
            match serde_json::from_value::<super::FindingRaisePayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_finding_raise(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::FindingAssert => {
            match serde_json::from_value::<super::FindingAssertPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_finding_assert(state, payload, peer)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::FindingSettle => {
            match serde_json::from_value::<super::FindingSettlePayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_finding_settle(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::FindingApplied => {
            match serde_json::from_value::<super::FindingAppliedPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_finding_applied(state, payload, peer)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::FindingList => {
            match serde_json::from_value::<super::FindingListPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_finding_list(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::AtlasFindings => {
            match serde_json::from_value::<super::AtlasFindingsPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_atlas_findings(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::WorkObligations => {
            match serde_json::from_value::<super::WorkObligationsPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_work_obligations(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::WorldExpand => {
            match serde_json::from_value::<super::WorldExpandPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_world_expand(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::WorldShow => {
            match serde_json::from_value::<super::WorldShowPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_world_show(state, payload)),
                Err(err) => Outcome::Reply(err_reply("BadRequest", &err.to_string())),
            }
        }
        Verb::RunOutputs => {
            match serde_json::from_value::<super::RunOutputsPayload>(request.payload.clone()) {
                Ok(payload) => Outcome::Reply(handle_run_outputs(state, payload)),
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

/// W-B (§2.3, A2): the peer's uid/gid on this Unix domain socket —
/// attribution, never authentication (the same OS uid runs both an
/// honest human terminal and an actor's shell). `std::os::unix::net::
/// UnixStream::peer_cred()` is gated behind the unstable
/// `peer_credentials_unix_socket` feature on this toolchain (checked
/// against 1.98.1, corrected from `loop-b-prepare-correct/HANDOFF.md`'s
/// own "stdlib, R3" citation) — R3 fails, so this falls back to R5:
/// `getsockopt(SOL_SOCKET, SO_PEERCRED)` through the already-installed
/// `libc` dependency (`ChildExecutor`'s own `PR_SET_PDEATHSIG` use, same
/// crate, same discipline), exactly the mechanism the stdlib feature
/// itself wraps on Linux. `None` on any platform or kernel that refuses
/// the call — the caller reads that as the unprivileged `0/0`, never a
/// request failure.
fn peer_credentials(stream: &UnixStream) -> Option<PeerIdentity> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(PeerIdentity {
            uid: cred.uid,
            gid: cred.gid,
        })
    } else {
        None
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
    // The launch review's F-C, applied to `status`'s own sibling: this
    // streams raw journal events, so it reaches strictly more than
    // `status` does and cannot be answered unscoped either. Admitted or
    // refused whole — a partially redacted `Event` is not an `Event`,
    // and every consumer of this stream folds it.
    if !payload.admin {
        let Some(requester_id) = &payload.requester else {
            write_one_reply(
                &stream,
                &err_reply(
                    "BadRequest",
                    "a non-administrative watch requires --requesting-work",
                ),
            );
            return;
        };
        let Some(requester_events) = replay_events(state, requester_id) else {
            write_one_reply(&stream, &err_reply("NotFound", "no such requesting work"));
            return;
        };
        let requester = fold(&requester_events);
        let lineage = lineage_of(state, &requester, &requester_events);
        let admitted = lineage.contains(&work_id)
            && fold_work(state, &work_id).is_some_and(|work| {
                work.repositories
                    .iter()
                    .all(|binding| requester_grants_alias(&requester, &binding.name))
            });
        if !admitted {
            // One answer for "not on your lineage" and "not covered by
            // your bindings", naming neither: the same non-disclosure
            // discipline every other refusal here follows.
            write_one_reply(
                &stream,
                &err_reply(
                    "InadmissibleEvidence",
                    "the named work's journal is not admitted to the requesting work",
                ),
            );
            return;
        }
    }
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

    // The scoped stream says, on the wire and before its first `Event`
    // line, which scope this daemon actually applied (the integration
    // review's V-5). `status` already carried that fact in its own
    // reply `scope` field; `watch` had nowhere to carry it, so a client
    // asking a daemon that predates the gate for a narrow stream got
    // the whole raw journal and no way to tell. This is the same
    // `Reply::Ok` envelope every other verb answers in (R2), written
    // once, ahead of everything — never a warning appended after the
    // content it was supposed to govern. It names only the applied
    // scope and the Work the caller already named: no journal content,
    // no count, no requester identity.
    //
    // Only the scoped stream carries it. An `admin` watch is
    // byte-identical to what it always was, so the ordinary operator
    // pane and every existing consumer of it are unchanged, and an
    // older client can still read this daemon's administrative stream.
    if !payload.admin && write_scope_ack(&stream, &work_id).is_err() {
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    let existing = {
        let journal = lock_journal(&journal);
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

/// The scoped `watch` stream's opening line: one ordinary `Reply::Ok`
/// naming the scope this daemon applied, written before any `Event`
/// line and only when the request named a scope
/// (`handle_watch_connection`'s own doc). `wirkd::client::watch`
/// requires it for a scoped request and refuses the stream without it,
/// which is what makes a silently-unscoped answer from an older daemon
/// impossible to consume as though it were scoped. Carries the applied
/// scope and the Work id the caller itself sent, and nothing else.
fn write_scope_ack(stream: &UnixStream, work_id: &WorkId) -> io::Result<()> {
    let reply = ok_reply(json!({"scope": "requester", "work_id": work_id.0}));
    let mut bytes = serde_json::to_vec(&reply).expect("Reply always serializes");
    bytes.push(b'\n');
    let mut writer = stream;
    writer.write_all(&bytes)?;
    writer.flush()
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
                // Nor an orientation request: there is no authored Route
                // here to have written one, and a Deterministic Waypoint
                // could not carry one anyway (`validate_tree`).
                orient: None,
                // The ad hoc, Route-less single-Waypoint shape declares
                // no verification obligation: there is no authored Route
                // edition here for a policy to have admitted.
                verifies: None,
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
                if let Err(message) = validate_route_capacities(&defs) {
                    return err_reply("BadRequest", &message);
                }
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
                    // P3 execution-recovery item 1: this Deterministic
                    // World's `cwd` must be this Work's own worktree,
                    // never the caller's `--repo-path` checkout, which
                    // every other Work using that same `--repo-path`
                    // shares. Left as the shared checkout, a Deterministic
                    // executor's own untracked outputs (`DeterministicWorld`'s
                    // doc: "same worktree_path as ActorWorld") land in a
                    // directory another Work's later Claim never wrote to
                    // but is validated against, and an auto-advanced Actor
                    // Waypoint on this same Work inherits the shared path
                    // while `wirk run` computes `<estate>/worktrees/<work>`
                    // and refuses to reattach (native-learning-use
                    // MECHANISM-REPORT.md qualifications 1 and 3). Reuses
                    // the exact worktree-establishment call and computed
                    // path Actor Worlds use (`executor.rs`'s own Step 2,
                    // `<estate>/worktrees/<work_id>`) — this Work's later
                    // Actor Waypoint (if any) then finds the worktree
                    // already materialized and `wirk run`'s own reuse arm
                    // (`worktree_add`'s "path exists on disk" case) takes
                    // it over unchanged, rather than a second module
                    // reinventing worktree creation.
                    let worktree_path = state.estate_root.join("worktrees").join(&work_id.0);
                    if let Err(err) = wirk_herdr::git::worktree_add(
                        Path::new(&repo_path),
                        &worktree_path,
                        &branch,
                        &verified,
                    ) {
                        return err_reply("GitError", &err.to_string());
                    }
                    (
                        verified.clone(),
                        SourceBasis::Git { base: verified },
                        worktree_path,
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
                // W-B target binding: freeze the declared review
                // selectors here, before the review can run.
                review_targets: freeze_review_targets(state, &payload.repositories, &first_def),
                // W-C1: the reference is filled in below, once the
                // Work's own directory exists and the projection file
                // has been written into it durably. Assembling here,
                // before the World is built, keeps the whole of it
                // outside any journal guard — this Work has no journal
                // yet, and no other Work's is touched.
                evidence: None,
            })
        }
        WaypointKind::Actor => {
            // W-C1 (BUILD.md §3.3), the refusal aimed where `Unknown` is
            // minted: this bare arm reserves an Actor World with
            // `SourceBasis::Unknown`, and a World whose source inspection
            // contract is unrecorded may not carry a stage projection —
            // the projection's coordinates would name generations no
            // recorded basis binds. Refused *before* anything is
            // journaled, naming the Waypoint and the submit shape that
            // works. The positive control is the sibling arm above:
            // `--kind actor --repo-path <p>` needs no `--source-basis` at
            // all and resolves a Git basis from the checkout.
            if first_def.orient.is_some() {
                return err_reply(
                    "UnsupportedAssembly",
                    &format!(
                        "waypoint {} declares an orientation request, which needs a recorded \
                         source basis: submit it with --kind actor --repo-path <checkout>",
                        first_def.id.0
                    ),
                );
            }
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
                // The ad hoc, unknown-basis Actor shape has no admitted
                // source basis to freeze a review target against, so a
                // review obligation here can never discharge — the same
                // fail-closed outcome an unresolvable selector reaches.
                review_targets: freeze_review_targets(state, &payload.repositories, &first_def),
                // W-C1 (BUILD.md §3.3): this is the one writer that mints
                // `SourceBasis::Unknown` for an Actor World, and a World
                // may not carry a projection while its source inspection
                // contract is unrecorded — so this arm refuses an
                // orienting Waypoint outright, above, rather than
                // reserving one whose evidence could never be trusted.
                evidence: None,
            })
        }
        // `waypoint_id` is `all_waypoints[0]`, drawn from `flatten_leaves`
        // (§3.1) — it can never resolve to a `Container` definition.
        WaypointKind::Container => {
            unreachable!("the flattened waypoint sequence names only executable leaves")
        }
    };
    // W-C1: assemble this Work's first stage projection here — before
    // its journal exists, so no guard of any kind is held, and no other
    // Work's journal is read. The Atlas publication revision is
    // re-checked inside `prepared_without_journal`; the journal half of
    // BUILD.md §4.6's re-check is vacuous at submit because there is no
    // journal to have moved.
    let mut world = world;
    let prepared = prepared_without_journal(
        state,
        &payload.repositories,
        &first_def,
        &route_edition_of(&waypoint_defs),
    );

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
    let mut journal = lock_journal(&journal);

    // W-C1 (BUILD.md §5.1): the file is serialized, fsynced and renamed
    // into place **before** the event that names it exists. A crash
    // between the two leaves a projection no event references, which
    // nothing reads and nothing deletes; the reverse order would leave a
    // journaled reference to a file that never existed.
    if let Some(prepared) = &prepared {
        match prepared.commit(state, &work_id) {
            Ok(reference) => {
                if let World::Actor(actor) = &mut world {
                    actor.evidence = Some(Box::new(reference));
                }
            }
            Err((code, detail)) => return err_reply(code, &detail),
        }
    }
    let world_hash = WorldHash::of(&world);

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
    let mut journal = lock_journal(&journal);
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
            // W-B: all four join this arm (HANDOFF.md §8) — the same
            // defense-in-depth `probe B2` executed for every other
            // server-owned transition. `FindingRaised` also has its own
            // dedicated, triple-checked verb (`handle_finding_raise`);
            // `FindingSettled`/`FindingAsserted`/`FindingApplied` have
            // no client-callable producer at all outside their own
            // verbs, so a raw `record` can never mint any of the four.
            | EventKind::FindingRaised { .. }
            | EventKind::FindingSettled { .. }
            | EventKind::FindingAsserted { .. }
            | EventKind::FindingApplied { .. }
            // W-C3: a projection revision is minted only by
            // `world expand`, which assembles it, writes it durably and
            // appends this under the guard it read the parent under. A
            // raw `record` could otherwise hand a Run a reference to a
            // file it never wrote, or a parent that is not the chain's
            // tail.
            | EventKind::ProjectionExpanded { .. }
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
    let mut journal = lock_journal(&journal);
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
    // P3 native closeout item 1a (root qualification 1). This guard's
    // three conditions are all kept — nothing here admits a write that
    // was refused before — but they are no longer answered with one
    // sentence. They are genuinely different facts, and the caller's
    // correct response differs for each:
    //
    // * a *superseded* Run: a newer Run exists for this Waypoint, so
    //   this record belongs to a Run that is no longer the Waypoint's.
    //   Folding it would attribute an old attempt's observation to the
    //   current one. Refused, and the superseding Run is named so the
    //   driver learns it was replaced rather than guessing. The event is
    //   never re-aimed at the new Run to make it land.
    // * a *settled* Run (or Work): the Run this record names already
    //   reached its own outcome. This is the condition the live
    //   `run_verb` retry failures actually met — the actor's validated
    //   `Done` Claim landing while its driver's `LifecycleObserved` was
    //   in flight — and it is benign: the outcome stands, and the
    //   observer stops observing. Its own code lets a driver read that
    //   without parsing prose.
    //
    // Order matters: a retried Run is both superseded *and* failed, and
    // supersession is the more specific fact, so it is answered first.
    let work_state = fold(&events).state;
    let current_for_waypoint = latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0);
    if current_for_waypoint.as_ref() != Some(run_id) {
        return err_reply(
            "InvalidTransition",
            &match &current_for_waypoint {
                Some(current) => format!(
                    "record names Run {}, but Waypoint {} has since opened Run {}: a superseded \
                     Run's record is never folded into the current one",
                    run_id.0, run.waypoint.0, current.0
                ),
                None => format!(
                    "record names Run {}, which is not the current Run of Waypoint {}",
                    run_id.0, run.waypoint.0
                ),
            },
        );
    }
    if work_state.is_terminal() || !matches!(run.state, RunState::Open) {
        return err_reply(
            "RunSettled",
            &format!(
                "Run {} has already settled (run {}, work {}): a record made after a Run reached \
                 its own outcome is not folded, and that outcome stands",
                run_id.0,
                run_state_name(&run.state),
                work_state_name(work_state),
            ),
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
        | EventKind::ChildWorkSpawned { .. }
        | EventKind::FindingRaised { .. }
        | EventKind::FindingSettled { .. }
        | EventKind::FindingAsserted { .. }
        | EventKind::FindingApplied { .. }
        | EventKind::ProjectionExpanded { .. } => unreachable!(),
    };
    let event = new_event(&payload.work_id, Some(run_id.clone()), kind);
    if let Err(err) = append_event(state, &mut journal, &payload.work_id, &event) {
        return err_reply("JournalError", &err.to_string());
    }
    ok_reply(json!({}))
}

/// Whether this claimed artifact is addressed in the Run's own checkout
/// (ruling 0145). Every worktree escape, existence, containment and
/// boundary-diff check below applies to these and only these: a managed
/// output is not in any repository, so there is nothing for those checks
/// to inspect and — critically — nothing for a `Read` binding's "refuses
/// any change at all" rule to see. That rule is not relaxed anywhere;
/// the managed route simply never puts a byte inside the checkout.
fn is_worktree_artifact(artifact: &ArtifactRef) -> bool {
    matches!(artifact.store, wirk_core::ArtifactStore::Worktree)
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

/// W-B (§6): a thin wrapper so `settle_ready` runs only *after*
/// `handle_claim_inner`'s own journal lock is fully released — a
/// Validated Done Claim can be exactly the `deterministic-verified`
/// trigger a `VerifiedOutcome` finding's evidence already names, and
/// `close_cascade`'s own `StageClosed` (fired from auto-advance below)
/// can be the `child-investigation-confirmed` trigger — `settle_ready`
/// re-locks the same Work's journal internally, which would deadlock if
/// called while `handle_claim_inner` still held it.
fn handle_claim(state: &Arc<WirkdState>, payload: ClaimPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let reply = handle_claim_inner(state, payload);
    if let Err(err) = settle_ready(state, &work_id, false) {
        eprintln!(
            "wirkd: settlement evaluation after claim failed for {}: {err}",
            work_id.0
        );
    }
    reply
}

fn handle_claim_inner(state: &Arc<WirkdState>, payload: ClaimPayload) -> Reply {
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
    let mut journal = lock_journal(&journal);

    let events = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };

    let claim_id = ClaimId(mint_id("claim"));
    // Ruling 0145: two kinds of claimed artifact, kept apart from the
    // first line of this handler and never merged again. A `Worktree`
    // ref carries the actor's own path and goes through every existing
    // escape/existence/containment/diff check unchanged; a `WorkOutputs`
    // ref carries no path at all and is addressed by name against this
    // Work's own managed area. `validate_claim`'s required-output check
    // reads `name` alone, so both satisfy an output contract the same
    // way — which is the whole point — while nothing about the Read
    // binding's "refuses any change at all" rule sees a managed output,
    // because a managed output is not in any repository to change.
    let artifacts: Vec<ArtifactRef> = payload
        .artifacts
        .iter()
        .map(|(name, path)| ArtifactRef::worktree(name.clone(), path.clone()))
        .chain(
            payload
                .outputs
                .iter()
                .map(|name| ArtifactRef::managed(name.clone())),
        )
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
    // Ruling 0145, before anything else looks at this Claim: a managed
    // output whose *address* is unusable is refused with the rule it
    // broke, by name. Ahead of `validate_claim` deliberately — a
    // misspelled or malformed `--output` would otherwise surface as the
    // required output it failed to satisfy ("MissingArtifact
    // report.md"), which names the wrong file and tells the actor
    // nothing about what it actually got wrong.
    let managed_address_refusal = claim
        .artifacts
        .iter()
        .filter(|a| !is_worktree_artifact(a))
        .find_map(|artifact| {
            if let Err(err) = wirk_core::outputs::check_output_name(&artifact.name) {
                return Some(ClaimRefusal::OutOfBoundary(format!(
                    "declared output `{}` cannot address a managed output: {}",
                    artifact.name,
                    err.detail()
                )));
            }
            // The Route's own output contract bounds the namespace: a
            // managed output exists because a Waypoint declared it, so a
            // name this Waypoint never declared has no derived address
            // and is refused rather than invented. This is also what
            // keeps the area from becoming a general filesystem.
            if !waypoint
                .declared_outputs
                .iter()
                .any(|spec| spec.name == artifact.name)
            {
                return Some(ClaimRefusal::OutOfBoundary(format!(
                    "`{}` is not a declared output of this Waypoint, so this Work owns no \
                     managed output by that name",
                    artifact.name
                )));
            }
            None
        });

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
    if let Some(refusal) = managed_address_refusal {
        verdict = ClaimVerdict::Refused(refusal);
    }

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
        // Ruling 0145: only a *checkout* artifact needs a worktree to
        // inspect. A managed output lives under `works/<work>/outputs/`
        // and is unaffected by materialization, so an unmaterialized
        // Run's Question naming one is not sent down this refusal.
        if !binding.materialized && claim.artifacts.iter().any(is_worktree_artifact) {
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
                .filter(|a| is_worktree_artifact(a))
                .find(|a| artifact_join_escapes(&worktree_path, &a.path))
            {
                verdict = ClaimVerdict::Refused(ClaimRefusal::OutOfBoundary(escaping.path.clone()));
            }

            if matches!(verdict, ClaimVerdict::Validated) {
                for artifact in claim.artifacts.iter().filter(|a| is_worktree_artifact(a)) {
                    if !worktree_path.join(&artifact.path).exists() {
                        verdict = ClaimVerdict::Refused(ClaimRefusal::MissingArtifact(
                            artifact.name.clone(),
                        ));
                        break;
                    }
                }
            }

            if matches!(verdict, ClaimVerdict::Validated) {
                for artifact in claim.artifacts.iter().filter(|a| is_worktree_artifact(a)) {
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
                    // Only a checkout artifact can be a changed path in
                    // the worktree diff at all (ruling 0145), so only one
                    // can be excluded from `offending`. A managed output
                    // is not in this repository and never appears in
                    // `changed`.
                    let mut declared: std::collections::BTreeSet<String> = claim
                        .artifacts
                        .iter()
                        .filter(|a| is_worktree_artifact(a))
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

    // ---- Ruling 0145: the managed declared outputs of this Claim ----
    //
    // Validated here, *snapshotted below* once every other check has
    // passed, so a refused Claim writes nothing into the Work's durable
    // area. Each name is read exactly once and digested from the bytes
    // that read returned, which is what binds the receipt to the content
    // that was actually inspected rather than to whatever the mutable
    // staged path holds a moment later.
    //
    // Nothing here takes a caller path. The address is derived from the
    // triple's Work and Run — both already checked against this estate
    // and this journal above — plus the declared name, so "another
    // Work's staging area" and "an arbitrary path outside the checkout"
    // are not refusals this code has to make: they are shapes it cannot
    // express. What it does still have to refuse, and does, is a name
    // that cannot be one filename component, a name the Route never
    // declared, and an entry in the staging area that is not a plain
    // regular file contained in it (a symlink out is the case that
    // matters).
    let mut managed_bytes: Vec<(String, Vec<u8>, String)> = Vec::new();
    if matches!(verdict, ClaimVerdict::Validated) {
        // Addressability and declaration were settled above, before
        // `validate_claim`; what is left is the state of the actual
        // staged file.
        for artifact in claim.artifacts.iter().filter(|a| !is_worktree_artifact(a)) {
            // Addressability first, so "this Work and Run cannot address a
            // managed output by that name" stays its own verdict rather
            // than arriving as an unreadable area.
            if wirk_core::outputs::staged_path(
                &state.estate_root,
                &work_id,
                &run_id,
                &artifact.name,
            )
            .is_none()
            {
                verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(format!(
                    "no managed output address can be derived for `{}` from this Work and Run",
                    artifact.name
                )));
                break;
            }
            // Absent is `MissingArtifact` — the same answer a missing
            // checkout artifact gets, and the honest one: the actor did
            // not produce it. Present but not a regular file reached
            // inside the area (a symlink at the name, a symlink where an
            // ancestor directory should be, a directory) is
            // `OutOfBoundary`. Both verdicts, and the bytes, come from
            // one no-follow walk ending in one open file object — never
            // from a check followed by a second lookup (F6).
            let bytes = match read_staged_output(
                &state.estate_root,
                &work_id,
                &run_id,
                &artifact.name,
            ) {
                StagedRead::Bytes(bytes) => bytes,
                StagedRead::Absent => {
                    verdict =
                        ClaimVerdict::Refused(ClaimRefusal::MissingArtifact(artifact.name.clone()));
                    break;
                }
                StagedRead::OutOfBoundary => {
                    verdict = ClaimVerdict::Refused(ClaimRefusal::OutOfBoundary(format!(
                        "the staged output `{}` is not a regular file contained in this Run's \
                         own output area",
                        artifact.name
                    )));
                    break;
                }
                StagedRead::Unreadable => {
                    verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(format!(
                        "the staged output {} could not be read to record its content identity",
                        artifact.name
                    )));
                    break;
                }
            };
            let digest = sha256_hex(&bytes);
            managed_bytes.push((artifact.name.clone(), bytes, digest));
        }
        if !matches!(verdict, ClaimVerdict::Validated) {
            managed_bytes.clear();
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
        for artifact in claim.artifacts.iter().filter(|a| is_worktree_artifact(a)) {
            let resolved = worktree_path.join(&artifact.path);
            let Some(digest) = ArtifactReceipt::digest_of(&resolved) else {
                verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(format!(
                    "the claimed artifact {} could not be read to record its content identity",
                    artifact.name
                )));
                artifact_receipts.clear();
                break;
            };
            artifact_receipts.push(ArtifactReceipt::worktree(
                artifact.name.clone(),
                artifact_relative_to_worktree(&worktree_path, &artifact.path)
                    .map(|relative| relative.to_string_lossy().into_owned())
                    .unwrap_or_else(|| artifact.path.clone()),
                digest,
            ));
        }
    }

    // Ruling 0145, durable before referenced: the bytes validated just
    // above are written, fsynced and renamed under
    // `works/<work>/outputs/claims/<claim>/` **before** the
    // `ClaimRecorded` that names them exists — the same order, and the
    // same reason, as `works/<work>/projections/` (R2). A crash between
    // the two leaves a snapshot no event references, which nothing reads
    // and nothing deletes; the reverse order would journal a receipt for
    // a file that never existed.
    //
    // Last, after every refusal check including the worktree receipts'
    // own: a Claim that is going to be refused writes nothing here.
    if matches!(verdict, ClaimVerdict::Validated) && !managed_bytes.is_empty() {
        for (name, bytes, digest) in &managed_bytes {
            match wirk_core::outputs::store_claimed_bytes(
                &state.estate_root,
                &work_id,
                &claim_id,
                name,
                bytes,
            ) {
                Ok(_) => {}
                // The rename made it visible; only the directory fsync
                // failed. The bytes are there and re-hash to `digest`,
                // so reporting this as "never wrote" would be false —
                // `PreparedProjection::commit`'s own judgement, reused.
                Err(wirk_core::outputs::OutputWriteError::DurabilityUncertain(detail)) => {
                    eprintln!("wirkd: {detail}");
                }
                Err(err) => {
                    verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(format!(
                        "the managed output {name} could not be stored durably: {err}"
                    )));
                    artifact_receipts.clear();
                    break;
                }
            }
            // `stored_relative` re-checks the same two rules
            // `store_claimed_bytes` just enforced, so it cannot be
            // `None` here; a defensive `None` is an explicit
            // unavailability rather than a receipt naming nothing.
            let Some(path) = wirk_core::outputs::stored_relative(&claim_id, name) else {
                verdict = ClaimVerdict::Refused(ClaimRefusal::ValidationUnavailable(format!(
                    "no managed output address could be recorded for {name}"
                )));
                artifact_receipts.clear();
                break;
            };
            artifact_receipts.push(ArtifactReceipt {
                name: name.clone(),
                path,
                digest: digest.clone(),
                store: wirk_core::ArtifactStore::WorkOutputs,
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

        if !is_last {
            // W-C1: an orienting next leaf reads the Atlas, and nothing
            // that reads outside this Work may run under this Work's own
            // journal guard (0119, 0124). Drop it, assemble, and let
            // `advance_to_next_leaf` re-take the guard for the append;
            // `reserve_next_leaf` re-derives the next leaf and every
            // authority fact under that guard, exactly as it always has,
            // so nothing is carried across but the projection itself —
            // and that only if it was assembled for the very Waypoint
            // being reserved.
            //
            // A next leaf that declares no orientation never takes this
            // branch: it keeps the original single-guard advance, atomic
            // with the claim, byte for byte the behaviour it had.
            let orients = next_leaf_after(&events_now, &run.waypoint)
                .and_then(|next| find_definition(&journaled_defs, &next))
                .is_some_and(|def| def.orient.is_some());
            if orients {
                drop(journal);
                if let Err((code, message)) =
                    advance_to_next_leaf(state, &work_id, &journaled_defs, &run.waypoint)
                {
                    return err_reply(code, &message);
                }
                return reply;
            }
            if let Err((code, message)) = reserve_next_leaf(
                state,
                &work_id,
                &mut journal,
                &journaled_defs,
                &run.waypoint,
                None,
            ) {
                return err_reply(code, &message);
            }
        }
    }

    reply
}

/// The Waypoint that follows `after_leaf` in this Work's own flattened
/// Route order, derived from the journal exactly as `reserve_next_leaf`
/// derives it — extracted so the *decision* can be taken on a
/// dropped-guard observation and then re-taken under the commit guard,
/// rather than existing in two spellings that could drift.
fn next_leaf_after(events: &[Event], after_leaf: &WaypointId) -> Option<WaypointId> {
    let waypoints = route_waypoints(events);
    let position = waypoints.iter().position(|w| w == after_leaf)?;
    waypoints.get(position + 1).cloned()
}

/// The orienting auto-advance: assemble with **no** journal guard held,
/// then take the guard and reserve.
///
/// Callers reach this only when the next leaf declares an `orient`
/// block. A Waypoint that declares none never comes here at all, keeps
/// the original in-guard reservation, does no Atlas work and journals a
/// byte-identical World — which is precisely the compatibility this wave
/// owes every Route written before it.
fn advance_to_next_leaf(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    journaled_defs: &[WaypointDefinition],
    after_leaf: &WaypointId,
) -> Result<(), (&'static str, String)> {
    no_journal_guard_held("orienting auto-advance");
    let after = after_leaf.clone();
    let prepared = prepared_for_waypoint(state, work_id, move |events| {
        next_leaf_after(events, &after)
    });
    let handle = match journal_for(state, work_id) {
        Ok(Some(handle)) => handle,
        Ok(None) => return Ok(()),
        Err(err) => return Err(("JournalError", err.to_string())),
    };
    let mut journal = lock_journal(&handle);
    reserve_next_leaf(
        state,
        work_id,
        &mut journal,
        journaled_defs,
        after_leaf,
        prepared,
    )
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
    prepared: Option<PreparedProjection>,
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
                // P3 native closeout item 4. This used to compile one
                // development box's absolute cache path
                // (`/var/tmp/wirk-target`) into the product and set it
                // on every auto-advanced Deterministic World, where it
                // was content-addressed into the World hash with no
                // supported override — the operator's own environment
                // could not reach it, and the *first* Deterministic
                // Waypoint (`handle_submit`'s Git arm) set no env at
                // all, so the two paths disagreed about the same Route.
                //
                // The warm-cache policy itself is not the defect and is
                // not abandoned: it is an estate choice (workspace
                // rulings 0030, 0039 D126) about how *this* development
                // box builds, and the mechanism for it already exists
                // natively. `ChildExecutor` spawns with `command.envs
                // (&det.env)` over an inherited environment, so a wirkd
                // started with `CARGO_TARGET_DIR` in its own environment
                // hands it to every deterministic child, first Waypoint
                // and auto-advanced alike, with no product default, no
                // new Route field and no host path in a World hash.
                // Both Deterministic paths now reserve the same empty
                // env, and the cache is configured where it belongs —
                // outside the product, by whoever starts the daemon.
                env: BTreeMap::new(),
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
                    // W-B target binding: a review Waypoint reached by
                    // auto-advance freezes its targets at exactly the
                    // same point — its own reservation.
                    review_targets: freeze_review_targets(
                        state,
                        &fold(&events).repositories,
                        next_def,
                    ),
                    // W-C1: the projection this reservation delivers,
                    // resolved under the commit guard. `next_id` is
                    // re-derived here, from this Work's journal as it
                    // stands *now*; a projection prepared for any other
                    // Waypoint is discarded rather than attached, so a
                    // lost re-check degrades the evidence and never the
                    // authority (ruling 0124).
                    evidence: reservation_evidence(
                        state,
                        work_id,
                        next_def,
                        journaled_defs,
                        prepared,
                    )?,
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

/// Resolves the projection reference a reservation journals, under the
/// commit guard.
///
/// Three outcomes, and no fourth: a Waypoint that declares no
/// orientation gets `None` and does no Atlas work at all; a Waypoint
/// whose prepared projection was assembled for exactly this Waypoint
/// gets it, written durably first; and a Waypoint whose preparation was
/// missing or was assembled for a different Waypoint gets an explicitly
/// degraded projection, minted here without taking any lock. An
/// orienting reservation therefore always carries a projection, and it
/// is never one prepared for something else.
fn reservation_evidence(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    def: &WaypointDefinition,
    journaled_defs: &[WaypointDefinition],
    prepared: Option<PreparedProjection>,
) -> Result<Option<Box<wirk_core::EvidenceProjectionRef>>, (&'static str, String)> {
    let Some(orient) = def.orient.as_ref() else {
        return Ok(None);
    };
    let prepared = match prepared {
        Some(prepared) if prepared.waypoint == def.id => prepared,
        // The observation that happened is the one this receipt reports.
        // A preparation made for another Waypoint still cost its laps and
        // its wall-clock; no preparation at all cost neither, and saying
        // "eight laps" there would be a fabricated, now integrity-covered
        // provenance claim (ruling 0126, F1).
        other => {
            let span = other
                .as_ref()
                .map_or_else(ObservationSpan::none, |stale| ObservationSpan {
                    laps: stale.file.receipt.laps,
                    window_ms: stale.file.receipt.observation_window_ms,
                });
            degraded_projection(
                def,
                orient,
                &route_edition_of(journaled_defs),
                span,
                DegradedCause::PreparationDiscarded,
            )
        }
    };
    prepared
        .commit(state, work_id)
        .map(|reference| Some(Box::new(reference)))
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
    // The launch review's F-C, closed the way `finding list` and
    // `atlas findings` already answer: one of two *named* scopes, never
    // a silent unscoped default. `admin` keeps the whole reply; a
    // `requester` is gated by the identical `lineage_of` set
    // `admit_evidence` computes at raise time, and then sees the
    // checkout-derived half only if its own bindings cover this Work's
    // whole binding set (`DisclosureView::admits_work_checkout`, the
    // same rule `finding list` applies to an artifact path). A Work
    // reading its own status is trivially both, which is why `wirk
    // run`'s setup read and `RunLoop`'s progress poll are unchanged.
    let scoped: Option<(Work, Vec<Event>, HashSet<WorkId>)> = if payload.admin {
        None
    } else {
        let Some(requester_id) = &payload.requester else {
            return err_reply(
                "BadRequest",
                "a non-administrative status read requires --requesting-work",
            );
        };
        let Some(requester_events) = replay_events(state, requester_id) else {
            return err_reply("NotFound", "no such requesting work");
        };
        let requester = fold(&requester_events);
        let lineage = lineage_of(state, &requester, &requester_events);
        if !lineage.contains(&payload.work_id) {
            return err_reply(
                "InadmissibleEvidence",
                "the named work is not the requesting work's own journal or its parent/child lineage",
            );
        }
        Some((requester, requester_events, lineage))
    };

    let journal = match journal_for(state, &payload.work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let journal = lock_journal(&journal);
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
            // W-C3: the ordered chain of projection revisions this Run
            // was delivered — the initial reservation's, then each one
            // its own actor expanded. Identity only: a revision number,
            // an observation, a content id and a format tag. No
            // coordinate, no summary, no source alias; the delivered
            // content is reachable only through `wirk world show` under
            // this Run's own triple. Taken before `binding_status`
            // consumes the binding.
            let orientation: Vec<Value> = binding
                .as_ref()
                .ok()
                .map(|binding| projection_chain(binding, &run))
                .unwrap_or_default()
                .iter()
                .map(|entry| {
                    json!({
                        "revision": entry.revision,
                        "observation": entry.observation.0,
                        "projection": entry.projection.0,
                        "format": entry.format,
                        "initial": entry.revision == 0,
                    })
                })
                .collect();
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
            // Ruling 0159: only a Run that has not yet had its own
            // launch admitted can usefully receive a carried-forward
            // prior selection — one that already launched (or already
            // has a `RunLaunchRequested` bound) resolves from its own
            // durable `run.kind`/`run.selection`, not from this reply's
            // pre-launch precedence layers at all (`wirk run`'s
            // `launch_requested` branch never reads this field).
            let prior_selection = if run.launch_requested {
                None
            } else {
                prior_launch_for_waypoint(&events, &run.waypoint, run.attempt)
                    .map(|(kind, selection)| json!({"kind": kind, "selection": selection}))
            };
            // Ruling 0160 item 2 (interrupted materialization): this
            // Run's own already-journaled `WorktreeCreated`, when it
            // has one. `Run::apply` deliberately folds nothing from
            // that event (`wirk-core/src/lib.rs`: "`ClaimFiled` and
            // `WorktreeCreated` change no state"), and the World only
            // gains its `worktree_path` at the *following*
            // `WaypointReserved` — so a caller killed between the two
            // records had no way to learn that the first half of its
            // own materialization is already durable, re-emitted
            // `WorktreeCreated`, and was refused by `handle_record`'s
            // at-most-one guard for the rest of the Run's life.
            // Reporting the durable fact is what lets `wirk run`
            // finish the interrupted materialization from the journal
            // instead of duplicating its first half (`executor.rs`'s
            // `run_command`). Journal identity only — the same
            // `repo`/`base_sha` pair the reply's own `world` already
            // carries, and withheld beside it under narrowing.
            let worktree_created = events.iter().find_map(|event| match &event.kind {
                EventKind::WorktreeCreated { repo, base_sha }
                    if event.run.as_ref() == Some(&run.id) =>
                {
                    Some(json!({"repo": repo, "base_sha": base_sha}))
                }
                _ => None,
            });
            Some(json!({
                "run": serde_json::to_value(&run).ok()?,
                "world": world,
                "world_binding": world_binding,
                "selection": selection,
                "prior_selection": prior_selection,
                "worktree_created": worktree_created,
                "orientation": orientation,
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
    result["evidence"] = Value::Array(claim_evidence(&state.estate_root, &work.id, &events));
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

    // Release this Work's journal guard before the disclosure view
    // reads any further state — `DisclosureView::admits_work_checkout`
    // re-reads the reporting Work's own journal, so scoping under the
    // guard would self-deadlock on the first scoped read. Same order
    // and same reason as `handle_finding_assert`'s own `drop(journal)`
    // before it builds a view; the established journal-then-anything
    // direction is unchanged.
    drop(journal);
    match scoped {
        None => {
            result["scope"] = json!("administrative");
            ok_reply(result)
        }
        Some((requester, requester_events, lineage)) => {
            let mut view = DisclosureView::new(&requester, &requester_events, &lineage);
            if !view.admits_work_checkout(state, &payload.work_id) {
                view.withheld += withhold_status_content(&mut result);
            }
            result["scope"] = json!("requester");
            // Which Work this scoped answer is *about* — the target the
            // caller named, not the requester (the acknowledgment
            // review's F-2). `client::status` compares it to the id it
            // sent, so a reply that answers about something else cannot
            // be read as the answer to this consultation. The scoped
            // `watch` acknowledgment already carried exactly this pair;
            // `status` named the applied scope alone. Journal identity
            // the caller itself supplied: no content, and nothing the
            // requester did not already know. Added on the scoped reply
            // only — the administrative reply, which asked for no scope
            // and is bound to none, is byte-identical to what it was.
            result["work_id"] = json!(payload.work_id.0);
            result["disclosure"] = json!({"withheld": view.withheld});
            ok_reply(result)
        }
    }
}

/// Every checkout-derived part of a `status` reply, withheld as whole
/// objects for a requester whose own bindings do not cover the
/// reporting Work's. Returns how many parts were withheld — a count and
/// never a description, the same `withheld_json` discipline `finding
/// list` uses, so two withheld parts are indistinguishable and no
/// alias, path, generation, argv or claim text travels in the marker.
///
/// What is withheld is exactly what `event_source_disclosure` treats as
/// content:
///
/// - the compiled `world` and its binding, per Run and for the
///   effective Run — a `DeterministicWorld` is cwd, argv and env;
/// - a Run's `selection`, `launch_argv` and `launch_attempt`, plus the
///   Route-authored `selection` the reply offers for the Run's own
///   Waypoint (all three of `model`/`effort`/`args`, per F-A);
/// - a `RunFailed` cause's `detail`, wherever it surfaces: inside the
///   folded `RunState::Failed`, and as the flattened `failure_detail`;
/// - `needs_input.detail`, which is *the same string* — `fold` copies a
///   `LifecycleObserved{Blocked}` pane capture straight into it, so
///   leaving it here would have let the whole F-B correction be read
///   off a sibling field of the same reply;
/// - each validated Claim's artifact receipts (paths and digests).
///
/// What survives is journal identity: Work state, current waypoint,
/// event count, Run ids, attempts, content-addressed world hashes,
/// parent binding, container activations, a hold's declared-output
/// names, and which Run and *why* (`needs_input.reason`) the Work is
/// waiting.
fn withhold_status_content(result: &mut Value) -> usize {
    let mut withheld = 0usize;

    fn hide(parent: &mut Value, key: &str, withheld: &mut usize) {
        if let Some(slot) = parent.get_mut(key)
            && !slot.is_null()
        {
            *slot = withheld_json();
            *withheld += 1;
        }
    }

    /// For a list field: an empty list is not content, and marking it
    /// withheld would tell a narrowed caller that something is being
    /// kept from them when nothing is. The count this function feeds is
    /// read as "how much was hidden", so it must not be inflated by
    /// fields that were empty.
    fn hide_if_present(parent: &mut Value, key: &str, withheld: &mut usize) {
        if parent
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|list| !list.is_empty())
        {
            hide(parent, key, withheld);
        }
    }

    hide(result, "world", &mut withheld);
    hide(result, "world_binding", &mut withheld);
    hide(result, "failure_detail", &mut withheld);
    if let Some(needs_input) = result.get_mut("needs_input") {
        hide(needs_input, "detail", &mut withheld);
    }
    if let Some(entries) = result.get_mut("evidence").and_then(Value::as_array_mut) {
        for entry in entries {
            hide(entry, "artifacts", &mut withheld);
        }
    }
    if let Some(entries) = result.get_mut("runs").and_then(Value::as_array_mut) {
        for entry in entries {
            hide(entry, "world", &mut withheld);
            hide(entry, "world_binding", &mut withheld);
            hide(entry, "selection", &mut withheld);
            hide(entry, "prior_selection", &mut withheld);
            // Ruling 0160 item 2: the durable materialization fact
            // carries this Run's repository path and base sha — the
            // same two fields `world` above already withholds — so it
            // is narrowed with them rather than published beside a
            // hidden copy of itself.
            hide(entry, "worktree_created", &mut withheld);
            // W-C3: a narrowed reader learns *that* this Run's context
            // has a history and that it is not being shown it, the same
            // answer `world` already gives. The chain is hidden in both
            // places it appears — the summary this handler builds, and
            // the `Run`'s own folded tail — because withholding one and
            // serializing the other would be a hidden field and a
            // published copy of it.
            hide_if_present(entry, "orientation", &mut withheld);
            if let Some(run) = entry.get_mut("run") {
                hide_if_present(run, "expansions", &mut withheld);
                hide(run, "selection", &mut withheld);
                hide(run, "launch_argv", &mut withheld);
                hide(run, "launch_attempt", &mut withheld);
                // `RunState` is externally tagged: `"Open"`,
                // `"Vanished"`, `{"Claimed": <id>}` — all journal
                // identity — and `{"Failed": {status, request_id, at,
                // detail}}`, whose `detail` is the one content half.
                if let Some(failed) = run.pointer_mut("/state/Failed") {
                    hide(failed, "detail", &mut withheld);
                }
            }
        }
    }
    withheld
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
fn claim_evidence(estate_root: &Path, work_id: &WorkId, events: &[Event]) -> Vec<Value> {
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
                    // Ruling 0145: the receipt names the root its path
                    // is relative to, so this resolves against that root
                    // and never guesses from the string. A managed
                    // output resolves from the estate and this Work's id
                    // — which are always available — so it is never
                    // `unresolved` for want of a worktree; but it *is*
                    // `unresolved` when the canonical containment check
                    // fails, which is the honest answer when the stored
                    // bytes can no longer be reached inside the area
                    // that owns them.
                    let resolved: Option<PathBuf> = match artifact.store {
                        wirk_core::ArtifactStore::Worktree => worktree
                            .as_ref()
                            .map(|worktree| worktree.join(&artifact.path)),
                        wirk_core::ArtifactStore::WorkOutputs => {
                            wirk_core::outputs::resolve_stored(estate_root, work_id, &artifact.path)
                        }
                    };
                    let (available, reason) = match &resolved {
                        // A pre-correction receipt recorded a name and
                        // nothing else: inspectable, but never
                        // reportable as evidence that still holds.
                        _ if artifact.digest.is_empty() => (false, Some("unrecorded")),
                        None => (false, Some("unresolved")),
                        Some(path) => match ArtifactReceipt::digest_of(path) {
                            None => (false, Some("absent")),
                            Some(now) if now == artifact.digest => (true, None),
                            Some(_) => (false, Some("changed")),
                        },
                    };
                    json!({
                        "name": artifact.name,
                        "path": artifact.path,
                        "store": artifact.store.label(),
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
    let mut journal = lock_journal(&journal);
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
    // W-C1: a retried Waypoint that declares an `orient` block gets a
    // freshly assembled projection — revision 0, its own observation,
    // its own file. It is assembled here, with **no** journal guard
    // held, and used inside only if the Waypoint being re-reserved is
    // still the one it was assembled for; otherwise the retry proceeds
    // with an explicitly degraded projection. A Waypoint with no
    // `orient` block observes nothing and the whole retry path is
    // unchanged.
    let run_id = payload.triple.run_id.clone();
    let prepared = prepared_for_waypoint(state, &payload.triple.work_id, move |events| {
        find_run(events, &run_id).map(|run| run.waypoint)
    });
    handle_retry_inner(state, payload, prepared)
}

fn handle_retry_inner(
    state: &Arc<WirkdState>,
    payload: RetryPayload,
    prepared: Option<PreparedProjection>,
) -> Reply {
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
    let mut journal = lock_journal(&journal);
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
                // W-C1: a retry re-orients rather than inheriting. The
                // prior Run's projection stays on its own historical
                // World, on disk and readable; this Run gets its own
                // file at its own observation, so "written once, never
                // rewritten" holds across retries too.
                evidence: match find_definition(&waypoint_defs, &run.waypoint) {
                    Some(def) => {
                        match reservation_evidence(state, &work_id, def, &waypoint_defs, prepared) {
                            Ok(evidence) => evidence,
                            Err((code, detail)) => return err_reply(code, &detail),
                        }
                    }
                    None => None,
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
    let mut journal = lock_journal(&journal);
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
                    // W-C1 (BUILD.md §3.3): refuse the *journaled
                    // combination* `Unknown` basis plus a stage
                    // projection, evaluated **before** the legacy
                    // upgrade below — not a blanket refusal of an
                    // Unknown Actor basis, which would make every
                    // pre-`source_basis` journal unreplayable. No legal
                    // writer produces this pair (the one arm that mints
                    // `Unknown` for an Actor refuses an orienting
                    // Waypoint outright) and no legacy World carries a
                    // projection, so this branch only ever stops a
                    // hand-edited or corrupted journal replaying into a
                    // launch. The positive control is the very next
                    // statement: a legacy Unknown World with no evidence
                    // still resolves exactly as it always has, with
                    // `legacy_basis` set.
                    if actor.evidence.is_some() {
                        return Err(
                            "Actor World carries a stage projection with no recorded source basis"
                                .to_string(),
                        );
                    }
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

// ---- The journal lock discipline (ruling 0119) ----------------------------
//
// One rule governs every `Mutex<Journal>` in this file: **a thread that
// is deciding something about another Work's journal does not hold its
// own Work's journal guard while it reads that other journal.**
//
// The defect that rule exists to prevent is an AB/BA inversion, and it
// is reached by ordinary supported use rather than by anything exotic.
// `handle_finding_raise` held the raising Work's guard across
// `admit_evidence`, and admission reads whatever Work the evidence
// names — an ancestor, a descendant, or (since the off-lineage relation
// route) any estate publisher at all. Two Works naming each other at the
// same moment therefore took the same two locks in opposite orders and
// wedged permanently: not only the two raises, but *every* later read of
// either journal, `--admin` included, until the daemon was restarted.
// A settled EstateLocal publication the whole estate could read a moment
// earlier became unreadable. The rest of the estate stayed responsive,
// so it presents as two Works going quiet rather than as an outage.
//
// Two disciplines already in this file satisfy the rule, and the paths
// below reuse them rather than inventing a third or serializing the
// estate behind one lock:
//
//   - **A total acquisition order**, `handle_finding_applied`'s: it
//     needs several journals held at once, so it computes
//     `journal_lock_order` (ancestor depth, then id) for all of them
//     *before* taking any, and acquires in that order. Two callers of it
//     can never invert.
//   - **Observe, then re-acquire and re-check**, `cancel_work`'s: it
//     needs other journals *while deciding*, so it reads its own
//     journal, drops the guard, reads the children with no guard held,
//     then re-acquires its own guard and re-checks terminality under it
//     before appending ("a concurrent cancel of the same Work between
//     the read above and this write is harmless").
//
// Evidence admission cannot use the first: the set of journals an
// admission walk touches is discovered *by* the walk (each record it
// follows may name another), so there is no set to sort before locking.
// It uses the second, which is why `handle_finding_raise` and
// `settle_ready` below are written as an observe/decide/re-check loop.
//
// Re-checking is not optional bookkeeping. Between the observation and
// the append, the raising Work's own authority can change underneath:
// its Run can be retried and superseded, the Work can be canceled or
// completed, a child can be spawned that widens its lineage. Everything
// those checks rest on is derived from that Work's own append-only
// journal, so "the journal has not moved" is exactly "every authority
// fact this decision rested on still holds" — and appending under the
// same guard the check ran under is what keeps a concurrent raise from
// being lost. What is *not* re-checked, and never was, is the state of
// the other Works the evidence named: an admission freezes what it
// observed there, exactly as it did when it read them under a guard.
//
// `lock_journal` makes the rule checkable rather than hoped: every
// journal acquisition in this file goes through it, and `replay_events`
// — the one place another Work's journal is locked on a decision path —
// asserts that no guard is held when it is called.

thread_local! {
    /// How many journal guards this thread holds (or is blocked
    /// acquiring). Bookkeeping only: it is read by a `debug_assert`, and
    /// never by anything that decides an outcome.
    static JOURNAL_GUARDS_HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A `Journal` guard that counts itself for the discipline above.
struct JournalGuard<'a> {
    inner: std::sync::MutexGuard<'a, Journal>,
}

impl std::ops::Deref for JournalGuard<'_> {
    type Target = Journal;

    fn deref(&self) -> &Journal {
        &self.inner
    }
}

impl std::ops::DerefMut for JournalGuard<'_> {
    fn deref_mut(&mut self) -> &mut Journal {
        &mut self.inner
    }
}

impl Drop for JournalGuard<'_> {
    fn drop(&mut self) {
        JOURNAL_GUARDS_HELD.with(|held| held.set(held.get().saturating_sub(1)));
    }
}

/// The one acquisition path. Poisoning is recovered the way every call
/// site already did: a panic elsewhere must not make a Work's journal
/// permanently unreadable.
fn lock_journal(journal: &Mutex<Journal>) -> JournalGuard<'_> {
    JOURNAL_GUARDS_HELD.with(|held| held.set(held.get() + 1));
    JournalGuard {
        inner: journal.lock().unwrap_or_else(|poison| poison.into_inner()),
    }
}

/// The one nested acquisition this file still makes, named so it stays a
/// stated exception rather than an unremarked second pattern.
/// `evaluate_closure` runs under the **parent's** own journal guard and
/// reads the journal of a Work that parent's own `ChildWorkSpawned`
/// names, so every edge it adds to the lock graph points from a Work to
/// one of its own children. Those edges cannot close a cycle: a Work's
/// `parent` is fixed when it is spawned and no Work is its own ancestor.
/// What made the estate deadlockable was the *opposite* edge — a Work
/// taking an ancestor's, a sibling's or a stranger's journal under its
/// own guard — and evidence admission was the only path that took it.
fn lock_journal_of_declared_child(journal: &Mutex<Journal>) -> JournalGuard<'_> {
    lock_journal(journal)
}

/// The discipline above, asserted where it is actually violated — at the
/// moment a second journal would be locked. `debug_assert` because this
/// is a construction rule about this file's own call graph, not a
/// runtime input to validate: a release daemon must not gain a new
/// failure mode from it, and every test and every daemon this estate
/// builds runs with debug assertions on.
fn no_journal_guard_held(site: &str) {
    debug_assert_eq!(
        JOURNAL_GUARDS_HELD.with(std::cell::Cell::get),
        0,
        "{site} locks another Work's journal and must not run under a journal guard (ruling 0119)"
    );
}

/// Two observations of the same journal, taken at different moments.
/// The journal is append-only, so one is a prefix of the other and
/// "nothing was appended between them" is the whole question: same
/// length, and the same event last. Every authority fact the raise and
/// settlement paths check — the Work's state, its Run's currency for its
/// Waypoint, its repository bindings, its lineage, which Findings it
/// holds — is folded from these events and nothing else, so an unmoved
/// journal is an unchanged decision.
fn same_observation(before: &[Event], after: &[Event]) -> bool {
    before.len() == after.len()
        && before.last().map(|event| &event.id) == after.last().map(|event| &event.id)
}

/// How many times an observe/decide/re-check loop re-reads before giving
/// up. Each lap is lost only to a *concurrent append on the same Work*,
/// which is rare and never a spin: the loser re-reads once and proceeds.
/// The bound exists so a pathologically busy Work returns an honest
/// refusal instead of looping.
const JOURNAL_OBSERVATION_ATTEMPTS: usize = 8;

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

/// The estate's **pure discovery read** of one `works/` entry: replay
/// the canonical journal that is there, and be no reason for one to
/// appear that was not.
///
/// Six sweeps in this file walk `works/` only to *find out* which Works
/// exist and what they hold, and each then hands the answer to a
/// separate, already-authoritative operation — `remove_owned_containers`
/// to `docker rm -f`, `open_deterministic_runs` to the recovery match,
/// `reevaluate_waiting_works` to `reevaluate_parent`, `find_finding_
/// owner` to the `finding assert`/`settle`/`applied` handlers,
/// `settle_ready_findings` to `settle_ready`, and the admin estate-wide
/// `finding list` to its own reply. Every one of those mutating paths
/// still goes through `journal_for`/`create_journal_for` and appends
/// through `Journal::append`; the write path is untouched and still
/// exactly one.
///
/// Routed through `Journal::open` — the estate's one *write* path — the
/// discovery half had the two executed consequences the findings walk
/// was already cured of (`index-health-reverify/VERDICT.md`,
/// "`Journal::open` on the read path"), and they are worse here because
/// these run at startup, at shutdown and on ordinary mutating verbs:
///
/// 1. A `works/` entry with no journal got a **zero-byte
///    `journal.ndjson` created by the sweep itself** — a read path
///    inventing a canonical file, and the reason a restart turned an
///    absent journal into an empty one.
/// 2. A journal that is readable but not writable (`0444`: a restored
///    backup, an archived tree, a `chmod -R a-w` snapshot) failed to
///    open, so a real Work was **silently skipped** by container
///    cleanup, by run recovery, by held-work re-evaluation, by
///    settlement and by the admin listing — its findings simply were
///    not there.
///
/// Same on-disk format, same `EnvelopeIter`, same fail-closed rule on a
/// malformed line or a sequence gap (`store.md` §5): `JournalReader` is
/// the read-only half of the same journal, not a second parser.
///
/// `None` is *nothing this sweep may act on*, exactly as before: a
/// directory that is not a Work (no journal — the estate's own layout
/// rule, the one `journal_for` already applies to every other read), a
/// journal this sweep could not read, or a journal whose replay failed.
/// Each caller already `continue`s on `None`, so an absent, denied or
/// torn journal authorizes no mutation and is never folded as if it
/// were a legitimately empty history. What a caller must never do is
/// treat `None` as an attestation that the estate holds nothing — that
/// is the completeness question, and it is answered where it is asked,
/// by `CanonicalScan` against the rows the index already holds.
fn discovery_events(dir: &Path) -> Option<Vec<Event>> {
    JournalReader::open(dir).ok()?.replay().ok()
}

/// Reconstructs the `Run` named `run_id` by replaying `events` in order:
/// seeds the initial state at its `RunOpened`, then folds every
/// subsequent event through `Run::apply` (which already ignores events
/// naming a different Run — `lib.rs` "An event whose `run` is not this
/// Run's id is ignored"). `None` when no `RunOpened` names `run_id` at
/// all — the fabricated/stale-triple case (D9#4).
/// Ruling 0159 (recovery preserves explicit selection), corrected by
/// ruling 0160 (retry lineage): the **latest admitted** launch at this
/// same Waypoint — the most recent prior Run that actually reached an
/// admitted `RunLaunchRequested` — and its own bound `kind`/`selection`.
/// `wirk run` folds this in as a precedence tier between the Route's
/// own authored default and the harness's native default
/// (`resolve_launch_selection`'s own doc in `executor.rs`), so an
/// explicit CLI selection bound at the original launch survives a
/// supported retry instead of silently falling to the harness default.
///
/// 0160 corrected the original attempt-minus-one lookup, which asked
/// only this retry's immediate predecessor and so returned `None` the
/// moment one retry was opened and abandoned before launch — erasing,
/// permanently and for every later attempt, a choice still sitting
/// durable and unrewritten in the same journal this function reads
/// (`native-selection-retry-verify/VERIFIED.md` §1: an explicit
/// `codex` resolved to `claude` on attempt 3 and on every attempt
/// after it). Admission is the durable selection boundary, so the walk
/// goes back through the whole lineage, newest attempt first, and stops
/// at the first prior Run whose launch was admitted. An intervening
/// unlaunched attempt is passed over: it decided nothing, so it erases
/// nothing.
///
/// `None` for the very first attempt (`attempt <= 1`: no prior Run
/// exists at all) and for a lineage in which **no** attempt was ever
/// admitted — nothing explicit was ever chosen here, so there is
/// nothing to carry, and fabricating one would invent a choice nobody
/// made; the fresh Run resolves exactly as a first launch does instead.
/// The walk never leaves this Waypoint: a distinct Waypoint's own
/// authoring is its own, never inherited from a sibling.
fn prior_launch_for_waypoint(
    events: &[Event],
    waypoint_id: &WaypointId,
    attempt: u32,
) -> Option<(ActorKind, ActorSelection)> {
    // Newest-first: the latest admitted selection is the operative one,
    // so a later same-harness override that was itself admitted wins
    // over the older admission it replaced.
    (1..attempt).rev().find_map(|prior_attempt| {
        let prior_run_id = events.iter().find_map(|event| match &event.kind {
            EventKind::RunOpened {
                run,
                waypoint,
                attempt: a,
                ..
            } if waypoint == waypoint_id && *a == prior_attempt => Some(run.clone()),
            _ => None,
        })?;
        let prior = find_run(events, &prior_run_id)?;
        prior
            .launch_requested
            .then_some((prior.kind, prior.selection))
    })
}

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
                expansions: Vec::new(),
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

/// The one-word name of a Run's state, for the messages that must name
/// it (`handle_record`'s settled refusal). The same four words
/// `handle_status` already publishes for `run_state` (R2) — not a second
/// vocabulary.
fn run_state_name(state: &RunState) -> &'static str {
    match state {
        RunState::Open => "open",
        RunState::Claimed(_) => "claimed",
        RunState::Vanished => "vanished",
        RunState::Failed(_) => "failed",
    }
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
        match valid_child_receipt(state, events, container_id, &role.role) {
            // W-B obligation proof: a role that really closed is
            // recorded in this activation's own receipts whether or not
            // the outcome contract *required* it. Before this wave an
            // optional role's real, valid receipt was discarded, so a
            // verification obligation naming an optional role could
            // never be discharged by the role that actually performed it
            // — which is how the independent review's C2 ended up
            // crediting `auditor` through `scribe`. Holding is unchanged:
            // only a missing *required* role still holds the container.
            Some(receipt) => receipts.push(receipt),
            None if role.required => missing.push(format!("child role {}", role.role)),
            None => {}
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
        let journal = lock_journal_of_declared_child(&journal);
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
/// W-B (§6): same lock-ordering wrapper as `handle_claim`'s own —
/// `reevaluate_parent_inner`'s own `close_cascade` can append the exact
/// `StageClosed` a `child-investigation-confirmed` settlement is waiting
/// on, and `settle_ready` must never run while that Work's journal lock
/// is still held.
fn reevaluate_parent(state: &Arc<WirkdState>, parent: &ParentBinding) -> Result<(), JournalError> {
    let result = reevaluate_parent_inner(state, parent);
    if result.is_ok()
        && let Err(err) = settle_ready(state, &parent.work, false)
    {
        eprintln!(
            "wirkd: settlement evaluation after container closure failed for {}: {err}",
            parent.work.0
        );
    }
    result
}

fn reevaluate_parent_inner(
    state: &Arc<WirkdState>,
    parent: &ParentBinding,
) -> Result<(), JournalError> {
    let Some(journal_handle) = journal_for(state, &parent.work)? else {
        return Ok(());
    };
    let mut journal = lock_journal(&journal_handle);
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
    // W-C1: the same split the claim path draws. An orienting next leaf
    // is reserved through `advance_to_next_leaf`, with this Work's
    // journal guard **dropped** for the assembly and re-taken for the
    // append; a next leaf that declares no orientation keeps the
    // original in-guard reservation unchanged.
    let mut orienting_advance: Option<WaypointId> = None;
    if let Some(container) = find_definition(&defs, &closed)
        && let Some(last_leaf) = flatten_leaves(std::slice::from_ref(container))
            .last()
            .cloned()
    {
        let orients = journal
            .replay()
            .ok()
            .and_then(|events| next_leaf_after(&events, &last_leaf))
            .and_then(|next| find_definition(&defs, &next))
            .is_some_and(|def| def.orient.is_some());
        if orients {
            orienting_advance = Some(last_leaf);
        } else if let Err((code, message)) =
            reserve_next_leaf(state, &parent.work, &mut journal, &defs, &last_leaf, None)
        {
            eprintln!(
                "wirkd: advancing {} past its closed container failed: {code} {message}",
                parent.work.0
            );
        }
    }
    if let Some(last_leaf) = orienting_advance {
        drop(journal);
        if let Err((code, message)) = advance_to_next_leaf(state, &parent.work, &defs, &last_leaf) {
            eprintln!(
                "wirkd: advancing {} past its closed container failed: {code} {message}",
                parent.work.0
            );
        }
        journal = lock_journal(&journal_handle);
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
        // Pure discovery: read the journal that is there, create
        // nothing, and read one an operator left read-only
        // (`discovery_events`). The mutation this sweep decides on
        // still goes through the one write path below.
        let Some(events) = discovery_events(&dir) else {
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
        let journal = lock_journal(&journal);
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
                    let journal = lock_journal(&journal);
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
    let mut journal = lock_journal(&journal);
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
            // The chunker that actually produced the rows. `null` is the
            // honest answer for an edition whose rows are the
            // generation's own units — it says "nothing else chunked
            // this" rather than implying a native boundary.
            "chunks": edition.chunker.chunks.as_ref().map(|chunks| json!({
                "implementation": chunks.implementation,
                "entry_point": chunks.entry_point,
                "constants": chunks.constants,
                "parsers": chunks.parsers,
                "files": chunks.files.iter().map(configured_path_json).collect::<Vec<_>>(),
                // The bytes behind `parsers`, which is only a provider
                // and a version. `unreported` is the honest answer for
                // every edition built before this was measured, and
                // `none_loaded` says zero libraries rather than
                // pretending an empty list is coverage
                // (`EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` O1).
                "grammars": grammar_coverage_json(&chunks.grammars),
            })),
        },
        // How these rows are ranked. `null` on every edition built before
        // retrieval existed: such an edition binds no ranking
        // representation, and a query refuses to invent one for it.
        "retrieval": edition.retrieval.as_ref().map(|retrieval| json!({
            "scheme": retrieval.scheme,
            "chunking": retrieval.chunking.label(),
            "native": retrieval.native,
            "dense": retrieval.dense,
            "sparse": retrieval.sparse,
            "path_convention": retrieval.path_convention,
            "fusion": retrieval.fusion,
            "capacity_policy": retrieval.capacity_policy,
            "capacity_max": retrieval.capacity_max,
            // Present only on an edition built under the previous
            // universal-depth policy, whose own bytes are left exactly as
            // they were written and are read back verbatim here.
            "candidate_limit": retrieval.candidate_limit,
            "digest": retrieval.digest,
        })),
        // What the rows cover of the generation, and what they honestly
        // do not: native chunks are not a partition and a whitespace-only
        // resource yields none at all.
        "coverage": {
            "resources_indexed": edition.coverage.resources_indexed,
            "resources_with_rows": edition.coverage.resources_with_rows,
            "indexed_bytes": edition.coverage.indexed_bytes,
            "covered_bytes": edition.coverage.covered_bytes,
            "resources_without_rows": unavailable_entries_json(
                &edition.coverage.resources_without_rows),
            "resources_unmapped": unavailable_entries_json(
                &edition.coverage.resources_unmapped),
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

/// The parser shared libraries an edition's boundaries actually came out
/// of, or the named absence of that coverage. A version string is not a
/// measurement of the library it names, and this is where the difference
/// is published.
fn grammar_coverage_json(grammars: &wirk_atlas::GrammarCoverage) -> Value {
    match grammars {
        wirk_atlas::GrammarCoverage::Unreported => json!({"state": "unreported"}),
        wirk_atlas::GrammarCoverage::NoneLoaded(reason) => {
            json!({"state": "none_loaded", "reason": reason})
        }
        wirk_atlas::GrammarCoverage::Unavailable(reason) => {
            json!({"state": "unavailable", "reason": reason})
        }
        wirk_atlas::GrammarCoverage::Measured(measured) => json!({
            "state": "measured",
            "provider": measured.provider,
            "cache_root": measured.cache_root,
            "scope": measured.scope,
            "libraries": measured.libraries.iter().map(|library| json!({
                "languages": library.languages,
                "file": configured_path_json(&library.file),
                "declaration": match &library.declaration {
                    wirk_atlas::ModuleAttribution::Declared(detail) =>
                        json!({"state": "declared", "detail": detail}),
                    wirk_atlas::ModuleAttribution::Undeclared(detail) =>
                        json!({"state": "undeclared", "detail": detail}),
                },
            })).collect::<Vec<_>>(),
            "uncovered": unavailable_entries_json(&measured.uncovered),
        }),
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

/// The same environment record, rendered for an *answer* rather than for
/// a stored edition.
///
/// An edition is written once and read rarely, so `backend_environment_json`
/// above prints every module it measured. A query answer is produced on
/// every page: the full list is 435 rows and about 188 KB here, which
/// would make the reply fifteen times larger than the hits it carries and
/// would say the same thing on every page.
///
/// What is dropped is only the rows a reader can already account for: the
/// modules a distribution's own `RECORD` declares and whose bytes verify.
/// What is kept is everything that answers "what does this record not
/// cover" — the counts, the coverage state and its detail, the scope
/// sentence, the digest that actually binds the measurement, and, in
/// full, every module declared by no `RECORD` and everything the
/// interpreter could not describe. That is
/// `W4-PRODUCER-PROVENANCE-CORRECTION.md` item 2's requirement: the
/// missing scope stays inspectable, and a skipped count alone never
/// stands in for it.
fn producer_environment_json(environment: &wirk_atlas::BackendEnvironment) -> Value {
    let wirk_atlas::BackendEnvironment::Reported(identity) = environment else {
        return json!({"state": "unreported"});
    };
    let undeclared: Vec<Value> = identity
        .modules
        .iter()
        .filter(|module| {
            matches!(
                module.attribution,
                wirk_atlas::ModuleAttribution::Undeclared(_)
            )
        })
        .map(|module| {
            json!({
                "name": module.name,
                "origin": module.origin,
                "path": module.path,
                "digest": module.digest,
                "byte_len": module.byte_len,
                "detail": match &module.attribution {
                    wirk_atlas::ModuleAttribution::Undeclared(detail) => detail.clone(),
                    wirk_atlas::ModuleAttribution::Declared(name) => name.clone(),
                },
            })
        })
        .collect();
    json!({
        "state": "reported",
        "coverage": environment_coverage_json(&identity.coverage),
        "scope": identity.scope,
        "kind": identity.kind,
        "root": identity.root,
        "runtime": identity.runtime,
        "executable": identity.executable,
        "digest": identity.digest,
        "distributions_total": identity.distributions.len(),
        "modules_total": identity.modules.len(),
        "modules_declared": identity.modules.len() - undeclared.len(),
        // Named in full, never only counted: these are the modules whose
        // membership the record declines to assert.
        "modules_undeclared": undeclared,
        "undescribed_distributions": unavailable_entries_json(
            &identity.undescribed_distributions),
        "unmeasured_modules": unavailable_entries_json(&identity.unmeasured_modules),
    })
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
        chunking: match payload.chunker.as_deref() {
            None | Some("units") => wirk_atlas::SemanticChunking::Units,
            Some("native") => wirk_atlas::SemanticChunking::Native,
            Some(other) => {
                return err_reply("BadRequest", &format!("unknown --chunker value {other}"));
            }
        },
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
            let journal = lock_journal(&journal);
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
/// W-B target binding: resolve a Waypoint's declared review selectors
/// against the estate's own published sources and **freeze** them into
/// the World being reserved. This is the one place the review's target
/// becomes an exact identity, and it happens before the review executes
/// and before the operator has anything to admit.
///
/// Each selector names an Atlas source alias and a resource path. The
/// membership must be admitted under the reviewing Work's own
/// `repositories` bindings — the identical `admitted_membership_for`
/// call `admit_evidence`'s own `Source` arm makes, so a review can never
/// be pointed at a source the Work is not bound to. The generation is
/// that membership's own **currently published** generation, and the
/// object is the resource record's own object id in it.
///
/// A selector that does not resolve contributes nothing rather than
/// failing the reservation: the resulting World then carries fewer
/// frozen targets than the contract declares, its hash and therefore its
/// obligation basis differ from the fully-resolved one, and
/// `actor_reviewed_readiness` refuses it outright. Fail closed, and
/// visible in the basis the operator is asked to admit.
///
/// Returns an empty vector for every Waypoint that declares no review,
/// which is every Waypoint outside the agentic class.
fn freeze_review_targets(
    state: &Arc<WirkdState>,
    bindings: &[RepositoryBinding],
    def: &WaypointDefinition,
) -> Vec<ReviewTarget> {
    let Some(obligation) = def.verifies.as_ref() else {
        return Vec::new();
    };
    let Some(review) = obligation.review.as_ref() else {
        return Vec::new();
    };
    if review.targets.is_empty() {
        return Vec::new();
    }
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let scope = wirk_atlas::QueryScope::Work(bindings.to_vec());
    let mut frozen = Vec::new();
    for selector in &review.targets {
        let Some(membership) = atlas
            .memberships()
            .find(|member| member.alias == selector.source)
            .cloned()
        else {
            continue;
        };
        if admitted_membership_for(&atlas, &scope, &membership.id).is_none() {
            continue;
        }
        let Ok(Some(generation)) = atlas.current(&membership) else {
            continue;
        };
        let Some(resource) = generation
            .resources
            .iter()
            .find(|record| record.path == selector.path.as_bytes())
        else {
            continue;
        };
        let Some(object_id) = resource.object_id.clone() else {
            continue;
        };
        frozen.push(ReviewTarget {
            source: selector.source.clone(),
            path: selector.path.clone(),
            estate: membership.estate.0.clone(),
            membership: membership.id.0.clone(),
            source_id: membership.source.0.clone(),
            generation: generation.id.0.clone(),
            object_id,
        });
    }
    frozen
}

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

/// The presentation budget on one hit's snippet.
///
/// The current default extractor edition packs consecutive short lines
/// into one retrieval unit up to 65,536 bytes, so a hit's unit text is
/// now routinely thousands of lines where the historical one-line-per-
/// unit edition made it one. A reply that inlined all of it for every
/// hit would hand a reader tens of thousands of bytes per hit and call
/// it a snippet.
///
/// Presentation only, and disclosed: the cut never reaches `coverage`,
/// `budget` or the hit's coordinate, so the caller keeps the exact
/// byte/line range and can `atlas resolve` the whole span whenever it
/// wants it (ruling 0126 F2 / 0044: a rendering budget must not become a
/// completion oracle, in either direction).
const SEARCH_SNIPPET_BYTES: usize = 2 * 1024;

/// The first `SEARCH_SNIPPET_BYTES` of a unit, cut on a UTF-8 boundary,
/// with the honest total beside it.
///
/// The fallback, not the rule: it is what a hit gets when there is no
/// term location to be local to — a semantically ranked row, or a unit
/// that fits the budget whole. `evidence_window` is what a lexical hit
/// on a packed unit gets.
fn bounded_snippet(snippet: &str) -> (String, bool, usize) {
    let bytes = snippet.as_bytes();
    if bytes.len() <= SEARCH_SNIPPET_BYTES {
        return (snippet.to_string(), false, bytes.len());
    }
    let mut cap = SEARCH_SNIPPET_BYTES;
    while cap > 0 && !snippet.is_char_boundary(cap) {
        cap -= 1;
    }
    (snippet[..cap].to_string(), true, bytes.len())
}

/// The part of a unit that is actually shown, chosen so the query's own
/// matches are inside it (ruling 0142).
///
/// Byte offsets are relative to the unit's text. `whole_match_shown` is
/// false only in the one case where no bounded text can carry the whole
/// match: the matched token is itself at least the display budget.
struct EvidenceWindow {
    start: usize,
    end: usize,
    matched_terms: Vec<String>,
    whole_match_shown: bool,
}

/// The start of the line `offset` is on.
///
/// A byte inside a character is on that character's own line, so the
/// offset falls back to a character boundary before the line is looked
/// for: `evidence_window` computes its lead by arithmetic, and that
/// arithmetic lands mid-character over multi-byte prose.
fn line_begin(text: &str, offset: usize) -> usize {
    let mut at = offset;
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    text[..at].rfind('\n').map_or(0, |index| index + 1)
}

/// Choose the displayed window for a lexical hit.
///
/// The anchor is the match after which the most *distinct* query terms
/// fall inside one budget — so a two-term query shows where the two
/// terms actually meet rather than wherever the first one happens to
/// occur. Ties go to the earliest match, which keeps the choice
/// deterministic and reading-order natural.
///
/// The window is then snapped to committed line boundaries, because a
/// half line of source is not evidence a reader can act on and a line
/// boundary is always a character boundary. Snapping never drops the
/// anchor: the start falls back through "the anchor's own line" to "the
/// anchor's first byte", and the end falls back from a line boundary to
/// a character boundary rather than cutting the anchor away.
fn evidence_window(
    text: &str,
    matches: &[wirk_atlas::TermMatch],
    budget: usize,
) -> Option<EvidenceWindow> {
    if text.len() <= budget || matches.is_empty() || budget == 0 {
        return None;
    }
    let offsets: Vec<(usize, usize)> = matches
        .iter()
        .map(|found| (found.offset as usize, found.len as usize))
        .filter(|(offset, len)| offset + len <= text.len())
        .collect();
    let (anchor_at, anchor_len) = *offsets
        .iter()
        .max_by_key(|(offset, _)| {
            let reach = offset + budget;
            let distinct: std::collections::BTreeSet<&str> = matches
                .iter()
                .filter(|found| {
                    found.offset as usize >= *offset && (found.offset + found.len) as usize <= reach
                })
                .map(|found| found.term.as_str())
                .collect();
            // Earliest wins a tie: `max_by_key` keeps the last maximum,
            // so the tiebreak is the negated offset.
            (distinct.len(), std::cmp::Reverse(*offset))
        })
        .expect("a lexical hit has at least one match inside its own unit");

    // A token at least as wide as the budget cannot be shown whole by
    // any bounded window. Show what fits, starting at the match itself.
    if anchor_len >= budget {
        let mut end = anchor_at + budget;
        while end > anchor_at && !text.is_char_boundary(end) {
            end -= 1;
        }
        return Some(EvidenceWindow {
            start: anchor_at,
            end,
            matched_terms: vec![
                matches
                    .iter()
                    .find(|found| found.offset as usize == anchor_at)
                    .map(|found| found.term.clone())
                    .unwrap_or_default(),
            ],
            whole_match_shown: false,
        });
    }

    let cluster_end = matches
        .iter()
        .map(|found| (found.offset + found.len) as usize)
        .filter(|end| *end <= anchor_at + budget)
        .max()
        .unwrap_or(anchor_at + anchor_len);
    let lead = budget.saturating_sub(cluster_end - anchor_at) / 2;
    let start = [
        line_begin(text, anchor_at.saturating_sub(lead)),
        line_begin(text, anchor_at),
        anchor_at,
    ]
    .into_iter()
    .find(|candidate| candidate + budget >= anchor_at + anchor_len)
    .unwrap_or(anchor_at);

    let ceiling = (start + budget).min(text.len());
    let mut end = ceiling;
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len() {
        // Prefer a whole number of committed lines.
        if let Some(index) = text[start..end].rfind('\n')
            && start + index + 1 >= anchor_at + anchor_len
        {
            end = start + index + 1;
        }
    }
    let mut matched_terms: Vec<String> = matches
        .iter()
        .filter(|found| {
            found.offset as usize >= start && (found.offset + found.len) as usize <= end
        })
        .map(|found| found.term.clone())
        .collect();
    matched_terms.sort();
    matched_terms.dedup();
    Some(EvidenceWindow {
        start,
        end,
        matched_terms,
        whole_match_shown: true,
    })
}

/// The window's own exact coordinate over the same committed blob.
///
/// `whole` is the coordinate of the text the window is inside — a ranked
/// unit for a search hit, the same unit or a resolved resource for an
/// assembled item — and `text` is exactly the bytes that coordinate
/// names.
///
/// R2: the line arithmetic is `wirk_atlas::actual_line_bounds`, the very
/// function `AtlasStore::resolve_exact` validates a coordinate with, run
/// over that text and rebased onto its own first line — so a span this
/// builds is a span that resolver accepts, or none is returned at all.
fn window_coordinate(
    whole: &wirk_atlas::ExactCoordinate,
    text: &str,
    window: &EvidenceWindow,
) -> Option<wirk_atlas::ExactCoordinate> {
    let (line_start, line_end) =
        wirk_atlas::actual_line_bounds(text.as_bytes(), window.start as u64, window.end as u64)?;
    Some(wirk_atlas::ExactCoordinate {
        byte_start: whole.byte_start + window.start as u64,
        byte_end: whole.byte_start + window.end as u64,
        line_start: whole.line_start + line_start - 1,
        line_end: whole.line_start + line_end - 1,
        ..whole.clone()
    })
}

fn evidence_hit_json(hit: &wirk_atlas::EvidenceHit) -> Value {
    let (mut snippet, mut snippet_truncated, unit_bytes) = bounded_snippet(&hit.snippet);
    // What was shown, and exactly where it came from. Absent — and the
    // reply falls back to the head of the unit — whenever there is no
    // term location to be local to, so a semantic row is never dressed
    // up as a lexical match.
    let mut evidence = Value::Null;
    if let Some(window) = evidence_window(&hit.snippet, &hit.matches, SEARCH_SNIPPET_BYTES)
        && let Some(coordinate) = window_coordinate(&hit.coordinate, &hit.snippet, &window)
    {
        snippet = hit.snippet[window.start..window.end].to_string();
        snippet_truncated = true;
        evidence = json!({
            "coordinate": encode_coordinate(&coordinate),
            "byte_start": coordinate.byte_start,
            "byte_end": coordinate.byte_end,
            "line_start": coordinate.line_start,
            "line_end": coordinate.line_end,
            "matched_terms": window.matched_terms,
            // False only when the matched token is itself at least the
            // display budget, so no bounded text could carry it whole.
            "whole_match_shown": window.whole_match_shown,
        });
    }
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
        "snippet": snippet,
        // What was cut, said plainly. `unit_bytes` is the real size of
        // the retrieval unit this hit addresses, so a truncated snippet
        // is never mistaken for a short unit — and the coordinate above
        // still names the whole span.
        "snippet_truncated": snippet_truncated,
        "unit_bytes": unit_bytes,
        // Ruling 0142: when the shown bytes are a *part* of the ranked
        // unit chosen around the query's own matches, this names that
        // part exactly — a supported coordinate `atlas resolve` returns
        // those same committed bytes for. Null when the snippet is the
        // head of the unit (nothing lexical located anything inside it)
        // or the whole unit; the hit's own `coordinate` above always
        // names the whole ranked unit either way.
        "evidence": evidence,
    })
}

fn budget_json(budget: &wirk_atlas::AnswerBudget) -> Value {
    json!({
        "limit": budget.limit,
        "offset": budget.offset,
        "total_candidates": budget.total_candidates,
        "returned": budget.returned,
        // Ruling 0171: the result capacity this query ran at, held apart
        // from `limit`, which is only how many of its rows this page
        // shows. `total_candidates` is the size of the result set the
        // capacity bounded, never a count of what the estate holds.
        "capacity": budget.capacity,
        "capacity_source": budget.capacity_source.label(),
        "capacity_applies": budget.capacity_applies,
    })
}

/// `applied`, `partial`, `unavailable` or `disabled`, and — for the two
/// that are not self-explanatory — the reason, in the same words the
/// plain-text surface prints. `partial` is its own state on purpose:
/// semantic ranking that covered some of the admitted sources is neither
/// a full application nor an absence.
fn semantic_status_json(status: &wirk_atlas::SemanticStatus) -> Value {
    let mut value = json!({"status": status.label()});
    if let Some(reason) = status.reason() {
        value["reason"] = json!(reason);
    }
    value
}

/// What the native implementation reported about a ranking that actually
/// happened. Present only when one did, so "semantic" is never a word the
/// product says without something behind it.
fn application_json(application: &wirk_atlas::SemanticApplication) -> Value {
    json!({
        "native": application.native,
        "model_digest": application.model_digest,
        "retrieval": application.retrieval_digest,
        "rows_ranked": application.rows_ranked,
        // What this ranking's own budget was and what it did with it
        // (ruling 0171). `capacity` is the `top_k` the native ranker was
        // actually asked for and the whole continuation is frozen under;
        // `result_rows` is how many rows it returned. `capacity_reached`
        // says relevant rows may exist beyond this query's budget;
        // `resultset_exhausted` says this ranker's bounded result set ran
        // out at this capacity — which is never a statement that the
        // admitted view holds nothing further.
        "capacity": application.capacity,
        "capacity_source": application.capacity_source.label(),
        "capacity_policy": application.capacity_policy,
        "capacity_max": application.capacity_max,
        "result_rows": application.result_rows,
        "capacity_reached": application.capacity_reached,
        "resultset_exhausted": application.resultset_exhausted,
        // The implementation that actually ranked this answer, measured by
        // the product. Rendered through the same helpers the edition's own
        // backend block uses, because it is the same kind of claim about
        // the same kind of boundary — and bounded by the same words: this
        // is what was measured, never an attestation that the process
        // which returned these scores is the one described.
        "producer": {
            "scheme": wirk_atlas::QUERY_PRODUCER_SCHEME,
            "protocol": application.producer.protocol,
            "program": configured_path_json(&application.producer.program),
            "arguments": application.producer.arguments.iter()
                .map(configured_path_json).collect::<Vec<_>>(),
            "argv": application.producer.argv.iter()
                .map(backend_argument_json).collect::<Vec<_>>(),
            "reported": application.producer.reported,
            "environment": producer_environment_json(&application.producer.environment),
            "scope": wirk_atlas::QUERY_PRODUCER_SCOPE,
            "configuration_digest": application.producer_pin.configuration,
            "digest": application.producer_pin.identity,
            // What those digests were measured on, published on page 1
            // rather than left to be inferred from `environment.state`:
            // `configuration_only` is an honest, usable answer whose
            // continuation this product will refuse, and a caller is
            // entitled to know that before it asks for page 2.
            "basis": application.producer_pin.basis.label(),
            "basis_detail": basis_detail(application.producer_pin.basis),
        },
    })
}

/// What a published basis actually claims, in the words both public
/// surfaces print.
///
/// `EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(b): the measured sentence
/// describes the *reported* scope, never the process — an honestly
/// narrowed module list is a true list and a bound, and a sentence
/// claiming "the loaded-module bytes of the process that ranked this
/// answer were measured" says more than any self-report can carry. Both
/// arms are the atlas constants so the two surfaces cannot drift apart.
fn basis_detail(basis: wirk_atlas::QueryProducerBasis) -> &'static str {
    match basis {
        wirk_atlas::QueryProducerBasis::ImplementationMeasured => {
            wirk_atlas::QUERY_PRODUCER_BASIS_MEASURED
        }
        wirk_atlas::QueryProducerBasis::ConfigurationOnly => {
            wirk_atlas::QUERY_PRODUCER_BASIS_MISSING
        }
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
        // W4 B: this continuation's captured semantic editions cannot be
        // ranked through any more, so the page it asks for is refused
        // rather than silently reproduced from a different corpus.
        "continuation_unrecoverable": coverage.continuation_unrecoverable,
        // Ruling 0135 C4-R12: at least one generation this answer read
        // records a resource the extractor could not turn into retrieval
        // units, so part of the admitted corpus was never searched. A
        // flag and nothing more — which resource, in which source, at
        // what size is `atlas status` for a source the caller is already
        // admitted to, not something a search answer discloses.
        "source_extraction_incomplete": coverage.source_extraction_incomplete,
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
    /// The result capacity this continuation's first page was ranked at
    /// and every later page must be ranked at (ruling 0171), or `None` on
    /// a lexical answer, which has no native result set to bound.
    ///
    /// Separate from `limit` above on purpose: `limit` is how many rows a
    /// page shows and moves nothing about the ranking, while a different
    /// capacity is a different ranking function and so a different query.
    /// A semantic token carrying no capacity at all was issued under the
    /// previous universal-depth policy and is refused rather than resumed
    /// under this one.
    #[serde(default)]
    capacity: Option<u64>,
    offset: usize,
    generations: Vec<(String, String)>,
    /// P3 W4 B: the semantic editions the issuing answer actually ranked
    /// through, and the mode it ranked in. Both are re-executed from the
    /// token rather than re-derived, so a same-generation edition switch,
    /// a fresh selection, or semantics becoming available in between
    /// cannot change what an open continuation is paging through.
    #[serde(default)]
    editions: Vec<(String, String)>,
    #[serde(default)]
    mode: String,
    /// The query backend this continuation was issued under. Restated by
    /// the caller and compared, exactly as the query and limit are: a
    /// page ranked by a different backend or model is not the next page
    /// of this answer.
    #[serde(default)]
    semantic_backend: Option<String>,
    #[serde(default)]
    semantic_backend_args: Vec<String>,
    #[serde(default)]
    semantic_model: Option<String>,
    /// P3 W4 B correction: the *implementation* that ranked the issuing
    /// answer, as two digests — not a second copy of the backend path,
    /// which `semantic_backend` above already restates. A file edited in
    /// place at the same configured path, an alias retargeted to a
    /// different file, or a module loading from somewhere else inside the
    /// same interpreter all leave every field above untouched and move
    /// these (`public-retrieval-verify/VERDICT.md` O1, executed as this
    /// stage's red). Answer-derived, so a continuation restating the
    /// request is compared on the request fields alone and these two are
    /// carried through — then checked against a freshly measured
    /// producer, in `wirk_atlas`, where the refusal belongs.
    #[serde(default)]
    producer_configuration: Option<String>,
    #[serde(default)]
    producer_identity: Option<String>,
    /// What those two digests were measured on
    /// (`wirk_atlas::QueryProducerBasis`). Carried because only page 1
    /// can state it, and a continuation that cannot tell a pin covering
    /// implementation bytes from one covering an argv line cannot know
    /// what its own check is worth (`VERDICT.md` V1).
    #[serde(default)]
    producer_basis: Option<String>,
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
/// What token, if any, an answer hands back.
///
/// Pulled out of the reply builder because the interesting case is a
/// judgement rather than a rendering: an answer that refused a
/// continuation must not mint a new one
/// (`public-retrieval-identity-verify/VERDICT.md` V2). The minted token
/// would carry no application, therefore no producer pin, and following
/// it would refuse with "issued before the query producer identity was
/// recorded" — a true sentence about pre-correction history and a false
/// one about a token this build issued seconds earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuationDecision {
    /// No token at all: there is nothing this caller may page through.
    Withheld,
    /// Hand back the caller's own token unchanged, so restoring whatever
    /// broke resumes the continuation it already holds.
    Preserved,
    /// P3 native closeout item 3: this page returned no rows, so the
    /// offset a fresh token would carry is the offset this request
    /// already used (`offset + hits.len()`, with `hits` empty). Issuing
    /// one would hand the caller a byte-identical request whose answer
    /// is byte-identical again — the walk observed in
    /// `p3-sources/source-coverage-verify/raw/p4-walk.txt`, where pages
    /// 10 through 15 each "returned 0 of 18" and each still received a
    /// token. There is nothing left to page to, so no token is issued.
    Exhausted,
    /// Issue the token this answer's own page earned.
    Fresh,
}

/// P3 native closeout item 3, scoped exactly as root qualified it: the
/// defect is a page that returns **zero rows and advances the offset by
/// zero**, which is the precise no-progress condition — not a general
/// "must never loop" rule and not a limit on how many valid finite pages
/// a caller may walk.
///
/// Every page that returned at least one row still issues a fresh token,
/// so a legitimately long walk continues to exhaustion. The two
/// deliberate refusals are untouched and still answered first:
/// `Withheld` (no sources, or denied) and `Preserved` (a continuation
/// whose own ranking cannot be reproduced — its caller's token is what
/// still resumes, and replacing it would be exactly the quiet
/// substitution `VERDICT.md` V2 refuses).
///
/// `truncated` deliberately does not rescue an empty page. The ranked
/// list is one deterministic list paged with `skip(offset).take(limit)`
/// (`wirk-atlas::query`), so a window that yielded nothing at this
/// offset yields nothing at the same offset again: `truncated` there
/// says the corpus is larger than the window reached, not that another
/// page exists beyond it.
fn continuation_decision(
    coverage: &wirk_atlas::AnswerCoverage,
    rows_returned: usize,
) -> ContinuationDecision {
    if coverage.no_sources || coverage.denied {
        ContinuationDecision::Withheld
    } else if coverage.continuation_unrecoverable {
        ContinuationDecision::Preserved
    } else if rows_returned == 0 {
        ContinuationDecision::Exhausted
    } else {
        ContinuationDecision::Fresh
    }
}

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
    // Ruling 0171. Resolved here rather than only inside `wirk_atlas`
    // because this is the surface that mints and checks continuation
    // tokens: the capacity a page is frozen under has to be computable
    // from the request alone, before anything is ranked, or a restated
    // request could not be compared with the token that answered it.
    let capacity = match wirk_atlas::resolve_capacity(&wirk_atlas::SearchRequest {
        scope: scope.clone(),
        requested_source: payload.source.clone(),
        query: payload.query.clone(),
        families: Vec::new(),
        semantic,
        limit,
        capacity: payload.capacity,
        pinned: None,
        offset: 0,
        semantic_query: None,
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    }) {
        Ok(capacity) => capacity,
        Err(detail) => return err_reply("BadRequest", &detail),
    };
    // The query-time half of the portability boundary: an explicitly
    // configured executable and an explicitly configured offline model
    // directory, or nothing. Both are refused unless absolute, by the
    // same 0089 rule the build side applies — a bare model name resolves
    // through a shared mutable cache and names no fixed bytes.
    let semantic_query = match (&payload.semantic_backend, &payload.semantic_model) {
        (None, None) => None,
        (Some(backend), Some(model)) => Some(wirk_atlas::SemanticQueryConfig {
            backend: std::path::PathBuf::from(backend),
            backend_args: payload.semantic_backend_args.clone(),
            model: std::path::PathBuf::from(model),
        }),
        _ => {
            return err_reply(
                "BadRequest",
                "a semantic query needs both --semantic-backend and --semantic-model; one \
                 without the other names no runnable configuration",
            );
        }
    };
    let mut pinned_editions: Option<BTreeMap<wirk_atlas::MembershipId, wirk_atlas::EditionId>> =
        None;
    let mut pinned_mode: Option<wirk_atlas::RankingMode> = None;
    let mut pinned_producer = wirk_atlas::PinnedProducer::Unrecorded;
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
                // A lexical answer's token carries no capacity, so what is
                // restated for comparison is whatever the token holds:
                // this check is "did the caller change the request", and
                // the separate policy check below is "was this token
                // issued under this policy at all".
                capacity: decoded.capacity.map(|_| capacity.value),
                offset: decoded.offset,
                generations: decoded.generations.clone(),
                editions: decoded.editions.clone(),
                mode: decoded.mode.clone(),
                semantic_backend: payload.semantic_backend.clone(),
                semantic_backend_args: payload.semantic_backend_args.clone(),
                semantic_model: payload.semantic_model.clone(),
                producer_configuration: decoded.producer_configuration.clone(),
                producer_identity: decoded.producer_identity.clone(),
                producer_basis: decoded.producer_basis.clone(),
            };
            // Ruling 0171. A semantic continuation issued before result
            // capacity was bound to the query was ranked under one
            // universal candidate depth for every request. Resuming it
            // now would serve a page from a different ranking function
            // under the first page's receipt, and restarting it would
            // hide that entirely — so it is refused, by name, with the
            // ordinary recovery.
            if decoded.capacity.is_none() && decoded.mode == "semantic" {
                return err_reply(
                    "ContinuationPolicyMismatch",
                    "this continuation was issued under the previous policy, which ranked every \
                     query at one fixed candidate depth; this product binds a result capacity to \
                     the query itself, so there is no capacity on the token to reproduce this \
                     page at and nothing was restarted. Re-run the query — naming --capacity if \
                     you want a result set deeper than the page you ask for",
                );
            }
            if decoded != restated {
                return err_reply(
                    "ContinuationMismatch",
                    "the continuation token names a different work/query/source/family/semantic/limit/capacity/backend than this request",
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
            pinned_editions = Some(
                decoded
                    .editions
                    .iter()
                    .map(|(membership, edition)| {
                        (
                            wirk_atlas::MembershipId(membership.clone()),
                            wirk_atlas::EditionId(edition.clone()),
                        )
                    })
                    .collect(),
            );
            pinned_mode = wirk_atlas::RankingMode::parse(&decoded.mode);
            pinned_producer = match (
                &decoded.producer_configuration,
                &decoded.producer_identity,
                decoded
                    .producer_basis
                    .as_deref()
                    .map(wirk_atlas::QueryProducerBasis::parse),
            ) {
                (Some(configuration), Some(identity), Some(Some(basis))) => {
                    wirk_atlas::PinnedProducer::Recorded(wirk_atlas::QueryProducerPin {
                        configuration: configuration.clone(),
                        identity: identity.clone(),
                        basis,
                    })
                }
                // No producer field at all is genuine pre-correction
                // history. Some but not all is a malformed token, and
                // calling that history would be false (`VERDICT.md` V2).
                (None, None, None) => wirk_atlas::PinnedProducer::Unrecorded,
                _ => wirk_atlas::PinnedProducer::Incomplete,
            };
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
        capacity: payload.capacity,
        pinned,
        offset,
        semantic_query,
        pinned_editions,
        pinned_mode,
        pinned_producer,
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
                // Frozen on the answer that actually ranked: a semantic
                // walk is bound to the capacity its first page ran at,
                // and a lexical one binds none because none applied.
                capacity: answer
                    .budget
                    .capacity_applies
                    .then_some(answer.budget.capacity),
                offset: offset + answer.hits.len(),
                generations: answer
                    .generations
                    .iter()
                    .map(|(membership, generation)| (membership.0.clone(), generation.0.clone()))
                    .collect(),
                editions: answer
                    .editions
                    .iter()
                    .map(|(membership, edition)| (membership.0.clone(), edition.0.clone()))
                    .collect(),
                mode: answer.mode.label().to_owned(),
                semantic_backend: payload.semantic_backend.clone(),
                semantic_backend_args: payload.semantic_backend_args.clone(),
                semantic_model: payload.semantic_model.clone(),
                producer_configuration: answer
                    .application
                    .as_ref()
                    .map(|application| application.producer_pin.configuration.clone()),
                producer_identity: answer
                    .application
                    .as_ref()
                    .map(|application| application.producer_pin.identity.clone()),
                producer_basis: answer
                    .application
                    .as_ref()
                    .map(|application| application.producer_pin.basis.label().to_owned()),
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
                "ranking": {
                    "mode": answer.mode.label(),
                    "editions": answer.editions.iter().map(|(membership, edition)| json!({
                        "membership": membership.0,
                        "edition": edition.0,
                    })).collect::<Vec<_>>(),
                    "application": answer.application.as_ref().map(application_json),
                },
                "coverage": coverage_json(&answer.coverage),
                "truncated": answer.truncated,
                "budget": budget_json(&answer.budget),
                "continuation": match continuation_decision(&answer.coverage, answer.hits.len()) {
                    ContinuationDecision::Withheld => None,
                    // Item 3: the page returned nothing and the offset
                    // did not move, so the only continuation this answer
                    // could mint is the request that just produced it.
                    // The walk ends here instead of repeating forever.
                    ContinuationDecision::Exhausted => None,
                    // `VERDICT.md` V2. A refused continuation returned no
                    // hit, so a *fresh* token here would advance an offset
                    // over a page that was never served, and — carrying no
                    // application, hence no producer — would refuse in
                    // turn with "issued before the query producer identity
                    // was recorded", which of a token this build minted
                    // seconds ago is simply false.
                    //
                    // The caller's own token is handed back unchanged
                    // instead. It is the thing that still resumes: the
                    // same token replays its page byte-identically once
                    // the implementation is restored, and the original
                    // contract asks exactly that a continuation be
                    // preserved or explicitly refused, never quietly
                    // replaced by one that cannot work.
                    ContinuationDecision::Preserved => payload.continuation.clone(),
                    ContinuationDecision::Fresh => {
                        Some(encode_continuation(&state.continuation_key, &continuation))
                    }
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

/// The estate-wide order in which one request acquires more than one
/// Work journal lock at once (`handle_finding_applied`'s own
/// linearization boundary is the first caller). Two requests that lock
/// the same pair in opposite orders deadlock head-on, so the order has
/// to be a property of the *Works*, never of the request.
///
/// The key is `(distance from this Work's root ancestor, WorkId)`. That
/// is not an arbitrary choice: it is the order this daemon's existing
/// multi-journal sites already take. `settle_ready` holds a Work's own
/// journal lock while `child_investigation_ready` reads a child's
/// journal, and `close_cascade` does the same walking down a container's
/// children — ancestor first, descendant second, every time. An
/// ancestor's chain is strictly shorter than its descendant's, so a
/// smaller key can never be a descendant, and this order agrees with
/// every one of those sites rather than competing with it.
/// `reevaluate_parent` is the deliberate counterpart: it drops the
/// child's lock *before* touching the parent's, and its own comment says
/// why.
///
/// Works with no ancestry between them need only *some* total order for
/// two requests to agree; the `WorkId` supplies it.
///
/// This reads journals (`fold_work`), so it must be called before any of
/// the locks it is ordering has been taken.
fn journal_lock_order(state: &Arc<WirkdState>, work_id: &WorkId) -> (usize, String) {
    let mut depth = 0usize;
    let mut seen = HashSet::new();
    seen.insert(work_id.clone());
    let mut next = fold_work(state, work_id).and_then(|work| work.parent);
    while let Some(binding) = next {
        if !seen.insert(binding.work.clone()) {
            break;
        }
        depth += 1;
        next = fold_work(state, &binding.work).and_then(|work| work.parent);
    }
    (depth, work_id.0.clone())
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
            expansions: Vec::new(),
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
            let journal = lock_journal(&journal);
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

// ---- Findings, Settlement, Assertion, Application (W-B) ------------------
//
// `knowledge/work/p3-world-loop/W-B-BUILD.md`, corrected by
// `loop-b-prepare-correct/HANDOFF.md` and `W-B-CONSTRUCTION-REVIEW.md`.
// Five things kept distinct: Evidence -> Finding -> Settlement ->
// Application -> the estate index. Settlement is never a wire field
// (§2.4): `EventKind::FindingSettled` has exactly one producer,
// `settle_ready` below, which mints it only when an admitted policy
// class's check holds against a journal fact this daemon derived
// itself. The one caller boundary this daemon actually has (§2.2): a
// fact wirkd derives from its own journals is authority; anything a
// client asserts is a label.

/// R2: the same table `wirk_core::finding_kind_name` owns, which
/// `obligation_basis` now hashes a declared decision set with. One table,
/// so a rendered kind and an admitted decision can never drift apart.
fn finding_kind_str(kind: FindingKind) -> &'static str {
    finding_kind_name(kind)
}

fn parse_finding_kind(raw: &str) -> Result<FindingKind, String> {
    match raw {
        "gap" => Ok(FindingKind::Gap),
        "contradicted_assumption" => Ok(FindingKind::ContradictedAssumption),
        "relationship" => Ok(FindingKind::Relationship),
        "verified_outcome" => Ok(FindingKind::VerifiedOutcome),
        other => Err(format!("unknown finding kind {other}")),
    }
}

fn finding_scope_str(scope: FindingScope) -> &'static str {
    match scope {
        FindingScope::WorkLocal => "work_local",
        FindingScope::EstateLocal => "estate_local",
    }
}

fn parse_finding_scope(raw: &str) -> Result<FindingScope, String> {
    match raw {
        "work_local" => Ok(FindingScope::WorkLocal),
        "estate_local" => Ok(FindingScope::EstateLocal),
        other => Err(format!("unknown finding scope {other}")),
    }
}

fn settlement_class_str(class: SettlementClass) -> &'static str {
    match class {
        SettlementClass::DeterministicVerified => "deterministic_verified",
        SettlementClass::ActorReviewed => "actor_reviewed",
        SettlementClass::ChildInvestigationConfirmed => "child_investigation_confirmed",
        SettlementClass::SupersededInOrigin => "superseded_in_origin",
    }
}

fn parse_settlement_class(raw: &str) -> Option<SettlementClass> {
    match raw {
        "deterministic_verified" => Some(SettlementClass::DeterministicVerified),
        "actor_reviewed" => Some(SettlementClass::ActorReviewed),
        "child_investigation_confirmed" => Some(SettlementClass::ChildInvestigationConfirmed),
        "superseded_in_origin" => Some(SettlementClass::SupersededInOrigin),
        _ => None,
    }
}

/// §9's public shape: `"accepted"|"partially_accepted"|"rejected"|
/// "deferred"|"superseded"`. `"rejected"` reads `reason` (empty if
/// absent); `"superseded"` requires `superseded_by`.
fn parse_decision(
    raw: &str,
    reason: Option<String>,
    superseded_by: Option<String>,
) -> Result<Decision, String> {
    match raw {
        "accepted" => Ok(Decision::Accepted),
        "partially_accepted" => Ok(Decision::PartiallyAccepted),
        "rejected" => Ok(Decision::Rejected {
            reason: reason.unwrap_or_default(),
        }),
        "deferred" => Ok(Decision::Deferred),
        "superseded" => match superseded_by {
            Some(id) => Ok(Decision::Superseded(FindingId(id))),
            None => Err("decision superseded requires --superseded-by".to_string()),
        },
        other => Err(format!("unknown decision {other}")),
    }
}

fn decision_json(decision: &Decision) -> Value {
    match decision {
        Decision::Accepted => json!({"decision": "accepted"}),
        Decision::PartiallyAccepted => json!({"decision": "partially_accepted"}),
        Decision::Rejected { reason } => json!({"decision": "rejected", "reason": reason}),
        Decision::Deferred => json!({"decision": "deferred"}),
        Decision::Superseded(id) => json!({"decision": "superseded", "superseded_by": id.0}),
    }
}

/// §3: a Source coordinate is the same already-encoded hex string
/// `wirk atlas resolve`'s own `--coordinate` uses; a hex string never
/// contains `/`, so a leading `work/` prefix unambiguously names the
/// Journal form (`work/<id>/event/<id>`).
/// The exact forms an evidence token takes, named once so a refusal can
/// quote them back rather than leaving a caller to guess which of three
/// spellings it got wrong.
const EVIDENCE_TOKEN_FORMS: &str = "an evidence token is an exact source coordinate, work/<work-id>/event/<event-id>, \
     or work/<work-id>/finding/<finding-id>";

/// Classifies one evidence token, or refuses its *form*.
///
/// A token that opens `work/` has declared itself a journal reference
/// and is held to one of the two journal shapes; a bare `finding-…` id
/// has declared itself a finding reference and is missing the Work that
/// would locate it. Neither can be a source coordinate, which is hex, so
/// neither is silently handed to `decode_coordinate` to come back as
/// "invalid coordinate hex" — the diagnostic that sent a real later Work
/// looking for a malformed coordinate it had never written
/// (`NATIVE-CHAIN-ADJUDICATION.md` G3, `raw/13-raise-and-escape.txt`).
///
/// A form refusal is decided from the token's own characters and names
/// no record, so it says nothing about what does or does not exist.
fn parse_evidence_token(raw: &str) -> Result<EvidenceRef, String> {
    if let Some(rest) = raw.strip_prefix("work/") {
        if let Some((work, finding)) = rest.split_once("/finding/")
            && !work.is_empty()
            && !finding.is_empty()
        {
            return Ok(EvidenceRef::Finding {
                work: WorkId(work.to_string()),
                finding: FindingId(finding.to_string()),
            });
        }
        if let Some((work, event)) = rest.split_once("/event/")
            && !work.is_empty()
            && !event.is_empty()
        {
            return Ok(EvidenceRef::Journal {
                work: WorkId(work.to_string()),
                event: EventId(event.to_string()),
            });
        }
        return Err(EVIDENCE_TOKEN_FORMS.to_string());
    }
    if raw.starts_with("finding-") {
        return Err(format!(
            "a bare finding id names no work and is not a source coordinate; {EVIDENCE_TOKEN_FORMS}"
        ));
    }
    Ok(EvidenceRef::Source(raw.to_string()))
}

fn evidence_ref_json(reference: &EvidenceRef) -> Value {
    match reference {
        EvidenceRef::Source(encoded) => json!({"reference": "source", "coordinate": encoded}),
        EvidenceRef::Journal { work, event } => {
            json!({"reference": "journal", "work": work.0, "event": event.0})
        }
        // The *claimed* half of a relation, rendered exactly as the
        // caller named it. What it resolved to is the outcome's job.
        EvidenceRef::Finding { work, finding } => {
            json!({"reference": "finding", "work": work.0, "finding": finding.0})
        }
    }
}

fn evidence_outcome_json(outcome: &EvidenceOutcome) -> Value {
    match outcome {
        EvidenceOutcome::Admitted {
            generation,
            object_id,
        } => json!({"outcome": "admitted", "generation": generation, "object_id": object_id}),
        EvidenceOutcome::Unavailable { reason } => {
            json!({"outcome": "unavailable", "reason": reason})
        }
        // The *resolved* half, and deliberately four separate things: an
        // admitted relation is not a settled one, and an admission route
        // is not a claim's truth.
        EvidenceOutcome::Relation {
            work,
            origin_event,
            route,
            standing,
        } => json!({
            "outcome": "admitted",
            "resolved": {"work": work.0, "origin_event": origin_event.0},
            "admitted_by": relation_route_str(*route),
            "target_standing": relation_standing_str(*standing),
        }),
    }
}

fn relation_route_str(route: RelationRoute) -> &'static str {
    match route {
        RelationRoute::OwnJournal => "own_journal",
        RelationRoute::Lineage => "lineage",
        RelationRoute::SettledEstatePublication => "settled_estate_publication",
    }
}

fn relation_standing_str(standing: RelationStanding) -> &'static str {
    match standing {
        RelationStanding::Settled => "settled",
        RelationStanding::Unsettled => "unsettled",
    }
}

fn admitted_evidence_json(item: &AdmittedEvidence) -> Value {
    let mut merged = evidence_ref_json(&item.reference);
    if let (Value::Object(target), Value::Object(source)) =
        (&mut merged, evidence_outcome_json(&item.outcome))
    {
        target.extend(source);
    }
    merged
}

/// A stable, order-independent key for one `AdmittedEvidence`'s own
/// reference — used only to compare a parent's `applies_to` against a
/// child's own as *sets* (§5.3: "applies_to must match as sets").
fn evidence_ref_key(item: &AdmittedEvidence) -> String {
    match &item.reference {
        EvidenceRef::Source(encoded) => format!("source:{encoded}"),
        EvidenceRef::Journal { work, event } => format!("journal:{}/{}", work.0, event.0),
        EvidenceRef::Finding { work, finding } => format!("finding:{}/{}", work.0, finding.0),
    }
}

fn artifact_receipt_json(receipt: &ArtifactReceipt) -> Value {
    json!({
        "name": receipt.name,
        "path": receipt.path,
        // Ruling 0145: a rendered receipt says which root its path is
        // relative to. Without it a reader would have to guess, and
        // `claims/<claim>/<name>` reads like a repository path.
        "store": receipt.store.label(),
        "digest": if receipt.digest.is_empty() { Value::Null } else { Value::String(receipt.digest.clone()) },
    })
}

fn settlement_check_json(check: &SettlementCheck) -> Value {
    match check {
        SettlementCheck::ValidatedClaim {
            work,
            claim,
            claim_event,
            proof,
            unread,
        } => {
            let mut value = json!({
                "check": "validated_claim", "work": work.0, "claim": claim.0,
                "claim_event": claim_event.0,
            });
            match proof {
                Some(proof) => {
                    value["waypoint"] = json!(proof.waypoint.0);
                    value["attempt"] = json!(proof.attempt);
                    value["world_hash"] = json!(proof.world_hash.0);
                    value["obligation"] = json!({
                        "id": proof.obligation.id, "edition": proof.obligation.edition,
                        "basis": proof.basis,
                    });
                    value["artifacts"] =
                        Value::Array(proof.artifacts.iter().map(artifact_receipt_json).collect());
                }
                None => value["obligation"] = historical_obligation_json(unread),
            }
            value
        }
        SettlementCheck::ActorReview {
            work,
            claim,
            claim_event,
            proof,
        } => json!({
            "check": "actor_review", "work": work.0, "claim": claim.0,
            "claim_event": claim_event.0,
            "waypoint": proof.waypoint.0, "attempt": proof.attempt,
            "world_hash": proof.world_hash.0,
            "intent": proof.intent,
            "recipe": proof.recipe,
            "obligation": {
                "id": proof.obligation.id, "edition": proof.obligation.edition,
                "basis": proof.basis,
            },
            "decision": finding_kind_str(proof.decision),
            "targets": proof.targets.iter().map(reviewed_target_json).collect::<Vec<_>>(),
            "report": proof.report.iter().map(artifact_receipt_json).collect::<Vec<_>>(),
        }),
        SettlementCheck::ChildReceipt {
            parent,
            waypoint,
            attempt,
            child,
            role,
            claim,
            closed_event,
            child_raise_event,
            proof,
            unread,
        } => {
            let mut value = json!({
                "check": "child_receipt", "parent": parent.0, "waypoint": waypoint.0,
                "attempt": attempt, "child": child.0, "role": role, "claim": claim.0,
                "closed_event": closed_event.0, "child_raise_event": child_raise_event.0,
            });
            match proof {
                Some(proof) => {
                    value["child_finding"] = json!(proof.confirmed_by.0);
                    value["obligation"] = json!({
                        "id": proof.obligation.id, "edition": proof.obligation.edition,
                        "basis": proof.basis,
                    });
                    value["requires"] = json!({
                        "id": proof.requires.id, "edition": proof.requires.edition,
                    });
                    value["obligated_roles"] =
                        Value::Array(proof.roles.iter().map(discharged_role_json).collect());
                }
                None => value["obligation"] = historical_obligation_json(unread),
            }
            value
        }
        SettlementCheck::SupersededBy {
            work,
            finding,
            raise_event,
        } => json!({
            "check": "superseded_by", "work": work.0, "finding": finding.0,
            "raise_event": raise_event.0,
        }),
    }
}

/// A settlement minted before the obligation-proof revision recorded no
/// obligation at all. It renders as exactly that — historical and
/// unknown — never as a zero-valued obligation and never as something
/// newly verified. Nothing this daemon mints can produce it: both
/// readiness functions always construct a real proof.
/// What this reader can establish about a settlement whose proof it
/// cannot read — and nothing more. Two genuinely different facts, said
/// apart:
///
/// - the record carries no proof fields at all (a base-era settlement);
/// - the record carries fields this revision does not interpret, shown
///   verbatim.
///
/// Neither sentence claims the past failed to record something. The
/// independent re-review's C2 was exactly that overreach.
fn historical_obligation_json(unread: &UnreadFields) -> Value {
    if unread.is_empty() {
        json!({
            "recorded": false,
            "reason": "this record carries no obligation-proof fields; this revision reads none and asserts nothing about what minted it",
        })
    } else {
        json!({
            "recorded": false,
            "reason": "this revision does not interpret the obligation-proof shape in this record; the fields it carries are shown verbatim, unread and not relied on",
            "unread_by_this_revision": unread.0,
        })
    }
}

/// The complete checked target identity, rendered. The selector the
/// Route asked for is shown beside the exact membership, source,
/// generation and object it was frozen to and the review's own admitted
/// evidence matched — so a reader can see *which* `socket.rs`, not only
/// that some file of that name was cited.
fn reviewed_target_json(target: &ReviewTarget) -> Value {
    json!({
        "selector": {"source": target.source, "path": target.path},
        "estate": target.estate,
        "membership": target.membership,
        "source_id": target.source_id,
        "generation": target.generation,
        "object_id": target.object_id,
    })
}

fn discharged_role_json(role: &DischargedRole) -> Value {
    json!({
        "role": role.role,
        "child": role.child.0,
        "claim": role.claim.0,
        "finding": role.finding.0,
        "mechanism": {"id": role.mechanism.id, "edition": role.mechanism.edition},
        "mechanism_basis": role.mechanism_basis,
        "settled_event": role.settled_event.0,
    })
}

/// The exact, limited statement a settled check proves, with the
/// immutable identity that discharged it. Never the Finding's own
/// sentence, and never a general claim of semantic truth: a settlement
/// says one Route-authored, estate-admitted obligation was discharged by
/// one named receipt, and says nothing else.
///
/// A settlement whose `proof` is absent was minted before this contract
/// existed. It renders `recorded: false` with an explicit historical
/// reason and a `null` statement — readable, honest about what is
/// unknown, and never dressed up as a verified proof.
fn settlement_proves_json(check: &SettlementCheck) -> Value {
    match check {
        SettlementCheck::ValidatedClaim {
            work,
            claim,
            claim_event,
            proof,
            unread,
        } => match proof {
            Some(proof) => json!({
                "recorded": true,
                "statement": proof.proves,
                "obligation": {
                    "id": proof.obligation.id, "edition": proof.obligation.edition,
                    "basis": proof.basis,
                },
                "discharged_by": {
                    "kind": "validated_claim",
                    "waypoint": proof.waypoint.0, "attempt": proof.attempt,
                    "world_hash": proof.world_hash.0, "claim": claim.0,
                    "artifacts": proof.artifacts.iter().map(artifact_receipt_json).collect::<Vec<_>>(),
                },
            }),
            None => historical_proves_json(
                unread,
                json!({
                    "kind": "validated_claim", "work": work.0, "claim": claim.0,
                    "claim_event": claim_event.0,
                }),
            ),
        },
        // The agentic mechanism states its own standing in the record.
        // A deterministic settlement proves a command's outcome; this
        // proves that an admitted review happened, under an admitted
        // recipe, over admitted targets, and recorded a declared
        // decision — and says so, rather than borrowing the
        // deterministic vocabulary or implying its conclusion is true.
        SettlementCheck::ActorReview { claim, proof, .. } => json!({
            "recorded": true,
            "statement": proof.proves,
            "obligation": {
                "id": proof.obligation.id, "edition": proof.obligation.edition,
                "basis": proof.basis,
            },
            "standing": "an admitted independent review was performed under this recipe over these exact admitted targets and recorded this declared decision; whether its conclusion is true is judgement, not proof",
            "discharged_by": {
                "kind": "actor_review",
                "waypoint": proof.waypoint.0, "attempt": proof.attempt,
                "world_hash": proof.world_hash.0, "claim": claim.0,
                "intent": proof.intent,
                "recipe": proof.recipe,
                "decision": finding_kind_str(proof.decision),
                "targets": proof.targets.iter().map(reviewed_target_json).collect::<Vec<_>>(),
                "report": proof.report.iter().map(artifact_receipt_json).collect::<Vec<_>>(),
            },
        }),
        SettlementCheck::ChildReceipt {
            waypoint,
            attempt,
            child,
            role,
            claim,
            proof,
            unread,
            ..
        } => match proof {
            Some(proof) => json!({
                "recorded": true,
                "statement": proof.proves,
                "obligation": {
                    "id": proof.obligation.id, "edition": proof.obligation.edition,
                    "basis": proof.basis,
                },
                "discharged_by": {
                    "kind": "child_receipt",
                    "waypoint": waypoint.0, "attempt": attempt,
                    "child": child.0, "role": role, "claim": claim.0,
                    "child_finding": proof.confirmed_by.0,
                    "requires": {"id": proof.requires.id, "edition": proof.requires.edition},
                    "obligated_roles": proof.roles.iter().map(discharged_role_json).collect::<Vec<_>>(),
                },
            }),
            None => historical_proves_json(
                unread,
                json!({
                    "kind": "child_receipt", "waypoint": waypoint.0, "attempt": attempt,
                    "child": child.0, "role": role, "claim": claim.0,
                }),
            ),
        },
        // A Work replacing its own provisional record proves no
        // verification obligation and says so, rather than borrowing the
        // vocabulary of one.
        SettlementCheck::SupersededBy { finding, .. } => json!({
            "recorded": true,
            "statement": "this Work replaced its own earlier provisional finding; no verification obligation is discharged",
            "obligation": Value::Null,
            "discharged_by": {"kind": "superseded_by", "superseding_finding": finding.0},
        }),
    }
}

fn historical_proves_json(unread: &UnreadFields, discharged_by: Value) -> Value {
    let mut value = json!({
        "recorded": false,
        "statement": Value::Null,
        "obligation": Value::Null,
        "discharged_by": discharged_by,
    });
    if unread.is_empty() {
        value["historical"] = json!(
            "this record carries no obligation-proof fields, so this reader has nothing to render as proved; it makes no claim about what the revision that minted it did or did not record"
        );
    } else {
        value["historical"] = json!(
            "this revision does not interpret the obligation-proof shape in this record; what the record carries is shown under `unread_by_this_revision`, disclosed and not relied on, because the rule those values were computed under is not this revision's rule"
        );
        value["unread_by_this_revision"] = json!(unread.0);
    }
    value
}

fn settlement_json(settlement: &Settlement) -> Value {
    json!({
        "authority": {
            "policy": {
                "class": settlement_class_str(settlement.authority.class),
                "policy_version": settlement.authority.policy_version,
                "policy_digest": settlement.authority.policy_digest,
            },
        },
        "check": settlement_check_json(&settlement.check),
        // W-B obligation proof: *what this settlement proves* — the
        // Route-authored, policy-admitted statement of the discharged
        // obligation and the immutable receipt that discharged it. The
        // Finding's own `claim` sentence is rendered beside it as a
        // recorded, unverified sentence (`finding_json`), so a reader is
        // never shown free text as the settled thing.
        "proves": settlement_proves_json(&settlement.check),
        "settled_by_event": settlement.settled_by.0,
        "at": settlement.at.0,
        "minted_at_startup": settlement.minted_at_startup,
    })
}

/// §2.5: rendered "unverified" always — an assertion is a recorded
/// label, never a checked identity, whatever the peer credential says.
fn assertion_json(assertion: &Assertion) -> Value {
    json!({
        "decision": decision_json(&assertion.decision),
        "by": format!("recorded name: {}, unverified", assertion.by),
        "verified": false,
        // The operator's own sentence, exactly as recorded — `null` when
        // none was given, never a caption standing in for one. Ruling
        // 0114's first carried gap: `Decision` carries a `reason` only
        // on `Rejected`, so a sentence supplied with any other decision
        // was journaled durably and then rendered to nobody, `--admin`
        // included. It is prose, so it goes through
        // `withhold_assertion_prose` on its recorded author like the
        // rejected reason already did, and the two renderings of the one
        // sentence count once.
        "reason": assertion.reason,
        "peer": {"uid": assertion.peer.uid, "gid": assertion.peer.gid},
        "at": assertion.at.0,
        // The one *checked* thing beside the unverified label: which
        // requester wirkd admitted when it wrote this
        // (`ASSERTION-AUTHOR-ADJUDICATION.md`). It is a journal identity,
        // it is what the prose gate reads, and a reader that is shown a
        // withheld reason is entitled to see whose reason it was.
        "author": match &assertion.author {
            Some(AssertingAuthor::Work(work)) => json!({"author": "work", "work": work.0}),
            Some(AssertingAuthor::Administrator) => json!({"author": "administrator"}),
            None => json!({"author": "unknown", "recorded_before_authorship": true}),
        },
    })
}

fn attribution_json(attribution: &Attribution) -> Value {
    match attribution {
        Attribution::Claim {
            work,
            run,
            claim,
            claim_event,
        } => json!({
            "attribution": "claim", "work": work.0, "run": run.0, "claim": claim.0,
            "claim_event": claim_event.0,
        }),
        Attribution::Asserted { by, peer, producer } => json!({
            "attribution": "asserted",
            "by": format!("recorded name: {by}, unverified"),
            "verified": false,
            "peer": {"uid": peer.uid, "gid": peer.gid},
            "producer": {
                "work": producer.work.0,
                "run": producer.run.0,
                "world_hash": producer.world_hash.0,
            },
        }),
    }
}

/// The recorded resource identity at one generation point. `object_id`
/// stays exactly what the journal holds; `resource` names the fact a
/// bare `null` left the reader to guess — W-B-AUTHORITY-ADJUDICATION.md's
/// "explicit deleted-resource absence". `handle_finding_applied` refuses
/// the one case that could otherwise reach here meaning something else
/// (a resource Atlas recorded with no object id), so absence here is
/// deletion and nothing else. An emptied file is `present` with a real
/// zero-byte object id, which is a different fact and reads as one.
fn generation_point_json(point: &wirk_core::GenerationPoint) -> Value {
    json!({
        "generation": point.generation,
        "object_id": point.object_id,
        "resource": if point.object_id.is_some() { "present" } else { "absent" },
    })
}

fn application_ref_json(application: &ApplicationRef) -> Value {
    json!({
        "source": application.source,
        "before": generation_point_json(&application.before),
        "after": generation_point_json(&application.after),
        "revision": application.revision,
        "attribution": attribution_json(&application.attribution),
        "implements_finding": {
            "by": format!("recorded name: {}, unverified", application.implements_finding.by),
            "verified": false,
            "peer": {
                "uid": application.implements_finding.peer.uid,
                "gid": application.implements_finding.peer.gid,
            },
            "at": application.implements_finding.at.0,
        },
    })
}

fn finding_json(work_id: &WorkId, id: &FindingId, record: &FindingRecord) -> Value {
    json!({
        "id": id.0,
        "work": work_id.0,
        "run": record.finding.run.0,
        "waypoint": record.finding.waypoint.0,
        "kind": finding_kind_str(record.finding.kind),
        "scope": finding_scope_str(record.finding.scope),
        // W-B obligation proof / authority review §1: a settled Finding
        // used to render its free-text sentence under `settled`, so the
        // reader saw the sentence as the settled thing. It is now
        // rendered exactly as an Assertion's own `by` already is — a
        // recorded, unverified string — with `claim_text` carrying the
        // machine-readable sentence and `settled.proves` carrying what
        // the check actually proves. This makes the *claim* honest about
        // its standing; it does not declare findings universally
        // unverifiable, and an unsettled Finding's claim reads the same
        // way it always did.
        "claim": format!("recorded claim: {}, unverified", record.finding.claim),
        "claim_text": record.finding.claim,
        "claim_verified": false,
        "obligation": record.finding.obligation.as_ref().map(|obligation| json!({
            "id": obligation.id, "edition": obligation.edition,
        })),
        "confirmed_by": record.finding.confirmed_by.as_ref().map(|reference| json!({
            "work": reference.work.0, "finding": reference.finding.0,
        })),
        "evidence": record.finding.evidence.iter().map(admitted_evidence_json).collect::<Vec<_>>(),
        "contradicts": record.finding.contradicts.iter().map(admitted_evidence_json).collect::<Vec<_>>(),
        "applies_to": record.finding.applies_to.iter().map(admitted_evidence_json).collect::<Vec<_>>(),
        "supersedes": record.finding.supersedes.as_ref().map(|id| id.0.clone()),
        "proposed_change": record.finding.proposed_change,
        "settled": match &record.state {
            FindingState::Settled(settlement) => Some(settlement_json(settlement)),
            FindingState::Proposed => None,
        },
        "assertions": record.assertions.iter().map(assertion_json).collect::<Vec<_>>(),
        // W-B-CORRECT.md defect 3: every Application, oldest first —
        // never just the latest, which would erase history a newer
        // generation's own Application does not undo.
        "applied": record.applied.iter().map(application_ref_json).collect::<Vec<_>>(),
    })
}

// ---- Scoped presentation (W-B-DISCLOSURE-REPAIR.md) -----------------------

/// One requester's own view of the estate's Finding records, shared by
/// `finding list` and the `atlas findings` index so both draw the
/// identical boundary rather than each re-deriving one.
///
/// `withheld` counts the record parts this view replaced. The count is
/// reported and the identities are not — the same shape
/// `wirk_atlas::AdmissionSummary` already uses for a denied membership
/// on `atlas search`, and the reason a caller can tell "there is nothing
/// here" from "there is something here you may not see" without learning
/// what.
struct DisclosureView<'a> {
    requester: &'a Work,
    requester_events: &'a [Event],
    lineage: &'a HashSet<WorkId>,
    withheld: usize,
}

impl<'a> DisclosureView<'a> {
    fn new(
        requester: &'a Work,
        requester_events: &'a [Event],
        lineage: &'a HashSet<WorkId>,
    ) -> Self {
        Self {
            requester,
            requester_events,
            lineage,
            withheld: 0,
        }
    }

    /// Whether this requester may be shown everything read out of
    /// `work_id`'s own checkout — an artifact path and digest, a
    /// compiled World. Its bindings must cover that Work's own.
    fn admits_work_checkout(&self, state: &Arc<WirkdState>, work_id: &WorkId) -> bool {
        let Some(work) = fold_work(state, work_id) else {
            // A Work whose journal this daemon cannot read discloses
            // nothing it can vouch for: fail closed.
            return false;
        };
        work.repositories
            .iter()
            .all(|binding| requester_grants_alias(self.requester, &binding.name))
    }

    fn admits_alias(&self, alias: &str) -> bool {
        requester_grants_alias(self.requester, alias)
    }

    /// Whether one frozen evidence entry may be rendered in full: a
    /// `Source` against the requester's own scope, a `Journal` through
    /// the identical recursive walk raise-time admission performs, so a
    /// wrapper cannot launder a coordinate into a *listing* either.
    fn admits_evidence(&self, state: &Arc<WirkdState>, item: &AdmittedEvidence) -> bool {
        match &item.reference {
            EvidenceRef::Source(encoded) => {
                let disclosure = SourceDisclosure {
                    coordinates: vec![encoded.clone()],
                    ..SourceDisclosure::default()
                };
                disclosure_admitted(state, self.requester, &disclosure)
            }
            EvidenceRef::Journal { work, event } => {
                let mut walk = ReferenceWalk::new(self.lineage);
                journal_reference_admitted(
                    state,
                    self.requester,
                    self.requester_events,
                    &mut walk,
                    work,
                    event,
                )
                .is_ok()
            }
            // A recorded relation is rendered to a *later* reader only
            // if that reader would itself be admitted to the named
            // record right now. The author's own admission, frozen at
            // raise time, is not transferable: a narrower reader of this
            // finding learns that a part was withheld and nothing else.
            EvidenceRef::Finding { work, finding } => {
                let mut walk = ReferenceWalk::new(self.lineage);
                finding_reference_admitted(
                    state,
                    self.requester,
                    self.requester_events,
                    &mut walk,
                    work,
                    finding,
                )
                .is_ok()
            }
        }
    }
}

/// The one shape every withheld part takes. It says that something was
/// withheld and nothing whatever about what: no alias, path, generation,
/// object id, encoded coordinate or claim text travels in it. A caller
/// that could tell two withheld parts apart could enumerate the estate
/// through the marker itself.
fn withheld_json() -> Value {
    json!({"withheld": true})
}

fn evidence_array_scoped(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    items: &[AdmittedEvidence],
) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| {
                if view.admits_evidence(state, item) {
                    admitted_evidence_json(item)
                } else {
                    view.withheld += 1;
                    withheld_json()
                }
            })
            .collect(),
    )
}

/// A settled record, with its source-disclosing half withheld when the
/// requester's own bindings do not reach it.
///
/// What survives is deliberate, not residual: the policy class, version
/// and digest; the settlement's own event and timestamp; and the check's
/// **journal** identities — which Work, Claim, Claim event, container
/// activation, child and role settled it. That is the "useful admitted
/// cross-Work/child outcome" a narrowed child legitimately consumes: it
/// learns that its sibling's obligated role closed, and it learns
/// nothing about the sources that closed it.
///
/// What is withheld is `proves` and the proof half of `check`, together
/// and as whole objects rather than field by field. Those carry the
/// review targets' exact membership/generation/object identities, the
/// artifact paths and digests, and — for a historical record — arbitrary
/// unread JSON that an earlier revision wrote artifact paths into.
/// Redacting them individually would mean re-auditing this function
/// every time a proof gains a field; withholding the object means a new
/// field is withheld by default.
fn settlement_json_scoped(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    settlement: &Settlement,
) -> Value {
    let mut disclosure = SourceDisclosure::default();
    let mut from_producing_checkout = false;
    settlement_source_disclosure(settlement, &mut disclosure, &mut from_producing_checkout);
    let checkout_admitted = !from_producing_checkout
        || match settlement_producing_work(&settlement.check) {
            Some(work) => view.admits_work_checkout(state, work),
            None => false,
        };
    if checkout_admitted && disclosure_admitted(state, view.requester, &disclosure) {
        return settlement_json(settlement);
    }
    view.withheld += 1;
    let mut value = settlement_json(settlement);
    value["check"] = settlement_check_identities_json(&settlement.check);
    value["proves"] = withheld_json();
    value
}

/// Whose checkout a settlement's artifact receipts were read out of.
/// `SupersededBy` names no execution at all, and a `ChildReceipt`'s own
/// unread historical fields belong to the parent that closed the stage.
fn settlement_producing_work(check: &SettlementCheck) -> Option<&WorkId> {
    match check {
        SettlementCheck::ValidatedClaim { work, .. }
        | SettlementCheck::ActorReview { work, .. } => Some(work),
        SettlementCheck::ChildReceipt { parent, .. } => Some(parent),
        SettlementCheck::SupersededBy { .. } => None,
    }
}

/// A settled check's journal identities alone — every field of
/// `settlement_check_json` that names a Work, Run, Claim, Event,
/// Finding, Waypoint, role or activation, and no field that names a
/// source, a path, a generation, an object or an artifact digest.
fn settlement_check_identities_json(check: &SettlementCheck) -> Value {
    match check {
        SettlementCheck::ValidatedClaim {
            work,
            claim,
            claim_event,
            ..
        } => json!({
            "check": "validated_claim", "work": work.0, "claim": claim.0,
            "claim_event": claim_event.0, "proof": withheld_json(),
        }),
        SettlementCheck::ActorReview {
            work,
            claim,
            claim_event,
            proof,
        } => json!({
            "check": "actor_review", "work": work.0, "claim": claim.0,
            "claim_event": claim_event.0,
            "waypoint": proof.waypoint.0, "attempt": proof.attempt,
            "decision": finding_kind_str(proof.decision),
            "proof": withheld_json(),
        }),
        SettlementCheck::ChildReceipt {
            parent,
            waypoint,
            attempt,
            child,
            role,
            claim,
            closed_event,
            child_raise_event,
            ..
        } => json!({
            "check": "child_receipt", "parent": parent.0, "waypoint": waypoint.0,
            "attempt": attempt, "child": child.0, "role": role, "claim": claim.0,
            "closed_event": closed_event.0, "child_raise_event": child_raise_event.0,
            "proof": withheld_json(),
        }),
        // Nothing here is source-disclosing, so this arm is reached only
        // for symmetry and renders exactly as it always does.
        SettlementCheck::SupersededBy { .. } => settlement_check_json(check),
    }
}

/// An Application's mechanical half is entirely source identity — the
/// alias, both generation points, the object ids and the published
/// revision — so an unadmitted source withholds it whole. Its
/// attribution and its asserted judgement are journal identities and a
/// recorded unverified name, and stay.
fn application_json_scoped(view: &mut DisclosureView, application: &ApplicationRef) -> Value {
    if view.admits_alias(&application.source) {
        return application_ref_json(application);
    }
    view.withheld += 1;
    let mut value = application_ref_json(application);
    for field in ["source", "before", "after", "revision"] {
        value[field] = withheld_json();
    }
    value
}

/// Whether one requester may be shown the free prose the Work behind a
/// record authored: its claim sentence, its proposed change, an
/// assertion's rejection reason.
///
/// `CHILD-PRODUCER-DISCLOSURE-ADJUDICATION.md`, on the executed
/// counterexample (`loop-b-child-disclosure-control/raw/scen/41-H.json`):
/// a child narrowed to `ledger+public` received its parent's authored
/// claim naming a `vaultx`-only sentinel, while an unrelated Work with
/// the *identical* bindings was refused the same row whole. Lineage is
/// permission to consult the family's journal; it is not a source grant,
/// so it cannot widen what an authored sentence may quote.
///
/// The rule is the one the off-lineage publication route already
/// applies to a whole row (`published_row_scoped`, condition 3),
/// narrowed to the prose: free text carries no provenance finer than its
/// author — nothing in the sentence says which source a phrase came from
/// — so the requester must admit *every* binding of the authoring Work,
/// or the prose is withheld. Publication is unaffected: a requester that
/// fails this check off lineage never reached the row at all.
///
/// `author` is the Work that *wrote* the prose, which for a claim
/// sentence and a proposed change is the Work whose journal holds them,
/// and for an assertion is routinely not
/// (`ASSERTION-AUTHOR-ADJUDICATION.md`). Callers pass the author; only
/// `admits_assertion_prose` knows where an assertion's comes from.
fn admits_authored_prose(state: &Arc<WirkdState>, view: &DisclosureView, author: &WorkId) -> bool {
    view.admits_work_checkout(state, author)
}

/// The same rule, applied to the one authored sentence whose author is
/// *not* the Work whose journal holds it.
///
/// `ASSERTION-AUTHOR-ADJUDICATION.md`, on the executed counterexample in
/// `loop-b-lineage-prose-verify/raw/32-matrix-green.txt` section C: a
/// parent holding `vaultx` asserted a rejection on a narrowed child's
/// own Finding, and the child — reading its *own* journal, which it of
/// course admits whole — was handed a reason quoting the `vaultx`-only
/// sentinel in clear. Gating on the holder is exactly what made that
/// pass; the sentence's provenance is `Assertion.author`, and nothing
/// else in the record carries it.
///
/// Two cases withhold from every scoped requester rather than guess:
///
/// - `Administrator`: the unscoped path names no source breadth at all,
///   so no requester's bindings can be said to cover it. `--admin` reads
///   never reach here and stay total.
/// - `None`: an assertion older than this field. Its author is genuinely
///   unknown, and the two labels beside it — `by` and `peer` — are the
///   very things §2.5 records as attribution and refuses as identity.
///   Falling back to the holder would re-admit the leak verbatim, since
///   the leaking requester is precisely the one that admits the holder.
///   Old records keep their content and their standing; what narrows is
///   only who is shown the sentence.
fn admits_assertion_prose(
    state: &Arc<WirkdState>,
    view: &DisclosureView,
    author: Option<&AssertingAuthor>,
) -> bool {
    match author {
        Some(AssertingAuthor::Work(work)) => admits_authored_prose(state, view, work),
        Some(AssertingAuthor::Administrator) | None => false,
    }
}

/// One record's authored free prose, withheld as whole fields through
/// the standard marker, returning how many authored parts were replaced.
///
/// The claim sentence counts once though it is rendered twice (`claim`
/// and `claim_text` are one authored thing in two forms), and a record
/// with no proposed change had nothing to withhold and counts nothing —
/// the same "counts, never identities" discipline `withhold_status_content`
/// keeps, so the number stays a number.
///
/// Everything structural survives: the record's own id, kind, scope,
/// origin events, obligation *name*, `confirmed_by`, the admitted
/// evidence entries and the settlement's journal half. That is the
/// narrowed consultation a child legitimately has, and it is why this is
/// not "an opinion is unpublishable" — the author's own Work reads its
/// prose in full, so does any requester as broad as the author, and
/// `--admin` is untouched.
fn withhold_authored_prose(target: &mut Value) -> usize {
    let mut withheld = 0usize;
    let mut claim_withheld = false;
    for key in ["claim", "claim_text"] {
        if let Some(slot) = target.get_mut(key)
            && !slot.is_null()
        {
            *slot = withheld_json();
            claim_withheld = true;
        }
    }
    if claim_withheld {
        withheld += 1;
    }
    if let Some(slot) = target.get_mut("proposed_change")
        && !slot.is_null()
    {
        *slot = withheld_json();
        withheld += 1;
    }
    withheld
}

/// An assertion's own free prose. The recorded decision, the recorded
/// name, the peer credential, the timestamp and the recorded author are
/// journal-side and stay: what a narrowed reader loses is the sentence,
/// never the fact that a rejection was recorded, nor who recorded it.
///
/// The reason is carried twice on the record — inside
/// `Decision::Rejected` and again in `Assertion.reason` — so both
/// renderings are replaced wherever they appear, and the pair counts
/// once: one authored sentence, one withholding, the same "counts,
/// never identities" discipline `withhold_authored_prose` keeps.
fn withhold_assertion_prose(assertion: &mut Value) -> usize {
    let mut withheld = false;
    for pointer in ["/decision/reason", "/reason"] {
        if let Some(slot) = assertion.pointer_mut(pointer)
            && !slot.is_null()
        {
            *slot = withheld_json();
            withheld = true;
        }
    }
    usize::from(withheld)
}

/// `finding_json`, rendered for one requester. Journal identities, the
/// obligation *name*, an assertion's recorded decision and the
/// settlement's own journal half are disclosed on reference permission;
/// every source-disclosing part goes through the view, and the authored
/// prose — which `0110` left uncensored on the reference route and the
/// child-producer adjudication corrected — goes through
/// `admits_authored_prose`.
fn finding_json_scoped(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    work_id: &WorkId,
    id: &FindingId,
    record: &FindingRecord,
) -> Value {
    let mut value = finding_json(work_id, id, record);
    if !admits_authored_prose(state, view, work_id) {
        view.withheld += withhold_authored_prose(&mut value);
    }
    // The assertions are gated one at a time, on their own recorded
    // authors, and never on the Work whose journal holds them: an
    // assertion's author is routinely a different Work, and on this very
    // record the two can differ from each other as well.
    let mut assertion_prose = 0usize;
    if let Some(assertions) = value["assertions"].as_array_mut() {
        for (assertion, recorded) in assertions.iter_mut().zip(record.assertions.iter()) {
            if !admits_assertion_prose(state, view, recorded.author.as_ref()) {
                assertion_prose += withhold_assertion_prose(assertion);
            }
        }
    }
    view.withheld += assertion_prose;
    value["evidence"] = evidence_array_scoped(state, view, &record.finding.evidence);
    value["contradicts"] = evidence_array_scoped(state, view, &record.finding.contradicts);
    value["applies_to"] = evidence_array_scoped(state, view, &record.finding.applies_to);
    if let FindingState::Settled(settlement) = &record.state {
        value["settled"] = settlement_json_scoped(state, view, settlement);
    }
    value["applied"] = Value::Array(
        record
            .applied
            .iter()
            .map(|application| application_json_scoped(view, application))
            .collect(),
    );
    value
}

/// Reads one Work's journal. This is the acquisition every decision
/// path reaches another Work through, so it is where the journal lock
/// discipline (ruling 0119) is asserted.
fn replay_events(state: &Arc<WirkdState>, work_id: &WorkId) -> Option<Vec<Event>> {
    no_journal_guard_held("replay_events");
    let journal = journal_for(state, work_id).ok().flatten()?;
    let journal = lock_journal(&journal);
    journal.replay().ok()
}

fn fold_work(state: &Arc<WirkdState>, work_id: &WorkId) -> Option<Work> {
    let events = replay_events(state, work_id)?;
    if events.is_empty() {
        return None;
    }
    Some(fold(&events))
}

/// §3's "journal kinship is not universal evidence access": a Journal
/// evidence reference is admitted only when its named Work is the
/// raising Work itself or lies on that Work's own parent/child lineage —
/// walked upward through `Work.parent` and downward through every
/// journaled `ChildWorkSpawned.child`, recursively. A sibling or
/// unrelated Work is never in lineage, however same-estate it is.
///
/// `own_events` is the raising Work's own already-replayed journal,
/// handed in rather than re-read: every caller of this function
/// (`handle_finding_raise`, via `admit_evidence`) is already holding
/// that Work's own journal lock at the moment it calls this — `Mutex`
/// is not reentrant, so re-locking the same journal here (as an earlier
/// draft's `replay_events(state, &raising.id)` did on the very first
/// BFS step) deadlocks every self-referencing evidence token, which is
/// the overwhelmingly common case. Every *other* Work's journal is a
/// different lock and is read normally.
/// The full set of Works `raising` is entitled to treat as its own
/// lineage: itself, every ancestor, and every descendant, recursively
/// (§3's "journal kinship is not universal evidence access" — this is
/// the exact boundary, computed once so every caller — raise-time
/// evidence admission, `finding list`'s own requester scoping — draws
/// the identical set rather than re-deriving it (W-B-CORRECT.md defect
/// 2: "selecting an origin Work id is not a general evidence grant").
fn lineage_of(
    state: &Arc<WirkdState>,
    raising: &Work,
    own_events: &[Event],
) -> std::collections::HashSet<WorkId> {
    let mut lineage = std::collections::HashSet::new();
    lineage.insert(raising.id.clone());
    let mut ancestor = raising.parent.clone();
    while let Some(binding) = ancestor {
        if !lineage.insert(binding.work.clone()) {
            break;
        }
        ancestor = fold_work(state, &binding.work).and_then(|work| work.parent);
    }
    let mut frontier = vec![raising.id.clone()];
    let mut seen = std::collections::HashSet::new();
    while let Some(work_id) = frontier.pop() {
        if !seen.insert(work_id.clone()) {
            continue;
        }
        let events = if work_id == raising.id {
            own_events.to_vec()
        } else {
            let Some(events) = replay_events(state, &work_id) else {
                continue;
            };
            events
        };
        for event in &events {
            if let EventKind::ChildWorkSpawned { child, .. } = &event.kind {
                lineage.insert(child.clone());
                frontier.push(child.clone());
            }
        }
    }
    lineage
}

/// §3: admits one list of evidence tokens against `raising`'s own scope.
/// A `Source` coordinate must resolve inside a membership this Work's
/// own `repositories` grant; a `Journal` reference must name an event on
/// this Work's own lineage. Either kind of authority violation refuses
/// the *whole* raise with `InadmissibleEvidence` (§3: "refuse the whole
/// raise... naming which entry"); a resolvable-but-absent/forged/drifted
/// entry is recorded `Unavailable` and never promoted later.
fn admit_evidence(
    state: &Arc<WirkdState>,
    raising: &Work,
    own_events: &[Event],
    tokens: &[String],
) -> Result<Vec<AdmittedEvidence>, (&'static str, String)> {
    let mut out = Vec::new();
    // Computed once for the whole call: `lineage_of` walks every
    // ancestor and descendant journal, and the recursive admission below
    // consults the same set at every hop.
    let lineage = lineage_of(state, raising, own_events);
    for token in tokens {
        let reference = match parse_evidence_token(token) {
            Ok(reference) => reference,
            // A *form* error, not an authority one: the token could not
            // name anything, so nothing was looked up and nothing about
            // the estate is disclosed by saying so.
            Err(detail) => return Err(("BadRequest", format!("{token}: {detail}"))),
        };
        match reference {
            EvidenceRef::Source(encoded) => {
                let coordinate = decode_coordinate(&encoded)
                    .map_err(|detail| ("InadmissibleEvidence", format!("{token}: {detail}")))?;
                let atlas = state
                    .atlas
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                let scope = wirk_atlas::QueryScope::Work(raising.repositories.clone());
                let Some(membership) =
                    admitted_membership_for(&atlas, &scope, &coordinate.membership)
                else {
                    return Err((
                        "InadmissibleEvidence",
                        format!("{token}: membership is not admitted by this Work's own bindings"),
                    ));
                };
                match atlas.resolve_exact(&membership, &coordinate) {
                    Ok(wirk_atlas::ResolveOutcome::Resolved(_)) => out.push(AdmittedEvidence {
                        reference: EvidenceRef::Source(encoded),
                        outcome: EvidenceOutcome::Admitted {
                            generation: coordinate.generation.0.clone(),
                            object_id: coordinate.object_id.clone(),
                        },
                    }),
                    Ok(other) => out.push(AdmittedEvidence {
                        reference: EvidenceRef::Source(encoded),
                        outcome: EvidenceOutcome::Unavailable {
                            reason: format!("{other:?}"),
                        },
                    }),
                    Err(err) => {
                        return Err(("InadmissibleEvidence", format!("{token}: {err}")));
                    }
                }
            }
            EvidenceRef::Journal { work, event } => {
                // The *reference route*, checked first and unchanged: a
                // Work outside this one's own lineage refuses the whole
                // raise, however admissible its sources might be. Source
                // admission below is a second gate, never a replacement
                // for this one.
                if !lineage.contains(&work) {
                    return Err((
                        "InadmissibleEvidence",
                        format!(
                            "{token}: work {} is not this Work's own journal or its parent/child lineage",
                            work.0
                        ),
                    ));
                }
                // The *disclosure* half: everything the referenced
                // record structurally names, and everything each record
                // it points at names in turn, must be admitted by this
                // requesting Work's own bindings. A resolvable-but-denied
                // reference is `Unavailable`, exactly like a
                // resolvable-but-absent one — established
                // denied-versus-absent semantics, with the reason
                // carrying no alias, path, generation or token.
                let mut walk = ReferenceWalk::new(&lineage);
                let admission = journal_reference_admitted(
                    state, raising, own_events, &mut walk, &work, &event,
                );
                let reference = EvidenceRef::Journal {
                    work: work.clone(),
                    event: event.clone(),
                };
                match admission {
                    Ok(()) => out.push(AdmittedEvidence {
                        reference,
                        outcome: EvidenceOutcome::Admitted {
                            generation: work.0.clone(),
                            object_id: event.0.clone(),
                        },
                    }),
                    Err(reason) => out.push(AdmittedEvidence {
                        reference,
                        outcome: EvidenceOutcome::Unavailable { reason },
                    }),
                }
            }
            EvidenceRef::Finding { work, finding } => {
                // Every failure of this route — no such Work, no such
                // Finding, the wrong pair, an id from another estate, a
                // record this Work may not discover, one not settled,
                // one whose own sources are denied — refuses the whole
                // raise with one message. That is deliberate on two
                // counts. It never records a relation to a target that
                // was not there (the executed defect
                // `NATIVE-CHAIN-ADJUDICATION.md` names: "recorded
                // nonexistent event self as Unavailable evidence"), and
                // one message for every failure means the refusal
                // discloses nothing about which of them happened.
                let mut walk = ReferenceWalk::new(&lineage);
                let Ok((origin_event, route, standing)) = finding_reference_admitted(
                    state, raising, own_events, &mut walk, &work, &finding,
                ) else {
                    return Err((
                        "InadmissibleEvidence",
                        format!("{token}: no such finding is admitted to this work"),
                    ));
                };
                out.push(AdmittedEvidence {
                    reference: EvidenceRef::Finding {
                        work: work.clone(),
                        finding,
                    },
                    outcome: EvidenceOutcome::Relation {
                        work,
                        origin_event,
                        route,
                        standing,
                    },
                });
            }
        }
    }
    Ok(out)
}

// ---- The disclosure boundary (W-B-DISCLOSURE-REPAIR.md) -------------------
//
// One sentence governs every surface below: **lineage grants permission
// to *reference* another Work's journal; it never grants *disclosure* of
// that journal's sources.**
//
// The two halves are checked separately and neither substitutes for the
// other. The reference route is `lineage_of` — unchanged, and still a
// whole-raise `InadmissibleEvidence` when it fails. Source disclosure is
// the requesting Work's *own* `repositories` bindings, applied to
// whatever the referenced record structurally names, however many
// journal hops away it is.
//
// **What "structurally names" means, exactly.** A record part is
// source-disclosing when it carries an Atlas source identity — a source
// or membership alias, a generation id, an object id, an encoded exact
// coordinate — or content read out of a source checkout: a resolved
// path, an artifact digest, a compiled World's worktree, argv or
// environment, a base SHA. A part carrying only journal identities
// (Work, Run, Claim, Event, Finding, Waypoint, role, attempt), policy
// identities, Route-authored prose, timestamps or content-addressed
// basis/World hashes is not source-disclosing, and is returned on
// reference permission alone. That is what keeps settled EstateLocal
// learning genuinely reusable by a later admitted Work rather than
// solving disclosure by banning cross-Work evidence.
//
// **The honest limit.** This is a *structural* provenance boundary, not
// semantic censorship. A Finding's own `claim` sentence, a Route's
// `intent`, a `proposed_change`, a `WorkCanceled` reason and the
// `--reason` an operator types at `wirk work fail` are free prose
// *authored by a person about the work*: a proposer that copies an
// embargoed path into its own English claim has disclosed it, and
// nothing here inspects prose to prevent that.
//
// Free prose is decided by **which producer writes the field**, never
// by reading the string. A `LifecycleObserved` `detail` is not in that
// list and never was: `RunLoop` fills it with the actor's own captured
// pane screen (`HerdrClient::read_pane` on the `Blocked` transition) —
// execution output read out of the producing Work's own checkout, not
// an authored sentence — and `RunFailed.cause.detail` likewise carries
// launch and transport diagnostics that name real filesystem paths.
// Both are classified below by content presence, exactly like launch
// metadata (`W-B-DISCLOSURE-RESPONSE-REPAIR.md`: "the free-prose limit
// does not make captured pane text safe source content; it means
// semantic censorship was not promised").
//
// Nor does any of this authenticate a caller: the same OS
// uid runs an honest operator's terminal and an actor's shell
// (`PeerIdentity`'s own doc), so `--admin` is an explicitly *named*
// surface, never a proven one.

/// Everything one journal event structurally discloses, and everything
/// it points at. Built per event by `event_source_disclosure`, which is
/// an exhaustive match over `EventKind` on purpose: a new variant must
/// be classified here before it can be referenced as evidence, rather
/// than defaulting to "discloses nothing" the way the base's own
/// `FindingRaised`-only check did.
#[derive(Default)]
struct SourceDisclosure {
    /// Source/membership aliases this record names or is derived from.
    aliases: BTreeSet<String>,
    /// Encoded exact Atlas coordinates embedded in this record.
    coordinates: Vec<String>,
    /// Journal references embedded in this record, followed recursively
    /// — a wrapper is not a wall.
    journal_refs: Vec<(WorkId, EventId)>,
    /// Finding references embedded in this record, followed recursively
    /// through their own admission route for exactly the same reason: a
    /// relation is a wrapper too, and a reader that may not reach the
    /// named record may not reach it by reading someone's disagreement
    /// with it either.
    finding_refs: Vec<(WorkId, FindingId)>,
}

/// `producing` is the referenced Work's own `repositories`, used for the
/// records whose content comes out of that Work's checkout without
/// naming an alias of its own to check more narrowly: an artifact's
/// resolved path and digest, a Deterministic World's cwd/argv/env, a
/// historical settlement's unread fields. Those disclose the producing
/// Work's binding set as a set, because the record itself offers nothing
/// finer to bind them to.
/// Whether a resolved launch selection carries any operator-authored
/// content at all. All three fields travel the same way — `wirk run`
/// reads `--actor-model`/`--actor-effort` through the same unvalidated
/// `flag_value` that fills `args`, and a Route file's `AuthoredSelection`
/// carries all three verbatim — so all three answer together. There is
/// deliberately no vocabulary check anywhere in this product to lean on:
/// the user owns their harness configuration (0106) and any string is a
/// legal model or effort.
fn selection_carries_content(selection: &ActorSelection) -> bool {
    selection.model.is_some() || selection.effort.is_some() || !selection.args.is_empty()
}

fn event_source_disclosure(event: &Event, producing: &[RepositoryBinding]) -> SourceDisclosure {
    let mut out = SourceDisclosure::default();
    // Set by the arms whose content is read out of the producing Work's
    // own checkout rather than named against a specific source.
    let mut from_producing_checkout = false;

    match &event.kind {
        // Journal identities only: a vanished Run, a Claim id, a Run
        // identity, a container activation, a hold's own unmet
        // declared-output *names*, a child spawn's role, an unverified
        // human assertion. None carries a source coordinate, a path or
        // a checkout-derived digest.
        //
        // `WorkFailed` and `WorkCanceled` stay here on the *producer*
        // test this whole classification runs on, not on a reading of
        // their strings. Both are written only by an operator verb —
        // `handle_work_fail` puts `wirk work fail --reason <text>`
        // straight into `cause.detail`, and `WorkCanceled.reason` is
        // `wirk work cancel --reason` — so their text is authored prose
        // about the Work, the named free-prose limit above. Their
        // sibling `RunFailed` is classified separately below precisely
        // because *its* producers are execution paths, not a person.
        EventKind::RunVanished
        | EventKind::ClaimFiled { .. }
        | EventKind::RunOpened { .. }
        | EventKind::WorkFailed { .. }
        | EventKind::WorkCanceled { .. }
        | EventKind::ContainerActivated { .. }
        | EventKind::StageHeld { .. }
        | EventKind::ChildWorkSpawned { .. }
        // W-C3: a Waypoint id, an observation id, a content id, a format
        // tag and two digests. No alias, no coordinate, no path, no
        // checkout-derived content — the same answer `WaypointReserved`
        // already gives for the *reference* half of its own World (its
        // alias comes from `repository`, not from the projection it
        // carries). The delivered content itself is reachable only
        // through `world show` under the triple, which is a different
        // surface with its own gate.
        | EventKind::ProjectionExpanded { .. }
        | EventKind::FindingAsserted { .. } => {}

        // The two execution-output events (the independent launch
        // review's F-B). Neither is authored by a person and neither
        // names an alias or a coordinate, so both take the identical
        // content-present/content-absent answer the launch events take
        // below:
        //
        // - `LifecycleObserved.detail` is `RunLoop`'s own capture. On
        //   the `Blocked` transition it is literally the actor's pane
        //   screen (`read_pane`), which is whatever the actor printed
        //   out of the producing Work's checkout; on a pane release it
        //   names the pane and Run it closed. `None` for every other
        //   status this loop journals, which is the overwhelming
        //   majority of lifecycle events — so the useful positive a
        //   narrowed child keeps is the whole `Working`/`Idle`/
        //   `Claimed` lifecycle stream, unchanged.
        // - `RunFailed.cause.detail` is the launch/transport diagnostic
        //   (`RunLoop::record_run_failed`, the stuck observation,
        //   `handle_fail`'s own executor report). Those strings carry
        //   real filesystem paths in practice — a blocked `wirk run`
        //   produces `connecting to <path>/.herdr/herdr.sock: ...`. A
        //   `RunFailed` with no detail (`status` alone, an HTTP-shaped
        //   failure, the retry supersession's own journal-identity
        //   text) still discloses nothing.
        EventKind::LifecycleObserved { detail, .. } => {
            from_producing_checkout = detail.is_some();
        }
        EventKind::RunFailed { cause } => {
            from_producing_checkout = cause.detail.is_some();
        }

        // The producing Work's whole binding set, verbatim on the wire —
        // the broadest single disclosure in the journal, and the one the
        // base returned unconditionally.
        EventKind::WorkSubmitted { repositories, .. } => {
            for binding in repositories {
                out.aliases.insert(binding.name.clone());
            }
        }
        EventKind::WorktreeCreated { repo, .. } => {
            out.aliases.insert(repo.clone());
        }
        // The compiled World. An `ActorWorld` names its own repository
        // and freezes its review targets against named sources, so both
        // are checkable exactly; a `DeterministicWorld` carries cwd,
        // argv and env with no alias at all, so it discloses the
        // producing Work's bindings.
        EventKind::WaypointReserved { world, .. } => match world {
            World::Actor(actor) => {
                out.aliases.insert(actor.repository.clone());
                for target in &actor.review_targets {
                    out.aliases.insert(target.source.clone());
                }
            }
            World::Deterministic(_) => from_producing_checkout = true,
        },
        // P3 native launch, one classification across all three events
        // (the independent currentness verification's V-2, corrected by
        // the launch review's F-A). What is *mechanism* here is decided
        // by who mints the field, not by what its string looks like:
        //
        // - `actor_kind` is a closed harness identity this product
        //   resolves itself, and `holder`'s pid and start token are
        //   kernel facts `wirkd` reads off `SO_PEERCRED` and `/proc`
        //   and discards whatever a client sent. Neither can carry
        //   checkout content, so both disclose nothing.
        // - everything else on these three events is an unvalidated
        //   operator-authored launch input, or Herdr's echo of one:
        //   `selection.args`, `selection.model` and `selection.effort`
        //   are read by the identical `flag_value` call at `wirk run`
        //   (`--actor-model`/`--actor-effort`/raw args) with no
        //   vocabulary check and no validation of any kind, or come
        //   verbatim off a Route file; `launch_argv` is Herdr's own
        //   reply about what it submitted, containing those same
        //   tokens plus the harness wrapper's; `destination` is a
        //   filesystem path, the client's own canonicalized Herdr
        //   socket.
        //
        // The launch review executed the counterexample the earlier
        // "model and effort name no source" comment ruled out by
        // assertion: a `RunLaunchRequested` whose only source-derived
        // content is `selection.model = <a denied checkout>/embargoed.md`,
        // with `args`, `launch_argv` and `destination` all empty. It
        // was admitted to a narrowed child. Keeping arbitrary model and
        // effort strings executable is deliberate (ruling 0106: the
        // user owns their harness configuration, and this product
        // holds no model catalogue and adds no permission gate) — so
        // the classification, not the input, is what has to be honest.
        //
        // None of these fields carries an alias or an Atlas coordinate,
        // so none can be checked more narrowly than the Work that
        // produced it. That is exactly the case `World::Deterministic`
        // (cwd, argv, env, no alias) and `ClaimRecorded` (artifact
        // paths) already answer with the producing Work's own binding
        // set, and it is the answer here: content present -> the
        // producer's bindings; content absent -> nothing to disclose.
        //
        // What the empty case means, exactly. It means the record
        // carries no launch content for this daemon to disclose — and
        // that is *all* it means. It is **not** evidence that the Run
        // launched bare: a `RunLaunched` written before these fields
        // existed folds through `#[serde(default)]` to the same empty
        // shape, and the estate's own pre-field launch path really did
        // pass `--model sonnet` plus a `--settings <estate root>/…`
        // pair for every claude Run. Those launches had arguments; the
        // journal simply never recorded them. Absent is unrecorded,
        // never none (the launch review's F-D).
        //
        // Cost, recorded rather than hidden (F-E), stated as the
        // launch code actually behaves (the integration review's V-3).
        // Every real *attempt* has a destination, and a launch that
        // resolves any of model, effort or args carries content — so
        // after this correction a narrowed child can cite another
        // Work's launch event only when its own bindings already cover
        // that Work's whole binding set. A model is *not* always
        // resolved: `HerdrExecutor::start_actor_agent` adds no flag at
        // all when model and effort are absent and the harness's own
        // native default runs, so a Run submitted with no
        // `--actor-model`/`--actor-effort` against a Route authoring no
        // selection produces a live `RunLaunchRequested` with an empty
        // selection, which stays admissible. The preserved
        // content-absent positive is therefore two records, not one:
        // the unrecorded pre-field launch, and the live launch that
        // genuinely resolved nothing to record. Both are honest —
        // neither carries launch content for this daemon to disclose —
        // and neither is evidence about the other. (In this estate's
        // own Routes model, effort and args are authored, so its real
        // launches do carry content; that is a fact about these Routes,
        // not about the product.) Fail-closed is the right default for
        // opaque
        // source-bearing content with no finer provenance, and no
        // narrower projection of these fields exists to name; if one is
        // ever wanted it must say exactly what it reveals and be tested
        // as such, never be justified as "mechanism" over a record that
        // carries content.
        EventKind::RunLaunchRequested { selection, .. } => {
            from_producing_checkout = selection_carries_content(selection);
        }
        EventKind::RunLaunched {
            selection,
            launch_argv,
            ..
        } => {
            from_producing_checkout =
                selection_carries_content(selection) || !launch_argv.is_empty();
        }
        EventKind::RunLaunchAttempted { destination, .. } => {
            from_producing_checkout = !destination.is_empty();
        }
        EventKind::ClaimRecorded { artifacts, .. } => {
            from_producing_checkout = !artifacts.is_empty();
        }
        EventKind::StageClosed { receipts, .. } => {
            from_producing_checkout = receipts.iter().any(receipt_carries_artifacts);
        }
        EventKind::FindingRaised { finding } => {
            for item in finding
                .evidence
                .iter()
                .chain(finding.contradicts.iter())
                .chain(finding.applies_to.iter())
            {
                match &item.reference {
                    EvidenceRef::Source(encoded) => out.coordinates.push(encoded.clone()),
                    EvidenceRef::Journal { work, event } => {
                        out.journal_refs.push((work.clone(), event.clone()))
                    }
                    EvidenceRef::Finding { work, finding } => {
                        out.finding_refs.push((work.clone(), finding.clone()))
                    }
                }
            }
        }
        EventKind::FindingSettled { settlement, .. } => {
            settlement_source_disclosure(settlement, &mut out, &mut from_producing_checkout);
        }
        EventKind::FindingApplied { application, .. } => {
            out.aliases.insert(application.source.clone());
        }
    }

    if from_producing_checkout {
        for binding in producing {
            out.aliases.insert(binding.name.clone());
        }
    }
    out
}

/// A container receipt is satisfied by nested receipts; a leaf's own
/// `artifacts` are the checkout-derived part, at whatever depth.
fn receipt_carries_artifacts(receipt: &OutcomeReceipt) -> bool {
    match receipt {
        OutcomeReceipt::Leaf { artifacts, .. } => !artifacts.is_empty(),
        OutcomeReceipt::Child { .. } => false,
        OutcomeReceipt::Container { receipts, .. } => {
            receipts.iter().any(receipt_carries_artifacts)
        }
    }
}

/// A settlement's own source-disclosing parts. `ChildProof` and
/// `SupersededBy` carry only journal identities and content-addressed
/// bases, which is why a narrowed child can still be told *that* its
/// sibling's role settled. `UnreadFields` are arbitrary historical JSON
/// — the intermediate revision wrote artifact paths there — so a record
/// carrying any is treated as checkout-derived rather than inspected
/// field by field.
fn settlement_source_disclosure(
    settlement: &Settlement,
    out: &mut SourceDisclosure,
    from_producing_checkout: &mut bool,
) {
    match &settlement.check {
        SettlementCheck::ValidatedClaim { proof, unread, .. } => {
            if !unread.is_empty()
                || proof
                    .as_ref()
                    .is_some_and(|proof| !proof.artifacts.is_empty())
            {
                *from_producing_checkout = true;
            }
        }
        SettlementCheck::ActorReview { proof, .. } => {
            for target in &proof.targets {
                out.aliases.insert(target.source.clone());
            }
            if !proof.report.is_empty() {
                *from_producing_checkout = true;
            }
        }
        SettlementCheck::ChildReceipt { unread, .. } => {
            if !unread.is_empty() {
                *from_producing_checkout = true;
            }
        }
        SettlementCheck::SupersededBy { .. } => {}
    }
}

/// Whether `requester`'s own bindings admit every source `disclosure`
/// names. An alias is matched against the requester's own
/// `repositories` (the identical rule `admitted_membership_for` applies
/// to a scope, at the alias level); a coordinate goes through
/// `admitted_membership_for` itself, so the catalog — never the caller's
/// token — decides which membership it names.
fn disclosure_admitted(
    state: &Arc<WirkdState>,
    requester: &Work,
    disclosure: &SourceDisclosure,
) -> bool {
    if !disclosure
        .aliases
        .iter()
        .all(|alias| requester_grants_alias(requester, alias))
    {
        return false;
    }
    if disclosure.coordinates.is_empty() {
        return true;
    }
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let scope = wirk_atlas::QueryScope::Work(requester.repositories.clone());
    disclosure.coordinates.iter().all(|encoded| {
        decode_coordinate(encoded).is_ok_and(|coordinate| {
            admitted_membership_for(&atlas, &scope, &coordinate.membership).is_some()
        })
    })
}

fn requester_grants_alias(requester: &Work, alias: &str) -> bool {
    requester
        .repositories
        .iter()
        .any(|binding| binding.name == alias)
}

/// The most references one admission walk follows. A journal graph is
/// append-only and finite, and `seen` already collapses cycles and
/// repetition, so this is a denial-of-service bound on a deliberately
/// wide fan-out, not a correctness mechanism — which is why exceeding it
/// is an honest refusal rather than a silent truncation that would
/// admit the very reference it stopped short of checking.
const EVIDENCE_REFERENCE_BUDGET: usize = 512;

/// One recursive admission walk over the evidence reference graph.
struct ReferenceWalk<'a> {
    lineage: &'a HashSet<WorkId>,
    /// `(work, event)` pairs already admitted on this walk. A repeated
    /// or cyclic reference discloses nothing new and must not be
    /// followed again.
    seen: HashSet<(String, String)>,
    steps: usize,
}

impl<'a> ReferenceWalk<'a> {
    fn new(lineage: &'a HashSet<WorkId>) -> Self {
        Self {
            lineage,
            seen: HashSet::new(),
            steps: 0,
        }
    }
}

/// Admits one journal reference for `requester`, recursively.
///
/// The referenced event must lie on `requester`'s own lineage, and
/// everything it structurally discloses must be admitted by
/// `requester`'s own bindings — and then the same two questions are
/// asked of every reference *it* embeds, to any depth. That recursion is
/// the whole point: the base checked a `FindingRaised`'s direct `Source`
/// coordinates and dropped its `Journal` ones on the floor, so wrapping
/// an embargoed coordinate in one extra finding laundered it past a
/// check the direct citation already refused.
///
/// `requester_events` is the requester's own already-replayed journal,
/// handed in for the same reason `lineage_of` takes it: `Mutex` is not
/// reentrant and `admit_evidence`'s caller is holding that journal's
/// lock. Every other Work's journal is a different lock and is read
/// normally.
///
/// Every `Err` names the *requester's own* event id and nothing else. A
/// reason that quoted the alias, path, generation, object id or encoded
/// token it refused would disclose exactly what it denied.
fn journal_reference_admitted(
    state: &Arc<WirkdState>,
    requester: &Work,
    requester_events: &[Event],
    walk: &mut ReferenceWalk,
    work: &WorkId,
    event_id: &EventId,
) -> Result<(), String> {
    if !walk.seen.insert((work.0.clone(), event_id.0.clone())) {
        return Ok(());
    }
    walk.steps += 1;
    if walk.steps > EVIDENCE_REFERENCE_BUDGET {
        return Err(format!(
            "{}: this reference's own graph is wider than this daemon follows in one admission",
            event_id.0
        ));
    }
    if !walk.lineage.contains(work) {
        return Err(format!(
            "{}: it references a work outside the requesting work's own parent/child lineage",
            event_id.0
        ));
    }
    // Borrowed, never cloned: a self-referencing chain would otherwise
    // copy the requester's whole journal once per hop, and this
    // recursion is bounded by step count rather than by journal size.
    let replayed;
    let events: &[Event] = if work == &requester.id {
        requester_events
    } else {
        let Some(events) = replay_events(state, work) else {
            return Err(format!(
                "{}: it references a work with no readable journal",
                event_id.0
            ));
        };
        replayed = events;
        &replayed
    };
    let Some(event) = events.iter().find(|candidate| &candidate.id == event_id) else {
        return Err(format!("no event {} in work {}", event_id.0, work.0));
    };
    let producing = fold(events).repositories;
    let disclosure = event_source_disclosure(event, &producing);
    if !disclosure_admitted(state, requester, &disclosure) {
        return Err(format!(
            "{}: its own sources are not admitted by the requesting work's own bindings",
            event_id.0
        ));
    }
    for (nested_work, nested_event) in &disclosure.journal_refs {
        journal_reference_admitted(
            state,
            requester,
            requester_events,
            walk,
            nested_work,
            nested_event,
        )?;
    }
    for (nested_work, nested_finding) in &disclosure.finding_refs {
        finding_reference_admitted(
            state,
            requester,
            requester_events,
            walk,
            nested_work,
            nested_finding,
        )
        .map_err(|()| {
            format!(
                "{}: it references a finding this work is not admitted to",
                event_id.0
            )
        })?;
    }
    Ok(())
}

/// Admits one `EvidenceRef::Finding` for `requester`, and reports what
/// it resolved to.
///
/// Two routes, and no third. **Journal kinship**, which is the existing
/// `Journal` rule reached through the record's own `FindingRaised`
/// event, so a relation to a family record admits exactly what citing
/// that event already admits — same recursion, same source-disclosure
/// half, same cycle budget. **Settled estate publication**, for a Work
/// with no kinship at all: the named record must be one the estate
/// findings index would already publish to this very requester, decided
/// by `published_row_scoped` itself rather than by a second rule
/// alongside it — one gate, so "which records may I name" and "which
/// records may I discover" cannot drift apart and turn a relation into a
/// way of learning that something exists.
///
/// The four conditions that gate carries (a policy receipt not an
/// opinion, no `SupersededBy`, the producing and publishing checkouts
/// both admitted, every frozen review target admitted) are
/// `LATER-DISCOVERY-ADJUDICATION.md`'s, unchanged and not restated here.
/// What this adds is the scope check `all_finding_rows` performs before
/// a row exists at all: a `WorkLocal` finding is never published and is
/// never nameable off lineage.
///
/// Every failure returns the same `Err(())`. The caller turns it into
/// one message for all of them, so a Work cannot learn from a refusal
/// whether the record it named exists.
///
/// `requester_events` is handed in for the reason `lineage_of` and
/// `journal_reference_admitted` both state: the raise-time caller holds
/// the requester's own journal lock and `Mutex` is not reentrant. Every
/// read of the requester's own journal below goes through that slice,
/// and the off-lineage route — where the named Work is by definition not
/// the requester — is the only place another journal is folded.
fn finding_reference_admitted(
    state: &Arc<WirkdState>,
    requester: &Work,
    requester_events: &[Event],
    walk: &mut ReferenceWalk,
    work: &WorkId,
    finding: &FindingId,
) -> Result<(EventId, RelationRoute, RelationStanding), ()> {
    let replayed;
    let events: &[Event] = if work == &requester.id {
        requester_events
    } else {
        let Some(events) = replay_events(state, work) else {
            return Err(());
        };
        replayed = events;
        &replayed
    };
    let holder = fold(events);
    let Some(record) = holder.findings.get(finding) else {
        return Err(());
    };
    let Some((origin_event, _)) = find_raised_finding(events, finding) else {
        return Err(());
    };
    let standing = match &record.state {
        FindingState::Settled(_) => RelationStanding::Settled,
        _ => RelationStanding::Unsettled,
    };
    if walk.lineage.contains(work) {
        journal_reference_admitted(
            state,
            requester,
            requester_events,
            walk,
            work,
            &origin_event,
        )
        .map_err(|_| ())?;
        let route = if work == &requester.id {
            RelationRoute::OwnJournal
        } else {
            RelationRoute::Lineage
        };
        return Ok((origin_event, route, standing));
    }
    // Off lineage. Only a settled EstateLocal publication, and only the
    // one this requester could already have discovered.
    if record.finding.scope != FindingScope::EstateLocal {
        return Err(());
    }
    let FindingState::Settled(settlement) = &record.state else {
        return Err(());
    };
    // The publication gate folds the producing Work's own journal. Off
    // lineage the *holder* is never the requester, and a producer that
    // were the requester would put the holder on the requester's own
    // lineage and never reach here — so this guard is unreachable in
    // practice and fails closed rather than risk re-locking a journal
    // this call already holds.
    if settlement_producing_work(&settlement.check) == Some(&requester.id) {
        return Err(());
    }
    let Some(row_event) = events.iter().find_map(|event| match &event.kind {
        EventKind::FindingSettled {
            finding: settled, ..
        } if settled == finding => Some(event.id.clone()),
        _ => None,
    }) else {
        return Err(());
    };
    let row = wirk_atlas::FindingRow {
        id: wirk_atlas::FindingRowId::compute(
            finding,
            wirk_atlas::FindingRowKind::Settled,
            &row_event,
        ),
        kind: wirk_atlas::FindingRowKind::Settled,
        finding: record.finding.clone(),
        origin: wirk_atlas::FindingOrigin {
            work: work.clone(),
            raised_event: origin_event.clone(),
            row_event,
        },
        settlement: Some((**settlement).clone()),
        assertion: None,
        applied: None,
        superseded_by: match &settlement.check {
            SettlementCheck::SupersededBy { finding, .. } => Some(finding.clone()),
            _ => None,
        },
    };
    let mut view = DisclosureView::new(requester, requester_events, walk.lineage);
    if published_row_scoped(state, &mut view, &row).is_none() {
        return Err(());
    }
    Ok((
        origin_event,
        RelationRoute::SettledEstatePublication,
        standing,
    ))
}

/// A Finding's own owning Work, found by the estate-wide directory scan
/// every other cross-Work sweep in this file already uses
/// (`reevaluate_waiting_works`'s own convention) — a `FindingId` alone
/// names no journal directly, so `finding assert`/`settle`/`applied`
/// (none of which carry a triple) resolve it this way.
fn find_finding_owner(
    state: &Arc<WirkdState>,
    finding_id: &FindingId,
) -> Option<(WorkId, Vec<Event>)> {
    let works_dir = state.estate_root.join("works");
    let entries = std::fs::read_dir(&works_dir).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Pure discovery: read the journal that is there, create
        // nothing, and read one an operator left read-only
        // (`discovery_events`). The mutation this sweep decides on
        // still goes through the one write path below.
        let Some(events) = discovery_events(&dir) else {
            continue;
        };
        if events.is_empty() {
            continue;
        }
        let work = fold(&events);
        if work.findings.contains_key(finding_id) {
            return Some((work.id.clone(), events));
        }
    }
    None
}

/// `<id>@<edition>` — the two coordinates that *name* a verification
/// obligation. Neither half may be empty and `@` must appear exactly
/// once, so `security-audit` (no edition) and `a@b@c` are refused at the
/// wire rather than silently never matching anything.
fn parse_obligation_ref(token: &str) -> Result<ObligationRef, String> {
    let mut parts = token.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(id), Some(edition), None) if !id.is_empty() && !edition.is_empty() => {
            Ok(ObligationRef {
                id: id.to_string(),
                edition: edition.to_string(),
            })
        }
        _ => Err(format!(
            "--obligation must be <id>@<edition>, got {token:?}"
        )),
    }
}

/// `work/<work-id>/finding/<finding-id>` — the same
/// `work/<id>/event/<id>` shape a `Journal` evidence token already uses,
/// one noun over.
fn parse_confirmed_by(token: &str) -> Result<ConfirmedBy, String> {
    let parts: Vec<&str> = token.split('/').collect();
    match parts.as_slice() {
        ["work", work, "finding", finding] if !work.is_empty() && !finding.is_empty() => {
            Ok(ConfirmedBy {
                work: WorkId(work.to_string()),
                finding: FindingId(finding.to_string()),
            })
        }
        _ => Err(format!(
            "--confirmed-by must be work/<work-id>/finding/<finding-id>, got {token:?}"
        )),
    }
}

fn handle_finding_raise(state: &Arc<WirkdState>, payload: super::FindingRaisePayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();
    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the claim's estate root does not identify this daemon's estate",
        );
    }
    let journal_handle = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    // Payload shape, decided before anything is observed: these depend
    // on nothing but the request, so they never need re-deciding when
    // the loop below re-reads.
    //
    // W-B obligation proof: `obligation` and `confirmed_by` are both
    // pointers the caller names and wirkd re-derives everything about
    // later (`deterministic_verified_readiness`, `child_investigation_ready`).
    // Only their *shape* is checked here, so a malformed token is a
    // `BadRequest` at the wire rather than a silently unsettleable
    // Finding.
    let kind = match parse_finding_kind(&payload.kind) {
        Ok(kind) => kind,
        Err(message) => return err_reply("BadRequest", &message),
    };
    let scope = match parse_finding_scope(&payload.scope) {
        Ok(scope) => scope,
        Err(message) => return err_reply("BadRequest", &message),
    };
    let obligation = match payload.obligation.as_deref().map(parse_obligation_ref) {
        Some(Ok(obligation)) => Some(obligation),
        Some(Err(message)) => return err_reply("BadRequest", &message),
        None => None,
    };
    let confirmed_by = match payload.confirmed_by.as_deref().map(parse_confirmed_by) {
        Some(Ok(reference)) => Some(reference),
        Some(Err(message)) => return err_reply("BadRequest", &message),
        None => None,
    };
    let supersedes = payload.supersedes.clone().map(FindingId);
    // Ruling 0119, the journal lock discipline: `admit_evidence` reads
    // whichever Works the evidence names — an ancestor, a descendant, or
    // any estate publisher at all — so it runs with **no** journal guard
    // held. Observe this Work's own journal, release it, decide, then
    // re-acquire and re-check that nothing was appended in between
    // before appending. That is `cancel_work`'s pattern one verb over
    // ("re-check terminality under this Work's own lock"), and the
    // re-check is what keeps a decision from resting on authority that
    // has since moved: the Run retried, the Work canceled or completed,
    // a child spawned. All of those are events on this same journal, so
    // an unmoved journal is an unchanged decision, and appending under
    // the guard the check ran under is what makes a concurrent raise a
    // loser that re-reads rather than a lost update.
    let mut attempt = 0usize;
    let (mut journal, run, evidence, contradicts, applies_to) = loop {
        attempt += 1;
        let events = {
            let journal = lock_journal(&journal_handle);
            match journal.replay() {
                Ok(events) => events,
                Err(err) => return err_reply("JournalError", &err.to_string()),
            }
        };
        let Some(run) = find_run(&events, &run_id) else {
            return err_reply("TripleMismatch", "no such run");
        };
        let work = fold(&events);
        if work.id != work_id {
            return err_reply("TripleMismatch", "triple does not match this work");
        }
        if work.state.is_terminal() {
            return err_reply(
                "WorkTerminal",
                "the Work is already terminal: no further Finding can be raised against it",
            );
        }
        // §5.2: "wirkd refuses a Run that is not current for its Waypoint" —
        // the same currency check `record`/`spawn_child_on_parent` already
        // apply, so a superseded (retried) Run's actor cannot backdate
        // evidence onto a generation that has already moved on. It is
        // re-asked below, under the guard, against the same journal this
        // observation read: a retry that lands mid-admission moves the
        // journal and loses this lap rather than backdating a Finding.
        if latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0)
            != Some(run_id.clone())
        {
            return err_reply("TripleMismatch", "the run is not current for its waypoint");
        }
        // No guard is held here, by construction, and this is the whole
        // reason the loop exists.
        let evidence = match admit_evidence(state, &work, &events, &payload.evidence) {
            Ok(evidence) => evidence,
            Err((code, message)) => return err_reply(code, &message),
        };
        let contradicts = match admit_evidence(state, &work, &events, &payload.contradicts) {
            Ok(evidence) => evidence,
            Err((code, message)) => return err_reply(code, &message),
        };
        let applies_to = match admit_evidence(state, &work, &events, &payload.applies_to) {
            Ok(evidence) => evidence,
            Err((code, message)) => return err_reply(code, &message),
        };
        // §3: "an EstateLocal finding must have at least one Admitted
        // entry; one whose every entry is Unavailable is refused" — this
        // closes the hole a parseable-but-unresolvable coordinate plus a
        // granted alias would otherwise open.
        if scope == FindingScope::EstateLocal
            && !evidence
                .iter()
                .any(|item| matches!(item.outcome, EvidenceOutcome::Admitted { .. }))
        {
            return err_reply(
                "NoAdmittedEvidence",
                "an EstateLocal finding needs at least one admitted evidence entry",
            );
        }
        if let Some(target) = &supersedes
            && !work.findings.contains_key(target)
        {
            return err_reply(
                "UnknownFinding",
                "supersedes names no finding raised in this Work",
            );
        }
        // Re-acquire and re-check. An unmoved journal means every check
        // above still holds — they are all folded from these events —
        // and the append below happens under this same guard.
        let journal = lock_journal(&journal_handle);
        let events_now = match journal.replay() {
            Ok(events_now) => events_now,
            Err(err) => return err_reply("JournalError", &err.to_string()),
        };
        if same_observation(&events, &events_now) {
            break (journal, run, evidence, contradicts, applies_to);
        }
        drop(journal);
        if attempt >= JOURNAL_OBSERVATION_ATTEMPTS {
            return err_reply(
                "Conflict",
                "this Work's journal moved under every attempt to admit this evidence: retry the raise",
            );
        }
    };
    let finding = Finding {
        id: FindingId(mint_id("finding")),
        work: work_id.clone(),
        run: run_id.clone(),
        waypoint: run.waypoint.clone(),
        kind,
        scope,
        claim: payload.claim,
        evidence,
        contradicts,
        applies_to,
        supersedes,
        proposed_change: payload.proposed_change,
        obligation,
        confirmed_by,
    };
    let event = new_event(
        &work_id,
        Some(run_id.clone()),
        EventKind::FindingRaised {
            finding: finding.clone(),
        },
    );
    if let Err(err) = append_event(state, &mut journal, &work_id, &event) {
        return err_reply("JournalError", &err.to_string());
    }
    let events_now = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let record = fold(&events_now)
        .findings
        .get(&finding.id)
        .cloned()
        .expect("the finding just folded from the event this call just appended");
    drop(journal);
    if let Err(err) = settle_ready(state, &work_id, false) {
        eprintln!(
            "wirkd: settlement evaluation after raise failed for {}: {err}",
            work_id.0
        );
    }
    // Re-read once more: `settle_ready` may have just settled this very
    // finding (`deterministic-verified` naming an already-Claimed leaf).
    let record = fold_work(state, &work_id)
        .and_then(|work| work.findings.get(&finding.id).cloned())
        .unwrap_or(record);
    ok_reply(finding_json(&work_id, &finding.id, &record))
}

/// §2.5: the complete, usable human/client path. Never sets a Finding
/// `Settled` and never suppresses it from later consultation
/// (`fold`'s own rule) — this verb only ever appends `FindingAsserted`.
///
/// W-B disclosure response repair (`loop-b-disclosure-verify/VERDICT.md`
/// C2): this verb *writes* to the target finding's own Work, so scope
/// admission is checked before the append, not laundered into a scoped
/// reply after an unscoped write. `--requesting-work`/`--admin` is the
/// same exclusive pair `finding list` carries; a non-admin requester off
/// the target Work's own lineage is refused outright — the journal is
/// left unchanged, never written and then hidden.
fn handle_finding_assert(
    state: &Arc<WirkdState>,
    payload: super::FindingAssertPayload,
    peer: PeerIdentity,
) -> Reply {
    if payload.admin == payload.requester.is_some() {
        return err_reply(
            "BadRequest",
            "name exactly one of --requesting-work <id> (scoped) or --admin (unscoped)",
        );
    }
    let finding_id = FindingId(payload.finding.clone());
    let Some((work_id, _events)) = find_finding_owner(state, &finding_id) else {
        return err_reply("NotFound", "no such finding");
    };
    // Scope admission is a read boundary, never proof that a human
    // approved the assertion and never settlement permission
    // (W-B-AUTHORITY-ADJUDICATION.md) — it only decides whether this
    // requester may reference and write to this Work's own journal at
    // all, exactly as a Journal evidence reference does at raise time.
    let requester_view = if payload.admin {
        None
    } else {
        let requester_id = payload
            .requester
            .as_ref()
            .expect("the exclusivity check above admitted a requester");
        let Some(requester_events) = replay_events(state, requester_id) else {
            return err_reply("NotFound", "no such requesting work");
        };
        let requester = fold(&requester_events);
        let lineage = lineage_of(state, &requester, &requester_events);
        if !lineage.contains(&work_id) {
            return err_reply(
                "InadmissibleEvidence",
                "the target finding's own work is not the requesting work's own journal or its parent/child lineage",
            );
        }
        Some((requester, requester_events, lineage))
    };
    let decision = match parse_decision(
        &payload.decision,
        payload.reason.clone(),
        payload.superseded_by,
    ) {
        Ok(decision) => decision,
        Err(message) => return err_reply("BadRequest", &message),
    };
    // `ASSERTION-AUTHOR-ADJUDICATION.md`: the admission decided just
    // above is the only trustworthy statement of who wrote this
    // sentence, and this is the one moment it exists. The record it is
    // appended to belongs to `work_id`, which — on every cross-Work
    // assertion this verb deliberately admits — is somebody else.
    let author = Some(match payload.requester.as_ref() {
        Some(requester_id) => AssertingAuthor::Work(requester_id.clone()),
        None => AssertingAuthor::Administrator,
    });
    let assertion = Assertion {
        decision,
        by: payload.by,
        reason: payload.reason,
        peer,
        at: now_ts(),
        author,
    };
    let journal = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let mut journal = lock_journal(&journal);
    let event = new_event(
        &work_id,
        None,
        EventKind::FindingAsserted {
            finding: finding_id.clone(),
            assertion,
        },
    );
    if let Err(err) = append_event(state, &mut journal, &work_id, &event) {
        return err_reply("JournalError", &err.to_string());
    }
    let events_now = match journal.replay() {
        Ok(events) => events,
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    // `finding_json_scoped` below may need to fold *this same* work's
    // journal again — a self-citing evidence token, or a settlement
    // whose producing Work is this one — and `Mutex` is not reentrant
    // (`lineage_of`'s own doc comment states the identical hazard).
    // Dropped here exactly where `handle_finding_raise` drops its own
    // journal guard before its own further state reads.
    drop(journal);
    // Ruling 0114's second carried gap, the same repair `settle_ready`
    // and `handle_finding_applied` already carry: the append above is
    // durable, and until this sweep ran the new row reached `atlas
    // findings` only at the next daemon start or an administrative
    // `--rebuild` — so the estate index disagreed with `finding list`
    // about a record both were reading from the same journal. Journal
    // first, index second, through the same idempotent
    // content-addressed sweep; never a second append protocol, and
    // never a query that writes. A failure here is reported by the
    // sweep itself and does not unmake the journal fact.
    reconcile_findings_index(state);
    let Some(record) = fold(&events_now).findings.get(&finding_id).cloned() else {
        return err_reply("Internal", "finding vanished after assert");
    };
    let admin = requester_view.is_none();
    let result = match requester_view {
        None => finding_json(&work_id, &finding_id, &record),
        Some((requester, requester_events, lineage)) => {
            let mut view = DisclosureView::new(&requester, &requester_events, &lineage);
            finding_json_scoped(state, &mut view, &work_id, &finding_id, &record)
        }
    };
    // The assertion above is journaled and durable whatever the sweep
    // did, so this reply is a success — but it says, in the same
    // breath, whether the derived index actually took the row. This is
    // the surface ruling 0116 recorded as silent: exit 0 and a complete
    // reply while the index quietly fell behind.
    ok_reply(with_index_health(state, admin, result))
}

/// §2.4, construction review's own corrected verb: never a client
/// decision. Requests wirkd evaluate the named finding's settlement
/// readiness right now and reports the real outcome — `settled: {...}`
/// once an admitted policy class's check actually holds, or `pending`
/// naming why, never a forged success.
fn handle_finding_settle(state: &Arc<WirkdState>, payload: super::FindingSettlePayload) -> Reply {
    if payload.admin == payload.requester.is_some() {
        return err_reply(
            "BadRequest",
            "name exactly one of --requesting-work <id> (scoped) or --admin (unscoped)",
        );
    }
    let finding_id = FindingId(payload.finding);
    let Some((work_id, _events)) = find_finding_owner(state, &finding_id) else {
        return err_reply("NotFound", "no such finding");
    };
    if let Err(err) = settle_ready(state, &work_id, false) {
        return err_reply("JournalError", &err.to_string());
    }
    let Some(work) = fold_work(state, &work_id) else {
        return err_reply("JournalError", "work journal vanished during settlement");
    };
    let Some(record) = work.findings.get(&finding_id) else {
        return err_reply("NotFound", "no such finding");
    };
    // W-B disclosure response repair
    // (`loop-b-disclosure-verify/VERDICT.md` C1): naming a requester
    // here bounds only what this reply discloses. The evaluation above
    // already ran identically regardless of who asked — requester scope
    // is not settlement permission (W-B-AUTHORITY-ADJUDICATION.md).
    let mut result = if payload.admin {
        finding_json(&work_id, &finding_id, record)
    } else {
        let requester_id = payload
            .requester
            .as_ref()
            .expect("the exclusivity check above admitted a requester");
        let Some(requester_events) = replay_events(state, requester_id) else {
            return err_reply("NotFound", "no such requesting work");
        };
        let requester = fold(&requester_events);
        let lineage = lineage_of(state, &requester, &requester_events);
        let mut view = DisclosureView::new(&requester, &requester_events, &lineage);
        finding_json_scoped(state, &mut view, &work_id, &finding_id, record)
    };
    if !matches!(record.state, FindingState::Settled(_)) {
        // One coherent read of this request's world, exactly as `work
        // obligations` does: the journal is replayed once, the policy is
        // read once, and the readiness candidates are derived once, then
        // lent to the reason ladder (F3).
        let Some(events) = replay_events(state, &work_id) else {
            return err_reply("JournalError", "work journal vanished during settlement");
        };
        let policy = read_settlement_policy(state);
        let candidates = settlement_candidates(state, &events, &work);
        let reason = not_ready_reason(&policy, &events, &candidates, &finding_id, record);
        if let Value::Object(map) = &mut result {
            map.insert("pending".to_string(), json!({"reason": reason}));
        }
    }
    // `settle_ready` above reconciles the index whenever it minted a
    // settlement; a settled reply that does not say whether the row
    // reached the index is the same silence `assert` carried.
    ok_reply(with_index_health(state, payload.admin, result))
}

/// The reason a Finding is not yet ready to settle — shared by `wirk
/// finding settle`'s `pending.reason` and `wirk work obligations`'s
/// `findings[].ready.reason` so the two verbs report a consistent
/// reason for the same underlying condition rather than duplicating or
/// drifting from it (W-B obligation proof).
///
/// **Every input is the caller's own already-read request state.** The
/// policy, the replayed journal and the readiness candidates are read
/// once per request by the caller and lent here; nothing on this ladder
/// re-reads `policy/settlement.json`, re-replays the journal or
/// re-derives the candidate list. `work obligations` calls this once per
/// not-ready Finding, so a re-read here was a re-read per Finding —
/// quadratic in the Findings of one obligation, since each replay also
/// walks a journal every Finding lengthened (F3 of the independent
/// native-foundation review).
///
/// Read-only in the strict sense: it appends nothing and settles
/// nothing. Reading why a Finding is not ready never makes it ready.
fn not_ready_reason(
    policy: &PolicyState,
    events: &[Event],
    candidates: &[ReadySettlement],
    finding_id: &FindingId,
    record: &FindingRecord,
) -> &'static str {
    match policy {
        PolicyState::Absent => "no-policy-file",
        PolicyState::Unreadable => "policy-unreadable",
        // W-B obligation proof: distinguish "this estate has not
        // admitted the obligation you named" from "the check has not
        // held yet", so an operator reading a pending reply is told
        // which of the two it is rather than guessing.
        PolicyState::Loaded(policy) => {
            // The basis this Finding's obligation would have to be
            // admitted at.
            //
            // Read from the *declaring Waypoint's own* derived basis,
            // not only from a ready settlement candidate (F4). A ready
            // candidate exists only for a Finding whose check already
            // holds, so deriving the basis from the candidate alone made
            // `obligation-basis-not-admitted` unreachable in exactly the
            // case it names — an unadmitted basis is *why* no check
            // holds — and handed the operator the vaguer
            // `no-admitted-check-holds-yet` while the same reply's own
            // `admission.state` already said `basis-not-admitted`. When
            // a candidate does exist its check's basis is what would
            // actually be minted against, so that value still wins.
            let candidate_basis = candidates
                .iter()
                .find(|ready| &ready.finding == finding_id)
                .and_then(|ready| match obligation_admission(&ready.check) {
                    Some(Admission::Required(obligation, basis, _)) => Some((
                        obligation.id.clone(),
                        obligation.edition.clone(),
                        basis.to_string(),
                    )),
                    _ => None,
                });
            let named_basis = record.finding.obligation.as_ref().and_then(|named| {
                declared_obligation_basis(events, named)
                    .map(|basis| (named.id.clone(), named.edition.clone(), basis))
            });
            let basis_to_admit = candidate_basis.or(named_basis);
            match (&record.finding.obligation, basis_to_admit) {
                (None, _) if record.finding.kind == FindingKind::VerifiedOutcome => {
                    "no-obligation-named"
                }
                (Some(named), _)
                    if !policy.classes.iter().any(|entry| {
                        entry.obligations.iter().any(|admitted| {
                            admitted.id == named.id && admitted.edition == named.edition
                        })
                    }) =>
                {
                    "obligation-not-admitted"
                }
                (Some(_), Some((id, edition, basis)))
                    if !policy.classes.iter().any(|entry| {
                        entry.obligations.iter().any(|admitted| {
                            admitted.id == id
                                && admitted.edition == edition
                                && admitted.basis == basis
                        })
                    }) =>
                {
                    "obligation-basis-not-admitted"
                }
                (Some(named), _) if review_selectors_unresolved(events, named) => {
                    "review-targets-unresolved"
                }
                _ => "no-admitted-check-holds-yet",
            }
        }
    }
}

/// The obligation basis the Waypoint that *declares* `named` derives
/// right now, from the journal the caller already replayed.
///
/// The same `obligation_basis(def, world_hash)` call `work obligations`
/// renders as `basis.basis`, read for the Waypoint whose `verifies`
/// names this obligation — so a reason string and the `admission` object
/// beside it in the same reply are computed from one value, and cannot
/// disagree about which basis the estate was asked to admit. `None`
/// while the declaring Waypoint's World is unreserved: there is then no
/// derived basis to admit, and the ladder says so by falling through
/// rather than by inventing one.
fn declared_obligation_basis(events: &[Event], named: &ObligationRef) -> Option<String> {
    let defs = waypoint_defs_for(events);
    // Containers included: a container obligation is declared on the
    // container itself, which `flatten_leaves` deliberately omits, and
    // `work obligations` walks the same whole tree.
    let mut all: Vec<&WaypointDefinition> = Vec::new();
    fn walk<'a>(nodes: &'a [WaypointDefinition], out: &mut Vec<&'a WaypointDefinition>) {
        for node in nodes {
            out.push(node);
            walk(&node.leaves, out);
        }
    }
    walk(&defs, &mut all);
    all.into_iter().find_map(|def| {
        let obligation = def.verifies.as_ref()?;
        if obligation.id != named.id || obligation.edition != named.edition {
            return None;
        }
        let world_hash = latest_reservation_for_waypoint(events, &def.id).map(|(hash, _)| hash);
        obligation_basis(def, world_hash.as_ref())
    })
}

/// Whether the Actor Waypoint declaring `named` in this Work reserved
/// fewer frozen review targets than its contract declares — the one
/// pending case the vaguer "no admitted check holds yet" hid, which an
/// operator who has already admitted the basis cannot otherwise diagnose
/// (the independent review's L1).
///
/// Deliberately narrow: it answers a count question about the Work's own
/// Route and its own reserved World, and the reply is a fixed reason
/// string. No source alias, path, membership or generation is disclosed,
/// so this adds no disclosure surface — the wider consultation repair
/// stays a separate stage.
fn review_selectors_unresolved(events: &[Event], named: &ObligationRef) -> bool {
    let defs = waypoint_defs_for(events);
    flatten_leaves(&defs).iter().any(|waypoint| {
        let Some(def) = find_definition(&defs, waypoint) else {
            return false;
        };
        if def.kind != WaypointKind::Actor {
            return false;
        }
        let Some(obligation) = def.verifies.as_ref() else {
            return false;
        };
        if obligation.id != named.id || obligation.edition != named.edition {
            return false;
        }
        let Some(review) = obligation.review.as_ref() else {
            return false;
        };
        match latest_reservation_for_waypoint(events, waypoint) {
            Some((_, World::Actor(actor))) => actor.review_targets.len() != review.targets.len(),
            _ => false,
        }
    })
}

/// `wirk work obligations` (`loop-b-basis-access`; the integrated
/// review's §5.1). **Read-only.** Nothing here appends an event, writes
/// the findings index, mints a settlement, or touches
/// `policy/settlement.json`: it replays journals, re-derives the
/// canonical `wirk_core::obligation_basis` for what is actually
/// reserved, and reports what this estate's policy currently admits.
///
/// The gap it closes, stated exactly by the independent review that
/// found it: an operator who must write `policy/settlement.json` needs
/// the obligation's content `basis`, and for an `Actor` or
/// `Deterministic` Waypoint that value binds the **reserved World
/// hash**, which does not exist until the Work is submitted. It was
/// rendered only inside an *already settled* record, and a `pending`
/// reply said `obligation-basis-not-admitted` without ever naming the
/// value — so the only way to configure the policy was to reimplement
/// the hash out of band. That is a usability defect, not a policy one,
/// and it is fixed by *disclosing* the value, never by admitting it.
///
/// Five things this reply keeps apart, because conflating any two of
/// them is how a digest becomes a proof:
///
/// 1. `obligation` — what the Route **authored**. Authored content is
///    never authority (`VerificationObligation`'s own doc).
/// 2. `basis` — the content address of that authored obligation bound
///    to the **execution basis** actually reserved. Present only when
///    something really is reserved; otherwise it names why not.
/// 3. `admission` — whether **this estate's own policy file** already
///    admits that exact `(id, edition, basis)`. This verb never writes
///    that file and never behaves as though it did.
/// 4. `findings[].ready` — whether the class's check **currently holds**
///    for a Finding naming this obligation. Readiness is not admission
///    and admission is not readiness; a settlement needs both.
/// 5. `findings[].settled` — the receipt, if one was already minted.
///
/// Supported bounds, and the reason for each: every Waypoint kind
/// `obligation_basis` computes a basis for is reported —
/// `Deterministic` and `Actor` (both bind their reserved `WorldHash`)
/// and `Container` (no World of its own; its basis content-addresses
/// its outcome contract and its declared `requires` mechanism, whose
/// own basis is admitted separately as a `mechanism` and is read off
/// the *child* Work's own obligations). A Waypoint declaring no
/// obligation is counted and not listed. An `Actor` obligation with no
/// `review` contract, or a `Container` obligation with no `requires`,
/// is listed with its mechanism reported absent, because those
/// discharge nothing by construction.
///
/// Disclosure: the lineage gate and the checkout rule are `status`'s
/// own, unchanged. An off-lineage requester is refused with the
/// identical `InadmissibleEvidence` message and learns nothing. A
/// requester on the lineage whose own bindings do not cover this Work's
/// gets the journal-identity half — Waypoint, kind, obligation *name*,
/// reserved World hash, the content-addressed basis, the admission
/// state and the Finding identities — and the authored content half
/// (`proves`, `outputs`, the `review` contract) is withheld whole,
/// exactly as `settlement_json_scoped` withholds `proves` and the proof
/// half of a settled check. Frozen review targets are never rendered
/// here at all, in any scope: only how many were declared and how many
/// resolved. This verb therefore adds no new source-disclosing surface.
fn handle_work_obligations(
    state: &Arc<WirkdState>,
    payload: super::WorkObligationsPayload,
) -> Reply {
    if payload.admin == payload.requester.is_some() {
        return err_reply(
            "BadRequest",
            "name exactly one of --requesting-work <id> (scoped) or --admin (unscoped)",
        );
    }
    // `status`'s own gate, verbatim in effect: a named requester sees
    // this Work only if it is on that requester's own lineage.
    let scoped: Option<(Work, Vec<Event>, HashSet<WorkId>)> = match &payload.requester {
        None => None,
        Some(requester_id) => {
            let Some(requester_events) = replay_events(state, requester_id) else {
                return err_reply("NotFound", "no such requesting work");
            };
            let requester = fold(&requester_events);
            let lineage = lineage_of(state, &requester, &requester_events);
            if !lineage.contains(&payload.work_id) {
                return err_reply(
                    "InadmissibleEvidence",
                    "the named work is not the requesting work's own journal or its parent/child lineage",
                );
            }
            Some((requester, requester_events, lineage))
        }
    };

    let Some(events) = replay_events(state, &payload.work_id) else {
        return err_reply("NotFound", "no such work");
    };
    if events.is_empty() {
        return err_reply("NotFound", "no such work");
    }
    let work = fold(&events);
    let defs = waypoint_defs_for(&events);

    // Every Waypoint of the Route, containers included — a container
    // obligation is declared on the container itself, which
    // `flatten_leaves` deliberately does not return.
    let mut all: Vec<&WaypointDefinition> = Vec::new();
    fn walk<'a>(nodes: &'a [WaypointDefinition], out: &mut Vec<&'a WaypointDefinition>) {
        for node in nodes {
            out.push(node);
            walk(&node.leaves, out);
        }
    }
    walk(&defs, &mut all);

    if let Some(named) = &payload.waypoint {
        // An unmatched `--waypoint` is refused, never answered with an
        // empty list: "this Route has no such Waypoint" and "this
        // Waypoint declares no obligation" are different facts and a
        // typo must not read as the second.
        if !all.iter().any(|def| def.id.0 == *named) {
            return err_reply("NotFound", "no such waypoint on this work's route");
        }
    }

    let policy = read_settlement_policy(state);
    let policy_json = match &policy {
        PolicyState::Absent => json!({"state": "absent", "path": "policy/settlement.json"}),
        PolicyState::Unreadable => json!({"state": "unreadable", "path": "policy/settlement.json"}),
        PolicyState::Loaded(loaded) => json!({
            "state": "loaded",
            "path": "policy/settlement.json",
            "version": loaded.version,
            "digest": loaded.digest,
        }),
    };

    // Readiness, read-only: the identical list `finding settle` names a
    // pending reason from, never `settle_ready`, which appends.
    let candidates = settlement_candidates(state, &events, &work);

    let mut obligations = Vec::new();
    let mut declaring = 0usize;
    for def in &all {
        let Some(obligation) = def.verifies.as_ref() else {
            continue;
        };
        declaring += 1;
        if let Some(named) = &payload.waypoint
            && def.id.0 != *named
        {
            continue;
        }
        let reservation = latest_reservation_for_waypoint(&events, &def.id);
        let world_hash = reservation.as_ref().map(|(hash, _)| hash.clone());
        let basis = obligation_basis(def, world_hash.as_ref());

        // Why a basis is unavailable, in the Waypoint's own terms —
        // never a bare `null` an operator has to guess at.
        let basis_json = match (&basis, def.kind) {
            (Some(value), _) => json!({"state": "available", "basis": value}),
            (None, WaypointKind::Actor) if obligation.review.is_none() => json!({
                "state": "no-mechanism",
                "basis": Value::Null,
                "reason": "this Actor obligation declares no `review` contract, so it discharges nothing and has no basis to admit",
            }),
            (None, WaypointKind::Actor | WaypointKind::Deterministic) => json!({
                "state": "not-reserved",
                "basis": Value::Null,
                "reason": "this Waypoint's World has not been reserved yet, and the basis binds it; it becomes available once the Work reserves this Waypoint",
            }),
            (None, WaypointKind::Container) => json!({
                "state": "unavailable",
                "basis": Value::Null,
                "reason": "no basis could be derived for this container obligation",
            }),
        };

        // The mechanism half, kept explicit: a Container obligation
        // without `requires`, and an Actor obligation without `review`,
        // oblige nothing however well-formed the rest is.
        let mechanism = match def.kind {
            WaypointKind::Deterministic => json!({
                "kind": "deterministic_command",
                "present": true,
            }),
            WaypointKind::Container => match &obligation.requires {
                Some(required) => json!({
                    "kind": "required_child_obligation",
                    "present": true,
                    "requires": {"id": required.id, "edition": required.edition},
                    "note": "the policy entry admitting this obligation must also list the child obligation's own basis under `mechanisms`; read that value from the child Work's own `work obligations`",
                }),
                None => json!({
                    "kind": "required_child_obligation",
                    "present": false,
                    "reason": "a container obligation naming no `requires` obliges nothing and can discharge nothing",
                }),
            },
            WaypointKind::Actor => match &obligation.review {
                Some(review) => {
                    let frozen = match reservation.as_ref() {
                        Some((_, World::Actor(actor))) => Some(actor.review_targets.len()),
                        _ => None,
                    };
                    json!({
                        "kind": "actor_review",
                        "present": true,
                        // Counts only. The frozen targets themselves
                        // carry membership, generation and object
                        // identity, and this verb deliberately opens no
                        // new window onto them.
                        "review_targets": {
                            "declared": review.targets.len(),
                            "frozen": frozen,
                            "resolved": frozen == Some(review.targets.len()),
                        },
                    })
                }
                None => json!({
                    "kind": "actor_review",
                    "present": false,
                    "reason": "an Actor obligation declaring no `review` contract obliges nothing and can discharge nothing",
                }),
            },
        };

        let admission = admission_json(
            &policy,
            &obligation.id,
            &obligation.edition,
            basis.as_deref(),
        );

        let findings: Vec<Value> = work
            .findings
            .iter()
            .filter(|(_, record)| {
                record.finding.obligation.as_ref().is_some_and(|named| {
                    named.id == obligation.id && named.edition == obligation.edition
                })
            })
            .map(|(id, record)| {
                let ready = candidates.iter().find(|ready| &ready.finding == id);
                let settled = match &record.state {
                    FindingState::Settled(settlement) => json!({
                        "state": "settled",
                        "class": settlement_class_str(settlement.authority.class),
                    }),
                    _ => json!({"state": "not-settled"}),
                };
                json!({
                    "finding": id.0,
                    "kind": finding_kind_str(record.finding.kind),
                    "scope": finding_scope_str(record.finding.scope),
                    "waypoint": record.finding.waypoint.0,
                    // Readiness is the class's own check holding right
                    // now against this journal — separate from, and
                    // never a substitute for, the policy admission
                    // above.
                    "ready": match ready {
                        // A settled Finding is not "not ready": its
                        // check already held and was minted. Saying
                        // `not-ready` beside `settled` would read as a
                        // contradiction, so the settled case names
                        // itself and readiness stays a statement about
                        // findings that could still settle.
                        _ if matches!(record.state, FindingState::Settled(_)) =>
                            json!({"state": "already-settled"}),
                        Some(ready) => json!({
                            "state": "ready",
                            "class": settlement_class_str(ready.class),
                            "basis": match obligation_admission(&ready.check) {
                                Some(Admission::Required(_, basis, _)) => json!(basis),
                                _ => Value::Null,
                            },
                        }),
                        None => json!({
                            "state": "not-ready",
                            "reason": not_ready_reason(&policy, &events, &candidates, id, record),
                        }),
                    },
                    "settled": settled,
                })
            })
            .collect();

        obligations.push(json!({
            "waypoint": def.id.0,
            "waypoint_kind": waypoint_kind_str(def.kind),
            "obligation": {
                "id": obligation.id,
                "edition": obligation.edition,
                "proves": obligation.proves,
                "outputs": obligation.outputs,
                "review": obligation.review.as_ref().map(|review| json!({
                    "recipe": review.recipe,
                    "targets": review.targets.iter()
                        .map(|target| json!({"source": target.source, "path": target.path}))
                        .collect::<Vec<Value>>(),
                    "decisions": review.decisions.iter()
                        .map(|kind| finding_kind_str(*kind))
                        .collect::<Vec<&str>>(),
                })),
                "requires": obligation.requires.as_ref()
                    .map(|required| json!({"id": required.id, "edition": required.edition})),
            },
            "reservation": match &world_hash {
                Some(hash) => json!({"state": "reserved", "world_hash": hash.0}),
                None => json!({"state": "not-reserved", "world_hash": Value::Null}),
            },
            "mechanism": mechanism,
            "basis": basis_json,
            "admission": admission,
            "findings": findings,
        }));
    }

    let mut result = json!({
        "work": payload.work_id.0,
        "policy": policy_json,
        "route": {"waypoints": all.len(), "declaring_obligation": declaring},
        "obligations": obligations,
    });

    match scoped {
        None => {
            result["scope"] = json!("administrative");
            ok_reply(result)
        }
        Some((requester, requester_events, lineage)) => {
            let mut view = DisclosureView::new(&requester, &requester_events, &lineage);
            if !view.admits_work_checkout(state, &payload.work_id) {
                view.withheld += withhold_obligation_content(&mut result);
            }
            result["scope"] = json!("requester");
            result["work_id"] = json!(payload.work_id.0);
            result["disclosure"] = json!({"withheld": view.withheld});
            ok_reply(result)
        }
    }
}

/// The authored-content half of a `work obligations` reply, withheld as
/// whole objects for a requester whose own bindings do not cover this
/// Work's — the same rule and the same `withheld_json()` marker
/// `withhold_status_content` and `settlement_json_scoped` already apply,
/// so two withheld parts stay indistinguishable and no alias, path or
/// sentence travels in the marker.
///
/// Withheld: the Route-authored `proves` sentence, the obligated
/// `outputs` names, and the whole `review` contract (whose `targets`
/// name a source alias and a resource path). Withholding `review` as an
/// object rather than field by field means a contract that gains a
/// field is withheld by default.
///
/// Kept: journal and content-address identity only — the Waypoint, its
/// kind, the obligation's `id`/`edition`, the reserved `world_hash`, the
/// content-addressed `basis`, the mechanism's presence and target
/// counts, the policy admission state, and the Finding identities and
/// their readiness. Every one of those is a value `status` already
/// discloses to exactly this requester (`world_hash`) or a count
/// (`review_targets`), and none of them names a source, a path, a
/// generation, an object or a digest of any artifact.
fn withhold_obligation_content(result: &mut Value) -> usize {
    let mut withheld = 0usize;
    let Some(entries) = result.get_mut("obligations").and_then(Value::as_array_mut) else {
        return withheld;
    };
    for entry in entries {
        let Some(obligation) = entry.get_mut("obligation") else {
            continue;
        };
        for field in ["proves", "outputs", "review"] {
            if let Some(slot) = obligation.get_mut(field)
                && !slot.is_null()
            {
                *slot = withheld_json();
                withheld += 1;
            }
        }
    }
    withheld
}

/// What this estate's settlement policy currently says about one
/// obligation at one basis — and nothing more. It reports; it never
/// admits.
///
/// The four states an operator actually has to tell apart:
///
/// - `unknown-basis`: nothing is reserved yet, so there is no value to
///   admit and no admission question to answer. Reported before the
///   policy is consulted at all, so an unreserved obligation can never
///   read as admitted.
/// - `no-policy-file` / `policy-unreadable`: the same two fail-closed
///   states `read_settlement_policy` already distinguishes.
/// - `obligation-not-admitted`: no entry names this `id`/`edition`.
/// - `basis-not-admitted`: an entry names it, at a **different** basis
///   — the exact case the reviewer hit, and the one that used to leave
///   an operator with a reason string and no value.
/// - `admitted`: an entry names this id, edition and basis. Every such
///   entry is listed with its class, scope and kinds, because
///   `try_mint_settlement` also matches the Finding's own scope and
///   kind against them — an entry admitting the basis under the wrong
///   scope settles nothing, and the operator can see that here rather
///   than discovering it as a pending reason.
fn admission_json(policy: &PolicyState, id: &str, edition: &str, basis: Option<&str>) -> Value {
    let Some(basis) = basis else {
        return json!({
            "state": "unknown-basis",
            "reason": "no basis is derivable yet, so this obligation is neither admitted nor admissible; nothing may be read as admitted here",
            "admitted_by": [],
        });
    };
    let loaded = match policy {
        PolicyState::Absent => {
            return json!({"state": "no-policy-file", "admitted_by": []});
        }
        PolicyState::Unreadable => {
            return json!({"state": "policy-unreadable", "admitted_by": []});
        }
        PolicyState::Loaded(loaded) => loaded,
    };
    let mut admitted_by = Vec::new();
    let mut names_obligation = false;
    for entry in &loaded.classes {
        for admitted in &entry.obligations {
            if admitted.id != id || admitted.edition != edition {
                continue;
            }
            names_obligation = true;
            if admitted.basis != basis {
                continue;
            }
            admitted_by.push(json!({
                "class": settlement_class_str(entry.class),
                "scope": finding_scope_str(entry.scope),
                "kinds": entry.kinds.iter().map(|kind| finding_kind_str(*kind)).collect::<Vec<&str>>(),
                "mechanisms": admitted.mechanisms,
            }));
        }
    }
    let state = if !admitted_by.is_empty() {
        "admitted"
    } else if names_obligation {
        "basis-not-admitted"
    } else {
        "obligation-not-admitted"
    };
    json!({"state": state, "admitted_by": admitted_by})
}

/// The wire name of a `WaypointKind`, so a reply says which of the three
/// obligation mechanisms applies without the reader inferring it.
fn waypoint_kind_str(kind: WaypointKind) -> &'static str {
    match kind {
        WaypointKind::Deterministic => "deterministic",
        WaypointKind::Container => "container",
        WaypointKind::Actor => "actor",
    }
}

/// §4: the mechanical proof of changed bytes, split from the always-
/// asserted judgement that they implement the finding. Every refusal
/// named in §4 is checked in the same order the design states it.
fn handle_finding_applied(
    state: &Arc<WirkdState>,
    payload: super::FindingAppliedPayload,
    peer: PeerIdentity,
) -> Reply {
    // W-B-CORRECT.md defect 3: the caller's own producer identity is
    // checked exactly like `handle_finding_raise` checks its own raising
    // Run — never a bare `--by` string from an arbitrary shell with no
    // Work, Run, or checkout at all (the authority review's own executed
    // counterexample).
    //
    // W-B Application repair: that check is now ruling 0095's own
    // `current_producing_action`, the identical reducer `atlas relate`
    // already uses, instead of this verb's private "latest attempt for
    // its Waypoint" approximation. The candidate's approximation
    // admitted a *spent* Run (one whose own Validated Done Claim is
    // already recorded), a failed Run, a vanished Run and a Run on a
    // completed, failed or canceled Work — every one of which is still
    // the latest attempt for its Waypoint, and none of which is
    // producing anything now. The reason the candidate had to allow
    // them is recorded in `FindingAppliedPayload::claim_run`'s own doc:
    // it conflated the caller with the historical Claim it cites. Those
    // are now two separate identities, so the caller can be held to the
    // real currency rule while a closing Claim stays usable as history.
    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the claim's estate root does not identify this daemon's estate",
        );
    }
    let producer_work_id = payload.triple.work_id.clone();
    let producer_run_id = payload.triple.run_id.clone();
    let Some(producer_events) = replay_events(state, &producer_work_id) else {
        return err_reply("NotFound", "no such work");
    };
    let action = match current_producing_action(&producer_events) {
        Ok(action) => action,
        Err(reply) => return reply,
    };
    if action.run != producer_run_id {
        return err_reply(
            "ProducingActionMismatch",
            "the calling run is not this Work's current producing action: a spent, failed, vanished or superseded attempt records no new assertion",
        );
    }
    // W-B disclosure response repair (`loop-b-disclosure-verify/VERDICT.md`
    // C3): folded once, before `producer_work_id` is possibly moved into
    // the `Attribution::Asserted` arm below, so the final reply can be
    // scoped by the caller's own already-checked identity with no new
    // flag — `applied` never lets a caller name a different requester.
    let producer = fold(&producer_events);

    let finding_id = FindingId(payload.finding);
    let Some((work_id, events)) = find_finding_owner(state, &finding_id) else {
        return err_reply("NotFound", "no such finding");
    };
    let work = fold(&events);
    let Some(record) = work.findings.get(&finding_id) else {
        return err_reply("NotFound", "no such finding");
    };
    // W-B Application repair, the journal-write boundary ruling 0101
    // already settled for `finding assert`: this verb *writes* into the
    // target finding's own Work, and the candidate let any Work in the
    // estate do it. The identical `lineage_of` set `handle_finding_assert`
    // checks is applied here — no second, looser rule, and no new flag:
    // `applied` never lets a caller name a requester other than itself,
    // so its own already-checked triple is the requester.
    let lineage = lineage_of(state, &producer, &producer_events);
    if !lineage.contains(&work_id) {
        return err_reply(
            "InadmissibleEvidence",
            "the finding's own work is not this producing work's own journal or its parent/child lineage",
        );
    }
    // 1. The finding's own admitted source coordinate.
    let Some((encoded, before_generation, before_object_id)) = record
        .finding
        .applies_to
        .iter()
        .find_map(|item| match (&item.reference, &item.outcome) {
            (
                EvidenceRef::Source(encoded),
                EvidenceOutcome::Admitted {
                    generation,
                    object_id,
                },
            ) => Some((encoded.clone(), generation.clone(), object_id.clone())),
            _ => None,
        })
    else {
        return err_reply(
            "NoOwningSource",
            "the finding names no admitted source coordinate to apply against",
        );
    };
    let coordinate = match decode_coordinate(&encoded) {
        Ok(coordinate) => coordinate,
        Err(detail) => return err_reply("InadmissibleEvidence", &detail),
    };
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let scope = wirk_atlas::QueryScope::Work(work.repositories.clone());
    let Some(membership) = admitted_membership_for(&atlas, &scope, &coordinate.membership) else {
        return err_reply(
            "InadmissibleEvidence",
            "the finding's own source membership is not admitted by this Work's own bindings",
        );
    };
    // W-B Application repair: and admitted by the *caller's* own
    // bindings too. The candidate resolved the membership only against
    // the finding-owning Work's grants, so a producing Work with no
    // binding at all on the changed source recorded a durable
    // Application against it — an origin Work's broader admission is
    // not the caller's grant. Same `admitted_membership_for`, same
    // `QueryScope::Work`, the caller's own repository set.
    if admitted_membership_for(
        &atlas,
        &wirk_atlas::QueryScope::Work(producer.repositories.clone()),
        &coordinate.membership,
    )
    .is_none()
    {
        return err_reply(
            "InadmissibleEvidence",
            "the changed source is not admitted by this producing work's own bindings",
        );
    }
    if membership.alias != payload.source {
        return err_reply(
            "BadRequest",
            "--source does not name the finding's own applies_to membership",
        );
    }
    // 2. Publication currency.
    let after_generation = match atlas.current(&membership) {
        Ok(Some(generation)) => generation,
        Ok(None) => {
            return err_reply(
                "SourceNotPublished",
                "no published generation for this source",
            );
        }
        Err(err) => return err_reply("AtlasError", &err.to_string()),
    };
    if after_generation.revision != payload.revision {
        return err_reply(
            "RevisionNotCurrent",
            "the named revision is not the currently published one",
        );
    }
    if after_generation.id == coordinate.generation {
        return err_reply(
            "GenerationUnchanged",
            "the published generation is unchanged: a republication is not an application",
        );
    }
    // 3. Mechanical proof: the before state still resolves, and the
    // same path names a different (or absent) object in the after
    // generation.
    match atlas.resolve_exact(&membership, &coordinate) {
        Ok(wirk_atlas::ResolveOutcome::Resolved(_)) => {}
        _ => {
            return err_reply(
                "InadmissibleEvidence",
                "the finding's own before state no longer resolves",
            );
        }
    }
    // W-B Application repair, "deletion has explicit absence rather
    // than a fabricated object id" — and, distinctly, is not the same
    // fact as a resource Atlas recorded with no object id at all. The
    // candidate collapsed both into one `None`, so a record whose bytes
    // this daemon simply cannot identify was published as a deletion.
    // Refusing the uninterpretable case here makes `after.object_id ==
    // None` mean exactly one thing downstream: the path is absent from
    // the after generation. An emptied file is *not* that — it keeps a
    // real, zero-byte Git object id and reads as present.
    let after_resource = after_generation
        .resources
        .iter()
        .find(|resource| resource.path == coordinate.path);
    let after_object_id = match after_resource {
        None => None,
        Some(resource) => match &resource.object_id {
            Some(object_id) => Some(object_id.clone()),
            None => {
                return err_reply(
                    "UnknownAfterObject",
                    "the after generation records this resource with no object id: its bytes cannot be identified, and absence must not be inferred from that",
                );
            }
        },
    };
    if after_object_id.as_deref() == Some(before_object_id.as_str()) {
        return err_reply(
            "CoordinatesUnchanged",
            "the finding's own coordinates carry the identical object id in both generations",
        );
    }
    // 4-5. Attribution and the judgement. `Attribution::Asserted`
    // (ruling 0077: permitted regardless of the membership's Read/Write
    // binding — a durable, evidenced assertion is distinct from source
    // mutation authority) stays the default, now always naming the
    // real, already-checked `producer` above — never a bare `--by`
    // string standing in for identity. `--claim-run` requests the
    // exact, checked `Attribution::Claim` path over a *historical*
    // Validated Done Claim instead (W-B-CORRECT.md defect 3).
    //
    // Which Work's journal that cited Claim lives in is decided here,
    // out of the locks, because the answer decides *which* journals the
    // append below must hold. The Claim itself is resolved under those
    // locks, not here.
    let claim_citation = payload.claim_run.as_ref().map(|claim_run| {
        // The cited Claim is evidence read out of another journal, so
        // it goes through the same reference rule every other journal
        // reference in this file obeys (`admit_evidence`'s own Journal
        // branch, `handle_finding_assert`): the caller's own lineage.
        let claim_work = payload
            .claim_work
            .clone()
            .map(WorkId)
            .unwrap_or_else(|| producer_work_id.clone());
        (claim_work, RunId(claim_run.clone()))
    });
    if let Some((claim_work, _)) = &claim_citation
        && claim_work != &producer_work_id
        && !lineage.contains(claim_work)
    {
        drop(atlas);
        return err_reply(
            "InadmissibleEvidence",
            "the cited claim's work is not this producing work's own journal or its parent/child lineage",
        );
    }
    // Atlas is released before any journal lock is taken. Route
    // advancement holds a Work's journal lock and then reaches for the
    // Atlas: `reserve_next_leaf` is called with the Work's journal
    // guard held (`handle_claim`'s auto-advance and `close_cascade`)
    // and freezes the next leaf's review targets through
    // `freeze_review_targets`, which locks the Atlas itself. So
    // journal-then-Atlas is this daemon's established direction, and
    // taking them the other way round here would be the classic
    // inversion.
    //
    // (Corrected by the independent currentness verification, V-1: this
    // comment used to cite `actor_reviewed_readiness`, which takes no
    // Atlas lock at all — its body is `let _ = state;`. The ordering
    // constraint is real; the site named for it was not.)
    drop(atlas);

    // ---- The mutation's linearization boundary (F-1) ----------------
    //
    // The independent Application verification (`APPLICATION-VERDICT.md`
    // F-1) executed what the earlier shape allowed: `current_producing_
    // action` was read from an unlocked `replay_events` at the top of
    // this function, and the append happened much later under a
    // *different* Work's lock. One thread per connection (`run`'s own
    // accept loop) meant a concurrent `fail`, `cancel`, `retry` or
    // `claim` on the producer's Work landed inside that window: in 15 of
    // 39 racers a `FindingApplied` was appended *after* the `RunFailed`
    // that superseded its own producer, minting exactly the assertion
    // this verb refuses when it arrives a millisecond later.
    //
    // A stale pre-read followed by a second unlocked check is not
    // atomic, so the fix is neither. Every journal whose facts decide
    // this append is locked *before* the decision and stays locked
    // *through* it: the producer's (its current producing action), the
    // cited Claim's (its own currency and receipts), and the finding
    // owner's (the one actually written). Whatever a concurrent request
    // does to any of them either lands entirely before this decision —
    // and is seen by it — or entirely after this append. A racer is now
    // indistinguishable from a caller that simply arrived late, which is
    // why the refusals below are the identical refusals the sequential
    // path returns rather than a new race-only code.
    //
    // Ordering. Several of those journals are frequently distinct, so an
    // order is required or two requests deadlock head-on. `journal_lock_
    // order` supplies one estate-wide order that the daemon's existing
    // multi-journal sites already obey: an ancestor is locked before its
    // descendant (`settle_ready` holds a Work's own lock while
    // `child_investigation_ready` reads a child's; `close_cascade` the
    // same), ties broken by `WorkId`. Distinct Works with no ancestry
    // between them simply need *some* total order, and the id gives one.
    // It is computed before any lock is taken, because computing it
    // reads journals.
    //
    // Self-locking. The overwhelmingly common Application is a Work
    // applying its own finding, citing its own Claim — one Work, one
    // journal, and `Mutex` is not reentrant. The set below is
    // deduplicated by `WorkId` for exactly that reason, the same
    // discipline `lineage_of` states for its own `own_events`.
    let mut needed: Vec<WorkId> = vec![work_id.clone()];
    if producer_work_id != work_id {
        needed.push(producer_work_id.clone());
    }
    if let Some((claim_work, _)) = &claim_citation
        && !needed.contains(claim_work)
    {
        needed.push(claim_work.clone());
    }
    let mut handles: Vec<(WorkId, Arc<Mutex<Journal>>)> = Vec::new();
    for id in &needed {
        match journal_for(state, id) {
            Ok(Some(journal)) => handles.push((id.clone(), journal)),
            Ok(None) => return err_reply("NotFound", "no such work"),
            Err(err) => return err_reply("JournalError", &err.to_string()),
        }
    }
    handles.sort_by_cached_key(|(id, _)| journal_lock_order(state, id));

    let mut guards: Vec<(WorkId, JournalGuard<'_>)> = Vec::new();
    for (id, journal) in &handles {
        guards.push((id.clone(), lock_journal(journal)));
    }
    // Read every locked journal once, here, so the checks below and the
    // append itself all see one consistent snapshot taken inside the
    // locks — never the pre-read from the top of this function.
    let mut replayed: Vec<(WorkId, Vec<Event>)> = Vec::new();
    for (id, guard) in &guards {
        match guard.replay() {
            Ok(events) => replayed.push((id.clone(), events)),
            Err(err) => return err_reply("JournalError", &err.to_string()),
        }
    }
    let events_for = |id: &WorkId| -> &[Event] {
        replayed
            .iter()
            .find(|(held, _)| held == id)
            .map(|(_, events)| events.as_slice())
            .unwrap_or(&[])
    };

    // The producer's authority, re-derived under its own lock and held
    // there until this append is durable. A `fail`, `cancel`, `retry` or
    // closing `claim` that won the race is already in these events and
    // refuses the assertion with the sequential path's own words; one
    // that lost it cannot append until this journal lock is released.
    let action_now = match current_producing_action(events_for(&producer_work_id)) {
        Ok(action) => action,
        Err(reply) => return reply,
    };
    if action_now != action {
        return err_reply(
            "ProducingActionMismatch",
            "the calling run is not this Work's current producing action: a spent, failed, vanished or superseded attempt records no new assertion",
        );
    }

    // And the cited Claim, resolved under the same held locks for the
    // same reason: `resolve_claim_attribution`'s own "current for its
    // waypoint" test is a journal fact a concurrent `retry` on the cited
    // Work can invalidate between a read and this append.
    let attribution = match &claim_citation {
        Some((claim_work, claim_run)) => {
            match resolve_claim_attribution(
                &membership,
                &coordinate,
                &after_object_id,
                events_for(claim_work),
                claim_work,
                claim_run,
            ) {
                Ok(attribution) => attribution,
                Err((code, message)) => return err_reply(code, &message),
            }
        }
        None => Attribution::Asserted {
            by: payload.by.clone(),
            peer,
            producer: ApplicationProducer {
                work: producer_work_id.clone(),
                run: producer_run_id.clone(),
                world_hash: action_now.world_hash.clone(),
            },
        },
    };
    let application = ApplicationRef {
        source: membership.alias.clone(),
        before: GenerationPoint {
            generation: before_generation,
            object_id: Some(before_object_id),
        },
        after: GenerationPoint {
            generation: after_generation.id.0.clone(),
            object_id: after_object_id,
        },
        revision: after_generation.revision.clone(),
        attribution,
        implements_finding: AssertedJudgement {
            by: payload.by,
            peer,
            at: now_ts(),
        },
    };
    let events_now = {
        let Some((_, owner_guard)) = guards.iter_mut().find(|(held, _)| held == &work_id) else {
            return err_reply("Internal", "the finding's own journal was not locked");
        };
        let event = new_event(
            &work_id,
            None,
            EventKind::FindingApplied {
                finding: finding_id.clone(),
                application,
            },
        );
        if let Err(err) = append_event(state, owner_guard, &work_id, &event) {
            return err_reply("JournalError", &err.to_string());
        }
        match owner_guard.replay() {
            Ok(events) => events,
            Err(err) => return err_reply("JournalError", &err.to_string()),
        }
    };
    // `reconcile_findings_index`, `lineage_of` and `finding_json_scoped`
    // below all fold journals of their own — including these — and
    // `Mutex` is not reentrant, so every guard is released first,
    // exactly as `handle_finding_assert` does before its own scoped
    // reply. The append is already durable; nothing after this point
    // decides anything.
    drop(guards);
    // W-B-AUTHORITY-ADJUDICATION.md, "immediate journal-first index
    // reconciliation after settle/application": `settle_ready` already
    // does this after minting a settlement, and the candidate never did
    // it here — an applied row was invisible to `atlas findings` until
    // the next restart or an explicit administrative `--rebuild`.
    // Journal first (the append above already succeeded), index second,
    // through the same idempotent content-addressed sweep, never a
    // second append protocol.
    reconcile_findings_index(state);
    let Some(record) = fold(&events_now).findings.get(&finding_id).cloned() else {
        return err_reply("Internal", "finding vanished after applied");
    };
    let mut view = DisclosureView::new(&producer, &producer_events, &lineage);
    // Same honesty as `assert`: the Application is journaled, and this
    // says whether the index took its row.
    ok_reply(with_index_health(
        state,
        false,
        finding_json_scoped(state, &mut view, &work_id, &finding_id, &record),
    ))
}

/// W-B-CORRECT.md defect 3 ("complete Application"): the checked
/// `Attribution::Claim` path. Every refusal here is a distinct, real
/// authority failure, never a bare id match:
/// - the named Run must be *current* for its own Waypoint — a stale or
///   superseded attempt's identity confers nothing (`TripleMismatch`);
/// - it must carry a real Validated Done `ClaimRecorded`, not merely
///   exist (`WrongClaim`);
/// - that Claim's own Work must hold a `Write` binding on the named
///   source — a Read-only membership's mutation credit is refused
///   (`ReadOnlyMutationCredit`), unlike `Attribution::Asserted`'s own
///   ruling-0077 allowance;
/// - that Claim's own artifact receipt must name this Finding's exact
///   path (`ChangedClaimedArtifact`) and its own recorded digest must
///   match the after-generation's actual bytes, read fresh from the
///   source's own Git object store, never only a declared path or a
///   Write label (`DifferentAfterBytes`).
///
/// `claim_events` is the cited Work's journal, replayed by the caller
/// rather than read here: `handle_finding_applied` evaluates this
/// function *under* the journal locks it appends beneath (F-1), and
/// `Mutex` is not reentrant, so a `replay_events` of its own would
/// deadlock the ordinary case where the cited Claim lives in the
/// caller's own Work. The same discipline `lineage_of` already states
/// for its own `own_events`.
fn resolve_claim_attribution(
    membership: &wirk_atlas::Membership,
    coordinate: &wirk_atlas::ExactCoordinate,
    after_object_id: &Option<String>,
    claim_events: &[Event],
    claim_work: &WorkId,
    claim_run: &RunId,
) -> Result<Attribution, (&'static str, String)> {
    let Some(run) = find_run(claim_events, claim_run) else {
        return Err((
            "NotFound",
            "no such claim run in the named claim work".to_string(),
        ));
    };
    if latest_run_for_waypoint(claim_events, &run.waypoint).map(|entry| entry.0)
        != Some(claim_run.clone())
    {
        return Err((
            "TripleMismatch",
            "the claim's own run is not current for its waypoint: a stale or superseded attempt confers no attribution".to_string(),
        ));
    }
    let Some((claim_event_id, claim_id)) = claim_events.iter().rev().find_map(|event| {
        if event.run.as_ref() != Some(claim_run) {
            return None;
        }
        match &event.kind {
            EventKind::ClaimRecorded {
                claim,
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
                ..
            } => Some((event.id.clone(), claim.clone())),
            _ => None,
        }
    }) else {
        return Err((
            "WrongClaim",
            "the named claim run carries no Validated Done Claim".to_string(),
        ));
    };
    let claim_work_folded = fold(claim_events);
    let has_write = claim_work_folded
        .repositories
        .iter()
        .any(|binding| binding.name == membership.alias && binding.access == Access::Write);
    if !has_write {
        return Err((
            "ReadOnlyMutationCredit",
            "the claim's own work holds no Write binding on this source: a Read-only membership confers no mutation credit".to_string(),
        ));
    }
    // W-B Application repair: a `Write` binding is a *declared* grant
    // carrying an alias and an access level and nothing else, so the
    // candidate's alias equality credited a Work that really executed
    // in some other repository — or another estate's same-named source
    // — with mutating this one. Ruling 0090 already resolved this exact
    // class for child bindings: `canonical_repository_identity` (`git
    // rev-parse --git-common-dir`, canonicalized) tells two worktrees
    // of one repository apart from two unrelated repositories sharing a
    // name. The Work's own recorded `execution_identity` is what wirkd
    // verified at submit time, never a caller string.
    let membership_identity =
        canonical_repository_identity(&membership.locator).map_err(|err| ("AtlasError", err))?;
    match claim_work_folded.execution_identity.as_deref() {
        Some(identity) if identity == membership_identity => {}
        Some(_) => {
            return Err((
                "DifferentExecutionSource",
                "the claim's own work executed in a different repository than this source: a shared --repo alias is not the same checkout".to_string(),
            ));
        }
        None => {
            return Err((
                "DifferentExecutionSource",
                "the claim's own work recorded no verified execution repository, so its Claim cannot be bound to this source".to_string(),
            ));
        }
    }
    let receipts = claim_artifact_receipts(claim_events, &claim_id);
    let coordinate_path = String::from_utf8_lossy(&coordinate.path).into_owned();
    // Ruling 0145, made explicit rather than left accidental: a finding
    // coordinate is a path *in a source repository*, and only a
    // `Worktree` receipt names one. A managed output lives under
    // `works/<work>/outputs/` and is in no repository at all, so it can
    // never attest a source coordinate — and must not be able to, since
    // its recorded path (`claims/<claim>/<name>`) is a different
    // namespace that could otherwise collide with a repository path by
    // string equality alone.
    let Some(receipt) = receipts
        .iter()
        .filter(|receipt| matches!(receipt.store, wirk_core::ArtifactStore::Worktree))
        .find(|receipt| receipt.path == coordinate_path)
    else {
        return Err((
            "ChangedClaimedArtifact",
            "the claim's own receipts name no artifact at this finding's own path".to_string(),
        ));
    };
    // An absent resource is a real, recorded Application outcome
    // (`Attribution::Asserted` records it as explicit absence), but an
    // artifact receipt attests the digest of bytes that exist. There is
    // nothing to compare, and saying so is a different fact from "the
    // bytes differ".
    let Some(after_object_id) = after_object_id else {
        return Err((
            "DeletedResource",
            "the resource is absent from the after generation: an artifact receipt's digest cannot attest a deletion".to_string(),
        ));
    };
    let bytes =
        read_blob(&membership.locator, after_object_id).map_err(|err| ("AtlasError", err))?;
    let after_digest = sha256_hex(&bytes);
    if after_digest != receipt.digest {
        return Err((
            "DifferentAfterBytes",
            "the claim's own artifact digest does not match the after-generation's actual bytes"
                .to_string(),
        ));
    }
    Ok(Attribution::Claim {
        work: claim_work.clone(),
        run: claim_run.clone(),
        claim: claim_id,
        claim_event: claim_event_id,
    })
}

/// R2/R5: the identical `git -C <repo> ...` subprocess convention
/// `wirk-atlas/src/git.rs`'s own private `git()` helper already uses —
/// no new Atlas API surface, since `Membership.locator` (a canonicalized
/// filesystem path, `wirk_atlas::store::register_git`) is already a
/// public field wirkd can read directly. `cat-file -p` reads content by
/// its already-resolved, content-addressed blob id alone — no ref or
/// commit needed.
fn read_blob(repo: &str, object_id: &str) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("-C")
        .arg(repo)
        .arg("cat-file")
        .arg("-p")
        .arg(object_id)
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// §3/W-B-CORRECT.md defect 2: a `work` selection is never itself an
/// evidence grant. `admin` is the one explicit, separately-named path
/// that keeps the old unscoped behavior (every Work in the estate, or
/// any named Work, no lineage check) — real administrative inspection,
/// not the default. Every other call must name its own `requester` Work
/// and only ever sees that Work's own effective lineage (itself, its
/// ancestors, its descendants) — reusing `lineage_of`, the identical set
/// `admit_evidence`'s Journal branch already computes at raise time,
/// never a second, looser rule for consultation.
fn handle_finding_list(state: &Arc<WirkdState>, payload: super::FindingListPayload) -> Reply {
    let mut findings = Vec::new();
    if payload.admin {
        match &payload.work {
            Some(work_id) => {
                let Some(work) = fold_work(state, work_id) else {
                    return err_reply("NotFound", "no such work");
                };
                for (id, record) in &work.findings {
                    findings.push(finding_json(work_id, id, record));
                }
            }
            None => {
                let works_dir = state.estate_root.join("works");
                if let Ok(entries) = std::fs::read_dir(&works_dir) {
                    for entry in entries.flatten() {
                        let dir = entry.path();
                        if !dir.is_dir() {
                            continue;
                        }
                        // Pure discovery: read the journal that is there, create
                        // nothing, and read one an operator left read-only
                        // (`discovery_events`). The mutation this sweep decides on
                        // still goes through the one write path below.
                        let Some(events) = discovery_events(&dir) else {
                            continue;
                        };
                        if events.is_empty() {
                            continue;
                        }
                        let work = fold(&events);
                        for (id, record) in &work.findings {
                            findings.push(finding_json(&work.id, id, record));
                        }
                    }
                }
            }
        }
        return ok_reply(json!({ "findings": findings }));
    }
    let Some(requester_id) = &payload.requester else {
        return err_reply(
            "BadRequest",
            "a non-administrative finding list requires --requesting-work",
        );
    };
    let Some(requester_events) = replay_events(state, requester_id) else {
        return err_reply("NotFound", "no such requesting work");
    };
    let requester = fold(&requester_events);
    let lineage = lineage_of(state, &requester, &requester_events);
    // Lineage decides *which journals* may be listed at all; the view
    // decides *how much of each record* this requester may be shown.
    // The base drew only the first line and then rendered every
    // coordinate, proof target, artifact path and Application source in
    // full — a `--requesting-work` that selected its parent read the
    // parent's embargoed sources straight off the wire.
    let mut view = DisclosureView::new(&requester, &requester_events, &lineage);
    if let Some(work_id) = &payload.work {
        if !lineage.contains(work_id) {
            return err_reply(
                "InadmissibleEvidence",
                "the named work is not the requesting work's own journal or its parent/child lineage",
            );
        }
        let Some(work) = fold_work(state, work_id) else {
            return err_reply("NotFound", "no such work");
        };
        for (id, record) in &work.findings {
            findings.push(finding_json_scoped(state, &mut view, work_id, id, record));
        }
    } else {
        for work_id in &lineage {
            let Some(work) = fold_work(state, work_id) else {
                continue;
            };
            for (id, record) in &work.findings {
                findings.push(finding_json_scoped(state, &mut view, work_id, id, record));
            }
        }
    }
    ok_reply(json!({
        "findings": findings,
        // Honest, and honestly bounded: how many record parts were
        // withheld, never which. Same discipline as `atlas search`'s own
        // `admission.denied` count.
        "disclosure": {"withheld": view.withheld},
    }))
}

// ---- Settlement policy (§2.4) --------------------------------------------

struct SettlementPolicyClass {
    class: SettlementClass,
    scope: FindingScope,
    kinds: Vec<FindingKind>,
    /// W-B obligation proof (`W-B-OBLIGATION-BUILD.md`): which
    /// verification obligations this class may discharge, each admitted
    /// by name **and** by content basis. `deterministic_verified` +
    /// scope + kind is no longer an admission of anything: without a
    /// matching entry here, nothing settles under this class.
    obligations: Vec<AdmittedObligation>,
}

/// One obligation the estate's own policy file pre-admits. `basis` is
/// `wirk_core::obligation_basis` — the content address of the authored
/// obligation *inseparably bound to* the execution basis that discharges
/// it. Admitting by name alone would let any proposer author a Route
/// Waypoint claiming the admitted check identity while running something
/// else entirely; admitting the basis pins the command, the source
/// basis, the expected artifacts, the proven statement and the obligated
/// outputs together.
struct AdmittedObligation {
    id: String,
    edition: String,
    basis: String,
    /// The verification-execution bases this estate admits as
    /// *discharging* this obligation. A `Container` obligation has no
    /// World of its own, so its own `basis` content-addresses prose and
    /// an outcome contract; the execution it stands for lives in the
    /// child obligations its obligated roles actually settle. Admitting
    /// those bases here is what ties the container obligation to a real,
    /// immutable verification execution and exact source basis — and
    /// what stops a changed repository generation, a changed child
    /// verification command or a changed evidence target from silently
    /// reusing an admission already granted.
    ///
    /// Empty for a `DeterministicVerified` entry, whose own `basis`
    /// already binds its execution.
    mechanisms: Vec<String>,
}

struct SettlementPolicy {
    version: u32,
    digest: String,
    classes: Vec<SettlementPolicyClass>,
}

/// Absent ⇒ no class enabled ⇒ no EstateLocal finding is ever settled.
/// Unreadable (malformed JSON, unknown version/class/kind name) ⇒ the
/// same "no settlement minted at all" outcome, never a partial read
/// (§2.4: "PolicyUnreadable, and no settlement is minted at all").
enum PolicyState {
    Absent,
    Unreadable,
    Loaded(SettlementPolicy),
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyClassRaw {
    class: String,
    scope: String,
    kinds: Vec<String>,
    obligations: Vec<PolicyObligationRaw>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyObligationRaw {
    id: String,
    edition: String,
    basis: String,
    #[serde(default)]
    mechanisms: Vec<String>,
}

#[derive(serde::Deserialize)]
struct PolicyFileRaw {
    version: u32,
    classes: Vec<PolicyClassRaw>,
}

/// Raised from 1 by the obligation-proof wave: a class entry now carries
/// its admitted `obligations`, and a version-1 file (which admitted a
/// class with no obligation at all) is `PolicyState::Unreadable` — the
/// existing fail-closed outcome, never a silent reinterpretation of an
/// old file as admitting the new contract. Already-minted settlements
/// keep the `policy_version`/`policy_digest` they were minted under and
/// are never recomputed (§6), so raising this revises what may settle
/// *next*, and rewrites no existing decision.
const SETTLEMENT_POLICY_VERSION: u32 = 2;

fn read_settlement_policy(state: &Arc<WirkdState>) -> PolicyState {
    let path = state.estate_root.join("policy").join("settlement.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return PolicyState::Absent;
    };
    let digest = sha256_hex(&bytes);
    let Ok(raw) = serde_json::from_slice::<PolicyFileRaw>(&bytes) else {
        return PolicyState::Unreadable;
    };
    if raw.version != SETTLEMENT_POLICY_VERSION {
        return PolicyState::Unreadable;
    }
    let mut classes = Vec::new();
    for entry in raw.classes {
        let Some(class) = parse_settlement_class(&entry.class) else {
            return PolicyState::Unreadable;
        };
        let scope = match entry.scope.as_str() {
            "work_local" => FindingScope::WorkLocal,
            "estate_local" => FindingScope::EstateLocal,
            _ => return PolicyState::Unreadable,
        };
        let mut kinds = Vec::new();
        for kind in &entry.kinds {
            match parse_finding_kind(kind) {
                Ok(kind) => kinds.push(kind),
                Err(_) => return PolicyState::Unreadable,
            }
        }
        let obligations = entry
            .obligations
            .into_iter()
            .map(|raw| AdmittedObligation {
                id: raw.id,
                edition: raw.edition,
                basis: raw.basis,
                mechanisms: raw.mechanisms,
            })
            .collect();
        classes.push(SettlementPolicyClass {
            class,
            scope,
            kinds,
            obligations,
        });
    }
    PolicyState::Loaded(SettlementPolicy {
        version: raw.version,
        digest,
        classes,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The obligation a check must have pre-admitted, or `None` for a class
/// that discharges no verification obligation.
/// What a check demands of the estate's settlement policy.
enum Admission<'a> {
    /// This obligation, at this basis, resting on these mechanism bases.
    Required(&'a ObligationRef, &'a str, Vec<&'a str>),
    /// This reader cannot interpret the check's proof, so it cannot
    /// establish what would have to be admitted — and therefore refuses
    /// to mint anything from it (re-review L3).
    Unreadable,
}

fn obligation_admission(check: &SettlementCheck) -> Option<Admission<'_>> {
    match check {
        SettlementCheck::ValidatedClaim { proof, .. } => match proof {
            Some(proof) => Some(Admission::Required(
                &proof.obligation,
                proof.basis.as_str(),
                Vec::new(),
            )),
            // Re-review L3: a proof this reader cannot interpret must
            // never *weaken* admission. Before, `None` here meant "no
            // obligation to admit", which let class + scope + kind alone
            // authorise a mint. It is unreachable today — both readiness
            // functions always construct `Some` — but the shape is the
            // hazard, so it is closed rather than argued about: an
            // unreadable proof is unmintable, full stop.
            None => Some(Admission::Unreadable),
        },
        // An agentic review's own basis already binds its execution (the
        // Actor World hash covers repository, branch, base_sha, source
        // basis, intent, output contract and boundary), exactly as a
        // deterministic check's does. It carries no further mechanism.
        SettlementCheck::ActorReview { proof, .. } => Some(Admission::Required(
            &proof.obligation,
            proof.basis.as_str(),
            Vec::new(),
        )),
        SettlementCheck::ChildReceipt { proof, .. } => match proof {
            Some(proof) => Some(Admission::Required(
                &proof.obligation,
                proof.basis.as_str(),
                proof
                    .roles
                    .iter()
                    .map(|role| role.mechanism_basis.as_str())
                    .collect(),
            )),
            None => Some(Admission::Unreadable),
        },
        SettlementCheck::SupersededBy { .. } => None,
    }
}

fn try_mint_settlement(
    policy: &PolicyState,
    finding: &Finding,
    ready: &ReadySettlement,
    at_startup: bool,
) -> Option<Settlement> {
    let PolicyState::Loaded(policy) = policy else {
        return None;
    };
    // W-B obligation proof: class + scope + kind selects *which* policy
    // entry could apply; it admits nothing on its own. For the two
    // classes that discharge a verification obligation, that entry must
    // additionally have pre-admitted this exact obligation — by id,
    // edition, **and** the content basis wirk itself re-derived from the
    // Route definition and the immutable execution basis. `SupersededBy`
    // discharges no verification obligation (it is a Work replacing its
    // own provisional record, whose authority is same-Work authorship,
    // already checked in `fold`), so it carries none and is admitted by
    // class/scope/kind alone, exactly as before.
    let required = obligation_admission(&ready.check);
    let enabled = policy.classes.iter().any(|entry| {
        entry.class == ready.class
            && entry.scope == finding.scope
            && entry.kinds.contains(&finding.kind)
            && match &required {
                // `SupersededBy` discharges no verification obligation.
                None => true,
                Some(Admission::Unreadable) => false,
                Some(Admission::Required(obligation, basis, mechanisms)) => {
                    entry.obligations.iter().any(|admitted| {
                        admitted.id == obligation.id
                            && admitted.edition == obligation.edition
                            && admitted.basis == *basis
                            // Every verification execution this
                            // settlement actually rests on must itself
                            // be admitted for this obligation.
                            && mechanisms
                                .iter()
                                .all(|used| admitted.mechanisms.iter().any(|ok| ok == used))
                    })
                }
            }
    });
    if !enabled {
        return None;
    }
    let settled_by = match &ready.check {
        SettlementCheck::ValidatedClaim { claim_event, .. }
        | SettlementCheck::ActorReview { claim_event, .. } => claim_event.clone(),
        SettlementCheck::ChildReceipt { closed_event, .. } => closed_event.clone(),
        SettlementCheck::SupersededBy { raise_event, .. } => raise_event.clone(),
    };
    Some(Settlement {
        authority: SettlementAuthority {
            class: ready.class,
            policy_version: policy.version,
            policy_digest: policy.digest.clone(),
        },
        check: ready.check.clone(),
        settled_by,
        at: now_ts(),
        minted_at_startup: at_startup,
    })
}

// ---- Settlement: the daemon's cross-journal assembly ----------------------

fn collect_child_receipts(receipts: &[OutcomeReceipt], out: &mut Vec<(String, WorkId, ClaimId)>) {
    for receipt in receipts {
        match receipt {
            OutcomeReceipt::Child {
                role, child, claim, ..
            } => out.push((role.clone(), child.clone(), claim.clone())),
            OutcomeReceipt::Container { receipts, .. } => collect_child_receipts(receipts, out),
            OutcomeReceipt::Leaf { .. } => {}
        }
    }
}

fn find_stage_closed_event_id(events: &[Event], id: &WaypointId, attempt: u32) -> Option<EventId> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::StageClosed {
            waypoint,
            attempt: at,
            ..
        } if waypoint == id && *at == attempt => Some(event.id.clone()),
        _ => None,
    })
}

/// W-B-AGENTIC-PROOF.md: the agentic sibling of
/// `deterministic_verified_readiness`. A bounded independent Actor review
/// discharges its own Waypoint's declared review obligation.
///
/// **Why this lives in the daemon and not in the pure fold.** Every other
/// part of this check is a same-journal fact, but one is not: a declared
/// review *target* is a resource path, and the reviewer's own admitted
/// evidence carries an opaque, already-encoded `ExactCoordinate`
/// (`EvidenceRef::Source`). Decoding it is an Atlas concept, and
/// `wirk-core` deliberately does not depend on `wirk-atlas` (0022 D71).
/// So the daemon assembles this candidate, exactly as it already
/// assembles `child_investigation_ready` — the pure fold keeps reading
/// only what it was handed.
///
/// **What the previous revision got wrong.** It required every child
/// verification mechanism to be a `Deterministic` obligation, on the
/// stated ground that wirk "cannot content-address an Actor
/// investigation". That conflated two different things: an Actor's
/// *reasoning* is not deterministic, but its *execution inputs* have had
/// a content identity all along — `WorldHash::of`'s own `Actor` arm
/// covers repository, branch, `base_sha`, source basis, **intent**,
/// output contract and boundary. `raw/00-red-actor-review-refused.txt`
/// records the consequence on the frozen `fad933dd`: a real, materialized,
/// claimed, target-applied review that the operator had no value to admit
/// and that settled nothing.
///
/// **What is bound here**, all re-derived from the journal:
///
/// 1. The Finding names an obligation, and some `Actor` Waypoint in this
///    Work's own frozen Route declares exactly that obligation *with a
///    review contract*. An Actor obligation without one declares no
///    mechanism and discharges nothing.
/// 2. The reviewer's own **structured decision** — the Finding's `kind` —
///    is one of the closed set the contract declares. Prose is never the
///    decision; a copied sentence discharges nothing, and the reviewer's
///    own sentence stays a recorded, unverified claim.
/// 3. The Finding cites, as evidence, that Waypoint's own Run's Validated
///    `Done` `ClaimRecorded` — the completed review's receipt, not a
///    neighbouring success.
/// 4. **Current activation, both axes**: that Run is the current one for
///    its Waypoint and was opened against the currently reserved World.
/// 5. The obligated `outputs` (the review report) are present in that
///    Claim's own receipt with real recorded digests. A generic `Done`
///    artifact is not a report.
/// 6. Every declared **target** is covered by an entry on the reviewer's
///    own Finding that this daemon already **admitted** against the
///    reviewing Work's own source bindings, and whose decoded coordinate
///    path is that exact target. Unrelated admitted evidence covers
///    nothing, and an `Unavailable` entry counts for nothing. The exact
///    generation and object id are recorded in the proof, so the reviewed
///    source basis stays explicit.
/// 7. `obligation_basis` over the whole authored obligation — including
///    the recipe, the targets and the declared decision set — bound to
///    that Actor World hash, which `try_mint_settlement` then requires the
///    estate policy to have admitted. A changed intent, target, recipe,
///    decision set or source generation of the reviewed checkout is a
///    different basis and a fresh admission.
///
/// The Route-position narrowing the deterministic path uses is kept: a
/// Finding cannot cite a Waypoint its own Route has not reached.
fn actor_reviewed_readiness(
    state: &Arc<WirkdState>,
    events: &[Event],
    work: &Work,
    finding: &Finding,
) -> Option<ReadySettlement> {
    // (1)
    let named = finding.obligation.as_ref()?;
    let defs = waypoint_defs_for(events);
    let sequence = flatten_leaves(&defs);
    let finding_position = sequence.iter().position(|id| id == &finding.waypoint)?;

    for item in &finding.evidence {
        let EvidenceRef::Journal {
            work: cited_work,
            event: event_id,
        } = &item.reference
        else {
            continue;
        };
        if cited_work != &work.id {
            continue;
        }
        let Some(claim_event) = events.iter().find(|event| &event.id == event_id) else {
            continue;
        };
        // (3)
        let EventKind::ClaimRecorded {
            claim,
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
            artifacts,
        } = &claim_event.kind
        else {
            continue;
        };
        let Some(run_id) = &claim_event.run else {
            continue;
        };
        let Some((waypoint, attempt, world_hash)) = run_opening_of(events, run_id) else {
            continue;
        };
        let Some(claim_position) = sequence.iter().position(|id| id == &waypoint) else {
            continue;
        };
        if claim_position > finding_position {
            continue;
        }
        // (4)
        if latest_run_for_waypoint(events, &waypoint).map(|entry| entry.0) != Some(run_id.clone()) {
            continue;
        }
        let Some(reserved) = latest_reservation_for_waypoint(events, &waypoint) else {
            continue;
        };
        if reserved.0 != world_hash {
            continue;
        }
        let Some(def) = find_definition(&defs, &waypoint) else {
            continue;
        };
        if def.kind != WaypointKind::Actor {
            continue;
        }
        // (1), continued: the declared obligation and its review contract.
        let Some(obligation) = def.verifies.as_ref() else {
            continue;
        };
        if obligation.id != named.id || obligation.edition != named.edition {
            continue;
        }
        let Some(review) = obligation.review.as_ref() else {
            continue;
        };
        // (2) the structured decision, from the declared closed set.
        if !review.decisions.contains(&finding.kind) {
            continue;
        }
        // (5) the obligated report really validated.
        if !obligation.outputs.iter().all(|name| {
            artifacts
                .iter()
                .any(|receipt| &receipt.name == name && !receipt.digest.is_empty())
        }) {
            continue;
        }
        let World::Actor(actor) = &reserved.1 else {
            continue;
        };
        // (6) the review's targets were frozen into this very World at
        // reservation, one per declared selector; each must be covered by
        // admitted evidence on the reviewer's own Finding, matched on the
        // complete identity. A selector that failed to resolve froze
        // nothing, so a short list refuses here as well.
        if actor.review_targets.len() != review.targets.len() {
            continue;
        }
        let Some(targets) = reviewed_targets(&actor.review_targets, finding) else {
            continue;
        };
        // (7)
        let Some(basis) = obligation_basis(def, Some(&world_hash)) else {
            continue;
        };
        let report: Vec<ArtifactReceipt> = artifacts
            .iter()
            .filter(|receipt| obligation.outputs.contains(&receipt.name))
            .cloned()
            .collect();
        let _ = state;
        return Some(ReadySettlement {
            finding: finding.id.clone(),
            class: SettlementClass::ActorReviewed,
            check: SettlementCheck::ActorReview {
                work: work.id.clone(),
                claim: claim.clone(),
                claim_event: event_id.clone(),
                proof: ActorReviewProof {
                    obligation: ObligationRef {
                        id: obligation.id.clone(),
                        edition: obligation.edition.clone(),
                    },
                    basis,
                    proves: obligation.proves.clone(),
                    waypoint,
                    attempt,
                    world_hash,
                    intent: actor.intent.clone(),
                    recipe: review.recipe.clone(),
                    targets,
                    decision: finding.kind,
                    report,
                },
            },
        });
    }
    None
}

/// Every **frozen** review target, matched against the reviewer's own
/// admitted `applies_to` entries — on the complete identity, not on a
/// path.
///
/// The independent re-review's executed C1: when this compared the
/// decoded coordinate's `path` to a declared string and ignored its
/// membership, source and generation, an admitted review of `demo`'s
/// current `socket.rs` was discharged equally by a coordinate in a
/// *different admitted repository* and by an *earlier generation the
/// reviewing World never opened against*, and the settled record could
/// not tell the three apart. Path equality is not target identity.
///
/// Now every frozen target must be covered by an entry that is an
/// `Admitted` `Source` reference whose decoded coordinate agrees on
/// **estate, membership, source, generation, path and object id**. An
/// `Unavailable` entry, a `Journal` reference, a same-path coordinate in
/// another membership and a same-path coordinate at another generation
/// all cover nothing. The reviewing World was frozen against these exact
/// identities at reservation and `obligation_basis` binds them, so this
/// is the same target the estate admitted, not merely a matching name.
fn reviewed_targets(frozen: &[ReviewTarget], finding: &Finding) -> Option<Vec<ReviewTarget>> {
    if frozen.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for target in frozen {
        let matched = finding.applies_to.iter().any(|item| {
            let EvidenceRef::Source(encoded) = &item.reference else {
                return false;
            };
            if !matches!(item.outcome, EvidenceOutcome::Admitted { .. }) {
                return false;
            }
            let Ok(coordinate) = decode_coordinate(encoded) else {
                return false;
            };
            coordinate.estate.0 == target.estate
                && coordinate.membership.0 == target.membership
                && coordinate.source.0 == target.source_id
                && coordinate.generation.0 == target.generation
                && coordinate.object_id == target.object_id
                && coordinate.path == target.path.as_bytes()
        });
        if !matched {
            return None;
        }
        out.push(target.clone());
    }
    Some(out)
}

/// The Waypoint, attempt and World hash a Run was opened against — the
/// daemon's own copy of `wirk-core`'s private `run_opening`, which the
/// crate boundary keeps out of reach here.
fn run_opening_of(events: &[Event], run: &RunId) -> Option<(WaypointId, u32, WorldHash)> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::RunOpened {
            run: id,
            waypoint,
            attempt,
            world_hash,
        } if id == run => Some((waypoint.clone(), *attempt, world_hash.clone())),
        _ => None,
    })
}

/// The World currently reserved for `waypoint`, with its hash — the
/// reviewing World whose `intent` the proof records, and the currency
/// check a superseded reservation fails.
fn latest_reservation_for_waypoint(
    events: &[Event],
    waypoint: &WaypointId,
) -> Option<(WorldHash, World)> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::WaypointReserved {
            waypoint: id,
            world_hash,
            world,
        } if id == waypoint => Some((world_hash.clone(), world.clone())),
        _ => None,
    })
}

/// The child settlement that discharged one obligated role: the named
/// child's own `FindingSettled` for a `DeterministicVerified` check
/// whose proof names exactly `requires`. This is the "actual immutable
/// verification execution" a container obligation stands for — the
/// child really ran a content-addressed check, and its own settlement
/// already required this estate to admit that check's basis.
fn role_discharge(
    child_events: &[Event],
    child: &WorkId,
    role: &str,
    claim: &ClaimId,
    requires: &ObligationRef,
    named_finding: Option<&FindingId>,
) -> Option<DischargedRole> {
    let child_work = fold(child_events);
    for (finding_id, record) in &child_work.findings {
        if let Some(named) = named_finding
            && finding_id != named
        {
            continue;
        }
        let FindingState::Settled(settlement) = &record.state else {
            continue;
        };
        // W-B-AGENTIC-PROOF.md: both mechanisms discharge a container's
        // child obligation, and they stay distinct. A deterministic check
        // and a bounded independent Actor review are different kinds of
        // evidence with different standing; what they share is that each
        // is an estate-admitted, content-addressed verification the child
        // really settled. Neither is required to be dressed as the other,
        // and in particular no synthetic deterministic step is needed to
        // rubber-stamp a review.
        let (discharged, mechanism_basis) = match (&settlement.authority.class, &settlement.check) {
            (
                SettlementClass::DeterministicVerified,
                SettlementCheck::ValidatedClaim {
                    proof: Some(proof), ..
                },
            ) => (&proof.obligation, &proof.basis),
            (SettlementClass::ActorReviewed, SettlementCheck::ActorReview { proof, .. }) => {
                (&proof.obligation, &proof.basis)
            }
            _ => continue,
        };
        if discharged != requires {
            continue;
        }
        let Some(settled_event) = child_events.iter().rev().find_map(|event| {
            matches!(&event.kind, EventKind::FindingSettled { finding, .. } if finding == finding_id)
                .then(|| event.id.clone())
        }) else {
            continue;
        };
        return Some(DischargedRole {
            role: role.to_string(),
            child: child.clone(),
            claim: claim.clone(),
            finding: finding_id.clone(),
            mechanism: discharged.clone(),
            mechanism_basis: mechanism_basis.clone(),
            settled_event,
        });
    }
    None
}

/// W-B obligation proof for the child-investigation class
/// (`W-B-OBLIGATION-CORRECT.md`; construction review "policy proves the
/// named obligation"; authority adjudication "a child reference must be
/// tied to the obligated outcome and current two-sided activation").
///
/// Rebuilt after the independent review executed two counterexamples
/// against the previous revision:
///
/// - **C1**: a container's `obligation_basis` hashed prose and outcome
///   shape only, so a rogue Route — different route id, waypoint id,
///   repository and leaf command — collided to the admitted basis, and a
///   child whose entire investigation was one Finding reading *"I did
///   not investigate anything"*, citing its own submission event,
///   settled a statement about a socket-mode investigation. "Any
///   admitted evidence at all" is not an investigation.
/// - **C2**: `VerificationObligation.outputs` was hashed and never read
///   for containers, so `outputs: ["auditor"]` discharged through role
///   `scribe` while the auditor role never existed.
///
/// What is bound now, all re-derived here:
///
/// 1. The parent names the obligation **and** names the confirming child
///    Finding explicitly. Neither is inferred.
/// 2. The container Waypoint declares exactly that obligation, and that
///    obligation declares a **mechanism** (`requires`) and at least one
///    **obligated role** (`outputs`). A container obligation with
///    neither obliges nothing and discharges nothing.
/// 3. The container's **current** activation (`container_attempt` at the
///    Finding's own raise position) is closed.
/// 4. **Every** obligated role in `outputs` has a Child receipt in that
///    activation — partial completion is never full proof — and each
///    such child has really **settled** a `DeterministicVerified`
///    Finding discharging exactly `requires`. That child settlement's
///    own basis content-addresses its command, source basis and expected
///    artifacts, and `try_mint_settlement` requires this estate to have
///    admitted it as a `mechanism` of this container obligation.
/// 5. **Two-sided** attribution: the child's own `WorkSubmitted.parent`
///    binding names this parent, container, activation and role. This
///    re-verifies a fact the receipt was only minted after checking
///    (`child_work_completed_receipt`) and that `work submit` refuses to
///    create (`ChildParentMismatch`); it is defence in depth, and the
///    real-service checks for both are recorded in `CONTRACT-CHECKS.md`.
/// 6. The receipt's own `ClaimId` is a Validated `Done` Claim in the
///    child's journal.
/// 7. The explicitly named `confirmed_by` Finding is one of the role
///    discharges — the parent cites a real confirmation, not a bystander.
fn child_investigation_ready(
    state: &Arc<WirkdState>,
    parent_events: &[Event],
    parent_work: &Work,
    finding: &Finding,
) -> Option<ReadySettlement> {
    // (1)
    let named_obligation = finding.obligation.as_ref()?;
    let confirmed_by = finding.confirmed_by.as_ref()?;
    let raise_index = parent_events.iter().position(|event| {
        matches!(&event.kind, EventKind::FindingRaised { finding: raised } if raised.id == finding.id)
    })?;
    let defs = waypoint_defs_for(parent_events);
    let mut containers: Vec<&WaypointDefinition> = Vec::new();
    fn walk<'a>(nodes: &'a [WaypointDefinition], out: &mut Vec<&'a WaypointDefinition>) {
        for node in nodes {
            if matches!(node.kind, WaypointKind::Container) {
                out.push(node);
                walk(&node.leaves, out);
            }
        }
    }
    walk(&defs, &mut containers);

    for container in containers {
        // (2)
        let Some(obligation) = container.verifies.as_ref() else {
            continue;
        };
        if obligation.id != named_obligation.id || obligation.edition != named_obligation.edition {
            continue;
        }
        let Some(requires) = obligation.requires.as_ref() else {
            continue;
        };
        if obligation.outputs.is_empty() {
            continue;
        }
        let Some(basis) = obligation_basis(container, None) else {
            continue;
        };
        // (3)
        let activation = container_attempt(&parent_events[..raise_index], &container.id);
        let Some(StageOutcomeRef::Closed(receipts)) =
            stage_outcome_at(parent_events, &container.id, activation)
        else {
            continue;
        };
        let mut children = Vec::new();
        collect_child_receipts(&receipts, &mut children);

        // (4) every obligated role, exactly matched against this
        // activation's own current receipts.
        let mut discharged: Vec<DischargedRole> = Vec::new();
        for obligated_role in &obligation.outputs {
            let mut found = None;
            for (role, child, claim) in &children {
                if role != obligated_role {
                    continue;
                }
                let Some(child_events) = replay_events(state, child) else {
                    continue;
                };
                // (5) the child's own half of the binding.
                let child_work = fold(&child_events);
                let bound = child_work.parent.as_ref().is_some_and(|binding| {
                    binding.work == parent_work.id
                        && binding.waypoint == container.id
                        && binding.attempt_or_first() == activation
                        && &binding.role == role
                });
                if !bound {
                    continue;
                }
                // (6) the receipt's Claim is really that child's own
                // Validated Done Claim.
                let claim_validated = child_events.iter().any(|event| {
                    matches!(
                        &event.kind,
                        EventKind::ClaimRecorded {
                            claim: recorded,
                            claim_kind: ClaimKind::Done,
                            verdict: ClaimVerdict::Validated,
                            ..
                        } if recorded == claim
                    )
                });
                if !claim_validated {
                    continue;
                }
                // The role the parent explicitly cited must be
                // discharged by the exact Finding it cited; any other
                // obligated role is discharged by whichever of that
                // child's settled Findings answers `requires`.
                let named = (child == &confirmed_by.work).then_some(&confirmed_by.finding);
                if let Some(entry) =
                    role_discharge(&child_events, child, role, claim, requires, named)
                {
                    found = Some(entry);
                    break;
                }
            }
            let Some(entry) = found else {
                break;
            };
            discharged.push(entry);
        }
        if discharged.len() != obligation.outputs.len() {
            continue;
        }
        // (7) the named confirmation is one of the real discharges.
        let Some(cited) = discharged.iter().find(|entry| {
            entry.child == confirmed_by.work && entry.finding == confirmed_by.finding
        }) else {
            continue;
        };
        let (role, child, claim) = (cited.role.clone(), cited.child.clone(), cited.claim.clone());
        let child_raise_event = replay_events(state, &child).and_then(|events| {
            events.iter().find_map(|event| {
                matches!(&event.kind, EventKind::FindingRaised { finding: raised }
                    if raised.id == confirmed_by.finding)
                .then(|| event.id.clone())
            })
        })?;
        let Some(closed_event) =
            find_stage_closed_event_id(parent_events, &container.id, activation)
        else {
            continue;
        };
        return Some(ReadySettlement {
            finding: finding.id.clone(),
            class: SettlementClass::ChildInvestigationConfirmed,
            check: SettlementCheck::ChildReceipt {
                parent: parent_work.id.clone(),
                waypoint: container.id.clone(),
                attempt: activation,
                child,
                role,
                claim,
                closed_event,
                child_raise_event,
                proof: Some(ChildProof {
                    obligation: ObligationRef {
                        id: obligation.id.clone(),
                        edition: obligation.edition.clone(),
                    },
                    basis,
                    proves: obligation.proves.clone(),
                    confirmed_by: confirmed_by.finding.clone(),
                    requires: requires.clone(),
                    roles: discharged,
                }),
                unread: UnreadFields::default(),
            },
        });
    }
    None
}

/// The one function that actually mints `FindingSettled` (§2.4's own
/// rule: "`FindingSettled` has exactly one producer"). Called after
/// `handle_claim_inner` appends `ClaimRecorded`, after
/// `reevaluate_parent_inner`'s own `close_cascade` appends `StageClosed`,
/// after `handle_finding_raise`, on demand from `finding settle`, and at
/// startup over every eligible journal (`settle_ready_findings`).
/// Idempotent by construction: a finding already `Settled` is skipped
/// (`FindingState::Proposed` guard), so calling this redundantly from
/// several trigger points is always safe.
/// Every settlement candidate for `work`, whatever its class: the pure,
/// same-journal ones `fold` already derived (`DeterministicVerified`,
/// `SupersededInOrigin`), plus the two the daemon assembles because they
/// need something the core cannot see — `ActorReviewed` (decoding an
/// Atlas coordinate to match a declared review target) and
/// `ChildInvestigationConfirmed` (another Work's journal). Factored out
/// of `settle_ready` so `finding settle` can name *why* a finding is
/// still pending without a second, drifting copy of this list.
fn settlement_candidates(
    state: &Arc<WirkdState>,
    events: &[Event],
    work: &Work,
) -> Vec<ReadySettlement> {
    let mut candidates = work.settlement_ready.clone();
    for (finding_id, record) in &work.findings {
        if !matches!(record.state, FindingState::Proposed) {
            continue;
        }
        if candidates.iter().any(|ready| &ready.finding == finding_id) {
            continue;
        }
        // W-B-AGENTIC-PROOF.md: an agentic review is scope-agnostic like
        // the deterministic class — `try_mint_settlement` is what matches
        // scope against the policy — while `child_investigation_confirmed`
        // stays `EstateLocal`, as it always was.
        if let Some(ready) = actor_reviewed_readiness(state, events, work, &record.finding) {
            candidates.push(ready);
            continue;
        }
        if record.finding.scope != FindingScope::EstateLocal {
            continue;
        }
        if let Some(ready) = child_investigation_ready(state, events, work, &record.finding) {
            candidates.push(ready);
        }
    }
    candidates
}

fn settle_ready(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    at_startup: bool,
) -> Result<(), JournalError> {
    let Some(journal_handle) = journal_for(state, work_id)? else {
        return Ok(());
    };
    // Ruling 0119, the journal lock discipline: `settlement_candidates`
    // assembles the cross-journal `ChildInvestigationConfirmed` class,
    // which reads a child Work's journal, so it runs with no guard held.
    // Same observe/decide/re-check shape as `handle_finding_raise`, and
    // for the same reason: the settlement this mints must rest on a
    // journal that has not moved since it was read, and the append must
    // happen under the guard the check ran under.
    let mut attempt = 0usize;
    let (mut journal, work, policy, candidates) = loop {
        attempt += 1;
        let events = {
            let journal = lock_journal(&journal_handle);
            journal.replay()?
        };
        if events.is_empty() {
            return Ok(());
        }
        let work = fold(&events);
        let policy = read_settlement_policy(state);

        // Pure, same-journal-derivable candidates (`DeterministicVerified`,
        // `SupersededInOrigin`), plus cross-journal `ChildInvestigationConfirmed`
        // candidates the daemon assembles here — the core `fold` never reads
        // another journal (construction review's own rule).
        let candidates = settlement_candidates(state, &events, &work);

        let journal = lock_journal(&journal_handle);
        if same_observation(&events, &journal.replay()?) {
            break (journal, work, policy, candidates);
        }
        drop(journal);
        if attempt >= JOURNAL_OBSERVATION_ATTEMPTS {
            // Settlement is idempotent and re-triggered by every raise,
            // claim, `finding settle` and daemon start, so a Work this
            // busy is re-evaluated by the next trigger rather than
            // spun on here.
            eprintln!(
                "wirkd: settlement evaluation for {} kept losing to concurrent appends; \
                 the next trigger re-evaluates it",
                work_id.0
            );
            return Ok(());
        }
    };

    let mut settled_any = false;
    for ready in candidates {
        let Some(record) = work.findings.get(&ready.finding) else {
            continue;
        };
        if !matches!(record.state, FindingState::Proposed) {
            continue;
        }
        let Some(settlement) = try_mint_settlement(&policy, &record.finding, &ready, at_startup)
        else {
            continue;
        };
        let event = new_event(
            work_id,
            None,
            EventKind::FindingSettled {
                finding: ready.finding.clone(),
                settlement,
            },
        );
        append_event(state, &mut journal, work_id, &event)?;
        settled_any = true;
    }
    // Authority review, §8 ("Real gap, executed"): a settled estate
    // record must be visible to estate consultation immediately, not
    // only after the next restart or an explicit `--rebuild`.
    // `append_finding_row` is content-addressed and idempotent (§7), so
    // reusing the same full sweep `reconcile_findings_index` already
    // performs at startup here is exactly as safe, never a second
    // protocol.
    if settled_any {
        drop(journal);
        reconcile_findings_index(state);
    }
    Ok(())
}

/// Startup sweep (§6, mirroring `reevaluate_waiting_works`'s own
/// once-at-startup convention): every eligible journal, **terminal
/// included, no state or recency filter** — the terminal design's own
/// defect (a completed Work missing its settlement would otherwise stay
/// missing indefinitely).
fn settle_ready_findings(state: &Arc<WirkdState>) {
    let works_dir = state.estate_root.join("works");
    let Ok(entries) = std::fs::read_dir(&works_dir) else {
        return;
    };
    let mut work_ids = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Pure discovery: read the journal that is there, create
        // nothing, and read one an operator left read-only
        // (`discovery_events`). The mutation this sweep decides on
        // still goes through the one write path below.
        let Some(events) = discovery_events(&dir) else {
            continue;
        };
        if let Some(event) = events.first() {
            work_ids.push(event.work.clone());
        }
    }
    for work_id in work_ids {
        if let Err(err) = settle_ready(state, &work_id, true) {
            eprintln!(
                "wirkd: startup settlement evaluation failed for {}: {err}",
                work_id.0
            );
        }
    }
}

// ---- The estate Findings index (§7) ---------------------------------------

fn find_raised_finding(events: &[Event], finding_id: &FindingId) -> Option<(EventId, Finding)> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::FindingRaised { finding } if &finding.id == finding_id => {
            Some((event.id.clone(), finding.clone()))
        }
        _ => None,
    })
}

/// What one walk of the estate's Work journals actually observed: the
/// rows it could build, **and** whatever it could not read.
///
/// The second half is the whole point. The walk this replaced returned a
/// bare `Vec` and dropped every `read_dir`, `Journal::open` and `replay`
/// error on the floor, so a canonical journal that could not be read —
/// a real `EACCES` on a wrongly-owned estate, or the torn final line a
/// crash between `write_all` and `sync_all` leaves, which `Journal`
/// fails closed on by design — produced a *short* row set that was
/// indistinguishable from a genuinely complete one. Appending that set
/// then reported `Synchronized`, and `--rebuild` replaced the whole
/// index with it and reported `Synchronized` too: valid rows deleted,
/// exit 0, empty stderr (`index-health-adversarial/REPORT.md` case 1,
/// executed; ruling 0125).
///
/// A projection built from a partial walk is missing rows nobody
/// counted. So this carries the fact forward and its holder is the one
/// thing that decides whether an attempt may call itself synchronized.
struct CanonicalScan {
    rows: Vec<wirk_atlas::FindingRow>,
    /// One entry per thing under `works/` this walk could not read,
    /// named the way an operator repairing it needs and nobody else
    /// ever sees it: `unreadable` reaches a caller only through
    /// `IndexProjection::Behind`'s `detail`, which is `--admin` only.
    unreadable: Vec<String>,
    /// One entry per Work the **standing index** durably holds rows for
    /// that this walk did not reproduce, filled in by `account` under
    /// the same Atlas hold that publishes.
    ///
    /// Kept beside `unreadable` and not inside it because they are not
    /// the same fact. `unreadable` is "this walk hit an error reading a
    /// journal"; this is "this walk hit no error at all and still came
    /// back short of evidence the estate already published", which is
    /// what an absent journal, an absent Work directory, a journal some
    /// other startup path re-created empty, and a canonical history
    /// that is valid but shorter than it was all look like. None of
    /// them raises an error anywhere, and every one of them is a walk
    /// that did not observe the estate.
    ///
    /// Admin-only on exactly the same terms as `unreadable`.
    unaccounted: Vec<String>,
}

impl CanonicalScan {
    /// True when every journal the estate's layout says is canonical was
    /// actually read. An estate with no `works/` directory at all is
    /// complete and empty — a legal new estate is not a broken one.
    fn complete(&self) -> bool {
        self.unreadable.is_empty() && self.unaccounted.is_empty()
    }

    /// Compares this walk against what the index durably holds, under
    /// the Atlas hold, and records every Work the index has rows for
    /// that this walk produced nothing for.
    ///
    /// This is the only thing that can tell an absent Work from one that
    /// never existed, and it does it without any new store, any new file
    /// and any second read: the index is already read under this hold,
    /// and it already names the origin Work of every row it holds
    /// (`FindingOrigin::work`). A `works/` directory alone genuinely
    /// cannot distinguish the two — this does not pretend otherwise, it
    /// asks the durable projection instead.
    ///
    /// `unaccounted` must be computed against what the index held
    /// **before** this walk began (`unaccounted_finding_rows`'s own
    /// doc): an ordinary sweep walks outside the Atlas lock, so a
    /// concurrent mutation legitimately publishes rows this walk is
    /// older than, and those are a healthy estate.
    ///
    /// **Evidence, never authority.** What this establishes is that a
    /// walk was incomplete. Nothing here invents a Work, reconstructs a
    /// canonical event or writes a journal; the refusal it drives leaves
    /// the estate exactly as it found it, and the record is still the
    /// raising Work's journal. See `UnaccountedFindingRow` for why a
    /// missing row is evidence at all: the Finding lifecycle has no
    /// retraction and the journals are append-only, so a complete walk
    /// of an intact estate reproduces every row it produced before.
    fn account(&mut self, unaccounted: &[wirk_atlas::UnaccountedFindingRow]) {
        // Grouped by Work, in the index's own order, because that is the
        // unit an operator repairs: one restored journal explains every
        // row that came out of it.
        let mut by_work: Vec<(String, usize)> = Vec::new();
        for row in unaccounted {
            match by_work.iter_mut().find(|(work, _)| work == &row.work.0) {
                Some((_, count)) => *count += 1,
                None => by_work.push((row.work.0.clone(), 1)),
            }
        }
        self.unaccounted = by_work
            .into_iter()
            .map(|(work, count)| {
                format!(
                    "{work}: {count} indexed row{} this walk did not reproduce",
                    if count == 1 { "" } else { "s" }
                )
            })
            .collect();
    }

    /// Admin-only: what could not be read, and how many. Never rendered
    /// to a scoped requester, which learns the projection's state and
    /// nothing about its contents (`index_health_json`'s own rule).
    fn detail(&self) -> String {
        let mut parts = Vec::new();
        if !self.unreadable.is_empty() {
            parts.push(format!(
                "the estate's canonical journals could not be read completely, so how many rows the index is missing is unknown: {} unreadable ({})",
                self.unreadable.len(),
                self.unreadable.join("; ")
            ));
        }
        if !self.unaccounted.is_empty() {
            parts.push(format!(
                "the estate's canonical journals no longer account for rows this index already holds, so this walk did not observe the whole estate: {} unaccounted ({})",
                self.unaccounted.len(),
                self.unaccounted.join("; ")
            ));
        }
        parts.join("; ")
    }
}

/// What the standing index could be established to hold, before a
/// destructive `--rebuild` replaces it.
///
/// The rebuild's preservation check (`CanonicalScan::account`) is only
/// as good as its basis, and the basis used to be `AtlasStore::findings`
/// alone — an all-or-nothing read that refuses the whole file when one
/// line does not parse. That is the right rule for every ordinary read,
/// and it was the wrong basis here: `--rebuild` exists precisely to
/// repair a file with a malformed line, so the one case the check was
/// most needed for was the one case it was skipped in, and a published
/// Work's rows were deleted at exit 0 with `synchronized` on the reply
/// (ruling 0130, executed as `index-combined-verify` H2).
///
/// One malformed line is not evidence that no valid row is in the file.
/// `salvage_findings` parses the same file with the same parser, keeping
/// the rows that are rows and counting the lines that are not, and those
/// rows are known evidence the estate published — checkable, and checked.
/// The malformed lines are the honest remainder: unknown content, so
/// unknowable absence, so their bytes are preserved rather than replaced
/// away and the projection says its completeness is unknown.
///
/// **Never a second authority.** No variant here is ever returned to a
/// caller, written back into the index, or used to reconstruct a Work or
/// a journal event. Rows are compared by id against a canonical walk and
/// then dropped.
enum IndexBasis {
    /// The ordinary read succeeded — including on an absent or empty
    /// index, which honestly holds no rows.
    Read(Vec<wirk_atlas::FindingRow>),
    /// The ordinary read refused because at least one line is not a row,
    /// and a line-by-line salvage recovered the rest.
    Salvaged {
        salvaged: wirk_atlas::SalvagedFindingIndex,
        read_error: wirk_atlas::AtlasError,
    },
    /// The file itself could not be read: a real denial, or a directory
    /// where the file should be. Nothing about what the index holds can
    /// be established, so nothing may replace it.
    Unreadable {
        read_error: wirk_atlas::AtlasError,
        open_error: wirk_atlas::AtlasError,
    },
}

impl IndexBasis {
    /// The rows the index is **known** to hold, or `None` when that is
    /// not established at all. An empty slice and `None` are deliberately
    /// different answers: the first is "the index holds nothing", the
    /// second is "nobody knows what the index holds".
    fn known_rows(&self) -> Option<&[wirk_atlas::FindingRow]> {
        match self {
            IndexBasis::Read(rows) => Some(rows.as_slice()),
            IndexBasis::Salvaged { salvaged, .. } => Some(salvaged.rows.as_slice()),
            IndexBasis::Unreadable { .. } => None,
        }
    }

    /// Admin-only: what the salvage had to do, and what it could not
    /// establish. `None` when the ordinary read succeeded, which is the
    /// path every healthy rebuild takes and where nothing about this is
    /// said at all.
    fn salvage_note(&self) -> Option<String> {
        let IndexBasis::Salvaged {
            salvaged,
            read_error,
        } = self
        else {
            return None;
        };
        Some(format!(
            "the standing index could not be parsed whole ({read_error}), so this rebuild was checked against the {} row(s) of it that could still be read; {} line(s) could not be, and what they held cannot be established from anything in the estate",
            salvaged.rows.len(),
            salvaged.malformed.len()
        ))
    }
}

/// Reads the standing index as a basis a destructive replacement may be
/// judged against, falling back to the recovery read only when the
/// ordinary one refuses.
fn index_basis(atlas: &wirk_atlas::AtlasStore) -> IndexBasis {
    match atlas.findings() {
        Ok(rows) => IndexBasis::Read(rows),
        Err(read_error) => match atlas.salvage_findings() {
            // A salvage that finds no malformed line at all cannot
            // happen from a parse failure, but it can from a file that
            // changed between the two reads — and then the ordinary
            // read's refusal is the older fact, so this is treated as
            // the salvage it is rather than as a clean read.
            Ok(salvaged) => IndexBasis::Salvaged {
                salvaged,
                read_error,
            },
            Err(open_error) => IndexBasis::Unreadable {
                read_error,
                open_error,
            },
        },
    }
}

/// Every settled/asserted/applied row this estate's journals currently
/// support, EstateLocal only (`work_local_finding... never indexed`,
/// §5.4/§9) — the daemon's own journal walk, shared by `--rebuild` and
/// by startup reconciliation — **and** what it could not read
/// (`CanonicalScan`).
///
/// What is skipped and what is an error is decided by the estate's own
/// layout, never by "an error happened here". A `works/` that does not
/// exist is an estate with no Work in it. A directory entry that is not
/// a directory is not a Work. A Work whose journal is empty holds no
/// Findings. Each of those is a complete observation of nothing. Every
/// other failure — the directory listing denied, an entry whose type
/// cannot be read, a journal that will not open or will not replay — is
/// a journal this walk did not see, and is recorded as one.
fn all_finding_rows(state: &Arc<WirkdState>) -> CanonicalScan {
    let mut rows = Vec::new();
    let mut unreadable = Vec::new();
    let unaccounted = Vec::new();
    let works_dir = state.estate_root.join("works");
    let entries = match std::fs::read_dir(&works_dir) {
        Ok(entries) => entries,
        // A new estate, or one that has never submitted: complete, and
        // empty. The positive control the failure below must not swallow.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CanonicalScan {
                rows,
                unreadable,
                unaccounted,
            };
        }
        // Anything else — denied, not a directory, an I/O error — means
        // this walk saw none of the estate's journals and knows it.
        Err(error) => {
            unreadable.push(format!("works/: {error}"));
            return CanonicalScan {
                rows,
                unreadable,
                unaccounted,
            };
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                unreadable.push(format!("works/ entry: {error}"));
                continue;
            }
        };
        let dir = entry.path();
        // `file_type` on the entry, not `is_dir()` on the path: the
        // latter answers `false` for a directory it could not `stat`,
        // which is exactly the silent skip this walk is being cured of.
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {}
            Ok(_) => continue,
            Err(error) => {
                unreadable.push(format!("{}: {}", entry_name(&dir), with_causes(&error)));
                continue;
            }
        }
        // Read-only, and only readable: this walk is a pure scan, and
        // `Journal::open` — the estate's one write path — would
        // `create_dir_all`, create the file it did not find, and demand
        // append permission on a journal any reader can read
        // (`index-health-reverify/VERDICT.md`). Same format, same
        // replay, same fail-closed rule on a torn tail; no creation and
        // no write.
        let journal = match JournalReader::open(&dir) {
            Ok(journal) => journal,
            // The estate's own layout rule, the one `journal_for`
            // already applies to every other read: a directory under
            // `works/` with no journal file is **not a Work**, and this
            // walk is not the thing that decides otherwise by making
            // one. A complete observation of nothing, exactly as a
            // `works/` that does not exist is a complete observation of
            // an empty estate. Any other failure is a journal this walk
            // did not see, and is recorded as one.
            Err(JournalError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => {
                unreadable.push(format!("{}: {}", entry_name(&dir), with_causes(&error)));
                continue;
            }
        };
        let events = match journal.replay() {
            Ok(events) => events,
            Err(error) => {
                unreadable.push(format!("{}: {}", entry_name(&dir), with_causes(&error)));
                continue;
            }
        };
        // An empty journal is read in full and holds nothing. Not an
        // error, and not a Work either until its first event names one.
        let Some(work_id) = events.first().map(|event| event.work.clone()) else {
            continue;
        };
        for event in &events {
            let (kind, finding_id) = match &event.kind {
                EventKind::FindingSettled { finding, .. } => {
                    (wirk_atlas::FindingRowKind::Settled, finding)
                }
                EventKind::FindingAsserted { finding, .. } => {
                    (wirk_atlas::FindingRowKind::Asserted, finding)
                }
                EventKind::FindingApplied { finding, .. } => {
                    (wirk_atlas::FindingRowKind::Applied, finding)
                }
                _ => continue,
            };
            let Some((raised_event, finding)) = find_raised_finding(&events, finding_id) else {
                continue;
            };
            if finding.scope != FindingScope::EstateLocal {
                continue;
            }
            let superseded_by = match &event.kind {
                EventKind::FindingSettled {
                    settlement:
                        Settlement {
                            check: SettlementCheck::SupersededBy { finding, .. },
                            ..
                        },
                    ..
                } => Some(finding.clone()),
                _ => None,
            };
            rows.push(wirk_atlas::FindingRow {
                id: wirk_atlas::FindingRowId::compute(finding_id, kind, &event.id),
                kind,
                finding,
                origin: wirk_atlas::FindingOrigin {
                    work: work_id.clone(),
                    raised_event,
                    row_event: event.id.clone(),
                },
                settlement: match &event.kind {
                    EventKind::FindingSettled { settlement, .. } => Some(settlement.clone()),
                    _ => None,
                },
                assertion: match &event.kind {
                    EventKind::FindingAsserted { assertion, .. } => Some(assertion.clone()),
                    _ => None,
                },
                applied: match &event.kind {
                    EventKind::FindingApplied { application, .. } => Some(application.clone()),
                    _ => None,
                },
                superseded_by,
            });
        }
    }
    CanonicalScan {
        rows,
        unreadable,
        unaccounted,
    }
}

/// The directory's own name, never the path it sits at: `detail` is
/// administrative, but there is no reason for it to carry the estate's
/// location around when the Work directory identifies the journal
/// completely.
/// An error with its whole source chain, which is what an operator
/// repairing a denied journal needs: `JournalError`'s own `Display` for
/// an I/O failure is the bare words "journal io error", and the errno
/// that says *which* failure it was lives on the source beneath it.
fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    text
}

fn entry_name(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// Startup pass (§5.4): journal first (already true by construction —
/// this reads what `settle_ready_findings` just minted), index second.
/// Additive only (`append_finding_rows`, idempotent) — never the
/// destructive `--rebuild` overwrite.
///
/// Two repairs over the shape ruling 0116 accepted and recorded limits
/// against, both of them here rather than split across surfaces:
///
/// 1. **One rewrite, not one per row.** This offers every journaled row
///    on every call, and the old per-row loop re-read and atomically
///    rewrote the whole index once per row — O(rows²) bytes and O(rows)
///    `fsync` pairs for a single `assert`. `append_finding_rows` does
///    the identical read/dedup/rewrite once for the whole sweep, and
///    writes nothing at all when nothing is missing.
/// 2. **The outcome is recorded, not only printed.** The old sweep's
///    only report was `eprintln!` on the daemon's own stderr: the
///    mutating caller still got a complete-looking reply, and a later
///    `atlas findings` answered from a silently stale index. Every
///    attempt now lands in `WirkdState::index_health`, which every
///    mutating and every dependent query surface renders. The journal
///    stays canonical and the mutation stays accepted — a derived
///    projection falling behind is not a lost record — but nothing
///    claims to be synchronized while it is not.
///
/// **What this sweep does not carry.** It reports what its own
/// `append_finding_rows` established about the atlas directory and
/// nothing else, recorded inside the critical section that established
/// it. A directory fact made by an *earlier* critical section — a
/// retirement's renames and their `fsync` — is recorded where it
/// happened, by `record_directory_durability`, and is never handed to
/// this sweep to publish on its behalf. `record_directory_durability`'s
/// own doc has the reason: this sweep's ticket orders its *walk*, and a
/// walk ticket cannot order an `fsync`.
fn reconcile_findings_index(state: &Arc<WirkdState>) {
    // The ticket is taken **before** the walk, because the walk is what
    // this attempt's outcome is an observation of. Everything after this
    // line — the walk, the wait for the Atlas mutex, the append — only
    // ever makes this attempt's picture of the estate older.
    let observation = next_index_observation(state);
    // What the index durably held *before* this walk begins, which is
    // the only honest basis to judge the walk's completeness against.
    // The index legitimately moves forward under a sweep — this walk
    // runs outside the lock, so a concurrent mutation can journal and
    // publish a row this walk is simply older than, and that is a
    // healthy estate. Taken before the walk, released immediately, and
    // never held across it.
    let held_before_walk = {
        let atlas = state
            .atlas
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        atlas.findings()
    };
    // Outside the lock, unchanged and deliberately so: the walk reads
    // journal files through its own handles and acquires no mutex at
    // all, and what it produces is *offered* to an additive, idempotent
    // append. A row that lands between this walk and the append below is
    // added by the append's own re-read under the lock, never erased —
    // which is precisely why the destructive `--rebuild` path, further
    // down, must not do the same thing.
    let mut scan = all_finding_rows(state);
    // The window a verifier parks at to prove the ordering this sweep
    // is subject to is decided by *when its walk ran*, not by when its
    // record happens to arrive: after the walk, before the Atlas lock.
    wirk_atlas::checkpoint("findings-index-scanned");
    // {publish, record} is one critical section (ruling 0125, case 2C
    // executed). Held apart, an older attempt that appended first and
    // recorded last overwrote a newer attempt's genuine failure with its
    // own stale success, and the daemon then told every later caller
    // that a demonstrably incomplete index was complete. The Atlas mutex
    // already serializes the publication; extending it over the health
    // record makes the pair one ordered operation, and it is also what
    // lets a reader take `rows` and the health that describes them
    // together (`handle_atlas_findings`). Lock order is
    // `atlas -> index_health` and nothing anywhere inverts it: the only
    // three sites that touch `index_health` are `record_index_projection`,
    // `record_directory_durability` and `index_health_snapshot`, and none
    // of them takes another lock.
    let mut atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let outcome = atlas.append_finding_rows(&scan.rows);
    // A pre-walk read that itself failed is not evidence of anything —
    // and the append below re-read the same file, so it reports the
    // failure on its own account and this projection is `Behind` either
    // way.
    if let Ok(held) = &held_before_walk {
        scan.account(&wirk_atlas::unaccounted_finding_rows(held, &scan.rows));
    }
    let durability = directory_durability_of(&outcome);
    // The other fact this critical section established, and the one the
    // record used to take from a listing made after it: whether there
    // was an index file here. `established_backing` has the argument.
    let established = established_backing(&outcome);
    let projection = projection_of(&scan, outcome);
    // The window a verifier parks at to prove the two are one operation:
    // between the publication and the record, with the lock held.
    wirk_atlas::checkpoint("findings-index-published");
    record_index_projection(state, observation, projection, durability, established);
    drop(atlas);
}

/// What an `append_finding_rows` outcome establishes about the atlas
/// directory, read off the one thing that decides it: whether this call
/// reached the directory `fsync` at the end of `rewrite_rows`, and what
/// it returned.
///
/// `Ok(0)` is the common case after the first mutation and is
/// deliberately **not** `Confirmed`: the method's own contract is that a
/// sweep which finds nothing missing "writes nothing at all", so it
/// never opened the directory. A failure before the rename is
/// `Unestablished` for the same reason — it did not get that far.
fn directory_durability_of(
    outcome: &Result<wirk_atlas::FindingIndexAppend, wirk_atlas::FindingIndexUnwritten>,
) -> DirectoryDurability {
    match outcome {
        Ok(append) if append.appended == 0 => DirectoryDurability::Unestablished,
        Ok(_) => DirectoryDurability::Confirmed,
        Err(unwritten) => match &unwritten.error {
            wirk_atlas::AtlasError::DurabilityUncertain(detail) => {
                DirectoryDurability::Uncertain(detail.clone())
            }
            _ => DirectoryDurability::Unestablished,
        },
    }
}

/// What an `append_finding_rows` outcome establishes about the index
/// **file**, as opposed to the directory entry behind it.
///
/// Ruling 0137's recorded half. `RecordedBacking` is what the
/// observation behind a health record saw of the file, and a later read
/// that finds no file compares its own open against it; the whole
/// question is therefore *which* observation that was. It used to be a
/// listing of `atlas/` taken inside `record_index_projection` — after
/// this append had already returned, and after the checkpoint a verifier
/// can park the sweep at. An external deletion in that window recorded
/// `Absent` beside a `Synchronized` projection, and every later read of
/// the now-missing file read absent-now against absent-then as "an
/// estate that never wrote an index" and answered `complete: true` with
/// no rows (`loop-c3-index-read-verify/VERDICT.md` §4, executed on a
/// real daemon through this product's own barrier).
///
/// The append is the observation that actually looked. It opens the file
/// before it appends, and a rewrite renames one into place before it
/// returns, so `FindingIndexAppend::backing` is this critical section's
/// own answer — not a claim about any later moment, and not an
/// assumption that a listing taken afterwards is atomic with the write.
///
/// A **failed** append establishes nothing here on purpose — with the
/// one exception the writer itself can prove. Its read may have found a
/// file, but its projection is `Behind` on its own account either way,
/// so no completeness claim rests on it, and `FindingIndexUnwritten`
/// keeps the shape ruling 0130's evidence was taken against. `Unknown`
/// is exactly right for it: not evidence in either direction.
///
/// The exception is `DurabilityUncertain`, which is raised only *after*
/// the atomic rename (`backing_after_failed_index_write`, next to the
/// rename it is a fact about): that call really did publish an index
/// file, and a deletion in the window before the listing must not be
/// allowed to record the estate as one that never wrote one. Nothing
/// else is read out of a failure.
fn established_backing(
    outcome: &Result<wirk_atlas::FindingIndexAppend, wirk_atlas::FindingIndexUnwritten>,
) -> RecordedBacking {
    match outcome {
        Ok(append) => recorded_from(append.backing),
        Err(unwritten) => match wirk_atlas::backing_after_failed_index_write(&unwritten.error) {
            Some(backing) => recorded_from(backing),
            None => RecordedBacking::Unknown,
        },
    }
}

/// What a write's own observation of the index **file** is, said in the
/// record's vocabulary. The one translation between the two types, so a
/// second caller cannot spell it differently.
fn recorded_from(backing: wirk_atlas::IndexBacking) -> RecordedBacking {
    match backing {
        wirk_atlas::IndexBacking::Present => RecordedBacking::Present,
        wirk_atlas::IndexBacking::Absent => RecordedBacking::Absent,
    }
}

/// The same fact for the **rebuild**, whose write is a whole-file
/// replacement rather than an append.
///
/// `rebuild_finding_rows` renames a file into place before it can return
/// `Ok`, whatever the walk held — an estate with no findings included —
/// so a successful rebuild published an index file and says so. A
/// failure says nothing, except the one that happens *after* that rename
/// (`backing_after_failed_index_write`): rows visible, directory entry
/// unconfirmed, and a file that is demonstrably there.
///
/// The three refusals earlier on this path do not come through here at
/// all. They are decided before a byte is written — an unreadable
/// standing index, a partial walk, a preservation that failed — and each
/// passes `RecordedBacking::Unknown` explicitly, because a rebuild that
/// refused neither read the file nor wrote one and its own projection is
/// `Behind` on its own account.
fn rebuild_established_backing(
    outcome: &Result<wirk_atlas::IndexBacking, wirk_atlas::AtlasError>,
) -> RecordedBacking {
    match outcome {
        Ok(backing) => recorded_from(*backing),
        Err(error) => match wirk_atlas::backing_after_failed_index_write(error) {
            Some(backing) => recorded_from(backing),
            None => RecordedBacking::Unknown,
        },
    }
}

/// The next observation ticket for this daemon. Monotonic, per estate,
/// and taken exactly once per reconciliation attempt.
fn next_index_observation(state: &Arc<WirkdState>) -> u64 {
    state.index_observations.fetch_add(1, Ordering::SeqCst) + 1
}

/// What an attempt observed, from the two facts that decide it: whether
/// the canonical walk was complete, and what the write did.
///
/// The rule the silence in ruling 0125 case 1 comes down to: **a
/// successful append of a partial walk is not a synchronized index.**
/// The append can only ever report on the rows it was handed; the walk
/// is the only thing that knows whether those were all of them. When it
/// was not, how far behind the index is, is genuinely unknown — the
/// unreadable journals were never counted — so this reports the
/// `Behind { pending: None }` shape the surface already uses for exactly
/// that ("unknown, not zero"), and not a number nobody measured.
fn projection_of(
    scan: &CanonicalScan,
    outcome: Result<wirk_atlas::FindingIndexAppend, wirk_atlas::FindingIndexUnwritten>,
) -> IndexProjection {
    if !scan.complete() {
        // Kept from the write path and extended to the read of the
        // record: the operator-facing line on the daemon's own stderr,
        // which is no longer the only report either way.
        eprintln!(
            "wirkd: findings index reconciliation read the estate's journals incompletely: {}",
            scan.detail()
        );
    }
    match outcome {
        Ok(_) if scan.complete() => IndexProjection::Synchronized,
        Ok(_) => IndexProjection::Behind {
            pending: None,
            detail: scan.detail(),
        },
        Err(unwritten) => {
            eprintln!("wirkd: findings index reconciliation failed: {unwritten}");
            match (unwritten.error, scan.complete()) {
                (wirk_atlas::AtlasError::DurabilityUncertain(detail), true) => {
                    IndexProjection::DurabilityUnconfirmed { detail }
                }
                // The rows that were offered are visible, but the ones
                // the walk never read are not — an incomplete projection,
                // not a durability window, and reported as the more
                // serious of the two facts. Both are still said: the
                // durability half is the health record's own directory
                // fact now, and `qualified_by_unconfirmed_directory`
                // appends it to this detail exactly once — here and on
                // every later attempt, until a directory `fsync`
                // succeeds.
                (wirk_atlas::AtlasError::DurabilityUncertain(_), false) => {
                    IndexProjection::Behind {
                        pending: None,
                        detail: scan.detail(),
                    }
                }
                (error, true) => IndexProjection::Behind {
                    pending: unwritten.pending,
                    detail: error.to_string(),
                },
                // A count of what the write missed is not a count of
                // what the index is missing when the walk was short.
                (error, false) => IndexProjection::Behind {
                    pending: None,
                    detail: format!("{}; {error}", scan.detail()),
                },
            }
        }
    }
}

/// What a **failed** retirement established about the atlas directory,
/// read off the two places `PreservedIndexRetirementFailed` records it.
///
/// The failure itself is a directory sync when the last rename had
/// already landed (`operation: "confirm the rename(s) on disk"`), and
/// `retired_unconfirmed` is the same window on the partial path, where
/// the sync was attempted for the renames that had landed before a later
/// one was refused. A partial failure whose sync *succeeded* leaves
/// `retired_unconfirmed: None` with renames in `retired`, and that is a
/// real confirmation: the directory was `fsync`ed and returned success.
/// A failure with nothing renamed — the listing, or the very first
/// claim — never opened the directory at all.
fn retirement_durability(
    failed: &wirk_atlas::PreservedIndexRetirementFailed,
) -> DirectoryDurability {
    if let wirk_atlas::AtlasError::DurabilityUncertain(detail) = &failed.error {
        return DirectoryDurability::Uncertain(detail.clone());
    }
    match &failed.retired_unconfirmed {
        Some(wirk_atlas::AtlasError::DurabilityUncertain(detail)) => {
            DirectoryDurability::Uncertain(detail.clone())
        }
        Some(error) => DirectoryDurability::Uncertain(error.to_string()),
        None if !failed.retired.is_empty() => DirectoryDurability::Confirmed,
        None => DirectoryDurability::Unestablished,
    }
}

/// Records a directory fact this caller's **own** Atlas critical section
/// established, from inside that critical section. Called with the Atlas
/// guard held; the lock order is the same `atlas -> index_health` every
/// other site takes.
///
/// **Why a directory fact cannot travel to a later sweep, and why no
/// ticket fixes it.** An observation ticket orders a *walk*: it is taken
/// before the walk, and everything after it only makes that walk's
/// picture of the estate older, which is exactly what
/// `record_index_projection`'s staleness guard needs. An `fsync` is not
/// a walk. Every `fsync` of the atlas directory this daemon makes —
/// `append_finding_rows`, `retire_preserved_unreadable_indexes`,
/// `rebuild_finding_rows` — is made under the Atlas mutex, so **the
/// order of the critical sections is the order the syncs really
/// happened in**, and that is the only order a durability fact can be
/// judged by. A ticket is a different clock, and the two are not
/// comparable in either direction:
///
/// * A concurrent sweep can take its ticket, walk, and *then* wait for
///   the mutex. Its record is therefore newer than a retirement's while
///   its ticket is older.
/// * So the retirement's renames-and-`fsync` completed *before* that
///   sweep's failing `fsync`, and published *after* it. Handed to a
///   later sweep to publish, its `Confirmed` cleared a window a
///   genuinely later failure had just opened, and the estate read
///   `synchronized` / `complete: true` over a rename nothing had
///   confirmed on disk — the D3 outcome, reached from the other side
///   (`index-retirement-durability-verify/raw/44`, isolated by its
///   no-preserved-copy control `raw/45`).
/// * Taking the retirement's ticket *before* its Atlas guard does not
///   close it. The concurrent sweep's ticket is older still — it was
///   taken before the retirement's call arrived at all — so the stale
///   record is the *failure*, and the guard drops it for exactly the
///   right reason. Moving the ticket only moves which of two
///   incomparable clocks is read.
///
/// Recorded here, the fact lands in the critical section that made it:
/// a `fsync` that happened first is applied first, and a failure in a
/// later critical section is applied over it. The `--rebuild` arm
/// already works this way (its ticket and its record both live inside
/// its hold), and so does the sweep (`{publish, record}` is one critical
/// section, ruling 0125 case 2C); this is the same discipline applied to
/// the one path that had left it.
///
/// No ticket is taken and `observation` is not moved. It does not need
/// to be, and the reason is **not** the one this doc used to give.
///
/// It used to say that an older sweep landing afterwards carries
/// `Unestablished`, "a sweep can only carry what its own append
/// established". The second clause is true; the first does not follow
/// from it and is false. `append_finding_rows` returns `Ok(0)` without
/// opening a file only when the index already holds **every** offered
/// row (`wirk-atlas/src/findings.rs:210-220`); any row it lacks sends
/// the call into `rewrite_rows`, which renames and `fsync`s the atlas
/// directory. A walk with an **older** ticket can hold a row a newer
/// walk never read — because the newer walk was short — so its append
/// really does write, and really does `fsync` that directory, in a
/// critical section that runs strictly **after** the newer record. Its
/// `Confirmed` or `Uncertain` is a genuinely later directory fact
/// wearing an older walk's ticket, which is precisely the shape this
/// function exists to keep out of the walk's clock
/// (`index-retirement-order-verify/raw/61`, `raw/62`: executed against
/// a real daemon and a real `EIO`, on this source and on its parent).
///
/// What actually makes a ticket unnecessary is the discipline this
/// function names and `record_index_projection` now shares: the
/// directory fact is applied **under the Atlas guard of the critical
/// section that made it**, before and independently of any judgement
/// about how fresh the walk was. So an older sweep that lands
/// afterwards no longer needs to carry anything on this fact's behalf,
/// and no longer silently drops one of its own.
///
/// **Asymmetric, deliberately.** `Uncertain` re-qualifies the standing
/// projection here and now, through the same
/// `qualified_by_unconfirmed_directory` every record uses, so the window
/// is open the instant the failing `fsync` returns and does not wait on
/// a sweep whose own record may be discarded as stale. `Confirmed`
/// clears the field but leaves the projection to the sweep that follows:
/// the qualification is one-way — `Behind`'s detail has the sentence
/// concatenated into it — and re-deriving a projection is what a walk is
/// for. Reading `durability_unconfirmed` for the moment between is the
/// conservative direction, and the sweep this call always runs closes it.
fn record_directory_durability(state: &Arc<WirkdState>, durability: DirectoryDurability) {
    let mut health = state
        .index_health
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    apply_directory_durability(&mut health, durability);
}

/// The directory fact itself, applied to a health record the caller
/// already holds the lock on.
///
/// Split out of `record_directory_durability` so the sweep's own record
/// can apply it too, under the Atlas guard its `fsync` was made under
/// and **before** `record_index_projection`'s staleness guard has a say
/// — the one path where an `fsync` that really happened was being judged
/// by the freshness of the walk that happened to make it. One body, one
/// set of rules, called from both: there is no second store, no second
/// clock and no latch here, and the three outcomes mean exactly what
/// `DirectoryDurability` says they mean.
fn apply_directory_durability(health: &mut IndexHealth, durability: DirectoryDurability) {
    match durability {
        // This call did not write to the directory at all, so it has
        // nothing to say about it and says nothing — the standing record
        // stands.
        DirectoryDurability::Unestablished => {}
        DirectoryDurability::Confirmed => health.unconfirmed_directory = None,
        DirectoryDurability::Uncertain(detail) => {
            let projection =
                qualified_by_unconfirmed_directory(health.projection.clone(), Some(&detail));
            if health.projection != projection {
                health.since = now_ts();
                health.projection = projection;
            }
            health.unconfirmed_directory = Some(detail);
        }
    }
}

/// Records what a reconciliation attempt observed. `since` moves only
/// when the state actually changes, so a window that has been open for
/// twenty attempts still reports when it opened rather than when it was
/// last re-confirmed.
///
/// `durability` is what the attempt established about the atlas
/// directory itself, which the projection cannot carry on its own: a
/// projection is recomputed from the estate every time, and an
/// unconfirmed directory entry is invisible to every later read of it.
///
/// **Two facts on two clocks, and only one of them is the walk's.** The
/// projection is what this attempt's *walk* saw, and a walk is ordered
/// by the observation ticket taken before it — that is what the
/// staleness guard below is for. The directory fact is what this
/// attempt's *own* `fsync` of the atlas directory returned, and an
/// `fsync` is ordered by the Atlas critical section it was made in.
/// Every production caller of this function holds the Atlas guard
/// across the call (the sweep, and all five arms of `--rebuild`), so by
/// the time it is reached the directory fact is already the latest one
/// there is, whatever the ticket says.
///
/// So the directory fact is applied **first and unconditionally**,
/// through the same `apply_directory_durability` the retirement's own
/// record uses, and only the projection is then subject to the guard.
/// Before this, a stale walk's genuinely later failure was discarded
/// with its projection and never reached `unconfirmed_directory` at
/// all: the assignment sat below the `return`. A walk with an older
/// ticket writes and `fsync`s exactly when it holds a row a newer,
/// shorter walk never read, so this was reachable by ordinary
/// mutations with no administrative call anywhere near it — executed
/// against a real daemon and a real `EIO` at
/// `index-retirement-order-verify/raw/61`, where the next no-op sweep
/// then certified `synchronized` / `complete: true` over an entry whose
/// `fsync` had returned `EIO`.
///
/// Both directions are kept, and they are the same two the retirement's
/// record already keeps:
///
/// * A genuinely later **failure** opens the window immediately,
///   re-qualifying the standing projection here and now, whether or not
///   the walk that made it is stale.
/// * A genuinely later **success** clears the window, leaving the
///   projection to the walk that re-derives it — which is the ordinary
///   sweep that follows, never this stale record.
/// * A stale **projection** still cannot overwrite a newer one: the
///   guard *expression* is untouched, and `weakens` still admits only
///   the one direction it ever admitted.
///
///   Said precisely, because the expression is not the whole of it
///   (`index-sync-and-live-sweep-verify/VERDICT.md`, finding S1):
///   `apply_directory_durability` runs **before** the guard and can
///   move `health.projection`, which is one of the guard's own
///   operands, so the comparison can be made against a different
///   standing projection than it would have been. Exactly one case
///   diverges — a stale observation, `Uncertain` durability, and a
///   standing projection of `Synchronized`: `apply` has already moved
///   the standing projection to `DurabilityUnconfirmed`, so `weakens`
///   is now false where it used to be true and this function returns
///   early instead of writing the record in full.
///
///   The direction is safe and the contract is not weakened: every
///   `Uncertain` outcome of `apply` leaves a non-`Synchronized`
///   projection, so `complete` cannot become `true` on this path in
///   either version (ruling 0125). What the early return costs is
///   narrow and transient — `health.preserved` and `last_attempt` are
///   not refreshed, and the state is reported as
///   `durability_unconfirmed` rather than `behind` with the preserved
///   note folded in, and only when a preserved copy appeared between
///   the newer walk's record and this one. The reconciliation that
///   always follows restores it. Arguably it is the better behaviour:
///   a walk judged stale no longer claims `last_attempt` or
///   re-publishes its own `preserved` listing.
/// * An attempt that wrote nothing (`Unestablished`) still establishes
///   nothing: it re-reads the standing field and carries it forward,
///   exactly as before, so a no-op sweep cannot resolve a window it
///   never touched (ruling 0130).
///
/// **And a third fact, on the same principle: the index file itself.**
/// `established` is what this attempt's own critical section saw of the
/// file — its append's read, or its own publication of one — and
/// `RecordedBacking::Unknown` is a caller that established nothing and
/// leaves the question to the listing, exactly as every caller did
/// before. The listing below is still taken, still on the path that is
/// already writing the directory, and still answers the preserved-copy
/// question; what it is no longer allowed to do is contradict an
/// observation this attempt actually made (`recorded_backing`).
fn record_index_projection(
    state: &Arc<WirkdState>,
    observation: u64,
    projection: IndexProjection,
    durability: DirectoryDurability,
    established: RecordedBacking,
) {
    let at = now_ts();
    // Read from the estate on every attempt rather than remembered in
    // this process, so it survives a restart and so an administrator who
    // retires the preserved bytes is believed by the very next
    // reconciliation without a second mechanism.
    let (preserved, listed) = atlas_listing(state);
    let backing = recorded_backing(established, listed);
    let mut health = state
        .index_health
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // The directory fact this attempt's own `fsync` established, applied
    // in the critical section that made it and before anything is judged
    // stale — the whole argument is on `record_index_projection`'s and
    // `record_directory_durability`'s docs. `Unestablished` is a no-op,
    // so an attempt that wrote nothing still reads the standing field
    // below rather than replacing it.
    apply_directory_durability(&mut health, durability);
    // Carried under the same lock as the record it qualifies, and
    // *before* the preserved qualification, so an unconfirmed directory
    // that a preserved copy then folds into `behind` is said once rather
    // than twice. Read back off the record rather than recomputed, so
    // this is the fact as it now stands: this attempt's own, when it
    // established one, and the standing one when it did not.
    let unconfirmed_directory = health.unconfirmed_directory.clone();
    let projection =
        qualified_by_unconfirmed_directory(projection, unconfirmed_directory.as_deref());
    let projection = qualified_by_preserved(projection, &preserved);
    if observation < health.observation && !weakens(&projection, &health.projection) {
        // An observation older than the standing one is discarded whole:
        // its projection, its `since` and its `last_attempt` alike. It
        // saw an older estate than the record already here, so it has
        // nothing to add — and the one thing it must never do is what
        // the reverify review executed, which is hand a caller
        // `synchronized` over a failure a newer sweep already found,
        // reported and did not repair.
        //
        // Its *directory* fact is not discarded with it and is already
        // applied above: that fact is not something the walk saw, it is
        // something this critical section did.
        return;
    }
    if health.projection != projection {
        health.since = at;
        health.projection = projection;
    }
    health.preserved = preserved;
    // Belongs to the observation whose record was kept, exactly as
    // `preserved` does — an attempt discarded above contributes neither.
    health.index_backing = backing;
    health.last_attempt = Some(at);
    health.observation = std::cmp::max(health.observation, observation);
}

/// Why an index that holds every journaled row is still not a
/// synchronized index when a directory entry behind it was never
/// confirmed on disk.
///
/// The same shape as `qualified_by_preserved` and for the same reason:
/// a fact the walk cannot see, applied to the walk's own finding rather
/// than folded into it. `Synchronized` becomes the state the product
/// already has for exactly this window — visible now, not confirmed on
/// disk — and every other state already says something at least as
/// serious, so only the detail is added.
///
/// It is deliberately **not** cleared by "the next reconciliation":
/// a sweep that finds nothing missing writes nothing at all
/// (`append_finding_rows` returns `Ok(0)` without a byte), so it opens
/// no file, `fsync`s no directory, and has learned exactly nothing
/// about the entry it would be certifying. What clears it is a real
/// successful `fsync` of the atlas directory — see `DirectoryDurability`.
fn qualified_by_unconfirmed_directory(
    projection: IndexProjection,
    unconfirmed: Option<&str>,
) -> IndexProjection {
    let Some(detail) = unconfirmed else {
        return projection;
    };
    match projection {
        // Nothing has checked the index at all; that is already not
        // complete, and claiming an observation this process has not
        // made is the one thing `Unreconciled` exists to avoid.
        IndexProjection::Unreconciled => IndexProjection::Unreconciled,
        IndexProjection::Synchronized => IndexProjection::DurabilityUnconfirmed {
            detail: detail.to_string(),
        },
        // Already saying this, with this attempt's own words for it.
        durability @ IndexProjection::DurabilityUnconfirmed { .. } => durability,
        IndexProjection::Behind {
            pending,
            detail: behind,
        } => IndexProjection::Behind {
            pending,
            detail: format!("{behind}; {detail}"),
        },
    }
}

/// The one backing fact this observation records, from the two halves of
/// it that looked at the file: what the attempt's own critical section
/// established, and what the listing taken at the end of it saw.
///
/// They are two observations of the same file at two different moments,
/// and the later one is not the more authoritative one. The listing runs
/// after `append_finding_rows` has returned — the product's own
/// `findings-index-published` checkpoint is that window — so an external
/// deletion inside it makes the listing report `Absent` about a file the
/// append had just read or just written. Recorded, that `Absent` is
/// indistinguishable from the pre-publication estate, and a later read
/// of the missing file answers `complete: true` with no rows while the
/// journals still hold the row (ruling 0137, and the seam
/// `loop-c3-index-read-verify/VERDICT.md` §4 executed).
///
/// So `Present` is the fact that decides it, from whichever half saw it:
/// there *was* a file, and a read that later finds none cannot call its
/// emptiness known. `Absent` is recorded only when nothing this attempt
/// looked at held one — which is the estate that has never published a
/// row, whose empty projection really is complete and must go on saying
/// so. Anything else stays `Unknown`, the state that is evidence in
/// neither direction: a listing that failed, or a caller (the
/// `--rebuild` arms, whose behaviour is unchanged here) that established
/// nothing of its own and leaves the answer to the listing.
///
/// Nothing is remembered between calls and nothing is latched: this is
/// one attempt's two observations, resolved once, and the next
/// reconciliation records its own.
fn recorded_backing(established: RecordedBacking, listed: RecordedBacking) -> RecordedBacking {
    match (established, listed) {
        (RecordedBacking::Present, _) | (_, RecordedBacking::Present) => RecordedBacking::Present,
        (RecordedBacking::Absent, RecordedBacking::Absent) => RecordedBacking::Absent,
        // `established` is `Unknown` (nothing established, so the
        // listing decides, including its own `Absent`), or the listing
        // failed over an attempt that found no file — unknown either
        // way, and never read as a known empty.
        (RecordedBacking::Unknown, listed) => listed,
        (RecordedBacking::Absent, RecordedBacking::Unknown) => RecordedBacking::Unknown,
    }
}

/// What one listing of `atlas/` establishes for the health record: the
/// preserved copies this estate is currently holding, and whether the
/// standing index file was there when this observation looked.
///
/// One directory listing of `atlas/`, taken on the reconciliation path
/// that is already writing that directory — no new store, no registry,
/// no process-global state and nothing to keep in sync. A listing that
/// **fails** is itself a reason not to certify the index: an estate
/// whose atlas directory cannot be listed has not been checked, so the
/// error is carried as the finding it is rather than flattened to "none
/// preserved" — and carried *as an error*, in its own field, rather than
/// as an invented file name in the list of names.
fn atlas_listing(state: &Arc<WirkdState>) -> (PreservedIndexCopies, RecordedBacking) {
    match wirk_atlas::atlas_directory_listing(&state.estate_root) {
        Ok(listing) => (
            PreservedIndexCopies {
                names: listing.preserved,
                unknown: None,
            },
            if listing.index_present {
                RecordedBacking::Present
            } else {
                RecordedBacking::Absent
            },
        ),
        // The one listing answered neither question, so neither is
        // claimed: the preserved half is carried as the error it is, and
        // the backing half stays `Unknown` rather than being read as
        // "there was no index file".
        Err(error) => (
            PreservedIndexCopies {
                names: Vec::new(),
                unknown: Some(error.to_string()),
            },
            RecordedBacking::Unknown,
        ),
    }
}

/// Why an index can be a complete projection of every journal the walk
/// read and still not be a complete projection of the estate.
///
/// `--rebuild` is allowed to proceed over a standing index whose lines
/// do not all parse — it is the documented repair for exactly that file
/// — but the lines that did not parse could have held anything,
/// including rows for a Work the walk no longer reaches. Their bytes are
/// preserved instead of replaced away, and while they are held *nothing
/// automatic* may report this projection complete: the rebuild's own
/// output is the basis every later sweep compares against, so a later
/// sweep is comparing the shortened file with itself and learns nothing
/// about what was lost (ruling 0130 — "subsequent mutation with a
/// shortened now-readable basis must not wash away known loss
/// automatically").
///
/// So this is deliberately *sticky*, and deliberately not a latch that
/// only a clock or a redesign can open: it is a fact about a file in the
/// estate, and it stops being true when an administrator reviews those
/// bytes and retires them.
fn qualified_by_preserved(
    projection: IndexProjection,
    preserved: &PreservedIndexCopies,
) -> IndexProjection {
    if !preserved.qualifies() {
        return projection;
    }
    qualified_by(projection, &preserved.note())
}

/// A fact the walk could not see, applied to the walk's own finding
/// rather than folded into it — the one shape every such qualification
/// takes, held in one place so two of them cannot drift apart.
///
/// The count always goes to unknown, because that is what a fact from
/// outside the walk leaves it at: a durability window reports `0` pending
/// because every offered row is visible, and that says nothing about rows
/// nobody could parse or about a file nobody could open.
fn qualified_by(projection: IndexProjection, note: &str) -> IndexProjection {
    match projection {
        // Nothing has checked at all: already not complete, and saying
        // `behind` would claim an observation this process has not made.
        IndexProjection::Unreconciled => IndexProjection::Unreconciled,
        IndexProjection::Synchronized => IndexProjection::Behind {
            pending: None,
            detail: note.to_string(),
        },
        // Both facts, and the more serious one decides the state.
        IndexProjection::DurabilityUnconfirmed { detail } => IndexProjection::Behind {
            pending: None,
            detail: format!("{note}; {detail}"),
        },
        IndexProjection::Behind { detail, .. } => IndexProjection::Behind {
            pending: None,
            detail: format!("{note}; {detail}"),
        },
    }
}

/// The sentence a read may say about its own backing file, and the one
/// state that earns it.
///
/// It says "observed", and not "listed", because the record's
/// `Present` has three sources and only one of them is a listing: the
/// append's own read of the file, the rename a successful write or
/// rebuild performed, and — when the attempt established nothing of its
/// own — the listing of `atlas/` taken inside `record_index_projection`
/// (`recorded_backing`). Naming a listing that may never have seen the
/// file, and in the deletion window this note exists for typically did
/// not, would put a fact in front of an administrator that this daemon
/// never observed.
const ABSENT_INDEX_NOTE: &str = "the standing findings index file was not there when these rows \
     were read, and the reconciliation this health record came from observed one that was: what \
     that file held is not established by this read, so the row list beside this health is not a \
     complete projection of this estate's journals";

/// Ruling 0137. **A read qualifies its own answer from the backing state
/// it just observed**, and from nothing else.
///
/// `read` is what this call's own open of the index found; `recorded` is
/// what the observation behind `health` saw of the same file. Exactly one
/// pair is a finding: this read had no file to open, and the record it is
/// about to be paired with was formed over one that was there. Then the
/// rows are not a subset that was measured, they are the absence of a
/// measurement, and `complete` may not describe them.
///
/// Every other pair is left alone, deliberately:
///
/// - **Absent now, absent then** is an estate that never wrote an index,
///   whose empty projection of an empty estate really is complete
///   (`a_legally_empty_estate_and_irrelevant_entries_are_a_complete_observation`).
///   Reading absence as loss here would invent a lost row out of a
///   healthy pre-publication estate.
/// - **Absent now, unknown then** is a listing that failed. That already
///   stops this estate certifying its projection, through
///   `qualified_by_preserved`, and it is not evidence about the index
///   file either way.
/// - **Present now** is an ordinary read, whose rows are the file's.
///
/// What this does **not** do: re-scan a journal, write anything, mint or
/// alter a canonical record, keep a store, or touch the recorded health
/// this daemon holds. It qualifies the copy being rendered, from an
/// observation this very read made, and the next real reconciliation —
/// which recreates the file additively from the journals — clears it
/// without anything here being latched.
fn qualified_by_absent_index(
    mut health: IndexHealth,
    read: wirk_atlas::IndexBacking,
) -> IndexHealth {
    if read != wirk_atlas::IndexBacking::Absent || health.index_backing != RecordedBacking::Present
    {
        return health;
    }
    let projection = qualified_by(health.projection.clone(), ABSENT_INDEX_NOTE);
    // `since` on the copy this read renders is **this read's own
    // observation of the absence**, and not when the absence began.
    // Nothing here is persisted, so each read re-derives it and three
    // consecutive reads of the same missing file report three different
    // values; an administrator cannot read how long the window has stood
    // off it. That is the honest limit of a qualification made from one
    // read, and the alternative — remembering when a read first saw the
    // file gone — is the new store, latch and cross-read state ruling
    // 0137 refused. The recorded `since` underneath is untouched and
    // returns intact when the file does.
    if projection != health.projection {
        health.since = now_ts();
        health.projection = projection;
    }
    health
}

/// Whether an older observation is allowed to land anyway, and it is
/// allowed exactly one direction: **an older walk may make the standing
/// projection less complete, never more.**
///
/// The rule this enforces is a safety rule, not a recency rule. What
/// must never reach a caller is `complete: true` over an index that is
/// demonstrably missing a durably journaled row, so an older attempt
/// claiming completeness is dropped. The mirror is not symmetrical: an
/// older attempt's *failure* is still a fact about a write this daemon
/// really just tried and a read it really just made, so it is allowed to
/// replace a newer success, and the estate errs toward "incomplete,
/// unknown" — the same direction ruling 0125 chose for a partial scan.
///
/// A conservative `Behind` recorded that way is cleared by exactly what
/// the surface's own `recovery` sentence already names: the **next**
/// observation, whose ticket is newer than everything parked behind it —
/// the next successful settle/assert/apply sweep, a daemon restart, or
/// an administrative `--rebuild`. It is never latched and never waits on
/// a clock.
fn weakens(older: &IndexProjection, standing: &IndexProjection) -> bool {
    matches!(standing, IndexProjection::Synchronized)
        && !matches!(older, IndexProjection::Synchronized)
}

/// The projection-health block every mutating verb and every dependent
/// query surface carries.
///
/// `complete` is the field a caller must read: `false` means this
/// estate's journals hold Finding records the derived index does not,
/// so a `rows` list beside it is a **subset**, not an answer. The
/// journal remains the record either way, which is why the mutation that
/// produced this reply still succeeded.
///
/// **Scoping.** A scoped requester is told the projection's *state* and
/// nothing about its contents: no count, no row or finding id, no error
/// text, no path. The rows it cannot see are estate-local Findings of
/// other Works, and how many of them are missing is as much theirs as
/// their claims are — an incompleteness signal is health metadata, and
/// health metadata is not a side channel onto the estate. `--admin`
/// already reads every row in the file, so it also gets the count, the
/// underlying error and the timings it needs to repair the thing.
fn index_health_json(health: &IndexHealth, admin: bool, paired_with_rows: bool) -> Value {
    let projection = match &health.projection {
        IndexProjection::Unreconciled => "unreconciled",
        IndexProjection::Synchronized => "synchronized",
        IndexProjection::DurabilityUnconfirmed { .. } => "durability_unconfirmed",
        IndexProjection::Behind { .. } => "behind",
    };
    let mut block = json!({
        "projection": projection,
        "complete": health.complete(),
        // Said on every reply, complete or not, because the thing a
        // reader most needs to know about this index is that it is never
        // the record.
        "canonical": "the raising Work's journal is the record; this index is a derived projection of it",
        // What this block describes, exactly. The first form is the one
        // ruling 0125 case 2A required: when a reply carries `rows`,
        // this health and those rows are one snapshot taken together
        // under the index's own lock, so `complete` is a statement about
        // *these* rows and not about some later moment. The second is
        // for a reply that carries no rows at all — a mutating verb's.
        // It says which *record* it renders and not which attempt
        // produced it: this call's sweep is always offered to the
        // record, but `weakens` deliberately lets an older attempt's
        // failure stand over a newer success, so the projection a
        // caller reads here is not always its own sweep's. The ticket
        // proves the offer, never the provenance of what is rendered
        // (`index-health-order-verify/VERDICT.md`), and claiming
        // otherwise was an overstatement on the one surface where the
        // asymmetry is visible. The direction is the safe one and the
        // next complete reconciliation clears it.
        "observed": if paired_with_rows {
            "this projection and the rows beside it were read together as one snapshot, so `complete` describes this list; reading the index does not re-scan the estate"
        } else {
            "this daemon's own most recently recorded reconciliation outcome, which this call's own sweep was offered to; an older attempt's failure is deliberately allowed to stand over a newer success, so `behind` here may have been observed before this call and is cleared by the next complete reconciliation; reading the index does not re-scan the estate"
        },
    });
    let map = block
        .as_object_mut()
        .expect("the literal above is an object");
    if !health.complete() {
        map.insert(
            "recovery".to_string(),
            Value::String(
                // The preserved case first, because the sentence below
                // it would be a lie there: a later reconciliation does
                // **not** re-project what nobody could parse, and
                // telling an operator to wait for one is telling them
                // to wait for something that will never arrive. No
                // name, no count and no path here — this string is read
                // by a scoped requester too, and what it must convey is
                // that the wait is on a person, not on a sweep.
                if !health.preserved.names.is_empty() {
                    "bytes of an earlier index that could not be parsed are preserved in this estate rather than dropped; no reconciliation, restart or rebuild can establish what they held, so this projection stays incomplete until an administrator reviews the preserved copy and retires it"
                } else if health.preserved.unknown.is_some() {
                    // Not the sentence above: no copy is known to be
                    // held, and telling a caller that bytes are
                    // preserved — or telling them to go and retire
                    // something — would be inventing both the file and
                    // the instruction. What is true is that the
                    // question could not be answered, which no sweep
                    // can change on its own. No path, no count and no
                    // name here either: a scoped requester reads this
                    // string too.
                    "whether bytes of an earlier index that could not be parsed are preserved in this estate could not be established, so no reconciliation, restart or rebuild can decide this projection's completeness until an administrator determines it"
                } else {
                    match health.projection {
                        IndexProjection::DurabilityUnconfirmed { .. } => "the rows are visible to a reader now; a machine that loses power before the next successful write may need `wirk atlas findings --admin --rebuild`",
                        _ => "the next successful settle/assert/apply reconciliation, a daemon restart, or `wirk atlas findings --admin --rebuild` re-projects every journaled row; no record is lost meanwhile",
                    }
                }
                .to_string(),
            ),
        );
    }
    if admin {
        map.insert("since".to_string(), Value::from(health.since.0));
        map.insert(
            "last_reconciled".to_string(),
            match health.last_attempt {
                Some(at) => Value::from(at.0),
                None => Value::Null,
            },
        );
        let (pending, detail) = match &health.projection {
            IndexProjection::Behind { pending, detail } => (
                match pending {
                    Some(pending) => Value::from(*pending),
                    // Unknown, not zero: the index could not be read.
                    None => Value::Null,
                },
                Some(detail.clone()),
            ),
            IndexProjection::DurabilityUnconfirmed { detail } => {
                (Value::from(0_u64), Some(detail.clone()))
            }
            IndexProjection::Synchronized => (Value::from(0_u64), None),
            IndexProjection::Unreconciled => (Value::Null, None),
        };
        map.insert("pending_rows".to_string(), pending);
        map.insert(
            "detail".to_string(),
            detail.map(Value::String).unwrap_or(Value::Null),
        );
        // Admin-only on exactly the terms `detail` is: these are file
        // names in the estate's own atlas directory, and a scoped
        // requester learns the projection's state and nothing about the
        // estate's contents.
        map.insert(
            "preserved_index_copies".to_string(),
            Value::Array(
                health
                    .preserved
                    .names
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
        // Beside the list, never inside it. A listing that failed named
        // no file, so `preserved_index_copies` stays empty and this
        // carries the cause the administrator can act on — the one
        // thing they need and the one thing a scoped requester still
        // does not get.
        map.insert(
            "preserved_index_copies_unknown".to_string(),
            match &health.preserved.unknown {
                Some(unknown) => Value::String(unknown.clone()),
                None => Value::Null,
            },
        );
    }
    block
}

/// The health this daemon has currently recorded, as one value.
///
/// A *snapshot*, not a live read, because a caller that carries index
/// rows has to render the health that describes those rows and not
/// whatever the number happened to be by the time the reply was built.
/// The disclosure work between the two in `handle_atlas_findings` is
/// long — a full journal replay, a lineage walk and a per-row scoping
/// pass — and a repair landing inside it produced a reply whose rows
/// were a strict subset while its own `complete` said `true` (ruling
/// 0125, case 2A executed).
fn index_health_snapshot(state: &Arc<WirkdState>) -> IndexHealth {
    state
        .index_health
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

/// Attaches `index_health_json` to a reply object under `index`.
/// A reply that is not an object is returned untouched rather than
/// wrapped — no verb this is called from produces one, and silently
/// changing a reply's shape to carry health would be a worse bug than
/// the one this closes.
///
/// For a reply that carries no index rows: it reads the health now,
/// which for a mutating verb is at or after its own sweep. A reply that
/// *does* carry rows must use `with_paired_index_health` instead.
fn with_index_health(state: &Arc<WirkdState>, admin: bool, mut result: Value) -> Value {
    if let Value::Object(map) = &mut result {
        let health = index_health_snapshot(state);
        map.insert(
            "index".to_string(),
            index_health_json(&health, admin, false),
        );
    }
    result
}

/// The same, for a reply whose `rows` were captured together with
/// `health` under one hold of the Atlas mutex: it renders the captured
/// value rather than re-reading a later one.
fn with_paired_index_health(health: &IndexHealth, admin: bool, mut result: Value) -> Value {
    if let Value::Object(map) = &mut result {
        map.insert("index".to_string(), index_health_json(health, admin, true));
    }
    result
}

/// The index's rows and the health that describes **those rows**, read
/// as one snapshot under the mutex that orders every publication and its
/// own health record (ruling 0125 case 2A, ruling 0137's paired
/// qualification).
///
/// Extracted rather than duplicated: `handle_atlas_findings` and the
/// stage assembler must not be able to disagree about what one
/// requester's index read was, and a second copy of this policy is
/// exactly how they would. The Atlas guard is taken here and dropped
/// here — every caller does its disclosure work outside it, and nothing
/// called from inside takes the Atlas lock again.
fn read_findings_with_health(
    state: &Arc<WirkdState>,
) -> Result<(Vec<wirk_atlas::FindingRow>, IndexHealth), wirk_atlas::AtlasError> {
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let read = atlas.read_findings()?;
    // The health this read renders is qualified by the backing state
    // this very read observed, inside the same hold of the mutex that
    // took the rows (ruling 0137). Nothing recorded is changed:
    // `index_health` still holds this daemon's own last reconciliation
    // outcome, and what is qualified is the copy that is about to be
    // paired with these rows and called complete.
    let health = qualified_by_absent_index(index_health_snapshot(state), read.backing);
    Ok((read.rows, health))
}

fn finding_row_json(row: &wirk_atlas::FindingRow) -> Value {
    json!({
        "id": row.id.0,
        "kind": match row.kind {
            wirk_atlas::FindingRowKind::Settled => "settled",
            wirk_atlas::FindingRowKind::Asserted => "asserted",
            wirk_atlas::FindingRowKind::Applied => "applied",
        },
        "finding": {
            "id": row.finding.id.0,
            "kind": finding_kind_str(row.finding.kind),
            "scope": finding_scope_str(row.finding.scope),
            // Same honesty as `finding_json`: the estate index is
            // exactly where a settled row's free sentence used to read
            // as the settled thing (the reproduced counterexample's own
            // last step). `settlement.proves` carries what the check
            // proves; this carries what was recorded.
            "claim": format!("recorded claim: {}, unverified", row.finding.claim),
            "claim_text": row.finding.claim,
            "claim_verified": false,
        },
        "origin": {
            "work": row.origin.work.0,
            "raised_event": row.origin.raised_event.0,
            "row_event": row.origin.row_event.0,
        },
        "settlement": row.settlement.as_ref().map(settlement_json),
        "assertion": row.assertion.as_ref().map(assertion_json),
        "applied": row.applied.as_ref().map(application_ref_json),
        "superseded_by": row.superseded_by.as_ref().map(|id| id.0.clone()),
    })
}

/// The estate Findings index, answered one of two explicitly named
/// ways. There is no unnamed default: this index is a derived
/// *disclosure* surface — a settled row carries its proof targets'
/// exact membership/generation/object identities and its artifact paths,
/// an applied row carries the changed source's alias, both generation
/// points and the published revision — and the base returned all of it
/// to any caller that reached the estate root, with no requester on the
/// wire at all.
///
/// Administration is preserved, not removed: it is now *named*
/// (`--admin`), which is honest about what it is. It remains
/// attribution, never authentication — the same OS uid runs an
/// operator's terminal and an actor's shell, and this daemon does not
/// pretend otherwise (`PeerIdentity`'s own doc). `--rebuild` rewrites
/// the index from every journal in the estate and is administrative on
/// its own account.
fn handle_atlas_findings(state: &Arc<WirkdState>, payload: super::AtlasFindingsPayload) -> Reply {
    if payload.admin == payload.requester.is_some() {
        return err_reply(
            "BadRequest",
            "the findings index answers a requesting work or an explicit administrative call, and needs exactly one of them named",
        );
    }
    if payload.rebuild && !payload.admin {
        return err_reply(
            "BadRequest",
            "rebuilding the index from every journal in the estate is an administrative call",
        );
    }
    if payload.retire_preserved && !payload.admin {
        return err_reply(
            "BadRequest",
            "retiring preserved copies of an unreadable index is an administrative call",
        );
    }
    if payload.retire_preserved && payload.rebuild {
        // Separate acts, deliberately not combinable. A rebuild that
        // proceeds over unparsable lines *creates* the preserved copy
        // and the unknown it stands for; retiring in the same call would
        // clear the signal in the call that raised it, which is the
        // automatic laundering ruling 0130 refused.
        return err_reply(
            "BadRequest",
            "rebuilding the index and retiring preserved copies of an earlier one are separate administrative acts; a rebuild that preserves bytes is the reason to review them, not a review of them",
        );
    }
    // Retirement is the one thing that clears the residual uncertainty a
    // preserved copy stands for, and it is deliberately a person saying
    // so. It **renames**; no byte of a preserved copy is removed here or
    // anywhere else in this product.
    let mut retired = Vec::new();
    if payload.retire_preserved {
        let outcome = {
            let atlas = state
                .atlas
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let outcome = atlas.retire_preserved_unreadable_indexes();
            // What this retirement established about the atlas
            // directory, recorded **here**, under the guard its renames
            // and their `fsync` were made under. A retirement that
            // renamed at least one copy and returned `Ok` `fsync`ed that
            // directory successfully, which confirms every entry pending
            // in it — including one an earlier call could not confirm.
            // One that renamed nothing touched nothing.
            //
            // It used to be handed to the sweep below to publish, which
            // put a fact made in *this* critical section under a ticket
            // taken after the guard was dropped — so a concurrent
            // mutation whose own `fsync` failed in between was overwritten
            // by this one's older success. The sync order is the order of
            // these critical sections and of nothing else, which is why
            // this record belongs inside this one:
            // `record_directory_durability` has the whole argument.
            record_directory_durability(
                state,
                match &outcome {
                    Ok(pairs) if !pairs.is_empty() => DirectoryDurability::Confirmed,
                    Ok(_) => DirectoryDurability::Unestablished,
                    Err(failed) => retirement_durability(failed),
                },
            );
            outcome
        };
        match outcome {
            Ok(pairs) => {
                retired = pairs
                    .into_iter()
                    .map(|(from, to)| json!({ "preserved": from, "retired": to }))
                    .collect();
            }
            Err(failed) => {
                // A retirement that stopped part-way still changed the
                // estate, and the record every later caller reads must
                // describe the estate as it now is rather than as it
                // was before this call — the failing path returning
                // without re-observing is what left the health naming a
                // file that had already been renamed away
                // (`index-basis-recovery-verify/raw/48`). Same sweep,
                // same measurement, same lock order as the success path
                // below: the Atlas guard is dropped with the block
                // above.
                //
                // **And the durability of what it did rename is
                // already recorded**, above, in the critical section
                // that renamed. This is the defect the bounded
                // verification executed
                // (`index-recovery-report-verify/raw/45`): the renames
                // landed, the atlas directory's `fsync` returned `EIO`,
                // the caller was told so — and this very sweep then
                // recorded `Synchronized`, because with the last
                // preserved name now retired there was nothing left for
                // it to qualify and an append with nothing to append
                // writes nothing. The one caller who ran the verb held
                // the only copy of the fact. The sweep here re-observes
                // the estate this call changed and carries the standing
                // window forward; it does not publish this call's
                // directory fact for it.
                reconcile_findings_index(state);
                return err_reply("AtlasError", &failed.to_string());
            }
        }
        // Re-observe through the estate's own sweep rather than editing
        // the health record from here: the projection this call changed
        // is the projection a reconciliation measures, and there is only
        // one thing in this daemon that measures it. The directory fact
        // is not its to carry — that was recorded above, where it
        // happened.
        reconcile_findings_index(state);
    }
    if payload.rebuild {
        // The walk moves **inside** the critical section on this path
        // and only on this path (ruling 0125, case 2B executed).
        // `rebuild_finding_rows` is an unconditional whole-file
        // replacement, so a row that lands between the walk and the
        // replacement is not merely missed — it is deleted, after being
        // durably journaled *and* successfully indexed, and the reply
        // that deleted it said `synchronized`. Holding the mutex across
        // the walk is what a whole-file replacement warrants and it
        // introduces no new wait: the walk opens journal files through
        // its own handles and takes no lock of any kind, so it cannot
        // wait on anything a lock holder holds. The ordinary sweep keeps
        // its walk outside the lock, where the same window is benign
        // because the append re-reads under the lock and only adds.
        let mut atlas = state
            .atlas
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // This path's walk lives **inside** the hold, so its ticket is
        // taken here rather than before the lock: a rebuild observes the
        // estate at this instant, and is newer than every sweep already
        // parked between its own walk and this mutex.
        let observation = next_index_observation(state);
        // The same pre-walk basis the sweep uses, read here under the
        // hold this replacement already takes — which is also why no
        // row can land between this read and the walk below.
        //
        // An index that cannot be *read* is not evidence of anything,
        // and rebuilding from journals is that file's own documented
        // repair (`findings.rs`: a malformed row blocks reads until
        // `rebuild_finding_rows` recreates it). So that case is said out
        // loud on the operator's stderr and the rebuild proceeds, rather
        // than the one repair path refusing to repair.
        let held_before_walk = index_basis(&atlas);
        let mut scan = all_finding_rows(state);
        // Rows the index durably held that this walk did not reproduce
        // make it a partial observation, whatever the filesystem said
        // while it was walking. Without this, a Work whose journal is
        // absent — moved aside, its directory gone, or re-created empty
        // by some other startup path — raises no error anywhere, and the
        // replacement deletes its published rows at exit 0.
        // `None` — nothing at all could be established about the
        // standing index — accounts nothing, and the refusal below is
        // then unconditional. Deliberately not treated as an empty
        // basis: an index that could not be opened is not an index known
        // to hold no rows, and treating it as one is how a replacement
        // deletes evidence it never read.
        if let Some(held) = held_before_walk.known_rows() {
            scan.account(&wirk_atlas::unaccounted_finding_rows(held, &scan.rows));
        }
        // The window a verifier parks at to prove the walk and the
        // replacement are one operation.
        wirk_atlas::checkpoint("findings-index-walked");
        // An index nobody could read at all is not a basis, and this
        // path is destructive. Refuse, leaving the file exactly as it
        // is — the same treatment a partial walk already gets, for the
        // same reason.
        if let IndexBasis::Unreadable {
            read_error,
            open_error,
        } = &held_before_walk
        {
            let detail = format!(
                "the standing index could not be read at all, so this rebuild's walk could not be checked against what the index already holds and the index was left as it is: {read_error}; {open_error}"
            );
            eprintln!("wirkd: findings index rebuild refused: {detail}");
            // Refused before a byte was written: this rebuild opened
            // no file and `fsync`ed no directory, so it establishes
            // nothing about one an earlier call left unconfirmed.
            record_index_projection(
                state,
                observation,
                IndexProjection::Behind {
                    pending: None,
                    detail: detail.clone(),
                },
                DirectoryDurability::Unestablished,
                RecordedBacking::Unknown,
            );
            drop(atlas);
            return err_reply("IndexBasisUnreadable", &detail);
        }
        // A partial walk is a refusal, never an overwrite (ruling 0125,
        // case 1). The rows this walk could not read are still in the
        // index from a healthier sweep; replacing the file with a walk
        // that is known to be short deletes them and calls it a rebuild.
        // The existing index is left exactly as it is, the health says
        // the projection is behind by an unknown amount, and the
        // administrator gets a non-zero exit and a reason to act on —
        // which is the same treatment a failed *write* already gets.
        if !scan.complete() {
            let mut detail = scan.detail();
            if let Some(note) = held_before_walk.salvage_note() {
                detail = format!("{detail}; {note}");
            }
            // The way out, said in the refusal itself, because a repair
            // that can only ever refuse is not a repair. Restoring the
            // canonical journals named above is the real fix. When that
            // history is genuinely gone, the standing index's rows are
            // the last remaining trace of it, so the escape is to
            // **keep** them under a name this estate recognises — never
            // to delete the file, which is the one instruction that
            // would destroy the only evidence left.
            detail = format!(
                "{detail}; restore the canonical journal(s) named above and run this again — or, if that history is genuinely unrecoverable, keep the standing index's evidence by moving `atlas/{}` to `atlas/{}<a name of your choosing>` and run this again, after which this estate reports its projection incomplete until `wirk atlas findings --admin --retire-preserved-index`",
                wirk_atlas::FINDINGS_INDEX_FILE,
                wirk_atlas::PRESERVED_INDEX_PREFIX,
            );
            eprintln!("wirkd: findings index rebuild refused: {detail}");
            record_index_projection(
                state,
                observation,
                IndexProjection::Behind {
                    pending: None,
                    detail: detail.clone(),
                },
                DirectoryDurability::Unestablished,
                RecordedBacking::Unknown,
            );
            drop(atlas);
            return err_reply(
                "IndexScanIncomplete",
                &format!(
                    "the index was left as it is rather than replaced from a partial walk of the estate: {detail}"
                ),
            );
        }
        // The walk accounts for every row that could be established —
        // but not for the lines that could not be parsed, whose content
        // is unknowable and may have named a Work this walk no longer
        // reaches. Their bytes are the only remaining trace of them, so
        // they are copied aside *before* the replacement, and a
        // preservation that fails stops the replacement rather than
        // being logged past: destroying the last copy of unreadable
        // evidence is the failure this whole path exists to prevent.
        if held_before_walk.salvage_note().is_some() {
            match atlas.preserve_unreadable_index() {
                Ok(_) => {}
                Err(error) => {
                    let detail = format!(
                        "the standing index's unparsable bytes could not be preserved, so it was left as it is rather than replaced: {error}"
                    );
                    eprintln!("wirkd: findings index rebuild refused: {detail}");
                    record_index_projection(
                        state,
                        observation,
                        IndexProjection::Behind {
                            pending: None,
                            detail: detail.clone(),
                        },
                        DirectoryDurability::Unestablished,
                        RecordedBacking::Unknown,
                    );
                    drop(atlas);
                    return err_reply("AtlasError", &detail);
                }
            }
        }
        let outcome = atlas.rebuild_finding_rows(scan.rows);
        // The index **file** this replacement established, taken from
        // the replacement itself and not from the listing
        // `record_index_projection` makes afterwards — ruling 0137's
        // recorded half on this path (`rebuild_established_backing`).
        let established = rebuild_established_backing(&outcome);
        // `--rebuild` writes exactly the full journal walk, so its
        // outcome *is* a reconciliation outcome and is recorded as one:
        // a rebuild that succeeds clears a `behind` window that the
        // operator ran it to clear, and one that fails leaves the health
        // saying so rather than silently reverting to the previous
        // reading. Unlike the sweep this is a refusal, not a warning —
        // an administrator who asked for a rebuild that did not happen
        // is told by exit code, not only by a field. Recorded under the
        // same hold as the replacement, for the same reason the sweep
        // records under its own.
        match outcome {
            Ok(_backing) => {
                // A whole-file replacement that returned `Ok` ran
                // `rewrite_rows` to its end, and its last act is a
                // successful `fsync` of the atlas directory — which is
                // why `--rebuild` is what the durability window's own
                // recovery sentence tells an operator to run.
                //
                // It also renamed a file into place on the way there,
                // and `backing` is that call's own answer about the
                // index file (ruling 0137's recorded half, exactly as
                // the sweep's append reports it). Recorded here rather
                // than left to the listing `record_index_projection`
                // takes afterwards: a deletion between this rename and
                // that listing is a later observation of the same file,
                // never evidence that the estate never wrote one.
                record_index_projection(
                    state,
                    observation,
                    IndexProjection::Synchronized,
                    DirectoryDurability::Confirmed,
                    established,
                );
                drop(atlas);
            }
            Err(wirk_atlas::AtlasError::DurabilityUncertain(detail)) => {
                record_index_projection(
                    state,
                    observation,
                    IndexProjection::DurabilityUnconfirmed {
                        detail: detail.clone(),
                    },
                    DirectoryDurability::Uncertain(detail.clone()),
                    established,
                );
                drop(atlas);
                return err_reply(
                    "AtlasError",
                    &format!(
                        "catalog is visible but its directory entry durability is uncertain: {detail}"
                    ),
                );
            }
            Err(err) => {
                record_index_projection(
                    state,
                    observation,
                    IndexProjection::Behind {
                        // A failed whole-file replacement left the file
                        // as it was; how far behind that is, is what the
                        // previous sweep already knew, and this one did
                        // not measure. Reported as unknown rather than
                        // invented.
                        pending: None,
                        detail: err.to_string(),
                    },
                    // The replacement failed before its rename, so the
                    // directory was never synced by this call.
                    DirectoryDurability::Unestablished,
                    // And for the same reason it neither read an index
                    // file nor wrote one, so it establishes nothing
                    // about one: `established` is `Unknown` on every
                    // failure but the post-rename window above.
                    established,
                );
                drop(atlas);
                return err_reply("AtlasError", &err.to_string());
            }
        }
    }
    // One snapshot: the rows and the health that describes them, taken
    // together under the mutex that orders every publication and its own
    // health record. Everything below — the replay, the lineage walk,
    // the per-row scoping — happens outside the lock and against the
    // captured pair, so a repair that lands while this reply is being
    // rendered changes neither half of it (ruling 0125, case 2A).
    let (rows, health) = match read_findings_with_health(state) {
        Ok(pair) => pair,
        Err(err) => return err_reply("AtlasError", &err.to_string()),
    };
    // The window a verifier parks at to prove the snapshot is one: after
    // the pair is captured and the lock released, before a single row is
    // rendered.
    wirk_atlas::checkpoint("findings-index-read");
    if payload.admin {
        return ok_reply(with_paired_index_health(
            &health,
            true,
            json!({
                "rows": rows.iter().map(finding_row_json).collect::<Vec<_>>(),
                "retired_index_copies": retired,
            }),
        ));
    }
    let requester_id = payload
        .requester
        .as_ref()
        .expect("the exclusivity check above admitted a requester");
    let Some(requester_events) = replay_events(state, requester_id) else {
        return err_reply("NotFound", "no such requesting work");
    };
    if requester_events.is_empty() {
        return err_reply("NotFound", "no such requesting work");
    }
    let requester = fold(&requester_events);
    let lineage = lineage_of(state, &requester, &requester_events);
    let mut view = DisclosureView::new(&requester, &requester_events, &lineage);
    // Two routes to a row, and one rendering.
    //
    // **On lineage**, unchanged: the row is this requester's own or its
    // family's, and it is rendered through its own source grants, part
    // by part, so an unadmitted half is withheld and counted.
    //
    // **Off lineage**, the estate-publication route
    // (`LATER-DISCOVERY-ADJUDICATION.md`): a genuinely settled
    // publication is admitted whole, to a requester whose own bindings
    // effectively admit its content, or not at all. Never in halves —
    // an off-lineage requester that may not see every part of a
    // published row learns that the row exists at all only as one more
    // number in `off_lineage`, exactly as a denied requester does.
    let mut scoped = Vec::new();
    let mut off_lineage = 0usize;
    for row in &rows {
        if lineage.contains(&row.origin.work) {
            scoped.push(finding_row_json_scoped(state, &mut view, row));
            continue;
        }
        match published_row_scoped(state, &mut view, row) {
            Some(value) => scoped.push(value),
            None => off_lineage += 1,
        }
    }
    // The `index` block below is the one thing this reply says about
    // rows the requester is not shown: whether the file it was rendered
    // from is a complete projection of the estate's journals. It carries
    // no count and no identity for a scoped requester, so it widens
    // nothing — and without it a stale index answers a scoped query
    // exactly as a complete one does, which is the silence ruling 0116
    // recorded.
    ok_reply(with_paired_index_health(
        &health,
        false,
        json!({
            "rows": scoped,
            // Counts, never identities — `wirk_atlas::AdmissionSummary`'s
            // own established shape: how many rows lay off this requester's
            // lineage and how many parts of the rows it did receive were
            // withheld, and nothing about either. Every counted row is still
            // off this requester's lineage; what the count no longer implies
            // is that lineage alone decided it.
            "disclosure": {"off_lineage": off_lineage, "withheld": view.withheld},
        }),
    ))
}

/// One estate publication, rendered for a requester that is **not** on
/// the producing lineage — or `None`, which the caller counts and says
/// nothing else about.
///
/// The defect this closes (`later-discovery-probe/HANDOFF.md`, executed):
/// the index gate was lineage and only lineage, so a later Work admitted
/// to *exactly the same sources* as the settling Work — able to resolve
/// the reviewed bytes itself, exit 0 — received the byte-identical empty
/// reply a Work denied those sources received. A weaker child saw the
/// row; a stronger independent Work did not. Source admission changed
/// nothing, which made genuinely settled estate knowledge undiscoverable
/// to every Work outside one family.
///
/// The correction is deliberately not "drop the gate". Four conditions,
/// each for its own reason:
///
/// 1. **A policy receipt, not an opinion.** Only a `Settled` row
///    carrying its `Settlement` publishes off lineage. An `Asserted` row
///    is a recorded judgement and an `Applied` row is source-side
///    history — `W-B-LATER-WORK-ADJUDICATION` is explicit that a
///    reference never promotes a claim and that publication must
///    preserve epistemic status, so neither crosses. This is a bound on
///    *this* route, not a redefinition of the estate index: both kinds
///    remain indexed and remain reachable on lineage and administratively
///    exactly as before.
/// 2. **`SupersededBy` never crosses.** It names no producing execution,
///    so there is no checkout whose bindings could bound it.
/// 3. **Producer bindings, conservatively.** A published row carries
///    authored text — the recorded `claim`, and through the settlement
///    the reviewing World's own `intent`, the recipe, `proves`, and the
///    report artifacts' names and paths. Any of it can quote any source
///    its author could read, and no narrower provenance for free prose
///    exists to check. So the requester must independently admit *every*
///    binding of both the Work whose journal raised and published the
///    row and the Work whose checkout produced the receipt — the same
///    `admits_work_checkout` rule `settlement_json_scoped` already
///    applies to an artifact path, applied to the whole row.
/// 4. **Narrower provenance wherever it resolves.** Admitting the
///    producer's bindings as a set is the fallback, not the standard:
///    every frozen review target names an exact membership, and each one
///    goes through `admitted_membership_for` under this requester's own
///    scope, plus the established alias-level `disclosure_admitted`.
///
/// Finally the rendered value must come back with **nothing withheld**.
/// That is not belt and braces about the four checks above; it is what
/// keeps a future field from leaking through this route by default — a
/// new part that `finding_row_json_scoped` learns to withhold drops the
/// whole row here instead of publishing it in halves.
fn published_row_scoped(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    row: &wirk_atlas::FindingRow,
) -> Option<Value> {
    if row.kind != wirk_atlas::FindingRowKind::Settled {
        return None;
    }
    let settlement = row.settlement.as_ref()?;
    let producer = settlement_producing_work(&settlement.check)?;
    if !view.admits_work_checkout(state, &row.origin.work)
        || !view.admits_work_checkout(state, producer)
    {
        return None;
    }
    if !review_targets_admitted(state, view.requester, &settlement.check) {
        return None;
    }
    let mut disclosure = SourceDisclosure::default();
    let mut from_producing_checkout = false;
    settlement_source_disclosure(settlement, &mut disclosure, &mut from_producing_checkout);
    if !disclosure_admitted(state, view.requester, &disclosure) {
        return None;
    }
    let before = view.withheld;
    let value = finding_row_json_scoped(state, view, row);
    if view.withheld != before {
        view.withheld = before;
        return None;
    }
    Some(value)
}

/// Every frozen review target's exact membership, admitted under the
/// requester's own scope through the catalog — never the record's own
/// alias string alone. `settlement_source_disclosure` already checks the
/// alias; this is the narrower identity beside it, and it is why
/// admitting a source by name is not enough to publish a review of a
/// membership within it that this requester is not bound to.
fn review_targets_admitted(
    state: &Arc<WirkdState>,
    requester: &Work,
    check: &SettlementCheck,
) -> bool {
    let SettlementCheck::ActorReview { proof, .. } = check else {
        return true;
    };
    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let scope = wirk_atlas::QueryScope::Work(requester.repositories.clone());
    proof.targets.iter().all(|target| {
        admitted_membership_for(
            &atlas,
            &scope,
            &wirk_atlas::MembershipId(target.membership.clone()),
        )
        .is_some()
    })
}

/// `finding_row_json`, rendered for one requester. The row's own
/// identity, kind and origin events are journal-side; its settlement and
/// Application are the source-side, and its authored prose is bounded by
/// the origin Work's own admission. All three go through the same view
/// `finding list` uses, so the index and the list can never disagree
/// about what one requester may see.
fn finding_row_json_scoped(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    row: &wirk_atlas::FindingRow,
) -> Value {
    let mut value = finding_row_json(row);
    if !admits_authored_prose(state, view, &row.origin.work) {
        view.withheld += withhold_authored_prose(&mut value["finding"]);
    }
    if let Some(assertion) = &row.assertion
        && !admits_assertion_prose(state, view, assertion.author.as_ref())
    {
        view.withheld += withhold_assertion_prose(&mut value["assertion"]);
    }
    if let Some(settlement) = &row.settlement {
        value["settlement"] = settlement_json_scoped(state, view, settlement);
    }
    if let Some(application) = &row.applied {
        value["applied"] = application_json_scoped(view, application);
    }
    value
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    fn orienting_def(id: &str) -> WaypointDefinition {
        WaypointDefinition {
            id: WaypointId(id.to_string()),
            kind: WaypointKind::Actor,
            declared_outputs: Vec::new(),
            intent: Some("investigate reserve_next_leaf".to_string()),
            command: None,
            boundary: Boundary(Vec::new()),
            leaves: Vec::new(),
            required_child_outcomes: Vec::new(),
            selection: None,
            verifies: None,
            orient: Some(wirk_core::OrientationRequest {
                question: "which function reserves the next leaf?".to_string(),
                sources: Vec::new(),
                budget: wirk_core::PresentationBudget::default(),
                semantic: None,
                capacity: None,
            }),
        }
    }

    /// Bounded Conflict is honest: when the estate moves under the
    /// assembler on every lap, the reservation still proceeds and the
    /// projection says so — an explicit `Degraded` coverage, no captured
    /// vector, no bound evidence, and an assumption naming what happened.
    /// It never claims the estate holds nothing.
    #[test]
    fn the_degraded_projection_claims_nothing_and_says_why() {
        let def = orienting_def("r/leaf");
        let orient = def.orient.clone().unwrap();
        let prepared = degraded_projection(
            &def,
            &orient,
            "edition-1",
            ObservationSpan {
                laps: 8,
                window_ms: 137,
            },
            DegradedCause::PublicationChurn,
        );
        let content = prepared
            .file
            .content
            .current()
            .expect("this wave writes v3 content");
        assert!(matches!(
            content.coverage,
            wirk_core::EvidenceCoverage::Degraded {
                reason: wirk_core::CoverageReason::ConcurrentPublication
            }
        ));
        assert!(content.bound.is_empty());
        assert!(content.generations.is_empty());
        assert_eq!(content.question, orient.question);
        assert_eq!(content.waypoint, def.id);
        assert_eq!(prepared.file.receipt.laps, 8);
        // The window the assembly actually spent, not a zero standing in
        // for one: the first candidate reported 0ms for eight laps
        // (ruling 0126, F1).
        assert_eq!(prepared.file.receipt.observation_window_ms, 137);
        assert!(
            content.assumptions.iter().any(|statement| statement
                .text
                .contains("Nothing here is a statement that the estate holds nothing")),
            "{:?}",
            content.assumptions
        );
        // It is a real projection: its own id, over its own content.
        assert_eq!(prepared.reference.projection, content.projection_id());
    }

    /// Ruling 0124, the rule this wave must not break: a failed re-check
    /// never falls through to a stale reservation. A projection prepared
    /// for one Waypoint is discarded — not attached — when the Waypoint
    /// actually being reserved is a different one, and what is journaled
    /// instead is an honest degraded projection for the right Waypoint.
    #[test]
    fn a_projection_prepared_for_another_waypoint_is_discarded_not_attached() {
        let dir = tempfile::tempdir().expect("temp estate");
        let state = Arc::new(WirkdState {
            estate_root: dir.path().to_path_buf(),
            journals: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            atlas: Mutex::new(
                wirk_atlas::AtlasStore::open(dir.path(), dir.path().display().to_string())
                    .expect("atlas"),
            ),
            continuation_key: [0u8; 32],
            index_health: Mutex::new(IndexHealth::unreconciled()),
            // Integration seam: the index-recovery wave gave `WirkdState`
            // its own per-daemon observation ticket, and a state built
            // here is a fresh daemon's. Zero is what `WirkdState::new`
            // and the index suite's own `state_over` both start it at,
            // and nothing in these two tests takes a ticket.
            index_observations: AtomicU64::new(0),
        });
        let reserving = orienting_def("r/leaf-b");
        let stale = degraded_projection(
            &orienting_def("r/leaf-a"),
            &orienting_def("r/leaf-a").orient.clone().unwrap(),
            "edition-1",
            ObservationSpan {
                laps: 1,
                window_ms: 42,
            },
            DegradedCause::PublicationChurn,
        );
        let stale_observation = stale.reference.observation.clone();
        let work = WorkId("work-1".to_string());
        let reference = reservation_evidence(&state, &work, &reserving, &[], Some(stale))
            .expect("evidence resolves")
            .expect("an orienting reservation always carries a projection");
        assert_ne!(
            reference.observation, stale_observation,
            "the stale preparation must not be journaled"
        );
        let file = wirk_core::ProjectionFile::read_referenced(dir.path(), &work, &reference)
            .expect("the journaled reference names a written file");
        assert_eq!(
            file.content.waypoint(),
            &reserving.id,
            "the journaled projection names the Waypoint actually being reserved"
        );
        assert!(matches!(
            file.content
                .current()
                .expect("this build writes v3 content")
                .coverage,
            wirk_core::EvidenceCoverage::Degraded { .. }
        ));
        // And a Waypoint that declares no orientation gets nothing at
        // all, from the same call.
        let mut plain = orienting_def("r/leaf-c");
        plain.orient = None;
        assert!(
            reservation_evidence(&state, &work, &plain, &[], None)
                .expect("no evidence to resolve")
                .is_none()
        );
    }

    /// Ruling 0126, F1, at the reservation site: the receipt is now
    /// integrity-covered, so a lap count or a window it reports is a
    /// checkable claim about what happened rather than a decoration. A
    /// preparation discarded for naming the wrong Waypoint really did
    /// cost its laps and its wall-clock, and that is what the degraded
    /// receipt reports; a reservation that reached the commit guard with
    /// nothing prepared spent no observation lap at all, and says zero
    /// rather than the loop's maximum.
    #[test]
    fn a_degraded_receipt_reports_the_observation_that_actually_happened() {
        let dir = tempfile::tempdir().expect("temp estate");
        let state = Arc::new(WirkdState {
            estate_root: dir.path().to_path_buf(),
            journals: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            atlas: Mutex::new(
                wirk_atlas::AtlasStore::open(dir.path(), dir.path().display().to_string())
                    .expect("atlas"),
            ),
            continuation_key: [0u8; 32],
            index_health: Mutex::new(IndexHealth::unreconciled()),
            // Integration seam: the index-recovery wave gave `WirkdState`
            // its own per-daemon observation ticket, and a state built
            // here is a fresh daemon's. Zero is what `WirkdState::new`
            // and the index suite's own `state_over` both start it at,
            // and nothing in these two tests takes a ticket.
            index_observations: AtomicU64::new(0),
        });
        let work = WorkId("work-1".to_string());
        let reserving = orienting_def("r/leaf-b");
        let elsewhere = orienting_def("r/leaf-a");
        let stale = degraded_projection(
            &elsewhere,
            &elsewhere.orient.clone().unwrap(),
            "edition-1",
            ObservationSpan {
                laps: 5,
                window_ms: 2_471,
            },
            DegradedCause::PublicationChurn,
        );

        let discarded = reservation_evidence(&state, &work, &reserving, &[], Some(stale))
            .expect("evidence resolves")
            .expect("an orienting reservation always carries a projection");
        let file = wirk_core::ProjectionFile::read_referenced(dir.path(), &work, &discarded)
            .expect("the journaled reference names a written file");
        assert_eq!(file.receipt.laps, 5);
        assert_eq!(
            file.receipt.observation_window_ms, 2_471,
            "the laps the discarded preparation really spent, not a zero"
        );

        let nothing = reservation_evidence(&state, &work, &reserving, &[], None)
            .expect("evidence resolves")
            .expect("an orienting reservation always carries a projection");
        let file = wirk_core::ProjectionFile::read_referenced(dir.path(), &work, &nothing)
            .expect("the journaled reference names a written file");
        assert_eq!(
            (file.receipt.laps, file.receipt.observation_window_ms),
            (0, 0),
            "no observation lap ran here, and the receipt must not claim one did"
        );
        assert!(
            file.content
                .current()
                .expect("this build writes v3 content")
                .assumptions
                .iter()
                .any(|statement| statement
                    .text
                    .contains("is not the one any prepared assembly was made for")),
            "the degraded projection must name the race it actually lost: {:?}",
            file.content
                .current()
                .expect("this build writes v3 content")
                .assumptions
        );
    }

    /// The token grammar, stated as behaviour rather than as prose: a
    /// path and an identifier are references; an ordinary word is not,
    /// and is therefore neither looked up nor reported as an unknown
    /// fact (ruling 0124).
    #[test]
    fn ordinary_prose_is_not_a_reference_and_paths_and_identifiers_are() {
        let references = authored_references(&[
            "Which function in wirk/src/wirkd/server.rs:12326 decides whether reserve_next_leaf              refuses a claim outside the declared boundary? See WorldHash::of and notes/plan.md.",
        ]);
        let paths: Vec<&str> = references
            .iter()
            .filter_map(|reference| match reference {
                Reference::Path(path) => Some(path.as_str()),
                _ => None,
            })
            .collect();
        let identifiers: Vec<&str> = references
            .iter()
            .filter_map(|reference| match reference {
                Reference::Identifier(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(paths, vec!["wirk/src/wirkd/server.rs", "notes/plan.md"]);
        assert_eq!(identifiers, vec!["reserve_next_leaf", "WorldHash"]);
        for prose in [
            "Which", "function", "decides", "whether", "refuses", "claim", "outside", "the",
            "declared", "boundary", "See", "and", "of", "in", "a",
        ] {
            assert!(
                !references.iter().any(|reference| reference.text() == prose),
                "{prose:?} is ordinary prose, not a reference"
            );
        }
    }

    /// Delivery order is first-appearance order, deduplicated — the one
    /// order that is not an invention, and part of the fingerprint.
    #[test]
    fn references_are_deduplicated_in_first_appearance_order() {
        let references = authored_references(&[
            "check reserve_next_leaf then src/a.rs then reserve_next_leaf again",
            "and src/a.rs once more",
        ]);
        assert_eq!(
            references,
            vec![
                Reference::Identifier("reserve_next_leaf".to_string()),
                Reference::Path("src/a.rs".to_string()),
            ]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// The journal lock discipline's own detector, watched failing
    /// (`CLAUDE.md`: a test is deterministic and has been watched fail,
    /// or it is not a test). `no_journal_guard_held` is what makes the
    /// rule enforced rather than hoped, and a counter that never counted
    /// would be silently inert — the discipline would then be a comment.
    #[test]
    #[should_panic(expected = "must not run under a journal guard")]
    fn locking_a_second_journal_under_a_held_guard_is_refused_in_debug() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Mutex::new(Journal::open(dir.path()).unwrap());
        let held = lock_journal(&journal);
        no_journal_guard_held("this test");
        drop(held);
    }

    /// And the counter is balanced: after the guard is dropped the same
    /// call is fine, so the assertion above is about the guard being
    /// held and not about having ever held one.
    #[test]
    fn the_guard_count_returns_to_zero_when_the_guard_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Mutex::new(Journal::open(dir.path()).unwrap());
        {
            let _held = lock_journal(&journal);
        }
        no_journal_guard_held("this test");
    }

    // ---- Ruling 0160: `prior_launch_for_waypoint` walks the lineage ----

    /// Builds one attempt at `waypoint`: its `RunOpened`, and — when
    /// `launch` is `Some` — the `RunLaunchRequested` that admits it.
    /// The same two events, in the same order, the real journal
    /// carries; nothing is stubbed, because `prior_launch_for_waypoint`
    /// reads events and `find_run` folds them with the production
    /// `Run::apply`.
    fn attempt_events(
        work: &WorkId,
        waypoint: &str,
        attempt: u32,
        run_id: &str,
        launch: Option<(&str, &str)>,
    ) -> Vec<Event> {
        let run = RunId(run_id.to_string());
        let mut events = vec![new_event(
            work,
            Some(run.clone()),
            EventKind::RunOpened {
                run: run.clone(),
                waypoint: WaypointId(waypoint.to_string()),
                attempt,
                world_hash: WorldHash("hash".to_string()),
            },
        )];
        if let Some((kind, model)) = launch {
            events.push(new_event(
                work,
                Some(run.clone()),
                EventKind::RunLaunchRequested {
                    run,
                    actor_kind: ActorKind(kind.to_string()),
                    selection: ActorSelection {
                        model: Some(model.to_string()),
                        effort: Some("high".to_string()),
                        args: Vec::new(),
                    },
                },
            ));
        }
        events
    }

    /// The blocking defect ruling 0160 named: attempt 1 admitted an
    /// explicit `codex`, attempt 2 was opened and abandoned before
    /// launch, and the attempt-minus-one lookup then answered attempt 3
    /// with `None` — so `wirk run` resolved the hardcoded `claude`
    /// default over an explicit choice still durable in this very event
    /// list. The lineage walk must find attempt 1.
    #[test]
    fn an_intervening_unlaunched_attempt_does_not_hide_the_lineages_admitted_launch() {
        let work = WorkId("work-lineage".to_string());
        let wp = "lin/wp-1";
        let mut events = attempt_events(&work, wp, 1, "run-1", Some(("codex", "gpt-5-codex")));
        events.extend(attempt_events(&work, wp, 2, "run-2", None));
        events.extend(attempt_events(&work, wp, 3, "run-3", None));

        let carried = prior_launch_for_waypoint(&events, &WaypointId(wp.to_string()), 3)
            .expect("attempt 1's admitted launch is still the lineage's latest admitted choice");
        assert_eq!(carried.0, ActorKind("codex".to_string()));
        assert_eq!(carried.1.model.as_deref(), Some("gpt-5-codex"));

        // And it does not decay with distance: attempt 4, two
        // unlaunched attempts out, is the case that showed the loss was
        // permanent rather than a one-attempt blip.
        let carried = prior_launch_for_waypoint(&events, &WaypointId(wp.to_string()), 4)
            .expect("two abandoned attempts still decided nothing");
        assert_eq!(carried.0, ActorKind("codex".to_string()));
    }

    /// Newest admitted wins: a later same-harness override that was
    /// itself admitted is the lineage's operative choice, not the older
    /// admission it replaced.
    #[test]
    fn the_latest_admitted_launch_is_the_one_carried() {
        let work = WorkId("work-lineage".to_string());
        let wp = "lin/wp-1";
        let mut events = attempt_events(&work, wp, 1, "run-1", Some(("codex", "gpt-5-codex")));
        events.extend(attempt_events(
            &work,
            wp,
            2,
            "run-2",
            Some(("codex", "gpt-5")),
        ));
        events.extend(attempt_events(&work, wp, 3, "run-3", None));

        let carried = prior_launch_for_waypoint(&events, &WaypointId(wp.to_string()), 4)
            .expect("attempt 2's admitted override is the latest admitted choice");
        assert_eq!(carried.1.model.as_deref(), Some("gpt-5"));
    }

    /// The allowed `None`s, kept `None`: a first attempt, and a lineage
    /// in which nothing was ever admitted. Nothing is fabricated in
    /// either case — that is what would invent a choice nobody made.
    #[test]
    fn a_lineage_that_never_admitted_a_launch_carries_nothing() {
        let work = WorkId("work-lineage".to_string());
        let wp = "lin/wp-1";
        let mut events = attempt_events(&work, wp, 1, "run-1", None);
        events.extend(attempt_events(&work, wp, 2, "run-2", None));
        assert_eq!(
            prior_launch_for_waypoint(&events, &WaypointId(wp.to_string()), 3),
            None
        );
        assert_eq!(
            prior_launch_for_waypoint(&events, &WaypointId(wp.to_string()), 1),
            None,
            "a first attempt has no prior Run at all"
        );
    }

    /// A distinct Waypoint's lineage is its own: a sibling's admitted
    /// launch is never inherited, however recent it is.
    #[test]
    fn a_distinct_waypoint_does_not_inherit_a_siblings_admitted_selection() {
        let work = WorkId("work-lineage".to_string());
        let mut events = attempt_events(
            &work,
            "lin/wp-1",
            1,
            "run-1",
            Some(("codex", "gpt-5-codex")),
        );
        events.extend(attempt_events(&work, "lin/wp-2", 1, "run-2", None));
        events.extend(attempt_events(&work, "lin/wp-2", 2, "run-3", None));
        assert_eq!(
            prior_launch_for_waypoint(&events, &WaypointId("lin/wp-2".to_string()), 3),
            None,
            "wp-2 authored nothing of its own; wp-1's choice is not its to inherit"
        );
    }

    /// A real `WirkdState` over a real, empty estate: a real
    /// `AtlasStore` on a real directory, so `record_index_projection`'s
    /// own listing of `atlas/` is a real listing and nothing here is a
    /// stand-in for the thing under test.
    fn state_over(estate_root: &Path) -> Arc<WirkdState> {
        std::fs::create_dir_all(estate_root).unwrap();
        let atlas =
            wirk_atlas::AtlasStore::open(estate_root, estate_root.display().to_string()).unwrap();
        Arc::new(WirkdState {
            estate_root: estate_root.to_path_buf(),
            journals: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            atlas: Mutex::new(atlas),
            continuation_key: [0u8; 32],
            index_health: Mutex::new(IndexHealth::unreconciled()),
            index_observations: AtomicU64::new(0),
        })
    }

    fn projection(state: &Arc<WirkdState>) -> IndexProjection {
        state
            .index_health
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .projection
            .clone()
    }

    /// The defect the bounded verification executed as D3, at the record
    /// it happens in: a retirement whose atlas-directory `fsync` failed
    /// reports the window to its one caller, and the reconciliation the
    /// same handler then runs finds nothing to append — so it writes
    /// nothing, `fsync`s nothing, and used to certify the estate
    /// `Synchronized` over the entry it had just been told it could not
    /// confirm.
    ///
    /// A sweep that wrote nothing establishes nothing about the
    /// directory. The window stands.
    #[test]
    fn a_sweep_that_wrote_nothing_does_not_confirm_a_directory_sync_that_failed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_over(&dir.path().join("estate"));

        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Uncertain("directory sync failed: EIO".to_string()),
            RecordedBacking::Unknown,
        );
        assert!(
            matches!(
                projection(&state),
                IndexProjection::DurabilityUnconfirmed { .. }
            ),
            "the failing call's own record says the window is open"
        );

        // Every later ordinary observation: the index holds every row,
        // the append had nothing to add, and not one byte was written.
        for _ in 0..3 {
            record_index_projection(
                &state,
                next_index_observation(&state),
                IndexProjection::Synchronized,
                DirectoryDurability::Unestablished,
                RecordedBacking::Unknown,
            );
            assert!(
                matches!(
                    projection(&state),
                    IndexProjection::DurabilityUnconfirmed { .. }
                ),
                "a no-op reconciliation must not certify a directory it never opened"
            );
        }
        assert!(
            !state.index_health.lock().unwrap().complete(),
            "and the estate does not read as complete while it stands"
        );
    }

    /// And it is not a latch: the one thing that answers the question is
    /// an `fsync` of that directory that returns success — an ordinary
    /// index write, an administrative `--rebuild`, or a retirement that
    /// renames a copy. Every one of them arrives here as `Confirmed`.
    #[test]
    fn a_successful_directory_sync_resolves_the_window_and_nothing_else_does() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_over(&dir.path().join("estate"));

        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Uncertain("directory sync failed: EIO".to_string()),
            RecordedBacking::Unknown,
        );
        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Confirmed,
            RecordedBacking::Unknown,
        );
        assert_eq!(projection(&state), IndexProjection::Synchronized);
        assert!(state.index_health.lock().unwrap().complete());
    }

    /// The partial branch, where the fact was masked rather than
    /// dropped: a retirement that renamed one copy, could not confirm
    /// it, and was refused on the next one. The preserved copy that is
    /// still there makes the projection `behind` on its own account —
    /// and the durability window is *also* said, in the administrator's
    /// detail, rather than lost behind the copy that happens to remain.
    /// When that last copy is retired the window must still be there
    /// unless something really synced.
    #[test]
    fn a_preserved_copy_masks_neither_the_durability_window_nor_its_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let estate_root = dir.path().join("estate");
        let state = state_over(&estate_root);
        let preserved = estate_root
            .join("atlas")
            .join(format!("{}01ABC", wirk_atlas::PRESERVED_INDEX_PREFIX));
        std::fs::write(&preserved, b"not json\n").unwrap();

        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Uncertain("directory sync failed: EIO".to_string()),
            RecordedBacking::Unknown,
        );
        let IndexProjection::Behind { detail, .. } = projection(&state) else {
            panic!("a held preserved copy is `behind` whatever else is true");
        };
        assert!(
            detail.contains("preserved copy(ies)"),
            "the copy that is held is said: {detail}"
        );
        assert!(
            detail.contains("directory sync failed"),
            "and so is the window the retirement could not confirm: {detail}"
        );

        // The administrator retires the last copy, and this time nothing
        // synced the directory (the rename is gone from the estate, but
        // the earlier entry is still unconfirmed).
        std::fs::remove_file(&preserved).unwrap();
        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Unestablished,
            RecordedBacking::Unknown,
        );
        assert!(
            matches!(
                projection(&state),
                IndexProjection::DurabilityUnconfirmed { .. }
            ),
            "with the copy gone, what is left is the window — not a clean estate"
        );
    }

    /// The ordering defect the independent verification executed with
    /// the product's own barrier and a real `EIO` from a real `fsync`
    /// (`index-retirement-durability-verify/raw/44`), at the record it
    /// happens in.
    ///
    /// The two calls are the two in that log. **B** is an ordinary
    /// mutation: it takes its ticket first, walks, then waits for the
    /// Atlas mutex. **A** is the retirement: it renames and `fsync`s the
    /// atlas directory successfully, drops its guard, and only then
    /// sweeps — so A's sweep ticket is *newer* than B's, while A's
    /// `fsync` is *older* than B's. B then wins the mutex, appends, and
    /// its own directory `fsync` fails.
    ///
    /// The tickets therefore run one way and the syncs the other, which
    /// is why no ticket the retirement could take fixes this: taken
    /// before its Atlas guard it is still newer than B's, because B's
    /// was taken before the retirement's call arrived. What orders two
    /// `fsync`s is the Atlas critical sections they were made in, and A
    /// records in its own. B's failure is the last word.
    #[test]
    fn an_older_successful_sync_does_not_erase_a_failure_that_happened_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_over(&dir.path().join("estate"));

        // B takes its ticket and walks, outside the Atlas mutex.
        let b = next_index_observation(&state);
        // A's retirement: under its own Atlas guard, the renames land and
        // the directory `fsync` returns success.
        record_directory_durability(&state, DirectoryDurability::Confirmed);
        // A drops that guard and sweeps, taking a later ticket.
        let a = next_index_observation(&state);
        assert!(b < a, "the retirement's sweep ticket is the newer one");

        // B wins the mutex, appends its row, and its directory `fsync`
        // fails — after A's succeeded.
        record_index_projection(
            &state,
            b,
            IndexProjection::Synchronized,
            DirectoryDurability::Uncertain("directory sync failed: EIO".to_string()),
            RecordedBacking::Unknown,
        );
        // A's sweep finds nothing to append, so it writes nothing and
        // establishes nothing.
        record_index_projection(
            &state,
            a,
            IndexProjection::Synchronized,
            DirectoryDurability::Unestablished,
            RecordedBacking::Unknown,
        );

        assert!(
            matches!(
                projection(&state),
                IndexProjection::DurabilityUnconfirmed { .. }
            ),
            "the window the later failing sync opened must still stand"
        );
        assert!(
            !state.index_health.lock().unwrap().complete(),
            "and the estate must not read complete over a rename nothing confirmed"
        );
    }

    /// The same two callers, the same ticket order, the opposite **sync**
    /// order — and the opposite outcome, so the rule above is the
    /// critical sections' order and not a rule against retirements.
    ///
    /// B's `fsync` fails first; A's retirement then really does `fsync`
    /// that directory and it really does return success, which confirms
    /// every entry pending in it, B's included. The window is resolved,
    /// exactly as a later successful sync always resolves it.
    #[test]
    fn a_sync_that_really_is_later_still_resolves_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_over(&dir.path().join("estate"));

        let b = next_index_observation(&state);
        record_index_projection(
            &state,
            b,
            IndexProjection::Synchronized,
            DirectoryDurability::Uncertain("directory sync failed: EIO".to_string()),
            RecordedBacking::Unknown,
        );
        assert!(
            matches!(
                projection(&state),
                IndexProjection::DurabilityUnconfirmed { .. }
            ),
            "B's failure opens the window"
        );

        // A's retirement, in its own critical section, after B's.
        record_directory_durability(&state, DirectoryDurability::Confirmed);
        let a = next_index_observation(&state);
        assert!(b < a);
        record_index_projection(
            &state,
            a,
            IndexProjection::Synchronized,
            DirectoryDurability::Unestablished,
            RecordedBacking::Unknown,
        );

        assert_eq!(projection(&state), IndexProjection::Synchronized);
        assert!(state.index_health.lock().unwrap().complete());
    }

    /// The two observations of the index file one attempt makes, and the
    /// one rule that resolves them (ruling 0137's recorded half).
    #[test]
    fn a_listing_taken_after_the_publication_cannot_unsay_what_the_append_saw() {
        use RecordedBacking::{Absent, Present, Unknown};
        // The seam: this attempt's own append read or wrote the file,
        // and the listing a moment later did not see it. There was a
        // file, and a read that later finds none may not call its
        // emptiness known.
        assert_eq!(recorded_backing(Present, Absent), Present);
        assert_eq!(recorded_backing(Present, Unknown), Present);
        assert_eq!(recorded_backing(Present, Present), Present);
        // A file that appeared after an attempt that found none is still
        // a file whose contents no later read establishes.
        assert_eq!(recorded_backing(Absent, Present), Present);
        // The one pair that is a known empty: nothing this attempt
        // looked at held an index. The pre-publication estate, whose
        // empty projection really is complete.
        assert_eq!(recorded_backing(Absent, Absent), Absent);
        // Evidence in neither direction.
        assert_eq!(recorded_backing(Absent, Unknown), Unknown);
        assert_eq!(recorded_backing(Unknown, Unknown), Unknown);
        // A caller that established nothing of its own leaves the
        // listing to decide — every caller's behaviour before this
        // repair, and the `--rebuild` arms' behaviour after it.
        assert_eq!(recorded_backing(Unknown, Absent), Absent);
        assert_eq!(recorded_backing(Unknown, Present), Present);
    }

    /// Which outcomes of a write establish an index **file**, on both
    /// write paths, including the failure that happens after the rename.
    ///
    /// The seam this closes on the rebuild path is the same one the
    /// append closed: an `Ok` rebuild renamed a file into place, so a
    /// listing taken afterwards that misses it is a later observation,
    /// not evidence that the estate never wrote an index.
    #[test]
    fn a_write_that_renamed_a_file_establishes_one_even_when_it_then_failed() {
        use wirk_atlas::IndexBacking;
        // A rebuild is an unconditional replacement: it publishes a file
        // whether the walk held rows or none at all.
        assert_eq!(
            rebuild_established_backing(&Ok(IndexBacking::Present)),
            RecordedBacking::Present
        );
        // Raised only after the atomic rename: the rows are visible, so
        // there is a file, and only the directory entry is in question.
        assert_eq!(
            rebuild_established_backing(&Err(wirk_atlas::AtlasError::DurabilityUncertain(
                "findings index (1 rows) is visible; directory sync failed".into()
            ))),
            RecordedBacking::Present
        );
        // Every other failure returns before the rename: nothing read,
        // nothing written, nothing established.
        assert_eq!(
            rebuild_established_backing(&Err(wirk_atlas::AtlasError::Catalog(
                "no space left on device".into()
            ))),
            RecordedBacking::Unknown
        );

        // The append's own outcomes, unchanged but for the same
        // post-rename window.
        assert_eq!(
            established_backing(&Ok(wirk_atlas::FindingIndexAppend {
                appended: 0,
                backing: IndexBacking::Absent,
            })),
            RecordedBacking::Absent,
            "a sweep that read no file and wrote none is the pre-publication estate"
        );
        assert_eq!(
            established_backing(&Ok(wirk_atlas::FindingIndexAppend {
                appended: 2,
                backing: IndexBacking::Present,
            })),
            RecordedBacking::Present
        );
        assert_eq!(
            established_backing(&Err(wirk_atlas::FindingIndexUnwritten {
                pending: Some(0),
                error: wirk_atlas::AtlasError::DurabilityUncertain(
                    "findings index (2 rows) is visible; directory sync failed".into()
                ),
            })),
            RecordedBacking::Present,
            "the append's post-rename failure published a file too"
        );
        assert_eq!(
            established_backing(&Err(wirk_atlas::FindingIndexUnwritten {
                pending: Some(2),
                error: wirk_atlas::AtlasError::Catalog("no space left on device".into()),
            })),
            RecordedBacking::Unknown
        );
    }

    /// The record and the read, joined: an attempt that established a
    /// file records one even though the listing at the end of it found
    /// none, and the next read of the missing file is therefore
    /// qualified instead of certified.
    #[test]
    fn an_attempt_that_established_a_file_records_one_and_a_later_read_is_qualified() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_over(&dir.path().join("estate"));
        // The estate's atlas directory is listable and holds no index
        // file, which is exactly the listing the seam produces.
        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Confirmed,
            RecordedBacking::Present,
        );
        let health = state
            .index_health
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        assert_eq!(
            health.index_backing,
            RecordedBacking::Present,
            "the observation this attempt actually made is the one recorded"
        );
        assert!(
            health.complete(),
            "and the projection it recorded is its own: {:?}",
            health.projection
        );

        let qualified = qualified_by_absent_index(health.clone(), wirk_atlas::IndexBacking::Absent);
        assert!(
            !qualified.complete(),
            "a read with no file to open cannot inherit it: {:?}",
            qualified.projection
        );
        assert!(
            state
                .index_health
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .complete(),
            "and the read changed nothing in the record it rendered"
        );

        // The same record, made by an attempt that established no file:
        // the estate that never wrote one, still complete.
        let state = state_over(&dir.path().join("empty-estate"));
        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Unestablished,
            RecordedBacking::Absent,
        );
        let health = state
            .index_health
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        assert_eq!(health.index_backing, RecordedBacking::Absent);
        assert!(
            qualified_by_absent_index(health, wirk_atlas::IndexBacking::Absent).complete(),
            "reading absence as loss here would invent a lost row"
        );
    }

    /// A failing retirement does not wait for its sweep to say so. The
    /// record is made in the critical section that made the `fsync`, so
    /// the window is open the instant the syscall returns — and it is
    /// still open after the sweep that follows, which appended nothing.
    #[test]
    fn a_failed_retirement_sync_opens_the_window_in_its_own_critical_section() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_over(&dir.path().join("estate"));

        // A healthy estate, observed.
        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Confirmed,
            RecordedBacking::Unknown,
        );
        assert_eq!(projection(&state), IndexProjection::Synchronized);

        record_directory_durability(
            &state,
            DirectoryDurability::Uncertain("directory sync failed: EIO".to_string()),
        );
        assert!(
            matches!(
                projection(&state),
                IndexProjection::DurabilityUnconfirmed { .. }
            ),
            "no sweep has run yet, and the estate already says so"
        );

        record_index_projection(
            &state,
            next_index_observation(&state),
            IndexProjection::Synchronized,
            DirectoryDurability::Unestablished,
            RecordedBacking::Unknown,
        );
        assert!(
            matches!(
                projection(&state),
                IndexProjection::DurabilityUnconfirmed { .. }
            ),
            "and the sweep that appended nothing carries it, rather than clearing it"
        );
    }

    /// The detail is said once, not once per attempt: an incomplete walk
    /// whose write also could not be confirmed reports both facts, and
    /// re-reports exactly the same sentence on the next attempt rather
    /// than growing one.
    #[test]
    fn the_durability_detail_does_not_accumulate_across_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_over(&dir.path().join("estate"));
        let uncertain = || DirectoryDurability::Uncertain("directory sync failed: EIO".to_string());

        let mut seen = Vec::new();
        for _ in 0..3 {
            record_index_projection(
                &state,
                next_index_observation(&state),
                IndexProjection::Behind {
                    pending: None,
                    detail: "one journal could not be read".to_string(),
                },
                uncertain(),
                RecordedBacking::Unknown,
            );
            let IndexProjection::Behind { detail, .. } = projection(&state) else {
                panic!("an unreadable journal is `behind`");
            };
            seen.push(detail);
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[1], seen[2]);
        assert_eq!(
            seen[0].matches("directory sync failed").count(),
            1,
            "said once: {}",
            seen[0]
        );
    }

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

    /// `public-retrieval-identity-verify/VERDICT.md` V2, as a guard. An
    /// answer that refused a continuation hands back the caller's own
    /// token, not a new one.
    ///
    /// The token it used to mint carried no application, therefore no
    /// producer pin, so following it refused with "issued before the
    /// query producer identity was recorded" — a description of
    /// pre-correction history applied to a token this build had issued
    /// seconds earlier, minted afresh on every attempt. Preserving the
    /// caller's token is also the branch the original continuation
    /// contract asks for: the same token resumes byte-identically once
    /// the implementation is restored.
    #[test]
    fn a_refused_continuation_preserves_the_callers_token() {
        // These cases are about coverage, never about exhaustion, so
        // each is asked with a page that really returned a row.
        const ONE_ROW: usize = 1;
        let mut coverage = wirk_atlas::AnswerCoverage::default();
        assert_eq!(
            continuation_decision(&coverage, ONE_ROW),
            ContinuationDecision::Fresh
        );

        coverage.continuation_unrecoverable = true;
        assert_eq!(
            continuation_decision(&coverage, ONE_ROW),
            ContinuationDecision::Preserved
        );

        // A refusal is still not a route to a token over content this
        // caller may not read: withholding wins over both.
        let mut denied = wirk_atlas::AnswerCoverage {
            denied: true,
            ..Default::default()
        };
        assert_eq!(
            continuation_decision(&denied, ONE_ROW),
            ContinuationDecision::Withheld
        );
        denied.continuation_unrecoverable = true;
        assert_eq!(
            continuation_decision(&denied, ONE_ROW),
            ContinuationDecision::Withheld
        );

        let no_sources = wirk_atlas::AnswerCoverage {
            no_sources: true,
            ..Default::default()
        };
        assert_eq!(
            continuation_decision(&no_sources, ONE_ROW),
            ContinuationDecision::Withheld
        );
    }

    /// P3 native closeout item 3 (`p3-sources/source-coverage-verify/
    /// raw/p4-walk.txt`): pages 10 through 15 each "returned 0 of 18"
    /// and each still received a fresh continuation token, so the walk
    /// could not end — the next request was byte-identical to the one
    /// that produced nothing, because the offset advances by exactly the
    /// number of rows returned.
    ///
    /// The condition is precisely "zero rows returned, so zero offset
    /// advance", and nothing wider. A page with rows still continues; a
    /// denied or source-less answer still withholds; an unrecoverable
    /// continuation still hands back the caller's own token, because
    /// that refusal is deliberate and returning no rows is exactly how
    /// it presents.
    #[test]
    fn a_page_that_returned_nothing_issues_no_continuation_to_repeat_it() {
        let coverage = wirk_atlas::AnswerCoverage::default();
        assert_eq!(
            continuation_decision(&coverage, 0),
            ContinuationDecision::Exhausted,
            "a page that returned no rows advances no offset, so a token would repeat it"
        );
        assert_eq!(
            continuation_decision(&coverage, 1),
            ContinuationDecision::Fresh,
            "one row is progress: a long walk still continues to its real end"
        );

        // The two deliberate refusals are answered first and are
        // untouched, both of which also return zero rows.
        let unrecoverable = wirk_atlas::AnswerCoverage {
            continuation_unrecoverable: true,
            ..Default::default()
        };
        assert_eq!(
            continuation_decision(&unrecoverable, 0),
            ContinuationDecision::Preserved,
            "an unrecoverable continuation still hands back the caller's own token"
        );
        let denied = wirk_atlas::AnswerCoverage {
            denied: true,
            ..Default::default()
        };
        assert_eq!(
            continuation_decision(&denied, 0),
            ContinuationDecision::Withheld
        );
        let no_sources = wirk_atlas::AnswerCoverage {
            no_sources: true,
            ..Default::default()
        };
        assert_eq!(
            continuation_decision(&no_sources, 0),
            ContinuationDecision::Withheld
        );
    }

    /// `EMPTY-PRODUCER-REVIEW-ADJUDICATION.md` D1(b), as a guard. The
    /// sentence a measured pin publishes is bounded by what the backend
    /// reported, on the JSON surface and the plain one alike — the
    /// executed counterexample is a real, honestly narrowed list that
    /// omits the very module that ranks, and no wording repairs that. It
    /// is stated, not claimed away.
    #[test]
    fn a_measured_basis_detail_is_bounded_by_the_reported_scope() {
        let measured = basis_detail(wirk_atlas::QueryProducerBasis::ImplementationMeasured);
        assert_eq!(measured, wirk_atlas::QUERY_PRODUCER_BASIS_MEASURED);
        assert!(
            measured.contains("the module files this backend reported"),
            "{measured}"
        );
        assert!(
            !measured.contains("of the process that ranked this answer were measured"),
            "a self-reported list is not a measurement of the process: {measured}"
        );
        assert_eq!(
            basis_detail(wirk_atlas::QueryProducerBasis::ConfigurationOnly),
            wirk_atlas::QUERY_PRODUCER_BASIS_MISSING
        );
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

    /// wirkd panicked (`end byte index ... is not a char boundary`) when
    /// an orienting Route's projection ran `evidence_window` over prose
    /// that put a multi-byte character exactly where the budget's raw
    /// ceiling landed: `text[start..end]` was sliced with that raw
    /// ceiling as `end` before any char-boundary correction, at
    /// `wirk/src/wirkd/server.rs:7241` (as of the panic reported in the
    /// field). The reproduction here uses a literal em dash — a 3-byte
    /// UTF-8 character — so a naive byte-count budget lands mid-character
    /// exactly the way the operator's knowledge corpus did.
    #[test]
    fn evidence_window_does_not_panic_when_the_budget_ceiling_lands_inside_a_multibyte_char() {
        // 48 ASCII bytes, then a 3-byte em dash at [48..51), then more
        // ASCII. A budget of 50 puts the raw ceiling at byte 50, which is
        // the middle byte of the em dash — not a char boundary.
        let text = format!("{}—{}", "a".repeat(48), "b".repeat(200));
        assert!(
            !text.is_char_boundary(50),
            "fixture must land mid-character"
        );
        let matches = vec![wirk_atlas::TermMatch {
            offset: 0,
            len: 1,
            term: "a".to_string(),
        }];

        let window = evidence_window(&text, &matches, 50)
            .expect("text longer than the budget with a match must produce a window");

        assert!(
            text.is_char_boundary(window.start) && text.is_char_boundary(window.end),
            "window [{}, {}) must land on char boundaries",
            window.start,
            window.end
        );
        assert!(
            window.end - window.start <= 50,
            "window must not exceed the budget: got {} bytes",
            window.end - window.start
        );
        assert!(
            window.start == 0 && window.end >= 1,
            "window must not cut the anchor away"
        );
    }

    #[test]
    fn evidence_window_does_not_panic_when_the_lead_lands_inside_a_multibyte_char() {
        // The real assembly budget. A single one-byte match makes the
        // lead `(320 - 1) / 2 == 159`, so `line_begin` is asked for the
        // line containing byte `anchor - 159`. Put the anchor at 318 and
        // a 3-byte em dash at [158..161) and that byte is the middle of
        // the dash — not a char boundary.
        let text = format!(
            "{}—{}q{}",
            "a".repeat(158),
            "c".repeat(157),
            "b".repeat(200)
        );
        assert_eq!(text.find('q'), Some(318), "fixture must anchor at 318");
        assert!(
            !text.is_char_boundary(318 - 159),
            "fixture must put the lead mid-character"
        );
        let matches = vec![wirk_atlas::TermMatch {
            offset: 318,
            len: 1,
            term: "q".to_string(),
        }];

        let window = evidence_window(&text, &matches, ASSEMBLY_SUMMARY_BYTES)
            .expect("text longer than the budget with a match must produce a window");

        assert!(
            text.is_char_boundary(window.start) && text.is_char_boundary(window.end),
            "window [{}, {}) must land on char boundaries",
            window.start,
            window.end
        );
        assert!(
            window.end - window.start <= ASSEMBLY_SUMMARY_BYTES,
            "window must not exceed the budget: got {} bytes",
            window.end - window.start
        );
        assert!(
            window.start <= 318 && window.end >= 319,
            "window [{}, {}) must not cut the anchor away",
            window.start,
            window.end
        );
    }

    #[test]
    fn evidence_window_honours_its_contract_over_multibyte_prose_at_every_budget() {
        // One paragraph of real multi-byte prose, so em dashes, curly
        // quotes and accents fall at many different byte offsets, and
        // one committed line break so line snapping is exercised too.
        let prose = "Le rapport — écrit à Genève — dit « la preuve n'est pas la promesse ».\n                     Une décision porte sa portée : elle expire par elle, jamais par décret.\n                     Ce qui est écrit — même à contrecœur — reste ce qui était connu alors.\n";
        let text = prose.repeat(6);

        // Every char boundary in the text is a candidate anchor, and the
        // budgets sweep through the real one.
        for anchor in (0..text.len()).filter(|at| text.is_char_boundary(*at)) {
            let len = text[anchor..].chars().next().map_or(1, |c| c.len_utf8());
            let matches = vec![wirk_atlas::TermMatch {
                offset: anchor as u64,
                len: len as u64,
                term: text[anchor..anchor + len].to_string(),
            }];
            for budget in [
                1usize,
                2,
                3,
                7,
                64,
                159,
                160,
                161,
                ASSEMBLY_SUMMARY_BYTES,
                321,
            ] {
                let Some(window) = evidence_window(&text, &matches, budget) else {
                    continue;
                };
                assert!(
                    text.is_char_boundary(window.start),
                    "start {} is not a char boundary (anchor {anchor}, budget {budget})",
                    window.start
                );
                assert!(
                    text.is_char_boundary(window.end),
                    "end {} is not a char boundary (anchor {anchor}, budget {budget})",
                    window.end
                );
                assert!(
                    window.start <= window.end,
                    "window [{}, {}) is inverted (anchor {anchor}, budget {budget})",
                    window.start,
                    window.end
                );
                assert!(
                    window.end - window.start <= budget,
                    "window [{}, {}) exceeds budget {budget} (anchor {anchor})",
                    window.start,
                    window.end
                );
                if budget >= len {
                    assert!(
                        window.start <= anchor && window.end > anchor,
                        "window [{}, {}) cuts the anchor {anchor} away (budget {budget})",
                        window.start,
                        window.end
                    );
                } else {
                    // No bounded window can carry even the anchor's first
                    // character; the window still begins at the anchor.
                    assert_eq!(
                        (window.start, window.end),
                        (anchor, anchor),
                        "a budget under the anchor's own character must not wander (budget {budget})"
                    );
                }
                if window.whole_match_shown {
                    assert!(
                        window.end >= anchor + len,
                        "window [{}, {}) claims the whole match but cuts it (anchor {anchor}, budget {budget})",
                        window.start,
                        window.end
                    );
                }
            }
        }
    }
}

// ---- W-C1: stage projection assembly --------------------------------------
//
// One entry point (`prepare_projection`), one observe/assemble/re-check
// wrapper (`prepared_for_reservation`), and the four reservation sites
// that call it. Everything here obeys three rules, each of them a
// ruling rather than a preference:
//
// * **No journal guard is held while a projection is assembled**
//   (0119, and 0124's restatement). Assembly reads the Atlas and the
//   filesystem; the reserving append happens afterwards, under the
//   guard, and re-derives its own authority there.
// * **A failed re-check never becomes a stale reservation** (0124). A
//   prepared projection is used only if the Waypoint it was assembled
//   for is still the Waypoint being reserved; otherwise the reservation
//   proceeds with an explicitly degraded projection that claims nothing.
// * **Reservation is never an availability risk** (BUILD.md §4.6). The
//   estate publishing under the assembler costs laps, then honesty —
//   never a refused stage.

/// How many unresolved references one assembly *lists* before reporting
/// a count instead.
///
/// This is the only cut left in the assembler, and it is a presentation
/// cut in the strict sense ruling 0124 requires: every authored
/// reference is resolved whatever this number is, an unresolved
/// reference already forces `Partial` before any of them is rendered, and
/// what this hides is reported as an `Omission::OverBudget` with the real
/// total. Moving it changes what is shown and cannot change the coverage
/// state — pinned by
/// `the_unknown_presentation_cut_never_moves_factual_coverage`.
///
/// Its predecessor `ASSEMBLY_TOKEN_MAX = 64` was not that: it cut the
/// reference list *before* resolution, which made a budget decide
/// factual coverage. Removed (ruling 0126, F2).
const ASSEMBLY_UNKNOWN_MAX: usize = 32;
/// How many resources one authored reference may bind. A path token
/// naming a file present in three admitted sources genuinely resolves
/// three times; an identifier occurring in a hundred does not make a
/// hundred of them the reference.
const ASSEMBLY_HITS_PER_REFERENCE: usize = 3;
/// How many ranked candidates an identifier reference examines before
/// giving up on finding a literal occurrence.
const ASSEMBLY_CANDIDATES_PER_REFERENCE: usize = 12;
/// The bounded read behind a resolved path, and the bounded summary
/// carried in the projection. Neither is a coverage fact.
const ASSEMBLY_LOOKUP_BYTES: u64 = 65_536;
const ASSEMBLY_SUMMARY_BYTES: usize = 320;
/// How many literal occurrences of one authored name are collected
/// before a summary window is chosen around them (ruling 0142). One name
/// is one distinct term, so the window falls on the earliest occurrence
/// whatever this is; it exists so a name occurring thousands of times in
/// one 64 KiB unit cannot turn summarising into a quadratic scan.
const ASSEMBLY_MATCH_SCAN: usize = 64;

/// A projection assembled outside the journal guard, together with the
/// Waypoint it was assembled for and the Atlas publication revision it
/// captured. Both are re-checked before it is used.
struct PreparedProjection {
    waypoint: WaypointId,
    file: wirk_core::ProjectionFile,
    reference: wirk_core::EvidenceProjectionRef,
    publication_revision: u64,
}

impl PreparedProjection {
    /// Writes the file durably and hands back the reference to journal.
    /// The file is fsynced and renamed **before** the referencing event
    /// exists, so a reference never names a file that was not durable
    /// first (BUILD.md §5.1).
    fn commit(
        &self,
        state: &Arc<WirkdState>,
        work_id: &WorkId,
    ) -> Result<wirk_core::EvidenceProjectionRef, (&'static str, String)> {
        match self.file.write_new(&state.estate_root, work_id) {
            Ok(_) => Ok(self.reference.clone()),
            // The rename made it visible; only the directory sync after
            // it failed. The file is there and re-hashes; reporting this
            // as "never wrote" would be false.
            Err(wirk_core::ProjectionWriteError::DurabilityUncertain(_, detail)) => {
                eprintln!("wirkd: {detail}");
                Ok(self.reference.clone())
            }
            Err(error) => Err(("ProjectionUnwritable", error.to_string())),
        }
    }
}

/// sha256 over the journaled Waypoint definitions this Work reserves
/// against: which Route edition produced a projection, carried as
/// content so a reader never has to join against the journal to learn it.
fn route_edition_of(defs: &[WaypointDefinition]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"wirk.route-edition/v1\0");
    hasher.update(serde_json::to_vec(defs).unwrap_or_default());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---- W-C4: consulted recorded learning, and the index it was read from ----
//
// BUILD.md §4.3 step 6 and §6, bounded by rulings 0124, 0135 and 0137.
//
// Two routes to a record and no others:
//
// 1. **this Work's own journal**, folded from the events this assembly
//    already observed — every finding this Work has raised, across every
//    Run, stage and retry of it, because a Work's own record does not
//    expire when a Run does (ruling 0135: "same-Work journaled findings
//    persist across retries"). A new Run's *projection chain* restarts at
//    revision 0; its journal does not restart at all.
//
// 2. **genuinely settled EstateLocal publications**, through
//    `published_row_scoped` — the same four conditions the estate index's
//    own publication route applies, whole-row-or-nothing, reused rather
//    than re-derived.
//
// Route 2 is applied to **every** foreign row, explicitly including rows
// on this requester's own lineage. `handle_atlas_findings` renders a
// lineage row through `finding_row_json_scoped`, which admits asserted
// and applied rows too; a stage projection must not consult those,
// because a reference is not a promotion and an assertion is a recorded
// judgement, not a settlement (`W-B-LATER-WORK-ADJUDICATION`). So the
// selector here is publication and only publication, and a lineage row
// that has not settled is counted like any other inadmissible row.
//
// Everything in here runs with **no journal guard and no Atlas guard
// held**: `published_row_scoped` reaches `review_targets_admitted` and
// `admit_evidence`, both of which take the Atlas mutex, which is not
// reentrant (BUILD.md §4.2).

/// The requester a `submit`-time assembly scopes by: the bindings, and
/// deliberately nothing else.
///
/// Not a fold of an empty slice — `fold` panics there, correctly, because
/// there is no oracle for a Work that has not been submitted. This is not
/// an oracle either: every other field is the empty value it will in fact
/// hold a moment later, and the only field anything reads is
/// `repositories`.
fn pre_journal_requester(bindings: &[RepositoryBinding]) -> Work {
    Work {
        id: WorkId(String::new()),
        intent: String::new(),
        route: RouteId(String::new()),
        repositories: bindings.to_vec(),
        state: WorkState::Pending,
        current_waypoint: None,
        last_activity: Timestamp(0),
        needs_input: None,
        parent: None,
        held: None,
        activations: Vec::new(),
        execution_repo: None,
        execution_identity: None,
        findings: BTreeMap::new(),
        settlement_ready: Vec::new(),
    }
}

/// What one consultation step produced.
struct ConsultedSet {
    findings: Vec<wirk_core::ConsultedFinding>,
    note: wirk_core::FindingsIndexNote,
    /// Rows this requester may not be shown at all. A count, exactly as
    /// the index surface's own `off_lineage` is.
    inadmissible: usize,
    /// The index could not be read at all, so the consulted set is this
    /// Work's own journal and nothing else.
    unreadable: bool,
}

/// The scoped note for one observed `IndexHealth` — a 1:1 map of the
/// projection state the daemon recorded, with no administrative count,
/// detail or path (ruling 0135 R11, ruling 0124).
fn findings_index_note(health: &IndexHealth) -> wirk_core::FindingsIndexNote {
    wirk_core::FindingsIndexNote {
        state: match health.projection {
            IndexProjection::Unreconciled => wirk_core::FindingsIndexState::Unreconciled,
            IndexProjection::Synchronized => wirk_core::FindingsIndexState::Synchronized,
            IndexProjection::DurabilityUnconfirmed { .. } => {
                wirk_core::FindingsIndexState::DurabilityUnconfirmed
            }
            IndexProjection::Behind { .. } => wirk_core::FindingsIndexState::Behind,
        },
        // The same single field, with the same meaning, that the scoped
        // `atlas findings` reply puts in front of a reader: true for
        // `Synchronized` and nothing else. A preserved unreadable copy
        // already forces the recorded projection off `Synchronized`
        // (`qualified_by_preserved`), and a backing file that has gone
        // away already forces it off here (`qualified_by_absent_index`,
        // inside `read_findings_with_health`) — so this reads the
        // qualified record rather than re-deciding the policy.
        complete: health.complete(),
    }
}

/// The `(membership, generation)` pairs one frozen evidence entry names,
/// and the coordinate it names them at.
///
/// Only a `Source` reference admitted at raise time has any: a `Journal`
/// or `Finding` reference names a journal record, not source bytes, and
/// an entry recorded `Unavailable` names an outcome, not an identity.
/// Neither is padded with a zero value to make this function total.
fn recorded_source_identity(
    item: &AdmittedEvidence,
) -> Option<(wirk_atlas::ExactCoordinate, String, String)> {
    let EvidenceRef::Source(encoded) = &item.reference else {
        return None;
    };
    let EvidenceOutcome::Admitted {
        generation,
        object_id,
    } = &item.outcome
    else {
        return None;
    };
    let coordinate = decode_coordinate(encoded).ok()?;
    Some((coordinate, generation.clone(), object_id.clone()))
}

/// Everything one consulted record says about generations, decided
/// against the vector **this assembly captured** and nothing else.
///
/// Three separate facts, and this computes exactly one of them: the
/// relation. The record's settlement standing is its own field, and
/// whether its evidence still resolves is a third — collapsing any two
/// is what ruling 0135 refuses.
fn generation_relation(
    recorded: &[(String, String)],
    captured: &BTreeMap<String, String>,
) -> wirk_core::GenerationRelation {
    let mut seen_captured = false;
    for (membership, generation) in recorded {
        let Some(current) = captured.get(membership) else {
            continue;
        };
        seen_captured = true;
        if current != generation {
            // One membership published here at another generation is
            // enough to say the pair differs, and it says nothing at all
            // about whether the change affects this claim.
            return wirk_core::GenerationRelation::RecordedSuperseded;
        }
    }
    if seen_captured {
        wirk_core::GenerationRelation::RecordedStillPublished
    } else {
        // Including the ordinary case: a record whose evidence is
        // journal-side and names no source generation at all. Unknown,
        // never "still published".
        wirk_core::GenerationRelation::Unknown
    }
}

/// The typed disagreements one record contributes: a `contradicts` entry
/// naming a coordinate **this projection actually delivered**, and
/// nothing else.
///
/// Two gates, both required. The entry must pass the *current*
/// requester's own disclosure view — raise-time admission is frozen
/// provenance and is not transferable — and the coordinate must already
/// be in `bound`, so naming it here discloses nothing this document did
/// not already deliver. No prose is read, matched or compared, and
/// neither side is endorsed or invalidated by the other.
fn consulted_contradictions(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    finding: &Finding,
    bound_coordinates: &HashSet<&str>,
) -> Vec<wirk_core::Contradiction> {
    let mut out = Vec::new();
    for item in &finding.contradicts {
        let EvidenceRef::Source(encoded) = &item.reference else {
            continue;
        };
        if !bound_coordinates.contains(encoded.as_str()) {
            continue;
        }
        if !view.admits_evidence(state, item) {
            continue;
        }
        out.push(wirk_core::Contradiction {
            coordinate: encoded.clone(),
            text: format!(
                "a consulted record ({}) names this delivered coordinate in its own \
                 `contradicts` list. That is the typed reference its author recorded and \
                 nothing more: no prose was read or compared here, the record is not made true \
                 by disagreeing, and the coordinate is not made false by being disagreed with.",
                finding.id.0
            ),
        });
    }
    out
}

/// The evidence half of one consulted record: which recorded
/// coordinates this document delivers as identity, and honest counts for
/// the rest.
///
/// A coordinate is delivered only when **both** hold: the current
/// requester's disclosure view admits the entry, and this assembly's own
/// admission step captured that membership at exactly the generation the
/// entry was recorded against. The second is what makes consulting a
/// record not a read-through: a Work's own finding may name evidence in a
/// source the *stage* was not oriented to, and the stage does not acquire
/// it by having recorded it (BUILD.md §4.1). No bytes are read here at
/// all — the coordinate is what an actor resolves for themselves.
fn consulted_evidence(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    entries: &[AdmittedEvidence],
    captured: &BTreeMap<String, String>,
) -> (Vec<wirk_core::ConsultedEvidence>, usize, usize) {
    let mut delivered = Vec::new();
    let mut withheld = 0usize;
    let mut not_delivered = 0usize;
    for item in entries {
        if !view.admits_evidence(state, item) {
            withheld += 1;
            continue;
        }
        let Some((coordinate, generation, object_id)) = recorded_source_identity(item) else {
            not_delivered += 1;
            continue;
        };
        if captured.get(&coordinate.membership.0) != Some(&generation) {
            not_delivered += 1;
            continue;
        }
        let EvidenceRef::Source(encoded) = &item.reference else {
            not_delivered += 1;
            continue;
        };
        delivered.push(wirk_core::ConsultedEvidence {
            coordinate: encoded.clone(),
            generation,
            object_id,
        });
    }
    (delivered, withheld, not_delivered)
}

/// The `(membership, generation)` pairs a record was raised against, in
/// recorded order, deduplicated — taken from the record's own frozen
/// evidence outcomes, never re-resolved against today's estate.
fn recorded_generations_of(finding: &Finding) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for item in &finding.evidence {
        let Some((coordinate, generation, _)) = recorded_source_identity(item) else {
            continue;
        };
        let pair = (coordinate.membership.0.clone(), generation);
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

/// Step 6 (BUILD.md §4.3, §6): this Work's own recorded findings and the
/// estate's genuinely settled publications, plus the actual scoped health
/// of the index the second half was read from — one observation, frozen
/// together.
///
/// No guard of any kind is held on entry and none is taken across a
/// helper that takes the Atlas mutex.
fn consult_findings(
    state: &Arc<WirkdState>,
    events: &[Event],
    bindings: &[RepositoryBinding],
    generations: &[(String, String)],
    bound: &[wirk_core::EvidenceItem],
) -> ConsultedSet {
    no_journal_guard_held("consulted findings assembly");

    let captured: BTreeMap<String, String> = generations.iter().cloned().collect();
    let bound_coordinates: HashSet<&str> =
        bound.iter().map(|item| item.coordinate.as_str()).collect();

    // The requester, from the events this assembly already observed.
    //
    // At `submit` there is no journal at all yet — the reservation is
    // being prepared *before* the first event is appended — and `fold`
    // is explicit that a Work exists only from its own `WorkSubmitted`
    // onward. So the pre-journal requester is built from the one thing
    // that does exist: the bindings the submit named, which are exactly
    // what `resolve_query_scope` would derive from the `WorkSubmitted`
    // about to be written. It has no id, so it is on nobody's lineage
    // and matches no row's origin, and it carries no findings, so route
    // 1 is legitimately empty — which is the literal truth for a Work
    // that has not run a stage yet.
    let mut requester = if events.is_empty() {
        pre_journal_requester(bindings)
    } else {
        fold(events)
    };
    requester.repositories = bindings.to_vec();
    let lineage = lineage_of(state, &requester, events);
    let mut view = DisclosureView::new(&requester, events, &lineage);

    let mut findings: Vec<wirk_core::ConsultedFinding> = Vec::new();

    // Route 1: this Work's own journal, in FindingId order — `fold`
    // keeps them in a `BTreeMap`, so delivery order is the record's own
    // identity and not iteration luck.
    for (id, record) in &requester.findings {
        let finding = &record.finding;
        let recorded_generations = recorded_generations_of(finding);
        let current_generations: Vec<(String, String)> = recorded_generations
            .iter()
            .filter_map(|(membership, _)| {
                captured
                    .get(membership)
                    .map(|current| (membership.clone(), current.clone()))
            })
            .collect();
        let (evidence, evidence_withheld, evidence_not_delivered) =
            consulted_evidence(state, &mut view, &finding.evidence, &captured);
        let status = match &record.state {
            FindingState::Proposed => wirk_core::ConsultedStatus::Provisional,
            FindingState::Settled(settlement) => wirk_core::ConsultedStatus::Settled {
                class: settlement_class_str(settlement.authority.class).to_string(),
            },
        };
        // A later record of this same Work naming this one is a journal
        // fact this Work owns on both ends, and it is said here rather
        // than the older record silently disappearing: history is
        // delivered, not erased (ruling 0135).
        let superseded_by = requester
            .findings
            .values()
            .find(|later| later.finding.supersedes.as_ref() == Some(id))
            .map(|later| later.finding.id.0.clone());
        let status = match superseded_by {
            Some(by) => wirk_core::ConsultedStatus::Superseded { by },
            None => status,
        };
        findings.push(wirk_core::ConsultedFinding {
            id: id.0.clone(),
            origin: wirk_core::ConsultedOrigin::OwnWork,
            work: finding.work.0.clone(),
            kind: finding_kind_str(finding.kind).to_string(),
            claim: format!("recorded claim: {}, unverified", finding.claim),
            claim_verified: false,
            status,
            generation_relation: generation_relation(&recorded_generations, &captured),
            recorded_generations,
            current_generations,
            contradictions: consulted_contradictions(state, &mut view, finding, &bound_coordinates),
            evidence,
            evidence_withheld,
            evidence_not_delivered,
            reason: format!(
                "this Work's own record, raised by its Run {} at Waypoint {}. It reaches this \
                 stage because the journal that holds it is this Work's, not because anything \
                 verified it.",
                finding.run.0, finding.waypoint.0
            ),
        });
    }

    // Route 2: the estate's genuinely settled publications.
    let (rows, health) = match read_findings_with_health(state) {
        Ok(pair) => pair,
        Err(_) => {
            // The error's own `Display` carries a filesystem path no
            // scope admitted, so none of it travels: the state is
            // `Unreadable` and that is the whole disclosure (BUILD.md
            // §9). What this Work's own journal holds is untouched.
            return ConsultedSet {
                findings,
                note: wirk_core::FindingsIndexNote {
                    state: wirk_core::FindingsIndexState::Unreadable,
                    complete: false,
                },
                inadmissible: 0,
                unreadable: true,
            };
        }
    };
    let mut inadmissible = 0usize;
    let mut published: Vec<(String, wirk_core::ConsultedFinding)> = Vec::new();
    for row in &rows {
        if row.origin.work == requester.id {
            // Already delivered from the journal that owns it, with its
            // real status — the index is a derived projection of that
            // journal, never a second, competing copy of it.
            continue;
        }
        // The explicit selector: publication, and only publication, for
        // every foreign row including one on this requester's lineage.
        let Some(rendered) = published_row_scoped(state, &mut view, row) else {
            inadmissible += 1;
            continue;
        };
        // Belt and braces on the contract this route already promises:
        // a row that came back with any part withheld is not published
        // in halves, and a projection is the last place to discover that
        // a future field slipped through.
        if json_contains_withheld(&rendered) {
            inadmissible += 1;
            continue;
        }
        let finding = &row.finding;
        let recorded_generations = published_recorded_generations(state, &mut view, row);
        let current_generations: Vec<(String, String)> = recorded_generations
            .iter()
            .filter_map(|(membership, _)| {
                captured
                    .get(membership)
                    .map(|current| (membership.clone(), current.clone()))
            })
            .collect();
        let class = row
            .settlement
            .as_ref()
            .map(|settlement| settlement_class_str(settlement.authority.class).to_string())
            .unwrap_or_else(|| "unrecorded".to_string());
        let status = match &row.superseded_by {
            Some(by) => wirk_core::ConsultedStatus::Superseded { by: by.0.clone() },
            None => wirk_core::ConsultedStatus::Settled { class },
        };
        published.push((
            row.id.0.clone(),
            wirk_core::ConsultedFinding {
                id: finding.id.0.clone(),
                origin: wirk_core::ConsultedOrigin::EstatePublication,
                work: row.origin.work.0.clone(),
                kind: finding_kind_str(finding.kind).to_string(),
                claim: format!("recorded claim: {}, unverified", finding.claim),
                claim_verified: false,
                status,
                generation_relation: generation_relation(&recorded_generations, &captured),
                recorded_generations,
                current_generations,
                contradictions: consulted_contradictions(
                    state,
                    &mut view,
                    finding,
                    &bound_coordinates,
                ),
                // A published row's own frozen evidence entries are not
                // part of what the publication route vouched for — it
                // renders identity, settlement and application, not the
                // raiser's evidence list — so none of them is delivered
                // as a coordinate here. Counted, exactly as a withheld
                // part is, rather than rendered through a gate that was
                // never asked about them.
                evidence: Vec::new(),
                evidence_withheld: 0,
                evidence_not_delivered: finding.evidence.len(),
                reason: format!(
                    "a settled EstateLocal publication of Work {}, reached through the estate \
                     publication route this requester's own bindings already admit it by. \
                     Settled says a policy-admitted check discharged; it does not say the \
                     recorded sentence is true.",
                    row.origin.work.0
                ),
            },
        ));
    }
    // Deterministic delivery order, and one that is not iteration luck:
    // by the row's own content-addressed id. Two rows for one finding
    // (an assertion and a later application legitimately coexist) do not
    // collapse into one record here, because only `Settled` rows reach
    // this list at all — but a re-minted identical settlement would, and
    // is deduplicated by the finding's own identity, keeping the first.
    published.sort_by(|a, b| a.0.cmp(&b.0));
    let mut seen: HashSet<String> = findings.iter().map(|item| item.id.clone()).collect();
    for (_, item) in published {
        if seen.insert(item.id.clone()) {
            findings.push(item);
        }
    }

    ConsultedSet {
        findings,
        note: findings_index_note(&health),
        inadmissible,
        unreadable: false,
    }
}

/// Whether any part of a rendered row is the one shape a withheld part
/// takes. Structural, so it cannot be fooled by a claim that happens to
/// contain the word.
fn json_contains_withheld(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map.len() == 1 && map.get("withheld") == Some(&Value::Bool(true)) {
                return true;
            }
            map.values().any(json_contains_withheld)
        }
        Value::Array(items) => items.iter().any(json_contains_withheld),
        _ => false,
    }
}

/// The `(membership, generation)` pairs a published row was settled
/// against.
///
/// An `ActorReview` names them directly: the frozen review targets,
/// whose exact memberships `published_row_scoped` has already admitted
/// under this requester's own scope (`review_targets_admitted`). That
/// behavior is unchanged.
///
/// Every other settlement shape's own check fields name no generation at
/// all — a `ChildReceipt`'s `ChildProof` carries an obligation and a
/// confirming `FindingId`, never a source coordinate (ROOT-CURRENTNESS-
/// CAUSE.md). What such a row can still carry is the underlying
/// `Finding`'s own admitted source evidence — `FindingRow` stores the
/// complete `Finding`, evidence included — raised and confirmed under
/// the frozen policy at RECOVERY-ACCEPTANCE.md. Using it here derives
/// only a generation *identity*, gated by this requester's own current
/// disclosure view exactly as `consulted_contradictions` gates a
/// coordinate: an entry the view does not admit contributes nothing.
/// This never delivers the evidence coordinate or object id themselves —
/// the publication route's deliberate omission of authored evidence
/// content is unchanged — and a `ValidatedClaim` or `SupersededBy` row
/// whose evidence is journal-side, or whose evidence this requester is
/// not admitted to, still names no source generation, reported as the
/// `Unknown` relation rather than invented.
fn published_recorded_generations(
    state: &Arc<WirkdState>,
    view: &mut DisclosureView,
    row: &wirk_atlas::FindingRow,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let Some(settlement) = &row.settlement else {
        return out;
    };
    if let SettlementCheck::ActorReview { proof, .. } = &settlement.check {
        for target in &proof.targets {
            let pair = (target.membership.clone(), target.generation.clone());
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
        return out;
    }
    for item in &row.finding.evidence {
        if !view.admits_evidence(state, item) {
            continue;
        }
        let Some((coordinate, generation, _)) = recorded_source_identity(item) else {
            continue;
        };
        let pair = (coordinate.membership.0.clone(), generation);
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

/// The projection a reservation falls back to when the estate moved
/// under the assembler often enough that it stopped re-observing, or
/// when a prepared projection turns out to have been assembled for a
/// Waypoint that is no longer the one being reserved.
///
/// Pure: it takes no lock of any kind, which is exactly why it is
/// always available under the commit guard. It claims nothing — no
/// generations, no bound evidence, an explicit `Degraded` coverage and
/// an assumption naming what happened. The stage runs and the actor is
/// told the assembler lost the race, which is the honest outcome; a
/// stale or invented projection would not be.
fn degraded_projection(
    def: &WaypointDefinition,
    orient: &wirk_core::OrientationRequest,
    route_edition: &str,
    span: ObservationSpan,
    cause: DegradedCause,
) -> PreparedProjection {
    let content = wirk_core::ProjectionContent {
        format: wirk_core::PROJECTION_FORMAT.to_string(),
        compilation_policy: wirk_core::ASSEMBLY_POLICY.to_string(),
        route_edition: route_edition.to_string(),
        waypoint: def.id.clone(),
        revision: 0,
        question: orient.question.clone(),
        generations: Vec::new(),
        publication_revision: 0,
        // No query ran, and the note says exactly that rather than
        // presenting a lexical default that never happened.
        retrieval: wirk_core::RetrievalNote {
            mode: "none".to_string(),
            semantic: "disabled".to_string(),
            semantic_reason: Some(
                "no ranked query ran: this assembly took no source snapshot to rank over"
                    .to_string(),
            ),
            editions: Vec::new(),
            degraded: vec!["no_snapshot".to_string()],
            total_candidates: 0,
            returned: 0,
            capacity: None,
        },
        bound: Vec::new(),
        // Nothing was observed, so nothing is claimed about what the
        // estate holds: an empty consulted list beside an `Unobserved`
        // note says the assembler never looked, which is a different
        // fact from "there is nothing there".
        consulted: Vec::new(),
        findings_index: wirk_core::FindingsIndexNote::unobserved(),
        referenced: Vec::new(),
        reachable: Vec::new(),
        assumptions: vec![wirk_core::Statement {
            text: cause.text().to_string(),
            attributed_to: wirk_core::StatementOrigin::Assembly,
        }],
        unknowns: Vec::new(),
        omitted: Vec::new(),
        next_action: next_action_for(
            wirk_core::EvidenceCoverage::Degraded {
                reason: wirk_core::CoverageReason::ConcurrentPublication,
            },
            true,
        ),
        coverage: wirk_core::EvidenceCoverage::Degraded {
            reason: wirk_core::CoverageReason::ConcurrentPublication,
        },
        truncated: false,
        // A degraded projection is an initial delivery that bound
        // nothing; it expands nothing.
        expansion: None,
    };
    finish_projection(content, span)
}

/// What the assembler actually observed, carried to the receipt. Two
/// numbers that always travel together, and neither is ever a constant
/// standing in for a measurement: the first candidate's degraded receipt
/// reported `observation_window_ms: 0` for an assembly that really spent
/// eight observation laps, and the receipt is now integrity-covered, so
/// a fabricated span would be a signed falsehood rather than a slip
/// (ruling 0126, F1).
#[derive(Debug, Clone, Copy)]
struct ObservationSpan {
    laps: u32,
    window_ms: u64,
}

impl ObservationSpan {
    /// The span an observation loop actually spent, from the instant it
    /// began to now.
    fn measured(laps: u32, started: std::time::Instant) -> Self {
        Self {
            laps,
            window_ms: started.elapsed().as_millis() as u64,
        }
    }

    /// No observation lap ran at all. Used only where that is the
    /// literal truth: a reservation site that reaches the commit guard
    /// with no preparation to use.
    fn none() -> Self {
        Self {
            laps: 0,
            window_ms: 0,
        }
    }
}

/// Why a degraded projection is being minted. Both are races and both
/// bind nothing; they are not the same event, and a receipt that now
/// carries a checked lap count must not describe one as the other.
#[derive(Debug, Clone, Copy)]
enum DegradedCause {
    /// Every observation lap lost to a publish under the assembler.
    PublicationChurn,
    /// The reservation reached the commit guard with no projection
    /// prepared for the Waypoint it is actually reserving — either none
    /// was prepared, or the one prepared was assembled for a different
    /// Waypoint and was discarded (ruling 0124).
    PreparationDiscarded,
}

impl DegradedCause {
    fn text(self) -> &'static str {
        match self {
            Self::PublicationChurn => {
                "no source snapshot was taken: the estate's published sources changed under this \
                 assembly on every observation attempt, so this projection reports no generation \
                 vector and binds no evidence. Nothing here is a statement that the estate holds \
                 nothing."
            }
            Self::PreparationDiscarded => {
                "no source snapshot was taken: the Waypoint being reserved under the commit guard \
                 is not the one any prepared assembly was made for, so nothing prepared was \
                 attached and this projection reports no generation vector and binds no evidence. \
                 Nothing here is a statement that the estate holds nothing."
            }
        }
    }
}

/// Mints the observation, builds the receipt from the **measured** span,
/// and derives the journal reference from both — the receipt first, so
/// its digest is taken over the bytes that are actually written rather
/// than over a value assembled twice.
fn finish_projection(
    content: wirk_core::ProjectionContent,
    span: ObservationSpan,
) -> PreparedProjection {
    let observation = wirk_core::ObservationId(mint_id("obs"));
    let receipt = wirk_core::ObservationReceipt {
        observation: observation.clone(),
        observed_at: now_ts().0.max(0) as u64,
        observation_window_ms: span.window_ms,
        laps: span.laps,
    };
    let reference = wirk_core::EvidenceProjectionRef {
        observation,
        projection: content.projection_id(),
        revision: content.revision,
        format: content.format.clone(),
        receipt: receipt.digest(),
    };
    let waypoint = content.waypoint.clone();
    let publication_revision = content.publication_revision;
    PreparedProjection {
        waypoint,
        publication_revision,
        file: wirk_core::ProjectionFile {
            content: wirk_core::DeliveredContent::V3(Box::new(content)),
            receipt,
        },
        reference,
    }
}

/// One authored reference, classified by its own shape.
///
/// The grammar is deliberately narrow and deliberately *not* a
/// judgement: a token that looks like a path or an identifier is looked
/// up, and an ordinary prose word is neither looked up nor reported as
/// an unknown fact (ruling 0124: "ordinary prose words are not
/// automatically unknown facts"). An unresolved *reference* is an
/// unknown; the word "boundary" in a sentence is not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Reference {
    Path(String),
    Identifier(String),
}

impl Reference {
    fn text(&self) -> &str {
        match self {
            Reference::Path(text) | Reference::Identifier(text) => text,
        }
    }
}

/// What an unresolved reference actually establishes — which is not the
/// same observation for the two reference kinds, and ruling 0127 rejects
/// saying it is.
///
/// A **path** is resolved by exact lookup: `resolve_path_reference` walks
/// every admitted source's own captured generation manifest and compares
/// the recorded path bytes. Nothing is ranked, nothing is sampled, and
/// nothing is examined-up-to-a-limit, so "no admitted source records this
/// path at the captured generations" is a complete statement about the
/// captured manifests, and the projection may make it.
///
/// An **identifier** is resolved by ranked candidate discovery over the
/// lexical index, then a literal byte check on the candidates that come
/// back. That establishes exactly one thing: no candidate this assembly
/// examined contained the name literally. It does **not** establish that
/// the bytes do not occur in the admitted sources, and the executed case
/// is why the distinction is written into the product rather than into a
/// comment: `embedded_marker` inside the indexed token
/// `wrapper_embedded_marker_tail` returns zero candidates
/// (`loop-c1-reverify/raw/15`, `raw/16` — `total_candidates: 0` beside a
/// `grep` proving the bytes are there), because the index tokenizes on
/// non-alphanumerics and `_`, so a name embedded in a longer token is
/// never its own term. The first wording called that "resolves to
/// nothing in the admitted sources", which reads as byte absence the
/// assembler never checked.
fn unresolved_statement(reference: &Reference) -> String {
    match reference {
        Reference::Path(path) => format!(
            "the authored text names the path `{path}`, which no admitted source records at the \
             captured generations: every admitted source's own captured generation was examined \
             and none carries this path. Whether it exists elsewhere, is misspelled, or was never \
             there is not decided here."
        ),
        Reference::Identifier(name) => format!(
            "the authored text names the identifier `{name}`, which was not found among the \
             indexed candidates this assembly examined at the captured generations. That is what \
             bounded, ranked candidate discovery returned; it is not a statement that these bytes \
             are absent from the admitted sources, because a name that occurs only inside a \
             longer indexed token is never itself a candidate. Whether it exists elsewhere, is \
             misspelled, or was never there is not decided here."
        ),
    }
}

/// Splits authored text into candidate tokens and keeps the ones whose
/// own shape makes them a reference.
///
/// Deduplicated, and returned in **first-appearance order**: delivery
/// order is part of the projection's fingerprint, and the order an
/// author wrote their references in is the one order that is not an
/// invention.
fn authored_references(texts: &[&str]) -> Vec<Reference> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for text in texts {
        for raw in text.split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '(' | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '<'
                        | '>'
                        | '"'
                        | '\''
                        | '`'
                        | ','
                        | ';'
                        | '|'
                )
        }) {
            for token in split_qualified(raw) {
                let Some(reference) = classify_reference(&token) else {
                    continue;
                };
                if seen.insert(reference.clone()) {
                    out.push(reference);
                }
            }
        }
    }
    out
}

/// `WorldHash::of` names two references, not one: a Rust path is a
/// qualified name whose segments are each resolvable, and treating the
/// whole string as one identifier would resolve neither.
fn split_qualified(raw: &str) -> Vec<String> {
    let trimmed = raw
        .trim_matches(|c: char| matches!(c, '.' | ',' | ':' | ';' | '!' | '?' | '*' | '#' | '-'));
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.contains("::") {
        return trimmed
            .split("::")
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect();
    }
    vec![trimmed.to_string()]
}

fn classify_reference(token: &str) -> Option<Reference> {
    if token.len() < 3 || token.len() > 200 {
        return None;
    }
    // `server.rs:12326` — a path with a line citation. The citation is
    // presentation; the path is the reference.
    let head = token.split(':').next().unwrap_or(token);
    if head.contains('/') && head.bytes().all(is_path_byte) {
        return Some(Reference::Path(head.trim_start_matches('/').to_string()));
    }
    if head.bytes().all(is_path_byte)
        && let Some((stem, extension)) = head.rsplit_once('.')
        && !stem.is_empty()
        && (1..=6).contains(&extension.len())
        && extension.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Some(Reference::Path(head.to_string()));
    }
    // An identifier: a Rust/C-shaped name that an author would not have
    // written by accident. `_` or an internal capital is what separates
    // `reserve_next_leaf` and `WorldHash` from `the` and `boundary`.
    let identifier = token;
    let mut bytes = identifier.bytes();
    let first = bytes.next()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    if !identifier
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    let has_underscore = identifier.contains('_');
    let has_internal_capital = identifier.bytes().skip(1).any(|b| b.is_ascii_uppercase());
    (has_underscore || has_internal_capital).then(|| Reference::Identifier(identifier.to_string()))
}

fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
}

fn bounded_summary(bytes: &[u8]) -> String {
    let mut cap = ASSEMBLY_SUMMARY_BYTES.min(bytes.len());
    while cap > 0 && std::str::from_utf8(&bytes[..cap]).is_err() {
        cap -= 1;
    }
    String::from_utf8_lossy(&bytes[..cap]).replace(['\n', '\r'], " ")
}

/// The same bounded summary, taken from where a match actually is, with
/// the exact coordinate of the bytes it was taken from (ruling 0142).
///
/// R2 exactly: the window is `evidence_window` — the one the search
/// reply already chooses its displayed bytes with — run at the
/// assembler's own budget, and the span is `window_coordinate`, built
/// through the resolver's own line arithmetic. No second tokenizer, no
/// second query language, no separate relevance rule; the only
/// difference is the budget, which belongs to the presentation layer
/// that owns it.
///
/// `None` means nothing located anything inside this text — a
/// semantically ranked row, or a resource that already fits the budget
/// whole — and the caller falls back to the head of it. An item no term
/// match put where it is therefore never acquires invented match terms
/// and never names a narrower span.
fn local_summary(
    whole: &wirk_atlas::ExactCoordinate,
    text: &str,
    matches: &[wirk_atlas::TermMatch],
) -> Option<(String, wirk_core::ShownEvidence)> {
    let window = evidence_window(text, matches, ASSEMBLY_SUMMARY_BYTES)?;
    let coordinate = window_coordinate(whole, text, &window)?;
    Some((
        bounded_summary(&text.as_bytes()[window.start..window.end]),
        wirk_core::ShownEvidence {
            coordinate: encode_coordinate(&coordinate),
            byte_start: coordinate.byte_start,
            byte_end: coordinate.byte_end,
            line_start: coordinate.line_start,
            line_end: coordinate.line_end,
            matched_terms: window.matched_terms,
            whole_match_shown: window.whole_match_shown,
        },
    ))
}

/// Where an authored name literally occurs in a candidate's text, as
/// term locations the shared window can be chosen around.
///
/// This is the *same* literal containment test that admits the candidate
/// in the first place, reported with its offsets instead of thrown away
/// — not a second matcher, and nothing that could admit a resource the
/// old test would not. Bounded by `ASSEMBLY_MATCH_SCAN`.
fn literal_matches(text: &str, name: &str) -> Vec<wirk_atlas::TermMatch> {
    text.match_indices(name)
        .take(ASSEMBLY_MATCH_SCAN)
        .map(|(offset, found)| wirk_atlas::TermMatch {
            offset: offset as u64,
            len: found.len() as u64,
            term: name.to_string(),
        })
        .collect()
}

/// Assembles one stage projection: BUILD.md §4.3 steps 1-3, and no
/// others.
///
/// Step 1, **admit**: the scope is `QueryScope::Work` over the Work's own
/// journaled bindings, never a client-supplied grant set. `orient.sources`
/// is *intersected* with those bindings; an alias the Work never bound is
/// a count-only `Omission::Inadmissible` and no lookup at all — the
/// projection never states whether such a source exists.
///
/// Step 2, **capture**: one Atlas window takes the admitted memberships,
/// each one's currently published generation and the store's publication
/// revision. Every later read in this assembly pins to that vector, so
/// every coordinate the projection reports resolves at the generation the
/// projection names it at. The Atlas guard is held across the read phase
/// and no journal guard is held anywhere in it; nothing called here takes
/// the Atlas lock again, so the non-reentrant `Mutex` is never re-entered
/// (the reentrancy BUILD.md §4.2 warns about arrives with the
/// consulted-findings step, which this wave does not implement).
///
/// Step 3, **resolve literal references**: path-shaped and
/// identifier-shaped tokens out of the authored question and the
/// Waypoint's own intent, each resolved through pinned exact path lookup
/// and pinned exact search whose hits are verified to contain the token
/// literally. A reference that resolves nowhere becomes an `unknowns`
/// entry attributed to `Intent`. **This is the whole premise mechanism.**
/// The assembler reports what it could not find. It never concludes the
/// premise is false, never scores the intent and emits no judgement.
fn prepare_projection(
    state: &Arc<WirkdState>,
    events: &[Event],
    bindings: &[RepositoryBinding],
    def: &WaypointDefinition,
    route_edition: &str,
    laps: u32,
    started: std::time::Instant,
) -> Option<PreparedProjection> {
    let orient = def.orient.as_ref()?;
    no_journal_guard_held("stage projection assembly");

    let mut bound: Vec<wirk_core::EvidenceItem> = Vec::new();
    let mut unknowns: Vec<wirk_core::Statement> = Vec::new();
    let mut omitted: Vec<wirk_core::Omission> = Vec::new();
    let mut assumptions: Vec<wirk_core::Statement> = Vec::new();

    // Step 1: admission, before any content or metadata.
    let scope = wirk_atlas::QueryScope::Work(bindings.to_vec());
    let bound_aliases: HashSet<&str> = bindings
        .iter()
        .map(|binding| binding.name.as_str())
        .collect();
    // One count for everything this requester may not see: an alias the
    // Work never bound, and a governance edge whose far side or evidence
    // lies outside admission. A count, never a coordinate, an alias or an
    // id — a caller learns "there is something here you may not see"
    // without learning what (the shape `AdmissionSummary` and
    // `DisclosureView::withheld` already use). Pushed once, at the end,
    // so one projection carries one such fact.
    let mut inadmissible = orient
        .sources
        .iter()
        .filter(|alias| !bound_aliases.contains(alias.as_str()))
        .count();

    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    // Step 2: one captured vector, one publication revision.
    let publication_revision = atlas.publication_revision();
    let mut admitted: Vec<(wirk_atlas::Membership, wirk_atlas::SourceGeneration)> = Vec::new();
    let mut memberships: Vec<wirk_atlas::Membership> = atlas
        .memberships()
        .filter(|member| bound_aliases.contains(member.alias.as_str()))
        .filter(|member| orient.sources.is_empty() || orient.sources.contains(&member.alias))
        .cloned()
        .collect();
    // Delivery order that is not an invention: membership id, which is
    // stable across restarts and independent of catalog iteration.
    memberships.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    for membership in memberships {
        match atlas.current(&membership) {
            Ok(Some(generation)) => admitted.push((membership, generation)),
            Ok(None) | Err(_) => {
                omitted.push(wirk_core::Omission::Unavailable {
                    coordinate: membership.alias.clone(),
                    reason: wirk_core::UnavailableReason::GenerationUnavailable,
                });
            }
        }
    }
    let pinned: BTreeMap<wirk_atlas::MembershipId, wirk_atlas::GenerationId> = admitted
        .iter()
        .map(|(membership, generation)| (membership.id.clone(), generation.id.clone()))
        .collect();
    let generations: Vec<(String, String)> = admitted
        .iter()
        .map(|(membership, generation)| (membership.id.0.clone(), generation.id.0.clone()))
        .collect();
    // Ruling 0135 C4-R12, the World half of the same fact `atlas
    // search`'s `coverage.source_extraction_incomplete` carries: how many
    // resources of the vector *this assembly captured* the extractor was
    // asked for and could not produce retrieval units for. Those bytes
    // are in the source and in no index, so the corpus this projection
    // describes is short by that many resources.
    //
    // Counted over the admitted memberships only, so a failure in a
    // source this requester was never shown cannot reach the count. Only
    // `Error`: an unsupported family and a deliberately excluded path
    // are the declared shape of the corpus, and counting them would
    // "obscure the map" (ruling 0135's own qualification).
    let unextractable = admitted
        .iter()
        .map(|(_, generation)| {
            generation
                .resources
                .iter()
                .filter(|resource| resource.disposition == wirk_atlas::CoverageDisposition::Error)
                .count()
        })
        .sum::<usize>();

    // Step 3: literal references, resolved at the captured generations.
    let intent = def.intent.clone().unwrap_or_default();
    // Every authored reference, resolved. The set is bounded by the
    // authored input — one question plus one intent, deduplicated — and
    // by nothing else.
    //
    // The first candidate cut this list to 64 *before* resolution and
    // then computed coverage without considering the cut, so 70 authored,
    // indexed, entirely resolvable paths were delivered as 64 bound items
    // with `coverage: complete`. Ruling 0126 rejects both that and the
    // reviewer's proposed repair of calling it `Partial`: a token budget
    // must not decide factual coverage in either direction. What is
    // bounded is the work per reference (`ASSEMBLY_HITS_PER_REFERENCE`,
    // `ASSEMBLY_CANDIDATES_PER_REFERENCE`) and the *presentation* of
    // what could not be resolved (`ASSEMBLY_UNKNOWN_MAX`), never the
    // question of whether a reference was looked up at all.
    let references = authored_references(&[orient.question.as_str(), intent.as_str()]);
    let references = references.as_slice();

    // Identifiers first, in one shared corpus pass; then walk the
    // references in **authored order**, because delivery order is part
    // of the fingerprint and the order an author wrote their references
    // in is the one order that is not an invention.
    let identifiers: Vec<&str> = references
        .iter()
        .filter_map(|reference| match reference {
            Reference::Identifier(name) => Some(name.as_str()),
            Reference::Path(_) => None,
        })
        .collect();
    let mut resolved_identifiers =
        resolve_identifier_references(&atlas, &scope, &admitted, &pinned, &identifiers);

    let mut unresolved: Vec<&Reference> = Vec::new();
    for reference in references {
        let hits = match reference {
            Reference::Path(path) => resolve_path_reference(&admitted, path, &mut omitted),
            Reference::Identifier(name) => resolved_identifiers
                .remove(name.as_str())
                .unwrap_or_default(),
        };
        if hits.is_empty() {
            unresolved.push(reference);
        }
        bound.extend(hits);
    }
    let literal_bound = bound.len();

    // Step 4: governance. Follow the `GovernedBy` edges the estate has
    // actually admitted out of the resources step 3 bound, and keep
    // following them out of what that reaches. The traversal is bounded
    // by a visited set rather than by a depth cap or a count: a cycle
    // terminates because a resource is delivered at most once, and
    // nothing here decides that some number of governing records is
    // enough (ruling 0124: bound is not budgeted).
    let governance = follow_governance(&atlas, &scope, &admitted, &mut bound, &mut omitted);
    inadmissible += governance.filtered;
    let governance_bound = governance.delivered;
    // Governance this estate really admitted about a resource bound
    // here, at an edition this assembly did not capture. Not followed —
    // the bytes it was admitted against are not the bytes delivered
    // here, and reading today's under a historical coordinate is the
    // substituted provenance ruling 0126 refuses. Not silent either:
    // before this, a `GovernedBy` edge disappeared with `coverage:
    // complete`, no count and no unknown the moment any byte anywhere in
    // the governed source changed, so a stage was handed a projection
    // that looked complete with the governing rule missing (ruling 0128
    // F1). A count, exactly as the mirror case — a governing endpoint
    // that no longer resolves — is already a count.
    if governance.admitted_at_another_edition > 0 {
        omitted.push(wirk_core::Omission::AdmittedAtAnotherEdition {
            count: governance.admitted_at_another_edition,
        });
    }

    // Step 7: ranked retrieval for the authored question, and the
    // admitted places this stage may go looking that nothing named.
    // Both are presentation-budgeted; neither can move `coverage`.
    let budget = orient.budget;
    let answer = ranked_answer(
        &atlas,
        &scope,
        &pinned,
        &orient.question,
        budget.referenced(),
        orient.capacity,
        orient.semantic.as_ref(),
    );
    let retrieval = retrieval_note(answer.as_ref(), orient.capacity);
    let referenced: Vec<wirk_core::EvidenceItem> = answer
        .as_ref()
        .map(|answer| {
            answer
                .hits
                .iter()
                .filter_map(|hit| ranked_item(&admitted, hit))
                .collect()
        })
        .unwrap_or_default();
    if retrieval.total_candidates > referenced.len() {
        omitted.push(wirk_core::Omission::OverBudget {
            of: "referenced".to_string(),
            shown: referenced.len(),
            total: retrieval.total_candidates,
        });
    }
    let (reachable, reachable_total) = reachable_entries(&admitted, budget.reachable());
    if reachable_total > reachable.len() {
        omitted.push(wirk_core::Omission::OverBudget {
            of: "reachable".to_string(),
            shown: reachable.len(),
            total: reachable_total,
        });
    }

    // Every Atlas read is done. The prior-stage artifacts below are
    // filesystem reads of this Work's own checkouts and take no Atlas
    // lock, so the guard is dropped here rather than held across them.
    drop(atlas);

    // Step 5: this Work's own already-claimed stages, by exact recorded
    // digest.
    let artifacts =
        bind_prior_stage_artifacts(&state.estate_root, events, def, &mut bound, &mut omitted);

    // Step 6 (§4.3, §6): consulted recorded learning, and the actual
    // scoped health of the index it was read from — deliberately here,
    // after `drop(atlas)`, because `published_row_scoped` reaches
    // helpers that take the Atlas mutex and it is not reentrant
    // (§4.2), and with no journal guard held anywhere in this function.
    let consulted = consult_findings(state, events, bindings, &generations, &bound);
    if consulted.inadmissible > 0 {
        inadmissible += consulted.inadmissible;
    }
    if consulted.unreadable {
        omitted.push(wirk_core::Omission::Unavailable {
            coordinate: "the estate findings index".to_string(),
            reason: wirk_core::UnavailableReason::FindingsIndexUnreadable,
        });
    }

    let shown_unknowns = unresolved.len().min(ASSEMBLY_UNKNOWN_MAX);
    for reference in &unresolved[..shown_unknowns] {
        unknowns.push(wirk_core::Statement {
            text: unresolved_statement(reference),
            attributed_to: wirk_core::StatementOrigin::Intent,
        });
    }
    if unresolved.len() > shown_unknowns {
        omitted.push(wirk_core::Omission::OverBudget {
            of: "unknowns".to_string(),
            shown: shown_unknowns,
            total: unresolved.len(),
        });
    }

    assumptions.push(wirk_core::Statement {
        text: format!(
            "assembled under compilation policy {} over {} admitted source(s), pinned to the \
             generation vector this projection names, at Atlas publication revision {}.",
            wirk_core::ASSEMBLY_POLICY,
            admitted.len(),
            publication_revision
        ),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    assumptions.push(wirk_core::Statement {
        text: format!(
            "references were taken from the authored question and this Waypoint's own intent by \
             literal shape alone: every distinct path-shaped and identifier-shaped token was \
             resolved — {resolved} of them here — binding at most \
             {ASSEMBLY_HITS_PER_REFERENCE} resources per reference. Ordinary prose words are \
             neither resolved nor reported as unknown, and no token is scored, judged or \
             interpreted.",
            resolved = references.len()
        ),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    assumptions.push(wirk_core::Statement {
        text: format!(
            "governance was followed out of the {literal_bound} literally-resolved item(s) \
             through the `GovernedBy` edges this estate has admitted, and out of what those \
             reached, until nothing new was reached — {governance_bound} further item(s). A \
             resource already delivered is not delivered again, and its bytes are not read \
             again, but the relationship that reached it is stated on the item that is already \
             here: deduplicating a resource never drops why it governs another. An edge is \
             followed only where it was admitted against the same generation this assembly \
             captured; an edge recorded against another generation is evidence about bytes this \
             projection is not pinned to and is not followed — {elsewhere} such edge(s) were \
             admitted about a resource delivered here and are reported as a count, which says \
             they exist at an edition this assembly did not capture and says nothing about \
             whether they still hold. Being governing makes an item `Standing`, which is how \
             long what it says stays true and confers no read at all.",
            elsewhere = governance.admitted_at_another_edition
        ),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    assumptions.push(wirk_core::Statement {
        text: format!(
            "{artifacts} artifact(s) of this Work's own already-claimed stages were bound from \
             the current Run of each, by reading the claimed bytes once and hashing those same \
             bytes against the digest the Claim recorded at validation. A rewritten, unreadable \
             or digest-less artifact is reported as unavailable and is not bound: later bytes \
             are never attributed to an earlier Claim, and a path beside a Claim id is not on \
             its own historical evidence identity. This says nothing about the state of that \
             path at any instant after the read."
        ),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    if let Some(reason) = retrieval.semantic_reason.as_deref() {
        assumptions.push(wirk_core::Statement {
            text: format!(
                "ranked retrieval for the question ran in `{}` mode and its semantic status is \
                 `{}`: {reason}",
                retrieval.mode, retrieval.semantic
            ),
            attributed_to: wirk_core::StatementOrigin::Assembly,
        });
    }
    assumptions.push(wirk_core::Statement {
        text: "the `referenced` list is what ranked retrieval returned for the question and the \
               `bound` list is what the authored text named; an item can honestly appear in \
               both, with its own reason in each, and neither list is deduplicated against the \
               other so that every total stated here is the count the query actually reported. \
               A `reachable` entry is an admitted place to look, not a claim about what is in \
               it."
        .to_string(),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    // Written once here and then carried verbatim by every expansion of
    // this chain (`prepare_expansion` clones the parent's assumptions),
    // so it must be true of whichever revision is reading it, not only
    // of the one that wrote it. It said "this is revision 0" as a
    // constant, and revision 1 of a real native Run therefore read as
    // revision 0 of itself (ruling 0135, "expanded projections repeat
    // the revision0 assumption"). The revision a reader is holding is
    // already stated, exactly and per revision, by `reference.revision`;
    // what this sentence is for is the disclosure and the verb, and both
    // are revision-neutral facts.
    assumptions.push(consulted_statement(&consulted));
    if inadmissible > 0 {
        omitted.push(wirk_core::Omission::Inadmissible {
            count: inadmissible,
        });
    }
    if unextractable > 0 {
        omitted.push(wirk_core::Omission::SourceExtractionIncomplete {
            count: unextractable,
        });
    }

    // Coverage comes only from admission denials, unavailability,
    // unresolved references and index health — never from a budget, a
    // truncation, a retry count or a depth (ruling 0044, BUILD.md §4.7).
    let coverage = if consulted.unreadable {
        // Worst first: the consulted set is this Work's own journal and
        // nothing else, and no other reason here is stronger than that.
        wirk_core::EvidenceCoverage::Degraded {
            reason: wirk_core::CoverageReason::FindingsIndexUnreadable,
        }
    } else if omitted
        .iter()
        .any(|item| matches!(item, wirk_core::Omission::Unavailable { .. }))
    {
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::EvidenceUnavailable,
        }
    } else if unextractable > 0 {
        // The captured vector holds a resource nothing could turn into
        // retrieval units. Ranked below `EvidenceUnavailable` — bytes
        // that were indexed and cannot be read back now is the stronger
        // statement — and above the reasons that are about the request
        // rather than about the corpus. Not a budget and not a
        // truncation: nothing a presentation number does can reach it.
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::SourceExtractionIncomplete,
        }
    } else if inadmissible > 0 {
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::InadmissibleSources,
        }
    } else if governance.admitted_at_another_edition > 0 {
        // The same completeness fact the mirror case already moves
        // coverage on. A governing record this estate admitted about a
        // resource delivered here exists and was not delivered; silence
        // would be read as "this estate governs nothing here" (ruling
        // 0128 F1). This is not a budget and not a truncation: nothing a
        // presentation number does can reach it.
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::GovernanceOutsideCapturedEditions,
        }
    } else if !unresolved.is_empty() {
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::UnresolvedReferences,
        }
    } else if !consulted.note.complete {
        // The consulted set was read from an index this estate cannot
        // attest is a complete projection of its journals. A parsable
        // index is not proof of synchronization (ruling 0124) and a
        // missing one is not an empty estate (ruling 0137), so the
        // projection says its consulted set may be short rather than
        // presenting it as everything there is. Not a budget and not a
        // truncation: nothing a presentation number does can reach it.
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::IndexCannotAttestCompleteness,
        }
    } else {
        wirk_core::EvidenceCoverage::Complete
    };

    // Presentation, stated separately from fact: `truncated` and the
    // `OverBudget` omissions say what was rendered, `coverage` above
    // says what was found, and `next_action` below reads only the
    // second. Conflating them is how a rendering budget becomes a
    // completion oracle (BUILD.md §4.7).
    let truncated = omitted
        .iter()
        .any(|item| matches!(item, wirk_core::Omission::OverBudget { .. }));
    let content = wirk_core::ProjectionContent {
        format: wirk_core::PROJECTION_FORMAT.to_string(),
        compilation_policy: wirk_core::ASSEMBLY_POLICY.to_string(),
        route_edition: route_edition.to_string(),
        waypoint: def.id.clone(),
        revision: 0,
        question: orient.question.clone(),
        generations,
        publication_revision,
        retrieval,
        bound,
        referenced,
        reachable,
        assumptions,
        unknowns: unknowns.clone(),
        omitted,
        next_action: next_action_for(coverage, unknowns.is_empty()),
        coverage,
        truncated,
        // Revision 0, which expands nothing — and, absent from the
        // document, serializes without the field at all.
        expansion: None,
        consulted: consulted.findings,
        findings_index: consulted.note,
    };
    Some(finish_projection(
        content,
        ObservationSpan::measured(laps, started),
    ))
}

/// The exact fragment that makes a delivered sentence a *consulted*
/// sentence. Written by `consulted_statement` and read by the expansion
/// that must not carry a parent's copy of one, so the two cannot drift
/// apart into a recognizer that stops recognizing.
const CONSULTED_STATEMENT_MARK: &str = " record(s) of this Work's own journal and ";

/// The Assembly-attributed sentence that says what step 6 actually did.
///
/// It replaced a placeholder that told every reader consulted findings
/// and the index note "are not assembled here". Leaving that sentence
/// standing while quietly adding the fields underneath would have been
/// worse than either: a delivered context that describes itself
/// incorrectly (ruling 0124's "no empty future schema pretending
/// implemented semantics", one turn around).
///
/// Written once at the initial assembly and re-authored by each
/// expansion for its own observation, because unlike the expansion
/// disclosure beside it this sentence is about *this* revision's own
/// read. Re-authored means the parent's copy is *dropped*, not merely
/// appended to: two of these in one document would be two different
/// counts of one consulted set, and only one of them would be about the
/// read this revision made.
fn consulted_statement(consulted: &ConsultedSet) -> wirk_core::Statement {
    let own = consulted
        .findings
        .iter()
        .filter(|item| item.origin == wirk_core::ConsultedOrigin::OwnWork)
        .count();
    let published = consulted.findings.len() - own;
    let backing = match consulted.note.state {
        wirk_core::FindingsIndexState::Unobserved => {
            "this assembly observed no index at all, so the published half of that set is not \
             short — it was never read"
        }
        wirk_core::FindingsIndexState::Synchronized => {
            "the estate's findings index was a complete projection of its journals when this \
             was read, so the published half of that set is what this requester may see of it"
        }
        wirk_core::FindingsIndexState::Unreconciled => {
            "nothing has reconciled the estate's findings index in this daemon yet, so it is \
             not an index known to be complete and the published half of that set may be short"
        }
        wirk_core::FindingsIndexState::DurabilityUnconfirmed => {
            "the estate's findings index is visible to a reader and its directory entry is not \
             confirmed on disk, so it is not known to be a complete projection and the \
             published half of that set may be short"
        }
        wirk_core::FindingsIndexState::Behind => {
            "the estate's findings index does not project every row its journals hold, or its \
             completeness could not be established at all, so the published half of that set \
             may be short"
        }
        wirk_core::FindingsIndexState::Unreadable => {
            "the estate's findings index could not be read at this assembly, so the published \
             half of that set is empty for that reason and not because the estate holds none"
        }
    };
    wirk_core::Statement {
        text: format!(
            "{own}{CONSULTED_STATEMENT_MARK}{published} settled estate \
             publication(s) were consulted: {backing}. A consulted record is delivered with the \
             sentence its author recorded, captioned unverified, and nothing here asserts, \
             endorses or invalidates any of it. Its recorded generations are stated beside the \
             ones this assembly captured, and a difference between them is a fact about \
             editions, never a verdict on the claim. Expansion is separate: each revision is \
             what one assembly delivered, and `wirk world expand` adds a later revision to this \
             Run's own chain rather than editing this one or any before it."
        ),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    }
}

/// What step 4 found: how many governing items it delivered, how many
/// edges this requester's scope refused to disclose, and how many the
/// estate admitted about a bound resource at an edition this assembly
/// did not capture.
struct Governance {
    delivered: usize,
    filtered: usize,
    admitted_at_another_edition: usize,
}

/// Step 4 (BUILD.md §4.3): follow the `GovernedBy` edges the estate has
/// admitted out of the resources step 3 bound, and out of what those
/// reach, until nothing new is reached.
///
/// Three properties, each of them a ruling rather than a preference:
///
/// * **No raw edge read.** The edges come back through
///   `wirk_atlas::relationships_from_resources`, which applies exactly
///   the disclosure gate `relationships_for` applies: an edge whose far
///   end or whose evidence lies outside this Work's admitted memberships
///   is an opaque `Filtered` marker, counted and never described. A
///   coordinate confers no authority, so reaching one through an edge
///   grants nothing the bindings did not already grant — every endpoint
///   is re-resolved under the admitted membership before it is
///   delivered.
/// * **Pinned to the captured vector.** An endpoint is delivered only if
///   its membership *and* its generation are in the vector this assembly
///   captured. An edge admitted against a superseded generation is
///   evidence about bytes this projection is not pinned to; it is
///   reported as an unresolvable governing record rather than silently
///   read at today's generation.
/// * **Cycles and duplicates are safe without a cap.** Two sets, not
///   one, both keyed by (membership, generation, path) and both seeded
///   with everything step 3 already delivered: what has been
///   *delivered*, so a resource's bytes are rendered at most once, and
///   what has been *expanded*, so a cycle terminates. Separating them is
///   the point (ruling 0128 F2): one visited set made the two decisions
///   at once, so a governing record the authored question happened to
///   name by path was already "visited" when its edge was read, and the
///   edge — the whole reason step 4 exists — was skipped in silence.
///   Deduplicating the *resource* is right; dropping the *relationship*
///   is not. Nothing here decides that some number of governing records
///   is enough: `bound` has no budget, and there is no depth cap.
fn follow_governance(
    atlas: &wirk_atlas::AtlasStore,
    scope: &wirk_atlas::QueryScope,
    admitted: &[(wirk_atlas::Membership, wirk_atlas::SourceGeneration)],
    bound: &mut Vec<wirk_core::EvidenceItem>,
    omitted: &mut Vec<wirk_core::Omission>,
) -> Governance {
    let mut governance = Governance {
        delivered: 0,
        filtered: 0,
        admitted_at_another_edition: 0,
    };
    // Where a resource's bytes were rendered, so an edge reaching one
    // that is already in `bound` can attribute the relationship onto the
    // item that is already there instead of either delivering the bytes
    // twice or — the defect — saying nothing.
    let mut delivered_at: BTreeMap<wirk_atlas::ResourceKey, usize> = BTreeMap::new();
    // What has been walked out of. This, and not delivery, is what
    // terminates a cycle.
    let mut expanded: BTreeSet<wirk_atlas::ResourceKey> = BTreeSet::new();
    // Governing records already reported as unresolvable, so one missing
    // record reached through two edges is one omission rather than two.
    let mut unresolvable: BTreeSet<wirk_atlas::ResourceKey> = BTreeSet::new();
    let mut frontier: Vec<wirk_atlas::ResourceKey> = Vec::new();
    for (index, item) in bound.iter().enumerate() {
        let Ok(coordinate) = decode_coordinate(&item.coordinate) else {
            continue;
        };
        let key = resource_key(&coordinate);
        delivered_at.entry(key.clone()).or_insert(index);
        if expanded.insert(key.clone()) {
            frontier.push(key);
        }
    }

    while !frontier.is_empty() {
        // One pass over the relationship log for the whole frontier, not
        // one per item: a per-item pass is the shape that does not
        // survive a real estate.
        let Ok(found) = wirk_atlas::relationships_from_resources(atlas, scope, None, &frontier)
        else {
            // The relationship log could not be read at all. Said as an
            // unavailability over the frontier rather than as "this
            // estate governs nothing".
            omitted.push(wirk_core::Omission::Unavailable {
                coordinate: "governance".to_string(),
                reason: wirk_core::UnavailableReason::GoverningRecordUnresolvable,
            });
            break;
        };
        governance.admitted_at_another_edition += found.admitted_at_another_edition;
        let mut edges: Vec<wirk_atlas::Relationship> = Vec::new();
        for view in found.views {
            match view {
                wirk_atlas::RelationshipView::Disclosed(relationship) => edges.push(*relationship),
                wirk_atlas::RelationshipView::Filtered => governance.filtered += 1,
            }
        }
        // Delivery order that is not an invention: the relationship id,
        // which content-addresses the edge itself.
        edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));

        let mut next: Vec<wirk_atlas::ResourceKey> = Vec::new();
        for edge in edges {
            // The governed resource's own source, not the governing
            // record's: an edge crosses sources, and naming the wrong
            // side of it is a false statement about where the governed
            // file lives (found by running this on a real two-source
            // estate before it was written down).
            let governed_alias = admitted
                .iter()
                .find(|(membership, _)| membership.id == edge.from.membership)
                .map(|(membership, _)| membership.alias.as_str())
                .unwrap_or("?");
            let governed = display_path(&edge.from.path);
            // The governing record first, then the evidence the edge was
            // admitted on — the order an edge is read in.
            let mut reached: Vec<(&wirk_atlas::ExactCoordinate, bool)> = vec![(&edge.to, true)];
            reached.extend(edge.evidence.iter().map(|coordinate| (coordinate, false)));
            for (coordinate, is_record) in reached {
                let key = resource_key(coordinate);
                let Some((membership, generation)) =
                    admitted.iter().find(|(membership, generation)| {
                        membership.id == coordinate.membership
                            && generation.id == coordinate.generation
                    })
                else {
                    if unresolvable.insert(key.clone()) {
                        omitted.push(wirk_core::Omission::Unavailable {
                            coordinate: format!("relationship/{}", edge.id.0),
                            reason: wirk_core::UnavailableReason::GoverningRecordUnresolvable,
                        });
                    }
                    continue;
                };
                let reason = if is_record {
                    format!(
                        "`{governed}` in source `{governed_alias}` is governed by this record, \
                         which is in source `{}`, through the GovernedBy edge {} admitted by \
                         `{}`, at the captured generation",
                        membership.alias, edge.id.0, edge.producer
                    )
                } else {
                    format!(
                        "this is evidence the GovernedBy edge {} governing `{governed}` in \
                         source `{governed_alias}` was admitted on, and it is in source `{}` at \
                         the captured generation",
                        edge.id.0, membership.alias
                    )
                };
                // Delivered before — as an authored literal, as a
                // ranked-in governing record, or through another edge.
                // The resource is not rendered a second time and its
                // bytes are **not read a second time**; what is added is
                // the one thing the old visited set threw away, which is
                // why this edge was followed at all.
                if let Some(index) = delivered_at.get(&key).copied() {
                    let item = &mut bound[index];
                    item.reason = format!("{}; and {reason}", item.reason);
                    if is_record {
                        // What a governing record says stays true past
                        // this stage, however this assembly first
                        // happened to reach its bytes. Still not a read
                        // grant (BUILD.md §4.1).
                        item.lifetime = wirk_core::Lifetime::Standing;
                    }
                    if is_record && expanded.insert(key.clone()) {
                        next.push(key);
                    }
                    continue;
                }
                let Ok(wirk_atlas::ResolveOutcome::Resolved(resolved)) =
                    atlas.resolve_exact(membership, coordinate)
                else {
                    if unresolvable.insert(key.clone()) {
                        omitted.push(wirk_core::Omission::Unavailable {
                            coordinate: format!("relationship/{}", edge.id.0),
                            reason: wirk_core::UnavailableReason::GoverningRecordUnresolvable,
                        });
                    }
                    continue;
                };
                delivered_at.insert(key.clone(), bound.len());
                bound.push(wirk_core::EvidenceItem {
                    coordinate: encode_coordinate(coordinate),
                    summary: bounded_summary(&resolved.bytes),
                    // The `to` end of a `GovernedBy` edge is a governing
                    // record: what it says stays true past this stage.
                    // The evidence keeps its own content family's answer.
                    // Neither is a read grant (BUILD.md §4.1).
                    lifetime: if is_record {
                        wirk_core::Lifetime::Standing
                    } else {
                        lifetime_of(generation, &coordinate.path)
                    },
                    reason,
                    identity: wirk_core::ItemIdentity::Generation {
                        generation: coordinate.generation.0.clone(),
                        object_id: coordinate.object_id.clone(),
                    },
                    shown: None,
                });
                governance.delivered += 1;
                if is_record && expanded.insert(key.clone()) {
                    next.push(key);
                }
            }
        }
        frontier = next;
    }
    governance
}

fn resource_key(coordinate: &wirk_atlas::ExactCoordinate) -> wirk_atlas::ResourceKey {
    wirk_atlas::ResourceKey {
        membership: coordinate.membership.clone(),
        generation: coordinate.generation.clone(),
        path: coordinate.path.clone(),
    }
}

fn display_path(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

/// Step 5 (BUILD.md §4.3, BUILD-AMENDMENTS.md, ruling 0124): this Work's
/// own already-claimed stages, bound by exact recorded digest.
///
/// For every leaf of this Work's Route other than the one being reserved,
/// the **current** Run — the last one opened for that Waypoint — and that
/// Run's own Validated `Done` `ClaimRecorded`. A superseded attempt's
/// receipt is never borrowed because it happens to sit earlier in the
/// event list, and a retry that re-opened a Waypoint moves what is bound
/// to the new Run's own Claim.
///
/// The read is the whole property. The bytes are read **once**; the
/// digest is taken over *those* bytes; the summary is derived from the
/// same bytes. There is no `digest_of(path)` followed by a second
/// `read(path)`, which is the window the amendment names and which would
/// let a rewrite between the two be delivered as verified.
///
/// What that establishes, exactly (ruling 0124): these captured bytes
/// hashed to the digest the Claim recorded. It is not a claim about the
/// path's state at any later instant, and this assembler does not pretend
/// a rewrite after its only read must have been detected — it retains the
/// bytes it verified rather than reading a second time to check.
/// Everything else is an explicit unavailability: rewritten bytes are
/// `ArtifactBytesChanged` and are not bound, an unreadable file is
/// `ArtifactUnreadable`, and a pre-correction name-only receipt is
/// `ArtifactUnrecorded` — a path beside a `ClaimId` is not on its own
/// historical evidence identity. Nothing here writes anything, so a Read
/// binding gains no mutation authority through a declared artifact.
fn bind_prior_stage_artifacts(
    estate_root: &Path,
    events: &[Event],
    def: &WaypointDefinition,
    bound: &mut Vec<wirk_core::EvidenceItem>,
    omitted: &mut Vec<wirk_core::Omission>,
) -> usize {
    let defs = waypoint_defs_for(events);
    // `events` is one Work's own journal, so every event in it names
    // that Work — the id a managed receipt resolves against (ruling
    // 0145). Taken from the slice rather than threaded through
    // `prepare_projection`'s signature, which has no Work id because
    // until now nothing in the assembly needed one.
    let work_id = events.first().map(|event| event.work.clone());
    let mut delivered = 0usize;
    for leaf in wirk_core::flatten_leaves(&defs) {
        if leaf == def.id {
            continue;
        }
        let Some((run_id, _, world_hash)) = latest_run_for_waypoint(events, &leaf) else {
            continue;
        };
        let Some((claim, receipts)) = validated_done_claim(events, &run_id) else {
            continue;
        };
        // A prior stage that produced only managed outputs needs no
        // worktree at all (ruling 0145), so an absent one is no longer
        // a whole-stage unavailability — it is resolved per receipt,
        // against the root that receipt names.
        let worktree = worktree_of_reserved_world(events, &leaf, &world_hash);
        for receipt in receipts {
            let coordinate = artifact_coordinate(&claim, &receipt.name);
            if receipt.digest.is_empty() {
                omitted.push(wirk_core::Omission::Unavailable {
                    coordinate,
                    reason: wirk_core::UnavailableReason::ArtifactUnrecorded,
                });
                continue;
            }
            let resolved: Option<PathBuf> = match receipt.store {
                wirk_core::ArtifactStore::Worktree => worktree
                    .as_ref()
                    .map(|worktree| worktree.join(&receipt.path)),
                wirk_core::ArtifactStore::WorkOutputs => work_id.as_ref().and_then(|work| {
                    wirk_core::outputs::resolve_stored(estate_root, work, &receipt.path)
                }),
            };
            let Some(resolved) = resolved else {
                omitted.push(wirk_core::Omission::Unavailable {
                    coordinate,
                    reason: wirk_core::UnavailableReason::ArtifactUnreadable,
                });
                continue;
            };
            // The only read. Everything below is derived from `bytes`.
            let Ok(bytes) = std::fs::read(resolved) else {
                omitted.push(wirk_core::Omission::Unavailable {
                    coordinate,
                    reason: wirk_core::UnavailableReason::ArtifactUnreadable,
                });
                continue;
            };
            if sha256_hex(&bytes) != receipt.digest {
                omitted.push(wirk_core::Omission::Unavailable {
                    coordinate,
                    reason: wirk_core::UnavailableReason::ArtifactBytesChanged,
                });
                continue;
            }
            let mut cap = (ASSEMBLY_LOOKUP_BYTES as usize).min(bytes.len());
            while cap > 0 && std::str::from_utf8(&bytes[..cap]).is_err() {
                cap -= 1;
            }
            bound.push(wirk_core::EvidenceItem {
                coordinate,
                summary: bounded_summary(&bytes[..cap]),
                lifetime: wirk_core::Lifetime::Working,
                reason: format!(
                    "the prior stage `{}` of this Work claimed the artifact `{}`, and the bytes \
                     read here hash to the digest that Claim validated",
                    leaf.0, receipt.name
                ),
                identity: wirk_core::ItemIdentity::ArtifactDigest {
                    claim: claim.0.clone(),
                    digest: receipt.digest.clone(),
                },
                shown: None,
            });
            delivered += 1;
        }
    }
    delivered
}

/// The opaque coordinate a prior-stage artifact is addressed by
/// (BUILD.md §2). It names this Work's own Claim and the declared output
/// name — never a host path.
fn artifact_coordinate(claim: &ClaimId, name: &str) -> String {
    format!("claim/{}/artifact/{name}", claim.0)
}

/// The Validated `Done` `ClaimRecorded` of exactly this Run, with the
/// artifact receipts it was validated against. A refused Claim, a
/// Question and a neighbouring Run's success are all not this.
fn validated_done_claim(
    events: &[Event],
    run_id: &RunId,
) -> Option<(ClaimId, Vec<wirk_core::ArtifactReceipt>)> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::ClaimRecorded {
            claim,
            claim_kind: ClaimKind::Done,
            verdict: ClaimVerdict::Validated,
            artifacts,
        } if event.run.as_ref() == Some(run_id) => Some((claim.clone(), artifacts.clone())),
        _ => None,
    })
}

/// The checkout the reserved World for this Waypoint names — the same
/// root `handle_claim` resolved the artifact against when it recorded the
/// digest. Matched on the Run's own `world_hash`, so a Waypoint reserved
/// again since is not read through the wrong reservation.
fn worktree_of_reserved_world(
    events: &[Event],
    waypoint: &WaypointId,
    world_hash: &WorldHash,
) -> Option<PathBuf> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::WaypointReserved {
            waypoint: reserved,
            world_hash: hash,
            world,
        } if reserved == waypoint && hash == world_hash => Some(match world {
            World::Actor(actor) => actor.worktree_path.clone(),
            World::Deterministic(deterministic) => deterministic.cwd.clone(),
        }),
        _ => None,
    })
}

/// Step 7a: one scoped, pinned, ranked query for the authored question.
///
/// The same `wirk_atlas::search` the public `wirk atlas search` runs,
/// under this Work's own scope and pinned to the vector this assembly
/// captured — so a `referenced` hit resolves at the generation the
/// projection names it at, exactly as a `bound` one does. Semantic
/// ranking is *requested* rather than disabled: with no configured
/// backend the answer reports `unavailable` with the reason the query
/// itself produces, which is a truthful note, where `disabled` would say
/// the caller asked for lexical.
///
/// `configured` is the Route's own `orient.semantic`, translated into
/// exactly the `wirk_atlas::SemanticQueryConfig` the public
/// `--semantic-backend`/`--semantic-model` flags build (ruling 0109,
/// ruling 0128 F3). Nothing here supplies a default, reads an
/// environment variable or names an installed executable: a Route that
/// configures no backend gets `None`, and the answer's own reason then
/// says the *request* named none — which is the true statement, where
/// the previous one described a product that ships no backend and was
/// false about both the product and any estate holding editions.
///
/// Refuse a Route whose authored `orient.capacity` cannot be run, at
/// submit time — before any World is assembled — rather than letting it
/// surface later as a silently unavailable ranked answer (ruling 0172).
/// `ranked_answer` and `prepare_expansion` hand the same number to
/// `wirk_atlas::search`, whose `resolve_capacity` refuses it identically,
/// but that refusal turns an assembly's answer into `None` rather than
/// failing the submit — the same silent-clamp shape ruling 0171 already
/// refuses on the public CLI. Checked against every Actor leaf in the
/// tree, nested or not.
fn validate_route_capacities(defs: &[WaypointDefinition]) -> Result<(), String> {
    for id in flatten_leaves(defs) {
        let Some(def) = find_definition(defs, &id) else {
            continue;
        };
        let Some(orient) = def.orient.as_ref() else {
            continue;
        };
        let Some(capacity) = orient.capacity else {
            continue;
        };
        if capacity == 0 || capacity > wirk_atlas::CAPACITY_MAX {
            return Err(format!(
                "waypoint {} authors a result capacity of {capacity}, which cannot be run: this \
                 product decides result capacity under {} and ranks at most {} results for one \
                 query in this increment, so name a capacity between 1 and {} — the number of \
                 results a page shows is a separate limit and is not bounded by it",
                id.0,
                wirk_atlas::CAPACITY_POLICY,
                wirk_atlas::CAPACITY_MAX,
                wirk_atlas::CAPACITY_MAX,
            ));
        }
    }
    Ok(())
}

/// Every check that makes a semantic answer trustworthy — the absolute
/// path rule, the digest of the executable actually opened, the query
/// producer identity and basis, edition selection and currency — lives
/// in `wirk_atlas` and is reached by handing it this config. That is the
/// reuse: this function adds no policy of its own.
fn ranked_answer(
    atlas: &wirk_atlas::AtlasStore,
    scope: &wirk_atlas::QueryScope,
    pinned: &BTreeMap<wirk_atlas::MembershipId, wirk_atlas::GenerationId>,
    question: &str,
    limit: usize,
    capacity: Option<u64>,
    configured: Option<&wirk_core::SemanticQueryRequest>,
) -> Option<wirk_atlas::SearchAnswer> {
    wirk_atlas::search(
        atlas,
        &wirk_atlas::SearchRequest {
            scope: scope.clone(),
            requested_source: None,
            query: question.to_string(),
            families: Vec::new(),
            semantic: wirk_atlas::SemanticRequest::Requested,
            limit,
            // The World's own result capacity, independent of how much of
            // it `limit` renders (ruling 0171, ruling 0172): a Route that
            // authors none gets the World's documented default of 200,
            // the same operational bound this assembly ran at before an
            // explicit capacity existed — never the rendering budget, so
            // a smaller `limit` alone can never choose a different
            // ranking.
            capacity: capacity.or(Some(wirk_atlas::CAPACITY_MAX)),
            pinned: Some(pinned.clone()),
            offset: 0,
            semantic_query: configured.map(|configured| wirk_atlas::SemanticQueryConfig {
                backend: PathBuf::from(&configured.backend),
                backend_args: configured.backend_args.clone(),
                model: PathBuf::from(&configured.model),
            }),
            pinned_editions: None,
            pinned_mode: None,
            pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
        },
    )
    .ok()
}

/// The retrieval note, copied out of the answer and nowhere else. An
/// answer that could not be produced at all says so, rather than
/// presenting a lexical default that never ran.
///
/// `authored_capacity` is the Route's own `orient.capacity` — `None` when
/// it named none — kept apart from the collapsed value handed to
/// `wirk_atlas::search`, which folds "authored none" into the World's
/// documented default before `resolve_capacity` ever sees the two apart
/// (ruling 0172). Without it, a Route that authored no capacity and one
/// that authored the default explicitly are indistinguishable on the
/// published `source`, the exact confusion ruling 0171 added the field to
/// prevent.
fn retrieval_note(
    answer: Option<&wirk_atlas::SearchAnswer>,
    authored_capacity: Option<u64>,
) -> wirk_core::RetrievalNote {
    let Some(answer) = answer else {
        return wirk_core::RetrievalNote {
            mode: "none".to_string(),
            semantic: "unavailable".to_string(),
            semantic_reason: Some(
                "the ranked query for this question could not be run against the captured \
                 vector; no ranking happened and no absence is asserted"
                    .to_string(),
            ),
            editions: Vec::new(),
            degraded: vec!["query_unavailable".to_string()],
            total_candidates: 0,
            returned: 0,
            capacity: None,
        };
    };
    let coverage = answer.coverage;
    let mut degraded = Vec::new();
    for (flag, label) in [
        (coverage.no_match, "no_match"),
        (coverage.partial, "partial"),
        (coverage.source_unavailable, "source_unavailable"),
        (coverage.generation_unavailable, "generation_unavailable"),
        (coverage.unsupported_family, "unsupported_family"),
        (coverage.denied, "denied"),
        (coverage.no_sources, "no_sources"),
        (coverage.spent, "spent"),
        (
            coverage.continuation_unrecoverable,
            "continuation_unrecoverable",
        ),
        (
            coverage.source_extraction_incomplete,
            "source_extraction_incomplete",
        ),
    ] {
        if flag {
            degraded.push(label.to_string());
        }
    }
    wirk_core::RetrievalNote {
        mode: answer.mode.label().to_string(),
        semantic: answer.semantic.label().to_string(),
        semantic_reason: answer.semantic.reason().map(str::to_string),
        editions: answer
            .editions
            .iter()
            .map(|(membership, edition)| (membership.0.clone(), edition.0.clone()))
            .collect(),
        degraded,
        total_candidates: answer.budget.total_candidates,
        returned: answer.budget.returned,
        // The capacity this query actually ranked at (ruling 0171, ruling
        // 0172), copied from what a real native ranking reports about
        // itself. Absent for a lexical answer, which has no capacity to
        // report — `capacity_applies` says so, and the lexical path's own
        // `total_candidates` already names its whole admitted list.
        capacity: answer
            .application
            .as_ref()
            .map(|application| wirk_core::RetrievalCapacityNote {
                capacity: application.capacity,
                // The World's own default is not the caller naming a
                // capacity: only an authored `orient.capacity` earns the
                // `explicit` label `wirk_atlas::CapacitySource::Explicit`
                // otherwise reports, since the collapse above already
                // resolved a Route that named none to the same value.
                source: if authored_capacity.is_none()
                    && application.capacity_source == wirk_atlas::CapacitySource::Explicit
                {
                    "world-default".to_string()
                } else {
                    application.capacity_source.label().to_string()
                },
                policy: application.capacity_policy.clone(),
                max: application.capacity_max,
                reached: application.capacity_reached,
                resultset_exhausted: application.resultset_exhausted,
            }),
    }
}

/// One ranked hit as a delivered item. Dropped, rather than delivered
/// with a coordinate that names a generation this projection is not
/// pinned to, if its membership is somehow not in the captured vector.
fn ranked_item(
    admitted: &[(wirk_atlas::Membership, wirk_atlas::SourceGeneration)],
    hit: &wirk_atlas::EvidenceHit,
) -> Option<wirk_core::EvidenceItem> {
    let (membership, generation) = admitted.iter().find(|(membership, generation)| {
        membership.id == hit.coordinate.membership && generation.id == hit.coordinate.generation
    })?;
    // Ruling 0142: what the question's own terms located inside the
    // ranked unit, not the head of it. The unit keeps its identity —
    // `coordinate`, and the score and position that put it here are
    // untouched; `shown` is what was displayed out of it. A row the
    // ranker chose by vector carries no term location and falls back to
    // the head, which is the honest summary of a row no term matched.
    let (summary, shown) = local_summary(&hit.coordinate, &hit.snippet, &hit.matches).map_or_else(
        || (bounded_summary(hit.snippet.as_bytes()), None),
        |(summary, shown)| (summary, Some(shown)),
    );
    Some(wirk_core::EvidenceItem {
        coordinate: encode_coordinate(&hit.coordinate),
        summary,
        lifetime: lifetime_of(generation, &hit.coordinate.path),
        reason: format!(
            "ranked for the authored question in source `{}` at the captured generation, \
             position by score and not by judgement; being ranked here is not a statement that \
             it answers the question",
            membership.alias
        ),
        identity: wirk_core::ItemIdentity::Generation {
            generation: hit.coordinate.generation.0.clone(),
            object_id: hit.coordinate.object_id.clone(),
        },
        shown,
    })
}

/// Step 7b: the admitted places this stage may go looking that nothing
/// in the authored text named.
///
/// A handle is `<source alias>:<family>`, and `fetch` is the exact public
/// command that turns it into evidence — `wirk atlas search`, whose
/// `--estate`/`--work` fall back to the actor's own injected triple by
/// the same rule `wirk atlas resolve` runs on, so the printed line runs
/// verbatim inside a pane. `resources` is a real count out of the
/// captured generation, so an entry says how much is there rather than
/// implying anything about what it holds.
///
/// Only admitted sources appear, and only families the captured
/// generation actually indexes: an entry can never name a source outside
/// this Work's bindings. Returns the rendered list and the real total.
fn reachable_entries(
    admitted: &[(wirk_atlas::Membership, wirk_atlas::SourceGeneration)],
    limit: usize,
) -> (Vec<wirk_core::ReachableEntry>, usize) {
    let mut entries: Vec<wirk_core::ReachableEntry> = Vec::new();
    for (membership, generation) in admitted {
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for record in &generation.resources {
            if record.disposition != wirk_atlas::CoverageDisposition::Indexed {
                continue;
            }
            let Some(unit) = record.units.first() else {
                continue;
            };
            *counts.entry(family_label(unit.family)).or_default() += 1;
        }
        for (family, resources) in counts {
            entries.push(wirk_core::ReachableEntry {
                handle: format!("{}:{family}", membership.alias),
                source: membership.alias.clone(),
                family: family.to_string(),
                resources,
                fetch: format!(
                    "wirk atlas search --source {} --family {family} --query <terms>",
                    membership.alias
                ),
            });
        }
    }
    // Delivery order that is not an invention: the handle, which is the
    // alias and the family the entry is made of.
    entries.sort_by(|a, b| a.handle.cmp(&b.handle));
    let total = entries.len();
    entries.truncate(limit);
    (entries, total)
}

fn family_label(family: wirk_atlas::ContentFamily) -> &'static str {
    match family {
        wirk_atlas::ContentFamily::Code => "code",
        wirk_atlas::ContentFamily::Knowledge => "knowledge",
        wirk_atlas::ContentFamily::Config => "config",
    }
}

/// How bad one coverage state is, for the one comparison that needs an
/// order: an expansion's coverage is the worse of what it inherited and
/// what it observed. Deliberately not `Ord` on the type — nothing else
/// in this product ranks coverage, and a derived ordering would silently
/// also rank the *reasons*, which have no order at all.
fn coverage_severity(coverage: wirk_core::EvidenceCoverage) -> u8 {
    match coverage {
        wirk_core::EvidenceCoverage::Complete => 0,
        wirk_core::EvidenceCoverage::Partial { .. } => 1,
        wirk_core::EvidenceCoverage::Degraded { .. } => 2,
    }
}

/// Which of the two facts a coverage reason is about.
///
/// `coverage` is one label over two independent things. One is about the
/// **sources**: a reference that resolved nowhere, a binding this
/// requester was refused, an evidence item that could not be read back,
/// a governing record admitted outside the captured editions, a snapshot
/// that could not be taken at all. Those are facts about `bound` and the
/// captured generation vector, both of which an expansion inherits
/// wholesale, so they are still true of every later revision whatever it
/// observes — coverage never improves on them by expanding.
///
/// The other is about the **consultation**: the health of the findings
/// index this assembly read its consulted set from. Nothing about that
/// is inherited. `consulted` and `findings_index` are re-observed and
/// replaced in full by every revision, so only this revision's own read
/// is a true statement about this revision's consulted set, and it has
/// to be free to recover as well as to worsen.
///
/// Carrying the second as if it were the first is the defect
/// `loop-c4-consult-verify/VERDICT.md` records as V1: one transient
/// unreadable index poisoned every later revision of that Run for the
/// life of the chain, so a frozen document called its own index
/// `synchronized`, listed a settled publication in `consulted`, and then
/// told the actor in plain prose that the index could not be read and
/// that no settled publication was consulted.
fn reason_is_about_the_consultation(reason: wirk_core::CoverageReason) -> bool {
    match reason {
        wirk_core::CoverageReason::IndexCannotAttestCompleteness
        | wirk_core::CoverageReason::FindingsIndexUnreadable => true,
        wirk_core::CoverageReason::UnresolvedReferences
        | wirk_core::CoverageReason::InadmissibleSources
        | wirk_core::CoverageReason::EvidenceUnavailable
        | wirk_core::CoverageReason::ConcurrentPublication
        | wirk_core::CoverageReason::GovernanceOutsideCapturedEditions
        // A hole in the captured vector's own index. An expansion
        // inherits that vector wholesale, so it inherits this.
        | wirk_core::CoverageReason::SourceExtractionIncomplete => false,
    }
}

/// The source half of an expansion's coverage: everything this revision
/// carries or observed that is *not* a statement about the findings
/// index it just read.
///
/// It cannot simply be read off `parent.coverage`, because that field
/// holds one reason and the consultation half is the more severe of the
/// two whenever the index was unreadable — so a parent reading
/// `degraded / findings_index_unreadable` says nothing at all about a
/// source limitation underneath it. Dropping the parent's stale index
/// reason without recovering what it hid would report a genuinely
/// incomplete chain as `complete`, which is the same untruth pointing
/// the other way.
///
/// So it is rebuilt from the facts the expansion actually carries.
/// `omitted` and `unknowns` are both seeded from the parent and appended
/// to by this revision, so between them they hold every source
/// limitation this revision inherited *and* every one it found. The
/// parent's own reason is preferred only where the two agree in
/// severity: it is the chain's original wording and still true of this
/// revision. `ConcurrentPublication` is the one source reason no
/// omission records, and it is `Degraded`, so it is carried by the
/// parent's own state and never masked by anything.
///
/// The precedence between reasons is the initial assembly's own, so one
/// document's coverage does not depend on which of the two assemblers
/// wrote it.
fn carried_source_coverage(
    parent: wirk_core::EvidenceCoverage,
    omitted: &[wirk_core::Omission],
    unknowns: &[wirk_core::Statement],
) -> wirk_core::EvidenceCoverage {
    let unavailable_beyond_the_index = omitted.iter().any(|item| {
        matches!(
            item,
            wirk_core::Omission::Unavailable { reason, .. }
                if *reason != wirk_core::UnavailableReason::FindingsIndexUnreadable
        )
    });
    // A reference that did not resolve is a fact about the request, and
    // the *budget* on how many of them are shown must not decide whether
    // it is recorded — `OverBudget` carries the honest total precisely so
    // a presentation cut cannot become a completeness oracle in either
    // direction (BUILD.md §4.7).
    let any_unresolved = !unknowns.is_empty()
        || omitted.iter().any(|item| {
            matches!(
                item,
                wirk_core::Omission::OverBudget { of, total, .. } if of == "unknowns" && *total > 0
            )
        });
    let rebuilt = if unavailable_beyond_the_index {
        Some(wirk_core::CoverageReason::EvidenceUnavailable)
    } else if omitted
        .iter()
        .any(|item| matches!(item, wirk_core::Omission::SourceExtractionIncomplete { .. }))
    {
        // Seeded from the parent and appended to by this revision like
        // every other omission, so an extraction hole the parent
        // recorded is recovered here even when the parent's own reason
        // was a stale consultation one that masked it.
        Some(wirk_core::CoverageReason::SourceExtractionIncomplete)
    } else if omitted
        .iter()
        .any(|item| matches!(item, wirk_core::Omission::Inadmissible { .. }))
    {
        Some(wirk_core::CoverageReason::InadmissibleSources)
    } else if omitted
        .iter()
        .any(|item| matches!(item, wirk_core::Omission::AdmittedAtAnotherEdition { .. }))
    {
        Some(wirk_core::CoverageReason::GovernanceOutsideCapturedEditions)
    } else if any_unresolved {
        Some(wirk_core::CoverageReason::UnresolvedReferences)
    } else {
        None
    };
    let rebuilt = match rebuilt {
        Some(reason) => wirk_core::EvidenceCoverage::Partial { reason },
        None => wirk_core::EvidenceCoverage::Complete,
    };
    let carried = match parent {
        wirk_core::EvidenceCoverage::Partial { reason }
        | wirk_core::EvidenceCoverage::Degraded { reason }
            if reason_is_about_the_consultation(reason) =>
        {
            // Not a source fact, and not this revision's observation
            // either. Whatever it hid is in `rebuilt`.
            wirk_core::EvidenceCoverage::Complete
        }
        other => other,
    };
    if coverage_severity(rebuilt) > coverage_severity(carried) {
        rebuilt
    } else {
        carried
    }
}

/// Step 8: one sentence about the **state of the delivered evidence**.
///
/// Chosen only by `coverage` and whether anything went unresolved, so it
/// is byte-identical under any budget — a cut list cannot reach it, which
/// is the whole point (BUILD.md §4.7: a rendering budget must not become
/// a completion oracle). It names no role, assigns no work, and says
/// nothing about whether the stage is finished: it describes what the
/// assembler delivered and what it could not, and stops there (ruling
/// 0124: "No role taxonomy or mandatory orientation agent").
fn next_action_for(coverage: wirk_core::EvidenceCoverage, no_unknowns: bool) -> String {
    let state = match coverage {
        wirk_core::EvidenceCoverage::Complete => {
            "every reference the authored text named resolved in the admitted sources at the \
             captured generations"
        }
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::UnresolvedReferences,
        } => "some references the authored text named did not resolve",
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::InadmissibleSources,
        } => {
            "something inside this request's reach was not disclosed to this Work, and is \
             reported here only as a count"
        }
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::EvidenceUnavailable,
        } => {
            "something the captured vector or this Work's own record names could not be read \
             back at the identity it was recorded against"
        }
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::SourceExtractionIncomplete,
        } => {
            "part of an admitted source could not be extracted into anything searchable at the \
             generation this projection captured, so those bytes are in the source and in no \
             index here; it is reported only as a count, and `wirk atlas status` for a source \
             this Work is bound to is where the detail is"
        }
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::IndexCannotAttestCompleteness,
        } => {
            "the estate's findings index is not a projection this estate can attest is \
             complete, so the settled publications consulted here may be short of what the \
             estate's journals hold"
        }
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::FindingsIndexUnreadable,
        }
        | wirk_core::EvidenceCoverage::Degraded {
            reason: wirk_core::CoverageReason::FindingsIndexUnreadable,
        } => {
            "the estate's findings index could not be read at this assembly, so no settled \
             publication was consulted and this Work's own record is all that is here"
        }
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::GovernanceOutsideCapturedEditions,
        } => {
            "this estate has admitted a governing relationship about a resource delivered here, \
             recorded at an edition this assembly did not capture; it is reported only as a \
             count, and nothing here says whether it still holds"
        }
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::ConcurrentPublication,
        }
        | wirk_core::EvidenceCoverage::Degraded { .. } => {
            "no usable snapshot was taken: the estate's published sources moved under this \
             assembly, so nothing here is a statement about what the estate holds"
        }
    };
    let unresolved = if no_unknowns {
        "The assembler recorded no unresolved reference."
    } else {
        "The unresolved references are listed as unknowns, attributed to the intent that named \
         them; none of them is a finding about whether the thing exists."
    };
    format!(
        "State of the delivered evidence: {state}. {unresolved} This describes what was \
         assembled and nothing else — not whether it is sufficient, not what should be done \
         next, and not whether this stage is finished."
    )
}

/// Exact path resolution **against the already-captured generation**,
/// across every admitted source. A path present in three admitted
/// sources genuinely resolves three times; each is its own coordinate at
/// its own generation.
///
/// It reads the captured `SourceGeneration` this assembly already holds
/// rather than asking the store to resolve the path, and that is the
/// whole of BUILD.md §4.2's "capture once, pin every later read" taken
/// literally: `AtlasStore::resolve_path` would re-read and re-validate
/// the generation manifest on every single call, which on a real estate
/// is the dominant cost of a reservation (measured: 35s for three path
/// references over a 19MB + 54MB pair of manifests) and is also a second
/// look at a catalog that may have moved. Reading the captured value
/// cannot see a moving catalog at all.
///
/// The coordinate is built to be resolvable: the line bounds come from
/// `wirk_atlas::actual_line_bounds`, the same function
/// `AtlasStore::resolve_exact` validates against, so every coordinate
/// this projection delivers resolves through the public
/// `wirk atlas resolve` the actor actually types.
fn resolve_path_reference(
    admitted: &[(wirk_atlas::Membership, wirk_atlas::SourceGeneration)],
    path: &str,
    omitted: &mut Vec<wirk_core::Omission>,
) -> Vec<wirk_core::EvidenceItem> {
    let mut hits = Vec::new();
    for (membership, generation) in admitted {
        if hits.len() >= ASSEMBLY_HITS_PER_REFERENCE {
            break;
        }
        // Not this source's file, and not a fact worth stating: an
        // authored path naming one source's file is absent from every
        // other one by construction.
        let Some(record) = generation
            .resources
            .iter()
            .find(|record| record.path == path.as_bytes())
        else {
            continue;
        };
        let reason = match record.disposition {
            wirk_atlas::CoverageDisposition::Indexed => {
                match bind_resource(membership, generation, record, path) {
                    Ok(item) => {
                        hits.push(item);
                        continue;
                    }
                    Err(reason) => reason,
                }
            }
            wirk_atlas::CoverageDisposition::Excluded => {
                wirk_core::UnavailableReason::ResourceExcluded
            }
            wirk_atlas::CoverageDisposition::Unsupported => {
                wirk_core::UnavailableReason::ResourceUnsupported
            }
            wirk_atlas::CoverageDisposition::Unavailable
            | wirk_atlas::CoverageDisposition::Error => {
                wirk_core::UnavailableReason::ResourceUnavailable
            }
        };
        // The recorded resource is there and its content is not
        // deliverable at the generation it was recorded against. Said
        // explicitly, with a closed reason and no raw error text — never
        // as absence, and never re-resolved against a newer generation to
        // fill the hole.
        omitted.push(wirk_core::Omission::Unavailable {
            coordinate: format!("{}:{path}", membership.alias),
            reason,
        });
    }
    hits
}

/// Builds one bound item from a recorded, indexed resource at the
/// captured generation, reading the committed Git object once.
fn bind_resource(
    membership: &wirk_atlas::Membership,
    generation: &wirk_atlas::SourceGeneration,
    record: &wirk_atlas::ResourceRecord,
    path: &str,
) -> Result<wirk_core::EvidenceItem, wirk_core::UnavailableReason> {
    let Some(object_id) = record.object_id.clone() else {
        return Err(wirk_core::UnavailableReason::ResourceUnavailable);
    };
    let Ok(bytes) = read_blob(&membership.locator, &object_id) else {
        return Err(wirk_core::UnavailableReason::ResourceUnavailable);
    };
    let mut cap = (ASSEMBLY_LOOKUP_BYTES as usize).min(bytes.len());
    while cap > 0 && std::str::from_utf8(&bytes[..cap]).is_err() {
        cap -= 1;
    }
    let Some((line_start, line_end)) = wirk_atlas::actual_line_bounds(&bytes, 0, cap as u64) else {
        return Err(wirk_core::UnavailableReason::ResourceUnavailable);
    };
    let coordinate = wirk_atlas::ExactCoordinate {
        estate: membership.estate.clone(),
        membership: membership.id.clone(),
        source: membership.source.clone(),
        generation: generation.id.clone(),
        path: record.path.clone(),
        object_id,
        byte_start: 0,
        byte_end: cap as u64,
        line_start,
        line_end,
    };
    Ok(wirk_core::EvidenceItem {
        coordinate: encode_coordinate(&coordinate),
        summary: bounded_summary(&bytes[..cap]),
        lifetime: lifetime_of(generation, &record.path),
        reason: format!(
            "the authored text names the path `{path}`, resolved exactly in source `{}` at the \
             captured generation",
            membership.alias
        ),
        identity: wirk_core::ItemIdentity::Generation {
            generation: coordinate.generation.0.clone(),
            object_id: coordinate.object_id.clone(),
        },
        shown: None,
    })
}

/// A resource's own content family decides how long what it says stays
/// true: Knowledge is `Standing`, Code and Config are `Working`. This is
/// a lifetime, never an authority — nothing may be read because it is
/// `Standing` (BUILD.md §4.1).
fn lifetime_of(generation: &wirk_atlas::SourceGeneration, path: &[u8]) -> wirk_core::Lifetime {
    let family = generation
        .resources
        .iter()
        .find(|record| record.path == path)
        .and_then(|record| record.units.first())
        .map(|unit| unit.family);
    match family {
        Some(wirk_atlas::ContentFamily::Knowledge) => wirk_core::Lifetime::Standing,
        _ => wirk_core::Lifetime::Working,
    }
}

/// Identifier resolution: pinned exact search for the tokens, then a
/// literal check against the committed bytes at the coordinate each hit
/// names.
///
/// The ranking chooses candidates; it never decides the answer. A hit
/// whose recorded bytes do not literally contain the authored token is
/// dropped, so what lands in `bound` is "this exact name occurs here at
/// this exact generation" — a fact, verified against the committed
/// object, not a similarity score.
///
/// One search covers every identifier the authored text named, because a
/// search is a whole-corpus pass and one per token is one corpus pass per
/// token. A token that the shared pass attributes nothing to gets its own
/// targeted search afterwards, so batching never turns a resolvable
/// identifier into a false `unknown` — it only saves the passes that
/// would have found the same rows.
fn resolve_identifier_references(
    atlas: &wirk_atlas::AtlasStore,
    scope: &wirk_atlas::QueryScope,
    admitted: &[(wirk_atlas::Membership, wirk_atlas::SourceGeneration)],
    pinned: &BTreeMap<wirk_atlas::MembershipId, wirk_atlas::GenerationId>,
    names: &[&str],
) -> BTreeMap<String, Vec<wirk_core::EvidenceItem>> {
    let mut found: BTreeMap<String, Vec<wirk_core::EvidenceItem>> = BTreeMap::new();
    if admitted.is_empty() || names.is_empty() {
        return found;
    }
    let shared = identifier_candidates(
        atlas,
        scope,
        pinned,
        &names.join(" "),
        ASSEMBLY_CANDIDATES_PER_REFERENCE * names.len(),
    );
    attribute_candidates(admitted, names, &shared, &mut found);
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !found.contains_key(*name))
        .collect();
    for name in missing {
        let targeted = identifier_candidates(
            atlas,
            scope,
            pinned,
            name,
            ASSEMBLY_CANDIDATES_PER_REFERENCE,
        );
        attribute_candidates(admitted, &[name], &targeted, &mut found);
    }
    found
}

fn identifier_candidates(
    atlas: &wirk_atlas::AtlasStore,
    scope: &wirk_atlas::QueryScope,
    pinned: &BTreeMap<wirk_atlas::MembershipId, wirk_atlas::GenerationId>,
    query: &str,
    limit: usize,
) -> Vec<wirk_atlas::EvidenceHit> {
    wirk_atlas::search(
        atlas,
        &wirk_atlas::SearchRequest {
            scope: scope.clone(),
            requested_source: None,
            query: query.to_string(),
            families: Vec::new(),
            semantic: wirk_atlas::SemanticRequest::Disabled,
            limit,
            // Lexical: capacity bounds a native result set and this path
            // has none. Defaulted rather than named, and reported as not
            // applying.
            capacity: None,
            // The captured vector, pinned: a page of this search reads
            // the generations this projection names and no others.
            pinned: Some(pinned.clone()),
            offset: 0,
            semantic_query: None,
            pinned_editions: None,
            pinned_mode: None,
            pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
        },
    )
    .map(|answer| answer.hits)
    .unwrap_or_default()
}

/// Attributes ranked candidates to the authored tokens they literally
/// contain, reading each candidate's committed bytes at most once.
fn attribute_candidates(
    admitted: &[(wirk_atlas::Membership, wirk_atlas::SourceGeneration)],
    names: &[&str],
    candidates: &[wirk_atlas::EvidenceHit],
    found: &mut BTreeMap<String, Vec<wirk_core::EvidenceItem>>,
) {
    for hit in candidates {
        let Some((membership, generation)) = admitted
            .iter()
            .find(|(membership, _)| membership.id == hit.coordinate.membership)
        else {
            continue;
        };
        let wanted: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| {
                found
                    .get(*name)
                    .is_none_or(|hits| hits.len() < ASSEMBLY_HITS_PER_REFERENCE)
            })
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let Ok(bytes) = read_blob(&membership.locator, &hit.coordinate.object_id) else {
            continue;
        };
        let (start, end) = (
            hit.coordinate.byte_start as usize,
            hit.coordinate.byte_end as usize,
        );
        if end > bytes.len() || start > end {
            continue;
        }
        let unit = &bytes[start..end];
        let Ok(text) = std::str::from_utf8(unit) else {
            continue;
        };
        for name in wanted {
            if !text.contains(name) {
                continue;
            }
            // Ruling 0142: this item is delivered *because* the name
            // occurs literally in these bytes, so the summary is taken
            // from where it occurs. Same window, same coordinate
            // arithmetic as everywhere else.
            let (summary, shown) =
                local_summary(&hit.coordinate, text, &literal_matches(text, name)).map_or_else(
                    || (bounded_summary(unit), None),
                    |(summary, shown)| (summary, Some(shown)),
                );
            found
                .entry(name.to_string())
                .or_default()
                .push(wirk_core::EvidenceItem {
                    coordinate: encode_coordinate(&hit.coordinate),
                    summary,
                    lifetime: lifetime_of(generation, &hit.coordinate.path),
                    reason: format!(
                        "the authored text names the identifier `{name}`, which occurs literally \
                         in source `{}` at the captured generation",
                        membership.alias
                    ),
                    identity: wirk_core::ItemIdentity::Generation {
                        generation: hit.coordinate.generation.0.clone(),
                        object_id: hit.coordinate.object_id.clone(),
                    },
                    shown,
                });
        }
    }
}

/// Assemble with no guard of any kind held, then re-check the Atlas
/// publication revision before the caller commits. Used at `submit`,
/// where no journal exists yet, so the journal half of the re-check is
/// vacuous and the Atlas half still applies (BUILD.md §4.6).
fn prepared_without_journal(
    state: &Arc<WirkdState>,
    bindings: &[RepositoryBinding],
    def: &WaypointDefinition,
    route_edition: &str,
) -> Option<PreparedProjection> {
    let orient = def.orient.as_ref()?;
    // One window for the whole assembly, laps included: the receipt
    // reports what the reservation actually spent, not what the last
    // successful lap spent.
    let started = std::time::Instant::now();
    for attempt in 1..=JOURNAL_OBSERVATION_ATTEMPTS {
        // No journal exists yet at `submit`, so there is no prior stage
        // of this Work to bind: an empty event slice is the literal
        // truth here, not a shortcut.
        let prepared = prepare_projection(
            state,
            &[],
            bindings,
            def,
            route_edition,
            attempt as u32,
            started,
        )?;
        let current = state
            .atlas
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .publication_revision();
        if current == prepared.publication_revision {
            return Some(prepared);
        }
    }
    Some(degraded_projection(
        def,
        orient,
        route_edition,
        ObservationSpan::measured(JOURNAL_OBSERVATION_ATTEMPTS as u32, started),
        DegradedCause::PublicationChurn,
    ))
}

/// The observe / assemble / re-check loop for the three reservation
/// sites that reserve against an existing journal.
///
/// 1. lock this Work's journal, replay, **drop the guard**;
/// 2. decide, from that observation, which Waypoint the caller will
///    reserve; if it declares no `orient`, there is nothing to do and
///    the caller's existing path runs unchanged;
/// 3. assemble on that dropped-guard observation;
/// 4. re-lock, and use the result only if the journal has not moved and
///    the Atlas publication revision has not moved.
///
/// The caller then takes the guard itself and reserves. The prepared
/// projection carries the Waypoint it was assembled for, and the
/// reserving code uses it only if that is still the Waypoint it decides
/// on under the guard — so a lost race degrades the *evidence* and never
/// the *authority* (ruling 0124).
fn prepared_for_waypoint(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    decide: impl Fn(&[Event]) -> Option<WaypointId>,
) -> Option<PreparedProjection> {
    no_journal_guard_held("stage projection observation");
    let journal_handle = journal_for(state, work_id).ok().flatten()?;
    let started = std::time::Instant::now();
    let mut last: Option<(WaypointDefinition, String)> = None;
    for attempt in 1..=JOURNAL_OBSERVATION_ATTEMPTS {
        let events = {
            let journal = lock_journal(&journal_handle);
            journal.replay().ok()?
        };
        let defs = waypoint_defs_for(&events);
        let waypoint = decide(&events)?;
        let def = find_definition(&defs, &waypoint)?.clone();
        def.orient.as_ref()?;
        let route_edition = route_edition_of(&defs);
        let prepared = prepare_projection(
            state,
            &events,
            &fold(&events).repositories,
            &def,
            &route_edition,
            attempt as u32,
            started,
        )?;
        let settled = {
            let journal = lock_journal(&journal_handle);
            journal
                .replay()
                .ok()
                .is_some_and(|after| same_observation(&events, &after))
        };
        let published = state
            .atlas
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .publication_revision();
        if settled && published == prepared.publication_revision {
            return Some(prepared);
        }
        last = Some((def, route_edition));
    }
    // Exhausted, and the reservation still proceeds: the stage runs, and
    // the projection says the assembler kept losing to a moving estate.
    let (def, route_edition) = last?;
    let orient = def.orient.clone()?;
    Some(degraded_projection(
        &def,
        &orient,
        &route_edition,
        ObservationSpan::measured(JOURNAL_OBSERVATION_ATTEMPTS as u32, started),
        DegradedCause::PublicationChurn,
    ))
}

// ---- the staged read is one open file object (F6) --------------------

/// What reading a staged managed output found.
///
/// The three answers the Claim path already distinguishes, established
/// by the syscalls that open the file rather than re-derived from a
/// second path lookup afterwards.
enum StagedRead {
    /// A regular file inside this Run's own staging directory, reached
    /// without following a symlink at any component — and these are the
    /// bytes of *that* file object.
    Bytes(Vec<u8>),
    /// No entry by that name. The actor did not produce it.
    Absent,
    /// The entry, or a component on the way to it, is not what the area
    /// requires: a symlink at the name, a symlink where an ancestor
    /// directory should be, or a non-directory in the middle.
    OutOfBoundary,
    /// The area could not be inspected at all.
    Unreadable,
}

/// Open `component` inside the directory `parent` already holds open,
/// following no symlink.
///
/// `openat` with `O_NOFOLLOW`, `libc` used the way `ChildExecutor`'s
/// `prctl` already uses it (R5, the installed dependency's own
/// mechanism, not a `nix`/`cap-std` adoption and not a filesystem
/// capability layer). `component` is resolved *relative to a descriptor
/// this process is already holding*, so the lookup has exactly one
/// component and no ancestor for anything to swap underneath it.
fn open_no_follow(
    parent: BorrowedFd<'_>,
    component: &str,
    directory: bool,
) -> std::io::Result<OwnedFd> {
    let Ok(name) = std::ffi::CString::new(component) else {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    };
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    // SAFETY: `name` is NUL-terminated and outlives the call, `parent`
    // is a live borrowed descriptor, and the result is either -1 or a
    // fresh descriptor owned by this process alone.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor just returned by `openat` and
    // is not owned anywhere else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// How a failed component open answers, by the kernel's own reason.
fn staged_open_failure(err: &std::io::Error) -> StagedRead {
    match err.raw_os_error() {
        Some(libc::ENOENT) => StagedRead::Absent,
        // `ELOOP` is `O_NOFOLLOW` refusing a symlink at this component;
        // `ENOTDIR` is a non-directory where the walk needed one. Both
        // are the area's boundary, not an inspection failure.
        Some(libc::ELOOP) | Some(libc::ENOTDIR) | Some(libc::ENAMETOOLONG) => {
            StagedRead::OutOfBoundary
        }
        _ => StagedRead::Unreadable,
    }
}

/// The bytes of the managed output `name` staged by this Run, read
/// through **one** open file object.
///
/// **Why not check the path and then read it.** The previous shape
/// `lstat`ed and `canonicalize`d the staged path and then called
/// `std::fs::read` on the result — a second, fresh path lookup that
/// *does* follow symlinks. The actor owns its staging directory, so
/// between the two the entry could be replaced and the bytes recorded
/// need never have been the bytes validated (F6 of the independent
/// native-foundation review; ruling 0147 declines to call the path
/// race fixed). `O_NOFOLLOW` on the final component alone would not
/// close it either: an ancestor of a multi-component path is resolved
/// by the same lookup and is not covered by that flag.
///
/// **What closes it.** The estate root is opened once, and every
/// component below it — `works`, the Work id, `outputs`, `staging`, the
/// Run id, and finally the declared name — is opened with `openat` and
/// `O_NOFOLLOW` *relative to the descriptor the previous step returned*.
/// Each lookup is therefore a single component inside an already-pinned
/// directory: there is no ancestor left in any lookup for a rename to
/// retarget, and a symlink at any component is refused rather than
/// followed. Every one of those components is either a literal or an id
/// `well_formed_id` admits, and the name is one `check_output_name`
/// admits, so none of them can contain a separator, a `..` or a NUL.
///
/// The descriptor that survives the walk is then `fstat`ed for a regular
/// file and read to end. Check and read are the same file object, so the
/// digest recorded downstream is the digest of exactly the bytes that
/// validated — which is what makes the durable snapshot taken before the
/// Claim a snapshot *of the validated bytes* and not of whatever the
/// path resolved to a moment later.
fn read_staged_output(
    estate_root: &Path,
    work_id: &WorkId,
    run_id: &RunId,
    name: &str,
) -> StagedRead {
    if wirk_core::outputs::check_output_name(name).is_err() {
        return StagedRead::OutOfBoundary;
    }
    // The addressability rules stay where they are authored: if the
    // module cannot derive this Run's staging address, there is nothing
    // to walk to.
    if wirk_core::outputs::staged_path(estate_root, work_id, run_id, name).is_none() {
        return StagedRead::Unreadable;
    }
    // The estate root is the daemon's own, established at startup and
    // not inside any actor's area; it is the anchor the no-follow walk
    // starts from, not a step of it.
    let Ok(root) = std::fs::canonicalize(estate_root) else {
        return StagedRead::Unreadable;
    };
    let Ok(root_dir) = std::fs::File::open(&root) else {
        return StagedRead::Unreadable;
    };
    let mut dir: OwnedFd = root_dir.into();
    for component in [
        "works",
        work_id.0.as_str(),
        "outputs",
        "staging",
        run_id.0.as_str(),
    ] {
        match open_no_follow(dir.as_fd(), component, true) {
            Ok(next) => dir = next,
            Err(err) => return staged_open_failure(&err),
        }
    }
    let opened = match open_no_follow(dir.as_fd(), name, false) {
        Ok(fd) => fd,
        Err(err) => return staged_open_failure(&err),
    };
    let mut file = std::fs::File::from(opened);
    // `fstat` on the descriptor just opened, never a path: a directory
    // opens read-only without `O_DIRECTORY`, and this is what refuses
    // it. There is no window between this and the read below, because
    // both address the same open file.
    let Ok(meta) = file.metadata() else {
        return StagedRead::Unreadable;
    };
    if !meta.file_type().is_file() {
        return StagedRead::OutOfBoundary;
    }
    let mut bytes = Vec::new();
    match io::Read::read_to_end(&mut file, &mut bytes) {
        Ok(_) => StagedRead::Bytes(bytes),
        Err(_) => StagedRead::Unreadable,
    }
}

/// Removes the `.tmp-` files a crash between a projection's temp write
/// and its rename can leave. See the call site's own note for why
/// nothing else in `projections/` is ever removed.
fn sweep_projection_temporaries(state: &Arc<WirkdState>) {
    let Ok(works) = std::fs::read_dir(state.estate_root.join("works")) else {
        return;
    };
    for work in works.flatten() {
        let Ok(entries) = std::fs::read_dir(work.path().join("projections")) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".tmp-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// `wirk output` (ruling 0145): where this Run's actor writes its
/// declared outputs, and which of them are staged right now.
///
/// **The daemon derives the storage.** The only inputs are the injected
/// triple's ids, each already checked against this estate and this
/// Work's own journal before a path is built: the caller supplies no
/// path, and there is no field on this verb through which it could. A
/// declared name that cannot be one filename component is reported here,
/// by name, with the rule it broke — the same diagnostic a Claim naming
/// it would refuse with, delivered before the actor spends a model on
/// producing it.
///
/// Read-only and side-effect-light: it creates this Run's staging
/// directory (so the path it prints is one the actor can write into
/// immediately) and appends nothing to any journal.
///
/// Deliberately no `--work`/`--run`: `WorldShow`'s own reasoning, which
/// is that authority is the journal line and never an id.
fn handle_run_outputs(state: &Arc<WirkdState>, payload: super::RunOutputsPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();
    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the triple's estate root does not identify this daemon's estate",
        );
    }
    let journal = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let events = {
        let journal = lock_journal(&journal);
        match journal.replay() {
            Ok(events) => events,
            Err(err) => return err_reply("JournalError", &err.to_string()),
        }
    };
    if events.is_empty() {
        return err_reply("NotFound", "no such work");
    }
    let Some(run) = find_run(&events, &run_id) else {
        return err_reply(
            "TripleMismatch",
            "the run id does not match any Run opened for this Work",
        );
    };
    let defs = waypoint_defs_for(&events);
    let Some(def) = find_definition(&defs, &run.waypoint) else {
        return err_reply(
            "TripleMismatch",
            "this Run's Waypoint has no journaled definition",
        );
    };
    let staging =
        match wirk_core::outputs::ensure_staging_dir(&state.estate_root, &work_id, &run_id) {
            Ok(dir) => dir,
            Err(err) => {
                return err_reply(
                    "OutputsUnavailable",
                    &format!("this Run's managed output area could not be prepared: {err}"),
                );
            }
        };
    let current = latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0)
        == Some(run_id.clone());
    let outputs: Vec<Value> = def
        .declared_outputs
        .iter()
        .map(
            |spec| match wirk_core::outputs::check_output_name(&spec.name) {
                Ok(()) => {
                    let path = staging.join(&spec.name);
                    // `staged` is the plain question "is there a regular
                    // file there now" — a symlink or a directory answers
                    // `false` here and refuses at Claim, rather than reading
                    // as ready and refusing later.
                    let staged = std::fs::symlink_metadata(&path)
                        .map(|meta| meta.file_type().is_file())
                        .unwrap_or(false);
                    json!({
                        "name": spec.name,
                        "required": spec.required,
                        "addressable": true,
                        "path": path.display().to_string(),
                        "staged": staged,
                    })
                }
                Err(err) => json!({
                    "name": spec.name,
                    "required": spec.required,
                    "addressable": false,
                    "detail": err.detail(),
                }),
            },
        )
        .collect();
    ok_reply(json!({
        "work": work_id.0,
        "run": run_id.0,
        "waypoint": run.waypoint.0,
        "current": current,
        "staging": staging.display().to_string(),
        "outputs": outputs,
    }))
}

/// `wirk world show` (W-C1, BUILD.md §5.3): the delivered stage
/// projection for the Run the caller's own injected triple names.
///
/// Three honest answers, and no fourth:
///
/// * the Waypoint declared no orientation request — said explicitly,
///   never as an empty object a reader would have to interpret;
/// * the reserved World names a projection and it is delivered, with its
///   observation receipt beside it;
/// * the reserved World names a projection and the file it names is
///   missing, unreadable, or does not re-hash to the id the journal
///   recorded — an explicit unavailability with a closed reason. It is
///   never regenerated against today's estate: what was delivered is a
///   historical fact, and re-assembling it now would answer a different
///   question with the first question's identity.
///
/// **An id is not a capability.** The Work and the Run come from the
/// triple; the file is read under `works/<work_id>/`; the reference
/// comes from that Work's own journal. There is no argument on this
/// surface that names a Work, a Run or a projection, so a coordinate or
/// an id copied out of one projection opens nothing. `currentness` is
/// reported, not enforced: a Run superseded by a retry may still be
/// executing, and the context *it* was delivered is its own historical
/// fact — the caller learns it is no longer the current Run rather than
/// being refused a read of its own World.
fn handle_world_show(state: &Arc<WirkdState>, payload: super::WorldShowPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();
    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the triple's estate root does not identify this daemon's estate",
        );
    }
    let journal = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };
    let events = {
        let journal = lock_journal(&journal);
        match journal.replay() {
            Ok(events) => events,
            Err(err) => return err_reply("JournalError", &err.to_string()),
        }
    };
    if events.is_empty() {
        return err_reply("NotFound", "no such work");
    }
    let Some(run) = find_run(&events, &run_id) else {
        return err_reply(
            "TripleMismatch",
            "the run id does not match any Run opened for this Work",
        );
    };
    // The whole of the binding check: `resolve_run_binding` refuses a
    // World whose own triple does not name this estate, this Work and
    // this Run, so a caller cannot read a World by asserting a triple
    // the journal does not carry.
    let binding = match resolve_run_binding(&events, &state.estate_root, &work_id, &run_id) {
        Ok(binding) => binding,
        Err(reason) => return err_reply("ValidationUnavailable", &reason),
    };
    let current = latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0)
        == Some(run_id.clone());
    let declared_orientation = find_definition(&waypoint_defs_for(&events), &run.waypoint)
        .is_some_and(|def| def.orient.is_some());

    let mut result = json!({
        "work": work_id.0,
        "run": run_id.0,
        "waypoint": run.waypoint.0,
        "current": current,
    });

    // The whole chain this Run was delivered, oldest first: the reserved
    // World's initial projection, then each revision its own actor
    // expanded. Listed on every reply, so a fresh actor with no
    // transcript learns that its context has a history — and how long it
    // is — from the one command it already runs.
    let chain = projection_chain(&binding, &run);
    if chain.is_empty() {
        result["orientation"] = json!(if declared_orientation {
            "unavailable"
        } else {
            "none"
        });
        result["detail"] = json!(if declared_orientation {
            "this Waypoint declares an orientation request, but the World reserved for this Run \
             carries no projection"
        } else {
            "this Waypoint declared no orientation request"
        });
        return ok_reply(result);
    }
    result["revisions"] = Value::Array(
        chain
            .iter()
            .map(|entry| {
                json!({
                    "revision": entry.revision,
                    "observation": entry.observation.0,
                    "projection": entry.projection.0,
                    "format": entry.format,
                    "initial": entry.revision == 0,
                })
            })
            .collect(),
    );
    result["latest_revision"] = json!(chain.last().map(|entry| entry.revision).unwrap_or(0));
    // Default: the latest revision, which is what "my context" means to
    // an actor that has expanded it. `--revision N` reads exactly N, and
    // a revision this Run was never delivered is refused by name rather
    // than silently falling back to one it was — a fallback would hand a
    // caller a different document under the number it asked for.
    let reference = match payload.revision {
        None => chain.last().expect("chain is non-empty"),
        Some(wanted) => {
            let Some(found) = chain.iter().find(|entry| entry.revision == wanted) else {
                result["orientation"] = json!("unavailable");
                result["reason"] = json!("no-such-revision");
                result["detail"] = json!(format!(
                    "this Run's delivered context has {} revision(s), 0 through {}; revision \
                     {wanted} is not one of them",
                    chain.len(),
                    chain.last().map(|entry| entry.revision).unwrap_or(0),
                ));
                return ok_reply(result);
            };
            found
        }
    };

    result["orientation"] = json!("delivered");
    result["reference"] = json!({
        "observation": reference.observation.0,
        "projection": reference.projection.0,
        "revision": reference.revision,
        "format": reference.format,
    });
    match wirk_core::ProjectionFile::read_referenced(&state.estate_root, &work_id, reference) {
        Ok(file) => {
            result["projection"] =
                serde_json::to_value(&file.content).expect("ProjectionContent always serializes");
            result["receipt"] =
                serde_json::to_value(&file.receipt).expect("ObservationReceipt always serializes");
        }
        Err(unavailable) => {
            result["orientation"] = json!("unavailable");
            result["reason"] = json!(unavailable.reason());
            // Only revision 0 is the reserved World's. A later revision
            // is one this Run asked for and this chain recorded, so name
            // that revision rather than blaming the reservation for a
            // file it does not name. The refusal and its `reason` are
            // unchanged either way.
            result["detail"] = json!(if reference.revision == 0 {
                "the reserved World names a projection this estate cannot deliver; it is not \
                 re-assembled, because what was delivered then is not what would be assembled now"
                    .to_string()
            } else {
                format!(
                    "revision {} of this Run's delivered context names a projection this estate \
                     cannot deliver; it is not re-assembled, because what was delivered then is \
                     not what would be assembled now",
                    reference.revision,
                )
            });
        }
    }
    ok_reply(result)
}

// ---- W-C3: expansion of a delivered stage context -------------------------
//
// One verb (`world expand`), one assembler (`prepare_expansion`), one
// event (`ProjectionExpanded`). Four rules carry it, each of them a
// ruling rather than a preference:
//
// * **Nothing already delivered is edited.** The reserved World, its
//   `WorldHash`, and every projection file already written stay exactly
//   as they are. An expansion writes a *new* file at `revision + 1` and
//   the journal carries a *new* reference. `world show --revision 0`
//   after any number of expansions reads the same bytes it read before
//   the first one.
// * **Authority is re-derived, never carried.** The triple is the only
//   door; the Run must be the current Run of its own Waypoint and still
//   Open; the binding must name this estate, this Work and this Run; and
//   all of it is re-checked under the guard the append happens under.
//   Laps buy a coherent parent, never a stale one (ruling 0124).
// * **The captured vector is preserved.** Every read pins to the parent
//   revision's own generations, re-admitted under this Work's bindings as
//   they stand now. An expansion never reads today's bytes under a
//   generation the stage was pinned to, and never observes a new vector
//   under an old World's identity.
// * **A handle is not a capability.** `--reference` must name a
//   `reachable` entry this Run's own chain actually delivered, and the
//   source it names must still be admitted. A handle copied from another
//   Work's projection, or invented, resolves to nothing.

/// The chain of projection revisions delivered to one Run, oldest first:
/// the reserved World's own reference at revision 0, then each
/// `ProjectionExpanded` this Run folded.
///
/// There is no other way to reach a revision. A `ProjectionId` is not an
/// address (`ProjectionFile::read_referenced` takes a reference out of
/// this Work's own journal and reads under `works/<work_id>/`), so a
/// chain is exactly what this Run was given and nothing else.
fn projection_chain(binding: &RunBinding, run: &Run) -> Vec<wirk_core::EvidenceProjectionRef> {
    let mut chain = Vec::new();
    if let Some(initial) = binding.world.evidence() {
        chain.push(initial.clone());
    }
    if chain.is_empty() {
        // No initial revision means no chain at all: an expansion cannot
        // have happened without one, and folding a tail onto nothing
        // would present a revision as if it were an initial delivery.
        return chain;
    }
    chain.extend(run.expansions.iter().cloned());
    chain
}

/// What one expansion asked for, after shape checking and before
/// anything is observed.
struct ExpansionAsk {
    question: Option<String>,
    reference: Option<String>,
    reason: Option<String>,
}

/// A `reachable` handle, revalidated against the chain that delivered it.
struct AdmittedHandle {
    handle: String,
    source: String,
    family: wirk_atlas::ContentFamily,
}

/// `wirk world expand` (W-C3): the current Run's actor adds a revision to
/// the context it was delivered.
///
/// The whole authority argument is here rather than spread across
/// helpers, because every line of it is the difference between "this Run
/// asked for more of its own context" and "something widened a stage's
/// reach". In order: the triple names this daemon's estate; the Run
/// exists in this Work's journal; the World reserved for it really is
/// bound to this estate/Work/Run (`resolve_run_binding`); the Work is
/// not terminal; the Run is still Open; the Run is the *current* Run of
/// its Waypoint; and the Waypoint declares an orientation request whose
/// initial projection is readable and re-hashes. Then, and only then,
/// the parent revision is read, the expansion is assembled with no guard
/// held, and the journal is re-checked under the commit guard before the
/// event is appended.
fn handle_world_expand(state: &Arc<WirkdState>, payload: super::WorldExpandPayload) -> Reply {
    let work_id = payload.triple.work_id.clone();
    let run_id = payload.triple.run_id.clone();
    if !estate_roots_equal(&state.estate_root, &payload.triple.estate_root) {
        return err_reply(
            "TripleMismatch",
            "the triple's estate root does not identify this daemon's estate",
        );
    }
    // Shape, before anything is observed: it depends on the request and
    // nothing else, so it never needs re-deciding when the loop re-reads.
    let ask = {
        let question = payload
            .question
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let reference = payload
            .reference
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let reason = payload
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        if question.is_none() && reference.is_none() {
            return err_reply(
                "BadRequest",
                "an expansion asks for something: give --question, --reference, or both",
            );
        }
        if question.as_ref().is_some_and(|text| text.len() > 4096)
            || reason.as_ref().is_some_and(|text| text.len() > 4096)
        {
            return err_reply("BadRequest", "the authored text is too long to record");
        }
        ExpansionAsk {
            question,
            reference,
            reason,
        }
    };
    let journal_handle = match journal_for(state, &work_id) {
        Ok(Some(journal)) => journal,
        Ok(None) => return err_reply("NotFound", "no such work"),
        Err(err) => return err_reply("JournalError", &err.to_string()),
    };

    // Observe with no guard held, assemble, then re-lock and re-check —
    // the same discipline `handle_finding_raise` runs on (ruling 0119),
    // for the same reason: assembly reads the Atlas and the filesystem,
    // and a decision must not rest on authority that has since moved. A
    // journal that did not move is a chain whose tail is still the
    // parent this expansion was built on, which is what makes a
    // concurrent expansion a loser that re-reads rather than a lost
    // update.
    let mut attempt = 0usize;
    let started = std::time::Instant::now();
    let (mut journal, prepared, parent_ref, mut chain) = loop {
        attempt += 1;
        let events = {
            let journal = lock_journal(&journal_handle);
            match journal.replay() {
                Ok(events) => events,
                Err(err) => return err_reply("JournalError", &err.to_string()),
            }
        };
        if events.is_empty() {
            return err_reply("NotFound", "no such work");
        }
        let Some(run) = find_run(&events, &run_id) else {
            return err_reply(
                "TripleMismatch",
                "the run id does not match any Run opened for this Work",
            );
        };
        let work = fold(&events);
        if work.id != work_id {
            return err_reply("TripleMismatch", "triple does not match this work");
        }
        if work.state.is_terminal() {
            return err_reply(
                "WorkTerminal",
                "the Work is already terminal: its delivered context is history, not a context \
                 to add to",
            );
        }
        // The stale-expansion guard, asked **before** the Run's own
        // state: a superseded Run is refused for being superseded, which
        // is the authority fact, rather than for whatever a retry
        // happened to leave its state as. An exhausted observation
        // budget can never reach past this — it is asked again under the
        // commit guard below, against the same events the append happens
        // on.
        if latest_run_for_waypoint(&events, &run.waypoint).map(|entry| entry.0)
            != Some(run_id.clone())
        {
            return err_reply(
                "TripleMismatch",
                "the run is not current for its waypoint: a superseded Run's delivered context \
                 is a historical fact and is never added to",
            );
        }
        if !matches!(run.state, RunState::Open) {
            return err_reply(
                "RunClosed",
                "this Run is no longer open: the context it was delivered stays exactly as it \
                 was delivered",
            );
        }
        let binding = match resolve_run_binding(&events, &state.estate_root, &work_id, &run_id) {
            Ok(binding) => binding,
            Err(reason) => return err_reply("ValidationUnavailable", &reason),
        };
        let chain = projection_chain(&binding, &run);
        let Some(parent_ref) = chain.last().cloned() else {
            return err_reply(
                "NoOrientation",
                "the World reserved for this Run carries no projection: there is nothing to \
                 expand",
            );
        };
        // The parent, read and re-hashed exactly as `world show` reads
        // it. An unreadable or substituted parent is an explicit
        // refusal, never an expansion built on bytes this journal does
        // not vouch for.
        let parent_file = match wirk_core::ProjectionFile::read_referenced(
            &state.estate_root,
            &work_id,
            &parent_ref,
        ) {
            Ok(file) => file,
            Err(unavailable) => {
                return err_reply(
                    "ProjectionUnavailable",
                    &format!(
                        "the revision this expansion would extend cannot be delivered \
                         ({}); it is not re-assembled against today's estate",
                        unavailable.reason()
                    ),
                );
            }
        };
        let Some(parent) = parent_file.content.expandable() else {
            return err_reply(
                "ProjectionUnavailable",
                "the revision this expansion would extend was written in a format that carries \
                 no expansion chain",
            );
        };
        // The handle, revalidated against this Run's own chain. Every
        // revision in the chain is read — a handle delivered by revision
        // 0 stays usable after revision 3 — and a handle no revision
        // delivered addresses nothing, whatever it spells.
        let handle = match ask.reference.as_deref() {
            None => None,
            Some(wanted) => match admitted_handle(state, &work_id, &chain, wanted) {
                Ok(handle) => Some(handle),
                Err(message) => return err_reply("UnknownHandle", &message),
            },
        };
        let defs = waypoint_defs_for(&events);
        let Some(def) = find_definition(&defs, &run.waypoint).cloned() else {
            return err_reply(
                "NoOrientation",
                "this Run's Waypoint is not in the Route this Work was submitted with",
            );
        };
        let Some(orient) = def.orient.clone() else {
            return err_reply(
                "NoOrientation",
                "this Run's Waypoint declares no orientation request",
            );
        };
        // No guard is held here, by construction, and that is the whole
        // reason this loop exists.
        let prepared = prepare_expansion(
            state,
            &orient,
            &events,
            &parent,
            &parent_ref,
            &run_id,
            &fold(&events).repositories,
            &ask,
            handle.as_ref(),
            attempt as u32,
            started,
        );

        // Re-acquire and re-check. An unmoved journal means the chain's
        // tail is still `parent_ref`, the Run is still current and still
        // Open, and the binding is unchanged — all of them are folded
        // from these same events.
        let journal = lock_journal(&journal_handle);
        let events_now = match journal.replay() {
            Ok(events_now) => events_now,
            Err(err) => return err_reply("JournalError", &err.to_string()),
        };
        if same_observation(&events, &events_now) {
            break (journal, prepared, parent_ref, chain);
        }
        drop(journal);
        if attempt >= JOURNAL_OBSERVATION_ATTEMPTS {
            return err_reply(
                "Conflict",
                "this Work's journal moved under every attempt to extend this context: re-read \
                 `wirk world show` and expand again from the revision that is now current",
            );
        }
    };

    // Durable before referenced: the file is written, fsynced and
    // renamed under `works/<work>/projections/` before the event that
    // names it exists, exactly as the initial reservation's is.
    let reference = match prepared.commit(state, &work_id) {
        Ok(reference) => reference,
        Err((code, message)) => return err_reply(code, &message),
    };
    let event = new_event(
        &work_id,
        Some(run_id.clone()),
        EventKind::ProjectionExpanded {
            waypoint: prepared.waypoint.clone(),
            parent: parent_ref.observation.clone(),
            reference: Box::new(reference.clone()),
        },
    );
    if let Err(err) = append_event(state, &mut journal, &work_id, &event) {
        return err_reply("JournalError", &err.to_string());
    }
    drop(journal);

    let file = prepared.file;
    chain.push(reference.clone());
    let mut result = json!({
        "work": work_id.0,
        "run": run_id.0,
        "waypoint": prepared.waypoint.0,
        // Reaching this line means the Run was current under the same
        // guard the append happened on. Said explicitly, because this
        // reply is rendered by the same code `world show`'s is and an
        // absent field would render as "not current" — which would be
        // false about the one Run that just proved it was.
        "current": true,
        "orientation": "delivered",
        "revision": reference.revision,
        "parent": parent_ref.observation.0,
        "latest_revision": reference.revision,
        "revisions": Value::Array(
            chain
                .iter()
                .map(|entry| {
                    json!({
                        "revision": entry.revision,
                        "observation": entry.observation.0,
                        "projection": entry.projection.0,
                        "format": entry.format,
                        "initial": entry.revision == 0,
                    })
                })
                .collect(),
        ),
        "reference": {
            "observation": reference.observation.0,
            "projection": reference.projection.0,
            "revision": reference.revision,
            "format": reference.format,
        },
    });
    result["projection"] =
        serde_json::to_value(&file.content).expect("ProjectionContent always serializes");
    result["receipt"] =
        serde_json::to_value(&file.receipt).expect("ObservationReceipt always serializes");
    ok_reply(result)
}

/// Revalidates a `--reference` handle against the chain that delivered
/// it, and against the family vocabulary the projection itself writes.
///
/// Two independent checks, and both are needed. The chain check is what
/// makes a handle non-transferable: a handle is reachable **because some
/// revision of this Run's own context delivered it**, so one lifted out
/// of another Work's projection, or invented, is refused here before any
/// source is named. The parse is what turns it into a query narrowing;
/// it is deliberately the inverse of `reachable_entries`' own
/// `<alias>:<family>` construction rather than a second grammar.
///
/// Admission is *not* checked here: it is re-derived in the assembler
/// against this Work's bindings as they stand now, so a source unbound
/// since delivery contributes an inadmissible count and no lookup, the
/// same way an unbound `orient.sources` alias does.
fn admitted_handle(
    state: &Arc<WirkdState>,
    work_id: &WorkId,
    chain: &[wirk_core::EvidenceProjectionRef],
    wanted: &str,
) -> Result<AdmittedHandle, String> {
    let mut delivered: Option<wirk_core::ReachableEntry> = None;
    for reference in chain {
        let Ok(file) =
            wirk_core::ProjectionFile::read_referenced(&state.estate_root, work_id, reference)
        else {
            // A revision that cannot be delivered cannot vouch for a
            // handle. Skipped rather than fatal: an earlier revision may
            // still carry it, and the parent's own readability was
            // already checked by the caller.
            continue;
        };
        if let Some(entry) = file
            .content
            .reachable()
            .iter()
            .find(|entry| entry.handle == wanted)
        {
            delivered = Some(entry.clone());
            break;
        }
    }
    let Some(entry) = delivered else {
        return Err(format!(
            "no revision of this Run's own delivered context offers the handle `{wanted}`; a \
             handle is usable because this context delivered it, never because it is spelled \
             correctly"
        ));
    };
    let family = match entry.family.as_str() {
        "code" => wirk_atlas::ContentFamily::Code,
        "knowledge" => wirk_atlas::ContentFamily::Knowledge,
        "config" => wirk_atlas::ContentFamily::Config,
        other => {
            return Err(format!(
                "the delivered handle `{wanted}` names a content family this binary does not \
                 query (`{other}`)"
            ));
        }
    };
    Ok(AdmittedHandle {
        handle: entry.handle.clone(),
        source: entry.source.clone(),
        family,
    })
}

/// Assembles one expansion revision on top of `parent`.
///
/// **The captured vector is preserved, and that is the whole basis.**
/// `parent.generations` is re-admitted — each membership must still be
/// bound by this Work and still be readable at *the generation the
/// parent named* — and every read pins to it. Nothing here consults the
/// currently published generation of anything: a stage that was pinned
/// to a snapshot stays pinned to it, so a coordinate delivered at
/// revision 3 means exactly what the same coordinate meant at revision 0
/// and no revision ever attributes today's bytes to a historical
/// identity (ruling 0126, ruling 0128 F1). A generation that can no
/// longer be read back is an explicit `Omission::Unavailable`; a source
/// this Work no longer binds is an inadmissible count and no lookup.
///
/// Everything the parent delivered is carried forward in delivery order
/// and the new material is appended after it, because a revision is the
/// context the actor now has, whole — an actor reads one document, not a
/// diff it has to reassemble. What the new material *is*: the ranked
/// answer for the authored terms, plus the literal path and identifier
/// references those terms name, both resolved exactly the way step 3 and
/// step 7 of the initial assembly resolve them, and narrowed to one
/// source and family when a handle was given.
///
/// `retrieval` describes the query *this revision* ran, not the parent's:
/// each revision's note is about the query that produced its newest
/// material, and the parent revision remains readable with its own note.
#[allow(clippy::too_many_arguments)]
fn prepare_expansion(
    state: &Arc<WirkdState>,
    orient: &wirk_core::OrientationRequest,
    // This Work's own journal as this expansion observed it, for the
    // same reason the initial assembly takes it: a record raised since
    // the revision being expanded is this Work's own and belongs in this
    // revision's consulted set.
    events: &[Event],
    parent: &wirk_core::ProjectionContent,
    parent_ref: &wirk_core::EvidenceProjectionRef,
    run_id: &RunId,
    bindings: &[RepositoryBinding],
    ask: &ExpansionAsk,
    handle: Option<&AdmittedHandle>,
    laps: u32,
    started: std::time::Instant,
) -> PreparedProjection {
    no_journal_guard_held("stage projection expansion");

    // The terms. An actor that authored none is expanding a handle, and
    // the stage's own question stands in — said as a fact on the record
    // rather than presented as something the actor wrote.
    let authored_question = ask.question.is_some();
    let question = ask
        .question
        .clone()
        .unwrap_or_else(|| parent.question.clone());

    let mut bound = parent.bound.clone();
    // The *initial* assembly's ranked list, carried forward unchanged:
    // this revision's ranked material is `bound`, because this Run's own
    // actor asked for it by name rather than the assembler offering it.
    let referenced = parent.referenced.clone();
    // The consultation half of a projection is re-observed in full by
    // every revision — the consulted set, the index note, the coverage
    // contribution, the omission that records an unreadable index, and
    // the sentence that describes all four. So the parent's copies of
    // the last two are not carried into a document that made its own
    // observation: a revision that read a healthy index must not also
    // deliver a sentence saying the index could not be read, or an
    // omission saying it was unavailable, both of which are statements
    // about a read some *other* revision made. Nothing is erased — the
    // parent's own frozen document still says exactly what it said, and
    // is still readable at `--revision`. Everything else the parent
    // omitted or could not resolve is a fact about `bound` and the
    // captured vector, and is carried.
    let mut assumptions = parent.assumptions.clone();
    assumptions.retain(|statement| {
        statement.attributed_to != wirk_core::StatementOrigin::Assembly
            || !statement.text.contains(CONSULTED_STATEMENT_MARK)
    });
    let mut unknowns = parent.unknowns.clone();
    let mut omitted = parent.omitted.clone();
    omitted.retain(|item| {
        !matches!(
            item,
            wirk_core::Omission::Unavailable { reason, .. }
                if *reason == wirk_core::UnavailableReason::FindingsIndexUnreadable
        )
    });
    let mut added: Vec<wirk_core::EvidenceItem> = Vec::new();
    let mut inadmissible = 0usize;

    // What this context has already **bound**, so an expansion adds
    // rather than repeats. Keyed by the coordinate, which is the exact
    // (membership, generation, path, object, span) identity — not by
    // summary text, which two resources can share.
    //
    // Deliberately not seeded with `referenced`. The initial assembly
    // does not deduplicate the two lists against each other either, and
    // for the same reason: `referenced` is what the assembler *offered*
    // for the Route's question, and binding it is what this Run's own
    // actor asked for by name. Suppressing it here was the first
    // candidate's behavior and it made expanding a handle deliver
    // nothing at all whenever the ranked answer was the one the initial
    // assembly had already offered — the exact "decorative listing"
    // outcome the verb exists to avoid. Each item carries its own reason
    // in each list, so an honest reader can see it appear in both.
    let already: HashSet<String> = parent
        .bound
        .iter()
        .map(|item| item.coordinate.clone())
        .collect();

    let scope = wirk_atlas::QueryScope::Work(bindings.to_vec());
    let bound_aliases: HashSet<&str> = bindings
        .iter()
        .map(|binding| binding.name.as_str())
        .collect();

    let atlas = state
        .atlas
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    // Re-admit the parent's own captured vector. Note the order: the
    // *parent's* list decides which generations, this Work's *current*
    // bindings decide which are still admitted. A membership the Work no
    // longer binds is counted and never read; a generation that no
    // longer resolves is reported and never substituted.
    let mut admitted: Vec<(wirk_atlas::Membership, wirk_atlas::SourceGeneration)> = Vec::new();
    // Two counts, and they are not the same fact.
    //
    // `still_bound` is about the captured vector: does this Work still
    // bind the membership the vector names. That is settled from
    // bindings this loop already holds, for every captured member, at no
    // cost.
    //
    // `selected` is this expansion's own query scope: of those, the ones
    // a `--reference` handle names, or all of them when no handle
    // narrows the request. Only a selected membership has its pinned
    // generation looked up, which is what makes narrowing cheaper than
    // not narrowing — and it is also why a narrowed expansion cannot
    // report anything at all about a source it did not select, neither
    // as readable nor as unavailable.
    let mut still_bound = 0usize;
    let mut selected = 0usize;
    for (membership_id, generation_id) in &parent.generations {
        let membership = atlas
            .memberships()
            .find(|member| member.id.0 == *membership_id)
            .cloned();
        let Some(membership) = membership else {
            inadmissible += 1;
            continue;
        };
        if !bound_aliases.contains(membership.alias.as_str())
            || !(orient.sources.is_empty() || orient.sources.contains(&membership.alias))
        {
            inadmissible += 1;
            continue;
        }
        still_bound += 1;
        if let Some(handle) = handle
            && handle.source != membership.alias
        {
            // Outside this expansion's query scope. Nothing further is
            // read about it here: no generation lookup, no `Unavailable`
            // omission, no effect on this revision's coverage. Narrowing
            // is a choice about what to read, and a choice not to read a
            // source is not a finding about it.
            continue;
        }
        selected += 1;
        match atlas.generation(&wirk_atlas::GenerationId(generation_id.clone())) {
            Ok(generation) if generation.id.0 == *generation_id => {
                admitted.push((membership, generation))
            }
            _ => omitted.push(wirk_core::Omission::Unavailable {
                coordinate: membership.alias.clone(),
                reason: wirk_core::UnavailableReason::GenerationUnavailable,
            }),
        }
    }
    let pinned: BTreeMap<wirk_atlas::MembershipId, wirk_atlas::GenerationId> = admitted
        .iter()
        .map(|(membership, generation)| (membership.id.clone(), generation.id.clone()))
        .collect();

    // The authored references in the expansion's own text, resolved
    // exactly as the initial assembly resolves the Route's — an actor
    // that names a path gets that path, at the captured generation.
    // Only the actor's own words: the Waypoint's intent was already
    // resolved into the parent and re-resolving it would deliver the
    // same items twice under a new reason.
    let mut unresolved: Vec<Reference> = Vec::new();
    if authored_question {
        let references = authored_references(&[question.as_str()]);
        let identifiers: Vec<&str> = references
            .iter()
            .filter_map(|reference| match reference {
                Reference::Identifier(name) => Some(name.as_str()),
                Reference::Path(_) => None,
            })
            .collect();
        let mut resolved_identifiers =
            resolve_identifier_references(&atlas, &scope, &admitted, &pinned, &identifiers);
        for reference in &references {
            let hits = match reference {
                Reference::Path(path) => resolve_path_reference(&admitted, path, &mut omitted),
                Reference::Identifier(name) => resolved_identifiers
                    .remove(name.as_str())
                    .unwrap_or_default(),
            };
            if hits.is_empty() {
                unresolved.push(reference.clone());
                continue;
            }
            for hit in hits {
                added.push(wirk_core::EvidenceItem {
                    reason: format!(
                        "named literally by this Run's own expansion request, resolved at the \
                         captured generation this stage was pinned to (revision {})",
                        parent.revision + 1
                    ),
                    ..hit
                });
            }
        }
    }

    // The ranked answer for the terms, narrowed to the handle's source
    // and family when there is one. Same `wirk_atlas::search` the public
    // `wirk atlas search` runs, same configured backend the Route
    // recorded (ruling 0128 F3) — an expansion does not get to choose a
    // different ranker than the stage was configured with, and it
    // certainly does not pick a host one.
    let answer = wirk_atlas::search(
        &atlas,
        &wirk_atlas::SearchRequest {
            scope: scope.clone(),
            requested_source: handle.map(|handle| handle.source.clone()),
            query: question.clone(),
            families: handle.map(|handle| vec![handle.family]).unwrap_or_default(),
            semantic: wirk_atlas::SemanticRequest::Requested,
            limit: orient.budget.referenced(),
            // The stage's own authored capacity, carried into its
            // expansion the same way its rendering budget already is —
            // an expansion keeps the ranking function the initial
            // assembly ran, never a fresh one implied by the display
            // page it happens to ask for (ruling 0171, ruling 0172).
            capacity: orient.capacity.or(Some(wirk_atlas::CAPACITY_MAX)),
            pinned: Some(pinned.clone()),
            offset: 0,
            semantic_query: orient.semantic.as_ref().map(|configured| {
                wirk_atlas::SemanticQueryConfig {
                    backend: PathBuf::from(&configured.backend),
                    backend_args: configured.backend_args.clone(),
                    model: PathBuf::from(&configured.model),
                }
            }),
            pinned_editions: None,
            pinned_mode: None,
            pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
        },
    )
    .ok();
    let retrieval = retrieval_note(answer.as_ref(), orient.capacity);
    let ranked: Vec<wirk_core::EvidenceItem> = answer
        .as_ref()
        .map(|answer| {
            answer
                .hits
                .iter()
                .filter_map(|hit| ranked_item(&admitted, hit))
                .collect()
        })
        .unwrap_or_default();
    let ranked_returned = ranked.len();
    for item in ranked {
        added.push(wirk_core::EvidenceItem {
            reason: match handle {
                Some(handle) => format!(
                    "ranked for this Run's own expansion request inside the delivered handle \
                     `{}`, at the captured generation this stage was pinned to; being ranked \
                     here is not a statement that it answers the request",
                    handle.handle
                ),
                None => "ranked for this Run's own expansion request across every admitted \
                         source, at the captured generation this stage was pinned to; being \
                         ranked here is not a statement that it answers the request"
                    .to_string(),
            },
            ..item
        });
    }
    drop(atlas);

    // Step 6, re-observed for **this** revision (BUILD.md §9): the note
    // and its consulted set are frozen per revision, so an expansion
    // records its own rather than inheriting the parent's — a health
    // change is then visible as a difference between two frozen
    // documents rather than as a silent edit of one. Outside every
    // guard, for the same reentrancy reason the initial assembly runs it
    // outside every guard.
    let consulted = consult_findings(state, events, bindings, &parent.generations, &bound);
    if consulted.inadmissible > 0 {
        inadmissible += consulted.inadmissible;
    }
    if consulted.unreadable {
        omitted.push(wirk_core::Omission::Unavailable {
            coordinate: "the estate findings index".to_string(),
            reason: wirk_core::UnavailableReason::FindingsIndexUnreadable,
        });
    }

    // What is genuinely new. A resource the chain already delivered is
    // not delivered again — its coordinate resolves to the same bytes it
    // always did — and the count says so rather than the list quietly
    // being shorter than the query's own total.
    let offered = added.len();
    let mut seen: HashSet<String> = already;
    let mut fresh: Vec<wirk_core::EvidenceItem> = Vec::new();
    for item in added {
        if seen.insert(item.coordinate.clone()) {
            fresh.push(item);
        }
    }
    let repeated = offered - fresh.len();
    let delivered = fresh.len();
    bound.extend(fresh);

    let shown_unknowns = unresolved.len().min(ASSEMBLY_UNKNOWN_MAX);
    for reference in &unresolved[..shown_unknowns] {
        unknowns.push(wirk_core::Statement {
            text: unresolved_statement(reference),
            attributed_to: wirk_core::StatementOrigin::Intent,
        });
    }
    if unresolved.len() > shown_unknowns {
        omitted.push(wirk_core::Omission::OverBudget {
            of: "unknowns".to_string(),
            shown: shown_unknowns,
            total: unresolved.len(),
        });
    }
    if inadmissible > 0 {
        omitted.push(wirk_core::Omission::Inadmissible {
            count: inadmissible,
        });
    }

    assumptions.push(wirk_core::Statement {
        text: format!(
            "revision {revision} expands revision {parent_revision} of this same Run's delivered \
             context. Every earlier revision is unchanged and still readable at the bytes it was \
             delivered with; the World reserved for this Run, and its hash, are untouched — an \
             expansion adds a revision, it never edits one.",
            revision = parent.revision + 1,
            parent_revision = parent.revision,
        ),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    // What this sentence may claim is exactly what this expansion
    // actually looked at. Unnarrowed, that is the whole captured vector
    // and the original sentence is true of it. Narrowed, readability was
    // only ever established for the selected memberships, so the
    // sentence separates the three facts it really has — how many the
    // vector names, how many this Work still binds, and how many of the
    // ones this request selected were still readable — and says plainly
    // that the unselected rest were not examined here. It must never
    // present a scope choice as a source having gone away.
    assumptions.push(wirk_core::Statement {
        text: match handle {
            None => format!(
                "this expansion preserved the captured generation vector of the revision it \
                 expands and observed no new one: {admitted} of the {captured} membership(s) \
                 that vector names were still bound by this Work and still readable at the \
                 exact generation the vector named, and every read here pinned to those. \
                 Nothing was read at any source's currently published generation, so a \
                 coordinate delivered here means what it meant when this stage was reserved, at \
                 Atlas publication revision {publication}.",
                admitted = admitted.len(),
                captured = parent.generations.len(),
                publication = parent.publication_revision,
            ),
            Some(handle) => format!(
                "this expansion preserved the captured generation vector of the revision it \
                 expands and observed no new one: of the {captured} membership(s) that vector \
                 names, {still_bound} are still bound by this Work, and this expansion read \
                 only the {selected} of those that the delivered handle `{name}` names — \
                 {admitted} of them were still readable at the exact generation the vector \
                 named, and every read here pinned to those. The rest were left unread here: \
                 that is this expansion's own query scope, not a finding about whether they are \
                 readable. Nothing was read at any source's currently published generation, so \
                 a coordinate delivered here means what it meant when this stage was reserved, \
                 at Atlas publication revision {publication}.",
                captured = parent.generations.len(),
                still_bound = still_bound,
                selected = selected,
                name = handle.handle,
                admitted = admitted.len(),
                publication = parent.publication_revision,
            ),
        },
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    assumptions.push(wirk_core::Statement {
        text: format!(
            "the request produced {offered} candidate item(s); {delivered} were added and \
             {repeated} were already bound in this context at the same coordinate and were not \
             delivered a second time. {ranked_returned} came from ranked retrieval{narrowed}, \
             the rest from resolving the request's own literal path and identifier references. \
             Being here is not a statement that any of it answers the request.",
            narrowed = match handle {
                Some(handle) => format!(
                    " narrowed to the delivered handle `{}`, which this Run's own context \
                     offered",
                    handle.handle
                ),
                None => " across every source this projection is admitted to".to_string(),
            },
        ),
        attributed_to: wirk_core::StatementOrigin::Assembly,
    });
    // Re-authored, not carried: the parent's sentence describes the
    // parent's own read of the index, and this revision made its own.
    // The exact-duplicate collapse below keeps one copy when the two
    // observations happened to say the same thing.
    assumptions.push(consulted_statement(&consulted));
    if !authored_question {
        assumptions.push(wirk_core::Statement {
            text: "this expansion authored no question of its own: the terms it ranked with are \
                   this stage's own orientation question, carried over verbatim. Nothing here is \
                   attributed to the actor as words it wrote."
                .to_string(),
            attributed_to: wirk_core::StatementOrigin::Assembly,
        });
    }
    if let Some(reason) = retrieval.semantic_reason.as_deref() {
        assumptions.push(wirk_core::Statement {
            text: format!(
                "this revision's ranked retrieval ran in `{}` mode and its semantic status is \
                 `{}`: {reason}. Each revision's retrieval note describes the query that \
                 produced *its* newest material; the note the revision it expands was delivered \
                 with is unchanged and still readable there.",
                retrieval.mode, retrieval.semantic
            ),
            attributed_to: wirk_core::StatementOrigin::Assembly,
        });
    }

    // Two facts under one label, and only one of them is carried
    // (`reason_is_about_the_consultation`).
    //
    // The source half never improves by expanding: a reference the
    // parent could not resolve is still unresolved here, because `bound`
    // and the captured vector are inherited whole. It is rebuilt from
    // the omissions and unknowns this revision carries rather than read
    // off the parent's single reason, so a parent whose index was
    // unreadable does not take a genuine source limitation down with it
    // when that index recovers.
    //
    // The consultation half is not carried at all. `consulted` and
    // `findings_index` were re-observed above and replaced in full, so
    // this revision's own read is the only true statement about this
    // revision's consulted set, in both directions: a worse observation
    // must be recorded (found by running the real binary, not by reading
    // this), and so must a better one — carrying it was V1
    // (`loop-c4-consult-verify/VERDICT.md`).
    //
    // Presentation cannot reach either half: `truncated` and the
    // `OverBudget` omissions are a separate field and a separate
    // sentence (BUILD.md §4.7).
    let source = carried_source_coverage(parent.coverage, &omitted, &unknowns);
    let consultation = if consulted.unreadable {
        wirk_core::EvidenceCoverage::Degraded {
            reason: wirk_core::CoverageReason::FindingsIndexUnreadable,
        }
    } else if !consulted.note.complete {
        // An index that was synchronized when revision 0 read it and is
        // behind now makes revision 1 partial, which is exactly the
        // difference between two frozen records that BUILD.md §9 says a
        // health change must be visible as.
        wirk_core::EvidenceCoverage::Partial {
            reason: wirk_core::CoverageReason::IndexCannotAttestCompleteness,
        }
    } else {
        wirk_core::EvidenceCoverage::Complete
    };
    // The worse of the two, and the source half wins a tie: it is the
    // chain's own original reason and still true of this revision, and
    // it is the order the initial assembly already resolves these in.
    let coverage = if coverage_severity(consultation) > coverage_severity(source) {
        consultation
    } else {
        source
    };
    let truncated = omitted
        .iter()
        .any(|item| matches!(item, wirk_core::Omission::OverBudget { .. }));

    // Each expansion appends its own copy of the fixed Assembly-attributed
    // sentences ("authored no question of its own", the retrieval-mode
    // note, ...), and a long chain accumulates exact repeats of them while
    // `bound` itself stops growing. Collapsing an *exact* byte-identical
    // text under the same attribution to one copy loses nothing: order,
    // attribution and every non-duplicate statement — including a
    // revision's own Intent-origin unknowns, and any Assembly sentence
    // whose wording actually differs — are unchanged. Every already-written
    // revision file is a separate, immutable document; this only shapes
    // the one being assembled now.
    let mut seen_assembly_text: HashSet<String> = HashSet::new();
    assumptions.retain(|statement| {
        if statement.attributed_to != wirk_core::StatementOrigin::Assembly {
            return true;
        }
        seen_assembly_text.insert(statement.text.clone())
    });

    let content = wirk_core::ProjectionContent {
        format: wirk_core::PROJECTION_FORMAT.to_string(),
        compilation_policy: wirk_core::ASSEMBLY_POLICY.to_string(),
        route_edition: parent.route_edition.clone(),
        waypoint: parent.waypoint.clone(),
        revision: parent.revision + 1,
        // The stage's own orientation question, unchanged. What this
        // expansion asked is on the expansion record, where a reader can
        // tell the two apart.
        question: parent.question.clone(),
        generations: parent.generations.clone(),
        publication_revision: parent.publication_revision,
        retrieval,
        bound,
        referenced,
        reachable: parent.reachable.clone(),
        assumptions,
        unknowns: unknowns.clone(),
        omitted,
        next_action: next_action_for(coverage, unknowns.is_empty()),
        coverage,
        truncated,
        consulted: consulted.findings,
        findings_index: consulted.note,
        expansion: Some(wirk_core::ExpansionRecord {
            parent_projection: parent_ref.projection.clone(),
            parent_observation: parent_ref.observation.clone(),
            expanded_by: run_id.0.clone(),
            request: wirk_core::ExpansionRequest {
                question,
                authored_question,
                reference: handle.map(|handle| handle.handle.clone()),
                reason: ask.reason.clone(),
            },
            basis: wirk_core::ExpansionBasis::PreservedCapturedVector,
            delivered,
            already_bound: repeated,
        }),
    };
    finish_projection(content, ObservationSpan::measured(laps, started))
}
