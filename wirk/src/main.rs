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
//! `wirk run-deterministic --estate <root> --work <id> --executor
//! child|docker` is item 5's own W3 (`orient/build-brief.md` §3 W3):
//! reads the reserved `World` for `work_id`'s current Waypoint back
//! from wirkd's own `status` verb (never recompiles it), launches it
//! through the chosen `ChildExecutor`/`DockerExecutor`, blocks once on
//! the child's own exit (`ChildExecutor::wait`/`DockerExecutor::wait`,
//! ruling 0044: no poll loop, no timeout), then reads wirkd's `status`
//! exactly once to learn `claimed` (exit 0) or `failed` (exit 5) —
//! named `run-deterministic`, not `run`, so it does not clash with item
//! 4's own `wirk run` on a sibling branch (build-brief outcome). `wirk
//! work submit --kind deterministic --command <argv...>` is the
//! additive flag that reserves a `World::Deterministic` for it to read
//! back, kept minimal on purpose to reduce that same merge.
//!
//! `wirk journal demo <dir>` is item 2's tried step (ruling 0028 D93,
//! `knowledge/work/p1-journal/orient/store.md` §6): glue over
//! `wirk_core::Journal`/`fold`, no new type (build-brief.md §5). On a
//! directory with no journal it appends the six-event lifecycle
//! (`orient/fold.md` §1) that carries a fresh Work from `Pending` to
//! `Completed`; on a directory already holding one it replays and
//! prints the folded `Work`. `--pause-after N` appends N events then
//! blocks (ruling 0044: no poll, no timeout) opening `<dir>/continue`
//! as a FIFO for reading — a verifier's own write to it is the signal —
//! so a verifier can `SIGKILL` the process mid-sequence with an exact,
//! reproducible line count.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

// wirkd wire protocol (envelope, verb, payload types), the client
// (`locate`, `call`) that reaches a running wirkd, and the server loop
// itself (W2 `orient/transport.md` §2-4; W3 `orient/build-brief.md` §3).
mod wirkd;

// Deterministic (child/docker) executors, wirk-owned per 0001 D4, in
// the `wirk` bin per 0022 D78 (no fifth crate). W1 (item 5 build-brief.md
// §3): `ChildExecutor`. `DockerExecutor` is W2.
mod executors;

// `wirk run` (item 4, W3, `knowledge/work/p1-herdr-executor/orient/
// build-brief.md`): drives one Actor Waypoint's Run against a live
// Herdr session and wirkd via `wirk_herdr::run_loop::RunLoop`.
mod executor;

use wirkd::{ClaimPayload, FailPayload, Reply, Request, StatusPayload, SubmitPayload};

