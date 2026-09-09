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

// `wirk atlas ...` (P3 W3, source-orient/BUILD-BRIEF.md "Public
// surface"): thin JSON-capable clients over wirkd's own seven Atlas
// verbs. This crate holds no Atlas domain logic of its own.
mod atlas;

// `wirk finding ...` (W-B, p3-world-loop/W-B-BUILD.md, corrected by
// loop-b-prepare-correct/HANDOFF.md and W-B-CONSTRUCTION-REVIEW.md):
// thin JSON-capable clients over wirkd's own Finding verbs.
mod finding;

use wirkd::{
    ClaimPayload, FailPayload, Reply, Request, RetryPayload, StatusPayload, SubmitPayload,
    WorkFailPayload,
};

use wirk_core::{
    Access, ClaimId, ClaimKind, ClaimVerdict, DeterministicWorld, Event, EventId, EventKind,
    ExecutionTriple, Executor, FailureCause, Journal, JournalError, OutputContract,
    RepositoryBinding, RouteId, Run, RunId, RunObservation, SourceBasis, WaypointId, WorkId,
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
        Some("claim") => claim(&args[2..]),
        Some("journal") => journal_command(&args[2..]),
        Some("wirkd") => wirkd_command(&args[2..]),
        Some("work") => work_command(&args[2..]),
        Some("run-deterministic") => run_deterministic_command(&args[2..]),
        Some("run") => executor::run_command(&args[2..]),
        Some("plugin") => plugin_command(&args[2..]),
        Some("atlas") => atlas::atlas_command(&args[2..]),
        Some("finding") => finding::finding_command(&args[2..]),
        Some("world") => world_command(&args[2..]),
        _ => {
            eprintln!(
                "usage: wirk claim | wirk journal demo <dir> | wirk wirkd start|stop|ping|status|watch --estate <root> [--work <id>] [--requesting-work <id>] [--admin] [--json] | wirk work submit --estate <root> --repo <name>:<read|write> --base <ref> (--route <name> [--kind actor --repo-path <path>] | --kind deterministic --command <argv...>) | wirk work status --estate <root> --work <id> [--requesting-work <id>] [--admin] [--json] | wirk run --estate <root> --work <id> --session <name> [--herdr-socket <path>] [--actor-kind <kind>] [--actor-model <model>] [--actor-effort <level>] | wirk run-deterministic --estate <root> --work <id> --executor child|docker | wirk plugin init --estate <root> | wirk atlas acquire|refresh|publish|status|search|resolve|relate|semantic build|semantic select|findings --estate <root> ... | wirk finding raise|assert|settle|applied|list ... | wirk world show [--revision N] [--json] | wirk world expand (--question TEXT | --reference HANDLE) [--reason TEXT] [--json]"
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
    let work_id = WorkId(triple["WIRK_WORK_ID"].clone());

    let pointer = match wirkd::client::locate(Path::new(&estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk claim: {err}");
            return ExitCode::from(2);
        }
    };

    // P2.7 W1 (`orient/reorient.md` §D, R2 over R7): no explicit
    // `--artifact` flags and no `--question` means the actor never
    // named its outputs by hand — ask wirkd for the current Waypoint's
    // declared output contract (the `status` verb it already returns,
    // `handle_status`'s `result["world"]`, unchanged wire method) and
    // claim each declared output at its own name as the worktree-
    // relative path; wirkd's own validator still refuses whatever is
    // actually missing. An explicit `--artifact` flag keeps its
    // meaning exactly — this only fires when the caller supplied none.
    if artifacts.is_empty() && question.is_none() {
        match fetch_output_contract_names(&pointer.socket, &work_id) {
            Ok(names) => {
                for name in names {
                    artifacts.insert(name.clone(), name);
                }
            }
            Err(err) => {
                eprintln!("wirk claim: {err}");
                return ExitCode::from(2);
            }
        }
    }

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

/// Asks wirkd's existing `status` verb for the current Waypoint's
/// reserved World (`handle_status`'s `result["world"]`, the same field
/// `wirk run-deterministic`'s `reserved_deterministic` and
/// `boundary_claim.rs`'s own `reserved_world` test helper already
/// read) and returns its declared output names, in the order the Route
/// authored them — `ActorWorld.output_contract` for an actor Waypoint,
/// `DeterministicWorld.expected_artifacts` for a deterministic one, R2
/// over adding a new wire method (`orient/reorient.md` §D).
fn fetch_output_contract_names(socket: &Path, work_id: &WorkId) -> Result<Vec<String>, String> {
    let reply = wirkd::client::status(
        socket,
        // The claiming Work reading its own output contract: scoped to
        // itself, never the administrative surface (F-C) — through the
        // typed door, so a daemon that never applied that scope is
        // refused rather than read (V-5).
        StatusPayload::scoped(work_id.clone(), work_id.clone()),
    )
    .map_err(|err| err.to_string())?;
    let result = match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => {
            return Err(format!("status refused: {} {}", error.code, error.message));
        }
    };
    let world_value = result
        .get("world")
        .filter(|value| !value.is_null())
        .ok_or_else(|| "wirkd status carries no World for this Work".to_string())?;
    let world: World = serde_json::from_value(world_value.clone())
        .map_err(|err| format!("malformed World from wirkd status: {err}"))?;
    let contract: OutputContract = match world {
        World::Actor(actor) => actor.output_contract,
        World::Deterministic(det) => det.expected_artifacts,
    };
    Ok(contract.0.into_iter().map(|spec| spec.name).collect())
}

// ---- wirk world (P3 W-C1) ------------------------------------------

/// `wirk world show [--json]`: the delivered stage projection for this
/// Run, read from inside the pane.
///
/// Reads the injected triple exactly as `wirk claim` does (0001 D3, D5)
/// and takes no `--work`, `--run` or `--estate` argument: an actor
/// inspects the context *it* was delivered, and there is no surface here
/// on which one Work asks for another's. A fresh actor with no
/// transcript can therefore recover what its stage was actually given,
/// with every coordinate resolvable through `wirk atlas resolve`.
fn world_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("show") => world_show_command(&rest[1..]),
        Some("expand") => world_expand_command(&rest[1..]),
        _ => world_usage(),
    }
}

/// The injected triple, or the exact names that are missing. The same
/// read `wirk claim` does (0001 D3, D5).
fn world_triple() -> Result<BTreeMap<&'static str, String>, Vec<&'static str>> {
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
    if missing.is_empty() {
        Ok(triple)
    } else {
        Err(missing)
    }
}

fn world_show_command(rest: &[String]) -> ExitCode {
    let mut json_out = false;
    let mut revision: Option<u64> = None;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_out = true,
            "--revision" => match args.next().map(|value| value.parse::<u64>()) {
                Some(Ok(value)) => revision = Some(value),
                _ => {
                    eprintln!("wirk world show: --revision takes a revision number");
                    return ExitCode::from(1);
                }
            },
            _ => return world_usage(),
        }
    }

    let triple = match world_triple() {
        Ok(triple) => triple,
        Err(missing) => {
            for name in &missing {
                eprintln!("wirk world show: missing {name}");
            }
            return ExitCode::from(1);
        }
    };
    let estate_root = triple["WIRK_ESTATE_ROOT"].clone();
    let pointer = match wirkd::client::locate(Path::new(&estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk world show: {err}");
            return ExitCode::from(2);
        }
    };
    let payload = wirkd::WorldShowPayload {
        triple: ExecutionTriple {
            estate_root,
            work_id: WorkId(triple["WIRK_WORK_ID"].clone()),
            run_id: RunId(triple["WIRK_RUN_ID"].clone()),
        },
        revision,
    };
    let result = match wirkd::client::call(&pointer.socket, &Request::world_show(payload)) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            eprintln!("wirk world show: {} {}", error.code, error.message);
            return ExitCode::from(3);
        }
        Err(err) => {
            eprintln!("wirk world show: {err}");
            return ExitCode::from(2);
        }
    };
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
        );
        return ExitCode::SUCCESS;
    }
    print_world_show(&result);
    ExitCode::SUCCESS
}

