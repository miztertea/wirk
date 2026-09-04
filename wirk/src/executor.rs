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
}

/// Calls wirkd's `status` verb and parses its reply into `StatusReply`
/// — the one parse both `fetch_open_run` (setup: needs the World too)
/// and `WirkdRunLoopApi::status` (the ongoing poll: needs only Run
/// states) read from.
fn fetch_status(socket: &Path, work_id: &WorkId) -> Result<StatusReply, ExecutorError> {
    let reply = wirkd::client::call(
        socket,
        &Request::status(StatusPayload {
            work_id: work_id.clone(),
        }),
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
fn fetch_open_run(socket: &Path, work_id: &WorkId) -> Result<(Run, World), ExecutorError> {
    let parsed = fetch_status(socket, work_id)?;
    let entry = parsed
        .runs
        .into_iter()
        .find(|entry| matches!(entry.run.state, RunState::Open))
        .ok_or_else(|| ExecutorError::Wirkd(format!("no open Run for work {}", work_id.0)))?;
    let world = entry.world.ok_or_else(|| {
        ExecutorError::Wirkd("the open Run's Waypoint has no reserved World".to_string())
    })?;
    Ok((entry.run, world))
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
            WatchPayload {
                work_id: work_id.clone(),
            },
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
pub fn run_command(rest: &[String]) -> ExitCode {
    let actor_kind = match parse_actor_kind(rest) {
        Ok(kind) => kind,
        Err(()) => {
            eprintln!("wirk run: --actor-kind must be claude or opencode");
            return run_usage();
        }
    };
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

    let (mut run, world) = match fetch_open_run(&pointer.socket, &work_id) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("wirk run: {err}");
            return ExitCode::from(2);
        }
    };
    // `--actor-kind` is this invocation's own choice, not something
    // wirkd's `status` reply carries (`RunOpened` is journaled at
    // submit, before any `wirk run` flag is known — orient/actor.md
    // §2); `run.kind` is unhashed (`WorldHash::of` never covers it), so
    // overriding it here is exactly the same kind of local mechanism
    // update `run.rs`'s own `worktree_path` re-emission is. `RunLoop`'s
    // `launch` journals the real kind used via `RunLaunched` below.
    run.kind = actor_kind;
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

    if let Err(err) = wirkd_record(
        &pointer.socket,
        &work_id,
        Some(run.id.clone()),
        EventKind::WorktreeCreated {
            repo: actor.repository.clone(),
            base_sha: head,
        },
    ) {
        eprintln!("wirk run: {err}");
        return ExitCode::from(2);
    }
    println!("WorktreeCreated");

    // Update the World's worktree_path (R1: re-emit the same
    // WaypointReserved rather than a new event type — `mod.rs`'s
    // `RecordPayload` doc comment, `server.rs`'s `world_for_waypoint`).
    // `worktree_path` is excluded from `WorldHash::of`'s covered fields
    // (`wirk-core/src/lib.rs`), so the hash is unchanged.
    let mut updated_actor = actor.clone();
    updated_actor.worktree_path = worktree_path;
    let updated_world = World::Actor(updated_actor);
    let world_hash = WorldHash::of(&updated_world);
    if let Err(err) = wirkd_record(
        &pointer.socket,
        &work_id,
        None,
        EventKind::WaypointReserved {
            waypoint: run.waypoint.clone(),
            world_hash,
            world: updated_world.clone(),
        },
    ) {
        eprintln!("wirk run: {err}");
        return ExitCode::from(2);
    }

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
        "usage: wirk run --estate <root> --work <id> --session <name> [--herdr-socket <path>] [--actor-kind claude|opencode]"
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

/// `--actor-kind claude|opencode` (0041 D129), default `claude` when
/// the flag is absent; any other value is refused (`Err(())`) so
/// `run_command` can turn it into the usage exit without inventing a
/// silent fallback (AGENTS.md's "a defect with a standard answer is not
/// J0" — here the standard answer is refuse, not guess).
fn parse_actor_kind(rest: &[String]) -> Result<wirk_core::ActorKind, ()> {
    match flag_value(rest, "--actor-kind").as_deref() {
        None => Ok(wirk_core::ActorKind::Claude),
        Some("claude") => Ok(wirk_core::ActorKind::Claude),
        Some("opencode") => Ok(wirk_core::ActorKind::Opencode),
        Some(_) => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_kind_defaults_to_claude_when_absent() {
        assert_eq!(parse_actor_kind(&[]), Ok(wirk_core::ActorKind::Claude));
    }

    #[test]
    fn actor_kind_opencode_selects_opencode() {
        let rest = vec!["--actor-kind".to_string(), "opencode".to_string()];
        assert_eq!(parse_actor_kind(&rest), Ok(wirk_core::ActorKind::Opencode));
    }

    #[test]
    fn actor_kind_claude_selects_claude() {
        let rest = vec!["--actor-kind".to_string(), "claude".to_string()];
        assert_eq!(parse_actor_kind(&rest), Ok(wirk_core::ActorKind::Claude));
    }

    #[test]
    fn actor_kind_bogus_is_refused() {
        let rest = vec!["--actor-kind".to_string(), "bogus".to_string()];
        assert_eq!(parse_actor_kind(&rest), Err(()));
    }
}