use wirk_core::{
    Access, ClaimId, ClaimKind, ClaimVerdict, DeterministicWorld, Event, EventId, EventKind,
    ExecutionTriple, Executor, FailureCause, Journal, JournalError, OutputContract,
    RepositoryBinding, RouteId, Run, RunId, RunObservation, WaypointId, WorkId, WorkState, World,
    WorldHash,
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
        Some("run-deterministic") => run_deterministic_command(&args[2..]),
        Some("run") => executor::run_command(&args[2..]),
        Some("plugin") => plugin_command(&args[2..]),
        _ => {
            eprintln!(
                "usage: wirk claim | wirk journal demo <dir> | wirk wirkd start|stop|ping|status|watch --estate <root> [--work <id>] | wirk work submit --estate <root> --intent <text> --repo <name>:<read|write> --base <ref> [--route <name>] [--kind actor|deterministic --repo-path <path> | --command <argv...>] | wirk work status --estate <root> --work <id> | wirk run --estate <root> --work <id> --session <name> [--herdr-socket <path>] [--actor-kind claude|opencode] | wirk run-deterministic --estate <root> --work <id> --executor child|docker | wirk plugin init --estate <root>"
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
        // The 0035 follow-up (ruling 0034 D118: "`wirk wirkd status` as
        // a CLI verb does not exist... carried to item 8 W1"): a thin
        // client over the existing `status` wire verb (R6, same shape
        // as `ping`/`stop`), naming every Work under the estate when
        // `--work` is absent, or just the one when it's given — the
        // manifest's own `wirkd-status` action names this verb.
        "status" => wirkd_status_command(&estate, flag_value(&rest[1..], "--work")),
        // Item B/G, ruling 0044: prints one line per journal append,
        // starting with what is already there, blocking (no timeout) for
        // more — the herdr-plugin status pane's own program (G). `--work
        // <id>` streams that one Work; absent, streams **every current
        // Work's** appends (not the estate's own status changes — no
        // single wirkd verb reports "the estate changed" as a stream,
        // only a Work's journal; `wirkd::server::handle_watch_connection`
        // is scoped per-Work, so covering "every Work" here means one
        // watch connection per Work id found under `<estate>/works/` at
        // start, merged onto one stdout — a Work submitted after this
        // command starts is not picked up, the one real limitation this
        // shape carries, named rather than silently accepted).
        "watch" => wirkd_watch_command(&estate, flag_value(&rest[1..], "--work")),
        _ => wirkd_usage(),
    }
}

fn wirkd_usage() -> ExitCode {
    eprintln!("usage: wirk wirkd start|stop|ping|status|watch --estate <root> [--work <id>]");
    ExitCode::from(1)
}

/// `wirk wirkd watch --estate <root> [--work <id>]`: opens one `watch`
/// connection per named (or discovered) Work id and prints one line per
/// `Event` it carries — `work_id kind {...event json...}` — as they
/// arrive, blocking between lines (no poll, no timeout, ruling 0044).
/// Never returns on its own: it ends only when every watched
/// connection's iterator ends (wirkd stopped, or every named Work's
/// connection was refused up front) or the process is killed, matching
/// the plugin pane's own "the pane program ends; that is the state"
/// contract (item G). `--estate` naming a wirkd that is not running
/// prints why and exits 2 immediately, rather than blocking on a
/// connection that will never come.
fn wirkd_watch_command(estate: &str, work_filter: Option<String>) -> ExitCode {
    let pointer = match wirkd::client::locate(Path::new(estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk wirkd watch: {err}");
            return ExitCode::from(2);
        }
    };
    let work_ids: Vec<String> = match work_filter {
        Some(id) => vec![id],
        None => match list_work_ids(Path::new(estate)) {
            Ok(ids) => ids,
            Err(err) => {
                eprintln!("wirk wirkd watch: {err}");
                return ExitCode::from(2);
            }
        },
    };
    if work_ids.is_empty() {
        eprintln!("wirk wirkd watch: no Work under {estate}/works to watch");
        return ExitCode::from(2);
    }

    // One reader thread per watched Work, one shared channel every
    // thread's lines funnel into — the same "many readers, one channel
    // the caller blocks on" shape item A's `RunLoop` uses for Herdr plus
    // wirkd, applied here to N Work watches instead of two fixed
    // streams.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut handles = Vec::new();
    for work_id in work_ids {
        let socket = pointer.socket.clone();
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            let events = match wirkd::client::watch(
                &socket,
                wirkd::WatchPayload {
                    work_id: WorkId(work_id.clone()),
                },
            ) {
                Ok(events) => events,
                Err(err) => {
                    let _ = tx.send(format!("{work_id} watch_error {err}"));
                    return;
                }
            };
            for event in events {
                match event {
                    Ok(event) => {
                        let line = serde_json::to_string(&event)
                            .unwrap_or_else(|_| "<unserializable event>".to_string());
                        if tx.send(format!("{work_id} {line}")).is_err() {
                            return;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(format!("{work_id} watch_error {err}"));
                        return;
                    }
                }
            }
        }));
    }
    drop(tx); // this thread's own copy: `rx` ends once every reader thread's clone is dropped

    // Blocks on the channel — no timeout, ruling 0044: ends only when
    // every reader thread above has returned (every watched connection
    // closed).
    for line in rx {
        println!("{line}");
    }
    for handle in handles {
        let _ = handle.join();
    }
    ExitCode::SUCCESS
}

