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
use wirk_herdr::run_loop::{Outcome, RecordOutcome, RunLoop, RunStatusEntry, WirkdApi, WorkStatus};

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
    /// Ruling 0159 (recovery preserves explicit selection): the
    /// immediately-preceding Run's own bound kind/selection, sent only
    /// when this Run has not yet had its own launch admitted and its
    /// predecessor at this Waypoint actually reached one
    /// (`server.rs`'s `prior_launch_for_waypoint`). `None` for a first
    /// attempt, an already-launch-requested Run, and a genuinely
    /// unlaunched retry — nothing fabricated in any of those cases.
    #[serde(default)]
    prior_selection: Option<PriorSelection>,
    /// Ruling 0160 item 2 (interrupted materialization): this Run's own
    /// already-journaled `WorktreeCreated`, when the journal carries
    /// one (`server.rs`'s `handle_status`). `None` when this Run has
    /// not created its worktree yet — the ordinary first-materialization
    /// case. Same `#[serde(default)]` tolerance as the fields above.
    #[serde(default)]
    worktree_created: Option<WorktreeCreatedFact>,
}

/// Wire shape of `StatusRunEntry.worktree_created` (ruling 0160): the
/// `repo`/`base_sha` pair this Run's own `WorktreeCreated` was admitted
/// with, exactly as `handle_status` serializes them (`server.rs`).
#[derive(Debug, Deserialize)]
struct WorktreeCreatedFact {
    repo: String,
    base_sha: String,
}

/// Wire shape of `StatusRunEntry.prior_selection` (ruling 0159): the
/// prior Run's own `kind`/`selection`, exactly as `handle_status`
/// serializes them (`server.rs`).
#[derive(Debug, Deserialize)]
struct PriorSelection {
    kind: wirk_core::ActorKind,
    selection: wirk_core::ActorSelection,
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
/// A prior Run's own bound kind/selection (ruling 0159), carried
/// forward only when that predecessor actually reached an admitted
/// launch.
type PriorLaunch = (wirk_core::ActorKind, wirk_core::ActorSelection);

/// `fetch_open_run`'s result: the open Run, its reserved World, the
/// Waypoint's own Route-authored default selection (if any), a
/// carried-forward prior launch (ruling 0159/0160, `None` outside a
/// same-waypoint retry whose lineage holds an admitted launch), and
/// this Run's own already-journaled `WorktreeCreated`, if it has one
/// (ruling 0160 item 2). A struct rather than a fifth tuple slot: five
/// anonymous positions at three call sites is how the wrong one gets
/// read.
struct OpenRun {
    run: Run,
    world: World,
    authored: Option<wirk_core::AuthoredSelection>,
    prior: Option<PriorLaunch>,
    worktree_created: Option<WorktreeCreatedFact>,
}

fn fetch_open_run(socket: &Path, work_id: &WorkId) -> Result<OpenRun, ExecutorError> {
    let parsed = fetch_status(socket, work_id)?;
    // P3 native closeout item 2 (`native-closeout/TRIAGE.md` §2, from
    // the independent reviewer's own observation in
    // `p3-world-loop/native-learning-use/raw/recovery-acceptance/
    // recovery-check.md`): a `wirk work cancel` leaves the Work
    // `canceled` while its Run stays `Open` — cancelling is a decision
    // about the Work, and `fold` deliberately does not rewrite Run
    // states. Selecting a Run purely on `RunState::Open` therefore
    // reattached a canceled Work and drove it on, printing "recovering
    // in-Work progress ... reattaching without resetting" for work its
    // owner had explicitly stopped. The Work's own state is already in
    // this same reply and already decoded (`parse_work_state`), so this
    // needs no new field and no new verb: a Work that has reached a
    // terminal state refuses reattachment and names the state, rather
    // than resuming a leftover Run. A non-terminal Work is unaffected —
    // `pending`, `active`, `waiting`, `needs_input` and `blocked` all
    // still select their open Run exactly as before.
    let work_state = parse_work_state(&parsed.state)?;
    if work_state.is_terminal() {
        return Err(ExecutorError::Wirkd(format!(
            "work {} is {}: a terminal Work is not reattached, and its leftover Run is not \
             resumed — `wirk work retry` is refused for it too, so a new Work is the only way on",
            work_id.0, parsed.state
        )));
    }
    let entry = parsed
        .runs
        .into_iter()
        .find(|entry| matches!(entry.run.state, RunState::Open))
        .ok_or_else(|| ExecutorError::Wirkd(format!("no open Run for work {}", work_id.0)))?;
    let world = entry.world.ok_or_else(|| {
        ExecutorError::Wirkd("the open Run's Waypoint has no reserved World".to_string())
    })?;
    let prior_selection = entry
        .prior_selection
        .map(|prior| (prior.kind, prior.selection));
    Ok(OpenRun {
        run: entry.run,
        world,
        authored: entry.selection,
        prior: prior_selection,
        worktree_created: entry.worktree_created,
    })
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
) -> Result<RecordOutcome, ExecutorError> {
    let reply = wirkd::client::call(
        socket,
        &Request::record(RecordPayload {
            work_id: work_id.clone(),
            run,
            kind,
        }),
    )?;
    match reply {
        Reply::Ok { .. } => Ok(RecordOutcome::Accepted),
        // P3 native closeout item 1a: wirkd now names which of its three
        // record refusals this is. `RunSettled` — the Run this write
        // names already reached its own outcome — is handed back as the
        // answer it is rather than flattened into an error string the
        // driver cannot read. Every other refusal, the superseded-Run
        // one included, stays an error exactly as before.
        Reply::Err { error, .. } if error.code == RUN_SETTLED_CODE => {
            Ok(RecordOutcome::RunSettled(error.message))
        }
        Reply::Err { error, .. } => Err(ExecutorError::Wirkd(format!(
            "record refused: {} {}",
            error.code, error.message
        ))),
    }
}

/// wirkd's own code for "the Run this record names has already settled"
/// (`server.rs`'s `handle_record`). Named once, here, because this is
/// the only place the wire code is interpreted.
const RUN_SETTLED_CODE: &str = "RunSettled";

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

