//! `wirk browser view|serve`: a browser view of a Work — what it is for,
//! how far it has got, what needs attention, the World it was given, the
//! evidence its Claims rest on — and, for `serve`, one typed return to
//! the Herdr pane running it.
//!
//! Everything shown comes from projections wirk already publishes:
//! `wirkd::client::status` (what `wirk work status` prints), the
//! `world_show` reply (`wirk world show`), and the `work_artifact` reply
//! (`wirk artifact read --estate/--work`). This module opens no new read
//! path into the estate and applies no scope rule of its own; it asks
//! those verbs under the scope the caller resolved and renders what they
//! answer, including their refusals.
//!
//! Two subcommands:
//!
//! - `wirk browser view --estate <root> [--work <id>] [--requesting-work
//!   <id> | --admin] --out <path.html>`: one self-contained HTML file.
//!   With `--work`, that Work; without it, the estate map — which is the
//!   administrative listing, so a scoped caller gets its own Work or an
//!   explicit refusal, never a walk of every Work id.
//! - `wirk browser serve --estate <root> --work <id> [--requesting-work
//!   <id> | --admin] [--open] [--idle-timeout <secs>]`: a loopback HTTP
//!   bridge serving that Work, re-read from wirkd on every request, with
//!   links into its World and its evidence and one "focus in Herdr"
//!   action.
//!
//! What `serve` exposes, and what it does not:
//!
//! - It binds `127.0.0.1` on an ephemeral port and registers no URL
//!   scheme; nothing outside this machine can reach it.
//! - Every request must carry the server's own random path token (16
//!   bytes from `/dev/urandom`). Anything else is `403` with an empty
//!   body, before any estate read happens.
//! - Reads from an accepted connection are bounded in both bytes and
//!   time, by one deadline covering the whole request, so a client that
//!   connects and then says nothing — or one that keeps sending without
//!   ever finishing — occupies the server for a bounded interval and
//!   cannot hold off the idle timeout indefinitely.
//! - The only mutating route, `POST /<token>/action/focus`, reads no
//!   request body, header or query. It re-reads this server's own pinned
//!   Work, re-reads Herdr's current agent list, and runs `herdr agent
//!   focus <pane_id>` as a fixed argv with no shell, where `pane_id` is
//!   Herdr's own answer and is additionally checked against an
//!   allow-list before use.
//! - Text that came out of the estate (a Work's intent, a failure
//!   detail, an artifact's bytes) is HTML-escaped wherever it is
//!   written, and is never used to build a link target: the only links
//!   this page emits point at routes under its own token, addressed by
//!   values that matched an allow-list.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wirk::wirkd;
use wirk_core::{ExecutionTriple, RunId, WorkId};
use wirkd::{Reply, Request, StatusPayload, WorkArtifactPayload, WorldShowPayload};

use crate::{TRIPLE_VARS, flag_value, list_work_ids, resolve_scope, verify_claimed_bytes};

/// How long a single accepted connection may spend waiting for the
/// bytes of its request, counted once from the moment it is accepted,
/// and how many of those bytes are read at all. Both are needed, and the
/// deadline has to be a total: `set_read_timeout` bounds each individual
/// read, so a client sending one byte at a time renews it forever and
/// spends none of its budget. Root's probe held a connection 6.505s that
/// way, and this server held one 25.1s. Every read below is given only
/// the time still left of this, so the budget is spent, never reset.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: u64 = 8 * 1024;
const MAX_REQUEST_LINE: usize = 2 * 1024;

/// How much of an artifact's bytes the content page shows before it
/// stops and says how much it did not show.
const MAX_ARTIFACT_PREVIEW: usize = 256 * 1024;

pub fn browser_command(rest: &[String]) -> ExitCode {
    match rest.first().map(String::as_str) {
        Some("view") => view_command(&rest[1..]),
        Some("serve") => serve_command(&rest[1..]),
        _ => browser_usage(),
    }
}

fn browser_usage() -> ExitCode {
    eprintln!(
        "usage: wirk browser view --estate <root> [--work <id>] \
         [--requesting-work <id> | --admin] --out <path.html> | \
         wirk browser serve --estate <root> --work <id> \
         [--requesting-work <id> | --admin] [--open] [--idle-timeout <secs>]"
    );
    ExitCode::from(1)
}

// ---- the scope this process asks under, held for the whole run --------

/// The resolved scope, carried rather than re-derived per request so
/// that every route — page, World, artifact, action — asks wirkd the
/// same question the caller was admitted to ask.
struct Scope {
    estate: String,
    /// `Some(requester)` is the scoped read as that Work; `None` is the
    /// administrative read. Never silently turned into the other one.
    requesting: Option<WorkId>,
}

impl Scope {
    fn status_payload(&self, work_id: &str) -> StatusPayload {
        match &self.requesting {
            Some(requester) => {
                StatusPayload::scoped(WorkId(work_id.to_string()), requester.clone())
            }
            None => StatusPayload::admin(WorkId(work_id.to_string())),
        }
    }

    fn is_administrative(&self) -> bool {
        self.requesting.is_none()
    }

    fn label(&self) -> String {
        match &self.requesting {
            Some(requester) => format!("read as Work {}", requester.0),
            None => "administrative read".to_string(),
        }
    }
}

fn fetch_status(scope: &Scope, work_id: &str) -> Result<serde_json::Value, String> {
    let pointer = wirkd::client::locate(Path::new(&scope.estate)).map_err(|err| err.to_string())?;
    match wirkd::client::status(&pointer.socket, scope.status_payload(work_id)) {
        Ok(Reply::Ok { result, .. }) => Ok(result),
        Ok(Reply::Err { error, .. }) => Err(format!("{}: {}", error.code, error.message)),
        Err(err) => Err(err.to_string()),
    }
}

/// The estate map's rows. Only an administrative caller reaches this:
/// walking `works/` to learn which Work ids exist is itself a
/// disclosure, and a scoped caller is admitted to its own lineage, not
/// to the estate's inventory. A scoped caller asking for "no particular
/// Work" is answered by its own Work or refused by name — it is never
/// handed a list of ids it could not have read.
fn fetch_estate(scope: &Scope) -> Result<Vec<(String, serde_json::Value)>, String> {
    if !scope.is_administrative() {
        return Err(
            "the estate map is the administrative listing; a scoped read answers about one \
             named Work"
                .to_string(),
        );
    }
    let ids = list_work_ids(Path::new(&scope.estate))?;
    let mut rows = Vec::with_capacity(ids.len());
    for id in ids {
        // One Work's read failing does not blank the map; it becomes its
        // own explicit row.
        let row = match fetch_status(scope, &id) {
            Ok(result) => result,
            Err(reason) => serde_json::json!({ "__unreadable__": reason }),
        };
        rows.push((id, row));
    }
    Ok(rows)
}

// ---- `wirk browser view` ---------------------------------------------

fn view_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return browser_usage();
    };
    let Some(out) = flag_value(rest, "--out") else {
        return browser_usage();
    };
    let admin = rest.iter().any(|a| a == "--admin");
    let requesting = flag_value(rest, "--requesting-work");
    let resolved = match resolve_scope("wirk browser view", &estate, requesting, admin) {
        Ok(resolved) => resolved,
        Err(refusal) => {
            eprintln!("wirk browser view: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &resolved.note {
        eprintln!("wirk browser view: {note}");
    }
    let target = flag_value(rest, "--work").or_else(|| resolved.default_target.clone());
    let scope = Scope {
        estate,
        requesting: resolved.requesting,
    };

    let html = match &target {
        Some(work_id) => match fetch_status(&scope, work_id) {
            Ok(result) => render_work_page(work_id, &result, &scope, None, None, true),
            Err(reason) => render_unavailable_page(
                &format!("Work {work_id}"),
                &format!("wirk could not answer for {work_id}"),
                &reason,
                None,
                scope.is_administrative(),
            ),
        },
        None => match fetch_estate(&scope) {
            Ok(rows) => render_estate_page(&rows, &scope, None),
            Err(reason) => render_unavailable_page(
                "Estate",
                "wirk could not produce the estate map",
                &reason,
                None,
                scope.is_administrative(),
            ),
        },
    };

    if let Err(err) = std::fs::write(&out, html) {
        eprintln!("wirk browser view: could not write {out}: {err}");
        return ExitCode::from(2);
    }
    println!("wirk browser view: wrote {out}");
    ExitCode::SUCCESS
}

// ---- `wirk browser serve` --------------------------------------------

/// The last status this server actually rendered, kept so that a later
/// failed read can show what was true at a named moment instead of an
/// empty page — labeled with its own capture time and never presented as
/// current.
struct LastKnown {
    result: serde_json::Value,
    at: i64,
}

fn serve_command(rest: &[String]) -> ExitCode {
    let Some(estate) = flag_value(rest, "--estate") else {
        return browser_usage();
    };
    let Some(work_id) = flag_value(rest, "--work") else {
        return browser_usage();
    };
    let admin = rest.iter().any(|a| a == "--admin");
    let requesting = flag_value(rest, "--requesting-work");
    let open = rest.iter().any(|a| a == "--open");
    let idle_timeout = flag_value(rest, "--idle-timeout")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(900);

    let resolved = match resolve_scope("wirk browser serve", &estate, requesting, admin) {
        Ok(resolved) => resolved,
        Err(refusal) => {
            eprintln!("wirk browser serve: {refusal}");
            return ExitCode::from(1);
        }
    };
    if let Some(note) = &resolved.note {
        eprintln!("wirk browser serve: {note}");
    }
    let scope = Scope {
        estate,
        requesting: resolved.requesting,
    };

    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("wirk browser serve: could not bind a loopback port: {err}");
            return ExitCode::from(2);
        }
    };
    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("wirk browser serve: {err}");
        return ExitCode::from(2);
    }
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => {
            eprintln!("wirk browser serve: {err}");
            return ExitCode::from(2);
        }
    };
    let token = match random_token() {
        Ok(token) => token,
        Err(err) => {
            eprintln!("wirk browser serve: could not generate a request token: {err}");
            return ExitCode::from(2);
        }
    };
    let url = format!("http://127.0.0.1:{port}/{token}/");
    println!("wirk browser serve: {url}");
    println!(
        "wirk browser serve: loopback only; token in the path; {}; idle-timeout {idle_timeout}s; \
         Ctrl-C to stop",
        scope.label()
    );

    if open {
        // The same outbound launch a terminal link click would make: the
        // operator's already-configured browser, opened once. No scheme
        // is registered and nothing is installed.
        if let Err(err) = Command::new("xdg-open").arg(&url).spawn() {
            eprintln!("wirk browser serve: --open: xdg-open failed: {err}");
        }
    }

    let mut last_known: Option<LastKnown> = None;
    let mut last_activity = Instant::now();
    loop {
        if last_activity.elapsed() > Duration::from_secs(idle_timeout) {
            println!("wirk browser serve: idle-timeout reached; stopping");
            return ExitCode::SUCCESS;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                handle_connection(stream, &token, &scope, &work_id, &mut last_known);
                // Taken again after the exchange: a connection that took
                // its full bounded read budget is still activity, and
                // the idle clock measures silence, not service time.
                last_activity = Instant::now();
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                eprintln!("wirk browser serve: accept: {err}");
                return ExitCode::from(2);
            }
        }
    }
}

