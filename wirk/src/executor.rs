//! `wirk run`: drives one Actor Waypoint's Run end to end against a
//! live Herdr session and wirkd (item 4, W3; `knowledge/work/
//! p1-herdr-executor/orient/build-brief.md`, `orient/loop.md`).
//!
//! Sequence, per the "Outcome" the build brief names: locate wirkd via
//! `WIRK_ESTATE_ROOT`'s pointer file convention (already `wirkd::
//! client::locate`, R2); read the Work's status and, from it, the
//! reserved World for the one open Run (wirkd's `status` verb, widened
//! this wave — `wirkd::server::handle_status`); build a
//! `wirk_herdr::SocketClient` against the named Herdr session's socket
//! (`~/.config/herdr/sessions/<session>/herdr.sock`, cited below) or an
//! explicit `--herdr-socket` override (this wave's own test, and the
//! tried step's escape hatch); create the worktree with `wirk_herdr::
//! git::worktree_add` from the World's `repository`/`branch`/
//! `base_sha`, journal `WorktreeCreated` with the SHA `worktree_add`
//! read back, and update the World's `worktree_path` — both through
//! wirkd's `record` verb, never by opening the journal file directly
//! (item 3's single-write-path discipline); then drive `wirk_herdr::
//! run_loop::RunLoop` with a `WirkdApi` built over the same wirkd
//! client, printing one status line per terminal transition and exiting
//! 0 (Claimed), 4 (NeedsInput), or 5 (Vanished, an unresolved stream,
//! or any error).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;

use wirk_core::{EventKind, Run, RunId, RunState, WorkId, WorkState, World, WorldHash};
use wirk_herdr::SocketClient;
use wirk_herdr::run_loop::{Outcome, RunLoop, RunStatusEntry, WirkdApi, WorkStatus};

use crate::wirkd::{self, RecordPayload, Reply, Request, StatusPayload, WatchPayload};

/// `~/.config/herdr/sessions/<session>/herdr.sock` — the named-session
/// socket convention (`knowledge/evidence/work/p1-plugin-spike/orient/
/// session.md`: "Poll `[ -S ~/.config/herdr/sessions/wirk-dev/herdr.sock
/// ]`"), reused verbatim (R2). `--herdr-socket` bypasses this entirely
/// — this wave's own ungated test, and the tried step's (W4) escape
/// hatch, point at a socket with no real Herdr session behind it.
fn session_socket_path(session: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".config/herdr/sessions")
            .join(session)
            .join("herdr.sock"),
    )
}

/// Everything `wirk run` itself can fail at, beyond `RunLoop`'s own
/// error (`RunLoopError`, printed inline by `run_command`). No
/// `thiserror` (not on this wave's allow-list; `wirkd::client`'s own
/// doc comment already reasons the same way — R3, stdlib `Display`/
/// `Error` suffice for a handful of variants).
#[derive(Debug)]
enum ExecutorError {
    Client(wirkd::client::ClientError),
    /// A `{"ok":false,...}` reply from wirkd, or a reply this module
    /// could not parse into the shape it expected.
    Wirkd(String),
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutorError::Client(err) => write!(f, "{err}"),
            ExecutorError::Wirkd(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ExecutorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExecutorError::Client(err) => Some(err),
            ExecutorError::Wirkd(_) => None,
        }
    }
}

impl From<wirkd::client::ClientError> for ExecutorError {
    fn from(err: wirkd::client::ClientError) -> Self {
        ExecutorError::Client(err)
    }
}

/// The wire shape `handle_status` now returns (`server.rs`'s widened
/// `"runs"` array, this wave): one entry per `RunOpened`, each carrying
/// the reconstructed `Run` (state included) and its Waypoint's most
/// recently reserved `World`. `#[serde(default)]` on `runs` keeps this
/// struct parseable against a reply from a not-yet-rebuilt wirkd during
/// development; every field wirkd actually sends is present in every
/// reply this wave's own code produces.
#[derive(Debug, Deserialize)]
struct StatusReply {
    state: String,
    #[serde(default)]
    runs: Vec<StatusRunEntry>,
}

