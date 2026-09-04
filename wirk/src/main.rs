//! wirk binary entrypoint.
//!
//! `wirk claim` reads the injected triple from env (ruling 0001 D3, D5;
//! unchanged since the P0 spike), then W3 (0023 D81) makes it real:
//! locates the running wirkd via `WIRK_ESTATE_ROOT`'s pointer file
//! (`orient/transport.md` §3), files the Claim over the socket, and
//! prints the verdict wirkd journaled — no more triple-printing stub.
//! `wirk wirkd start|stop|ping` and `wirk work submit` are new this
//! wave: `start` runs the server loop (`wirkd::server::run`, blocking,
//! foreground) that binds the socket, writes the pointer file, and
//! serves; `stop`/`ping`/`submit` are thin clients dialing it
//! (`orient/build-brief.md` §3 W3).
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

// wirkd wire protocol (envelope, verb, payload types), the client
// (`locate`, `call`) that reaches a running wirkd, and the server loop
// itself (W2 `orient/transport.md` §2-4; W3 `orient/build-brief.md` §3).
mod wirkd;

use wirkd::{ClaimPayload, Reply, Request, SubmitPayload};

use wirk_core::{
    Access, ClaimId, ClaimKind, ClaimVerdict, DeterministicWorld, Event, EventId, EventKind,
    ExecutionTriple, Journal, JournalError, OutputContract, RepositoryBinding, RouteId, RunId,
    WaypointId, WorkId, WorkState, World, WorldHash,
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
        Some("claim") => claim(&args[2..]),
        Some("journal") => journal_command(&args[2..]),
        Some("wirkd") => wirkd_command(&args[2..]),
        Some("work") => work_command(&args[2..]),
        _ => {
            eprintln!(
                "usage: wirk claim | wirk journal demo <dir> | wirk wirkd start|stop|ping --estate <root> | wirk work submit --estate <root> --intent <text> --repo <name>:<read|write> --base <ref>"
            );
            ExitCode::FAILURE
        }
    }
}

/// Reads the injected triple from env, same as always (0001 D5); if any
/// variable is absent or blank, names each missing one on stderr and
/// exits 1 (usage) — nothing is printed to stdout, no wirkd contacted.
/// Otherwise parses `--artifact <name>=<path>` (repeatable) and
/// `--question <text>` (W3, build-brief.md §3 amendment 2; D87), locates
/// wirkd via `WIRK_ESTATE_ROOT`'s pointer file, files the Claim, and
/// prints the verdict wirkd journaled: `Validated` (exit 0) or
/// `Refused: <code> <message>` (exit 3). A transport or locate failure
/// (wirkd unreachable, pointer missing or malformed, a malformed reply)
/// is exit 2, the error printed to stderr.
fn claim(args: &[String]) -> ExitCode {
    let mut artifacts: BTreeMap<String, String> = BTreeMap::new();
    let mut question: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--artifact" => {
                i += 1;
                let Some(pair) = args.get(i) else {
                    return claim_usage();
                };
                let Some((name, path)) = pair.split_once('=') else {
                    return claim_usage();
                };
                artifacts.insert(name.to_string(), path.to_string());
            }
            "--question" => {
                i += 1;
                let Some(text) = args.get(i) else {
                    return claim_usage();
                };
                question = Some(text.clone());
            }
            _ => return claim_usage(),
        }
        i += 1;
    }

    let mut missing = Vec::new();
    let mut triple: BTreeMap<&str, String> = BTreeMap::new();
    for name in TRIPLE_VARS {
        match env::var(name).ok().filter(|v| !v.trim().is_empty()) {
            Some(value) => {
                triple.insert(name, value);
            }
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        for name in &missing {
            eprintln!("wirk claim: missing {name}");
        }
        return ExitCode::from(1);
    }
    let estate_root = triple["WIRK_ESTATE_ROOT"].clone();

    let pointer = match wirkd::client::locate(Path::new(&estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk claim: {err}");
            return ExitCode::from(2);
        }
    };

    let kind = match question {
        Some(text) => ClaimKind::Question(text),
        None => ClaimKind::Done,
    };
    let payload = ClaimPayload {
        triple: ExecutionTriple {
            estate_root,
            work_id: WorkId(triple["WIRK_WORK_ID"].clone()),
            run_id: RunId(triple["WIRK_RUN_ID"].clone()),
        },
        kind,
        artifacts,
    };

    match wirkd::client::call(&pointer.socket, &Request::claim(payload)) {
        Ok(Reply::Ok { result, .. }) => {
            let _ = result;
            println!("Validated");
            ExitCode::SUCCESS
        }
        Ok(Reply::Err { error, .. }) => {
            println!("Refused: {} {}", error.code, error.message);
            ExitCode::from(3)
        }
        Err(err) => {
            eprintln!("wirk claim: {err}");
            ExitCode::from(2)
        }
    }
}

