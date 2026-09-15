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

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

// wirkd wire protocol (envelope, verb, payload types), the client
// (`locate`, `call`) that reaches a running wirkd, and the server loop
// itself (W2 `orient/transport.md` §2-4; W3 `orient/build-brief.md` §3).
// Lives in `wirk`'s library crate so every integration test binary can
// share this one compiled copy instead of each `#[path]`-including its
// own; `use` here introduces the same `crate::wirkd` name a local `mod
// wirkd;` would.
use wirk::wirkd;

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

/// P4.5 increment A (ruling 0256): `wirk estate storage` and
/// `wirk estate clean` — the operator's view of what this estate owns,
/// and the explicit, guarded way to remove an optional derivation from
/// it. Its own module beside `atlas` and `finding` for the same reason
/// those are: one noun, its own verbs, its own rendering.
mod estate;

/// `wirk browser view|serve`: a browser view of a Work — what it is for,
/// how far it has got, the World it was given and the evidence its
/// Claims rest on — rendered from the same `status`, `world_show` and
/// `work_artifact` replies the CLI already prints, plus one typed return
/// to the Herdr pane running it.
mod browser;

use wirkd::{
    ClaimPayload, FailPayload, Reply, Request, RetryPayload, StatusPayload, SubmitPayload,
    WorkFailPayload,
};