#[derive(Debug, Deserialize)]
struct StatusRunEntry {
    run: Run,
    #[serde(default)]
    world: Option<World>,
    /// P3 native launch selection (BUILD-BRIEF.md item 1): the
    /// Route-authored default harness/model/effort/args for this Run's
    /// own Waypoint (`server.rs`'s `handle_status`, reading
    /// `WorkSubmitted.waypoint_defs`) — the middle layer of `wirk run`'s
    /// own precedence (`resolve_launch_selection`'s own doc).
    /// `#[serde(default)]` for the same not-yet-rebuilt-wirkd tolerance
    /// `world` already has.
    #[serde(default)]
    selection: Option<wirk_core::AuthoredSelection>,
}

/// Calls wirkd's `status` verb and parses its reply into `StatusReply`
/// — the one parse both `fetch_open_run` (setup: needs the World too)
/// and `WirkdRunLoopApi::status` (the ongoing poll: needs only Run
/// states) read from.
fn fetch_status(socket: &Path, work_id: &WorkId) -> Result<StatusReply, ExecutorError> {
    let reply = wirkd::client::status(
        socket,
        // W-B launch disclosure integration (the launch review's F-C):
        // `wirk run` drives exactly one Work and reads exactly that
        // Work's own status, so it names itself as the requester and
        // never asks for the administrative surface. A Work's own
        // bindings trivially cover its own, so both this setup read and
        // `WirkdRunLoopApi::status`'s ongoing progress poll get the
        // identical full reply they got before — and an actor that
        // wanted another Work's launch metadata cannot reach it here by
        // omitting a scope.
        // Through the typed `status` door, which refuses an answer
        // that never established the requested scope rather than
        // reading it as though it had (the integration review's V-5):
        // `wirk run` against a daemon predating the gate now fails
        // honestly instead of driving on an unscoped reply.
        StatusPayload::scoped(work_id.clone(), work_id.clone()),
    )?;
    match reply {
        Reply::Ok { result, .. } => serde_json::from_value(result)
            .map_err(|err| ExecutorError::Wirkd(format!("malformed status reply: {err}"))),
        Reply::Err { error, .. } => Err(ExecutorError::Wirkd(format!(
            "status refused: {} {}",
            error.code, error.message
        ))),
    }
}

/// The one `Run` this Work has open, and its reserved World — `wirk
/// run`'s own setup read, once, before `RunLoop` starts polling `status`
/// itself. Errors when there is no open Run (nothing to drive) or the
/// open Run's Waypoint carries no reserved World (a malformed journal
/// this wave's own writers never produce).
fn fetch_open_run(
    socket: &Path,
    work_id: &WorkId,
) -> Result<(Run, World, Option<wirk_core::AuthoredSelection>), ExecutorError> {
    let parsed = fetch_status(socket, work_id)?;
    let entry = parsed
        .runs
        .into_iter()
        .find(|entry| matches!(entry.run.state, RunState::Open))
        .ok_or_else(|| ExecutorError::Wirkd(format!("no open Run for work {}", work_id.0)))?;
    let world = entry.world.ok_or_else(|| {
        ExecutorError::Wirkd("the open Run's Waypoint has no reserved World".to_string())
    })?;
    Ok((entry.run, world, entry.selection))
}

fn parse_work_state(state: &str) -> Result<WorkState, ExecutorError> {
    match state {
        "pending" => Ok(WorkState::Pending),
        "active" => Ok(WorkState::Active),
        "waiting" => Ok(WorkState::Waiting),
        "needs_input" => Ok(WorkState::NeedsInput),
        "blocked" => Ok(WorkState::Blocked),
        "completed" => Ok(WorkState::Completed),
        "failed" => Ok(WorkState::Failed),
        "canceled" => Ok(WorkState::Canceled),
        other => Err(ExecutorError::Wirkd(format!(
            "unknown Work state {other:?} in status reply"
        ))),
    }
}

/// Sends one `RecordPayload` through wirkd's `record` verb (`mod.rs`'s
/// doc comment: the single write path, never the journal file opened
/// directly).
fn wirkd_record(
    socket: &Path,
    work_id: &WorkId,
    run: Option<RunId>,
    kind: EventKind,
) -> Result<(), ExecutorError> {
    let reply = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: work_id.clone(),
            run,
            kind,
        }),
    )?;
    match reply {
        Reply::Ok { .. } => Ok(()),
        Reply::Err { error, .. } => Err(ExecutorError::Wirkd(format!(
            "record refused: {} {}",
            error.code, error.message
        ))),
    }
}

/// `wirk_herdr::run_loop::WirkdApi` over the same wirkd `record`/
/// `status` verbs `run_command` itself uses for setup — `RunLoop`'s own
/// ongoing polling and journal writes (loop.md §1 rows 8, 9, 11, 12).
struct WirkdRunLoopApi {
    socket: PathBuf,
}