fn claim_usage() -> ExitCode {
    eprintln!("usage: wirk claim [--artifact NAME=PATH]... [--question TEXT]");
    ExitCode::from(1)
}

// ---- wirkd / work submit (W3, orient/build-brief.md §3) -------------

/// Dispatches `wirk wirkd <rest>`: `start --estate <root>` runs the
/// server loop in the foreground (blocking; the caller backgrounds it);
/// `stop`/`ping --estate <root>` are thin clients.
fn wirkd_command(rest: &[String]) -> ExitCode {
    let Some(sub) = rest.first().map(String::as_str) else {
        return wirkd_usage();
    };
    let Some(estate) = flag_value(&rest[1..], "--estate") else {
        return wirkd_usage();
    };
    match sub {
        "start" => match wirkd::server::run(PathBuf::from(estate)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("wirk wirkd start: {err}");
                ExitCode::from(2)
            }
        },
        "stop" => wirkd_client_call(&estate, &Request::stop(), |_| {
            println!("stopped");
        }),
        "ping" => wirkd_client_call(&estate, &Request::ping(), |result| {
            println!(
                "protocol_version {} pid {}",
                result["protocol_version"].as_u64().unwrap_or_default(),
                result["pid"].as_u64().unwrap_or_default()
            );
        }),
        _ => wirkd_usage(),
    }
}

fn wirkd_usage() -> ExitCode {
    eprintln!("usage: wirk wirkd start|stop|ping --estate <root>");
    ExitCode::from(1)
}

/// Locates wirkd at `estate`, sends `request`, and on an `ok` reply
/// hands its `result` to `on_ok` for the subcommand's own printing.
/// Locate/transport failure is exit 2; a `{"ok":false,...}` reply (only
/// `ping`/`stop`/`submit` use this helper, none of which wirkd ever
/// refuses) is printed verbatim and treated as exit 2 too.
fn wirkd_client_call(
    estate: &str,
    request: &Request,
    on_ok: impl FnOnce(&serde_json::Value),
) -> ExitCode {
    let pointer = match wirkd::client::locate(Path::new(estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk wirkd: {err}");
            return ExitCode::from(2);
        }
    };
    match wirkd::client::call(&pointer.socket, request) {
        Ok(Reply::Ok { result, .. }) => {
            on_ok(&result);
            ExitCode::SUCCESS
        }
        Ok(Reply::Err { error, .. }) => {
            eprintln!("wirk wirkd: {} {}", error.code, error.message);
            ExitCode::from(2)
        }
        Err(err) => {
            eprintln!("wirk wirkd: {err}");
            ExitCode::from(2)
        }
    }
}

/// Dispatches `wirk work <rest>`: `submit --estate <root> --intent
/// <text> --repo <name>:<read|write> (repeatable) --base <ref>`.
fn work_command(rest: &[String]) -> ExitCode {
    if rest.first().map(String::as_str) != Some("submit") {
        return work_usage();
    }
    let rest = &rest[1..];
    let Some(estate) = flag_value(rest, "--estate") else {
        return work_usage();
    };
    let Some(intent) = flag_value(rest, "--intent") else {
        return work_usage();
    };
    let base_ref = flag_value(rest, "--base").unwrap_or_default();

    let mut repositories = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--repo" {
            i += 1;
            let Some(spec) = rest.get(i) else {
                return work_usage();
            };
            let Some((name, mode)) = spec.split_once(':') else {
                return work_usage();
            };
            let access = match mode.to_ascii_lowercase().as_str() {
                "read" => Access::Read,
                "write" => Access::Write,
                _ => return work_usage(),
            };
            repositories.push(RepositoryBinding {
                name: name.to_string(),
                access,
            });
        }
        i += 1;
    }

    let payload = SubmitPayload {
        intent,
        repositories,
        base_ref,
    };
    wirkd_client_call(&estate, &Request::submit(payload), |result| {
        println!(
            "work_id {} run_id {} waypoint {}",
            result["work_id"].as_str().unwrap_or_default(),
            result["run_id"].as_str().unwrap_or_default(),
            result["waypoint"].as_str().unwrap_or_default()
        );
    })
}

fn work_usage() -> ExitCode {
    eprintln!(
        "usage: wirk work submit --estate <root> --intent <text> --repo <name>:<read|write> --base <ref>"
    );
    ExitCode::from(1)
}

/// Returns the value following `flag` in `args`, or `None` if the flag
/// is absent or has no following value (R6: the one shared parsing move
/// every subcommand's `--estate`/`--intent`/`--base` needs).
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
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