/// `wirk wirkd status --estate <root> [--work <id>]` and its `wirk work
/// status` alias: prints wirkd's `status` verb reply for `work_id`
/// alone, or (no `--work`) for every Work directory under
/// `<estate>/works/` (`server.rs`'s own `journal_for` layout, 0033
/// D101), one line each, oldest-directory-name-order first (`sort`, R6
/// — no journal timestamp read needed for a listing). A locate/
/// transport failure is exit 2, same as `wirkd_client_call`; a `status`
/// refusal for one Work id (`NotFound`, a fabricated id passed via
/// `--work`) is printed on stderr and folded into the same exit 2
/// rather than aborting the rest of the listing.
fn wirkd_status_command(estate: &str, work_filter: Option<String>) -> ExitCode {
    let pointer = match wirkd::client::locate(Path::new(estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk wirkd status: {err}");
            return ExitCode::from(2);
        }
    };
    let work_ids: Vec<String> = match work_filter {
        Some(id) => vec![id],
        None => match list_work_ids(Path::new(estate)) {
            Ok(ids) => ids,
            Err(err) => {
                eprintln!("wirk wirkd status: {err}");
                return ExitCode::from(2);
            }
        },
    };

    let mut exit = ExitCode::SUCCESS;
    for work_id in work_ids {
        let reply = wirkd::client::call(
            &pointer.socket,
            &Request::status(StatusPayload {
                work_id: WorkId(work_id.clone()),
            }),
        );
        match reply {
            Ok(Reply::Ok { result, .. }) => {
                println!(
                    "work_id {} state {} current_waypoint {}",
                    work_id,
                    result["state"].as_str().unwrap_or("?"),
                    result["current_waypoint"].as_str().unwrap_or("-")
                );
            }
            Ok(Reply::Err { error, .. }) => {
                eprintln!(
                    "wirk wirkd status: {work_id} {} {}",
                    error.code, error.message
                );
                exit = ExitCode::from(2);
            }
            Err(err) => {
                eprintln!("wirk wirkd status: {work_id} {err}");
                exit = ExitCode::from(2);
            }
        }
    }
    exit
}

/// Every Work id under `<estate>/works/` (directory names, `server.rs`'s
/// own `journal_for` layout) — `wirkd_status_command`'s own listing
/// source when `--work` is absent. An absent `works/` directory (no
/// Work ever submitted) is an empty list, not an error.
fn list_work_ids(estate: &Path) -> Result<Vec<String>, String> {
    let dir = estate.join("works");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut ids: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        if entry.path().is_dir()
            && let Ok(name) = entry.file_name().into_string()
        {
            ids.push(name);
        }
    }
    ids.sort();
    Ok(ids)
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
/// <text> --repo <name>:<read|write> (repeatable) --base <ref>
/// [--kind actor|deterministic --repo-path <path>]`. `--kind`/
/// `--repo-path` are item 4's W3 addition (additive: omitted, `submit`
/// behaves exactly as before — the original hardcoded "smoke" World,
/// unresolved `base_ref`).
fn work_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("submit") => work_submit_command(&rest[1..]),
        // Item 8 (0035 follow-up, `orient/build-brief.md` §3 W1): an
        // alias for `wirk wirkd status --estate <root> --work <id>`,
        // named on the manifest's own `wirkd-status` action.
        Some("status") => {
            let Some(estate) = flag_value(&rest[1..], "--estate") else {
                return work_usage();
            };
            let Some(work_id) = flag_value(&rest[1..], "--work") else {
                return work_usage();
            };
            wirkd_status_command(&estate, Some(work_id))
        }
        _ => work_usage(),
    }
}