impl WirkdApi for WirkdRunLoopApi {
    type Error = ExecutorError;

    fn status(&self, work_id: &WorkId) -> Result<WorkStatus, Self::Error> {
        let parsed = fetch_status(&self.socket, work_id)?;
        let work_state = parse_work_state(&parsed.state)?;
        let runs = parsed
            .runs
            .into_iter()
            .map(|entry| RunStatusEntry {
                run_id: entry.run.id,
                state: entry.run.state,
            })
            .collect();
        Ok(WorkStatus { work_state, runs })
    }

    fn record(&self, work_id: &WorkId, run_id: &RunId, kind: EventKind) -> Result<(), Self::Error> {
        wirkd_record(&self.socket, work_id, Some(run_id.clone()), kind)
    }

    /// Item B: `wirkd::client::watch` over the same socket, mapped into
    /// `WirkdApi::watch`'s `Result<_, ExecutorError>` shape (`ClientError`
    /// already converts via `From`).
    fn watch(
        &self,
        work_id: &WorkId,
    ) -> Result<wirk_herdr::run_loop::WatchEvents<Self::Error>, Self::Error> {
        let events = wirkd::client::watch(
            &self.socket,
            // Same scope discipline as this Work's own status read
            // (F-C): `RunLoop` drives one Work and watches that Work.
            WatchPayload::scoped(work_id.clone(), work_id.clone()),
        )?;
        Ok(Box::new(
            events.map(|item| item.map_err(ExecutorError::from)),
        ))
    }
}