use wirk_core::{
    Access, ClaimId, ClaimKind, ClaimOrigin, ClaimVerdict, DeterministicWorld, Event, EventId,
    EventKind, ExecutionTriple, Executor, FailureCause, Journal, JournalError, OutputContract,
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
        Some("output") => output_command(&args[2..]),
        Some("artifact") => artifact_command(&args[2..]),
        Some("estate") => estate::estate_command(&args[2..]),
        Some("browser") => browser::browser_command(&args[2..]),
        _ => {
            eprintln!(
                "usage: wirk claim | wirk journal demo <dir> | wirk wirkd start|stop|ping|status|watch --estate <root> [--work <id>] [--requesting-work <id>] [--admin] [--json] | wirk work submit --estate <root> --repo <name>:<read|write> --base <ref> (--route <name> [--kind actor --repo-path <path> | --kind actor --source-basis output-only] | --kind deterministic --command <argv...>) | wirk work list --estate <root> [--requesting-work <id>] [--admin] [--json] | wirk work status --estate <root> --work <id> [--requesting-work <id>] [--admin] [--json] | wirk run --estate <root> --work <id> --session <name> [--herdr-socket <path>] [--actor-kind <kind>] [--actor-model <model>] [--actor-effort <level>] | wirk run-deterministic --estate <root> --work <id> --executor child|docker | wirk plugin init [--estate <root>] [--harness <kind>] [--harness-arg <arg>]... [--clear-harness-args] | wirk plugin show | wirk plugin harnesses [--socket <path>] [--json] | wirk atlas acquire|refresh|publish|remove|status|cancel|search|resolve|relate|semantic build|semantic select|findings --estate <root> ... | wirk finding raise|assert|settle|applied|list ... | wirk world show [--revision N] [--json] | wirk world expand (--question TEXT | --reference HANDLE) [--reason TEXT] [--json] | wirk output [dir | list] [--json] | wirk artifact read|export --claim <id> --name <name> [--to <path>] | wirk artifact read --estate <root> --work <id> (--admin | --requesting-work <id>) --claim <id> --name <name> | wirk estate storage|clean --estate <root> ... | wirk browser view --estate <root> [--work <id>] [--requesting-work <id> | --admin] --out <path.html> | wirk browser serve --estate <root> --work <id> [--requesting-work <id> | --admin] [--open] [--idle-timeout <secs>]"
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
    let mut outputs: BTreeSet<String> = BTreeSet::new();
    let mut question: Option<String> = None;
    // Ruling 0257: stated by wirk's own turn-end hook and by nothing
    // else. An actor typing `wirk claim` never passes it, which is the
    // whole distinction — the hook fires because a turn ended, the
    // actor files because it decided it was done.
    let mut automatic = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--automatic" => {
                automatic = true;
            }
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
            // Ruling 0145: a declared output this Work owns, claimed by
            // *name* — deliberately no `=PATH` half, because there is no
            // path for the actor to supply. wirkd derives the address
            // from the bound Work, the bound Run and this name; `wirk
            // output` prints where to write it.
            "--output" => {
                i += 1;
                let Some(name) = args.get(i) else {
                    return claim_usage();
                };
                if name.contains('=') {
                    eprintln!(
                        "wirk claim: --output takes a declared output NAME, not NAME=PATH: a \
                         managed output's location is derived by wirkd, not supplied"
                    );
                    return ExitCode::from(1);
                }
                outputs.insert(name.clone());
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

    // One name cannot be both a checkout artifact and a managed output:
    // they are different files in different places, and silently
    // preferring one would make the receipt disagree with what the actor
    // meant. Refused as usage, before wirkd is contacted.
    if let Some(both) = outputs.iter().find(|name| artifacts.contains_key(*name)) {
        eprintln!(
            "wirk claim: `{both}` is named by both --artifact and --output; a declared output is \
             claimed from one place or the other, never both"
        );
        return ExitCode::from(1);
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
    let run_id = RunId(triple["WIRK_RUN_ID"].clone());

    let pointer = match wirkd::client::locate(Path::new(&estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk claim: {err}");
            return ExitCode::from(2);
        }
    };

    let triple = ExecutionTriple {
        estate_root,
        work_id,
        run_id,
    };

    // P2.7 W1 (`orient/reorient.md` §D, R2 over R7), corrected by ruling
    // 0212, 0213 and 0235: no explicit `--artifact`/`--output` flags and
    // no `--question` means the actor never named its outputs by hand —
    // ask wirkd for *this Run's own bound* Waypoint's declared output
    // contract (`wirk output`'s own scoped `run_outputs` verb,
    // `handle_run_outputs`'s reply, keyed off this Run's triple — not
    // `status`'s current-Waypoint answer, which names whichever Waypoint
    // the Work has since advanced to and, for a Run that is no longer
    // current, is simply the wrong contract; ruling 0235) and claim each
    // *required* declared output at its own name, addressed the same
    // way its own World produces it: an `ActorWorld`'s outputs are
    // written under `wirk output dir` (the delivered worker contract,
    // `wirk-herdr/src/worker-contract.md` "Outputs"; the automatic
    // Claude/OpenCode Stop-hook always files a bare Claim,
    // `wirk-herdr/src/claim_hook.rs`), so those default to `--output`
    // addressing; a `DeterministicWorld`'s `expected_artifacts` are
    // written straight into the checkout by its own executor
    // (`executors::child::ChildExecutor::file_claim`'s own doc: "it
    // never reaches for the managed output area"), so those default to
    // `--artifact` addressing exactly as before. An optional
    // (`required: false`) declared output is never added by this
    // fallback: the daemon refuses `MissingArtifact` for any name a
    // Claim supplies whether or not it is required, so naming one by
    // default would turn it into a requirement the actor never agreed
    // to (0213) — it is claimed only when named explicitly. Either
    // explicit flag keeps its meaning exactly — this fallback only
    // fires when the caller supplied neither.
    if artifacts.is_empty() && outputs.is_empty() && question.is_none() {
        match fetch_output_contract_names(&pointer.socket, &triple) {
            Ok(ContractNames::Managed(names)) => {
                outputs.extend(names);
            }
            Ok(ContractNames::Checkout(names)) => {
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
        triple,
        kind,
        artifacts,
        outputs,
        // Ruling 0257: carried to wirkd, which decides on it under its
        // own journal lock. Nothing is pre-checked here: a hook that
        // asked first and claimed second would still be racing whatever
        // happened in between.
        origin: Some(if automatic {
            ClaimOrigin::Automatic
        } else {
            ClaimOrigin::Deliberate
        }),
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

/// A bare Claim's declared output names, tagged by which addressing
/// they resolve through — decided by which World kind declared them
/// (ruling 0212), never guessed from which file happens to exist.
enum ContractNames {
    /// `ActorWorld.output_contract`: staged under `wirk output dir`,
    /// so a bare Claim reaches for them the same way `--output NAME`
    /// does.
    Managed(Vec<String>),
    /// `DeterministicWorld.expected_artifacts`: written straight into
    /// the checkout by the executor, so a bare Claim reaches for them
    /// the same way `--artifact NAME=PATH` does (at their own
    /// worktree-relative name).
    Checkout(Vec<String>),
}

/// Asks wirkd's existing `run_outputs` verb — the same scoped query
/// `wirk output` itself calls (`output_command`, `RunOutputsPayload`) —
/// for the declared output contract of *this Run's own bound Waypoint*,
/// and returns its *required* names, in the order the Route authored
/// them.
///
/// Ruling 0235: this used to ask `status` for "the current Waypoint's
/// reserved World" — `handle_status`'s `result["world"]`, whichever
/// Waypoint the *Work* is on right now. For the Run that is still
/// current that is the same answer; for a Run a later Waypoint has
/// already superseded (an old Run's automatic Stop-hook Claim firing
/// after `wirk run` moved on) it silently names a *different*
/// Waypoint's outputs — the exact producer defect that let a finished
/// review Run's bare Claim address its Work's next `publish` Waypoint's
/// output name. `run_outputs` resolves from the triple's own `run_id`
/// (`handle_run_outputs`, `find_run` then that Run's own journaled
/// Waypoint definition) precisely because there is no other caller of
/// this verb it could mean: an actor's bare Claim is always about the
/// Run it is running as, never about wherever its Work has since moved.
///
/// A `required: false` spec is filtered out here (ruling 0213): the
/// daemon validates every name a Claim actually supplies and refuses
/// `MissingArtifact` for any of them that is absent, `required` or not
/// (`server.rs`'s own managed- and checkout-artifact validation) — so
/// naming an optional output by default would turn it into a
/// requirement the actor never agreed to. Explicit `--output`/
/// `--artifact` are untouched by this filter: a caller that names an
/// optional output by hand still has it validated, present or absent,
/// exactly as before.
fn fetch_output_contract_names(
    socket: &Path,
    triple: &ExecutionTriple,
) -> Result<ContractNames, String> {
    let reply = wirkd::client::call(
        socket,
        &Request::run_outputs(wirkd::RunOutputsPayload {
            triple: triple.clone(),
        }),
    )
    .map_err(|err| err.to_string())?;
    let result = match reply {
        Reply::Ok { result, .. } => result,
        Reply::Err { error, .. } => {
            return Err(format!(
                "run_outputs refused: {} {}",
                error.code, error.message
            ));
        }
    };
    let kind = result
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "wirkd run_outputs carries no Waypoint kind for this Run".to_string())?;
    let names: Vec<String> = result
        .get("outputs")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|spec| spec["required"].as_bool().unwrap_or(false))
        .filter_map(|spec| spec["name"].as_str().map(str::to_string))
        .collect();
    match kind {
        "actor" => Ok(ContractNames::Managed(names)),
        "deterministic" => Ok(ContractNames::Checkout(names)),
        other => Err(format!(
            "wirkd run_outputs named an unclaimable Waypoint kind `{other}` for this Run"
        )),
    }
}

// ---- wirk output (ruling 0145) --------------------------------------

/// `wirk output [list] [--json]` / `wirk output dir`: where this Run's
/// actor writes the outputs its Waypoint declares, and which of them are
/// staged.
///
/// The same triple-only door `wirk world show` uses, for the same reason
/// (0001 D3, D5): an actor asks about the Run *it* is executing, and
/// there is no `--work`, no `--run` and no path argument here — the
/// location is derived by wirkd from ids it already holds, so there is
/// nothing for a caller to point somewhere else.
///
/// `dir` prints that destination alone, for `$(wirk output dir)` in a
/// shell — this Run's managed staging area for an Actor Waypoint, and
/// the execution directory its own World names for a Deterministic one.
/// `list` (the default) prints one line per declared output. A declared
/// name that cannot be addressed is printed as `unaddressable` with the
/// rule it breaks, so the actor learns that before producing the file
/// rather than at its Claim.
fn output_command(rest: &[String]) -> ExitCode {
    let (mode, flags) = match rest.first().map(String::as_str) {
        Some("dir") => ("dir", &rest[1..]),
        Some("list") => ("list", &rest[1..]),
        Some(flag) if flag.starts_with("--") => ("list", rest),
        None => ("list", rest),
        _ => return output_usage(),
    };
    let mut json_out = false;
    for flag in flags {
        match flag.as_str() {
            "--json" => json_out = true,
            _ => return output_usage(),
        }
    }
    if mode == "dir" && json_out {
        return output_usage();
    }

    let triple = match world_triple() {
        Ok(triple) => triple,
        Err(missing) => {
            for name in &missing {
                eprintln!("wirk output: missing {name}");
            }
            return ExitCode::from(1);
        }
    };
    let estate_root = triple["WIRK_ESTATE_ROOT"].clone();
    let pointer = match wirkd::client::locate(Path::new(&estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk output: {err}");
            return ExitCode::from(2);
        }
    };
    let payload = wirkd::RunOutputsPayload {
        triple: ExecutionTriple {
            estate_root,
            work_id: WorkId(triple["WIRK_WORK_ID"].clone()),
            run_id: RunId(triple["WIRK_RUN_ID"].clone()),
        },
    };
    let result = match wirkd::client::call(&pointer.socket, &Request::run_outputs(payload)) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            eprintln!("wirk output: {} {}", error.code, error.message);
            return ExitCode::from(3);
        }
        Err(err) => {
            eprintln!("wirk output: {err}");
            return ExitCode::from(2);
        }
    };
    let kind = result["kind"].as_str().unwrap_or("?");
    if mode == "dir" {
        let Some(staging) = result["staging"].as_str() else {
            eprintln!(
                "wirk output dir: no destination for this Run: {}",
                result["unavailable_reason"]
                    .as_str()
                    .unwrap_or("unavailable"),
            );
            return ExitCode::from(3);
        };
        println!("{staging}");
        return ExitCode::SUCCESS;
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
        );
        return ExitCode::SUCCESS;
    }
    // A Deterministic Waypoint's declared outputs land in its own
    // execution directory, never in managed staging — said plainly here
    // rather than always printing "staging", which used to be true only
    // for an Actor Waypoint. `execution` rather than `checkout`: that
    // directory is a Git worktree on a Git basis and this Work's own
    // owned execution address on an output-only one.
    match result["staging"].as_str() {
        Some(path) if kind == "deterministic" => println!("execution {path}"),
        Some(path) => println!("staging {path}"),
        None => println!(
            "no destination: {}",
            result["unavailable_reason"]
                .as_str()
                .unwrap_or("unavailable"),
        ),
    }
    let empty = Vec::new();
    let outputs = result["outputs"].as_array().unwrap_or(&empty);
    if outputs.is_empty() {
        println!("  (this Waypoint declares no outputs)");
    }
    for output in outputs {
        let name = output["name"].as_str().unwrap_or("?");
        let required = if output["required"].as_bool().unwrap_or(false) {
            "required"
        } else {
            "optional"
        };
        if output["addressable"].as_bool().unwrap_or(false) {
            println!(
                "  {name} {required} {} {}",
                if output["staged"].as_bool().unwrap_or(false) {
                    "staged"
                } else {
                    "not-staged"
                },
                output["path"].as_str().unwrap_or("?"),
            );
        } else {
            println!(
                "  {name} {required} unaddressable: {}",
                output["detail"].as_str().unwrap_or("?"),
            );
        }
    }
    // Which flag actually addresses these names is this Run's own bound
    // Waypoint kind, the same `ContractNames::Managed`/`::Checkout`
    // split `fetch_output_contract_names` applies to a bare Claim.
    // Printing `--output NAME` unconditionally named, for a
    // Deterministic Run, the one form its own Claim never uses.
    match kind {
        "deterministic" => {
            println!(
                "claim with: wirk claim (its required declared outputs above are claimed at \
                 their own names in this directory); by hand: wirk claim --artifact NAME=NAME"
            );
        }
        _ => {
            println!(
                "claim with: wirk claim (its required declared outputs above are claimed from \
                 this Run's managed staging); by hand: wirk claim --output NAME"
            );
        }
    }
    ExitCode::SUCCESS
}

fn output_usage() -> ExitCode {
    eprintln!("usage: wirk output [list [--json] | dir]");
    ExitCode::from(1)
}

// ---- wirk artifact -------------------------------------------------

/// `wirk artifact read --claim <id> --name <name>`: the bytes a
/// validated Claim of this Work was checked against, written to stdout.
///
/// `wirk artifact export --claim <id> --name <name> --to <path>`: the
/// same bytes, written to a destination the caller names.
///
/// Both go through the same triple-only door `wirk output` uses: the
/// Work is this Run's own bound Work, never a named one. The daemon
/// re-verifies the bytes against the digest the Claim was validated
/// against before reporting an address, and `export` verifies again
/// after writing, so a destination that does not byte-match the Claim
/// is reported as a failure rather than left in place as if it did.
///
/// Ownership of the destination is the caller's, and is never inferred:
/// `--to` is required for `export` — this verb never picks a
/// destination — and an existing file is refused rather than
/// overwritten unless `--force` says otherwise.
fn artifact_command(rest: &[String]) -> ExitCode {
    let mode = match rest.first().map(String::as_str) {
        Some("read") => "read",
        Some("export") => "export",
        _ => return artifact_usage(),
    };
    let mut claim: Option<String> = None;
    let mut name: Option<String> = None;
    let mut destination: Option<String> = None;
    let mut force = false;
    let mut estate: Option<String> = None;
    let mut work: Option<String> = None;
    let mut requesting: Option<String> = None;
    let mut admin = false;
    let mut index = 1usize;
    while index < rest.len() {
        match rest[index].as_str() {
            "--claim" if index + 1 < rest.len() => {
                claim = Some(rest[index + 1].clone());
                index += 2;
            }
            "--name" if index + 1 < rest.len() => {
                name = Some(rest[index + 1].clone());
                index += 2;
            }
            "--to" if index + 1 < rest.len() => {
                destination = Some(rest[index + 1].clone());
                index += 2;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--estate" if index + 1 < rest.len() => {
                estate = Some(rest[index + 1].clone());
                index += 2;
            }
            "--work" if index + 1 < rest.len() => {
                work = Some(rest[index + 1].clone());
                index += 2;
            }
            "--requesting-work" if index + 1 < rest.len() => {
                requesting = Some(rest[index + 1].clone());
                index += 2;
            }
            "--admin" => {
                admin = true;
                index += 1;
            }
            _ => return artifact_usage(),
        }
    }
    let (Some(claim), Some(name)) = (claim, name) else {
        return artifact_usage();
    };
    if mode == "export" && destination.is_none() {
        eprintln!("wirk artifact: export needs --to <path>: this verb never picks a destination");
        return ExitCode::from(1);
    }
    if mode == "read" && (destination.is_some() || force) {
        // `--force` was accepted and silently ignored here. A flag that
        // does nothing is a promise the verb does not keep: `read`
        // writes nothing, so there is nothing for it to force.
        return artifact_usage();
    }

    // `--estate`/`--work` (ruling 0339): the named-Work door, for a
    // caller with no execution triple to present at all — an
    // administrative shell — or one naming its own scope explicitly by
    // id rather than by an injected environment. `export` keeps no such
    // door: its destination-protection reasoning below is written in
    // terms of *this Run's own* staging area and managed-storage
    // boundary, which a caller with no Run has none of.
    if estate.is_some() || work.is_some() || requesting.is_some() || admin {
        if mode != "read" {
            eprintln!(
                "wirk artifact: --estate/--work/--requesting-work/--admin name a Work for \
                 `read` only; `export` always reads this Run's own bound Work from its \
                 injected execution triple"
            );
            return artifact_usage();
        }
        let (Some(estate), Some(work)) = (estate, work) else {
            eprintln!(
                "wirk artifact: --estate <root> and --work <id> are both required to read \
                 without an execution triple"
            );
            return artifact_usage();
        };
        let scope = match resolve_scope("wirk artifact read", &estate, requesting, admin) {
            Ok(scope) => scope,
            Err(refusal) => {
                eprintln!("wirk artifact read: {refusal}");
                return ExitCode::from(1);
            }
        };
        if let Some(note) = &scope.note {
            eprintln!("wirk artifact read: {note}");
        }
        let pointer = match wirkd::client::locate(Path::new(&estate)) {
            Ok(pointer) => pointer,
            Err(err) => {
                eprintln!("wirk artifact: {err}");
                return ExitCode::from(2);
            }
        };
        let payload = match scope.requesting {
            Some(requester) => {
                wirkd::WorkArtifactPayload::scoped(WorkId(work), ClaimId(claim), name, requester)
            }
            None => wirkd::WorkArtifactPayload::admin(WorkId(work), ClaimId(claim), name),
        };
        let result = match wirkd::client::call(&pointer.socket, &Request::work_artifact(payload)) {
            Ok(Reply::Ok { result, .. }) => result,
            Ok(Reply::Err { error, .. }) => {
                eprintln!("wirk artifact: {} {}", error.code, error.message);
                return ExitCode::from(3);
            }
            Err(err) => {
                eprintln!("wirk artifact: {err}");
                return ExitCode::from(2);
            }
        };
        let bytes = match verify_claimed_bytes(&result) {
            Ok(bytes) => bytes,
            Err(code) => return code,
        };
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        return match out.write_all(&bytes).and_then(|()| out.flush()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("wirk artifact: {err}");
                ExitCode::from(2)
            }
        };
    }

    let triple = match world_triple() {
        Ok(triple) => triple,
        Err(missing) => {
            for name in &missing {
                eprintln!("wirk artifact: missing {name}");
            }
            return ExitCode::from(1);
        }
    };
    let estate_root = triple["WIRK_ESTATE_ROOT"].clone();
    let pointer = match wirkd::client::locate(Path::new(&estate_root)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk artifact: {err}");
            return ExitCode::from(2);
        }
    };
    let payload = wirkd::RunArtifactPayload {
        triple: ExecutionTriple {
            estate_root,
            work_id: WorkId(triple["WIRK_WORK_ID"].clone()),
            run_id: RunId(triple["WIRK_RUN_ID"].clone()),
        },
        claim: ClaimId(claim),
        name,
    };
    // The triple is read again after this call (the managed-storage
    // guard below), and `Request::run_artifact` takes the payload by
    // value: bound here, before the move, rather than read from a
    // payload that no longer exists.
    let caller = payload.triple.clone();
    let result = match wirkd::client::call(&pointer.socket, &Request::run_artifact(payload)) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            eprintln!("wirk artifact: {} {}", error.code, error.message);
            return ExitCode::from(3);
        }
        Err(err) => {
            eprintln!("wirk artifact: {err}");
            return ExitCode::from(2);
        }
    };
    let bytes = match verify_claimed_bytes(&result) {
        Ok(bytes) => bytes,
        Err(code) => return code,
    };

    if mode == "read" {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        if let Err(err) = out.write_all(&bytes).and_then(|()| out.flush()) {
            eprintln!("wirk artifact: {err}");
            return ExitCode::from(2);
        }
        return ExitCode::SUCCESS;
    }

    let destination = PathBuf::from(destination.expect("export requires --to, checked above"));
    // An export is for getting claimed bytes *out*. It is never a way to
    // write into the area that holds them: a destination resolving onto
    // the artifact's own stored path would let `--force` rewrite the
    // very bytes a Claim was validated against, through a verb whose
    // whole promise is that it only reads them.
    //
    // Ruling 0283: the area protected is this *estate's* managed
    // storage, not only this Work's own subtree of it. A caller holding
    // a valid triple for its own Work could name another Work's
    // `outputs/claims/<claim>/<name>` or its `journal.ndjson` and, with
    // `--force`, overwrite bytes a validated Claim was checked against
    // or the Trail itself. `--to` is real destination authority, but it
    // is authority over where this caller's *own* output goes; it is not
    // a waiver of the boundary around every other Work's record.
    //
    // The one destination inside that area this workflow genuinely
    // needs is the caller's own Run staging directory: an Actor stage
    // exporting a prior stage's claimed artifact in order to revise it
    // writes exactly there, and that is where `wirk output dir` sends
    // it. Excluded by name, from the same helper that derives it
    // (`outputs::staging_dir`), rather than by a prefix match invented
    // here. `<estate>/worktrees/<work>` is not in the protected area at
    // all: a Git stage's checkout is an ordinary working tree, and
    // exporting into it is the normal way to bring an artifact into the
    // work.
    let estate = PathBuf::from(&caller.estate_root);
    let works_root = estate.join("works");
    let own_staging = wirk_core::outputs::staging_dir(&estate, &caller.work_id, &caller.run_id);
    let source = Path::new(result["path"].as_str().unwrap_or(""));
    if let Some(reason) =
        managed_storage_conflict(&works_root, own_staging.as_deref(), source, &destination)
    {
        eprintln!("wirk artifact: {reason}");
        return ExitCode::from(1);
    }
    let opened = if force {
        // `--force` respects an explicit destination; it does not
        // respect whatever happens to be standing at it. A non-regular
        // entry is refused before anything is truncated.
        //
        // What `O_NOFOLLOW` on this open gives, precisely (ruling 0283):
        // the *final component* is not a symlink at the moment it is
        // opened. It does not bind an ancestor — a directory on the way
        // to the destination replaced by a symlink is still followed —
        // and it does not bind file identity: a regular file swapped
        // for another regular file, or for a hard link to one, between
        // the inspection and the open is still opened. So the checks
        // that matter are made on the descriptor itself, below, and the
        // file is opened *without* `O_TRUNC` so nothing is destroyed
        // before they run.
        match std::fs::symlink_metadata(&destination) {
            Ok(meta) if meta.file_type().is_file() => {}
            Ok(_) => {
                eprintln!(
                    "wirk artifact: {} is not a regular file; --force replaces a file, it does \
                     not write through whatever is standing at the destination",
                    destination.display()
                );
                return ExitCode::from(1);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                eprintln!("wirk artifact: {} {err}", destination.display());
                return ExitCode::from(2);
            }
        }
        open_destination(&destination, true)
    } else {
        // Atomic no-clobber. `exists()` follows links, so a dangling
        // symlink at `--to` answered `false`, the write landed at the
        // link's target, and the read-back followed the same link and
        // reported success — the export silently went somewhere the
        // caller never named. `create_new` is one syscall and refuses
        // *any* existing entry, symlink included.
        open_destination(&destination, false)
    };
    let mut file = match opened {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!(
                "wirk artifact: {} already exists; nothing was written (--force to replace it)",
                destination.display()
            );
            return ExitCode::from(1);
        }
        Err(err) => {
            eprintln!(
                "wirk artifact: could not write {}: {err}",
                destination.display()
            );
            return ExitCode::from(2);
        }
    };
    // The destination that was actually opened, judged as the open file
    // object rather than as the path that named it (ruling 0283). This
    // is where an ancestor swapped for a symlink, or a hard link into
    // managed storage, is caught — the canonical check above ran
    // against a path, and a path can stop describing this file the
    // instant after it is inspected. Nothing has been truncated yet.
    if let Some(reason) = opened_destination_conflict(
        &file,
        &works_root,
        own_staging.as_deref(),
        &destination,
        force,
    ) {
        eprintln!("wirk artifact: {reason}");
        if !force {
            // The no-clobber open creates the entry before this check
            // can run, so an entry this call made may be standing there
            // — empty, never written to, and said plainly rather than
            // left for the caller to discover.
            eprintln!(
                "wirk artifact: an empty file this call created may remain at {}; none of the \
                 claimed bytes were written to it",
                destination.display()
            );
        }
        return ExitCode::from(1);
    }
    if force && let Err(err) = file.set_len(0) {
        eprintln!(
            "wirk artifact: could not replace {}: {err}",
            destination.display()
        );
        return ExitCode::from(2);
    }
    {
        use std::io::Write;
        if let Err(err) = file.write_all(&bytes).and_then(|()| file.flush()) {
            eprintln!(
                "wirk artifact: could not write {}: {err}",
                destination.display()
            );
            return ExitCode::from(2);
        }
    }
    drop(file);
    // Byte-match confirmed at the destination, not assumed from a
    // successful write: a short write, a full filesystem, or a
    // destination that is not what it looked like all surface here.
    match read_regular_no_follow(&destination, bytes.len() as u64 + 1) {
        Ok(written) if written == bytes => {
            let digest = result["digest"].as_str().unwrap_or("");
            println!(
                "exported {} ({} bytes, {digest}) to {}",
                result["name"].as_str().unwrap_or("?"),
                bytes.len(),
                destination.display()
            );
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!(
                "wirk artifact: {} does not byte-match the Claim it was exported from",
                destination.display()
            );
            ExitCode::from(3)
        }
        Err(err) => {
            eprintln!(
                "wirk artifact: {} could not be read back to confirm it: {err}",
                destination.display()
            );
            ExitCode::from(3)
        }
    }
}

