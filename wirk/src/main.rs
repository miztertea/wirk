//! wirk binary entrypoint.
//!
//! `wirk claim` is a stub for the P0 Herdr spike (ruling 0001 D3, D9#4;
//! brief `p0-skeleton` W2 "What triple means"): its only purpose is to
//! prove, running inside a Herdr pane, that the execution triple Herdr
//! injects into the pane's env at creation is inherited by a process
//! wirkd launches there. No validation, no journal, no Herdr call, no
//! other subcommand — those arrive with the claim contract (plan item 4).
//!
//! `wirk journal demo <dir>` is item 2's tried step (ruling 0028 D93,
//! `knowledge/work/p1-journal/orient/store.md` §6): glue over
//! `wirk_core::Journal`/`fold`, no new type (build-brief.md §5). On a
//! directory with no journal it appends the six-event lifecycle
//! (`orient/fold.md` §1) that carries a fresh Work from `Pending` to
//! `Completed`; on a directory already holding one it replays and
//! prints the folded `Work`. `--pause-after N` appends N events then
//! blocks on a bounded poll for `<dir>/continue` (W3 build-brief
//! amendment 2: "the kill is deterministic, no tuned sleep" — issue
//! 359's shape, fixed here rather than a timed sleep in the killing
//! script) so a verifier can `SIGKILL` the process mid-sequence with an
//! exact, reproducible line count.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wirk_core::{
    Access, ClaimId, ClaimKind, ClaimVerdict, DeterministicWorld, Event, EventId, EventKind,
    Journal, JournalError, OutputContract, RepositoryBinding, RouteId, RunId, WaypointId, WorkId,
    WorkState, World, WorldHash,
};

/// The injected execution triple: ruling 0001 D3 ("the execution
/// identity injected into the pane env at creation"), names fixed by
/// D5 (`WIRK_ESTATE_ROOT`, `WIRK_WORK_ID`, `WIRK_RUN_ID`), shape from
/// the predecessor's causation contract (sergeant-rs v0.3.0, W1
/// hierarchical execution contract §6:
/// `SERGEANT_ESTATE_ROOT`/`SERGEANT_WORK_ID`/`SERGEANT_EXECUTION_ID`,
/// "a transport hint, not trusted lineage"). Order is print order.
const TRIPLE_VARS: [&str; 3] = ["WIRK_ESTATE_ROOT", "WIRK_WORK_ID", "WIRK_RUN_ID"];

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("claim") => claim(),
        Some("journal") => journal_command(&args[2..]),
        _ => {
            eprintln!("usage: wirk claim");
            ExitCode::FAILURE
        }
    }
}