/// `wirk run --estate <root> --work <id> --session <name>
/// [--herdr-socket <path>]` (build-brief.md "Outcome"). Exits 0
/// (Claimed), 4 (NeedsInput), or 5 (Vanished, a subscription stream that
/// ended with nothing terminal, or any setup/drive error) — a status
/// line is printed for each transition this function itself observes.
///
/// P2.3 W5 (build-brief.md §9, second gap): the rerun's driver exited
/// silently, with no journaled outcome and no printed line, cause
/// unobserved (`knowledge/evidence/p2-retry-escalation-2026-09-04/
/// rerun/RESULT-rerun.md`). Audited every `return` below against this
/// function's own body, read whole: every one already `eprintln!`s
/// (setup errors) or `println!`s (`RunLoop::drive`'s own `Outcome`
/// match, all four arms, plus the generic `Err` arm) before returning —
/// none was silent. The one gap an audit of `return`s cannot close is a
/// panic escaping this thread with Rust's own default handler having
/// been silenced or lost (e.g. its message landing in a piped stderr
/// nothing reads before teardown, the rerun's own suspected shape); the
/// hook below names itself so the line is unmistakably this command's,
/// not merely "thread panicked" noise indistinguishable from any other
/// crate's (R3: `std::panic::set_hook`, stdlib, no new dependency).
/// Scoped to this process (a fresh `wirk` invocation per subcommand —
/// `main.rs` never dispatches two), so setting it here never touches
/// any other verb.
pub fn run_command(rest: &[String]) -> ExitCode {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("wirk run: panic: {info}");
    }));
    let cli_kind = parse_actor_kind_override(rest);
    let cli_model = flag_value(rest, "--actor-model");
    let cli_effort = flag_value(rest, "--actor-effort");
    let Some(estate) = flag_value(rest, "--estate") else {
        return run_usage();
    };
    let Some(work_id_arg) = flag_value(rest, "--work") else {
        return run_usage();
    };
    let Some(session) = flag_value(rest, "--session") else {
        return run_usage();
    };
    let herdr_socket = match flag_value(rest, "--herdr-socket") {
        Some(path) => PathBuf::from(path),
        None => match session_socket_path(&session) {
            Some(path) => path,
            None => {
                eprintln!("wirk run: could not resolve $HOME to find the session socket");
                return ExitCode::from(2);
            }
        },
    };

    let work_id = WorkId(work_id_arg);
    let estate_path = PathBuf::from(&estate);

    let pointer = match wirkd::client::locate(Path::new(&estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            eprintln!("wirk run: {err}");
            return ExitCode::from(2);
        }
    };

    let (mut run, world, authored_selection) = match fetch_open_run(&pointer.socket, &work_id) {
        Ok(triple) => triple,
        Err(err) => {
            eprintln!("wirk run: {err}");
            return ExitCode::from(2);
        }
    };
    // P3 native launch selection (BUILD-BRIEF.md item 1, superseding
    // this comment's own prior "`--actor-kind` is this invocation's own
    // choice" — the choice is now layered, not a bare override):
    // `run.kind`/`run.selection` are unhashed (`WorldHash::of` never
    // covers either), so resolving them here is exactly the same kind
    // of local mechanism update `run.rs`'s own `worktree_path`
    // re-emission is. `RunLoop`'s `launch` journals the resolved
    // request via `RunLaunched` below.
    //
    // `run.launched` is `true` only once this Run's own `RunLaunched`
    // has already folded (a prior successful invocation) —
    // PREPARATION-ADJUDICATION.md point 3: "a repeated invocation must
    // not silently alter an already fixed Run launch." Reuse the bound
    // request verbatim then; refuse before touching Herdr at all if
    // this invocation's own explicit flags disagree with it. A first
    // invocation instead resolves fresh (CLI > Route-authored > native
    // default, `resolve_launch_selection`) and `RunLoop::launch` binds
    // it durably the moment `RunLaunched` is journaled.
    if run.launch_requested {
        if let Some(reason) = conflicting_reinvocation(&run, &cli_kind, &cli_model, &cli_effort) {
            eprintln!(
                "wirk run: {reason} — a repeated invocation cannot silently alter an \
                 already fixed launch"
            );
            return ExitCode::from(2);
        }
        println!(
            "launch already fixed: kind={} model={:?} effort={:?} args={:?} (bound at this \
             Run's own RunLaunchRequested, reused unchanged)",
            run.kind, run.selection.model, run.selection.effort, run.selection.args
        );
        if !run.launched {
            // D1: the admitted-but-unresolved state, stated rather than
            // papered over. `RunLoop::launch` asks Herdr whether the
            // agent is live and either reconciles onto it or launches
            // once under this same bound request.
            println!(
                "launch outcome uncertain: this Run's request was admitted but no RunLaunched \
                 ever followed (a lost reply or a daemon loss in the launch window); Herdr is \
                 the only place the answer can come from, and the bound request above does not \
                 change either way"
            );
        }
    } else {
        let (kind, selection, provenance) =
            resolve_launch_selection(cli_kind, cli_model, cli_effort, authored_selection);
        if let Some(provenance) = provenance {
            println!("launch selection: {provenance}");
        }
        println!(
            "launch selection resolved: kind={} model={:?} effort={:?} args={:?}",
            kind, selection.model, selection.effort, selection.args
        );
        run.kind = kind;
        run.selection = selection;
    }
    // D1 "validate before unnecessary execution-side effects": the
    // resolved request is judged here, before the worktree is
    // materialized, before anything is journaled and before Herdr is
    // connected to — the same check `RunLoop::launch` repeats as its
    // own backstop for every other caller.
    if let Err(err) = wirk_herdr::validate_selection(run.kind.0.as_str(), &run.selection) {
        eprintln!("wirk run: {err}");
        return ExitCode::from(2);
    }
    let actor = match world {
        World::Actor(actor) => actor,
        World::Deterministic(_) => {
            eprintln!(
                "wirk run: work {} is a Deterministic Waypoint, not this executor's kind",
                work_id.0
            );
            return ExitCode::from(2);
        }
    };

    // Step 2 (loop.md §1): `git worktree add` from the World's own
    // repository/branch/base_sha, wirk-side, before any Herdr call.
    let worktree_path = estate_path.join("worktrees").join(&work_id.0);
    let head = match wirk_herdr::git::worktree_add(
        Path::new(&actor.repository),
        &worktree_path,
        &actor.branch,
        &actor.base_sha,
    ) {
        Ok(head) => head,
        Err(err) => {
            eprintln!("wirk run: {err}");
            return ExitCode::from(2);
        }
    };
    println!("worktree {}", worktree_path.display());

    let mut updated_actor = actor.clone();
    if actor.worktree_path.as_os_str().is_empty() {
        if let Err(err) = wirkd_record(
            &pointer.socket,
            &work_id,
            Some(run.id.clone()),
            EventKind::WorktreeCreated {
                repo: actor.repository.clone(),
                base_sha: head.clone(),
            },
        ) {
            eprintln!("wirk run: {err}");
            return ExitCode::from(2);
        }
        println!("WorktreeCreated");

        // The first materialization is a run-scoped legal transition.
        // Location remains excluded from the content fingerprint.
        updated_actor.worktree_path = worktree_path;
        let updated_world = World::Actor(updated_actor.clone());
        let world_hash = WorldHash::of(&updated_world);
        if let Err(err) = wirkd_record(
            &pointer.socket,
            &work_id,
            Some(run.id.clone()),
            EventKind::WaypointReserved {
                waypoint: run.waypoint.clone(),
                world_hash,
                world: updated_world,
            },
        ) {
            eprintln!("wirk run: {err}");
            return ExitCode::from(2);
        }
    } else if actor.worktree_path != worktree_path || head != actor.base_sha {
        eprintln!("wirk run: the existing Run binding does not match the reusable checkout");
        return ExitCode::from(2);
    }
    let updated_world = World::Actor(updated_actor);

    let client = match SocketClient::connect(herdr_socket) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("wirk run: {err}");
            return ExitCode::from(2);
        }
    };
    let wirkd_api = WirkdRunLoopApi {
        socket: pointer.socket.clone(),
    };
    let mut run_loop = RunLoop::new(client, wirkd_api);

    match run_loop.drive(&work_id, &run, &updated_world) {
        Ok(Outcome::Claimed) => {
            println!("Claimed");
            ExitCode::SUCCESS
        }
        Ok(Outcome::NeedsInput) => {
            println!("NeedsInput");
            if let Some(observation) = run_loop.stuck_observation() {
                println!("{observation}");
            }
            ExitCode::from(4)
        }
        Ok(Outcome::Vanished) => {
            println!("Vanished");
            ExitCode::from(5)
        }
        // A live Herdr session's subscription stays open until a
        // terminal condition; `Pending` here means the stream ended
        // (EOF) without one — not named its own exit code by the build
        // brief, grouped with Vanished/Failed (J1: local, reversible,
        // AGENTS.md's "a defect with a standard answer is not J0").
        Ok(Outcome::Pending) => {
            println!("Pending");
            ExitCode::from(5)
        }
        Err(err) => {
            eprintln!("wirk run: {err}");
            ExitCode::from(5)
        }
    }
}