/// `wirk world expand --question TEXT | --reference HANDLE [--reason
/// TEXT] [--json]`: the actor of this Run adds a revision to the context
/// it was delivered.
///
/// The same triple-only door `world show` uses, for the same reason: an
/// actor expands the context *it* was given, and there is no argument
/// here that names a Work, a Run or a revision. `--reference` takes a
/// handle this Run's own context printed under `reachable`; a handle
/// from anywhere else addresses nothing.
fn world_expand_command(rest: &[String]) -> ExitCode {
    let mut json_out = false;
    let mut question: Option<String> = None;
    let mut reference: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        let mut take = |slot: &mut Option<String>, name: &str| -> bool {
            match args.next() {
                Some(value) => {
                    *slot = Some(value.clone());
                    true
                }
                None => {
                    eprintln!("wirk world expand: {name} takes a value");
                    false
                }
            }
        };
        match arg.as_str() {
            "--json" => json_out = true,
            "--question" => {
                if !take(&mut question, "--question") {
                    return ExitCode::from(1);
                }
            }
            "--reference" => {
                if !take(&mut reference, "--reference") {
                    return ExitCode::from(1);
                }
            }
            "--reason" => {
                if !take(&mut reason, "--reason") {
                    return ExitCode::from(1);
                }
            }
            _ => return world_usage(),
        }
    }
    if question.is_none() && reference.is_none() {
        eprintln!(
            "wirk world expand: an expansion asks for something: give --question, --reference, \
             or both"
        );
        return ExitCode::from(1);
    }

    let triple = match world_triple() {
        Ok(triple) => triple,
        Err(missing) => {
            for name in &missing {
                eprintln!("wirk world expand: missing {name}");
            }
            return ExitCode::from(1);
        }
    };
    let estate_root = triple["WIRK_ESTATE_ROOT"].clone();
    let pointer = match wirkd::client::locate(Path::new(&estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk world expand: {err}");
            return ExitCode::from(2);
        }
    };
    let payload = wirkd::WorldExpandPayload {
        triple: ExecutionTriple {
            estate_root,
            work_id: WorkId(triple["WIRK_WORK_ID"].clone()),
            run_id: RunId(triple["WIRK_RUN_ID"].clone()),
        },
        question,
        reference,
        reason,
    };
    let result = match wirkd::client::call(&pointer.socket, &Request::world_expand(payload)) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            eprintln!("wirk world expand: {} {}", error.code, error.message);
            return ExitCode::from(3);
        }
        Err(err) => {
            eprintln!("wirk world expand: {err}");
            return ExitCode::from(2);
        }
    };
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "expanded to revision {} from observation {}",
        result
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        result
            .get("parent")
            .and_then(|value| value.as_str())
            .unwrap_or("-")
    );
    print_world_show(&result);
    ExitCode::SUCCESS
}

/// The plain-text rendering. Every line is a fact the JSON also carries;
/// nothing is summarized away, and an unavailable or unoriented stage
/// says so in a sentence rather than by printing nothing.
fn print_world_show(result: &serde_json::Value) {
    let text = |key: &str| -> String {
        result
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string()
    };
    println!(
        "waypoint {} run {} current {}",
        text("waypoint"),
        text("run"),
        result
            .get("current")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    );
    // The chain, before anything about one revision of it. A fresh actor
    // that has never seen this Run's transcript learns from this line
    // that its context has a history, how long it is, and which revision
    // the document below is — and, when there is more than one, the
    // exact command that reads any earlier one.
    if let Some(revisions) = result.get("revisions").and_then(|v| v.as_array()) {
        let latest = result
            .get("latest_revision")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        println!(
            "context revisions {} (initial 0 .. latest {})",
            revisions.len(),
            latest
        );
        if revisions.len() > 1 {
            println!("      read an earlier one with: wirk world show --revision <n>");
        }
        // Advertised only where the verb would actually run: `world
        // expand` refuses a Run that is not the current Run of its own
        // Waypoint, so printing this line for a superseded Run would be
        // a command that cannot work. An honest absence, not a menu.
        if result
            .get("current")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            && result.get("projection").is_some()
        {
            println!("      add to it with: wirk world expand --question <text>");
        }
    }
    match text("orientation").as_str() {
        "none" | "unavailable" => {
            println!("orientation {}: {}", text("orientation"), text("detail"));
            if !text("reason").is_empty() {
                println!("reason {}", text("reason"));
            }
            return;
        }
        _ => {}
    }
    let Some(projection) = result.get("projection") else {
        return;
    };
    let string = |value: &serde_json::Value, key: &str| -> String {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    println!("question {}", string(projection, "question"));
    println!(
        "policy {} route_edition {} revision {}",
        string(projection, "compilation_policy"),
        string(projection, "route_edition"),
        projection
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    );
    if let Some(expansion) = projection.get("expansion") {
        println!(
            "expands revision's observation {} (basis {})",
            string(expansion, "parent_observation"),
            string(expansion, "basis")
        );
        if let Some(request) = expansion.get("request") {
            println!(
                "      asked [{}] {}",
                if request
                    .get("authored_question")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    "authored question"
                } else {
                    "this stage's own question, carried over"
                },
                string(request, "question")
            );
            if let Some(handle) = request.get("reference").and_then(|v| v.as_str()) {
                println!("      inside delivered handle {handle}");
            }
            if let Some(reason) = request.get("reason").and_then(|v| v.as_str()) {
                println!("      because {reason}");
            }
        }
        // Said in a line, not left to be reconstructed from two `bound`
        // lists: a revision that added nothing is a real and useful
        // answer, and a reader must not have to infer it from the
        // revision number going up.
        let count = |key: &str| -> u64 {
            expansion
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        println!(
            "      added {} item(s); {} candidate(s) were already bound here at the same \
             coordinate",
            count("delivered"),
            count("already_bound")
        );
    }
    if let Some(coverage) = projection.get("coverage") {
        println!("coverage {coverage}");
    }
    if let Some(generations) = projection.get("generations").and_then(|v| v.as_array()) {
        println!(
            "generations {} at publication revision {}",
            generations.len(),
            projection
                .get("publication_revision")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        );
    }
    // The reason first, the coordinate second. The coordinate is an
    // opaque encoded `ExactCoordinate` — the thing an actor pastes into
    // `wirk atlas resolve` — and leading with it buries the one line
    // that says what was bound and why.
    for (label, key) in [("bound", "bound"), ("referenced", "referenced")] {
        let Some(items) = projection.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            println!(
                "{label} [{}] {}",
                item.get("lifetime")
                    .and_then(|v| v.as_str())
                    .unwrap_or("working"),
                string(item, "reason")
            );
            // A prior-stage artifact is addressed by this Work's own
            // Claim and the declared output name, not by an Atlas
            // coordinate, so the line that would resolve it is the
            // digest it was verified against — printing an `atlas
            // resolve` for it would print a command that cannot run.
            match item.get("identity").and_then(|value| value.get("kind")) {
                Some(kind) if kind == "artifact_digest" => println!(
                    "      claim {} digest {}",
                    string(&item["identity"], "claim"),
                    string(&item["identity"], "digest")
                ),
                _ => println!(
                    "      resolve with: wirk atlas resolve --coordinate {}",
                    string(item, "coordinate")
                ),
            }
            // Ruling 0142: the summary above is a bounded presentation
            // string. When it was taken from the place a match located
            // rather than from the head of the resource, the reader is
            // told which lines those are and that they have a coordinate
            // of their own — otherwise the coordinate printed above (the
            // whole resource, or the whole ranked unit) reads as the
            // span that was shown. Absent when nothing located anything.
            if let Some(terms) = item["shown"]["matched_terms"].as_array() {
                let named = terms
                    .iter()
                    .filter_map(|term| term.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "      shown: lines {}-{} of that resource, around {}{}",
                    item["shown"]["line_start"].as_u64().unwrap_or(0),
                    item["shown"]["line_end"].as_u64().unwrap_or(0),
                    if named.is_empty() {
                        "the match".to_string()
                    } else {
                        named
                    },
                    if item["shown"]["whole_match_shown"].as_bool().unwrap_or(true) {
                        ""
                    } else {
                        "; the match itself is wider than the summary budget and is cut"
                    }
                );
                if let Some(coordinate) = item["shown"]["coordinate"].as_str() {
                    println!("      shown coordinate {coordinate}");
                }
            }
        }
    }
    // A discovery handle is only useful if the line under it runs. The
    // `fetch` string is the projection's own; `wirk atlas search` takes
    // its estate and Work from the injected triple, so this is the whole
    // command an actor types.
    if let Some(items) = projection.get("reachable").and_then(|v| v.as_array()) {
        for item in items {
            println!(
                "reachable {} — {} indexed {} resource(s) in source {}",
                string(item, "handle"),
                item.get("resources")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                string(item, "family"),
                string(item, "source")
            );
            println!("      discover with: {}", string(item, "fetch"));
            // A handle is usable because *this* context delivered it, so
            // the line that binds it is printed exactly where the
            // evidence for it is.
            if result
                .get("current")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                println!(
                    "      bind it into this context with: wirk world expand --reference {}",
                    string(item, "handle")
                );
            }
        }
    }
    if let Some(retrieval) = projection.get("retrieval") {
        println!(
            "retrieval mode {} semantic {} candidates {} shown {}",
            string(retrieval, "mode"),
            string(retrieval, "semantic"),
            retrieval
                .get("total_candidates")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            retrieval
                .get("returned")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        );
        if let Some(reason) = retrieval.get("semantic_reason").and_then(|v| v.as_str()) {
            println!("      semantic reason {reason}");
        }
        if let Some(degraded) = retrieval.get("degraded").and_then(|v| v.as_array())
            && !degraded.is_empty()
        {
            println!(
                "      degraded {}",
                serde_json::Value::Array(degraded.clone())
            );
        }
    }
    // The recorded learning this stage was handed, and the state of the
    // index the published half was read from — printed together and in
    // that order, because "nothing was consulted" means one thing beside
    // a synchronized index and a completely different one beside an
    // unreadable index, and a reader must not have to hold the second
    // fact in their head to interpret the first.
    if let Some(note) = projection.get("findings_index") {
        println!(
            "findings index {} (complete {})",
            string(note, "state"),
            note.get("complete")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        );
    }
    if let Some(items) = projection.get("consulted").and_then(|v| v.as_array()) {
        for item in items {
            println!(
                "consulted [{}] {} — {}",
                string(item, "origin"),
                string(item, "id"),
                string(item, "claim")
            );
            println!(
                "      status {} generations {} — {}",
                item.get("status")
                    .map(|status| string(status, "state"))
                    .unwrap_or_default(),
                string(item, "generation_relation"),
                string(item, "reason")
            );
            // Every recorded evidence entry this stage was not given, as
            // the two counts they are: one refused by this requester's
            // own scope, one outside what this assembly captured. No
            // alias, path, coordinate or generation travels in either.
            let count = |key: &str| -> u64 {
                item.get(key)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            };
            if count("evidence_withheld") > 0 || count("evidence_not_delivered") > 0 {
                println!(
                    "      {} recorded evidence entr(ies) withheld from this requester, {} not \
                     delivered here",
                    count("evidence_withheld"),
                    count("evidence_not_delivered")
                );
            }
            for evidence in item
                .get("evidence")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                println!(
                    "      resolve its evidence with: wirk atlas resolve --coordinate {}",
                    string(evidence, "coordinate")
                );
            }
            for contradiction in item
                .get("contradictions")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                println!("      contradicts {}", string(contradiction, "text"));
            }
        }
    }
    for (label, key) in [("assumption", "assumptions"), ("unknown", "unknowns")] {
        let Some(items) = projection.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            println!(
                "{label} [{}] {}",
                item.get("attributed_to")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                string(item, "text")
            );
        }
    }
    if let Some(items) = projection.get("omitted").and_then(|v| v.as_array()) {
        for item in items {
            println!("omitted {item}");
        }
    }
    // Presentation and fact, said separately: `truncated` is about what
    // was rendered, `coverage` above is about what was found, and
    // `next_action` reads only the second.
    println!(
        "truncated {}",
        projection
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    );
    if !string(projection, "next_action").is_empty() {
        println!("next {}", string(projection, "next_action"));
    }
    if let Some(receipt) = result.get("receipt") {
        println!(
            "observed {} over {}ms in {} lap(s), observation {}",
            receipt
                .get("observed_at")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            receipt
                .get("observation_window_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            receipt
                .get("laps")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            string(receipt, "observation")
        );
    }
}