fn work_submit_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return work_usage();
    };
    let Some(intent) = flag_value(rest, "--intent") else {
        return work_usage();
    };
    let base_ref = flag_value(rest, "--base").unwrap_or_default();

    let mut repositories = Vec::new();
    let mut command: Option<Vec<String>> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--repo" => {
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
            // `--command` consumes every remaining argument as the
            // command argv verbatim (`--kind deterministic --command
            // sh -c 'echo x > report.md'`): a deterministic command may
            // itself carry flag-shaped words, so `--command` must be
            // the last flag on the line, never interleaved with
            // `--repo`/`--kind`/etc. (additive, kept minimal per the
            // task to reduce a merge with item 4's own `submit`
            // changes). An optional `--` fence right after
            // `--command` marks the argv explicitly; without a fence,
            // any of `work submit`'s own flags among the remaining
            // arguments makes the argv and the submit flags
            // untellable apart, so the usage line and exit 1 go out
            // before the payload is built or wirkd is called.
            "--command" => {
                let remaining = &rest[i + 1..];
                let fenced = remaining.first().is_some_and(|arg| arg == "--");
                if !fenced
                    && [
                        "--estate",
                        "--intent",
                        "--repo",
                        "--base",
                        "--route",
                        "--kind",
                        "--repo-path",
                    ]
                    .iter()
                    .any(|flag| remaining.iter().any(|arg| arg == *flag))
                {
                    return work_usage();
                }
                command = Some(if fenced { &remaining[1..] } else { remaining }.to_vec());
                break;
            }
            _ => {}
        }
        i += 1;
    }

    let kind = flag_value(rest, "--kind");
    let repo_path = flag_value(rest, "--repo-path");
    let route = flag_value(rest, "--route");

    let payload = SubmitPayload {
        intent,
        repositories,
        base_ref,
        kind,
        command,
        repo_path,
        route,
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
        "usage: wirk work submit --estate <root> --intent <text> --repo <name>:<read|write> --base <ref> [--route <name>] [--kind actor|deterministic --repo-path <path> | --command <argv...>] | wirk work status --estate <root> --work <id>"
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

// ---- plugin init (item 7 W1, herdr-plugin.toml's operator setup) -----

/// Dispatches `wirk plugin <rest>`: `init --estate <root>` is the only
/// subcommand. It writes `<root>` as one line into
/// `$HERDR_PLUGIN_CONFIG_DIR/estate`, the file `plugin/startup.sh` and
/// the manifest's `submit`/`wirkd-status` commands read (R6: one write,
/// the operator-blocker fix named by this item's build brief §2 "the
/// operator blocker dissolves by design"). Refuses to run outside a
/// Herdr plugin invocation, where `HERDR_PLUGIN_CONFIG_DIR` is unset —
/// there is nothing to configure otherwise.
fn plugin_command(rest: &[String]) -> ExitCode {
    if rest.first().map(String::as_str) != Some("init") {
        return plugin_usage();
    }
    let rest = &rest[1..];
    let Some(estate) = flag_value(rest, "--estate") else {
        return plugin_usage();
    };
    let Ok(config_dir) = env::var("HERDR_PLUGIN_CONFIG_DIR") else {
        eprintln!(
            "wirk plugin init: HERDR_PLUGIN_CONFIG_DIR is not set (run inside a Herdr plugin action)"
        );
        return ExitCode::from(2);
    };
    let config_dir = PathBuf::from(config_dir);
    if let Err(err) = std::fs::create_dir_all(&config_dir) {
        eprintln!("wirk plugin init: {err}");
        return ExitCode::from(2);
    }
    let path = config_dir.join("estate");
    if let Err(err) = std::fs::write(&path, format!("{estate}\n")) {
        eprintln!("wirk plugin init: {err}");
        return ExitCode::from(2);
    }
    println!("wrote estate root to {}", path.display());
    ExitCode::SUCCESS
}

fn plugin_usage() -> ExitCode {
    eprintln!("usage: wirk plugin init --estate <root>");
    ExitCode::from(1)
}

// ---- run-deterministic (item 5 W3, orient/build-brief.md §3 W3) ------

/// Dispatches `wirk run-deterministic --estate <root> --work <id>
/// --executor child|docker` (module doc). Reads the reserved `World`
/// for `work_id`'s current Waypoint from wirkd's own `status` verb,
/// refusing anything but a `World::Deterministic` (item 4's own actor
/// executors, `wirk run`, are out of this command's scope), launches it
/// through the chosen executor, then drives a bounded poll loop
/// (`drive_run`) to a terminal outcome — printing one line per
/// transition (`Running` at launch, then `Claimed`/`RunFailed` at the
/// end) and exiting 0 (Claimed) or 5 (Failed, local or wirkd-side).
fn run_deterministic_command(args: &[String]) -> ExitCode {
    let Some(estate) = flag_value(args, "--estate") else {
        return run_deterministic_usage();
    };
    let Some(work_id_str) = flag_value(args, "--work") else {
        return run_deterministic_usage();
    };
    let Some(executor_kind) = flag_value(args, "--executor") else {
        return run_deterministic_usage();
    };
    if executor_kind != "child" && executor_kind != "docker" {
        return run_deterministic_usage();
    }

    let estate_root = PathBuf::from(&estate);
    let work_id = WorkId(work_id_str);

    let status = match wirkd_status(&estate, &work_id) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("wirk run-deterministic: {err}");
            return ExitCode::from(2);
        }
    };

    let (run, world) = match reserved_deterministic(&status) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("wirk run-deterministic: {msg}");
            return ExitCode::from(2);
        }
    };

    println!("Running {}", run.id.0);

    let outcome = if executor_kind == "child" {
        let executor = executors::child::ChildExecutor::new(estate_root, work_id.clone());
        drive_run_child(&executor, &run, &world, &estate, &work_id)
    } else {
        let executor = executors::docker::DockerExecutor::new(estate_root, work_id.clone());
        drive_run_docker(&executor, &run, &world, &estate, &work_id)
    };

    match outcome {
        Ok(()) => {
            println!("Claimed {}", run.id.0);
            ExitCode::SUCCESS
        }
        Err(cause) => {
            // Only this process's own `drive_run` ever sees a local
            // executor failure directly; a wirkd-side one (the Claim
            // itself refused) is already journaled by wirkd's own
            // `claim` handler, so re-filing it here would double-record
            // — `wirkd_fail`'s own `TripleMismatch`-shaped refusal for
            // an already-Failed Run is the harmless outcome of that
            // case, discarded (`let _ =`).
            let cause = ensure_detail(cause);
            let _ = wirkd_fail(&estate, &work_id, &run.id, &cause);
            println!(
                "RunFailed {} status={} detail={}",
                run.id.0,
                cause.status.as_deref().unwrap_or(""),
                cause.detail.as_deref().unwrap_or(""),
            );
            ExitCode::from(5)
        }
    }
}