/// Reads at most `cap` bytes of `path` as a regular file, following no
/// symlink at the final component and never blocking on a FIFO or a
/// device.
///
/// `std::fs::read` does a fresh path lookup that follows links and
/// happily opens anything the kernel will open — including an entry that
/// replaced a regular file between an inspection and the read. Here the
/// open *is* the inspection: `O_NOFOLLOW` refuses a symlink, `O_NONBLOCK`
/// makes the open of a FIFO or device return instead of parking this
/// process, and `fstat` on the descriptor that survives refuses anything
/// that is not a regular file before a byte is read. One file object,
/// checked and read.
fn read_regular_no_follow(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(&mut file, cap), &mut bytes)?;
    Ok(bytes)
}

/// Opens an export destination: atomically created when it must not
/// already exist, and otherwise replaced without following a symlink
/// that appeared since it was inspected.
fn open_destination(destination: &Path, replace: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if replace {
        // Deliberately *not* `truncate(true)`: the destination's
        // identity is checked on this descriptor before anything is
        // destroyed (ruling 0283), and the caller truncates afterwards.
        options
            .create(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    } else {
        options.create_new(true);
    }
    let file = options.open(destination)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", destination.display()),
        ));
    }
    Ok(file)
}

/// Why this destination may not be written, when it addresses the
/// artifact's own managed storage rather than somewhere outside it.
///
/// Compared after canonicalization, and the *parent* is compared too, so
/// a destination naming a file that does not exist yet inside the
/// managed area is caught as well as one that does. A destination that
/// cannot be canonicalized at all is not thereby a conflict: it is
/// simply somewhere this check cannot place, and the open below decides
/// it.
fn managed_storage_conflict(
    works_root: &Path,
    own_staging: Option<&Path>,
    source: &Path,
    destination: &Path,
) -> Option<String> {
    if let Ok(canonical_source) = std::fs::canonicalize(source)
        && std::fs::canonicalize(destination).is_ok_and(|actual| actual == canonical_source)
    {
        return Some(format!(
            "{} is the artifact's own stored path; an export reads managed storage, it never \
             writes into it",
            destination.display()
        ));
    }
    let managed = std::fs::canonicalize(works_root).ok()?;
    let destination_area = std::fs::canonicalize(destination.parent()?).ok()?;
    if !destination_area.starts_with(&managed) {
        return None;
    }
    if own_staging
        .and_then(|staging| std::fs::canonicalize(staging).ok())
        .is_some_and(|staging| destination_area.starts_with(&staging))
    {
        // This Run's own staging directory — where this caller's
        // outputs are supposed to be written, and the destination a
        // revision stage exports a prior artifact into.
        return None;
    }
    Some(format!(
        "{} is inside this estate's managed Work storage ({}), which holds every Work's \
         journal, projections and validated Claim bytes; an export writes outside the area it \
         reads from. This Run's own output directory is the exception, and `wirk output dir` \
         prints it",
        destination.display(),
        managed.display()
    ))
}

/// Why the destination that was *opened* may not be written, judged on
/// the open file object rather than on the path that named it.
///
/// The canonical path check runs before the open and can be made stale
/// by anything that happens in between; this runs after it, on a
/// descriptor that can no longer be retargeted. Two things it can
/// establish that the path check cannot:
///
/// * **Where this file actually is.** `/proc/self/fd/<n>` is the
///   kernel's own answer for the open file, fully resolved — so an
///   ancestor directory swapped for a symlink after the path check
///   shows up here as the real location, inside managed storage.
/// * **Whether it is the only name for these bytes.** A hard link in an
///   unprotected directory pointing at a Claim's stored file has its
///   own harmless-looking path and the protected file's contents.
///   `st_nlink > 1` under `--force` means this file has other names
///   that cannot be enumerated from here, so replacing its contents
///   might replace theirs; it is refused rather than guessed at.
///
/// `None` where nothing is proven wrong, including where `/proc` cannot
/// be read: that failure is disclosed on stderr rather than converted
/// into a refusal or into a silent claim of safety.
fn opened_destination_conflict(
    file: &std::fs::File,
    works_root: &Path,
    own_staging: Option<&Path>,
    destination: &Path,
    force: bool,
) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    if force
        && let Ok(meta) = file.metadata()
        && meta.nlink() > 1
    {
        return Some(format!(
            "{} has {} names on this filesystem, so replacing its contents would replace them \
             all — and this verb cannot tell whether one of them is managed storage; nothing \
             was written (remove the other names, or export to a fresh path)",
            destination.display(),
            meta.nlink()
        ));
    }
    match std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())) {
        Ok(actual) => {
            let managed = std::fs::canonicalize(works_root).ok()?;
            if !actual.starts_with(&managed) {
                return None;
            }
            if own_staging
                .and_then(|staging| std::fs::canonicalize(staging).ok())
                .is_some_and(|staging| actual.starts_with(&staging))
            {
                return None;
            }
            Some(format!(
                "the destination {} resolved to {}, inside this estate's managed Work storage \
                 ({}); nothing was written",
                destination.display(),
                actual.display(),
                managed.display()
            ))
        }
        Err(err) => {
            eprintln!(
                "wirk artifact: note: the opened destination could not be re-checked against \
                 managed storage ({err}); the path check before the open is all that stands \
                 behind this write"
            );
            None
        }
    }
}

fn artifact_usage() -> ExitCode {
    eprintln!(
        "usage: wirk artifact read --claim <id> --name <name> | wirk artifact read --estate \
         <root> --work <id> (--admin | --requesting-work <id>) --claim <id> --name <name> | \
         wirk artifact export --claim <id> --name <name> --to <path> [--force]"
    );
    ExitCode::from(1)
}