fn world_usage() -> ExitCode {
    eprintln!(
        "usage: wirk world show [--revision N] [--json] | wirk world expand (--question TEXT | \
         --reference HANDLE) [--reason TEXT] [--json]"
    );
    ExitCode::from(1)
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
        "status" => wirkd_status_command(
            &estate,
            flag_value(&rest[1..], "--work"),
            flag_value(&rest[1..], "--requesting-work"),
            rest[1..].iter().any(|arg| arg == "--admin"),
            rest[1..].iter().any(|arg| arg == "--json"),
        ),
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
        "watch" => wirkd_watch_command(
            &estate,
            flag_value(&rest[1..], "--work"),
            flag_value(&rest[1..], "--requesting-work"),
            rest[1..].iter().any(|arg| arg == "--admin"),
        ),
        _ => wirkd_usage(),
    }
}

// ---- who is asking: the injected actor context (ruling 0117) --------
//
// `status` and `watch` are read verbs with two named surfaces: the
// administrative read of any Work, and the scoped read a Work makes as
// itself. Until this correction the CLI chose between them by the
// *absence* of a flag: no `--requesting-work` meant administrative, and
// no `--work` meant "every Work under the estate". That default is
// right for the operator standing at the estate root and wrong for the
// only other caller this binary has — an actor running inside a Run,
// with the triple injected into its environment (0001 D5). Such an
// actor typing the obvious `wirk wirkd status --estate "$WIRK_ESTATE_ROOT"`
// received the administrative enumeration of every Work in the estate:
// prior Work ids, report names, artifact digests. It never named
// `--admin` and was never told it had used it (ruling 0117, from the
// executed trace in `loop-b-scoped-native-consumer-opus`).
//
// So the default now follows the context the process is actually in.
// Inside an actor context the omitted scope resolves to that actor's
// own Work — as the requester, and (absent `--work`) as the target, so
// nothing unrelated is enumerated before any admission is asked for.
// Outside one, nothing changes: the operator's defaults are exactly
// what they were. An incomplete or mismatched context resolves to
// neither: it is refused, because the one thing it must never do is
// widen to the administrative answer.
//
// What this is not: it is not authentication. The same uid runs the
// actor and the operator, the triple is a transport hint and not
// trusted lineage (`TRIPLE_VARS`), and `--admin` remains available to
// anyone who types it. What it buys is that the administrative surface
// is never reached by omission.

/// What the injected triple (`TRIPLE_VARS`) says about the process
/// running this command.
pub(crate) enum ActorContext {
    /// No part of the triple is set: the operator's own shell.
    Absent,
    /// Part of it is set and part is not. This names no valid identity,
    /// and it is not "no context" either — that reading is the wider
    /// one, which is exactly what a half-injected environment must not
    /// silently buy.
    Partial { missing: Vec<&'static str> },
    /// The whole triple, non-blank.
    Present {
        estate_root: String,
        work_id: String,
    },
}

/// Reads `TRIPLE_VARS` from the environment. A variable set to blank or
/// whitespace counts as unset (an exported-but-empty var is the common
/// shape of a half-inherited environment, not an identity).
pub(crate) fn actor_context() -> ActorContext {
    let values: Vec<Option<String>> = TRIPLE_VARS
        .iter()
        .map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
        .collect();
    if values.iter().all(Option::is_none) {
        return ActorContext::Absent;
    }
    let missing: Vec<&'static str> = TRIPLE_VARS
        .iter()
        .zip(values.iter())
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| *name)
        .collect();
    if !missing.is_empty() {
        return ActorContext::Partial { missing };
    }
    ActorContext::Present {
        estate_root: values[0].clone().unwrap_or_default(),
        work_id: values[1].clone().unwrap_or_default(),
    }
}

/// The scope `status`/`watch` will actually ask for, and the Work it
/// will ask about when the caller named none.
struct ResolvedScope {
    /// `Some(requester)` is the scoped read as that Work; `None` is the
    /// administrative read — reached only when it was named, or when
    /// there is no actor context at all (the operator's own default).
    requesting: Option<WorkId>,
    /// The target when `--work` is absent: the actor's own Work inside
    /// an actor context, and `None` — every Work under the estate, the
    /// operator's listing — outside one.
    default_target: Option<String>,
    /// Printed once on stderr before anything is fetched, when the
    /// resolution is worth saying out loud: which scope answered and
    /// why. Silent for the plain operator, whose behavior is unchanged.
    note: Option<String>,
}