fn run_deterministic_usage() -> ExitCode {
    eprintln!("usage: wirk run-deterministic --estate <root> --work <id> --executor child|docker");
    ExitCode::from(1)
}

/// Parses wirkd `status`'s reply (`handle_status`'s own additions) into
/// the `Run`/`World` pair `launch` needs. Refuses anything but a
/// `World::Deterministic` — this command drives only the deterministic
/// executors.
fn reserved_deterministic(status: &serde_json::Value) -> Result<(Run, World), String> {
    let Some(run_id) = status["run_id"].as_str() else {
        return Err("no open Run reserved for this Work".to_string());
    };
    let Some(waypoint) = status["current_waypoint"].as_str() else {
        return Err("wirkd status carries no current_waypoint".to_string());
    };
    let attempt = status["attempt"].as_u64().unwrap_or(1) as u32;
    let world_hash = status["world_hash"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let world_value = status
        .get("world")
        .filter(|value| !value.is_null())
        .ok_or_else(|| "wirkd status carries no World for this Work".to_string())?;
    let world: World = serde_json::from_value(world_value.clone())
        .map_err(|err| format!("malformed World from wirkd status: {err}"))?;
    if !matches!(world, World::Deterministic(_)) {
        return Err("the reserved World is not Deterministic".to_string());
    }
    let run = Run {
        id: RunId(run_id.to_string()),
        waypoint: WaypointId(waypoint.to_string()),
        attempt,
        world_hash: WorldHash(world_hash),
        state: wirk_core::RunState::Open,
        // Deterministic runs carry no actor kind (0041 D129 is
        // actor-only); default is inert here.
        kind: wirk_core::ActorKind::default(),
    };
    Ok((run, world))
}

/// Launches `world` through `executor`, then blocks once on the
/// child's own exit (`ChildExecutor::wait`/`DockerExecutor::wait`,
/// ruling 0044: no poll loop, no timeout — `std::process::Child::wait`
/// or, for docker, the supervisor thread's own `docker start -a` join,
/// both already blocking calls) and reads wirkd's `status` exactly
/// once afterward, since only wirkd's own journal knows whether a
/// filed Claim was Validated or Refused (a `MissingArtifact` refusal,
/// for instance, surfaces to the executor's `wait` as
/// `Err(ClaimFiling)`, handled the same way below as any other local
/// failure — no double-journaling: wirkd already recorded that
/// refusal itself). `Ok(())` once `status` reports the Run `claimed`;
/// `Err(FailureCause)` on any other terminal outcome, local
/// (`launch`/`wait` themselves) or wirkd-reported, for the caller to
/// journal via the `fail` verb when it was local — a wirkd-reported
/// failure is already journaled and this function's own `Err` for it
/// carries the same cause only so the caller can print it.
fn drive_run_child(
    executor: &executors::child::ChildExecutor,
    run: &Run,
    world: &World,
    estate: &str,
    work_id: &WorkId,
) -> Result<(), FailureCause> {
    if let Err(err) = executor.launch(run, world) {
        return Err(local_cause(&err));
    }
    match executor.wait(run) {
        Ok(RunObservation::Failed(cause)) => return Err(cause),
        Ok(RunObservation::Vanished) => {
            return Err(FailureCause {
                status: Some("vanished".to_string()),
                request_id: None,
                at: wirk_core_timestamp_now(),
                detail: None,
            });
        }
        Ok(RunObservation::Running) => {}
        Err(err) => return Err(local_cause(&err)),
    }
    read_terminal_status(estate, work_id)
}

/// As `drive_run_child`, against `DockerExecutor`.
fn drive_run_docker(
    executor: &executors::docker::DockerExecutor,
    run: &Run,
    world: &World,
    estate: &str,
    work_id: &WorkId,
) -> Result<(), FailureCause> {
    if let Err(err) = executor.launch(run, world) {
        return Err(local_cause(&err));
    }
    match executor.wait(run) {
        Ok(RunObservation::Failed(cause)) => return Err(cause),
        Ok(RunObservation::Vanished) => {
            return Err(FailureCause {
                status: Some("vanished".to_string()),
                request_id: None,
                at: wirk_core_timestamp_now(),
                detail: None,
            });
        }
        Ok(RunObservation::Running) => {}
        Err(err) => return Err(local_cause(&err)),
    }
    read_terminal_status(estate, work_id)
}

/// The one wirkd `status` read after the executor's blocking `wait`
/// returns (module doc): a clean exit only means the Claim was filed,
/// not that wirkd accepted it — that verdict lives in wirkd's own
/// journal alone.
fn read_terminal_status(estate: &str, work_id: &WorkId) -> Result<(), FailureCause> {
    match wirkd_status(estate, work_id) {
        Ok(status) => match status["run_state"].as_str() {
            Some("claimed") => Ok(()),
            Some("failed") => Err(FailureCause {
                status: status["failure_status"].as_str().map(str::to_string),
                request_id: None,
                at: wirk_core_timestamp_now(),
                detail: status["failure_detail"].as_str().map(str::to_string),
            }),
            other => Err(FailureCause {
                status: Some("unexpected_run_state".to_string()),
                request_id: None,
                at: wirk_core_timestamp_now(),
                detail: Some(format!(
                    "executor wait returned Running but wirkd status was {other:?}"
                )),
            }),
        },
        Err(err) => Err(FailureCause {
            status: Some("wirkd_status_failed".to_string()),
            request_id: None,
            at: wirk_core_timestamp_now(),
            detail: Some(err),
        }),
    }
}

/// Guarantees `cause.detail` is never empty by the time this command
/// prints or journals it (issue 279's own guarantee, extended: a
/// journaled failure must always carry *some* diagnostic, even when the
/// failing command captured none itself — a bare `sh -c false` writes
/// nothing to stderr, so `ChildExecutor::poll`'s own tail is
/// legitimately empty; that emptiness must not silently become an
/// empty `detail` here, one more layer up).
fn ensure_detail(mut cause: FailureCause) -> FailureCause {
    if cause.detail.as_deref().unwrap_or("").is_empty() {
        cause.detail = Some(format!(
            "run-deterministic: no diagnostic output captured (status {})",
            cause.status.as_deref().unwrap_or("unknown")
        ));
    }
    cause
}

fn local_cause<E: std::error::Error>(err: &E) -> FailureCause {
    FailureCause {
        status: None,
        request_id: None,
        at: wirk_core_timestamp_now(),
        detail: Some(err.to_string()),
    }
}

/// Calls wirkd's `status` verb for `work_id`, returning the parsed
/// `result` object on an `ok` reply.
fn wirkd_status(estate: &str, work_id: &WorkId) -> Result<serde_json::Value, String> {
    let pointer = wirkd::client::locate(Path::new(estate)).map_err(|err| err.to_string())?;
    match wirkd::client::call(
        &pointer.socket,
        &Request::status(wirkd::StatusPayload {
            work_id: work_id.clone(),
        }),
    ) {
        Ok(Reply::Ok { result, .. }) => Ok(result),
        Ok(Reply::Err { error, .. }) => Err(format!("{}: {}", error.code, error.message)),
        Err(err) => Err(err.to_string()),
    }
}

/// Files `cause` as a journaled `RunFailed` for `run_id` via wirkd's
/// `fail` verb — the only way this process, a separate `wirk`
/// invocation from the wirkd it talks to, can record a local executor
/// failure: the Journal itself lives behind wirkd's socket, not a
/// handle this process holds (`orient/child.md` §7 item 2).
fn wirkd_fail(
    estate: &str,
    work_id: &WorkId,
    run_id: &RunId,
    cause: &FailureCause,
) -> Result<(), String> {
    let pointer = wirkd::client::locate(Path::new(estate)).map_err(|err| err.to_string())?;
    let payload = FailPayload {
        triple: ExecutionTriple {
            estate_root: estate.to_string(),
            work_id: work_id.clone(),
            run_id: run_id.clone(),
        },
        status: cause.status.clone(),
        detail: cause.detail.clone(),
    };
    match wirkd::client::call(&pointer.socket, &Request::fail(payload)) {
        Ok(Reply::Ok { .. }) => Ok(()),
        Ok(Reply::Err { error, .. }) => Err(format!("{}: {}", error.code, error.message)),
        Err(err) => Err(err.to_string()),
    }
}

// ---- journal demo (item 2, ruling 0028 D93) --------------------------

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

/// Blocks on the verifier's continue signal (ruling 0044: no poll, no
/// timeout) via a named pipe: `<dir>/continue` is created as a FIFO
/// (`mkfifo`, R4 — no libc dependency earned for one call, `std::
/// process::Command` shells out to the same coreutils binary a
/// deterministic Waypoint's own command would) if it does not already
/// exist, then opened for reading, which itself blocks until some
/// other process opens the same path for writing (POSIX FIFO open
/// semantics — a reader's `open` blocks until a writer is present) —
/// the writer is the verifier's `SIGKILL`-timing probe, tried and
/// proven live in `w3/fix2/BUILD.md`'s tried step, not simulated here.
/// A single byte read (or EOF) is the signal; its content is never
/// interpreted.
fn wait_for_continue(dir: &str) {
    let signal = Path::new(dir).join("continue");
    if !signal.exists() {
        let _ = std::process::Command::new("mkfifo").arg(&signal).status();
    }
    if let Ok(mut fifo) = std::fs::File::open(&signal) {
        let mut buf = [0u8; 1];
        let _ = std::io::Read::read(&mut fifo, &mut buf);
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
        // Ripple-only value from item 5's additive `base_sha` field
        // (issue 285; wirk-core/src/lib.rs): the demo has no real repo
        // to pin, so it names itself rather than leaving the field
        // empty (a `ChildExecutor` refuses an empty `base_sha`).
        base_sha: "journal-demo".to_string(),
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
                wp2_command: None,
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
            EventKind::RunLaunched {
                run: run.clone(),
                actor_kind: wirk_core::ActorKind::default(),
            },
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