/// **One read, and the digest is of that read.** The daemon has already
/// verified its own read and returned an address; custody has to be
/// re-established on *this* side, on the exact buffer about to be
/// written to stdout or to a file, rather than trusting the daemon's
/// digest for a second, separate lookup of the same path.
pub(crate) fn verify_claimed_bytes(result: &serde_json::Value) -> Result<Vec<u8>, ExitCode> {
    let source = Path::new(result["path"].as_str().unwrap_or(""));
    let digest = result["digest"].as_str().unwrap_or("").to_string();
    let reported_len = result["bytes"].as_u64().unwrap_or(0);
    let bytes = match read_regular_no_follow(source, reported_len.saturating_add(1)) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("wirk artifact: the claimed bytes could not be read: {err}");
            return Err(ExitCode::from(2));
        }
    };
    if wirk_core::ArtifactReceipt::digest_of_bytes(&bytes) != digest {
        eprintln!(
            "wirk artifact: the claimed bytes are no longer readable at the digest this Claim \
             was validated against; nothing was written"
        );
        return Err(ExitCode::from(3));
    }
    Ok(bytes)
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
    // The whole projection body, shared with the actor's own composed
    // first prompt (`compose_first_prompt` in `wirk-herdr`) through
    // `wirk_core::render_projection_report` — one renderer, not two
    // ports of the same logic kept in step by hand. `Console` is this
    // caller's style: a person here can paste a printed coordinate
    // straight into `wirk atlas resolve`, so this surface is the one
    // that prints them.
    let current = result
        .get("current")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    print!(
        "{}",
        wirk_core::render_projection_report(
            projection,
            result.get("receipt"),
            current,
            wirk_core::ReportStyle::Console,
        )
    );
}

fn world_usage() -> ExitCode {
    eprintln!(
        "usage: wirk world show [--revision N] [--json] | wirk world expand (--question TEXT | \
         --reference HANDLE) [--reason TEXT] [--json]"
    );
    ExitCode::from(1)
}