    fn record(
        &self,
        work_id: &WorkId,
        run_id: &RunId,
        kind: EventKind,
    ) -> Result<RecordOutcome, Self::Error> {
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

    let OpenRun {
        mut run,
        world,
        authored: authored_selection,
        prior: prior_selection,
        worktree_created,
    } = match fetch_open_run(&pointer.socket, &work_id) {
        Ok(quad) => quad,
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
        // Ruling 0159 (recovery preserves explicit selection): a
        // supported retry's fresh Run reaches here with `prior_selection`
        // set to the latest *admitted* launch in this Waypoint's own
        // retry lineage — ruling 0160's correction: not merely the
        // immediate predecessor, so an intervening attempt that was
        // opened and abandoned before launch does not erase a choice
        // that was actually made (`server.rs`'s
        // `prior_launch_for_waypoint`; `None` for a first attempt or a
        // lineage that never admitted one). That prior
        // resolution already folded in whatever this Waypoint's own
        // precedence produced at the time — an explicit CLI override
        // included — so it stands in for the Route's own authored
        // default rather than beside it: `resolve_selection_source`
        // below picks whichever of the two actually applies, and the
        // existing same-harness boundary scoping
        // (`resolve_launch_selection`'s own `authored_applies`) still
        // governs it unchanged, now keyed on the *prior Run's* harness
        // instead of the Route's.
        let (effective_authored, origin) =
            resolve_selection_source(authored_selection, prior_selection);
        let (kind, selection, provenance) =
            resolve_launch_selection(cli_kind, cli_model, cli_effort, effective_authored, origin);
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
        // Ruling 0160 item 2 (the observed interrupted materialization,
        // `native-selection-retry-verify/VERIFIED.md`'s own crash-window
        // observation, retained at
        // `/var/tmp/wirk-p3-selection-retry-verify/evidence/w5a1.log`).
        // Materialization is two journal records: `WorktreeCreated`,
        // then `WaypointReserved` — and only the second one puts a
        // `worktree_path` on the World. So "is this Run's worktree
        // already created?" was being answered by a fact that only
        // becomes true one record *later*, and a caller killed in
        // between (there, `head`'s SIGPIPE on the `println!` that
        // immediately follows the first record) re-entered this branch
        // on its next invocation, re-emitted `WorktreeCreated`, and was
        // refused by `handle_record`'s at-most-one-per-Run guard:
        // `InvalidTransition WorktreeCreated does not match this Run's
        // unmaterialized Actor binding`. That refusal is correct — the
        // event really is a duplicate — but nothing else could make
        // progress either: the Work is still `active`, so `wirk work
        // retry` and `wirk work fail` both refuse it, and the Run was
        // wedged for the rest of its life with its worktree sitting
        // complete on disk.
        //
        // The missing piece was never a new transition; it was reading
        // the durable fact that already existed. wirkd now reports this
        // Run's own admitted `WorktreeCreated` in `status`
        // (`worktree_created`), so an interrupted materialization is
        // finished from the journal: `git worktree add` above is
        // idempotent and has already re-established (or found) the same
        // checkout, and this invocation simply skips the half that is
        // already durable and records the half that is not. No second
        // creation event, no reset, no journal surgery, and the
        // *content* is still checked — a durable record naming a
        // different repository or a different base sha than the
        // checkout this invocation just materialized is refused rather
        // than resumed onto.
        match &worktree_created {
            Some(created) if created.repo == actor.repository && created.base_sha == head => {
                println!(
                    "WorktreeCreated already journaled for this Run (repo {repo}, base {base}) \
                     — completing an interrupted materialization from the journal rather than \
                     creating it a second time",
                    repo = created.repo,
                    base = created.base_sha
                );
            }
            Some(created) => {
                eprintln!(
                    "wirk run: this Run already journaled WorktreeCreated for repo {repo} at \
                     base {base}, but this invocation materialized {here} at {head} — an \
                     interrupted materialization is only resumed onto the checkout it actually \
                     recorded",
                    repo = created.repo,
                    base = created.base_sha,
                    here = actor.repository
                );
                return ExitCode::from(2);
            }
            None => {
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
            }
        }

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
    } else if actor.worktree_path != worktree_path {
        eprintln!("wirk run: the existing Run binding does not match the reusable checkout");
        return ExitCode::from(2);
    } else {
        // P3 execution-recovery item 2, with root's own correction: an
        // actor that has committed on its own branch in this Run's own
        // worktree since it was materialized must not be treated the
        // same as a genuinely foreign checkout — the prior
        // exact-equality check refused both alike, losing daemon/Claim
        // delivery for legitimate in-Work progress
        // (MECHANISM-REPORT.md qualification 4, `executor.rs:457`). But
        // path equality alone (checked above) is not identity: this
        // block runs on *every* reattachment, `head == actor.base_sha`
        // included, not only a recovering one, and verifies the
        // worktree's checked-out branch and its actual git-common-dir
        // repository identity — never ancestry alone, and never trusted
        // merely because the path matched. Neither `actor.base_sha` nor
        // `actor.worktree_path` is widened or replaced here: the World
        // reattached to is the same one already reserved, and every
        // other check (foreign branch, foreign repository, wrong
        // destination, a superseded Run — refused earlier by
        // `fetch_open_run` before this function is ever reached) is
        // unchanged or strengthened, never relaxed.
        match wirk_herdr::git::current_branch(&worktree_path) {
            Ok(checked_out) if checked_out == actor.branch => {}
            Ok(checked_out) => {
                eprintln!(
                    "wirk run: the existing Run binding does not match the reusable checkout \
                     (worktree HEAD is on branch {checked_out}, not this Run's own \
                     {branch})",
                    branch = actor.branch
                );
                return ExitCode::from(2);
            }
            Err(err) => {
                eprintln!("wirk run: {err}");
                return ExitCode::from(2);
            }
        }
        match (
            wirk_herdr::git::repository_identity(&worktree_path),
            wirk_herdr::git::repository_identity(Path::new(&actor.repository)),
        ) {
            (Ok(here), Ok(reserved)) if here == reserved => {}
            (Ok(_), Ok(_)) => {
                eprintln!(
                    "wirk run: the existing Run binding does not match the reusable checkout \
                     (worktree at {} is not this Run's own repository)",
                    worktree_path.display()
                );
                return ExitCode::from(2);
            }
            (Err(err), _) | (_, Err(err)) => {
                eprintln!("wirk run: {err}");
                return ExitCode::from(2);
            }
        }
        if head != actor.base_sha {
            match wirk_herdr::git::is_ancestor(&worktree_path, &actor.base_sha, &head) {
                Ok(true) => {
                    println!(
                        "recovering in-Work progress: worktree HEAD {head} is ahead of this \
                         Run's reserved base {base} on its own branch {branch} — reattaching \
                         without resetting",
                        base = actor.base_sha,
                        branch = actor.branch
                    );
                }
                Ok(false) => {
                    eprintln!(
                        "wirk run: the existing Run binding does not match the reusable checkout"
                    );
                    return ExitCode::from(2);
                }
                Err(err) => {
                    eprintln!("wirk run: {err}");
                    return ExitCode::from(2);
                }
            }
        }
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

/// Ruling 0159 (recovery preserves explicit selection): which middle
/// precedence tier feeds `resolve_launch_selection` — the Route's own
/// authored default, or a carried-forward prior Run's already-effective
/// kind/selection. A prior Run's effective selection is not *beside*
/// the Route's own default, it *is* whichever the Route/CLI precedence
/// already produced for the original launch (an explicit CLI override
/// included, since that is exactly what `run.selection` binds once
/// `RunLaunchRequested` folds) — so when one is carried forward it
/// stands in for the Route's authored tier entirely rather than being
/// merged field-by-field against it. `prior` is `None` for a first
/// attempt and for a genuinely unlaunched retry (nothing effective to
/// carry — `server.rs`'s `prior_launch_for_waypoint` never sends one
/// then), so that ordinary case is unchanged: the Route's own authored
/// default, exactly as before this ruling.
fn resolve_selection_source(
    authored: Option<wirk_core::AuthoredSelection>,
    prior: Option<PriorLaunch>,
) -> (Option<wirk_core::AuthoredSelection>, SelectionOrigin) {
    match prior {
        Some((kind, selection)) => (
            Some(wirk_core::AuthoredSelection {
                harness: Some(kind),
                model: selection.model,
                effort: selection.effort,
                args: selection.args,
            }),
            SelectionOrigin::PriorRun,
        ),
        None => (authored, SelectionOrigin::RouteAuthored),
    }
}

/// Ruling 0160: which of the two sources `resolve_selection_source`
/// actually chose, so the boundary disclosure names it correctly.
/// `native-selection-retry-verify/VERIFIED.md` §3 caught the drop
/// message calling a carried-forward prior selection "this Waypoint's
/// authored selection" — in the one path ruling 0159 added, that is
/// the wrong source, and a provenance line that misnames its own
/// source is read and believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionOrigin {
    /// This Waypoint's own Route-authored `selection`, as submitted.
    RouteAuthored,
    /// A prior Run at this same Waypoint whose launch was admitted —
    /// whatever the Route/CLI precedence resolved to *then*.
    PriorRun,
}

impl SelectionOrigin {
    /// How the drop disclosure names this source, in the subject
    /// position of its own sentence.
    fn describe(self) -> &'static str {
        match self {
            Self::RouteAuthored => "this Waypoint's authored selection",
            Self::PriorRun => {
                "the selection carried forward from this Waypoint's last admitted launch"
            }
        }
    }
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
    origin: SelectionOrigin,
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
                "{source} is scoped to harness {authored_kind}; \
                 this invocation launches {kind}, so its model/effort/raw args are not \
                 inherited",
                source = origin.describe()
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
            SelectionOrigin::RouteAuthored,
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
        let (kind, selection, provenance) =
            resolve_launch_selection(None, None, None, None, SelectionOrigin::RouteAuthored);
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
            SelectionOrigin::RouteAuthored,
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
            SelectionOrigin::RouteAuthored,
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
            SelectionOrigin::RouteAuthored,
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
            expansions: Vec::new(),
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

    // ---- Ruling 0159: recovery preserves explicit selection ----
    //
    // Reproduces the measured defect exactly: an original Run launched
    // with an explicit CLI selection (kind=claude, model=sonnet,
    // effort=medium); the Waypoint's Route carries no authored
    // selection at all. A supported retry's fresh Run must resolve to
    // the identical kind/model/effort, not the harness's native
    // default — end to end through `resolve_selection_source` and then
    // `resolve_launch_selection`, the same two calls `run_command`
    // itself makes.

    #[test]
    fn retry_carries_forward_the_prior_explicit_selection_over_the_harness_default() {
        let prior = Some((
            wirk_core::ActorKind::claude(),
            wirk_core::ActorSelection {
                model: Some("sonnet".to_string()),
                effort: Some("medium".to_string()),
                args: vec![],
            },
        ));
        // No Route-authored selection at all — the only way the bug's
        // fallback ("the harness's hardcoded default model/effort")
        // could have been reached, exactly as ruling 0159 describes it.
        let (effective, origin) = resolve_selection_source(None, prior);
        assert_eq!(origin, SelectionOrigin::PriorRun);
        let (kind, selection, provenance) =
            resolve_launch_selection(None, None, None, effective, origin);
        assert_eq!(kind, wirk_core::ActorKind::claude());
        assert_eq!(
            selection.model.as_deref(),
            Some("sonnet"),
            "the retry must not fall back to the harness's hardcoded default model"
        );
        assert_eq!(selection.effort.as_deref(), Some("medium"));
        assert_eq!(provenance, None, "same harness both sides: nothing dropped");
    }

    #[test]
    fn a_genuinely_unlaunched_retry_carries_nothing_forward() {
        // `prior_launch_for_waypoint` sends `None` when the predecessor
        // Run never reached `launch_requested` — nothing explicit was
        // ever decided for it, so nothing is fabricated here either.
        // Route-authored default (if any) governs exactly as a first
        // attempt would.
        let authored = Some(wirk_core::AuthoredSelection {
            harness: None,
            model: Some("route-default-model".to_string()),
            effort: None,
            args: vec![],
        });
        let (effective, origin) = resolve_selection_source(authored.clone(), None);
        assert_eq!(origin, SelectionOrigin::RouteAuthored);
        assert_eq!(
            effective, authored,
            "with no prior launch to carry, the Route's own authored default is unchanged"
        );
    }

    #[test]
    fn retry_across_an_explicit_different_harness_does_not_inherit_the_prior_models_flags() {
        // The existing same-harness boundary rule stays intact: this
        // invocation explicitly asks for a different harness than the
        // prior Run actually launched under, so the prior model/effort
        // (that harness's own vocabulary) must not silently cross over
        // — the same rule an authored selection scoped to one harness
        // already obeys.
        let prior = Some((
            wirk_core::ActorKind::claude(),
            wirk_core::ActorSelection {
                model: Some("sonnet".to_string()),
                effort: Some("medium".to_string()),
                args: vec!["--dangerously-skip-permissions".to_string()],
            },
        ));
        let (effective, origin) = resolve_selection_source(None, prior);
        let (kind, selection, provenance) = resolve_launch_selection(
            Some(wirk_core::ActorKind::opencode()),
            None,
            None,
            effective,
            origin,
        );
        assert_eq!(kind, wirk_core::ActorKind::opencode());
        assert_eq!(
            selection,
            wirk_core::ActorSelection::default(),
            "claude's carried-forward model/effort/args are not opencode's to inherit"
        );
        let provenance = provenance.expect("the drop is announced, never silent");
        assert!(provenance.contains("claude"), "{provenance}");
        // Ruling 0160: the disclosure names the source it actually
        // used. The carried selection came from a prior Run, not from
        // anything this Waypoint's Route authored, and saying otherwise
        // in a provenance line is a false statement about where a
        // binding came from.
        assert!(
            provenance.contains("carried forward from this Waypoint's last admitted launch"),
            "{provenance}"
        );
        assert!(
            !provenance.contains("authored selection"),
            "a prior Run's selection is not the Waypoint's authoring: {provenance}"
        );
    }

    // ---- Ruling 0160: the whole retry lineage, not one hop ----

    /// The measured defect (`native-selection-retry-verify/VERIFIED.md`
    /// §1): launched/admitted, then one attempt abandoned before
    /// launch, then a retry. The abandoned attempt decided nothing, so
    /// it must not erase what attempt 1 decided — and in particular
    /// must not swap an operator's explicit harness for the hardcoded
    /// default. Driven through the exact same pair of calls
    /// `run_command` makes, with the input `prior_launch_for_waypoint`
    /// now produces for that lineage.
    #[test]
    fn an_intervening_unlaunched_attempt_does_not_erase_the_lineages_admitted_selection() {
        let prior = Some((
            wirk_core::ActorKind("codex".to_string()),
            wirk_core::ActorSelection {
                model: Some("gpt-5-codex".to_string()),
                effort: Some("high".to_string()),
                args: vec![],
            },
        ));
        let (effective, origin) = resolve_selection_source(None, prior);
        assert_eq!(origin, SelectionOrigin::PriorRun);
        let (kind, selection, _) = resolve_launch_selection(None, None, None, effective, origin);
        assert_eq!(
            kind,
            wirk_core::ActorKind("codex".to_string()),
            "attempt 3 must not launch the harness default in place of the operator's codex"
        );
        assert_eq!(selection.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(selection.effort.as_deref(), Some("high"));
    }

    /// The other half of the same rule: an explicit same-harness
    /// override that is itself admitted becomes the lineage's operative
    /// choice, so a later retry carries the override, not the older
    /// admission it replaced. (Which of two admissions wins is decided
    /// by `prior_launch_for_waypoint`'s newest-first walk; this pins
    /// what `run_command` then does with the answer.)
    #[test]
    fn a_later_admitted_override_is_what_a_further_retry_carries() {
        let prior = Some((
            wirk_core::ActorKind("codex".to_string()),
            wirk_core::ActorSelection {
                model: Some("gpt-5".to_string()),
                effort: Some("low".to_string()),
                args: vec![],
            },
        ));
        // The Waypoint's Route authored something else entirely; the
        // admitted override still governs (ruling 0160: "an override
        // actually admitted becomes the choice carried on later
        // retries").
        let authored = Some(wirk_core::AuthoredSelection {
            harness: Some(wirk_core::ActorKind("codex".to_string())),
            model: Some("gpt-5-codex".to_string()),
            effort: Some("high".to_string()),
            args: vec![],
        });
        let (effective, origin) = resolve_selection_source(authored, prior);
        assert_eq!(origin, SelectionOrigin::PriorRun);
        let (_, selection, _) = resolve_launch_selection(None, None, None, effective, origin);
        assert_eq!(selection.model.as_deref(), Some("gpt-5"));
        assert_eq!(selection.effort.as_deref(), Some("low"));
    }
}