fn run_usage() -> ExitCode {
    eprintln!(
        "usage: wirk run --estate <root> --work <id> --session <name> [--herdr-socket <path>] \
         [--actor-kind <kind>] [--actor-model <model>] [--actor-effort <level>]\n\
         \n\
         P3 native launch selection precedence, per field (kind/model/effort \
         independently): this invocation's own explicit flag, else the Route's \
         authored `selection` for this Waypoint, else the harness's own native \
         default (unrepresented as any particular value). Resolved once, at this \
         Run's first successful launch, and durably bound from then on — a later \
         `wirk run` invocation for the same Run reuses the bound selection and \
         refuses a conflicting explicit flag rather than silently relaunching \
         differently configured.\n\
         \n\
         An authored `selection` that names its own `harness` is scoped to that \
         harness: launching a different one with `--actor-kind` does not inherit \
         its model, effort or raw args (they are that harness's syntax), and the \
         drop is printed. An authored selection with no `harness` is unscoped and \
         applies to whichever harness runs.\n\
         \n\
         `selection.args` stays the raw escape hatch for anything the convenience \
         fields do not cover, but may not restate one of them: a raw argument that \
         sets the same harness flag as an explicit model/effort is refused before \
         launch rather than submitted alongside it.\n\
         \n\
         One invocation at a time owns a Run's launch and its drive loop. wirkd \
         admits the launch *attempt* as well as the request: a duplicate or \
         recovery invocation is refused while the admitted holder's own process is \
         still running, and inherits the attempt when it is not — there is no \
         marker to release and no flag to force. A Run's launch is also bound to \
         the Herdr socket it was first attempted on, so recovering it against a \
         different Herdr session is refused rather than allowed to start a second \
         agent where the first is neither visible nor name-colliding. wirk never \
         claims an external launch happened exactly once: an admitted request whose \
         result Herdr will not confirm is recorded as uncertain and left for a \
         later invocation to reconcile."
    );
    ExitCode::from(1)
}