fn claim_usage() -> ExitCode {
    eprintln!(
        "usage: wirk claim [--artifact NAME=PATH]... [--output NAME]... [--question TEXT] \
         [--automatic]\n  \
         --artifact  a file in this Run's own checkout, at the path you name\n  \
         --output    a declared output this Work owns, by name; run `wirk output` for where to \
         write it\n  \
         --question  ask instead of finishing: the Run stays open and the Work waits on the \
         answer\n  \
         --automatic wirk's own turn-end hook fired this, nobody decided it (ruling 0257): the \
         daemon refuses such an attempt while this Run's own question is still standing, and \
         leaves the question visible. Never pass it by hand — your own `wirk claim` is the \
         deliberate completion that finishes this Run, question or no question\n  \
         with none of --artifact/--output/--question: claims every *required* declared output at \
         its own name, addressed the way its World writes it — managed for an actor Waypoint, \
         checkout for a deterministic one; an optional declared output is claimed only by naming \
         it explicitly"
    );
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
        // Ruling 0251 F4: `ping`'s resource answer lists every running
        // job's source alias. Inside an actor context it is asked as
        // that actor's own Work, so the listing is what that Work may
        // control; `--admin` is the deliberate estate-wide read. The
        // operator's own shell is unchanged.
        "ping" => {
            // `ping` is a health check and stays one: a scope this
            // caller cannot resolve withholds the job listing and says
            // so, rather than refusing the whole verb or widening to
            // the administrative answer.
            let (work, jobs) = match resolve_scope(
                "wirk wirkd ping",
                &estate,
                flag_value(&rest[1..], "--requesting-work"),
                rest[1..].iter().any(|arg| arg == "--admin"),
            ) {
                Ok(scope) => {
                    if let Some(note) = &scope.note {
                        eprintln!("wirk wirkd ping: {note}");
                    }
                    (scope.requesting, true)
                }
                Err(refusal) => {
                    eprintln!("wirk wirkd ping: {refusal}");
                    (None, false)
                }
            };
            wirkd_client_call(
                &estate,
                &Request::ping_as(wirkd::PingPayload { work, jobs }),
                |result| {
                    println!(
                        "protocol_version {} pid {}",
                        result["protocol_version"].as_u64().unwrap_or_default(),
                        result["pid"].as_u64().unwrap_or_default()
                    );
                    // P4.5 B (ruling 0237): what this daemon can actually
                    // enforce, in the operator's own terms. A bound nobody can
                    // see is not a bound, and an unavailable capability is
                    // printed with its reason rather than left silent — silence
                    // reads as "in force".
                    let resources = &result["resources"];
                    if resources.is_null() {
                        return;
                    }
                    let policy = &resources["policy"];
                    println!(
                        "resources: expensive {}/{} (host {}), materialization {}, deadline {}s",
                        policy["effective_estate_slots"]
                            .as_u64()
                            .unwrap_or_default(),
                        policy["max_expensive"].as_u64().unwrap_or_default(),
                        policy["max_host_expensive"].as_u64().unwrap_or_default(),
                        policy["max_materialization"].as_u64().unwrap_or_default(),
                        policy["job_deadline_secs"].as_u64().unwrap_or_default(),
                    );
                    if let Some(note) = policy["capacity_note"].as_str() {
                        println!("resources: {note}");
                    }
                    println!(
                        "resources: configured in {}",
                        policy["configured_in"].as_str().unwrap_or("(defaults)")
                    );
                    if let Some(summary) = resources["capabilities"]["summary"].as_str() {
                        println!("capabilities: {summary}");
                    }
                    if let Some(limit) = resources["capabilities"]["containment_limit"].as_str() {
                        println!("capabilities: {limit}");
                    }
                    // Memory, at the scope that actually applies to this
                    // process — soft threshold and hard ceiling reported
                    // separately, because they are different promises.
                    let observed = &resources["observed"];
                    match (
                        observed["memory_pressure_some_avg10"].as_f64(),
                        observed["effective_available_bytes"].as_u64(),
                    ) {
                        (Some(avg10), Some(available)) => println!(
                            "observed: memory pressure some avg10 {avg10:.2} from {}, {available} bytes \
                     available against {} (advisory sample, not an allocation guarantee)",
                            observed["memory_pressure_scope"]
                                .as_str()
                                .unwrap_or("an unnamed scope"),
                            observed["effective_available_origin"]
                                .as_str()
                                .unwrap_or("an unidentified bound"),
                        ),
                        (Some(avg10), None) => println!(
                            "observed: memory pressure some avg10 {avg10:.2} from {}; no byte figure \
                     could be read",
                            observed["memory_pressure_scope"]
                                .as_str()
                                .unwrap_or("an unnamed scope"),
                        ),
                        _ => println!("observed: memory unobserved"),
                    }
                    match (
                        observed["soft_headroom_bytes"].as_u64(),
                        observed["soft_headroom_from"].as_str(),
                    ) {
                        (Some(soft), Some(from)) => println!(
                            "observed: {soft} bytes before the soft throttling threshold \
                     (memory.high) of {from} — throttling, not a ceiling; it refuses nothing \
                     by itself"
                        ),
                        _ => println!(
                            "observed: no soft throttling threshold applies to this process"
                        ),
                    }
                    if let Some(unavailable) = observed["unavailable"].as_array()
                        && !unavailable.is_empty()
                    {
                        for reason in unavailable {
                            if let Some(reason) = reason.as_str() {
                                println!("observed: unavailable — {reason}");
                            }
                        }
                    }
                    if let Some(pool) = resources["host_pool"].as_object() {
                        // Three separate facts, printed as three separate facts:
                        // what this estate asked for, what the pool has agreed,
                        // and what this estate would actually be offered. An
                        // uninitialized pool says so instead of reporting a
                        // number nobody has agreed to.
                        println!(
                            "host pool: {} at {}",
                            pool.get("agreement_status")
                                .and_then(|value| value.as_str())
                                .unwrap_or("?"),
                            pool.get("directory")
                                .and_then(|value| value.as_str())
                                .unwrap_or("?"),
                        );
                        match (
                            pool.get("agreed_capacity")
                                .and_then(serde_json::Value::as_u64),
                            pool.get("effective_usable_slots")
                                .and_then(serde_json::Value::as_u64),
                        ) {
                            (Some(agreed), Some(usable)) => println!(
                                "host pool: agreed capacity {agreed}, this estate configured {} and is \
                         offered {usable}",
                                pool.get("configured_max_host_expensive")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or_default(),
                            ),
                            _ => println!(
                                "host pool: no agreed capacity yet; this estate is configured {} and \
                         nothing has been initialized by reading it",
                                pool.get("configured_max_host_expensive")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or_default(),
                            ),
                        }
                        if let Some(notes) = pool.get("notes").and_then(|value| value.as_array()) {
                            for note in notes {
                                if let Some(note) = note.as_str() {
                                    println!("host pool: {note}");
                                }
                            }
                        }
                        if let Some(advisory) =
                            pool.get("advisory").and_then(|value| value.as_str())
                        {
                            println!("host pool: {advisory}");
                        }
                    }
                    if let Some(withheld) = resources["running_jobs_undisclosed"].as_str() {
                        println!("running jobs: withheld — {withheld}");
                    } else if let Some(running) = resources["running_jobs"].as_array() {
                        println!(
                            "running jobs ({}): {}",
                            resources["running_jobs_scope"]
                                .as_str()
                                .unwrap_or("administrative"),
                            running.len()
                        );
                        for job in running {
                            println!(
                                "  {} {} scope {} signalled {}",
                                job["job_id"].as_str().unwrap_or("?"),
                                job["verb"].as_str().unwrap_or("?"),
                                job["scope"].as_str().unwrap_or("?"),
                                job["cancel_signalled"].as_bool().unwrap_or(false)
                            );
                        }
                    }
                    let recovery = &resources["recovery_at_start"];
                    let killed = recovery["cgroups_killed"]
                        .as_array()
                        .map(|v| v.len())
                        .unwrap_or(0);
                    let staged = recovery["staging_removed"]
                        .as_array()
                        .map(|v| v.len())
                        .unwrap_or(0);
                    let seen = recovery["records_seen"].as_u64().unwrap_or_default();
                    if seen > 0 || killed > 0 || staged > 0 {
                        println!(
                            "recovery at start: {seen} recorded job(s), {killed} cgroup(s) killed, \
                     {staged} staging directory/ies removed"
                        );
                    }
                    // Recovery lives behind the atlas mutex; a busy estate
                    // answers "unobserved" rather than making this read queue.
                    if let Some(unavailable) = recovery["unavailable"].as_str() {
                        println!("recovery at start: unobserved — {unavailable}");
                    }
                },
            )
        }
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
            rest[1..].iter().any(|arg| arg == "--json"),
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
pub(crate) struct ResolvedScope {
    /// `Some(requester)` is the scoped read as that Work; `None` is the
    /// administrative read — reached only when it was named, or when
    /// there is no actor context at all (the operator's own default).
    pub(crate) requesting: Option<WorkId>,
    /// The target when `--work` is absent: the actor's own Work inside
    /// an actor context, and `None` — every Work under the estate, the
    /// operator's listing — outside one.
    pub(crate) default_target: Option<String>,
    /// Printed once on stderr before anything is fetched, when the
    /// resolution is worth saying out loud: which scope answered and
    /// why. Silent for the plain operator, whose behavior is unchanged.
    pub(crate) note: Option<String>,
}

/// Resolves the scope for `status`/`watch` **before** the daemon is
/// located or a single Work is read, so a refusal here costs no
/// content. `Err` is the refusal text; the caller prints it and exits 1
/// (usage), the same exit a malformed command line already takes.
pub(crate) fn resolve_scope(
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
  watch   that Work's journal appends, streamed as they land. --json
          prints the events themselves, one complete parseable object
          per stdout line, with no `work_id` prefix (the event carries
          its own `work`/`run` identity); refusals and the other
          diagnostics go to stderr, and the exit codes are unchanged.

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
///
/// `--json` switches the stream from a human line to a machine line:
/// every stdout line is the event's own serialization, one complete
/// parseable object per line, with no `work_id` prefix — the event
/// carries its own `work`/`run` identity. Refusals and the other
/// diagnostics (a refused scope, an unacknowledged scope, a transport
/// failure) go to stderr instead, and the exit codes are unchanged: a
/// refused watch still exits nonzero, an unrefused stream still exits
/// zero. Without the flag the human shape stands.
fn wirkd_watch_command(
    estate: &str,
    work_filter: Option<String>,
    requesting: Option<String>,
    admin: bool,
    json: bool,
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
                    if json {
                        eprintln!("{work_id} watch_error {err}");
                    } else {
                        let _ = tx.send(format!("{work_id} watch_error {err}"));
                    }
                    return;
                }
            };
            for event in events {
                match event {
                    Ok(event) => {
                        let line = serde_json::to_string(&event)
                            .unwrap_or_else(|_| "<unserializable event>".to_string());
                        // In `--json` mode the serialized event *is* the
                        // line: no `work_id` prefix, because the event
                        // carries its own `work`/`run` identity.
                        if json {
                            if tx.send(line).is_err() {
                                return;
                            }
                        } else if tx.send(format!("{work_id} {line}")).is_err() {
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
                        if json {
                            eprintln!("{work_id} refused {}: {}", detail.code, detail.message);
                        } else {
                            let _ = tx.send(format!(
                                "{work_id} refused {}: {}",
                                detail.code, detail.message
                            ));
                        }
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
                        if json {
                            eprintln!("{work_id} refused scope: {err}");
                        } else {
                            let _ = tx.send(format!("{work_id} refused scope: {err}"));
                        }
                        return;
                    }
                    Err(err) => {
                        if json {
                            eprintln!("{work_id} watch_error {err}");
                        } else {
                            let _ = tx.send(format!("{work_id} watch_error {err}"));
                        }
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

/// Milliseconds since the Unix epoch (`wirk_core::Timestamp`, the wire
/// shape `WorkCleaned.at` carries) rendered as elapsed time from now —
/// `"3m ago"`, `"2h ago"` — never a calendar date or time-of-day, which
/// would need calendar arithmetic no dependency here provides. Integer
/// bucketed duration only, reusing the already-imported `SystemTime`/
/// `UNIX_EPOCH`. A timestamp at or after `now` (clock skew, or a caller
/// reading mid-write) floors at zero rather than printing a negative
/// duration.
fn format_elapsed_ms(at_ms: i64) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis() as i64)
        .unwrap_or(at_ms);
    format_elapsed_from(now_ms, at_ms)
}

/// `format_elapsed_ms`'s pure half, `now_ms` taken as a parameter
/// instead of read from the clock — a deterministic check pins this
/// bucketing without a flaky sleep or a mocked clock.
fn format_elapsed_from(now_ms: i64, at_ms: i64) -> String {
    let elapsed_s = (now_ms - at_ms).max(0) / 1000;
    if elapsed_s < 60 {
        format!("{elapsed_s}s ago")
    } else if elapsed_s < 3600 {
        format!("{}m ago", elapsed_s / 60)
    } else if elapsed_s < 86400 {
        format!("{}h ago", elapsed_s / 3600)
    } else {
        format!("{}d ago", elapsed_s / 86400)
    }
}

/// One shell word, single-quoted so that no character inside it is
/// re-read by the shell — the printed retrieval command is meant to be
/// copied and run, and a Work id, Claim id or output name is not this
/// command's authority to expand, glob or substitute. An embedded
/// single quote is closed, escaped and reopened, the only escape a
/// single-quoted shell word admits.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The listing row's second line: the stage/attempt/Run/result facts a
/// person scanning an estate walk needs to tell a Work that is waiting
/// on someone from one that is still running or already finished.
/// Every value is read out of the `status` reply this row already
/// carries — no extra request, and no field this reply did not already
/// publish.
///
/// Three different answers have to stay distinguishable in `outputs`:
/// no validated Claim has recorded artifacts (`none`), some have and
/// this reader is admitted to them (`<available>/<total> available`),
/// and some have but this reader's own bindings do not cover this
/// Work's (`withheld`). The third is what a narrowed requester sees:
/// `withhold_status_content` replaces each entry's whole `artifacts`
/// list with a marker object, so the count is not merely zero — it is
/// unknown to this reader, and saying `none` there would be a claim
/// this row cannot make.
fn listing_summary(result: &serde_json::Value) -> String {
    let attempt = match result["attempt"].as_u64() {
        Some(attempt) => attempt.to_string(),
        None => "-".to_string(),
    };
    let run = result["run_id"].as_str().unwrap_or("-");
    // `run_state` is the folded state of that same Run — `failed`
    // carries its own status word beside it, which is journal identity
    // and survives narrowing (the cause's `detail` is the content half,
    // and this line never reads it).
    let run_state = match (
        result["run_state"].as_str(),
        result["failure_status"].as_str(),
    ) {
        (Some(state), Some(status)) => format!("{state}:{status}"),
        (Some(state), None) => state.to_string(),
        (None, _) => "-".to_string(),
    };
    let evidence = result["evidence"].as_array();
    let outputs = match evidence {
        None => "-".to_string(),
        Some(entries) if entries.is_empty() => "none".to_string(),
        Some(entries) => {
            let mut total = 0usize;
            let mut available = 0usize;
            let mut withheld = false;
            for entry in entries {
                match entry["artifacts"].as_array() {
                    None => withheld = true,
                    Some(artifacts) => {
                        total += artifacts.len();
                        available += artifacts
                            .iter()
                            .filter(|artifact| artifact["available"].as_bool().unwrap_or(false))
                            .count();
                    }
                }
            }
            if withheld {
                "withheld".to_string()
            } else if total == 0 {
                "none".to_string()
            } else {
                format!("{available}/{total} available")
            }
        }
    };
    format!("attempt {attempt} run {run} {run_state} outputs {outputs}")
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
    // `default_target` is `Some` only inside an actor context (an
    // injected `WIRK_*` triple this process already has), and it names
    // that actor's *own* Work: the one target for which printed
    // retrieval guidance can safely say nothing about scope at all,
    // because the reader's own shell already carries exactly it. Held
    // as the id rather than as a bare "is an actor" flag — the guidance
    // below is printed per Work, and a bound actor may legitimately ask
    // about a related Work it covers, which its own triple does not
    // read. Captured before `scope` is consumed below.
    let own_work = scope.default_target.clone();
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
    // An estate with nothing in it used to answer the human surface with
    // nothing at all: exit 0, not one byte printed, and no way to tell a
    // fresh estate from a lookup that quietly found the wrong one. The
    // Herdr plugin's `wirkd status` action is exactly this call, and what
    // it opened was a blank pane.
    //
    // So the listing says what it looked at and what it found. Both
    // facts, because either alone is the ambiguity: the estate, which is
    // the thing an operator most often has wrong, and the scope it
    // actually asked in, which decides what "none" even covers. It
    // discloses nothing a Work row would not have — a scoped listing
    // names the requester it was already given, and there is no Work to
    // name. The machine surface is untouched: `--json` still prints its
    // empty array, which is a complete answer already.
    if !json && enumerated && work_ids.is_empty() {
        let asked_as = match &requesting {
            Some(requester) => format!("requester {}", requester.0),
            None => "administrative".to_string(),
        };
        println!("estate {estate} scope {asked_as}");
        println!("  no Work is recorded under this estate");
    }
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
                // The second line of every Work, on both surfaces:
                // which attempt of the current stage is live, what that
                // Run's own state is, and whether this Work has a
                // validated result to fetch. All of it is read off
                // fields this reply already carried, and none of it
                // appears anywhere else on the human verb — the per-Run
                // lines below report checkout and pin presence, not
                // attempt or Run state.
                println!("  {}", listing_summary(&result));
                // The estate walk is a *listing* and stops there, one
                // Work per pair of lines, so a person scanning many
                // Works is not handed every World's full stage, Run and
                // evidence history at once. A single named Work
                // (`--work <id>`, or an actor's own inherited target)
                // goes on to the detail below.
                if enumerated {
                    continue;
                }
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
                // Ruling 0224: the same current checkout/pin presence
                // and per-call cleanup history `wirk work clean`'s own
                // scoped JSON reply already carries, rendered on the
                // ordinary human status verb instead of only being
                // reachable with `--json`. Absent (`null`/missing) is
                // printed as `?` — never folded into `absent`, since
                // this World's own binding could not be resolved and
                // "unknown" is a different fact than "checked and
                // gone" (`verify/ASSESSMENT.md`'s own missing/absent
                // distinction). Work completion, current resource
                // presence, and each cleanup call's own effects are
                // three separate facts and stay on three separate
                // kinds of line. `any_run_pin` labels the wire's
                // `runtime_pin_present`, which is true when *any* of
                // this Run's pin components (runtime, Claude, OpenCode
                // — `run_pin_dirs`) still exists, not only the runtime
                // executable; a label of plain `runtime_pin` would read
                // as absent once only the runtime component is gone
                // even though a pin component remains.
                for run in result["runs"].as_array().unwrap_or(&Vec::new()) {
                    let worktree = match run["worktree_present"].as_bool() {
                        Some(true) => "present",
                        Some(false) => "absent",
                        None => "?",
                    };
                    let pin = match run["runtime_pin_present"].as_bool() {
                        Some(true) => "present",
                        Some(false) => "absent",
                        None => "?",
                    };
                    println!(
                        "  run {} worktree {} any_run_pin {}",
                        run["run"]["id"].as_str().unwrap_or("?"),
                        worktree,
                        pin,
                    );
                }
                for entry in result["cleanup"].as_array().unwrap_or(&Vec::new()) {
                    let runs = entry["runs"]
                        .as_array()
                        .map(|runs| {
                            runs.iter()
                                .filter_map(|run| run.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    let pins_removed = entry["runtime_pins_removed"]
                        .as_array()
                        .map(|runs| {
                            runs.iter()
                                .filter_map(|run| run.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    // `at` is a `wirk_core::Timestamp` (Unix ms, a bare
                    // `i64`), so reading it as a string misses on every
                    // real entry and reports the recorded cleanup time
                    // as unknown regardless of when cleanup ran.
                    // Rendered as an elapsed duration rather than a
                    // calendar time: that is what this line is read for,
                    // and it needs no calendar arithmetic.
                    let at = match entry["at"].as_i64() {
                        Some(ms) => format_elapsed_ms(ms),
                        None => "?".to_string(),
                    };
                    println!(
                        "  clean at {at} runs [{}] worktree_removed {} runtime_pins_removed [{}] \
                         complete {}",
                        runs,
                        entry["worktree_removed"].as_bool().unwrap_or(false),
                        pins_removed,
                        entry["complete"].as_bool().unwrap_or(false),
                    );
                }
                for entry in result["evidence"].as_array().unwrap_or(&Vec::new()) {
                    for artifact in entry["artifacts"].as_array().unwrap_or(&Vec::new()) {
                        let available = artifact["available"].as_bool().unwrap_or(false);
                        let availability = if available {
                            "available".to_string()
                        } else {
                            format!(
                                "unavailable ({})",
                                artifact["reason"].as_str().unwrap_or("unknown")
                            )
                        };
                        // Ruling 0145: which store the bytes were read
                        // back from is part of the evidence line, not an
                        // inference from the path.
                        println!(
                            "  evidence {} {} [{}] sha256:{} {} claim {} run {}",
                            entry["waypoint"].as_str().unwrap_or("?"),
                            artifact["name"].as_str().unwrap_or("?"),
                            artifact["store"].as_str().unwrap_or("worktree"),
                            artifact["digest"].as_str().unwrap_or("?"),
                            availability,
                            entry["claim"].as_str().unwrap_or("?"),
                            entry["run"].as_str().unwrap_or("-"),
                        );
                        // A validated result's retrieval command, printed
                        // beside the evidence line that says its bytes
                        // still check out — named in **this reply's own
                        // scope**, ruling 0339: the scope that generated
                        // the reply is the scope the guidance hands back,
                        // never a different identity borrowed from the
                        // Claim's producer.
                        //
                        // A bound actor already carries a triple whose
                        // Work matches this one (`own_work`): its
                        // own current-Run stays the caller `wirk artifact
                        // read` checks, unchanged, so the guidance names
                        // no scope at all rather than instructing it to
                        // overwrite `WIRK_RUN_ID` with the Claim's
                        // producing Run — which can since have been
                        // superseded on its own Waypoint (0339) while the
                        // Claim itself remains validated and readable by
                        // *this* actor.
                        //
                        // *Matches this one* is the whole of it: the
                        // bare form's door is `handle_run_artifact`
                        // against the triple in the reader's own
                        // environment, so it reads the actor's own Work
                        // and no other. A bound actor asking about a
                        // related Work it covers (`--work <other>`,
                        // answered scoped as itself) is printing
                        // guidance for *that* Work's evidence, and the
                        // bare form would silently read somewhere else —
                        // the same "never a different identity borrowed"
                        // rule above, applied to the target rather than
                        // to the producer. Such a reader takes the named
                        // form below, exactly as any other non-own-Work
                        // reader does. Every other reader — the
                        // administrative surface, or a named
                        // `--requesting-work` — has no Run to preserve,
                        // and reads by naming the Work directly (`wirk
                        // artifact read --estate/--work`, ruling 0339),
                        // needing no producing Run at all. Offered only
                        // for an artifact this same line just reported
                        // available; every substituted word is
                        // shell-quoted so the line can be copied and run
                        // as printed.
                        if available {
                            let claim = entry["claim"].as_str().unwrap_or("?");
                            let name = artifact["name"].as_str().unwrap_or("?");
                            if own_work.as_deref() == Some(work_id.as_str()) {
                                println!(
                                    "    read with: wirk artifact read --claim {} --name {}",
                                    shell_quote(claim),
                                    shell_quote(name),
                                );
                            } else {
                                match &requesting {
                                    Some(requester) => println!(
                                        "    read with: wirk artifact read --estate {} --work \
                                         {} --requesting-work {} --claim {} --name {}",
                                        shell_quote(estate),
                                        shell_quote(&work_id),
                                        shell_quote(&requester.0),
                                        shell_quote(claim),
                                        shell_quote(name),
                                    ),
                                    None => println!(
                                        "    read with: wirk artifact read --estate {} --work \
                                         {} --admin --claim {} --name {}",
                                        shell_quote(estate),
                                        shell_quote(&work_id),
                                        shell_quote(claim),
                                        shell_quote(name),
                                    ),
                                }
                            }
                        }
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
pub(crate) fn list_work_ids(estate: &Path) -> Result<Vec<String>, String> {
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
/// How a daemon refusal reaches the operator, in **one** place.
///
/// Ruling 0251 F2: a refusal now carries the admission disclosure that
/// used to reach `wirkd`'s own stderr and nowhere else — the pool's
/// agreed number against this estate's own, and the deliberate way to
/// change it. There were two independent renderers for `Reply::Err`
/// (this one and `atlas::call_expecting_outcome`'s), and a fix applied
/// to one of them left every `atlas` verb — the verbs that actually
/// get refused for capacity — printing nothing. One renderer, so that
/// cannot happen again (R2).
pub(crate) fn render_refusal(error: &wirkd::ErrorDetail) {
    eprintln!("wirk wirkd: {} {}", error.code, error.message);
    if let Some(detail) = &error.detail {
        for line in detail.lines() {
            eprintln!("wirk wirkd: {line}");
        }
    }
}

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
            render_refusal(&error);
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
        // The estate walk under the name a person looks for it by.
        // `wirk wirkd status --estate <root>` with no `--work` has
        // always been the listing; nothing about that verb's name says
        // so, and a reader wanting "what Work is there" had to know to
        // ask the daemon's own status verb without an argument. This is
        // that same call — same scope resolution, same refusals, same
        // exit codes, same `--json` array — reached by the name the
        // question has. It adds no request and no field: `--work <id>`
        // is the one flag it does not take, because naming one Work is
        // what `wirk work status` is for.
        Some("list") => {
            let Some(estate) = flag_value(&rest[1..], "--estate") else {
                return work_usage();
            };
            // Refused rather than ignored: a caller who named one Work
            // asked a different question than this verb answers, and
            // silently handing back the whole estate instead would be
            // the widest possible answer to the narrowest request.
            if rest[1..].iter().any(|arg| arg == "--work") {
                eprintln!(
                    "wirk work list: --work names a single Work; that is wirk work status --work \
                     <id>. This verb lists the estate and takes no --work"
                );
                return ExitCode::from(1);
            }
            wirkd_status_command(
                &estate,
                None,
                flag_value(&rest[1..], "--requesting-work"),
                rest[1..].iter().any(|arg| arg == "--admin"),
                rest[1..].iter().any(|arg| arg == "--json"),
            )
        }
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
        // P4.5 first increment (ruling 0203): `wirk work clean --estate
        // <root> --work <id> [--dry-run] [--json]`.
        Some("clean") => work_clean_command(&rest[1..]),
        _ => work_usage(),
    }
}

/// `wirk work clean --estate <root> --work <id> [--dry-run] [--json]`
/// (P4.5 first increment, ruling 0203): one terminal Work's own
/// checkout and per-Run runtime residue. Refuses outright (nothing
/// touched) for a non-terminal Work, a live actor or pane, checkout-
/// backed validated Claim evidence, unresolvable path identity, or
/// ignored content in the checkout. `--dry-run` runs every one of those
/// checks with no mutation at all, reporting the same structured result
/// a real call would have acted on.
fn work_clean_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return work_usage();
    };
    let Some(work_id) = flag_value(rest, "--work") else {
        return work_usage();
    };
    let dry_run = rest.iter().any(|arg| arg == "--dry-run");
    let json = rest.iter().any(|arg| arg == "--json");
    let outputs_staging = rest.iter().any(|arg| arg == "--outputs-staging");

    wirkd_client_call(
        &estate,
        &Request::clean(wirkd::CleanPayload {
            work_id: WorkId(work_id.clone()),
            dry_run,
            outputs_staging,
        }),
        |result| {
            if json {
                println!("{result}");
                return;
            }
            let runs = result["runs"]
                .as_array()
                .map(|runs| {
                    runs.iter()
                        .filter_map(|run| run.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let worktree_removed = result["worktree_removed"].as_bool().unwrap_or(false);
            let runtime_pins_removed = result["runtime_pins_removed"]
                .as_array()
                .map(|runs| {
                    runs.iter()
                        .filter_map(|run| run.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let outputs_staging_removed = result["outputs_staging_removed"]
                .as_array()
                .map(|runs| {
                    runs.iter()
                        .filter_map(|run| run.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let verb = if dry_run { "would clean" } else { "cleaned" };
            println!(
                "{verb} {work_id} (runs: {runs}): worktree_removed={worktree_removed} \
                 runtime_pins_removed=[{runtime_pins_removed}] \
                 outputs_staging_removed=[{outputs_staging_removed}]"
            );
            if !outputs_staging {
                println!(
                    "this Work's output staging was left in place; --outputs-staging removes it \
                     too. A validated Claim's own artifacts are a separate write-once copy under \
                     outputs/claims/ and are never touched either way"
                );
            }
        },
    )
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
                    if let Some(reason) = finding["ready"]["reason"].as_str() {
                        println!("      reason: {reason}");
                    }
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
        "usage: wirk work submit --estate <root> --repo <name>:<read|write> [--repo <name>:<read|write> ...] [--execution-repo <name>] --base <ref> (--route <name> [--kind actor --repo-path <path> | --kind actor --source-basis output-only] | --kind deterministic [--source-basis git|output-only] [--repo-path <checkout>] --command <argv...>) [--parent-work <id> --parent-waypoint <id> --parent-run <id> --role <role> [--parent-attempt <n>]] | wirk work list --estate <root> [--requesting-work <id>] [--admin] [--json] | wirk work status --estate <root> --work <id> [--requesting-work <id>] [--admin] [--json] | wirk work retry --estate <root> --work <id> [--run <run-id>] | wirk work fail --estate <root> --work <id> --reason <text> | wirk work cancel --estate <root> --work <id> [--cascade] [--reason <text>] | wirk work obligations --estate <root> --work <id> (--requesting-work <id> | --admin) [--waypoint <id>] [--json] | wirk work clean --estate <root> --work <id> [--dry-run] [--outputs-staging] [--json]"
    );
    ExitCode::from(1)
}

/// Reject an unknown flag, an unexpected positional, and a flag whose
/// value is missing — before the daemon is located and before anything
/// is read.
///
/// `command` is the whole command as a reader types it (`atlas cancel`,
/// `estate clean`), so one implementation serves every subcommand
/// instead of each carrying its own copy of this loop.
pub(crate) fn check_flags(
    command: &str,
    rest: &[String],
    allowed: &[(&str, bool)],
) -> Result<(), ExitCode> {
    let mut index = 0;
    while index < rest.len() {
        let arg = &rest[index];
        let Some((_, takes_value)) = allowed.iter().find(|(name, _)| name == arg) else {
            if arg.starts_with('-') {
                eprintln!(
                    "wirk {command}: unknown flag {arg}\n\
                     accepted flags: {}",
                    allowed
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            } else {
                eprintln!("wirk {command}: unexpected argument {arg}");
            }
            return Err(ExitCode::from(2));
        };
        index += 1;
        if *takes_value {
            if index >= rest.len() {
                eprintln!("wirk {command}: {arg} requires a value");
                return Err(ExitCode::from(2));
            }
            index += 1;
        }
    }
    Ok(())
}

/// Every value given for a repeatable `flag`, in command-line order.
pub(crate) fn flag_values(args: &[String], flag: &str) -> Vec<String> {
    args.iter()
        .zip(args.iter().skip(1))
        .filter(|(name, _)| name.as_str() == flag)
        .map(|(_, value)| value.clone())
        .collect()
}

/// Returns the value following `flag` in `args`, or `None` if the flag
/// is absent or has no following value (R6: the one shared parsing move
/// every subcommand's `--estate`/`--intent`/`--base` needs).
pub(crate) fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

// ---- plugin configuration (the Herdr plugin's own setup surface) -----

/// The files a Wirk plugin installation is configured by, all inside
/// the per-plugin directory Herdr creates and names in
/// `HERDR_PLUGIN_CONFIG_DIR`: `estate` (which estate this installation
/// works in) and `harness` (which interactive agent kind the assistant
/// action starts) hold one line each, and `harness-args` holds one
/// argument per line. Every manifest entry and every script under
/// `plugin/` reads these three names and no others.
const ESTATE_FILE: &str = "estate";
const HARNESS_FILE: &str = "harness";
const HARNESS_ARGS_FILE: &str = "harness-args";

/// Dispatches `wirk plugin <rest>`:
///
/// * `init [--estate <root>] [--harness <kind>] [--harness-arg <arg>]...
///   [--clear-harness-args]` writes what it is given, one value per line
///   per file. At least one is required — an `init` that was asked to
///   write nothing is a mistake, not a no-op.
/// * `show` prints where the configuration lives and what is in it.
/// * `harnesses` prints the agent kinds Herdr itself reports.
///
/// All three need `HERDR_PLUGIN_CONFIG_DIR`, which Herdr sets for
/// everything it launches on a plugin's behalf; outside that there is
/// no per-plugin configuration directory to read or write.
fn plugin_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("init") => plugin_init(&rest[1..]),
        Some("show") => plugin_show(),
        Some("harnesses") => plugin_harnesses(&rest[1..]),
        _ => plugin_usage(),
    }
}

/// `HERDR_PLUGIN_CONFIG_DIR`, or an explanation naming the one thing
/// that supplies it.
fn plugin_config_dir() -> Result<PathBuf, ExitCode> {
    match env::var("HERDR_PLUGIN_CONFIG_DIR") {
        Ok(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => {
            eprintln!(
                "wirk plugin: HERDR_PLUGIN_CONFIG_DIR is not set. Herdr sets it for every\n\
                 command it runs on a plugin's behalf, so run this from a Herdr plugin\n\
                 action or pane (the plugin's 'Configure Wirk' action does exactly that).\n\
                 'herdr plugin config-dir wirk' prints the directory it would be."
            );
            Err(ExitCode::from(2))
        }
    }
}

/// Reads one configuration file's single line, `None` when the file is
/// absent or holds only whitespace.
fn plugin_config_value(config_dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(config_dir.join(name)).ok()?;
    let value = text.lines().next().unwrap_or("").trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Reads a one-value-per-line configuration file, each line taken
/// exactly as written so an argument holding a space or a glob survives
/// unchanged. Blank lines and `#` comments are skipped, so the file can
/// say what it is; an absent file reads as no values at all. This is the
/// same reading `plugin/assistant.sh` does, so both surfaces answer with
/// the same list.
fn plugin_config_lines(config_dir: &Path, name: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(config_dir.join(name)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// Why a `--harness-arg` value cannot survive the one-line-per-argument
/// file format, or `None` when it round-trips unchanged.
///
/// `plugin_config_lines` (the same reading `plugin/assistant.sh` does)
/// skips a blank line and a line starting with `#`, because the file
/// also carries comments; and a trailing `\r` on the line written just
/// before this function's caller appends `\n` forms a `\r\n` terminator
/// that `str::lines` strips on read, so an argument ending in `\r`
/// would come back shorter than it was stored. All three are silent
/// data loss, not a formatting nicety, so `plugin_init` refuses them
/// before writing anything rather than storing bytes that later read
/// back as "none" or as a different argument. A leading `-` is not
/// checked here: `flag_values` already takes the token after
/// `--harness-arg` verbatim regardless of what it starts with, and
/// storage and read-back treat it like any other character.
fn harness_arg_unrepresentable_reason(arg: &str) -> Option<&'static str> {
    if arg.is_empty() {
        Some("would be stored as a blank line, which reads back as no argument at all")
    } else if arg.starts_with('#') {
        Some(
            "would be stored as a line starting with '#', which reads back as a comment, not an argument",
        )
    } else if arg.ends_with('\r') {
        Some(
            "ends with a carriage return, which merges with the stored line's own newline and is stripped on read",
        )
    } else {
        None
    }
}

/// `init`: writes the estate root, the harness kind, the harness's own
/// arguments, or any combination of them.
///
/// The estate must already be a directory. A path that does not exist
/// is refused here rather than written and discovered later by the
/// startup hook, which would report it as "no wirkd" instead of "that
/// is not a directory". The harness kind is checked for shape only —
/// one bare token — because which kinds exist is Herdr's to say, and
/// `wirk plugin harnesses` asks it.
///
/// `--harness-arg` is repeatable and each value becomes one line of
/// `harness-args`, passed through to the harness by the assistant
/// action exactly as written: spaces, globs and leading dashes all
/// survive, because nothing re-splits or expands a line. `wirk` never
/// supplies one of its own — which model or effort a conversation runs
/// at is the operator's choice, and an installation that named none
/// starts the harness with no extra arguments at all. An argument the
/// one-line format cannot represent — empty, a `#` comment line, or
/// one ending in `\r` — is refused outright by
/// `harness_arg_unrepresentable_reason` rather than silently dropped;
/// there is no escaping syntax or migration to a richer format here.
///
/// Repeating the flag replaces the whole list rather than appending to
/// it: the file is what the operator last said in full, so a command
/// naming three arguments and a later one naming two leaves two.
/// `--clear-harness-args` is how "none" is said, and is distinct from
/// not mentioning arguments at all, which leaves them as they were.
///
/// All of `--estate`, `--harness` and `--harness-arg`/
/// `--clear-harness-args` are validated before any of the three
/// configuration files is touched. Each file is an independent piece
/// of state a later `show` or `assistant.sh` run reads on its own, so
/// a command naming a valid `--estate` alongside an invalid `--harness`
/// must fail as a whole, not write the estate and leave the harness
/// error for the operator to notice separately.
fn plugin_init(rest: &[String]) -> ExitCode {
    let estate = flag_value(rest, "--estate");
    let harness = flag_value(rest, "--harness");
    let harness_args = flag_values(rest, "--harness-arg");
    let clear_harness_args = rest.iter().any(|a| a == "--clear-harness-args");
    if estate.is_none() && harness.is_none() && harness_args.is_empty() && !clear_harness_args {
        return plugin_usage();
    }
    if clear_harness_args && !harness_args.is_empty() {
        eprintln!(
            "wirk plugin init: --clear-harness-args and --harness-arg contradict each other.\n\
             Give the arguments to set, or --clear-harness-args to set none."
        );
        return ExitCode::from(2);
    }
    // One argument per line is the whole format, so an argument that
    // itself contains a newline cannot be stored and read back as the
    // same argument. Refused at the point it is given rather than
    // silently written as two.
    if let Some(bad) = harness_args.iter().find(|arg| arg.contains('\n')) {
        eprintln!(
            "wirk plugin init: --harness-arg must not contain a newline; {bad:?} does.\n\
             Arguments are stored one per line."
        );
        return ExitCode::from(2);
    }
    if let Some((bad, reason)) = harness_args
        .iter()
        .find_map(|arg| harness_arg_unrepresentable_reason(arg).map(|reason| (arg, reason)))
    {
        eprintln!(
            "wirk plugin init: --harness-arg {bad:?} cannot be stored: {reason}.\n\
             Nothing was written. The other door into the harness's arguments\n\
             is WIRK_ASSISTANT_HARNESS_ARGS (read by plugin/assistant.sh):\n\
             it is split on whitespace by read -a, so it cannot carry an\n\
             empty argument at all, but it does carry the values this file\n\
             cannot — an argument starting with '#' or one ending in a\n\
             carriage return."
        );
        return ExitCode::from(2);
    }
    // The estate directory and the harness shape are also checked
    // before any file is written, for the same reason: a rejected
    // --harness must not leave a --estate given in the same command
    // already on disk.
    if let Some(estate) = estate.as_deref()
        && !Path::new(estate).is_dir()
    {
        eprintln!(
            "wirk plugin init: --estate {estate} is not an existing directory.\n\
             An estate is a directory wirk works in; create it first, or name one\n\
             that exists."
        );
        return ExitCode::from(2);
    }
    if let Some(harness) = harness.as_deref()
        && (harness.is_empty() || harness.split_whitespace().count() != 1)
    {
        eprintln!(
            "wirk plugin init: --harness must be one agent kind, with no spaces.\n\
             'wirk plugin harnesses' lists the kinds this Herdr reports."
        );
        return ExitCode::from(2);
    }

    let config_dir = match plugin_config_dir() {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    if let Err(err) = std::fs::create_dir_all(&config_dir) {
        eprintln!("wirk plugin init: {}: {err}", config_dir.display());
        return ExitCode::from(2);
    }

    if let Some(estate) = estate.as_deref() {
        // Stored as given rather than canonicalized: an operator who
        // named a symlinked path meant that path, and every other
        // surface prints this value back to them.
        if let Err(err) = std::fs::write(config_dir.join(ESTATE_FILE), format!("{estate}\n")) {
            eprintln!("wirk plugin init: writing {ESTATE_FILE}: {err}");
            return ExitCode::from(2);
        }
        println!("estate  {estate}");
    }

    if let Some(harness) = harness.as_deref() {
        if let Err(err) = std::fs::write(config_dir.join(HARNESS_FILE), format!("{harness}\n")) {
            eprintln!("wirk plugin init: writing {HARNESS_FILE}: {err}");
            return ExitCode::from(2);
        }
        println!("harness {harness}");
    }

    if clear_harness_args {
        match std::fs::remove_file(config_dir.join(HARNESS_ARGS_FILE)) {
            Ok(()) => {}
            // Nothing set is already the state asked for.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                eprintln!("wirk plugin init: removing {HARNESS_ARGS_FILE}: {err}");
                return ExitCode::from(2);
            }
        }
        println!("harness-args (none)");
    } else if !harness_args.is_empty() {
        let body: String = harness_args.iter().map(|arg| format!("{arg}\n")).collect();
        if let Err(err) = std::fs::write(config_dir.join(HARNESS_ARGS_FILE), body) {
            eprintln!("wirk plugin init: writing {HARNESS_ARGS_FILE}: {err}");
            return ExitCode::from(2);
        }
        for arg in &harness_args {
            println!("harness-arg {arg}");
        }
    }

    println!("written to {}", config_dir.display());
    ExitCode::SUCCESS
}

/// `show`: the configuration as it stands, with each unset value named
/// as unset and the command that sets it. Exits 0 whether or not
/// anything is configured — nothing failed; this is a read.
fn plugin_show() -> ExitCode {
    let config_dir = match plugin_config_dir() {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    println!("config dir {}", config_dir.display());
    match plugin_config_value(&config_dir, ESTATE_FILE) {
        Some(estate) => {
            let exists = if Path::new(&estate).is_dir() {
                ""
            } else {
                "   (no such directory)"
            };
            println!("estate     {estate}{exists}");
        }
        None => println!("estate     (not set)   wirk plugin init --estate <root>"),
    }
    match plugin_config_value(&config_dir, HARNESS_FILE) {
        Some(harness) => println!("harness    {harness}"),
        None => println!("harness    (not set)   wirk plugin init --harness <kind>"),
    }
    // One per line, each on its own row, because that is how they are
    // stored and how they are passed: an argument holding a space is a
    // single argument, and joining them for display would read as two.
    let harness_args = plugin_config_lines(&config_dir, HARNESS_ARGS_FILE);
    if harness_args.is_empty() {
        println!("harness-args (none)    wirk plugin init --harness-arg <arg>");
    } else {
        for (index, arg) in harness_args.iter().enumerate() {
            let label = if index == 0 {
                "harness-args"
            } else {
                "            "
            };
            println!("{label} {arg}");
        }
    }
    ExitCode::SUCCESS
}

/// `harnesses [--socket <path>] [--json]`: the interactive agent kinds
/// Herdr reports, read from Herdr's own `server.agent_manifests`.
///
/// This is Herdr's structured answer, not a scrape of its help text, so
/// a kind Herdr gains or drops is reflected without an edit here. The
/// socket is `--socket`, else `HERDR_SOCKET_PATH`; `herdr status server`
/// prints the path of the session it would talk to, which is how the
/// plugin's own scripts supply it.
///
/// Two things it deliberately does not claim, both said in the footer
/// it prints. Herdr reports the kinds it carries detection manifests
/// for, which can be fewer than `agent start --kind` accepts, so this
/// offers choices rather than settling them and nothing downstream
/// treats it as a veto. And the `PATH` column looks for an executable
/// named after the kind: Herdr chooses that executable and for a few
/// kinds it is not the kind's own name, so a blank is a hint that
/// something is missing, never a verdict that Herdr could not start
/// it.
fn plugin_harnesses(rest: &[String]) -> ExitCode {
    let json = rest.iter().any(|a| a == "--json");
    let socket = flag_value(rest, "--socket")
        .or_else(|| env::var("HERDR_SOCKET_PATH").ok().filter(|s| !s.is_empty()));
    let Some(socket) = socket else {
        eprintln!(
            "wirk plugin harnesses: no Herdr socket given. Pass --socket <path>, or set\n\
             HERDR_SOCKET_PATH. 'herdr status server' prints the socket of the session\n\
             it is talking to."
        );
        return ExitCode::from(2);
    };

    let client = match wirk_herdr::SocketClient::connect(PathBuf::from(&socket)) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("wirk plugin harnesses: cannot reach Herdr at {socket}: {err}");
            return ExitCode::from(2);
        }
    };
    let kinds = match client.agent_manifests() {
        Ok(kinds) => kinds,
        Err(err) => {
            eprintln!("wirk plugin harnesses: Herdr did not report its agent kinds: {err}");
            return ExitCode::from(2);
        }
    };

    let rows: Vec<(String, Option<PathBuf>)> = kinds
        .into_iter()
        .map(|kind| {
            let found = executable_on_path(&kind);
            (kind, found)
        })
        .collect();

    if json {
        let payload: Vec<serde_json::Value> = rows
            .iter()
            .map(|(kind, found)| {
                serde_json::json!({
                    "kind": kind,
                    "executable_named_kind_on_path": found
                        .as_ref()
                        .map(|p| p.display().to_string()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "source": "herdr server.agent_manifests",
                "socket": socket,
                "harnesses": payload,
            })
        );
        return ExitCode::SUCCESS;
    }

    for (kind, found) in &rows {
        match found {
            Some(path) => println!("{kind:<12} {}", path.display()),
            None => println!("{kind:<12} no '{kind}' on PATH"),
        }
    }
    println!();
    println!("Kinds come from Herdr itself (server.agent_manifests) at {socket}.");
    println!(
        "That is what Herdr carries a detection manifest for, which can be fewer than\n\
         its 'agent start --kind' accepts, so this list offers choices rather than\n\
         settling them. The PATH column looks for an executable named after the kind;\n\
         Herdr picks the executable for each kind and for a few it differs from the\n\
         kind name, so a blank there means 'probably not installed', not 'Herdr cannot\n\
         start it'."
    );
    ExitCode::SUCCESS
}

/// The first executable named `name` on `PATH` (R3: `PATH` splitting is
/// all this needs; nothing here wants a `which` dependency).
fn executable_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn plugin_usage() -> ExitCode {
    eprintln!(
        "usage: wirk plugin init [--estate <root>] [--harness <kind>]\n\
         \x20                       [--harness-arg <arg>]... | [--clear-harness-args]\n\
         \x20      wirk plugin show\n\
         \x20      wirk plugin harnesses [--socket <path>] [--json]"
    );
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

    // A relative `--estate` stays correct for every call this function
    // itself makes (its own process's cwd never moves), but both
    // executors spawn their real process into a *different* directory
    // (`det.cwd`, the Work's own worktree) and then inject
    // `WIRK_ESTATE_ROOT` from this `PathBuf`'s own display string
    // (`ChildExecutor::launch`, `DockerExecutor`'s env/argv): an
    // unresolved relative value is then read back relative to that
    // other cwd, not this one, and a real child spawned that way filed
    // its Claim against the wrong path and was refused
    // (`TripleMismatch`, reproduced live with a relative `--estate` and
    // an output-only World whose worktree differs from the launch
    // directory). Canonicalizing once, here, before either executor is
    // constructed, fixes the value both then inject and the
    // `owned_execution_address` equality check just below, which
    // compares this same `estate_root` against wirkd's own (always
    // absolute) reservation.
    let estate_root = match std::fs::canonicalize(&estate) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("wirk run-deterministic: --estate {estate} could not be resolved: {err}");
            return ExitCode::from(2);
        }
    };
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

    // Ruling 0292: an output-only Deterministic World executes in this
    // Work's *own* owned directory, and this is where that directory is
    // established — creation is where ownership is proven and
    // registered, the same step `wirk run` performs for an output-only
    // Actor (`executor.rs`), through the same
    // `wirk_core::materialize_owned_directory`.
    //
    // Nothing is spawned until it holds. The address the reservation
    // carries is checked against the address this estate would
    // materialize for this Work, so a World reserved anywhere else — a
    // historical one submitted before this correction, or a hand-built
    // one — is refused here rather than run in a directory that is not
    // this Work's. A Git-basis Deterministic World is untouched: its
    // `cwd` is a real git worktree, established and registered by git
    // itself at submit.
    if let World::Deterministic(det) = &world
        && matches!(det.source_basis, wirk_core::SourceBasis::OutputOnly { .. })
    {
        let owned = wirk_core::owned_execution_address(&estate_root, &work_id);
        if !wirk_core::paths_equal(&owned, &det.cwd) {
            eprintln!(
                "wirk run-deterministic: this Run's output-only World names {} as its execution \
                 directory, which is not this estate's own address for this Work ({}); \
                 nothing was created or executed",
                det.cwd.display(),
                owned.display()
            );
            return ExitCode::from(2);
        }
        let registered = registered_owned_identity(&status, &run.id);
        match wirk_core::materialize_owned_directory(&owned, &work_id, &run.id, registered) {
            Ok(wirk_core::OwnedMaterialization::Created) => {
                // Durable before it is relied on: the identity of the
                // directory object this materialization actually
                // created, journaled outside the directory so a later
                // attempt (a retry, a reattachment, `wirk work clean`)
                // can tell it from whatever is standing at the address
                // then (ruling 0283). A creation this estate cannot
                // register is refused rather than executed in.
                let identity = wirk_core::directory_identity(&owned);
                if let Err(detail) = record_owned_creation(
                    &estate,
                    &work_id,
                    &run.id,
                    det.base_sha.clone(),
                    identity,
                ) {
                    eprintln!(
                        "wirk run-deterministic: could not register this Run's creation of {}: \
                         {detail}",
                        owned.display()
                    );
                    return ExitCode::from(2);
                }
                println!("owned execution directory {}", owned.display());
            }
            Ok(wirk_core::OwnedMaterialization::ReattachedByIdentity(identity)) => {
                println!(
                    "owned execution directory {} is the directory this Work created \
                     (registered identity {}:{}) and is reattached, not re-created",
                    owned.display(),
                    identity.dev,
                    identity.ino
                );
            }
            Err(err) => {
                eprintln!(
                    "wirk run-deterministic: {} is not this Run's owned execution directory: \
                     {err}",
                    owned.display()
                );
                return ExitCode::from(2);
            }
        }
    }

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
        contract_delivery: None,
        claim_hook: None,
        // Attempt admission is the Actor launch path's own
        // (`RunLaunchAttempted`); a Deterministic Run never takes one.
        launch_attempt: None,
    };
    Ok((run, world))
}

/// The creation identity this Work already registered for its owned
/// execution directory, read out of the same `status` reply this
/// command already holds — `runs[].owned_registration.identity`, the
/// Work-scoped registration `executor.rs` reads for the Actor path
/// (ruling 0283). `None` when nothing was ever registered, which is the
/// case a first materialization is.
fn registered_owned_identity(
    status: &serde_json::Value,
    run_id: &RunId,
) -> Option<wirk_core::DirectoryIdentity> {
    let entry = status["runs"]
        .as_array()?
        .iter()
        .find(|entry| entry["run"]["id"].as_str() == Some(run_id.0.as_str()))?;
    serde_json::from_value(entry["owned_registration"]["identity"].clone()).ok()
}

/// Journals this Run's creation of its owned execution directory. The
/// same `WorktreeCreated` record the Actor path writes, with the fields
/// an output-only World carries: no repository, this World's own
/// `base_sha`, and the identity of the directory object just created.
fn record_owned_creation(
    estate: &str,
    work_id: &WorkId,
    run_id: &RunId,
    base_sha: String,
    identity: Option<wirk_core::DirectoryIdentity>,
) -> Result<(), String> {
    let pointer = wirkd::client::locate(Path::new(estate)).map_err(|err| err.to_string())?;
    match wirkd::client::call(
        &pointer.socket,
        &Request::record(wirkd::RecordPayload {
            work_id: work_id.clone(),
            run: Some(run_id.clone()),
            kind: EventKind::WorktreeCreated {
                repo: String::new(),
                base_sha,
                identity,
            },
        }),
    ) {
        Ok(Reply::Ok { .. }) => Ok(()),
        Ok(Reply::Err { error, .. }) => Err(format!("{}: {}", error.code, error.message)),
        Err(err) => Err(err.to_string()),
    }
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
                contract: None,
                claim_hook: None,
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
                // `journal demo`'s synthetic shape: nobody filed this,
                // so there is no origin to state (ruling 0257).
                origin: None,
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
        EventKind::WorkCleaned { .. } => "WorkCleaned",
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

/// Deterministic checks on the pure rendering helpers behind the
/// status lines: `format_elapsed_from`'s bucketing and its zero floor,
/// and the shell quoting the printed retrieval command depends on.
/// Neither stands in for a service — they are string functions, and the
/// live service behaviour they feed is exercised against a real daemon
/// in `tests/work_listing.rs`.
#[cfg(test)]
mod status_rendering_tests {
    use super::{format_elapsed_from, listing_summary, shell_quote};

    #[test]
    fn format_elapsed_from_buckets_by_unit() {
        let now = 1_700_000_000_000i64;
        assert_eq!(format_elapsed_from(now, now), "0s ago");
        assert_eq!(format_elapsed_from(now, now - 45_000), "45s ago");
        assert_eq!(format_elapsed_from(now, now - 5 * 60_000), "5m ago");
        assert_eq!(format_elapsed_from(now, now - 3 * 3_600_000), "3h ago");
        assert_eq!(format_elapsed_from(now, now - 2 * 86_400_000), "2d ago");
    }

    #[test]
    fn format_elapsed_from_never_prints_a_negative_duration() {
        let now = 1_700_000_000_000i64;
        // A timestamp after `now` (clock skew, or read mid-write) must
        // floor at zero, never print a negative "-3s ago".
        assert_eq!(format_elapsed_from(now, now + 5_000), "0s ago");
    }

    /// The retrieval command is printed to be copied into a shell, so
    /// nothing substituted into it may survive as shell syntax.
    #[test]
    fn shell_quote_neutralizes_shell_syntax() {
        assert_eq!(shell_quote("work-18d5-0"), "'work-18d5-0'");
        assert_eq!(shell_quote("a b; rm -rf /"), "'a b; rm -rf /'");
        assert_eq!(shell_quote("$HOME`id`"), "'$HOME`id`'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    /// A narrowed reader's `artifacts` arrive as a withheld marker
    /// rather than a list. That is not the same fact as "this Work has
    /// no validated outputs", and the row must not report it as one.
    #[test]
    fn listing_summary_separates_withheld_outputs_from_absent_ones() {
        let none = serde_json::json!({"evidence": []});
        assert!(listing_summary(&none).ends_with("outputs none"));

        let withheld = serde_json::json!({
            "evidence": [{"claim": "claim-1", "artifacts": {"withheld": true}}],
        });
        assert!(listing_summary(&withheld).ends_with("outputs withheld"));

        let counted = serde_json::json!({
            "attempt": 2,
            "run_id": "run-1",
            "run_state": "claimed",
            "evidence": [{"claim": "claim-1", "artifacts": [
                {"name": "a.md", "available": true},
                {"name": "b.md", "available": false},
            ]}],
        });
        assert_eq!(
            listing_summary(&counted),
            "attempt 2 run run-1 claimed outputs 1/2 available"
        );
    }
}