/// Resolves the scope for `status`/`watch` **before** the daemon is
/// located or a single Work is read, so a refusal here costs no
/// content. `Err` is the refusal text; the caller prints it and exits 1
/// (usage), the same exit a malformed command line already takes.
fn resolve_scope(
    verb: &str,
    estate: &str,
    requesting: Option<String>,
    admin: bool,
) -> Result<ResolvedScope, String> {
    if admin && requesting.is_some() {
        return Err(format!(
            "name at most one of --requesting-work <id> (scoped) or --admin (administrative); \
             {verb} refuses both together rather than choosing one for you"
        ));
    }
    let context = actor_context();
    if admin {
        // Named explicitly, so it is what the caller asked for — here
        // and inside an actor context alike (ruling 0117: a same-uid
        // deliberate administrative override is not something this
        // correction claims to prevent). What changes is that it is
        // said out loud when the caller had an identity of its own.
        let note = match &context {
            ActorContext::Present { work_id, .. } => Some(format!(
                "--admin named: reading administratively, not as this actor's own Work {work_id}"
            )),
            _ => None,
        };
        return Ok(ResolvedScope {
            requesting: None,
            default_target: None,
            note,
        });
    }
    match context {
        // Unchanged: the operator at the estate root, whose omitted
        // scope has always meant the administrative read of every Work
        // and still does (ruling 0117 preserves it).
        ActorContext::Absent => Ok(ResolvedScope {
            requesting: requesting.map(WorkId),
            default_target: None,
            note: None,
        }),
        ActorContext::Partial { missing } => {
            // An explicitly named scope is the caller's own decision and
            // needs no context to stand on.
            if let Some(requester) = requesting {
                return Ok(ResolvedScope {
                    requesting: Some(WorkId(requester)),
                    default_target: None,
                    note: None,
                });
            }
            Err(format!(
                "the injected actor context is incomplete ({} unset) and names no Work to ask as; \
                 name --requesting-work <id> for a scoped read or --admin for the administrative one. \
                 {verb} will not read an incomplete context as an operator shell",
                missing.join(", ")
            ))
        }
        ActorContext::Present {
            estate_root,
            work_id,
        } => {
            let same_estate = same_estate(&estate_root, estate);
            match requesting {
                Some(requester) if requester != work_id => Err(format!(
                    "--requesting-work {requester} names a Work other than this actor's own \
                     {work_id} (WIRK_WORK_ID); a scoped read here is asked as {work_id}, and \
                     --admin is the administrative read"
                )),
                // Naming your own Work explicitly is the same request
                // the default now makes; it stays legal and explicit.
                Some(requester) => Ok(ResolvedScope {
                    requesting: Some(WorkId(requester)),
                    default_target: Some(work_id),
                    note: None,
                }),
                None if !same_estate => Err(format!(
                    "--estate {estate} is not this actor's estate {estate_root} \
                     (WIRK_ESTATE_ROOT), so this actor's own Work is not the scope for it; \
                     name --requesting-work <id> or --admin to read another estate deliberately"
                )),
                None => Ok(ResolvedScope {
                    requesting: Some(WorkId(work_id.clone())),
                    default_target: Some(work_id.clone()),
                    note: Some(format!(
                        "actor context {work_id}: asking as that Work (--admin for the \
                         administrative read of the estate)"
                    )),
                }),
            }
        }
    }
}