/// Same shared move `main.rs`'s own `flag_value` is (R6): duplicated
/// rather than threaded through a shared module, matching this
/// codebase's existing precedent of a small per-file copy over a new
/// shared-utility module for a one-line helper (`work_state_name`
/// already exists once in `main.rs` and once in `server.rs`).
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// `--actor-kind <kind>` (0056 D164, superseding 0041 D129's closed
/// `claude|opencode` list): `None` when the flag is absent — P3 native
/// launch selection needs to tell "no explicit override this
/// invocation" apart from "explicitly claude", so a Route-authored
/// default or an already-fixed launch can still apply
/// (`resolve_launch_selection`). Any given value is carried through
/// as-is, never refused here — Herdr's own `agent.start` is the
/// validation (0056 D164 "wirk adds no list of its own"), and a
/// wirk-side rejection of a kind Herdr accepts is exactly the block
/// the owner ruled out.
fn parse_actor_kind_override(rest: &[String]) -> Option<wirk_core::ActorKind> {
    flag_value(rest, "--actor-kind").map(wirk_core::ActorKind)
}

/// P3 native launch selection: `wirk run`'s own precedence, applied
/// once per field (BUILD-BRIEF.md item 1 — "explicit CLI override and
/// documented precedence"). CLI-explicit wins when given; otherwise
/// the Route's own authored default for this Waypoint
/// (`AuthoredSelection`, read from wirkd's `status` reply); otherwise
/// absent — no flag reaches `build_selection_args`, and the harness's
/// own native default engages, honestly unrepresented as any
/// particular model/effort identity (PREPARATION-ADJUDICATION.md point
/// 4). `harness` falls back to `ActorKind::default()` (claude) only at
/// the very end, unchanged from before this wave.
fn resolve_launch_selection(
    cli_kind: Option<wirk_core::ActorKind>,
    cli_model: Option<String>,
    cli_effort: Option<String>,
    authored: Option<wirk_core::AuthoredSelection>,
) -> (
    wirk_core::ActorKind,
    wirk_core::ActorSelection,
    Option<String>,
) {
    let authored = authored.unwrap_or_default();
    let kind = cli_kind
        .clone()
        .or(authored.harness.clone())
        .unwrap_or_default();
    // P3 native launch selection, D3: an authored selection that names
    // its own `harness` is scoped to that harness. `harness` was
    // already used for precedence; it decides compatibility too. When
    // this invocation launches a *different* harness, the authored
    // `model`/`effort`/`args` are not inherited — they are that
    // harness's syntax, and handing claude's model name and claude's
    // own flags to opencode is exactly the silent incompatible
    // inheritance this contract forbids. Only the explicit flags of
    // this invocation apply then, and the drop is announced, never
    // silent.
    //
    // An authored selection with **no** `harness` is deliberately
    // unscoped: the author declined to tie it to one, so it applies to
    // whichever harness runs. That is authorship, not inference, and it
    // is the only case where authored fields cross a harness boundary.
    let authored_applies = match &authored.harness {
        Some(authored_kind) => *authored_kind == kind,
        None => true,
    };
    let provenance = match &authored.harness {
        Some(authored_kind)
            if !authored_applies
                && (authored.model.is_some()
                    || authored.effort.is_some()
                    || !authored.args.is_empty()) =>
        {
            Some(format!(
                "this Waypoint's authored selection is scoped to harness {authored_kind}; \
                 this invocation launches {kind}, so its model/effort/raw args are not \
                 inherited"
            ))
        }
        _ => None,
    };
    let selection = if authored_applies {
        wirk_core::ActorSelection {
            model: cli_model.or(authored.model),
            effort: cli_effort.or(authored.effort),
            args: authored.args,
        }
    } else {
        wirk_core::ActorSelection {
            model: cli_model,
            effort: cli_effort,
            args: Vec::new(),
        }
    };
    (kind, selection, provenance)
}