fn random_token() -> std::io::Result<String> {
    let mut file = std::fs::File::open("/dev/urandom")?;
    let mut bytes = [0u8; 16];
    file.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

// ---- request reading, bounded in bytes and in time --------------------

struct HttpRequest {
    method: String,
    path: String,
}

enum ReadOutcome {
    Request(HttpRequest),
    /// The peer sent nothing usable and the connection ends with no
    /// reply: a closed socket, or a request line that never arrived.
    Nothing,
    /// The peer is still notionally there but did not finish a request
    /// within this server's budget. Answered, then closed.
    TooSlow,
    TooLarge,
}

/// Reads one request line and its headers under a single wall-clock
/// deadline covering the whole request, and a hard byte cap. The body is
/// never read: no route here consumes one.
fn read_request(stream: &TcpStream) -> ReadOutcome {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let _ = stream.set_write_timeout(Some(RESPONSE_WRITE_TIMEOUT));
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    match read_line_by(&mut reader, stream, &mut line, deadline) {
        LineOutcome::Eof if line.is_empty() => return ReadOutcome::Nothing,
        LineOutcome::Eof | LineOutcome::Line => {}
        LineOutcome::Expired => return ReadOutcome::TooSlow,
        LineOutcome::Failed => return ReadOutcome::Nothing,
    }
    if line.len() > MAX_REQUEST_LINE {
        return ReadOutcome::TooLarge;
    }
    if !line.ends_with('\n') {
        // The cap was reached before the request line ended.
        return ReadOutcome::TooLarge;
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return ReadOutcome::Nothing;
    };
    let request = HttpRequest {
        method: method.to_string(),
        path: path.to_string(),
    };
    // Drain the headers to the blank line, still under the same cap and
    // the same deadline. Nothing read here is used for anything; it is
    // read only so the client sees a well-formed exchange.
    loop {
        let mut header = String::new();
        match read_line_by(&mut reader, stream, &mut header, deadline) {
            LineOutcome::Eof => break,
            LineOutcome::Line if header == "\r\n" || header == "\n" => break,
            LineOutcome::Line => continue,
            LineOutcome::Expired => return ReadOutcome::TooSlow,
            LineOutcome::Failed => break,
        }
    }
    ReadOutcome::Request(request)
}

enum LineOutcome {
    /// A line ending in `\n` was appended.
    Line,
    /// The peer closed, or the byte cap ended the stream, before a `\n`.
    /// Whatever arrived has been appended.
    Eof,
    /// The request deadline passed with the line unfinished.
    Expired,
    Failed,
}

/// `BufReader::read_line` cannot be given a deadline: it loops on
/// `fill_buf` until it sees a newline, and each of those reads gets the
/// socket's per-read timeout afresh. This reads the same line, but sets
/// the socket timeout to the time actually remaining before every read,
/// so a client that keeps sending without finishing still runs out.
fn read_line_by(
    reader: &mut BufReader<std::io::Take<&TcpStream>>,
    stream: &TcpStream,
    line: &mut String,
    deadline: Instant,
) -> LineOutcome {
    loop {
        let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|r| !r.is_zero())
        else {
            return LineOutcome::Expired;
        };
        // A zero timeout means "block forever" to the OS, so the floor
        // is one millisecond and the deadline check above owns expiry.
        if stream
            .set_read_timeout(Some(remaining.max(Duration::from_millis(1))))
            .is_err()
        {
            return LineOutcome::Failed;
        }
        let (chunk, done) = match reader.fill_buf() {
            Ok([]) => return LineOutcome::Eof,
            Ok(buf) => match buf.iter().position(|b| *b == b'\n') {
                Some(at) => (buf[..=at].to_vec(), true),
                None => (buf.to_vec(), false),
            },
            Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(ref err) if is_timeout(err) => return LineOutcome::Expired,
            Err(_) => return LineOutcome::Failed,
        };
        reader.consume(chunk.len());
        line.push_str(&String::from_utf8_lossy(&chunk));
        if done {
            return LineOutcome::Line;
        }
        if line.len() > MAX_REQUEST_LINE {
            // Nothing above this point needs more; stop reading and let
            // the caller answer 413 rather than keep buffering.
            return LineOutcome::Eof;
        }
    }
}

fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn write_response(mut stream: &TcpStream, status: &str, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn handle_connection(
    stream: TcpStream,
    token: &str,
    scope: &Scope,
    work_id: &str,
    last_known: &mut Option<LastKnown>,
) {
    let request = match read_request(&stream) {
        ReadOutcome::Request(request) => request,
        ReadOutcome::Nothing => return,
        ReadOutcome::TooSlow => {
            write_response(
                &stream,
                "408 Request Timeout",
                "text/plain; charset=utf-8",
                "",
            );
            return;
        }
        ReadOutcome::TooLarge => {
            write_response(
                &stream,
                "431 Request Header Fields Too Large",
                "text/plain; charset=utf-8",
                "",
            );
            return;
        }
    };

    let prefix = format!("/{token}/");
    // The one gate every route sits behind, checked before any estate
    // read: a wrong or missing token learns nothing at all.
    if !request.path.starts_with(&prefix) {
        write_response(&stream, "403 Forbidden", "text/plain; charset=utf-8", "");
        return;
    }
    // Only the path addresses a route. A query string is accepted and
    // discarded: no route here takes a parameter, so there is nothing
    // for one to select.
    let route = request.path[prefix.len()..]
        .split('?')
        .next()
        .unwrap_or("")
        .to_string();

    match (request.method.as_str(), route.as_str()) {
        ("GET", "" | "index.html") => {
            let html = work_page(scope, work_id, token, None, last_known);
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("POST", "action/focus") => {
            let outcome = do_focus_action(scope, work_id, None);
            let html = work_page(scope, work_id, token, Some(outcome), last_known);
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("POST", other) if other.starts_with("action/focus/") => {
            // Returning to one named stage's pane. The Run is checked
            // against this Work's own listed Runs inside the action,
            // under the same scope every other read here uses.
            let run = &other["action/focus/".len()..];
            let outcome = do_focus_action(scope, work_id, Some(run));
            let html = world_page_with(scope, work_id, token, &Stage::parse(run), Some(&outcome));
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("GET", "world") => {
            let html = world_page(scope, work_id, token, &Stage::CURRENT);
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("GET", "estate") => {
            let html = match fetch_estate(scope) {
                Ok(rows) => render_estate_page(&rows, scope, Some(token)),
                Err(reason) => render_unavailable_page(
                    "Estate",
                    "wirk could not produce the estate map",
                    &reason,
                    Some(token),
                    scope.is_administrative(),
                ),
            };
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("GET", other) if other.starts_with("work/") => {
            // Reached from the estate map, which only an administrative
            // read produces. That read is already admitted to every
            // Work here, so following one of its rows discloses nothing
            // the map did not. A scoped server never renders this map,
            // so it never offers this route either.
            let id = &other["work/".len()..];
            if !scope.is_administrative() || !is_safe_work_id(id) {
                write_response(&stream, "404 Not Found", "text/plain; charset=utf-8", "");
                return;
            }
            let html = match fetch_status(scope, id) {
                Ok(result) => render_work_page(id, &result, scope, Some(token), None, false),
                Err(reason) => render_unavailable_page(
                    &format!("Work {id}"),
                    &format!("wirk could not answer for {id}"),
                    &reason,
                    Some(token),
                    scope.is_administrative(),
                )
                .replace(LAST_KNOWN_SLOT, ""),
            };
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("GET", other) if other.starts_with("world/") => {
            let html = world_page(
                scope,
                work_id,
                token,
                &Stage::parse(&other["world/".len()..]),
            );
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("GET", other) if other.starts_with("source/") => {
            let html = source_page(scope, work_id, token, &other["source/".len()..]);
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        ("GET", other) if other.starts_with("evidence/") => {
            let html = evidence_page(scope, work_id, token, &other["evidence/".len()..]);
            write_response(&stream, "200 OK", "text/html; charset=utf-8", &html);
        }
        _ => {
            write_response(&stream, "404 Not Found", "text/plain; charset=utf-8", "");
        }
    }
}

/// The Work page, and the honest failure that replaces it when wirkd
/// cannot be reached. A successful read replaces `last_known`; a failed
/// one shows what `last_known` holds, dated, under a heading that says
/// it is not current.
fn work_page(
    scope: &Scope,
    work_id: &str,
    token: &str,
    focus: Option<FocusOutcome>,
    last_known: &mut Option<LastKnown>,
) -> String {
    match fetch_status(scope, work_id) {
        Ok(result) => {
            *last_known = Some(LastKnown {
                result: result.clone(),
                at: unix_now(),
            });
            render_work_page(work_id, &result, scope, Some(token), focus.as_ref(), true)
        }
        Err(reason) => render_unavailable_page(
            &format!("Work {work_id}"),
            &format!("wirk could not answer for {work_id} just now"),
            &reason,
            Some(token),
            scope.is_administrative(),
        )
        .replace(
            LAST_KNOWN_SLOT,
            &match last_known.as_ref() {
                Some(previous) => render_last_known(work_id, previous),
                None => String::new(),
            },
        ),
    }
}

// ---- the typed return to Herdr ---------------------------------------

struct FocusOutcome {
    ok: bool,
    message: String,
}

/// Strict allow-list for a pane id before it becomes an argv element.
/// A pane id is never taken from a request — it is Herdr's own answer —
/// but it is checked anyway, so an unexpected shape is refused rather
/// than passed along.
fn is_safe_pane_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'))
}

struct HerdrAgent {
    pane_id: String,
    status: Option<String>,
}

/// Asks Herdr's own CLI which agents it is running now, and returns the
/// one whose name is this Run's id. Fixed argv, no shell, nothing from a
/// request.
fn find_agent_for_run(run_id: &str) -> Result<Option<HerdrAgent>, String> {
    let output = Command::new("herdr")
        .args(["agent", "list"])
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "herdr agent list exited {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())?;
    let empty = Vec::new();
    for agent in value["result"]["agents"].as_array().unwrap_or(&empty) {
        if agent["name"].as_str() != Some(run_id) {
            continue;
        }
        let Some(pane) = agent["pane_id"].as_str() else {
            continue;
        };
        if !is_safe_pane_id(pane) {
            return Err(format!(
                "herdr reported an unexpected pane id shape: {pane:?}"
            ));
        }
        return Ok(Some(HerdrAgent {
            pane_id: pane.to_string(),
            status: agent["agent_status"].as_str().map(str::to_string),
        }));
    }
    Ok(None)
}

/// The one mutating route. Re-reads this server's own pinned Work under
/// its own scope, then re-reads Herdr's live agent list, then runs a
/// fixed two-argument command. Whether the Run has ended is reported,
/// not used to refuse: Herdr keeps a finished agent's pane, and looking
/// at what a completed Run left behind is a reasonable thing to want.
/// What decides the outcome is whether Herdr lists a pane for this Run
/// right now.
fn do_focus_action(scope: &Scope, work_id: &str, run: Option<&str>) -> FocusOutcome {
    let result = match fetch_status(scope, work_id) {
        Ok(result) => result,
        Err(reason) => {
            return FocusOutcome {
                ok: false,
                message: format!("could not re-read {work_id} before acting: {reason}"),
            };
        }
    };
    let run_id = match run {
        Some(asked) => {
            if !listed_runs(&result).iter().any(|id| id == asked) {
                return FocusOutcome {
                    ok: false,
                    message: format!(
                        "{asked} is not among the Runs wirk lists for {work_id}, so nothing was \
                         asked of Herdr"
                    ),
                };
            }
            asked.to_string()
        }
        None => {
            let Some(run_id) = current_run_id(&result) else {
                return FocusOutcome {
                    ok: false,
                    message: format!(
                        "{work_id} has no Run recorded yet, so there is no pane to focus"
                    ),
                };
            };
            run_id
        }
    };
    let run_state = if run.is_none() {
        result["run_state"]
            .as_str()
            .unwrap_or("unknown")
            .to_string()
    } else {
        // A named stage's own state, not the Work's current one.
        describe_run_state(
            &result["runs"]
                .as_array()
                .and_then(|runs| {
                    runs.iter()
                        .find(|entry| entry["run"]["id"].as_str() == Some(run_id.as_str()))
                })
                .map(|entry| entry["run"]["state"].clone())
                .unwrap_or(serde_json::Value::Null),
        )
    };
    match find_agent_for_run(&run_id) {
        Ok(Some(agent)) => {
            match Command::new("herdr")
                .args(["agent", "focus", &agent.pane_id])
                .status()
            {
                Ok(status) if status.success() => {
                    let running = agent.status.as_deref().unwrap_or("unreported");
                    FocusOutcome {
                        ok: true,
                        message: format!(
                            "Focused the pane running {run_id} (pane {}, Herdr reports it {running}; \
                             the Run itself is {run_state}).",
                            agent.pane_id
                        ),
                    }
                }
                Ok(status) => FocusOutcome {
                    ok: false,
                    message: format!(
                        "Herdr refused to focus pane {}: herdr agent focus exited {}",
                        agent.pane_id,
                        status.code().unwrap_or(-1)
                    ),
                },
                Err(err) => FocusOutcome {
                    ok: false,
                    message: format!("could not run herdr agent focus: {err}"),
                },
            }
        }
        Ok(None) => FocusOutcome {
            ok: false,
            message: format!(
                "Herdr is not listing a pane for {run_id} right now, so there is nothing to \
                 focus. The Run itself is {run_state}."
            ),
        },
        Err(reason) => FocusOutcome {
            ok: false,
            message: format!("could not read Herdr's current agent list: {reason}"),
        },
    }
}

// ---- reading the projections into human shape -------------------------

/// Which Run's delivered context a page is reading, and at which
/// revision of that Run's own projection chain.
///
/// A Work outlives its stages. Reading only the current Run means that
/// the moment a mixed Work advances past its oriented Actor stage, the
/// context that stage was actually given stops being reachable —
/// observed directly, and the reason this exists. wirkd already answers
/// `world_show` for any Run of a Work, so the whole of the change is
/// carrying *which* Run and *which* revision, instead of asking again
/// for "the current one" and hoping it has not moved.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stage {
    run: Option<String>,
    revision: Option<u64>,
}

impl Stage {
    const CURRENT: Stage = Stage {
        run: None,
        revision: None,
    };

    /// `world/…` and `source/…` tails: `<run>/<revision>`, `<run>`, or a
    /// bare `<revision>` for the current Run — the form the page emitted
    /// before a Run could be named, kept because it still means exactly
    /// what it always meant.
    fn parse(rest: &str) -> Stage {
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            return Stage::CURRENT;
        }
        let (head, tail) = match rest.split_once('/') {
            Some((head, tail)) => (head, Some(tail)),
            None => (rest, None),
        };
        if let Ok(revision) = head.parse::<u64>()
            && tail.is_none()
        {
            return Stage {
                run: None,
                revision: Some(revision),
            };
        }
        Stage {
            run: Some(head.to_string()),
            revision: tail.and_then(|tail| tail.parse::<u64>().ok()),
        }
    }

    /// How a stage addresses itself in a link, so that a link written
    /// now still names the same Run and the same revision when it is
    /// followed later, whatever the Work has done since.
    fn address(run: &str, revision: u64) -> String {
        format!("{run}/{revision}")
    }
}

/// Every Run this Work's own status projection lists, oldest first.
///
/// This is the admitted view: `fetch_status` already ran under the
/// scope the caller resolved, so a Run that is in this list is one this
/// reader is admitted to see. A Run id that arrives in a URL is checked
/// against it before it is used to address anything, for the same
/// reason a pane id is checked before it becomes an argv element.
fn listed_runs(result: &serde_json::Value) -> Vec<String> {
    let empty = Vec::new();
    result["runs"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|entry| entry["run"]["id"].as_str().map(str::to_string))
        .collect()
}

fn current_run_id(result: &serde_json::Value) -> Option<String> {
    if let Some(id) = result["run_id"].as_str() {
        return Some(id.to_string());
    }
    result["runs"]
        .as_array()
        .and_then(|runs| runs.last())
        .and_then(|run| run["run"]["id"].as_str())
        .map(str::to_string)
}

/// The Actor World a Run was given, if this reply carries one and it was
/// not withheld from this reader.
fn actor_world(result: &serde_json::Value) -> Option<&serde_json::Value> {
    result.get("world").and_then(|world| world.get("Actor"))
}

fn deterministic_world(result: &serde_json::Value) -> Option<&serde_json::Value> {
    result
        .get("world")
        .and_then(|world| world.get("Deterministic"))
}

/// A Deterministic stage has no checkout to describe and no intent to
/// read: what it is, is the command it runs, the commit it runs against,
/// and what it owes when it is done. That is the same projection the
/// Actor view reads, so it is shown the same way rather than omitted.
///
/// Its environment carries the execution triple plus whatever the Route
/// declared. The triple is identity this page already shows elsewhere;
/// everything else is named and its value withheld, because a Route may
/// put anything in there and this page has no way to tell a token from a
/// setting.
fn render_deterministic_world(world: &serde_json::Value) -> String {
    let empty = Vec::new();
    let mut out = String::from(
        "<p class=\"note\">A Deterministic stage: wirk runs a fixed command rather than \
         handing a checkout to an actor.</p><table>",
    );
    let mut row = |label: &str, value: String| {
        out.push_str(&format!("<tr><th>{label}</th><td>{value}</td></tr>"));
    };
    let command = world["command"].as_array().unwrap_or(&empty);
    if !command.is_empty() {
        // Shown as the argv it is — one list, not a shell line. Nothing
        // here is quoted into something runnable, because it is not run
        // from this page.
        row(
            "runs",
            format!(
                "<code>{}</code>",
                command
                    .iter()
                    .filter_map(|a| a.as_str())
                    .map(html_escape)
                    .collect::<Vec<_>>()
                    .join("</code> <code>")
            ),
        );
    }
    if let Some(base) = world["base_sha"].as_str().filter(|b| !b.is_empty()) {
        row(
            "against commit",
            format!("<code>{}</code>", html_escape(base)),
        );
    }
    if let Some(cwd) = world["cwd"].as_str().filter(|c| !c.is_empty()) {
        row("in", html_escape(cwd));
    }
    let outputs = world["expected_artifacts"].as_array().unwrap_or(&empty);
    if !outputs.is_empty() {
        row(
            "owes",
            outputs
                .iter()
                .map(|o| {
                    format!(
                        "<code>{}</code>{}",
                        html_escape(o["name"].as_str().unwrap_or("?")),
                        if o["required"].as_bool().unwrap_or(false) {
                            ""
                        } else {
                            " <span class=\"note\">(optional)</span>"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    out.push_str("</table>");
    if let Some(env) = world["env"].as_object() {
        let (triple, rest): (Vec<_>, Vec<_>) = env
            .iter()
            .partition(|(name, _)| TRIPLE_VARS.contains(&name.as_str()));
        if !triple.is_empty() {
            out.push_str("<table>");
            for (name, value) in triple {
                out.push_str(&format!(
                    "<tr><th>{}</th><td><code>{}</code></td></tr>",
                    html_escape(name),
                    html_escape(value.as_str().unwrap_or("")),
                ));
            }
            out.push_str("</table>");
        }
        if !rest.is_empty() {
            out.push_str(&format!(
                "<p class=\"note\">The Route also sets {}. Names only: this page cannot tell \
                 which of a Route's own variables carry secrets, so it shows none of their \
                 values.</p>",
                rest.iter()
                    .map(|(name, _)| format!("<code>{}</code>", html_escape(name)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    out
}

fn world_withheld(result: &serde_json::Value) -> bool {
    result
        .get("world")
        .is_some_and(|world| world.get("withheld").is_some())
}

/// The first heading or first sentence of a Work's stated intent: what
/// this Work is for, in the words it was admitted with.
fn purpose_line(result: &serde_json::Value) -> Option<String> {
    first_line_of(work_intent(result)?.as_str())
}

/// A heading's worth of the stated purpose.
///
/// An intent that opens with a Markdown heading gives one line and that
/// line is the answer. An intent written as a paragraph of instructions
/// does not: taking its whole first line put a four-line instruction in
/// an `<h1>`. The first sentence is what a heading can carry; the rest
/// of the intent is still on the page, in full, just below.
fn heading_of(text: &str) -> Option<String> {
    let line = first_line_of(text)?;
    let sentence = match line.find(". ") {
        Some(at) if at < HEADING_BUDGET => line[..=at].to_string(),
        _ => line.clone(),
    };
    if sentence.chars().count() <= HEADING_BUDGET {
        return Some(sentence);
    }
    // No sentence end inside the budget: cut on a word boundary and say
    // so, rather than running a paragraph across the top of the page.
    let cut: String = sentence.chars().take(HEADING_BUDGET).collect();
    let cut = match cut.rsplit_once(' ') {
        Some((head, _)) => head.to_string(),
        None => cut,
    };
    Some(format!("{cut}\u{2026}"))
}

/// How much of a stated purpose a heading can carry and stay a heading.
const HEADING_BUDGET: usize = 80;

/// What this Work is for, in the words it was admitted with.
///
/// The current Run's own World is the first place to look, and for a
/// single-stage Actor Work it is the only one. A mixed Work that has
/// advanced to a Deterministic stage has no intent on its *current*
/// World at all — which is how a Work's page came to be headed by its
/// own hash. The intent is still there, on the stage that was given
/// one, and every Run's own World is already in this same reply.
fn work_intent(result: &serde_json::Value) -> Option<String> {
    if let Some(intent) = actor_world(result)
        .and_then(|world| world.get("intent"))
        .and_then(|intent| intent.as_str())
    {
        return Some(intent.to_string());
    }
    let empty = Vec::new();
    result["runs"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .find_map(|entry| {
            entry["world"]
                .get("Actor")?
                .get("intent")?
                .as_str()
                .map(str::to_string)
        })
}

fn first_line_of(text: &str) -> Option<String> {
    let first = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(first.trim_start_matches('#').trim().to_string())
}

fn state_sentence(result: &serde_json::Value) -> String {
    let state = result["state"].as_str().unwrap_or("in an unreported state");
    match result["current_waypoint"].as_str() {
        Some(waypoint) => format!("This Work is {state}, at its {waypoint} waypoint."),
        None => format!("This Work is {state}."),
    }
}

/// What a person should look at, in order. Each entry is something the
/// projection actually says, not a judgement this renderer invented.
fn attention(result: &serde_json::Value) -> Vec<(&'static str, String)> {
    let mut items = Vec::new();
    if let Some(held) = result.get("held") {
        let missing = held["missing"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        items.push((
            "waiting",
            format!(
                "Held at {} (attempt {}): it has not produced {}.",
                held["waypoint"].as_str().unwrap_or("its current waypoint"),
                held["attempt"].as_u64().unwrap_or(1),
                if missing.is_empty() {
                    "its required outputs".to_string()
                } else {
                    missing
                }
            ),
        ));
    }
    if let Some(cause) = result.get("needs_input") {
        let detail = match &cause["detail"] {
            detail if detail["withheld"] == true => "(the detail is not disclosed to this reader)",
            detail => detail.as_str().unwrap_or("(no detail recorded)"),
        };
        items.push((
            "waiting",
            format!(
                "Waiting for input: {} {detail}",
                cause["reason"].as_str().unwrap_or("reason unrecorded")
            ),
        ));
    }
    if result["run_state"].as_str() == Some("failed") {
        let detail = result["failure_detail"]
            .as_str()
            .or_else(|| result["failure_status"].as_str())
            .unwrap_or("no detail recorded");
        items.push(("failed", format!("The current Run failed: {detail}")));
    }
    if result["run_state"].as_str() == Some("vanished") {
        items.push((
            "failed",
            "The current Run vanished: its process is gone without a Claim.".to_string(),
        ));
    }
    let empty = Vec::new();
    let unavailable: Vec<String> = result["evidence"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .flat_map(|entry| entry["artifacts"].as_array().cloned().unwrap_or_default())
        .filter(|artifact| !artifact["available"].as_bool().unwrap_or(false))
        .filter_map(|artifact| artifact["name"].as_str().map(str::to_string))
        .collect();
    if !unavailable.is_empty() {
        items.push((
            "failed",
            format!(
                "Evidence a validated Claim rests on is no longer readable at the content it \
                 was checked against: {}.",
                unavailable.join(", ")
            ),
        ));
    }
    items
}

// ---- rendering --------------------------------------------------------

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A value is safe to place in a link this page builds only if it is a
/// plain path segment. Ids and output names that do not match are shown
/// as text and are not linked — this page never constructs a URL out of
/// a string it did not check.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A Work id as this estate writes them: the same plain-segment rule
/// every other addressable value on these pages has to pass.
fn is_safe_work_id(id: &str) -> bool {
    is_safe_segment(id)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// `2026-09-15 02:31:09 UTC` from unix seconds. Civil-from-days, so the
/// page can put a real time on a snapshot without a date dependency.
fn fmt_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days, shifted to an era beginning
    // 0000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn describe_age(seconds: i64) -> String {
    match seconds {
        s if s < 0 => "in the future (this machine's clock moved)".to_string(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s => format!("{}h {}m ago", s / 3600, (s % 3600) / 60),
    }
}

const STYLE: &str = "\
body{font-family:system-ui,-apple-system,Segoe UI,sans-serif;max-width:62rem;margin:0 auto;\
padding:2rem 1.25rem 4rem;color:#17181a;background:#fff;line-height:1.5}\
h1{font-size:1.5rem;margin:0 0 .25rem}h2{font-size:1.05rem;margin:2rem 0 .5rem;\
border-bottom:1px solid #e3e5e8;padding-bottom:.3rem}\
.lede{font-size:1.05rem;margin:.25rem 0 1rem}\
.note{color:#5d6470;font-size:.85rem}\
a{color:#1550a8}\
table{border-collapse:collapse;width:100%;margin:.5rem 0;font-size:.9rem}\
td,th{border-bottom:1px solid #e3e5e8;padding:.4rem .5rem;text-align:left;vertical-align:top}\
th{font-weight:600;color:#5d6470;font-size:.8rem;text-transform:uppercase;letter-spacing:.03em}\
.card{border:1px solid #e3e5e8;border-left-width:4px;border-radius:4px;padding:.6rem .85rem;\
margin:.5rem 0}\
.waiting{border-left-color:#b8860b;background:#fdf7e8}\
.failed{border-left-color:#a3231b;background:#fbeceb}\
.ok{border-left-color:#1a7f37;background:#eaf6ec}\
.stale{border-left-color:#5d6470;background:#f3f4f6}\
.good{color:#1a7f37}.bad{color:#a3231b}.dim{color:#5d6470}\
pre{background:#f6f7f9;border:1px solid #e3e5e8;border-radius:4px;padding:.75rem;\
overflow-x:auto;font-size:.82rem;line-height:1.45;white-space:pre-wrap;word-break:break-word}\
form{display:inline}button{font:inherit;padding:.4rem .85rem;cursor:pointer;border-radius:4px;\
border:1px solid #9aa0a8;background:#f6f7f9}\
code{font-size:.85em;background:#f0f1f3;padding:.05rem .25rem;border-radius:3px}\
details{margin:.5rem 0;font-size:.85rem}summary{cursor:pointer;color:#5d6470}\
.item{border:1px solid #e3e5e8;border-radius:4px;padding:.6rem .85rem;margin:.6rem 0}\
.where{font-size:.9rem;font-weight:600;margin:0 0 .15rem;word-break:break-all}\
.why{color:#5d6470;font-size:.85rem;margin:.1rem 0 .4rem}\
.excerpt{background:#f6f7f9;border:1px solid #e3e5e8;border-radius:4px;padding:.5rem .65rem;\
margin:.35rem 0;font-size:.85rem;white-space:pre-wrap;word-break:break-word;\
font-family:ui-monospace,SFMono-Regular,Menlo,monospace}\
.tag{display:inline-block;font-size:.72rem;text-transform:uppercase;letter-spacing:.04em;\
background:#f0f1f3;color:#5d6470;border-radius:3px;padding:.05rem .35rem;margin-right:.3rem}\
footer{margin-top:2.5rem;border-top:1px solid #e3e5e8;padding-top:.75rem}";

/// Where `work_page` splices last-known content into a failure page.
const LAST_KNOWN_SLOT: &str = "<!--last-known-->";

/// Whether this process's scope can produce the estate map at all. The
/// nav offers that link only when it can: a control that is always
/// going to refuse is not a control.
fn page(title: &str, body: String, token: Option<&str>, estate_map: bool) -> String {
    let nav = match token {
        Some(token) => format!(
            "<p class=\"note\"><a href=\"/{token}/\">Work</a> &middot; \
             <a href=\"/{token}/world\">World</a>{}</p>",
            if estate_map {
                format!(" &middot; <a href=\"/{token}/estate\">Estate</a>")
            } else {
                String::new()
            }
        ),
        None => String::new(),
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{}</title><style>{STYLE}</style></head><body>{nav}{body}\
         <footer><p class=\"note\">Read from wirk's own projections at {}. \
         This page is what those answered at that moment; it does not update on its own. \
         Reload to ask again.</p></footer></body></html>",
        html_escape(title),
        html_escape(&fmt_utc(unix_now())),
    )
}

fn render_unavailable_page(
    title: &str,
    headline: &str,
    reason: &str,
    token: Option<&str>,
    estate_map: bool,
) -> String {
    page(
        title,
        format!(
            "<h1>{}</h1><div class=\"card failed\"><p>{}</p></div>\
             <p class=\"note\">This is the read failing, reported as a failure. Nothing older \
             is being shown in its place as though it were current.</p>{LAST_KNOWN_SLOT}",
            html_escape(headline),
            html_escape(reason),
        ),
        token,
        estate_map,
    )
}

fn render_last_known(work_id: &str, previous: &LastKnown) -> String {
    let age = describe_age(unix_now() - previous.at);
    format!(
        "<h2>Last answer this page received</h2>\
         <div class=\"card stale\"><p><b>Not current.</b> This is what wirk said about {} at {} \
         ({age}), kept only so the failure above has context. It may have changed since.</p>\
         <p>{}</p></div>",
        html_escape(work_id),
        html_escape(&fmt_utc(previous.at)),
        html_escape(&state_sentence(&previous.result)),
    )
}

/// `pinned` is whether this is the Work the server was started on. The
/// return action belongs to that Work alone; a Work reached from the
/// estate map is shown, and says how to open its own bridge, rather
/// than offering a button that would act somewhere else.
fn render_work_page(
    work_id: &str,
    result: &serde_json::Value,
    scope: &Scope,
    token: Option<&str>,
    focus: Option<&FocusOutcome>,
    pinned: bool,
) -> String {
    let intent = work_intent(result);
    let heading = intent
        .as_deref()
        .and_then(heading_of)
        .unwrap_or_else(|| format!("Work {work_id}"));
    let mut body = format!("<h1>{}</h1>", html_escape(&heading));
    body.push_str(&format!(
        "<p class=\"lede\">{}</p>",
        html_escape(&state_sentence(result))
    ));
    // The whole of what was asked, in the words it was asked, under the
    // heading that is only its opening.
    if let Some(intent) = intent.as_deref()
        && intent.trim() != heading.trim()
    {
        body.push_str(&format!(
            "<details><summary>What this Work was asked for, in full</summary><pre>{}</pre>\
             </details>",
            html_escape(intent.trim())
        ));
    }
    body.push_str(&format!(
        "<details><summary>Identifiers and read scope</summary><table>\
         <tr><th>Work</th><td><code>{}</code></td></tr>\
         <tr><th>read as</th><td>{}</td></tr>\
         <tr><th>recorded events</th><td>{}</td></tr></table></details>",
        html_escape(work_id),
        html_escape(&scope.label()),
        result["events"].as_u64().unwrap_or(0),
    ));

    // What needs attention, before anything a reader would have to dig for.
    let attention = attention(result);
    if attention.is_empty() {
        let has_evidence = result["evidence"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty());
        body.push_str(&format!(
            "<div class=\"card ok\"><p>Nothing here is waiting on a person: no hold, no \
             request for input and no failed Run{}.</p></div>",
            if has_evidence {
                ", and every artifact a validated Claim rests on still reads at the content it \
                 was checked against"
            } else {
                ""
            }
        ));
    } else {
        for (kind, text) in &attention {
            body.push_str(&format!(
                "<div class=\"card {kind}\"><p>{}</p></div>",
                html_escape(text)
            ));
        }
    }

    body.push_str(&render_progress(result, token));
    body.push_str(&render_world_summary(result, token));
    body.push_str(&render_evidence(result, token));

    match (token, pinned) {
        (Some(token), true) => body.push_str(&render_return(result, token, focus)),
        (Some(_), false) => body.push_str(&format!(
            "<h2>Return to Herdr</h2><p>This page is showing {} from the estate map. The \
             return action belongs to the Work this server was started on. To get a button \
             that focuses this one, run <code>wirk browser serve --estate &lt;root&gt; --work \
             {}</code>.</p>",
            html_escape(work_id),
            html_escape(work_id),
        )),
        (None, _) => {}
    }

    page(&heading, body, token, scope.is_administrative())
}

/// The Trail as progress: which waypoint each Run served, which attempt,
/// and where it got to. Ids stay, but they are the last column, not the
/// first thing a reader meets.
fn render_progress(result: &serde_json::Value, token: Option<&str>) -> String {
    let empty = Vec::new();
    let runs = result["runs"].as_array().unwrap_or(&empty);
    if runs.is_empty() {
        return "<h2>Progress</h2><p>This Work has been submitted but no Run has opened yet, \
                so there is nothing it has done.</p>"
            .to_string();
    }
    let current = current_run_id(result);
    let mut out = String::from(
        "<h2>Progress</h2><table><tr><th>stage</th><th>attempt</th><th>how it went</th>\
         <th>context</th></tr>",
    );
    for entry in runs {
        let run = &entry["run"];
        let id = run["id"].as_str().unwrap_or("?");
        let is_current = current.as_deref() == Some(id);
        let state = describe_run_state(&run["state"]);
        // Every stage, not only the current one, is one click from the
        // context it was actually given. Served pages only: a written
        // file has no server to answer for the other stages.
        let context = match token {
            Some(token) if id != "?" => format!(
                "<a href=\"/{token}/world/{}\">what it was given</a>",
                html_escape(id)
            ),
            _ => "<span class=\"dim\">&mdash;</span>".to_string(),
        };
        out.push_str(&format!(
            "<tr><td>{}{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_escape(run["waypoint"].as_str().unwrap_or("?")),
            if is_current {
                " <span class=\"note\">(current)</span>"
            } else {
                ""
            },
            run["attempt"].as_u64().unwrap_or(1),
            html_escape(&state),
            context,
        ));
    }
    out.push_str("</table>");
    out.push_str(&format!(
        "<details><summary>Run and Claim identifiers</summary><table>\
         <tr><th>stage</th><th>run</th><th>claim</th></tr>{}</table></details>",
        runs.iter()
            .map(|entry| format!(
                "<tr><td>{}</td><td><code>{}</code></td><td><code>{}</code></td></tr>",
                html_escape(entry["run"]["waypoint"].as_str().unwrap_or("?")),
                html_escape(entry["run"]["id"].as_str().unwrap_or("?")),
                html_escape(
                    entry["run"]["state"]["Claimed"]
                        .as_str()
                        .unwrap_or("\u{2014}")
                )
            ))
            .collect::<String>()
    ));
    out
}

fn describe_run_state(state: &serde_json::Value) -> String {
    if let Some(name) = state.as_str() {
        return match name {
            "Open" => "running or waiting".to_string(),
            "Vanished" => "vanished without a Claim".to_string(),
            other => other.to_lowercase(),
        };
    }
    if state.get("Claimed").and_then(|c| c.as_str()).is_some() {
        // The Claim id is identity, not outcome. It is beside this row
        // under "Run identifiers", where a reader who wants to quote it
        // can find it.
        return "claimed".to_string();
    }
    if let Some(cause) = state.get("Failed") {
        return format!(
            "failed: {}",
            cause["status"].as_str().unwrap_or("no status recorded")
        );
    }
    "unreported".to_string()
}

/// The World this Work was given, as a person would ask about it: which
/// repository at which commit, what it may write, what it owes. The
/// delivered orientation, if any, is a link — the document itself is on
/// its own page.
fn render_world_summary(result: &serde_json::Value, token: Option<&str>) -> String {
    let mut out = String::from("<h2>The World this Work was given</h2>");
    if world_withheld(result) {
        out.push_str(
            "<div class=\"card stale\"><p>This reader is not admitted to the World that was \
             reserved for this Work, so wirk withheld it. That is a scope answer, not a \
             missing World.</p></div>",
        );
        return out;
    }
    if let Some(world) = deterministic_world(result) {
        // What the stage is, in a sentence. The argv, the commit, the
        // working directory and the Route's own variables are what it is
        // made of, and they are one click away for a reader who wants
        // them — but they are not what a person came to the page for.
        let empty = Vec::new();
        let owes = world["expected_artifacts"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(|o| o["name"].as_str())
            .map(html_escape)
            .collect::<Vec<_>>();
        out.push_str(&format!(
            "<p>wirk runs a fixed command here rather than handing a checkout to an actor{}.</p>",
            if owes.is_empty() {
                String::new()
            } else {
                format!(
                    ", and it owes <code>{}</code>",
                    owes.join("</code>, <code>")
                )
            }
        ));
        out.push_str(&format!(
            "<details><summary>The command, the commit it runs against, and its \
             environment</summary>{}</details>",
            render_deterministic_world(world)
        ));
        return out;
    }
    let Some(world) = actor_world(result) else {
        out.push_str(
            "<p>No World is recorded on this Work's current Run: a Work with no open Run has \
             nothing reserved yet.</p>",
        );
        return out;
    };

    out.push_str("<table>");
    let mut row = |label: &str, value: String| {
        out.push_str(&format!("<tr><th>{label}</th><td>{value}</td></tr>"));
    };
    if let Some(repo) = world["repository"].as_str() {
        row("repository", html_escape(repo));
    }
    if let Some(branch) = world["branch"].as_str() {
        row("branch", format!("<code>{}</code>", html_escape(branch)));
    }
    if let Some(base) = world["base_sha"].as_str() {
        row("built from", format!("<code>{}</code>", html_escape(base)));
    }
    // An Actor World carries an empty checkout path until its Run
    // materializes one; an empty row would read as a missing checkout
    // rather than as one not yet made.
    if let Some(path) = world["worktree_path"].as_str().filter(|p| !p.is_empty()) {
        let present = result["runs"]
            .as_array()
            .and_then(|runs| runs.last())
            .and_then(|run| run["worktree_present"].as_bool());
        row(
            "checkout",
            format!(
                "{} &mdash; {}",
                html_escape(path),
                match present {
                    Some(true) => "<span class=\"good\">still on disk</span>".to_string(),
                    Some(false) =>
                        "<span class=\"bad\">no longer on disk; the path is what was reserved, \
                         not what is there now</span>"
                            .to_string(),
                    None => "<span class=\"dim\">presence not reported</span>".to_string(),
                }
            ),
        );
    }
    let empty = Vec::new();
    let boundary = world["boundary"].as_array().unwrap_or(&empty);
    if !boundary.is_empty() {
        row(
            "may write",
            boundary
                .iter()
                .filter_map(|p| p.as_str())
                .map(|p| format!("<code>{}</code>", html_escape(p)))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    let outputs = world["output_contract"].as_array().unwrap_or(&empty);
    if !outputs.is_empty() {
        row(
            "owes",
            outputs
                .iter()
                .map(|o| {
                    format!(
                        "<code>{}</code>{}",
                        html_escape(o["name"].as_str().unwrap_or("?")),
                        if o["required"].as_bool().unwrap_or(false) {
                            ""
                        } else {
                            " <span class=\"note\">(optional)</span>"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    out.push_str("</table>");

    // The delivered context, if the Waypoint asked for one.
    let chain = result["runs"]
        .as_array()
        .and_then(|runs| runs.last())
        .and_then(|run| run["orientation"].as_array())
        .cloned()
        .unwrap_or_default();
    if chain.is_empty() {
        out.push_str(
            "<p class=\"note\">This Waypoint asked for no assembled orientation, so no \
             context document was delivered with it.</p>",
        );
    } else {
        out.push_str(&format!(
            "<p>{} revision{} of assembled context {} delivered to this Run.",
            chain.len(),
            if chain.len() == 1 { "" } else { "s" },
            if chain.len() == 1 { "was" } else { "were" },
        ));
        if let Some(token) = token {
            out.push_str(&format!(
                " <a href=\"/{token}/world\">Read the delivered context</a>."
            ));
        } else {
            out.push_str(" Read it with <code>wirk world show</code> inside that Run.");
        }
        out.push_str("</p>");
    }

    if let Some(intent) = world["intent"].as_str() {
        out.push_str(&format!(
            "<h2>What it was asked to do</h2><pre>{}</pre>",
            html_escape(intent)
        ));
    }
    out
}

/// What the Work's validated Claims rest on, and whether those bytes
/// still read the way they did when the Claim was checked. Each readable
/// artifact links to its own content.
fn render_evidence(result: &serde_json::Value, token: Option<&str>) -> String {
    let mut out = String::from("<h2>Evidence</h2>");
    let empty = Vec::new();
    let entries = result["evidence"].as_array().unwrap_or(&empty);
    if entries.is_empty() {
        out.push_str(
            "<p>No validated Claim has been recorded for this Work yet, so there is no \
             evidence to follow. That is an absence, not a failure.</p>",
        );
        return out;
    }
    out.push_str(
        "<p class=\"note\">Each row is an artifact a validated Claim was checked against. \
         &ldquo;Still matches&rdquo; is checked now, against the content identity recorded at \
         validation &mdash; it is an observation about this moment, not a standing \
         guarantee.</p>",
    );
    out.push_str("<table><tr><th>waypoint</th><th>artifact</th><th>now</th></tr>");
    for entry in entries {
        let waypoint = entry["waypoint"].as_str().unwrap_or("?");
        let claim = entry["claim"].as_str().unwrap_or("");
        for artifact in entry["artifacts"].as_array().unwrap_or(&empty) {
            let name = artifact["name"].as_str().unwrap_or("?");
            let available = artifact["available"].as_bool().unwrap_or(false);
            // Only a claim id and a name that are plain path segments
            // become a link; anything else is shown as text.
            let linkable =
                token.is_some() && available && is_safe_segment(claim) && is_safe_segment(name);
            let label = if linkable {
                format!(
                    "<a href=\"/{}/evidence/{}/{}\">{}</a>",
                    token.unwrap_or(""),
                    html_escape(claim),
                    html_escape(name),
                    html_escape(name)
                )
            } else {
                html_escape(name)
            };
            let status = if available {
                "<span class=\"good\">still matches what the Claim was checked against</span>"
                    .to_string()
            } else {
                format!(
                    "<span class=\"bad\">not readable now: {}</span>",
                    html_escape(artifact["reason"].as_str().unwrap_or("reason unrecorded"))
                )
            };
            out.push_str(&format!(
                "<tr><td>{}</td><td>{label}</td><td>{status}</td></tr>",
                html_escape(waypoint),
            ));
        }
    }
    out.push_str("</table>");
    out
}

/// Which Herdr this page's return action will actually reach.
///
/// `herdr` routes by `HERDR_SOCKET_PATH`, and `HERDR_SESSION` is a
/// separate variable that can say something else — an inherited one
/// often does. Naming the session while talking to a different socket
/// tells the operator the wrong place, which was observed live: a
/// bridge started with an owned socket and an inherited `HERDR_SESSION`
/// focused a pane in the owned session and named the inherited one.
///
/// So the socket decides. Its session directory is the real name
/// (`.../sessions/<name>/herdr.sock`); anything else is reported as the
/// socket path itself rather than dressed up as a name. `HERDR_SESSION`
/// is used only when no socket is set and it is therefore what routing
/// will fall back to.
fn herdr_target() -> String {
    let socket = std::env::var("HERDR_SOCKET_PATH").unwrap_or_default();
    if socket.is_empty() {
        return std::env::var("HERDR_SESSION").unwrap_or_default();
    }
    let path = Path::new(&socket);
    path.parent()
        .filter(|dir| {
            dir.parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "sessions")
        })
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(socket)
}

fn render_return(result: &serde_json::Value, token: &str, focus: Option<&FocusOutcome>) -> String {
    let mut out = String::from("<h2>Return to Herdr</h2>");
    if let Some(outcome) = focus {
        out.push_str(&format!(
            "<div class=\"card {}\"><p>{}</p></div>",
            if outcome.ok { "ok" } else { "failed" },
            html_escape(&outcome.message)
        ));
    }
    let run = current_run_id(result).unwrap_or_else(|| "none yet".to_string());
    let session = herdr_target();
    let where_to = if session.is_empty() {
        String::new()
    } else {
        format!(
            " in the Herdr session <code>{}</code>",
            html_escape(&session)
        )
    };
    out.push_str(&format!(
        "<form method=\"POST\" action=\"/{token}/action/focus\">\
         <button type=\"submit\">Go to this Work's pane</button></form> \
         <a href=\"/{token}/\">Reload</a>\
         <p class=\"note\">Focuses the pane running this Work{where_to}.</p>\
         <details><summary>What this button actually does</summary><p>It asks Herdr, at the \
         moment you click it, which pane is running <code>{}</code>. If Herdr lists one &mdash; \
         including a pane it kept after the Run ended &mdash; that pane is focused. If it lists \
         none, nothing is run and the page says so. The button sends no data of its own; it \
         carries no target for a request to change.</p></details>",
        html_escape(&run),
    ));
    out
}

// ---- the delivered World's own page ------------------------------------

/// The Work's other stages, so a completed stage's context stays one
/// click away once the Work has moved on. Each row addresses its own
/// Run, never "whatever is current".
fn render_stage_switcher(result: &serde_json::Value, shown: &str, token: &str) -> String {
    let empty = Vec::new();
    let runs = result["runs"].as_array().unwrap_or(&empty);
    if runs.len() < 2 {
        return String::new();
    }
    let mut out = String::from("<p class=\"note\">Stages of this Work: ");
    let mut first = true;
    for entry in runs {
        let Some(id) = entry["run"]["id"].as_str() else {
            continue;
        };
        let name = entry["run"]["waypoint"].as_str().unwrap_or(id);
        if !first {
            out.push_str(" &middot; ");
        }
        first = false;
        if id == shown {
            out.push_str(&format!("<strong>{}</strong>", html_escape(name)));
        } else {
            out.push_str(&format!(
                "<a href=\"/{token}/world/{}\">{}</a>",
                html_escape(id),
                html_escape(name)
            ));
        }
    }
    out.push_str("</p>");
    out
}

/// The typed return to *this* stage, offered only where Herdr is
/// actually holding a pane for this Run right now.
///
/// A Deterministic stage has no agent pane, and neither does an Actor
/// stage whose pane Herdr has since let go. Either way the honest answer
/// is that there is nowhere to return to — said plainly, rather than a
/// button that would have to refuse itself.
fn render_stage_return(run_id: &str, waypoint: &str, token: &str) -> String {
    match find_agent_for_run(run_id) {
        Ok(Some(agent)) => format!(
            "<form method=\"post\" action=\"/{token}/action/focus/{}\"><button type=\"submit\">\
             Go to the pane running {}</button> <span class=\"note\">Herdr is holding pane \
             <code>{}</code> for this stage.</span></form>",
            html_escape(run_id),
            html_escape(waypoint),
            html_escape(&agent.pane_id)
        ),
        Ok(None) => format!(
            "<p class=\"note\">Herdr is not holding a pane for {}, so there is nowhere to \
             return to. Its context and its outputs are still here.</p>",
            html_escape(waypoint)
        ),
        Err(reason) => format!(
            "<p class=\"note\">Herdr could not be asked whether it still holds a pane for this \
             stage: {}</p>",
            html_escape(&reason)
        ),
    }
}

fn world_page(scope: &Scope, work_id: &str, token: &str, stage: &Stage) -> String {
    world_page_with(scope, work_id, token, stage, None)
}

fn world_page_with(
    scope: &Scope,
    work_id: &str,
    token: &str,
    stage: &Stage,
    focus: Option<&FocusOutcome>,
) -> String {
    let revision = stage.revision;
    let result = match fetch_status(scope, work_id) {
        Ok(result) => result,
        Err(reason) => {
            return render_unavailable_page(
                "World",
                "wirk could not answer for this Work",
                &reason,
                Some(token),
                scope.is_administrative(),
            )
            .replace(LAST_KNOWN_SLOT, "");
        }
    };
    if world_withheld(&result) {
        return page(
            "World",
            "<h1>The delivered context is not disclosed to this reader</h1>\
             <div class=\"card stale\"><p>wirk withheld this Work's World from the scope this \
             page is reading under. The context exists; this reader is not admitted to \
             it.</p></div>"
                .to_string(),
            Some(token),
            scope.is_administrative(),
        );
    }
    // Which stage this page is reading: the one the address named, or
    // the current Run when it named none. A Run that this Work's own
    // status projection does not list is refused here, before it is used
    // to address anything.
    let listed = listed_runs(&result);
    let run_id = match &stage.run {
        Some(asked) => {
            if !listed.iter().any(|id| id == asked) {
                return page(
                    "World",
                    format!(
                        "<h1>That is not a Run of this Work</h1><p>This page serves \
                         <code>{}</code>, and <code>{}</code> is not among the Runs wirk lists \
                         for it under this reader's scope. Nothing was read for it.</p>",
                        html_escape(work_id),
                        html_escape(asked)
                    ),
                    Some(token),
                    scope.is_administrative(),
                );
            }
            asked.clone()
        }
        None => {
            let Some(run_id) = current_run_id(&result) else {
                return page(
                    "World",
                    "<h1>No context has been delivered yet</h1><p>This Work has no open Run, so \
                     nothing has been assembled for it.</p>"
                        .to_string(),
                    Some(token),
                    scope.is_administrative(),
                );
            };
            run_id
        }
    };

    let pointer = match wirkd::client::locate(Path::new(&scope.estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            return render_unavailable_page(
                "World",
                "wirk could not be reached",
                &err.to_string(),
                Some(token),
                scope.is_administrative(),
            )
            .replace(LAST_KNOWN_SLOT, "");
        }
    };
    let payload = WorldShowPayload {
        triple: ExecutionTriple {
            estate_root: scope.estate.clone(),
            work_id: WorkId(work_id.to_string()),
            run_id: RunId(run_id.clone()),
        },
        revision,
    };
    let reply = match wirkd::client::call(&pointer.socket, &Request::world_show(payload)) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            return render_unavailable_page(
                "World",
                "wirk refused to read this Work's delivered context",
                &format!("{}: {}", error.code, error.message),
                Some(token),
                scope.is_administrative(),
            )
            .replace(LAST_KNOWN_SLOT, "");
        }
        Err(err) => {
            return render_unavailable_page(
                "World",
                "wirk could not read this Work's delivered context",
                &err.to_string(),
                Some(token),
                scope.is_administrative(),
            )
            .replace(LAST_KNOWN_SLOT, "");
        }
    };

    let waypoint = reply["waypoint"].as_str().unwrap_or("?").to_string();
    let mut body = format!(
        "<h1>The context delivered to {}</h1>",
        html_escape(&waypoint)
    );
    // Three different things a reader needs told apart: the stage the
    // Work is on now, a finished stage the Work has moved past, and an
    // attempt that was superseded by a retry of its own Waypoint. The
    // reply's `current` only answers the last of those — it means "the
    // current Run of its own Waypoint" — so the Work's own current Run
    // decides the first.
    let is_current_stage = current_run_id(&result).as_deref() == Some(run_id.as_str());
    body.push_str(&format!(
        "<p>{}</p>",
        if is_current_stage {
            "This is the stage the Work is on now."
        } else if reply["current"].as_bool().unwrap_or(false) {
            "The Work has moved past this stage. This is the context it was given, as it was \
             given."
        } else {
            "This attempt was superseded by a later attempt at the same stage. This is the \
             context this attempt was given."
        }
    ));
    body.push_str(&render_stage_switcher(&result, &run_id, token));
    body.push_str(&render_stage_return(&run_id, &waypoint, token));
    if let Some(outcome) = focus {
        body.push_str(&format!(
            "<div class=\"card {}\"><p>{}</p></div>",
            if outcome.ok { "" } else { "failed" },
            html_escape(&outcome.message)
        ));
    }

    if let Some(state @ ("none" | "unavailable")) = reply["orientation"].as_str() {
        body.push_str(&format!(
            "<div class=\"card {}\"><p>{}</p></div>",
            if state == "none" { "stale" } else { "failed" },
            html_escape(
                reply["detail"]
                    .as_str()
                    .unwrap_or("wirk reported no delivered context and gave no further detail.")
            )
        ));
        if let Some(reason) = reply["reason"].as_str() {
            body.push_str(&format!(
                "<p class=\"note\">{}</p>",
                html_escape(&format!("reason: {reason}"))
            ));
        }
        return page("World", body, Some(token), scope.is_administrative());
    }

    // The revision chain: which document this is, and what came before.
    let empty = Vec::new();
    let revisions = reply["revisions"].as_array().unwrap_or(&empty);
    if revisions.len() > 1 {
        body.push_str(
            "<h2>Revisions of this context</h2><table><tr><th>revision</th>\
                       <th>where it came from</th><th></th></tr>",
        );
        let shown = revision.unwrap_or_else(|| reply["latest_revision"].as_u64().unwrap_or(0));
        for entry in revisions {
            let n = entry["revision"].as_u64().unwrap_or(0);
            body.push_str(&format!(
                "<tr><td>{}{}</td><td>{}</td><td>{}</td></tr>",
                n,
                if n == shown {
                    " <span class=\"note\">(shown below)</span>"
                } else {
                    ""
                },
                if entry["initial"].as_bool().unwrap_or(false) {
                    "assembled when the Work was admitted"
                } else {
                    "added by the actor while it ran"
                },
                if n == shown {
                    String::new()
                } else {
                    format!("<a href=\"/{token}/world/{run_id}/{n}\">read this one</a>")
                }
            ));
        }
        body.push_str("</table>");
    }

    let Some(projection) = reply.get("projection") else {
        body.push_str(
            "<div class=\"card failed\"><p>wirk listed this context but returned no document \
             body for it.</p></div>",
        );
        return page("World", body, Some(token), scope.is_administrative());
    };
    // The revision actually served, not the one asked for: a link
    // written from this page names the document in front of the reader.
    let served_revision = reply["reference"]["revision"]
        .as_u64()
        .or(revision)
        .or_else(|| reply["latest_revision"].as_u64())
        .unwrap_or(0);
    body.push_str(&render_projection(
        projection,
        token,
        &Stage::address(&run_id, served_revision),
    ));

    // The console report is still here, and still exactly what `wirk
    // world show` prints — but as the machine detail it is, under the
    // reading above rather than in front of it.
    let report = wirk_core::render_projection_report(
        projection,
        reply.get("receipt"),
        reply["current"].as_bool().unwrap_or(false),
        wirk_core::ReportStyle::Console,
    );
    body.push_str(&format!(
        "<details><summary>The same context as <code>wirk world show</code> prints it</summary>\
         <pre>{}</pre></details>",
        html_escape(&report)
    ));
    page("World", body, Some(token), scope.is_administrative())
}

// ---- the delivered context, read as context ----------------------------

/// A coordinate is hex-encoded JSON of an `ExactCoordinate`, so the
/// source, path and line span a delivered item names can be read off the
/// handle the projection already carries — no estate read, no new
/// retrieval path. Only the *display* comes from here; following an item
/// to its bytes still goes through `atlas resolve` under this server's
/// own scope, which decides admission for itself.
fn coordinate_location(encoded: &str) -> Option<wirk_atlas::ExactCoordinate> {
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(encoded.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    serde_json::from_slice(&bytes).ok()
}

/// Which item of *one named* projection to follow. The index is
/// resolved against the projection the address pinned — this Run, this
/// revision — so a coordinate never arrives from the URL, and a link
/// written against one delivered document cannot come to mean position
/// `n` of a different one because the Work moved on.
fn item_coordinates(projection: &serde_json::Value) -> Vec<String> {
    let empty = Vec::new();
    ["bound", "referenced"]
        .iter()
        .flat_map(|key| projection[*key].as_array().unwrap_or(&empty).clone())
        .filter_map(|item| item["coordinate"].as_str().map(str::to_string))
        .collect()
}

/// Where an item came from, in words: the source it was read in, the
/// path inside it, and the lines. Falls back to saying plainly that the
/// handle could not be read rather than showing hex as if it were a
/// location.
fn where_from(item: &serde_json::Value) -> String {
    let Some(coordinate) = item["coordinate"].as_str().and_then(coordinate_location) else {
        return "<span class=\"dim\">an opaque handle this page could not read as a \
                location</span>"
            .to_string();
    };
    let path = String::from_utf8_lossy(&coordinate.path).to_string();
    let shown = item.get("shown");
    let (first, last) = match shown {
        Some(shown) => (
            shown["line_start"]
                .as_u64()
                .unwrap_or(coordinate.line_start),
            shown["line_end"].as_u64().unwrap_or(coordinate.line_end),
        ),
        None => (coordinate.line_start, coordinate.line_end),
    };
    let lines = if first == 0 && last == 0 {
        String::new()
    } else if first == last {
        format!(":{first}")
    } else {
        format!(":{first}\u{2013}{last}")
    };
    // The source is named in words on the item's own "why" line, which
    // the assembler wrote. Its internal id is machine detail and stays
    // in the exact coordinate below.
    html_escape(&format!("{path}{lines}"))
}

/// Who says so: the generation and object a literal reference was read
/// at, or the Claim and digest a prior stage's artifact was validated
/// against. Short forms in the line, full values in the detail below it.
fn attribution(item: &serde_json::Value) -> String {
    let identity = &item["identity"];
    let short = |s: &str| s.chars().take(12).collect::<String>();
    match identity["kind"].as_str() {
        Some("generation") => format!(
            "read at generation <code>{}</code>, object <code>{}</code>",
            html_escape(&short(identity["generation"].as_str().unwrap_or("?"))),
            html_escape(&short(identity["object_id"].as_str().unwrap_or("?"))),
        ),
        Some("artifact_digest") => format!(
            "an earlier stage's artifact, as Claim <code>{}</code> validated it \
             (sha256 <code>{}</code>)",
            html_escape(identity["claim"].as_str().unwrap_or("?")),
            html_escape(&short(identity["digest"].as_str().unwrap_or("?"))),
        ),
        _ => "<span class=\"dim\">no content identity recorded</span>".to_string(),
    }
}

/// One coverage state as a sentence a person can act on.
fn coverage_sentence(coverage: &serde_json::Value) -> (&'static str, String) {
    let reason = match coverage["reason"].as_str() {
        Some("unresolved_references") => {
            "the question named references that resolved nowhere in the admitted sources"
        }
        Some("inadmissible_sources") => "it asked for a source this Work is not bound to",
        Some("evidence_unavailable") => {
            "something the captured sources record could not be read \
                                         back"
        }
        Some("concurrent_publication") => {
            "the estate kept being republished while this was \
                                           assembled"
        }
        Some("governance_outside_captured_editions") => {
            "a governing record exists at an edition \
                                                          outside what was captured here"
        }
        Some("index_cannot_attest_completeness") => {
            "the findings index cannot be attested \
                                                      complete, so the consulted set may be short"
        }
        Some("findings_index_unreadable") => {
            "the findings index could not be read at all, so the \
                                              consulted set is this Work's own journal and nothing \
                                              else"
        }
        Some("source_extraction_incomplete") => {
            "part of the captured corpus was never turned into \
                                                 searchable units, so it was not searched"
        }
        Some(other) => return ("stale", html_escape(other)),
        None => "",
    };
    // Coverage is a statement about *delivery*: everything the assembly
    // set out to supply was supplied, or some of it was not. It is not a
    // statement that what was supplied is enough to do the work, and it
    // must not read like one — nothing here searched for sufficiency,
    // and saying "everything is here" invites exactly that reading.
    match coverage["state"].as_str() {
        Some("complete") => (
            "ok",
            "Everything this assembly set out to supply was supplied. Whether it is enough for \
             the task is not something this says."
                .to_string(),
        ),
        Some("partial") => (
            "waiting",
            format!("Some of what this assembly set out to supply was left out: {reason}."),
        ),
        Some("degraded") => (
            "failed",
            format!("This assembly could not supply what it set out to: {reason}."),
        ),
        _ => (
            "stale",
            "wirk recorded no coverage state for this context.".to_string(),
        ),
    }
}

/// The structured projection, rendered as the reading it is: what was
/// asked, how complete the answer is, where it was read from, and then
/// the selected context itself — each item with its location, why it is
/// here, its excerpt and its attribution.
fn render_projection(projection: &serde_json::Value, token: &str, stage: &str) -> String {
    let empty = Vec::new();
    let mut out = String::new();

    if let Some(question) = projection["question"].as_str().filter(|q| !q.is_empty()) {
        out.push_str(&format!(
            "<h2>What this context was assembled to answer</h2><p class=\"lede\">{}</p>",
            html_escape(question)
        ));
    }

    // How much to trust it, before any of it is read.
    let (class, sentence) = coverage_sentence(&projection["coverage"]);
    out.push_str(&format!("<div class=\"card {class}\"><p>{sentence}</p>"));
    let retrieval = &projection["retrieval"];
    if let Some(mode) = retrieval["mode"].as_str() {
        let mut note = format!(
            "Ranked {} over {} candidate{}, {} delivered.",
            html_escape(mode),
            retrieval["total_candidates"].as_u64().unwrap_or(0),
            if retrieval["total_candidates"].as_u64() == Some(1) {
                ""
            } else {
                "s"
            },
            retrieval["returned"].as_u64().unwrap_or(0),
        );
        // The status is the fact a reader acts on; the backend's own
        // explanation of it is a paragraph, and belongs under a
        // disclosure rather than in front of the context it describes.
        let mut semantic_detail = String::new();
        if let Some(semantic) = retrieval["semantic"].as_str() {
            note.push_str(&format!(" Semantic ranking {}.", html_escape(semantic)));
            if let Some(reason) = retrieval["semantic_reason"].as_str() {
                semantic_detail = format!(
                    "<details><summary>Why semantic ranking {}</summary><p class=\"note\">{}\
                     </p></details>",
                    html_escape(semantic),
                    html_escape(reason),
                );
            }
        }
        if retrieval["capacity"]["reached"].as_bool() == Some(true) {
            note.push_str(
                " The query filled its capacity, so relevant material may exist beyond it; \
                 reaching it is a new, wider query, not a bigger page.",
            );
        }
        let degraded = retrieval["degraded"].as_array().unwrap_or(&empty);
        if !degraded.is_empty() {
            note.push_str(&format!(
                " Degraded: {}.",
                html_escape(
                    &degraded
                        .iter()
                        .filter_map(|d| d.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            ));
        }
        out.push_str(&format!("<p class=\"note\">{note}</p>{semantic_detail}"));
    }
    out.push_str("</div>");

    // Freshness: which edition of each source this was read at.
    let generations = projection["generations"].as_array().unwrap_or(&empty);
    if !generations.is_empty() {
        // The captured vector pairs an internal membership id with a
        // generation id. Neither is a name, so neither is printed as
        // one: the count and the publication revision are what a person
        // can act on, and the ids stay in the detail below.
        out.push_str(&format!(
            "<p class=\"note\">Read across {} captured source edition{}, at publication \
             revision {}. Nothing published after that is in here.</p>",
            generations.len(),
            if generations.len() == 1 { "" } else { "s" },
            projection["publication_revision"].as_u64().unwrap_or(0),
        ));
        out.push_str(&format!(
            "<details><summary>Exact editions</summary><table><tr><th>membership</th>\
             <th>generation</th></tr>{}</table></details>",
            generations
                .iter()
                .filter_map(|g| g.as_array())
                .map(|g| format!(
                    "<tr><td>{}</td><td><code>{}</code></td></tr>",
                    html_escape(g.first().and_then(|v| v.as_str()).unwrap_or("?")),
                    html_escape(g.get(1).and_then(|v| v.as_str()).unwrap_or("?")),
                ))
                .collect::<String>()
        ));
    }

    // The context itself.
    let mut index = 0usize;
    for (key, heading, lede) in [
        (
            "bound",
            "Required reading",
            "Delivered as part of this stage's context: it was pulled in, not merely offered.",
        ),
        (
            "referenced",
            "Worth reading",
            "Ranked as relevant to the question and delivered alongside the required reading.",
        ),
    ] {
        let items = projection[key].as_array().unwrap_or(&empty);
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "<h2>{heading} ({})</h2><p class=\"note\">{lede}</p>",
            items.len()
        ));
        for item in items {
            out.push_str(&render_item(item, index, token, stage));
            index += 1;
        }
    }
    if projection["bound"].as_array().is_none_or(|b| b.is_empty())
        && projection["referenced"]
            .as_array()
            .is_none_or(|r| r.is_empty())
    {
        out.push_str(
            "<h2>Selected context</h2><div class=\"card stale\"><p>This context delivered no \
             source material. That is what the assembly recorded; it is not a statement that the \
             estate holds nothing on the question.</p></div>",
        );
    }

    // Places this stage may go that nothing named for it.
    let reachable = projection["reachable"].as_array().unwrap_or(&empty);
    if !reachable.is_empty() {
        out.push_str(&format!(
            "<h2>Reachable if you go looking ({})</h2><p class=\"note\">Admitted places this \
             stage may expand into. Nothing here was delivered; each is a handle for \
             <code>wirk world expand</code>.</p><table><tr><th>handle</th><th>source</th>\
             <th>resources</th></tr>{}</table>",
            reachable.len(),
            reachable
                .iter()
                .map(|entry| format!(
                    "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
                    html_escape(entry["handle"].as_str().unwrap_or("?")),
                    html_escape(entry["source"].as_str().unwrap_or("?")),
                    entry["resources"].as_u64().unwrap_or(0),
                ))
                .collect::<String>()
        ));
    }

    // What the assembler says about its own run, and what it left out.
    for (key, heading) in [
        ("assumptions", "Assumed while assembling this"),
        ("unknowns", "Left unknown"),
    ] {
        let statements = projection[key].as_array().unwrap_or(&empty);
        if statements.is_empty() {
            continue;
        }
        // An assembler's account of its own run is several hundred words
        // of implementation prose, and a person reading the context is
        // not deciding anything with it. The ones attributed to the
        // *intent* are different: an unresolved reference is something
        // the reader may need to act on, so those stay in the flow.
        let (from_intent, from_assembly): (Vec<_>, Vec<_>) = statements
            .iter()
            .partition(|s| s["attributed_to"].as_str() == Some("intent"));
        if !from_intent.is_empty() {
            out.push_str(&format!("<h2>{heading}</h2><ul>"));
            for statement in &from_intent {
                out.push_str(&format!(
                    "<li>{} <span class=\"note\">(a reference the intent made that did not \
                     resolve)</span></li>",
                    html_escape(statement["text"].as_str().unwrap_or("?")),
                ));
            }
            out.push_str("</ul>");
        }
        if !from_assembly.is_empty() {
            out.push_str(&format!(
                "<details><summary>{} &mdash; {} note{} the assembler made about how it \
                 ran</summary><ul>",
                heading,
                from_assembly.len(),
                if from_assembly.len() == 1 { "" } else { "s" },
            ));
            for statement in &from_assembly {
                out.push_str(&format!(
                    "<li>{}</li>",
                    html_escape(statement["text"].as_str().unwrap_or("?"))
                ));
            }
            out.push_str("</ul></details>");
        }
    }

    let omitted = projection["omitted"].as_array().unwrap_or(&empty);
    if !omitted.is_empty() {
        out.push_str("<h2>Left out</h2><ul>");
        for omission in omitted {
            out.push_str(&format!("<li>{}</li>", render_omission(omission)));
        }
        out.push_str("</ul>");
    }

    if let Some(next) = projection["next_action"].as_str().filter(|n| !n.is_empty()) {
        out.push_str(&format!(
            "<h2>What the assembler says it delivered</h2><p>{}</p>",
            html_escape(next)
        ));
    }
    out
}

fn render_omission(omission: &serde_json::Value) -> String {
    match omission["kind"].as_str() {
        Some("over_budget") => format!(
            "{} of {} {} fitted the delivery budget; the rest were ranked but not delivered.",
            omission["shown"].as_u64().unwrap_or(0),
            omission["total"].as_u64().unwrap_or(0),
            html_escape(omission["of"].as_str().unwrap_or("items")),
        ),
        Some("inadmissible") => format!(
            "{} item{} exist that this Work is not admitted to. They are counted, not named: \
             that is the difference between nothing being here and something being here you may \
             not see.",
            omission["count"].as_u64().unwrap_or(0),
            if omission["count"].as_u64() == Some(1) {
                ""
            } else {
                "s"
            },
        ),
        Some("unavailable") => format!(
            "<code>{}</code> was asked for and could not be delivered: {}.",
            html_escape(
                &omission["coordinate"]
                    .as_str()
                    .and_then(coordinate_location)
                    .map(|c| String::from_utf8_lossy(&c.path).to_string())
                    .unwrap_or_else(|| "a coordinate".to_string())
            ),
            match omission["reason"].as_str() {
                Some("generation_unavailable") =>
                    "no published edition of that source was captured",
                Some("resource_excluded") => "that edition does not record it as indexed content",
                Some("resource_unsupported") => "its kind is not one this estate indexes",
                Some("resource_unavailable") =>
                    "its bytes could not be read back at the edition recorded",
                Some("artifact_bytes_changed") =>
                    "an earlier stage's artifact no longer hashes to what its Claim validated, so \
                     the later bytes are not attributed to that Claim",
                Some("artifact_unreadable") => "an earlier stage's artifact cannot be read now",
                Some("artifact_unrecorded") =>
                    "an earlier stage's receipt records no content identity, so it is reported \
                     rather than bound",
                Some("governing_record_unresolvable") =>
                    "a governing record touching it no longer resolves at the edition it was \
                     admitted against",
                Some("findings_index_unreadable") => "the findings index could not be read",
                _ => "wirk recorded no reason",
            }
        ),
        _ => "wirk recorded an omission this page does not know how to describe.".to_string(),
    }
}

/// One delivered item: where it is, why it is here, what it says, and
/// who says so. The excerpt is the projection's own bounded summary,
/// written as text and never as markup; the link follows the item to its
/// actual bytes through `atlas resolve`.
fn render_item(item: &serde_json::Value, index: usize, token: &str, stage: &str) -> String {
    let mut out = format!(
        "<div class=\"item\"><p class=\"where\">{}</p>",
        where_from(item)
    );
    // The assembler's own reason, in its own words, and never rewritten
    // here. Its first clause says why this item is in front of the
    // reader; the qualifications after it are about how to read the
    // whole list, and repeating them on every item buries the item. So
    // the lead stays in the flow and the rest is one click away.
    let reason = item["reason"].as_str().unwrap_or("delivered");
    let (lead, qualification) = match reason.split_once("; ") {
        Some((lead, rest)) => (lead, Some(rest)),
        None => (reason, None),
    };
    out.push_str(&format!(
        "<p class=\"why\"><span class=\"tag\">{}</span>{}</p>",
        match item["lifetime"].as_str() {
            Some("standing") => "standing",
            _ => "working",
        },
        html_escape(lead),
    ));
    if let Some(qualification) = qualification {
        out.push_str(&format!(
            "<details><summary>How to read this reason</summary><p>{}</p></details>",
            html_escape(qualification)
        ));
    }
    if let Some(summary) = item["summary"].as_str().filter(|s| !s.is_empty()) {
        out.push_str(&format!(
            "<div class=\"excerpt\">{}</div>",
            html_escape(summary)
        ));
    }
    let empty = Vec::new();
    let matched = item["shown"]["matched_terms"].as_array().unwrap_or(&empty);
    if !matched.is_empty() {
        out.push_str(&format!(
            "<p class=\"note\">Located on: {}.{}</p>",
            html_escape(
                &matched
                    .iter()
                    .filter_map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            if item["shown"]["whole_match_shown"].as_bool() == Some(false) {
                " The match is longer than the excerpt budget, so it is shown cut."
            } else {
                ""
            }
        ));
    }
    out.push_str(&format!(
        "<p class=\"note\">{} &middot; <a href=\"/{token}/source/{stage}/{index}\">Read the source \
         it was taken from</a></p>",
        attribution(item)
    ));
    if let Some(coordinate) = item["coordinate"].as_str() {
        out.push_str(&format!(
            "<details><summary>Exact coordinate</summary><pre>{}</pre></details>",
            html_escape(coordinate)
        ));
    }
    out.push_str("</div>");
    out
}

// ---- a delivered item's own source --------------------------------------

/// `source/<n>`: the committed bytes one delivered item was taken from,
/// read through `atlas resolve` — the same resolver `wirk atlas resolve`
/// uses, under this server's own scope, which decides admission for
/// itself and refuses in its own words.
///
/// The URL carries an index, never a coordinate. The coordinate is taken
/// from the projection this server re-reads for the request, so a caller
/// cannot address the estate through this route: an index outside the
/// current context's own items is a 404 and nothing reaches the daemon.
fn source_page(scope: &Scope, work_id: &str, token: &str, rest: &str) -> String {
    let unavailable = |headline: &str, detail: &str| {
        render_unavailable_page(
            "Source",
            headline,
            detail,
            Some(token),
            scope.is_administrative(),
        )
        .replace(LAST_KNOWN_SLOT, "")
    };
    let not_an_item = || {
        page(
            "Source",
            "<h1>Not a delivered item</h1><p>This address does not name an item of this \
             Work's context.</p>"
                .to_string(),
            Some(token),
            scope.is_administrative(),
        )
    };
    // `<run>/<revision>/<index>`: the Run and the revision are the
    // identity the link was written against, and the index means a
    // position in *that* document.
    let trimmed = rest.trim_end_matches('/');
    let (stage_part, index_part) = match trimmed.rsplit_once('/') {
        Some(split) => split,
        None => return not_an_item(),
    };
    let Ok(index) = index_part.parse::<usize>() else {
        return not_an_item();
    };
    let stage = Stage::parse(stage_part);
    let (Some(asked_run), Some(asked_revision)) = (stage.run.clone(), stage.revision) else {
        return not_an_item();
    };
    let result = match fetch_status(scope, work_id) {
        Ok(result) => result,
        Err(reason) => return unavailable("wirk could not answer for this Work", &reason),
    };
    if world_withheld(&result) {
        return page(
            "Source",
            "<h1>The delivered context is not disclosed to this reader</h1><div class=\"card \
             stale\"><p>wirk withheld this Work's World from the scope this page reads under, \
             so there is no item here to follow.</p></div>"
                .to_string(),
            Some(token),
            scope.is_administrative(),
        );
    }
    // The Run this link names must be one this Work's own status
    // projection lists under this reader's scope. Anything else is not
    // addressable from here at all.
    if !listed_runs(&result).contains(&asked_run) {
        return page(
            "Source",
            format!(
                "<h1>That is not a Run of this Work</h1><p><code>{}</code> is not among the Runs \
                 wirk lists for this Work under this reader's scope, so nothing was read for \
                 it.</p>",
                html_escape(&asked_run)
            ),
            Some(token),
            scope.is_administrative(),
        );
    }
    let run_id = asked_run.clone();
    let pointer = match wirkd::client::locate(Path::new(&scope.estate)) {
        Ok(pointer) => pointer,
        Err(err) => return unavailable("wirk could not be reached", &err.to_string()),
    };
    let reply = match wirkd::client::call(
        &pointer.socket,
        &Request::world_show(WorldShowPayload {
            triple: ExecutionTriple {
                estate_root: scope.estate.clone(),
                work_id: WorkId(work_id.to_string()),
                run_id: RunId(run_id.clone()),
            },
            revision: Some(asked_revision),
        }),
    ) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            return unavailable(
                "wirk refused to read this Work's delivered context",
                &format!("{}: {}", error.code, error.message),
            );
        }
        Err(err) => {
            return unavailable(
                "wirk could not read this Work's delivered context",
                &err.to_string(),
            );
        }
    };
    // The revision this link names is either still deliverable or it is
    // not. wirkd refuses a revision a Run was never given, and reports a
    // projection it can no longer read, by name — neither is quietly
    // replaced with a document that happens to be current.
    if reply["orientation"].as_str() == Some("unavailable") {
        return page(
            "Source",
            format!(
                "<h1>That source is no longer available</h1><p>This link names revision {} of \
                 the context delivered to Run <code>{}</code>. wirk cannot deliver that \
                 document now, so the item it pointed at cannot be shown. It has not been \
                 replaced by whatever is at that position in this Work's context today.</p>\
                 <p class=\"note\">{}</p>",
                asked_revision,
                html_escape(&run_id),
                html_escape(
                    reply["detail"]
                        .as_str()
                        .unwrap_or("wirk gave no further detail.")
                )
            ),
            Some(token),
            scope.is_administrative(),
        );
    }
    let Some(projection) = reply.get("projection") else {
        return unavailable(
            "wirk returned no context document",
            "there is no delivered item to follow",
        );
    };
    let coordinates = item_coordinates(projection);
    let Some(coordinate) = coordinates.get(index) else {
        return page(
            "Source",
            "<h1>Not a delivered item</h1><p>This Work's context has no item at that \
             position. Only what it actually delivered can be followed from here.</p>"
                .to_string(),
            Some(token),
            scope.is_administrative(),
        );
    };

    let resolved = match wirkd::client::call(
        &pointer.socket,
        &Request::atlas_resolve(crate::wirkd::AtlasResolvePayload {
            work: scope.requesting.clone(),
            coordinate: coordinate.clone(),
        }),
    ) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            return unavailable(
                "wirk refused to resolve this item's source",
                &format!("{}: {}", error.code, error.message),
            );
        }
        Err(err) => return unavailable("wirk could not resolve this item", &err.to_string()),
    };

    let location = coordinate_location(coordinate);
    let title = location
        .as_ref()
        .map(|c| String::from_utf8_lossy(&c.path).to_string())
        .unwrap_or_else(|| "this item's source".to_string());
    let mut body = format!("<h1>{}</h1>", html_escape(&title));
    if let Some(coordinate) = &location {
        body.push_str(&format!(
            "<p class=\"note\">Lines {}&ndash;{}, read at generation <code>{}</code></p>",
            coordinate.line_start,
            coordinate.line_end,
            html_escape(&coordinate.generation.0.chars().take(12).collect::<String>()),
        ));
    }

    match resolved["outcome"].as_str() {
        Some("resolved") => {
            let path = resolved["path"].as_str().unwrap_or(&title);
            body.push_str(&format!(
                "<p class=\"note\"><code>{}</code>, lines {}&ndash;{}. {} byte{} returned{}.</p>",
                html_escape(path),
                resolved["line_start"].as_u64().unwrap_or(0),
                resolved["line_end"].as_u64().unwrap_or(0),
                resolved["budget"]["returned_bytes"].as_u64().unwrap_or(0),
                if resolved["budget"]["returned_bytes"].as_u64() == Some(1) {
                    ""
                } else {
                    "s"
                },
                match resolved["budget"]["total_bytes"].as_u64() {
                    Some(total) => format!(" of {total} in the whole resource"),
                    None => String::new(),
                }
            ));
            match resolved["text"].as_str() {
                // Source text is content. It is escaped and written into
                // a text block: never markup, never a link, never
                // anything a browser will act on.
                Some(text) => body.push_str(&format!("<pre>{}</pre>", html_escape(text))),
                None => body.push_str(
                    "<div class=\"card stale\"><p>These bytes are not text, so they are not \
                     shown here.</p></div>",
                ),
            }
        }
        Some("absent") => body.push_str(
            "<div class=\"card stale\"><p>wirk resolved this coordinate to nothing: the \
             edition it names no longer holds those bytes.</p></div>",
        ),
        Some(state @ ("excluded" | "unsupported" | "unavailable")) => {
            body.push_str(&format!(
                "<div class=\"card failed\"><p>wirk could not return these bytes ({}): {}</p>\
                 </div>",
                html_escape(state),
                html_escape(resolved["detail"].as_str().unwrap_or("no detail was given")),
            ));
        }
        _ => body.push_str(
            "<div class=\"card failed\"><p>wirk answered with an outcome this page does not \
             know how to read.</p></div>",
        ),
    }
    // Back to the context this item was actually delivered in, not to
    // whatever the Work's current stage happens to be reading now.
    body.push_str(&format!(
        "<p class=\"note\"><a href=\"/{token}/world/{}\">Back to the delivered context</a></p>",
        Stage::address(&run_id, asked_revision)
    ));
    page("Source", body, Some(token), scope.is_administrative())
}

// ---- an artifact's own content -----------------------------------------

/// `evidence/<claim>/<name>`: the bytes a validated Claim was checked
/// against, read through the same scoped door `wirk artifact read
/// --estate/--work` uses and re-verified against the recorded digest
/// before anything is shown.
///
/// The claim and name in the path do not address the estate directly.
/// They are matched against the evidence list this server has just
/// fetched under its own scope; a pair that is not in that list is a
/// `404` and no request reaches the daemon.
fn evidence_page(scope: &Scope, work_id: &str, token: &str, rest: &str) -> String {
    let Some((claim, name)) = rest.split_once('/') else {
        return page(
            "Evidence",
            "<h1>That is not an artifact address</h1><p>An artifact is addressed by the Claim \
             that was checked against it and the name it was claimed under.</p>"
                .to_string(),
            Some(token),
            scope.is_administrative(),
        );
    };
    if !is_safe_segment(claim) || !is_safe_segment(name) {
        return page(
            "Evidence",
            "<h1>That is not an artifact address</h1><p>Neither part of an artifact address \
             may hold anything but letters, digits, dot, dash and underscore.</p>"
                .to_string(),
            Some(token),
            scope.is_administrative(),
        );
    }
    let result = match fetch_status(scope, work_id) {
        Ok(result) => result,
        Err(reason) => {
            return render_unavailable_page(
                "Evidence",
                "wirk could not answer for this Work",
                &reason,
                Some(token),
                scope.is_administrative(),
            )
            .replace(LAST_KNOWN_SLOT, "");
        }
    };
    let empty = Vec::new();
    let mut found: Option<(&str, &serde_json::Value)> = None;
    for entry in result["evidence"].as_array().unwrap_or(&empty) {
        if entry["claim"].as_str() != Some(claim) {
            continue;
        }
        for artifact in entry["artifacts"].as_array().unwrap_or(&empty) {
            if artifact["name"].as_str() == Some(name) {
                found = Some((entry["waypoint"].as_str().unwrap_or("?"), artifact));
            }
        }
    }
    let Some((waypoint, artifact)) = found else {
        return page(
            "Evidence",
            format!(
                "<h1>No such artifact on this Work</h1><p>Nothing this reader is admitted to \
                 names <code>{}</code> under that Claim. Nothing was looked up in the \
                 estate.</p>",
                html_escape(name)
            ),
            Some(token),
            scope.is_administrative(),
        );
    };
    let heading = format!(
        "<h1>{}</h1><p class=\"note\">Claimed at waypoint {} &middot; Claim <code>{}</code></p>",
        html_escape(name),
        html_escape(waypoint),
        html_escape(claim),
    );
    if !artifact["available"].as_bool().unwrap_or(false) {
        return page(
            "Evidence",
            format!(
                "{heading}<div class=\"card failed\"><p>These bytes are not readable now: {}. \
                 The Claim was validated against content that is no longer there, so there is \
                 nothing honest to show.</p></div>",
                html_escape(artifact["reason"].as_str().unwrap_or("reason unrecorded"))
            ),
            Some(token),
            scope.is_administrative(),
        );
    }

    let pointer = match wirkd::client::locate(Path::new(&scope.estate)) {
        Ok(pointer) => pointer,
        Err(err) => {
            return render_unavailable_page(
                "Evidence",
                "wirk could not be reached",
                &err.to_string(),
                Some(token),
                scope.is_administrative(),
            )
            .replace(LAST_KNOWN_SLOT, "");
        }
    };
    let payload = match &scope.requesting {
        Some(requester) => WorkArtifactPayload::scoped(
            WorkId(work_id.to_string()),
            wirk_core::ClaimId(claim.to_string()),
            name.to_string(),
            requester.clone(),
        ),
        None => WorkArtifactPayload::admin(
            WorkId(work_id.to_string()),
            wirk_core::ClaimId(claim.to_string()),
            name.to_string(),
        ),
    };
    let receipt = match wirkd::client::call(&pointer.socket, &Request::work_artifact(payload)) {
        Ok(Reply::Ok { result, .. }) => result,
        Ok(Reply::Err { error, .. }) => {
            return page(
                "Evidence",
                format!(
                    "{heading}<div class=\"card failed\"><p>wirk refused this read: {}</p></div>",
                    html_escape(&format!("{}: {}", error.code, error.message))
                ),
                Some(token),
                scope.is_administrative(),
            );
        }
        Err(err) => {
            return render_unavailable_page(
                "Evidence",
                "wirk could not be reached",
                &err.to_string(),
                Some(token),
                scope.is_administrative(),
            )
            .replace(LAST_KNOWN_SLOT, "");
        }
    };
    // The same check `wirk artifact read` makes before it writes a byte:
    // the content must still hash to what the Claim was validated
    // against, or nothing is shown.
    let Ok(bytes) = verify_claimed_bytes(&receipt) else {
        return page(
            "Evidence",
            format!(
                "{heading}<div class=\"card failed\"><p>These bytes no longer hash to the \
                 content identity this Claim was validated against, so nothing is shown. This \
                 is the check failing, not the file being missing.</p></div>"
            ),
            Some(token),
            scope.is_administrative(),
        );
    };

    let total = bytes.len();
    let mut body = format!(
        "{heading}<div class=\"card ok\"><p>{total} bytes, re-read and re-hashed just now \
         against the content identity recorded when this Claim was validated.</p></div>"
    );
    match std::str::from_utf8(&bytes) {
        Ok(text) if !text.contains('\0') => {
            let shown = if total > MAX_ARTIFACT_PREVIEW {
                let mut cut = MAX_ARTIFACT_PREVIEW;
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                body.push_str(&format!(
                    "<p class=\"note\">Showing the first {cut} of {total} bytes.</p>"
                ));
                &text[..cut]
            } else {
                text
            };
            // Escaped, inside a <pre>: an artifact's own text is content,
            // never markup, and never a link this page offers to follow.
            body.push_str(&format!("<pre>{}</pre>", html_escape(shown)));
        }
        _ => {
            body.push_str(
                "<p>These are not UTF-8 text. Read them with <code>wirk artifact read</code>, \
                 which writes the exact bytes.</p>",
            );
        }
    }
    page(
        &format!("Evidence: {name}"),
        body,
        Some(token),
        scope.is_administrative(),
    )
}

// ---- the estate map ----------------------------------------------------

/// Which Works exist, what each is for, and which repository and commit
/// each is working from — grouped so a reader can see where the work is
/// landing, not just that ids exist. Every Work links to its own page
/// when this map is being served rather than exported.
fn render_estate_page(
    rows: &[(String, serde_json::Value)],
    scope: &Scope,
    token: Option<&str>,
) -> String {
    let mut body = String::from("<h1>What is happening in this estate</h1>");
    if rows.is_empty() {
        body.push_str("<p>No Work has been submitted here yet.</p>");
        return page("Estate", body, token, scope.is_administrative());
    }

    let live = rows
        .iter()
        .filter(|(_, r)| r["state"].as_str() == Some("active"))
        .count();
    let needing = rows
        .iter()
        .filter(|(_, r)| !attention(r).is_empty())
        .count();
    body.push_str(&format!(
        "<p class=\"lede\">{} Work{} recorded; {live} still active, {needing} with something \
         waiting on a person.</p><p class=\"note\">{}</p>",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        html_escape(&scope.label()),
    ));

    // Grouped by the source each Work is changing: the estate's map is
    // about where work is landing, not about id order.
    let mut groups: std::collections::BTreeMap<String, Vec<&(String, serde_json::Value)>> =
        std::collections::BTreeMap::new();
    for row in rows {
        let key = actor_world(&row.1)
            .and_then(|world| world["repository"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if row.1.get("__unreadable__").is_some() {
                    "could not be read".to_string()
                } else if world_withheld(&row.1) {
                    "not disclosed to this reader".to_string()
                } else {
                    "no repository reserved".to_string()
                }
            });
        groups.entry(key).or_default().push(row);
    }

    for (source, works) in groups {
        body.push_str(&format!("<h2>{}</h2>", html_escape(&source)));
        body.push_str(
            "<table><tr><th>what it is for</th><th>where it is</th><th>needs attention</th>\
             <th>branch</th></tr>",
        );
        for (id, result) in works {
            if let Some(reason) = result.get("__unreadable__").and_then(|v| v.as_str()) {
                body.push_str(&format!(
                    "<tr><td><code>{}</code></td><td class=\"bad\" colspan=\"3\">{}</td></tr>",
                    html_escape(id),
                    html_escape(reason)
                ));
                continue;
            }
            let purpose = purpose_line(result).unwrap_or_else(|| format!("Work {id}"));
            let label = match token {
                Some(token) if is_safe_work_id(id) => format!(
                    "<a href=\"/{token}/work/{}\">{}</a>",
                    html_escape(id),
                    html_escape(&purpose)
                ),
                _ => html_escape(&purpose),
            };
            let attention = attention(result);
            body.push_str(&format!(
                "<tr><td>{label}<br><span class=\"note\"><code>{}</code></span></td>\
                 <td>{} at {}</td><td>{}</td><td class=\"dim\"><code>{}</code></td></tr>",
                html_escape(id),
                html_escape(result["state"].as_str().unwrap_or("?")),
                html_escape(result["current_waypoint"].as_str().unwrap_or("no waypoint")),
                if attention.is_empty() {
                    "<span class=\"good\">nothing</span>".to_string()
                } else {
                    format!(
                        "<span class=\"bad\">{}</span>",
                        html_escape(attention[0].1.as_str())
                    )
                },
                html_escape(
                    actor_world(result)
                        .and_then(|w| w["branch"].as_str())
                        .unwrap_or("-")
                ),
            ));
        }
        body.push_str("</table>");
    }
    page("Estate", body, token, scope.is_administrative())
}