/// Print each variable of the injected triple as `NAME=value`, one per
/// line, in `TRIPLE_VARS` order, and exit 0. If any is absent, name
/// every missing one on stderr (`wirk claim: missing NAME`) and exit
/// nonzero — nothing is printed to stdout in that case.
///
/// Read exactly the way the predecessor's CLI reads its causation env
/// (sergeant-rs v0.3.0, W1 hierarchical execution contract §6,
/// `claimed_causation`/`origin`, R5): `std::env::var`, with an empty
/// value treated as absent so an exported-but-blank variable still
/// counts as missing, never as `""`.
fn claim() -> ExitCode {
    let mut missing = Vec::new();
    let mut lines = Vec::new();
    for name in TRIPLE_VARS {
        match env::var(name).ok().filter(|v| !v.trim().is_empty()) {
            Some(value) => lines.push(format!("{name}={value}")),
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        for name in &missing {
            eprintln!("wirk claim: missing {name}");
        }
        return ExitCode::FAILURE;
    }
    for line in lines {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

// ---- journal demo (item 2, ruling 0028 D93) --------------------------

/// A verifier polls at this interval for the `<dir>/continue` signal
/// file, never for longer than `CONTINUE_POLL_TIMEOUT` — a bounded poll
/// on a signal file, not a tuned sleep guessing how long a kill takes
/// (build-brief.md §8 amendment 2, issue 359).
const CONTINUE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CONTINUE_POLL_TIMEOUT: Duration = Duration::from_secs(60);

/// Dispatches `wirk journal <rest>`. Only `demo <dir> [--pause-after
/// N]` is defined; anything else is a usage error, same shape as
/// `claim`'s (one line to stderr, `ExitCode::FAILURE`).
fn journal_command(rest: &[String]) -> ExitCode {
    match rest {
        [sub, dir] if sub == "demo" => journal_demo(dir, None),
        [sub, dir, flag, n] if sub == "demo" && flag == "--pause-after" => match n.parse::<usize>()
        {
            Ok(n) => journal_demo(dir, Some(n)),
            Err(_) => journal_usage(),
        },
        _ => journal_usage(),
    }
}

fn journal_usage() -> ExitCode {
    eprintln!("usage: wirk journal demo <dir> [--pause-after N]");
    ExitCode::FAILURE
}

/// On an empty journal, appends the six-event lifecycle (`orient/
/// fold.md` §1) that carries a fresh Work from `Pending` through
/// `Completed`, printing one line per appended event. On a
/// non-empty journal, replays and folds it, printing the rebuilt
/// `Work`'s id/state/current_waypoint and the number of events
/// replayed. A malformed or seq-discontinuous journal — caught by
/// `Journal::open`'s own scan (store.md §2: "fails closed... a
/// corrupted journal never opens silently as if it were empty") or by
/// `replay` — prints its `JournalError` to stderr and exits 2.
fn journal_demo(dir: &str, pause_after: Option<usize>) -> ExitCode {
    let mut journal = match Journal::open(dir) {
        Ok(journal) => journal,
        Err(err) => return journal_error(&err),
    };
    let events = match journal.replay() {
        Ok(events) => events,
        Err(err) => return journal_error(&err),
    };
    if events.is_empty() {
        append_demo_sequence(&mut journal, dir, pause_after)
    } else {
        print_replayed_work(&events);
        ExitCode::SUCCESS
    }
}

fn journal_error(err: &JournalError) -> ExitCode {
    eprintln!("{err}");
    ExitCode::from(2)
}

/// Appends `demo_events()` one at a time, printing one line per append
/// (BRIEF outcome). When `pause_after` names a count already reached,
/// blocks on `wait_for_continue` before appending the next event — the
/// deterministic kill point a verifier's `SIGKILL` targets.
fn append_demo_sequence(journal: &mut Journal, dir: &str, pause_after: Option<usize>) -> ExitCode {
    if pause_after == Some(0) {
        wait_for_continue(dir);
    }
    for (index, event) in demo_events().into_iter().enumerate() {
        let kind_name = event_kind_name(&event.kind);
        if let Err(err) = journal.append(&event) {
            return journal_error(&err);
        }
        let appended = index + 1;
        println!("appended {appended} {kind_name}");
        if pause_after == Some(appended) {
            wait_for_continue(dir);
        }
    }
    ExitCode::SUCCESS
}

/// Polls for `<dir>/continue` every `CONTINUE_POLL_INTERVAL`, for at
/// most `CONTINUE_POLL_TIMEOUT` — a bound so a signal that never
/// arrives cannot hang the process forever, not a substitute for the
/// signal itself (build-brief.md §8 amendment 2).
fn wait_for_continue(dir: &str) {
    let signal = Path::new(dir).join("continue");
    let deadline = Instant::now() + CONTINUE_POLL_TIMEOUT;
    while !signal.exists() && Instant::now() < deadline {
        std::thread::sleep(CONTINUE_POLL_INTERVAL);
    }
}

/// The six-event lifecycle from `orient/build-brief.md` §2:
/// `WorkSubmitted` (one waypoint) -> `WaypointReserved` -> `RunOpened`
/// -> `RunLaunched` -> `ClaimFiled` -> `ClaimRecorded{Done,Validated}`
/// on that (last) waypoint, ending `Completed`. `EventId`s are left
/// empty; `Journal::append` mints a ULID into each (store.md §2).
fn demo_events() -> Vec<Event> {
    let work = WorkId("work-1".to_string());
    let route = RouteId("demo-route".to_string());
    let waypoint = WaypointId("demo-route/wp-1".to_string());
    let run = RunId("run-1".to_string());
    let claim = ClaimId("claim-1".to_string());

    let world = World::Deterministic(DeterministicWorld {
        command: vec!["true".to_string()],
        cwd: PathBuf::from("."),
        env: BTreeMap::new(),
        expected_artifacts: OutputContract(Vec::new()),
    });
    let world_hash = WorldHash::of(&world);

    vec![
        new_event(
            &work,
            None,
            EventKind::WorkSubmitted {
                route: route.clone(),
                repositories: vec![RepositoryBinding {
                    name: "wirk".to_string(),
                    access: Access::Write,
                }],
                intent: "demo the journal lifecycle".to_string(),
                waypoints: vec![waypoint.clone()],
            },
        ),
        new_event(
            &work,
            None,
            EventKind::WaypointReserved {
                waypoint: waypoint.clone(),
                world_hash: world_hash.clone(),
                world,
            },
        ),
        new_event(
            &work,
            Some(run.clone()),
            EventKind::RunOpened {
                run: run.clone(),
                waypoint,
                attempt: 1,
                world_hash,
            },
        ),
        new_event(
            &work,
            Some(run.clone()),
            EventKind::RunLaunched { run: run.clone() },
        ),
        new_event(
            &work,
            Some(run.clone()),
            EventKind::ClaimFiled {
                claim: claim.clone(),
            },
        ),
        new_event(
            &work,
            Some(run),
            EventKind::ClaimRecorded {
                claim,
                claim_kind: ClaimKind::Done,
                verdict: ClaimVerdict::Validated,
            },
        ),
    ]
}

fn new_event(work: &WorkId, run: Option<RunId>, kind: EventKind) -> Event {
    Event {
        id: EventId(String::new()),
        work: work.clone(),
        run,
        at: wirk_core_timestamp_now(),
        kind,
    }
}

fn wirk_core_timestamp_now() -> wirk_core::Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    wirk_core::Timestamp(ms as i64)
}

fn event_kind_name(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::LifecycleObserved { .. } => "LifecycleObserved",
        EventKind::RunFailed { .. } => "RunFailed",
        EventKind::RunVanished => "RunVanished",
        EventKind::ClaimFiled { .. } => "ClaimFiled",
        EventKind::ClaimRecorded { .. } => "ClaimRecorded",
        EventKind::WorktreeCreated { .. } => "WorktreeCreated",
        EventKind::WorkSubmitted { .. } => "WorkSubmitted",
        EventKind::WaypointReserved { .. } => "WaypointReserved",
        EventKind::RunOpened { .. } => "RunOpened",
        EventKind::RunLaunched { .. } => "RunLaunched",
        EventKind::WorkFailed { .. } => "WorkFailed",
        EventKind::WorkCanceled { .. } => "WorkCanceled",
    }
}

/// Prints `work <id> state <state> current_waypoint <.. | none> events
/// <n>` (BRIEF outcome's exact shape) and exits 0.
fn print_replayed_work(events: &[Event]) {
    let count = events.len();
    let work = wirk_core::fold(events);
    let waypoint = match &work.current_waypoint {
        Some(waypoint) => waypoint.0.as_str(),
        None => "none",
    };
    println!(
        "work {} state {} current_waypoint {} events {}",
        work.id.0,
        work_state_name(work.state),
        waypoint,
        count,
    );
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