/// P3 native launch selection (PREPARATION-ADJUDICATION.md point 3:
/// "a repeated invocation must not silently alter an already fixed Run
/// launch"). Called only when `run.launch_requested` is already
/// `true` — this Run's own `RunLaunchRequested` has folded, so
/// `run.kind`/`run.selection` are the durably bound request, not the
/// empty default. D1 moved this from `launched` to `launch_requested`
/// deliberately: the request is fixed the moment it is admitted, which
/// is *before* the launch, so a reinvocation inside the failure window
/// is refused a different model exactly as one after a successful
/// launch is. `None` when every explicit flag this invocation gave (if
/// any) agrees with what was already bound; `Some(reason)` otherwise,
/// naming the field and both values so the printed refusal is
/// self-explanatory. An invocation with no explicit flags at all
/// always agrees — reusing a bound launch silently is the whole point,
/// only an *explicit, conflicting* ask is refused.
fn conflicting_reinvocation(
    run: &Run,
    cli_kind: &Option<wirk_core::ActorKind>,
    cli_model: &Option<String>,
    cli_effort: &Option<String>,
) -> Option<String> {
    if let Some(kind) = cli_kind
        && kind != &run.kind
    {
        return Some(format!(
            "Run {}'s launch is already bound to actor-kind {}; this invocation asked for {kind}",
            run.id.0, run.kind
        ));
    }
    if let Some(model) = cli_model
        && Some(model) != run.selection.model.as_ref()
    {
        return Some(format!(
            "Run {}'s launch is already bound to model {:?}; this invocation asked for {model:?}",
            run.id.0, run.selection.model
        ));
    }
    if let Some(effort) = cli_effort
        && Some(effort) != run.selection.effort.as_ref()
    {
        return Some(format!(
            "Run {}'s launch is already bound to effort {:?}; this invocation asked for {effort:?}",
            run.id.0, run.selection.effort
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_kind_override_is_none_when_absent() {
        assert_eq!(parse_actor_kind_override(&[]), None);
    }

    #[test]
    fn actor_kind_opencode_selects_opencode() {
        let rest = vec!["--actor-kind".to_string(), "opencode".to_string()];
        assert_eq!(
            parse_actor_kind_override(&rest),
            Some(wirk_core::ActorKind::opencode())
        );
    }

    #[test]
    fn actor_kind_claude_selects_claude() {
        let rest = vec!["--actor-kind".to_string(), "claude".to_string()];
        assert_eq!(
            parse_actor_kind_override(&rest),
            Some(wirk_core::ActorKind::claude())
        );
    }

    /// 0056 D164's decisive case: a kind wirk has never heard of is
    /// carried through, not refused — the Aria probe's `codex` and an
    /// arbitrary `somekind`, both accepted here where main's
    /// `actor_kind_bogus_is_refused` returned `Err(())`.
    #[test]
    fn actor_kind_codex_is_carried_through() {
        let rest = vec!["--actor-kind".to_string(), "codex".to_string()];
        assert_eq!(
            parse_actor_kind_override(&rest),
            Some(wirk_core::ActorKind("codex".to_string()))
        );
    }

    #[test]
    fn actor_kind_arbitrary_kind_is_carried_through() {
        let rest = vec!["--actor-kind".to_string(), "somekind".to_string()];
        assert_eq!(
            parse_actor_kind_override(&rest),
            Some(wirk_core::ActorKind("somekind".to_string()))
        );
    }

    // ---- resolve_launch_selection: CLI > Route-authored > native default ----

    #[test]
    fn resolve_prefers_cli_over_authored_over_default() {
        let authored = wirk_core::AuthoredSelection {
            harness: Some(wirk_core::ActorKind("codex".to_string())),
            model: Some("authored-model".to_string()),
            effort: Some("authored-effort".to_string()),
            args: vec!["--from-route".to_string()],
        };
        let (kind, selection, provenance) = resolve_launch_selection(
            Some(wirk_core::ActorKind("codex".to_string())),
            Some("cli-model".to_string()),
            None,
            Some(authored),
        );
        assert_eq!(kind, wirk_core::ActorKind("codex".to_string()));
        assert_eq!(selection.model.as_deref(), Some("cli-model"));
        assert_eq!(
            selection.effort.as_deref(),
            Some("authored-effort"),
            "effort had no CLI override: the authored value applies"
        );
        assert_eq!(selection.args, vec!["--from-route".to_string()]);
        assert_eq!(
            provenance, None,
            "the authored selection is scoped to the harness actually launched: nothing dropped"
        );
    }

    #[test]
    fn resolve_with_nothing_given_falls_back_to_native_default() {
        let (kind, selection, provenance) = resolve_launch_selection(None, None, None, None);
        assert_eq!(kind, wirk_core::ActorKind::claude());
        assert_eq!(selection, wirk_core::ActorSelection::default());
        assert_eq!(provenance, None);
    }

    // ---- D3: an authored selection is scoped to the harness it names ----

    #[test]
    fn resolve_does_not_inherit_another_harnesss_model_effort_or_args() {
        let authored = wirk_core::AuthoredSelection {
            harness: Some(wirk_core::ActorKind::claude()),
            model: Some("claude-only-model".to_string()),
            effort: Some("high".to_string()),
            args: vec!["--dangerously-skip-permissions".to_string()],
        };
        let (kind, selection, provenance) = resolve_launch_selection(
            Some(wirk_core::ActorKind::opencode()),
            None,
            None,
            Some(authored),
        );
        assert_eq!(kind, wirk_core::ActorKind::opencode());
        assert_eq!(
            selection,
            wirk_core::ActorSelection::default(),
            "claude's model, claude's effort and a claude-only flag are not opencode's to \
             inherit"
        );
        let provenance = provenance.expect("the drop is announced, never silent");
        assert!(
            provenance.contains("scoped to harness claude"),
            "{provenance}"
        );
        assert!(provenance.contains("not"), "{provenance}");
    }

    #[test]
    fn resolve_keeps_explicit_flags_when_the_authored_harness_is_swapped_out() {
        let authored = wirk_core::AuthoredSelection {
            harness: Some(wirk_core::ActorKind::claude()),
            model: Some("claude-only-model".to_string()),
            effort: None,
            args: vec!["--dangerously-skip-permissions".to_string()],
        };
        let (kind, selection, _) = resolve_launch_selection(
            Some(wirk_core::ActorKind::opencode()),
            Some("provider/real".to_string()),
            None,
            Some(authored),
        );
        assert_eq!(kind, wirk_core::ActorKind::opencode());
        assert_eq!(
            selection.model.as_deref(),
            Some("provider/real"),
            "this invocation's own explicit model still applies"
        );
        assert!(selection.args.is_empty());
    }

    #[test]
    fn resolve_applies_an_unscoped_authored_selection_to_whatever_harness_runs() {
        let authored = wirk_core::AuthoredSelection {
            harness: None,
            model: Some("m-x".to_string()),
            effort: None,
            args: vec!["--raw".to_string()],
        };
        let (kind, selection, provenance) = resolve_launch_selection(
            Some(wirk_core::ActorKind("codex".to_string())),
            None,
            None,
            Some(authored),
        );
        assert_eq!(kind, wirk_core::ActorKind("codex".to_string()));
        assert_eq!(
            selection.model.as_deref(),
            Some("m-x"),
            "an author who names no harness scoped it to none: it applies"
        );
        assert_eq!(selection.args, vec!["--raw".to_string()]);
        assert_eq!(provenance, None);
    }

    // ---- conflicting_reinvocation: repeated invocation immutability ----

    fn launched_run(kind: wirk_core::ActorKind, model: Option<&str>) -> Run {
        Run {
            id: RunId("run-1".to_string()),
            waypoint: wirk_core::WaypointId("wp-1".to_string()),
            attempt: 1,
            world_hash: wirk_core::WorldHash("deadbeef".to_string()),
            state: RunState::Open,
            kind,
            selection: wirk_core::ActorSelection {
                model: model.map(str::to_string),
                effort: None,
                args: Vec::new(),
            },
            launched: true,
            launch_requested: true,
            launch_argv: Vec::new(),
            launch_attempt: None,
        }
    }

    #[test]
    fn reinvocation_with_no_explicit_flags_never_conflicts() {
        let run = launched_run(wirk_core::ActorKind::claude(), Some("opus"));
        assert_eq!(conflicting_reinvocation(&run, &None, &None, &None), None);
    }

    #[test]
    fn reinvocation_repeating_the_same_bound_kind_does_not_conflict() {
        let run = launched_run(wirk_core::ActorKind::claude(), None);
        assert_eq!(
            conflicting_reinvocation(&run, &Some(wirk_core::ActorKind::claude()), &None, &None),
            None
        );
    }

    #[test]
    fn reinvocation_with_a_conflicting_kind_is_refused() {
        let run = launched_run(wirk_core::ActorKind::claude(), None);
        let reason =
            conflicting_reinvocation(&run, &Some(wirk_core::ActorKind::opencode()), &None, &None)
                .expect("a conflicting --actor-kind must be refused");
        assert!(reason.contains("claude"));
        assert!(reason.contains("opencode"));
    }

    #[test]
    fn reinvocation_with_a_conflicting_model_is_refused() {
        let run = launched_run(wirk_core::ActorKind::claude(), Some("opus"));
        let reason = conflicting_reinvocation(&run, &None, &Some("sonnet".to_string()), &None)
            .expect("a conflicting --actor-model must be refused");
        assert!(reason.contains("opus"));
        assert!(reason.contains("sonnet"));
    }
}