/// Whether two estate roots name the same directory. Canonicalized when
/// both exist (a symlinked or trailing-slash spelling of the injected
/// root is the same estate); compared as written otherwise, which is
/// the conservative answer — an unresolvable path is not silently the
/// same estate as anything.
fn same_estate(a: &str, b: &str) -> bool {
    let canon = |path: &str| std::fs::canonicalize(path).ok();
    match (canon(a), canon(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

fn wirkd_usage() -> ExitCode {
    // The usage line is where the two scopes are explained, because it
    // is what a caller who guessed wrong sees (ruling 0117: "explain
    // the modes in help/output"). `ping` is named for what it is — a
    // daemon health check — because it was read as "the status of the
    // estate" and answered with nothing of the sort. The scope flag is
    // named exactly once here: the integration review's V-2.
    eprintln!(
        "usage: wirk wirkd start|stop|ping|status|watch --estate <root> [--work <id>] [--requesting-work <id>] [--admin] [--json]

  ping    daemon health only: the protocol version and pid of the running
          wirkd. It reports nothing about any Work.
  status  a Work's state, waypoint, needs_input and evidence. --json
          renders the daemon's own reply instead of the human lines: one
          object for a single Work, an array of {{work_id, status}} for
          the administrative estate walk. Refusals stay on stderr. A
          refused or failed single read prints no JSON; the
          administrative estate walk prints the rows that answered and
          reports the rest on stderr and in the exit code.
  watch   that Work's journal appends, streamed as they land.

scope of status and watch:
  Inside an actor context (WIRK_ESTATE_ROOT, WIRK_WORK_ID and WIRK_RUN_ID
  all injected) both verbs answer as that Work, about that Work, unless
  --work names another target for it to ask about — lineage decides
  whether the daemon admits that. Outside an actor context both answer
  administratively about every Work under the estate, unchanged.
  Name a scope explicitly to override the default: the scope flag above
  reads as one Work (inside an actor context it must be that actor's
  own), and --admin is the administrative read. Naming both is refused,
  as is an incomplete or foreign actor context, before anything is read.
  --admin proves nothing about who is asking: the same user runs both."
    );
    ExitCode::from(1)
}

/// `wirk wirkd watch --estate <root> [--work <id>]
/// [--requesting-work <id>]`: opens one `watch` connection per named
/// (or discovered) Work id and prints one line per
/// `Event` it carries — `work_id kind {...event json...}` — as they
/// arrive, blocking between lines (no poll, no timeout, ruling 0044).
/// Never returns on its own: it ends only when every watched
/// connection's iterator ends (wirkd stopped, or every named Work's
/// connection was refused up front) or the process is killed, matching
/// the plugin pane's own "the pane program ends; that is the state"
/// contract (item G). `--estate` naming a wirkd that is not running
/// prints why and exits 2 immediately, rather than blocking on a
/// connection that will never come. Once every watched connection has
/// ended, the exit is 0 unless at least one Work's own watch was
/// explicitly refused by wirkd (0069 correction) — an unrefused
/// stream's own valid events and clean EOF are printed exactly as
/// before either way, and that refusal never cuts a sibling Work's
/// still-live stream short.
///
/// With no scope flag and no actor context this is the operator's
/// stream, the same named administrative surface `wirk wirkd status`
/// is. Inside an actor context (ruling 0117) the omitted scope is that
/// actor's own Work instead, and the omitted `--work` its own Work as
/// well, so the discovery walk above never enumerates the estate for a
/// caller who never asked to administer it. `--requesting-work <id>`
/// asks as that Work explicitly: the daemon admits the stream whole
/// or refuses it whole (`handle_watch_connection` — a partially
/// redacted `Event` is not an `Event`), and acknowledges the applied
/// scope before its first event line, so a daemon that would have
/// answered the narrow request with the raw journal is refused here
/// rather than read. A Work watching itself is trivially admitted.
fn wirkd_watch_command(
    estate: &str,
    work_filter: Option<String>,
    requesting: Option<String>,
    admin: bool,
) -> ExitCode {
    // Ruling 0117: the scope is settled before the daemon is located,
    // so a refused one costs no stream and no Work listing.
    let scope = match resolve_scope("wirk wirkd watch", estate, requesting, admin) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk wirkd watch: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk wirkd watch: {note}");
    }
    let pointer = match wirkd::client::locate(Path::new(estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk wirkd watch: {err}");
            return ExitCode::from(2);
        }
    };
    let work_ids: Vec<String> = match (work_filter, &scope.default_target) {
        (Some(id), _) => vec![id],
        // An actor asking with no target watches its own Work. The
        // estate walk below is the operator's listing, and reaching it
        // from inside a scoped call would enumerate every Work id in
        // the estate before a single admission was asked for.
        (None, Some(own)) => vec![own.clone()],
        (None, None) => match list_work_ids(Path::new(estate)) {
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
    // streams. `any_refused` is the one piece of state a thread reports
    // back besides its printed lines: whether *its* Work was refused by
    // wirkd (0069 correction) — checked only after every stream has
    // ended on its own, never used to cut a still-live stream short.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let any_refused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut handles = Vec::new();
    let requesting = scope.requesting;
    for work_id in work_ids {
        let socket = pointer.socket.clone();
        let requesting = requesting.clone();
        let tx = tx.clone();
        let any_refused = std::sync::Arc::clone(&any_refused);
        handles.push(std::thread::spawn(move || {
            let payload = match &requesting {
                // The integration review's V-1: `--requesting-work` was
                // advertised on this verb's own usage line and never
                // read, so a caller naming a narrow scope silently got
                // the administrative stream — the exact failure the
                // scope gate exists to remove, left standing on the
                // human half of the surface the wire half already
                // guards. Named here, it is the scope the daemon is
                // asked for and must acknowledge before a single event
                // line is consumed.
                Some(requester) => {
                    wirkd::WatchPayload::scoped(WorkId(work_id.clone()), requester.clone())
                }
                // The operator's own stream verb, the same named
                // administrative surface `wirk wirkd status` is (F-C).
                None => wirkd::WatchPayload::admin(WorkId(work_id.clone())),
            };
            let events = match wirkd::client::watch(&socket, payload) {
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
                    // A well-formed daemon refusal (`NotFound` for an
                    // unknown or path-like id, most commonly): labeled
                    // `refused`, not folded into `watch_error`'s
                    // generic-failure line, and recorded so the whole
                    // command's exit reflects it — while every other
                    // thread here keeps streaming its own Work
                    // untouched.
                    Err(wirkd::client::ClientError::Refused(detail)) => {
                        any_refused.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = tx.send(format!(
                            "{work_id} refused {}: {}",
                            detail.code, detail.message
                        ));
                        return;
                    }
                    // A named scope this daemon never established
                    // (V-5). Folded with the explicit refusals rather
                    // than with the generic transport failures: a
                    // scripted consumer asked for a narrow stream and
                    // did not get one, and a zero exit would say it
                    // did — the same silence this correction removes.
                    Err(err @ wirkd::client::ClientError::ScopeNotApplied(_)) => {
                        any_refused.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = tx.send(format!("{work_id} refused scope: {err}"));
                        return;
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
    if any_refused.load(std::sync::atomic::Ordering::Relaxed) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `wirk wirkd status --estate <root> [--work <id>]
/// [--requesting-work <id>]` and its `wirk work status` alias: prints
/// wirkd's `status` verb reply for `work_id`
/// alone, or (no `--work`) for every Work directory under
/// `<estate>/works/` (`server.rs`'s own `journal_for` layout, 0033
/// D101), one line each, oldest-directory-name-order first (`sort`, R6
/// — no journal timestamp read needed for a listing). A locate/
/// transport failure is exit 2, same as `wirkd_client_call`; a `status`
/// refusal for one Work id (`NotFound`, a fabricated id passed via
/// `--work`) is printed on stderr and folded into the same exit 2
/// rather than aborting the rest of the listing.
///
/// W-B launch disclosure integration (the launch review's F-C). The
/// `status` wire verb has no unscoped default: it answers a named
/// `admin` read or a named `requester`-scoped one. Which of the two
/// this verb asks for is `resolve_scope`'s answer (ruling 0117), and it
/// is printed on every line rather than left for the reader to assume.
/// For the human at the estate root and the herdr-plugin
/// `wirkd-status` action — no actor context — that is still the
/// administrative read, exactly as `wirk finding list --admin` is, and
/// `--admin` names it explicitly. For an actor running inside a Run it
/// is that actor's own Work, which is also the target when `--work` is
/// absent. `--requesting-work <id>` asks the same verb as that Work
/// instead:
/// the estate walk narrows to that Work's own lineage, and a Work whose
/// bindings do not cover the reporting Work's gets journal identities
/// with the checkout-derived halves marked withheld. Naming `admin`
/// proves nothing about who is asking (the same uid runs both), and no
/// approval is added here; what it buys is that a scoped consultation
/// can never silently fall through to the unscoped answer.
///
/// `--json` (ruling 0135's observed defect: "work status ignores
/// --json") renders the daemon's own `status` reply instead of the
/// human lines, on both named entry points — `wirk wirkd status` and
/// its `wirk work status` alias reach this one function, so the flag
/// cannot honour one surface and ignore the other. It is a *rendering*
/// flag and nothing else (R2, the shape `wirk work obligations --json`
/// already established): the same request is made, the same scope is
/// resolved first, the same refusals happen, the same exit codes come
/// back, and no field of the daemon's reply is added, renamed or
/// recased on the way out.
///
/// Two shapes, chosen by the *request* and never by how many Works
/// happen to exist:
///
/// - one Work asked about — `--work <id>`, or an actor's own Work
///   resolved from its injected context — prints that Work's reply
///   `result` verbatim, a single JSON object;
/// - the operator's estate walk (administrative, no `--work`) prints a
///   JSON array of `{"work_id": <the id asked about>, "status":
///   <result verbatim>}`. The daemon's administrative reply carries no
///   `work_id` of its own (only the scoped reply does, and it stays
///   exactly as sent), so the enumeration wraps each answer beside the
///   id this command asked for — the same id the human line prints
///   from the same loop variable — rather than editing a reply body to
///   make the array addressable.
///
/// Everything that is not the answer stays on stderr in `--json` mode:
/// the resolved-scope note, the behind-projection sentence, and a
/// refusal's `code`/`message`. A refused or failed single read
/// therefore prints *no* JSON at all and exits non-zero. The
/// administrative estate walk is different: it prints the array of
/// rows that answered and reports the rest on stderr and in the exit
/// code, so a non-zero exit there can still carry a partial array on
/// stdout — a script reads the exit status first and parses stdout
/// only on success, and never has to tell an answer apart from an
/// apology.
fn wirkd_status_command(
    estate: &str,
    work_filter: Option<String>,
    requesting: Option<String>,
    admin: bool,
    json: bool,
) -> ExitCode {
    // Ruling 0117: settle the scope first. A refusal here happens
    // before wirkd is located, before any Work directory is listed and
    // before a single status is fetched — a refused scoped query is
    // never quietly answered by the administrative surface instead.
    let scope = match resolve_scope("wirk wirkd status", estate, requesting, admin) {
        Ok(scope) => scope,
        Err(refusal) => {
            eprintln!("wirk wirkd status: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &scope.note {
        eprintln!("wirk wirkd status: {note}");
    }
    let pointer = match wirkd::client::locate(Path::new(estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk wirkd status: {err}");
            return ExitCode::from(2);
        }
    };
    let requesting = scope.requesting;
    // Which of the two `--json` shapes this request asks for, settled
    // here with the request itself: the estate walk is a listing and
    // renders an array, a named or inherited single target renders that
    // Work's own object. An estate that happens to hold exactly one
    // Work does not change the shape of its listing.
    let mut enumerated = false;
    let work_ids: Vec<String> = match (work_filter, &scope.default_target) {
        (Some(id), _) => vec![id],
        // The actor's own Work is the target it did not have to name.
        // The estate walk is the operator's listing and stays there:
        // enumerating every Work id is itself the disclosure ruling
        // 0117 names, and it used to happen before any admission.
        (None, Some(own)) => vec![own.clone()],
        (None, None) => match list_work_ids(Path::new(estate)) {
            Ok(ids) => {
                enumerated = true;
                ids
            }
            Err(err) => {
                eprintln!("wirk wirkd status: {err}");
                return ExitCode::from(2);
            }
        },
    };

    let mut exit = ExitCode::SUCCESS;
    // The estate walk's rows, held until every Work has answered: one
    // JSON document is printed, not a line per Work that a `json.load`
    // would choke on.
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for work_id in work_ids {
        let payload = match &requesting {
            Some(requester) => StatusPayload::scoped(WorkId(work_id.clone()), requester.clone()),
            None => StatusPayload::admin(WorkId(work_id.clone())),
        };
        let reply = wirkd::client::status(&pointer.socket, payload);
        match reply {
            Ok(Reply::Ok { result, .. }) => {
                if json {
                    // The daemon's own answer, unedited. The listing
                    // wraps it beside the id it was asked about; the
                    // single read is the object itself, which is what a
                    // caller that named one Work asked for.
                    if enumerated {
                        rows.push(serde_json::json!({
                            "work_id": work_id,
                            "status": result,
                        }));
                    } else {
                        println!("{result}");
                    }
                    continue;
                }
                // P2.3 W1 (states.md §2): `needs_input` is absent from
                // the reply when the Work never has been NeedsInput
                // (`handle_status`'s own additive field) — printed as
                // `-` then, same convention as `current_waypoint`.
                let needs_input = match result.get("needs_input") {
                    Some(cause) => format!(
                        "{}: {}",
                        cause["reason"].as_str().unwrap_or("?"),
                        // The integration review's V-4. Three different
                        // facts used to print as the same empty string
                        // after the colon: the detail was recorded and
                        // says this, it was never recorded at all, or
                        // this reader is not admitted to it. That is
                        // the absent/unrecorded/withheld conflation
                        // F-D corrects, and the scoped human surface
                        // was the one place it survived. `withheld`
                        // says a part was removed for this requester
                        // and nothing about what it held — the same
                        // count-not-content discipline the `scope`
                        // suffix already follows.
                        match &cause["detail"] {
                            detail if detail["withheld"] == true => "withheld",
                            detail => detail.as_str().unwrap_or("unrecorded"),
                        }
                    ),
                    None => "-".to_string(),
                };
                // Which of the two named surfaces answered, printed
                // rather than assumed (F-C), with the withheld count
                // the scoped answer carries — honest and bounded: how
                // many parts, never which.
                let scope = match result["disclosure"]["withheld"].as_u64() {
                    Some(withheld) => format!(
                        "{} withheld {}",
                        result["scope"].as_str().unwrap_or("requester"),
                        withheld
                    ),
                    None => result["scope"].as_str().unwrap_or("?").to_string(),
                };
                println!(
                    "work_id {} state {} current_waypoint {} needs_input {} scope {}",
                    work_id,
                    result["state"].as_str().unwrap_or("?"),
                    result["current_waypoint"].as_str().unwrap_or("-"),
                    needs_input,
                    scope
                );
                // W-A: a held container's own reason, the container
                // activations in force, and (W-A correction, F3) the
                // artifact evidence each validated Claim rests on —
                // answered against the content identity recorded at
                // validation, so a rewritten or removed artifact reads
                // `unavailable (changed)` / `unavailable (absent)`
                // here rather than as a path that silently still
                // "counts". Printed on the human verb, not only on the
                // JSON wire result: an operator deciding whether a
                // closure's evidence still holds is exactly who needs
                // this, and the pre-correction printer showed neither.
                if let Some(held) = result.get("held") {
                    println!(
                        "  held {} attempt {} missing {}",
                        held["waypoint"].as_str().unwrap_or("?"),
                        held["attempt"].as_u64().unwrap_or(1),
                        held["missing"]
                            .as_array()
                            .map(|names| names
                                .iter()
                                .filter_map(|name| name.as_str())
                                .collect::<Vec<_>>()
                                .join(", "))
                            .unwrap_or_default()
                    );
                }
                if let Some(parent) = result.get("parent") {
                    println!(
                        "  parent {} waypoint {} attempt {} run {} role {}",
                        parent["work"].as_str().unwrap_or("?"),
                        parent["waypoint"].as_str().unwrap_or("?"),
                        parent["attempt"].as_u64().unwrap_or(1),
                        parent["run"].as_str().unwrap_or("?"),
                        parent["role"].as_str().unwrap_or("?"),
                    );
                }
                for activation in result["activations"].as_array().unwrap_or(&Vec::new()) {
                    println!(
                        "  container {} activation {}",
                        activation["waypoint"].as_str().unwrap_or("?"),
                        activation["attempt"].as_u64().unwrap_or(1),
                    );
                }
                for entry in result["evidence"].as_array().unwrap_or(&Vec::new()) {
                    for artifact in entry["artifacts"].as_array().unwrap_or(&Vec::new()) {
                        let availability = if artifact["available"].as_bool().unwrap_or(false) {
                            "available".to_string()
                        } else {
                            format!(
                                "unavailable ({})",
                                artifact["reason"].as_str().unwrap_or("unknown")
                            )
                        };
                        println!(
                            "  evidence {} {} sha256:{} {} claim {} run {}",
                            entry["waypoint"].as_str().unwrap_or("?"),
                            artifact["name"].as_str().unwrap_or("?"),
                            artifact["digest"].as_str().unwrap_or("?"),
                            availability,
                            entry["claim"].as_str().unwrap_or("?"),
                            entry["run"].as_str().unwrap_or("-"),
                        );
                    }
                }
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
    // A listing that refused or failed on some Work has already said so
    // on stderr and set the exit code; the array it prints holds the
    // Works that did answer, and the caller reads the status before the
    // stdout — the same contract the single read keeps.
    if json && enumerated {
        println!("{}", serde_json::Value::Array(rows));
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

/// Says, on stderr, that a reply's rows are a subset of what the
/// estate's journals hold.
///
/// stderr on purpose, so it is seen in `--json` mode too — a script
/// reading stdout gets the machine-readable `index` block, a human
/// watching the terminal gets the sentence, and neither has to know
/// about the other. The exit code is deliberately unchanged: the
/// journal is the record, the mutation that produced this reply really
/// did happen, and a derived projection that is behind is a degraded
/// answer rather than a failed call. What is not acceptable — and what
/// this closes — is the answer looking identical either way.
pub(crate) fn warn_if_index_incomplete(what: &str, result: &serde_json::Value) {
    let index = &result["index"];
    if index["complete"] != serde_json::Value::Bool(false) {
        return;
    }
    let projection = index["projection"].as_str().unwrap_or("incomplete");
    eprintln!(
        "wirk {what}: the estate findings index is {projection} — this answer is a subset of what the estate's journals hold, not a complete one"
    );
    if let Some(pending) = index["pending_rows"].as_u64() {
        eprintln!("  {pending} journaled row(s) are not in the index");
    }
    if let Some(detail) = index["detail"].as_str() {
        eprintln!("  {detail}");
    }
    if let Some(recovery) = index["recovery"].as_str() {
        eprintln!("  {recovery}");
    }
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
    wirkd_typed_call(estate, |socket| wirkd::client::call(socket, request), on_ok)
}

/// `wirkd_client_call`'s locate-and-render half, with the call itself
/// left to the caller: a verb with a **typed door** in `wirkd::client`
/// (the door that checks the reply against the request before anything
/// is rendered) reaches the same locate, the same diagnostics and the
/// same exit codes through this, instead of bypassing its own contract
/// by going through the generic `client::call` (the basis review's F1).
fn wirkd_typed_call(
    estate: &str,
    call: impl FnOnce(&Path) -> Result<Reply, wirkd::client::ClientError>,
    on_ok: impl FnOnce(&serde_json::Value),
) -> ExitCode {
    let pointer = match wirkd::client::locate(Path::new(estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk wirkd: {err}");
            return ExitCode::from(2);
        }
    };
    match call(&pointer.socket) {
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

/// Dispatches `wirk work <rest>`: `submit --estate <root> --repo
/// <name>:<read|write> (repeatable) --base <ref> --route <name>
/// [--kind actor --repo-path <path>]`, or the Route-less ad hoc
/// `--kind deterministic --command <argv...>` (build-brief.md §7.3).
/// `--intent` is removed (p2-route-files W2, J1): an Actor Waypoint's
/// intent is authored per-Waypoint in the Route file now, not passed
/// on the submit line.
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
            wirkd_status_command(
                &estate,
                Some(work_id),
                flag_value(&rest[1..], "--requesting-work"),
                rest[1..].iter().any(|arg| arg == "--admin"),
                rest[1..].iter().any(|arg| arg == "--json"),
            )
        }
        // P2.3 W2 (decide.md §1): `wirk work retry --estate <root>
        // --work <id>` opens a fresh Run on the failed Waypoint's
        // reserved World; refused when the Work is not `NeedsInput`.
        Some("retry") => work_retry_command(&rest[1..]),
        // P2.3 W2 (decide.md §1): `wirk work fail --estate <root>
        // --work <id> --reason <text>` appends `WorkFailed` with the
        // reason; refused when the Work is not `NeedsInput`.
        Some("fail") => work_fail_command(&rest[1..]),
        // W-A (§3.4): `wirk work cancel --estate <root> --work <id>
        // [--cascade] [--reason <text>]`.
        Some("cancel") => work_cancel_command(&rest[1..]),
        // W-B basis access (`loop-b-basis-access`): the read-only
        // inspection an operator needs before writing
        // `policy/settlement.json` by hand — the obligations this
        // Work's own Route declares, the canonical basis its reserved
        // World produces, and what this estate already admits. It
        // reports; it never admits.
        Some("obligations") => work_obligations_command(&rest[1..]),
        _ => work_usage(),
    }
}

/// `wirk work cancel --estate <root> --work <id> [--cascade] [--reason
/// <text>]` (W-A, §3.4): refuses `OpenChild` naming the first open child
/// found when a spawned child is still non-terminal and `--cascade` was
/// not given; with `--cascade`, cancels every open descendant first,
/// attributed to this Work, then this Work itself.
fn work_cancel_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return work_usage();
    };
    let Some(work_id) = flag_value(rest, "--work") else {
        return work_usage();
    };
    let cascade = rest.iter().any(|arg| arg == "--cascade");
    let reason = flag_value(rest, "--reason");

    wirkd_client_call(
        &estate,
        &Request::cancel(wirkd::CancelPayload {
            work_id: WorkId(work_id.clone()),
            cascade,
            reason,
        }),
        |_result| {
            println!("Canceled {work_id}");
        },
    )
}

/// `wirk work retry --estate <root> --work <id> [--run <run-id>]`:
/// resolves the failed `run_id` itself from `wirk work status`'s own
/// `run_id` field (one extra round trip) rather than asking the human
/// to copy an id (decide.md §1's own CLI design), then calls wirkd's
/// `retry` verb. Prints `"Retried <old_run_id> -> <new_run_id>"` on
/// success, or wirkd's refusal text (`NotNeedsInput` when the Work
/// isn't `NeedsInput` or `Waiting`).
///
/// W-A correction (F1/F2): `--run` names one exact leaf Run instead.
/// That is what reopening an already-closed nested stage needs — the
/// stage to correct is not the Work's own current one (a held container
/// resolves to its most recent descendant Run), it is a leaf inside a
/// container that already closed. Reusing `retry` this way is the
/// minimal usable public reopen operation: it already mints the fresh
/// Run/World a correction needs, and wirkd's own `handle_retry` is
/// where the ancestor invalidation belongs.
fn work_retry_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return work_usage();
    };
    let Some(work_id) = flag_value(rest, "--work") else {
        return work_usage();
    };
    let named_run = flag_value(rest, "--run");

    let pointer = match wirkd::client::locate(Path::new(&estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk work retry: {err}");
            return ExitCode::from(2);
        }
    };

    if let Some(run_id) = named_run {
        return wirkd_client_call(
            &estate,
            &Request::retry(RetryPayload {
                triple: ExecutionTriple {
                    estate_root: estate.clone(),
                    work_id: WorkId(work_id.clone()),
                    run_id: RunId(run_id.clone()),
                },
            }),
            |result| {
                println!(
                    "Retried {} -> {}",
                    result["old_run_id"].as_str().unwrap_or(&run_id),
                    result["new_run_id"].as_str().unwrap_or_default()
                );
            },
        );
    }

    let status_reply = wirkd::client::status(
        &pointer.socket,
        // `wirk work retry` is an operator verb reached from the estate
        // root, the same named administrative surface `wirk work status`
        // is (F-C): it resolves the current Run id and nothing else. An
        // administrative read asks for the whole reply, so the scope
        // contract check does not apply to it (`client::status`).
        StatusPayload::admin(WorkId(work_id.clone())),
    );
    let run_id = match status_reply {
        Ok(Reply::Ok { result, .. }) => match result["run_id"].as_str() {
            Some(run_id) => run_id.to_string(),
            None => {
                eprintln!("wirk work retry: {work_id} has no run_id on its current Waypoint");
                return ExitCode::from(2);
            }
        },
        Ok(Reply::Err { error, .. }) => {
            eprintln!(
                "wirk work retry: {work_id} {} {}",
                error.code, error.message
            );
            return ExitCode::from(2);
        }
        Err(err) => {
            eprintln!("wirk work retry: {work_id} {err}");
            return ExitCode::from(2);
        }
    };

    wirkd_client_call(
        &estate,
        &Request::retry(RetryPayload {
            triple: ExecutionTriple {
                estate_root: estate.clone(),
                work_id: WorkId(work_id.clone()),
                run_id: RunId(run_id.clone()),
            },
        }),
        |result| {
            println!(
                "Retried {} -> {}",
                result["old_run_id"].as_str().unwrap_or(&run_id),
                result["new_run_id"].as_str().unwrap_or_default()
            );
        },
    )
}

/// `wirk work fail --estate <root> --work <id> --reason <text>`: calls
/// wirkd's `workfail` verb, printing `"WorkFailed <work_id>: <reason>"`
/// on success, or wirkd's refusal text (`NotNeedsInput`).
fn work_fail_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return work_usage();
    };
    let Some(work_id) = flag_value(rest, "--work") else {
        return work_usage();
    };
    let Some(reason) = flag_value(rest, "--reason") else {
        return work_usage();
    };

    wirkd_client_call(
        &estate,
        &Request::workfail(WorkFailPayload {
            work_id: WorkId(work_id.clone()),
            reason: reason.clone(),
        }),
        |_result| {
            println!("WorkFailed {work_id}: {reason}");
        },
    )
}

fn work_submit_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
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
            // p2-route-files W2 (build-brief.md §7.3, J1): `--intent`
            // is removed — an Actor Waypoint's intent is authored in
            // the Route file now, per-Waypoint. A submit that still
            // passes it is a usage exit, not a silently-ignored flag,
            // so an old caller notices the removal rather than
            // submitting with no intent text anywhere.
            "--intent" => {
                return work_usage();
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
                        "--repo",
                        "--base",
                        "--route",
                        "--kind",
                        "--repo-path",
                        "--source-basis",
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
    // P3 W3 (ruling 0090): required only when more than one `--repo`
    // binding is declared; wirkd itself refuses an ambiguous submission
    // rather than guessing the first one (`work_usage` below still
    // accepts a submit line that omits it, same as always, when there
    // is at most one binding to be ambiguous about).
    let execution_repo = flag_value(rest, "--execution-repo");
    // W-A (§3.3): a child submission names its requesting parent
    // Work/container/Run and the role it claims — all four or none;
    // wirkd itself is the one that checks the binding is real
    // (`ChildParentMismatch`/`ChildExceedsParentBinding`), this is only
    // parsing.
    let parent_work = flag_value(rest, "--parent-work");
    let parent_waypoint = flag_value(rest, "--parent-waypoint");
    let parent_run = flag_value(rest, "--parent-run");
    let role = flag_value(rest, "--role");
    // W-A correction (F4): `--parent-attempt <n>` names the container
    // *activation* the child serves. Optional: omitted, wirkd binds the
    // container's current generation and records it on both journals;
    // given, it must be that same current generation, so a request
    // built against a superseded activation is refused rather than
    // admitted and later found stale.
    let parent_attempt = match flag_value(rest, "--parent-attempt") {
        None => None,
        Some(text) => match text.parse::<u32>() {
            Ok(attempt) => Some(attempt),
            Err(_) => return work_usage(),
        },
    };
    let parent = match (&parent_work, &parent_waypoint, &parent_run, &role) {
        (None, None, None, None) => None,
        (Some(work), Some(waypoint), Some(run), Some(role)) => Some(wirk_core::ParentBinding {
            work: WorkId(work.clone()),
            waypoint: WaypointId(waypoint.clone()),
            attempt: parent_attempt,
            run: RunId(run.clone()),
            role: role.clone(),
        }),
        _ => return work_usage(),
    };
    let source_basis = match flag_value(rest, "--source-basis").as_deref() {
        Some("git") => Some(SourceBasis::Git {
            base: base_ref.clone(),
        }),
        Some("output-only") => Some(SourceBasis::OutputOnly {
            reference: base_ref.clone(),
        }),
        Some(_) => return work_usage(),
        None => None,
    };

    // p2-route-files W2 (build-brief.md §7.3): `--route` is required
    // for every submit except the ad hoc `--kind deterministic
    // --command` single-Waypoint Work, which carries no Route at all.
    let ad_hoc_deterministic = kind.as_deref() == Some("deterministic") && command.is_some();
    if route.is_none() && !ad_hoc_deterministic {
        return work_usage();
    }

    let payload = SubmitPayload {
        intent: String::new(),
        repositories,
        base_ref,
        source_basis,
        kind,
        command,
        repo_path,
        route,
        parent,
        execution_repo,
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

/// `wirk work obligations --estate <root> --work <id>
/// (--requesting-work <id> | --admin) [--waypoint <id>] [--json]`
/// (`loop-b-basis-access`).
///
/// **Read-only.** A thin client over `wirkd`'s own
/// `handle_work_obligations`, which owns every decision this prints:
/// this module parses argv and renders. It appends no event, writes no
/// index and never touches `policy/settlement.json` — admitting an
/// obligation stays an explicit operator edit to that file, and this
/// verb exists only so the operator has the exact value to put in it.
///
/// The scope pair is the same exclusive, always-named one `work
/// status`, `finding list` and `finding settle` carry: `--admin` for
/// the unscoped operator read, `--requesting-work <id>` for a Work
/// consulting within its own lineage.
fn work_obligations_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return work_usage();
    };
    let Some(work_id) = flag_value(rest, "--work") else {
        return work_usage();
    };
    let json = rest.iter().any(|arg| arg == "--json");
    let requester = flag_value(rest, "--requesting-work").map(WorkId);
    let admin = rest.iter().any(|arg| arg == "--admin");
    if admin == requester.is_some() {
        eprintln!(
            "wirk work obligations: name exactly one of --requesting-work <id> (scoped) or --admin (unscoped)"
        );
        return ExitCode::from(2);
    }
    let selected = flag_value(rest, "--waypoint");
    let payload = wirkd::WorkObligationsPayload {
        work_id: WorkId(work_id),
        waypoint: selected.clone(),
        requester,
        admin,
    };
    // The typed door, not the generic `client::call` (the basis review's
    // F1): this is a scoped consultation that discloses a settlement
    // basis and an admission state, and `client::work_obligations`
    // refuses a reply that names no applied scope, or names a different
    // Work than the one asked about, before any of it is rendered here —
    // in `--json` mode as much as in the human one.
    wirkd_typed_call(
        &estate,
        |socket| wirkd::client::work_obligations(socket, payload),
        |result| {
            if json {
                println!("{result}");
                return;
            }
            // F3: a narrowed read says so on its own header line, so
            // nothing below it can be read as a statement about the
            // whole Route.
            match &selected {
                Some(waypoint) => println!(
                    "scope {} policy {} | narrowed to waypoint {}",
                    result["scope"].as_str().unwrap_or("?"),
                    result["policy"]["state"].as_str().unwrap_or("?"),
                    waypoint,
                ),
                None => println!(
                    "scope {} policy {}",
                    result["scope"].as_str().unwrap_or("?"),
                    result["policy"]["state"].as_str().unwrap_or("?"),
                ),
            }
            let waypoints = result["route"]["waypoints"].as_u64().unwrap_or(0);
            let declaring = result["route"]["declaring_obligation"]
                .as_u64()
                .unwrap_or(0);
            let empty = Vec::new();
            let entries = result["obligations"].as_array().unwrap_or(&empty);
            if entries.is_empty() {
                // F3: an empty answer to a *narrowed* question says only
                // that the selected Waypoint declares nothing. Printing
                // the whole-Route sentence here was false whenever
                // another Waypoint did declare one — the same trap the
                // `NotFound` refusal for a mistyped `--waypoint` closes,
                // walked into by the other door.
                match &selected {
                    Some(waypoint) => println!(
                        "  waypoint {waypoint} declares no verification obligation ({declaring} of {waypoints} waypoint(s) on this route declare one)"
                    ),
                    None => println!(
                        "  no waypoint on this work's route declares a verification obligation ({waypoints} waypoint(s) read)"
                    ),
                }
            }
            for entry in entries {
                println!(
                    "  waypoint {} ({}) obligation {}@{}",
                    entry["waypoint"].as_str().unwrap_or("?"),
                    entry["waypoint_kind"].as_str().unwrap_or("?"),
                    entry["obligation"]["id"].as_str().unwrap_or("?"),
                    entry["obligation"]["edition"].as_str().unwrap_or("?"),
                );
                match entry["basis"]["basis"].as_str() {
                    // The one value this verb exists to disclose. It is
                    // an identity to admit, never a proof of anything:
                    // the line below says what admitting it still needs.
                    Some(basis) => println!("    basis {basis}"),
                    None => println!(
                        "    basis unavailable ({}): {}",
                        entry["basis"]["state"].as_str().unwrap_or("?"),
                        entry["basis"]["reason"].as_str().unwrap_or("-"),
                    ),
                }
                println!(
                    "    reserved world {} | admission {}",
                    entry["reservation"]["world_hash"].as_str().unwrap_or("-"),
                    entry["admission"]["state"].as_str().unwrap_or("?"),
                );
                // F2: the mechanism, on the human surface. An obligation
                // whose mechanism is absent obliges nothing however
                // admittable its basis line looks, and that was visible
                // only under `--json`; and the one class that needs a
                // second policy field — a container, whose entry must
                // also list the child obligation's own basis under
                // `mechanisms` — carried that instruction in a JSON-only
                // `note`, on the very surface whose purpose is telling
                // an operator what to write.
                let mechanism = &entry["mechanism"];
                let kind = mechanism["kind"].as_str().unwrap_or("?");
                if mechanism["present"].as_bool() == Some(false) {
                    println!(
                        "    mechanism {kind} absent: {}",
                        mechanism["reason"].as_str().unwrap_or("-"),
                    );
                } else {
                    match mechanism["requires"].as_object() {
                        Some(requires) => println!(
                            "    mechanism {kind} requires {}@{}",
                            requires["id"].as_str().unwrap_or("?"),
                            requires["edition"].as_str().unwrap_or("?"),
                        ),
                        None => println!("    mechanism {kind}"),
                    }
                    if let Some(note) = mechanism["note"].as_str() {
                        println!("      note: {note}");
                    }
                }
                let none = Vec::new();
                for finding in entry["findings"].as_array().unwrap_or(&none) {
                    println!(
                        "    finding {} ready {} settled {}",
                        finding["finding"].as_str().unwrap_or("?"),
                        finding["ready"]["state"].as_str().unwrap_or("?"),
                        finding["settled"]["state"].as_str().unwrap_or("?"),
                    );
                }
            }
            if result["scope"].as_str() == Some("requester") {
                println!(
                    "  withheld {}",
                    result["disclosure"]["withheld"].as_u64().unwrap_or(0)
                );
            }
            println!(
                "  (read-only: admitting an obligation is your own edit to <estate>/policy/settlement.json; a basis is an identity to admit, not a proof that any check holds.)"
            );
        },
    )
}

fn work_usage() -> ExitCode {
    eprintln!(
        "usage: wirk work submit --estate <root> --repo <name>:<read|write> [--repo <name>:<read|write> ...] [--execution-repo <name>] --base <ref> (--route <name> [--kind actor --repo-path <path>] | --kind deterministic [--source-basis git|output-only] [--repo-path <checkout>] --command <argv...>) [--parent-work <id> --parent-waypoint <id> --parent-run <id> --role <role> [--parent-attempt <n>]] | wirk work status --estate <root> --work <id> [--requesting-work <id>] [--admin] [--json] | wirk work retry --estate <root> --work <id> [--run <run-id>] | wirk work fail --estate <root> --work <id> --reason <text> | wirk work cancel --estate <root> --work <id> [--cascade] [--reason <text>] | wirk work obligations --estate <root> --work <id> (--requesting-work <id> | --admin) [--waypoint <id>] [--json]"
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
        // actor-only); default is inert here. Same for P3 native launch
        // selection: actor-only (`selection` is refused at Route load
        // for a non-Actor Waypoint), inert on a Deterministic Run.
        kind: wirk_core::ActorKind::default(),
        selection: wirk_core::ActorSelection::default(),
        launched: false,
        launch_requested: false,
        launch_argv: Vec::new(),
        expansions: Vec::new(),
        // Attempt admission is the Actor launch path's own
        // (`RunLaunchAttempted`); a Deterministic Run never takes one.
        launch_attempt: None,
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
    read_terminal_status(estate, work_id, &run.id)
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
    read_terminal_status(estate, work_id, &run.id)
}

/// The one wirkd `status` read after the executor's blocking `wait`
/// returns (module doc): a clean exit only means the Claim was filed,
/// not that wirkd accepted it — that verdict lives in wirkd's own
/// journal alone.
///
/// p2-route-files W2: reads *this Run's own* entry out of `status`'s
/// `"runs"` array by id, never the Work's own top-level
/// `current_waypoint`/`run_state` fields — those name whatever Run is
/// now current, which a Route file can auto-advance to a *different*
/// freshly-reserved Run in the same journal lock the Claim above just
/// appended under (a Deterministic Waypoint immediately followed by
/// another one, unlike every Route this command's callers used to
/// see). Looking the just-claimed Run up by its own id is what stays
/// correct regardless of what wirkd advanced to next.
fn read_terminal_status(
    estate: &str,
    work_id: &WorkId,
    run_id: &RunId,
) -> Result<(), FailureCause> {
    let status = match wirkd_status(estate, work_id) {
        Ok(status) => status,
        Err(err) => {
            return Err(FailureCause {
                status: Some("wirkd_status_failed".to_string()),
                request_id: None,
                at: wirk_core_timestamp_now(),
                detail: Some(err),
            });
        }
    };
    let run_state = status["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["run"]["id"].as_str() == Some(run_id.0.as_str()))
        .map(|entry| entry["run"]["state"].clone());
    match run_state {
        Some(ref state) if state.get("Claimed").is_some() => Ok(()),
        Some(ref state) if state.get("Failed").is_some() => Err(FailureCause {
            status: state["Failed"]["status"].as_str().map(str::to_string),
            request_id: None,
            at: wirk_core_timestamp_now(),
            detail: state["Failed"]["detail"].as_str().map(str::to_string),
        }),
        other => Err(FailureCause {
            status: Some("unexpected_run_state".to_string()),
            request_id: None,
            at: wirk_core_timestamp_now(),
            detail: Some(format!(
                "run {} not claimed after executor wait returned Running: {other:?}",
                run_id.0
            )),
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
    match wirkd::client::status(
        &pointer.socket,
        // `wirk run-deterministic` drives one Work and reads only that
        // Work's own reserved World: scoped to itself (F-C), through
        // the typed door that refuses an unestablished scope (V-5).
        wirkd::StatusPayload::scoped(work_id.clone(), work_id.clone()),
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
        source_basis: SourceBasis::Unknown,
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
                waypoint_defs: Vec::new(),
                parent: None,
                execution_repo: None,
                execution_identity: None,
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
                selection: wirk_core::ActorSelection::default(),
                launch_argv: Vec::new(),
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
                artifacts: Vec::new(),
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
        EventKind::RunLaunchRequested { .. } => "RunLaunchRequested",
        EventKind::RunLaunchAttempted { .. } => "RunLaunchAttempted",
        EventKind::RunLaunched { .. } => "RunLaunched",
        EventKind::WorkFailed { .. } => "WorkFailed",
        EventKind::WorkCanceled { .. } => "WorkCanceled",
        EventKind::ContainerActivated { .. } => "ContainerActivated",
        EventKind::StageHeld { .. } => "StageHeld",
        EventKind::StageClosed { .. } => "StageClosed",
        EventKind::ChildWorkSpawned { .. } => "ChildWorkSpawned",
        EventKind::FindingRaised { .. } => "FindingRaised",
        EventKind::FindingSettled { .. } => "FindingSettled",
        EventKind::FindingAsserted { .. } => "FindingAsserted",
        EventKind::FindingApplied { .. } => "FindingApplied",
        EventKind::ProjectionExpanded { .. } => "ProjectionExpanded",
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
