//! wirkd client: locate the running daemon's socket and call it
//! (W2, `orient/transport.md` §3-4). No server loop here — `wirk claim`
//! and the other verb subcommands that dial this are W3's job; this
//! module only proves the wire path: read the pointer file, connect,
//! send one NDJSON request line, read one NDJSON reply line.

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use wirk_core::{Event, WorkId};

use super::{
    ErrorDetail, Reply, Request, StatusPayload, WatchPayload, WirkdPointer, WorkObligationsPayload,
};

/// Everything that can go wrong locating or calling wirkd. Kept as one
/// flat enum, no `thiserror` (not on `wirk`'s allow-list this wave,
/// R3 stdlib `impl Display`/`impl std::error::Error` suffices for five
/// variants).
#[derive(Debug)]
pub enum ClientError {
    /// `<estate_root>/.wirk/wirkd.json` does not exist — a distinct
    /// variant from `Io` so a caller can tell "wirkd was never started
    /// here" apart from "the pointer file exists but couldn't be read".
    PointerNotFound(PathBuf),
    /// The pointer file exists but is not valid `WirkdPointer` JSON.
    PointerMalformed { path: PathBuf, reason: String },
    /// A filesystem or socket I/O failure: opening the pointer file,
    /// connecting the socket, writing the request, reading the reply.
    Io(io::Error),
    /// The reply line was read but is not valid `Reply` JSON at all —
    /// a genuine protocol violation, distinct from `Refused` below.
    MalformedReply(String),
    /// A *scoped* request whose answer never established the requested
    /// consultation contract: a `status` or `work obligations` reply
    /// naming no applied scope or a different Work than the one asked
    /// about, or a `watch` stream whose first line is not the scope
    /// acknowledgment for that Work — including a stream that ends
    /// before acknowledging anything at all (the acknowledgment review's F-1/F-2). Distinct
    /// from `Refused` (the daemon answered, and answered honestly) and
    /// from `MalformedReply` (the line is not a shape this protocol
    /// has): the wire is well-formed, it is the *requested consultation
    /// contract* that was not applied — a daemon predating the scope
    /// gate answers a narrowed request in full, which is silent scope
    /// loss (the integration review's V-5). Never carries the answer it
    /// rejected: the point is that the unscoped content is not
    /// presented. Its message states what was observed on the wire and
    /// claims no cause for it: an older daemon, a broken counterparty
    /// and a closed connection all reach the same states, and this
    /// client cannot tell them apart (the acknowledgment review).
    ScopeNotApplied(&'static str),
    /// The reply line parsed fine as `Reply::Err` — wirkd's own
    /// explicit, well-formed refusal (0069 correction: `watch`'s line
    /// iterator previously folded this into `MalformedReply`, which
    /// mislabeled a valid daemon refusal, such as `NotFound`, as a wire
    /// protocol violation it was not).
    Refused(ErrorDetail),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::PointerNotFound(path) => {
                write!(f, "wirkd pointer file not found: {}", path.display())
            }
            ClientError::PointerMalformed { path, reason } => {
                write!(
                    f,
                    "wirkd pointer file malformed at {}: {reason}",
                    path.display()
                )
            }
            ClientError::Io(err) => write!(f, "wirkd client I/O error: {err}"),
            ClientError::MalformedReply(reason) => {
                write!(f, "wirkd reply malformed: {reason}")
            }
            ClientError::ScopeNotApplied(what) => {
                write!(
                    f,
                    "wirkd did not apply the requested scope: {what}. \
                     The answer was discarded unread and is not shown; \
                     why the scope was not applied is not established here."
                )
            }
            ClientError::Refused(detail) => {
                write!(f, "wirkd refused {}: {}", detail.code, detail.message)
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ClientError {
    fn from(err: io::Error) -> Self {
        ClientError::Io(err)
    }
}

/// Reads `<estate_root>/.wirk/wirkd.json` (0022 D79) and parses it as a
/// `WirkdPointer`. A missing file is `ClientError::PointerNotFound`, a
/// present-but-invalid file is `ClientError::PointerMalformed` — the
/// two are distinguished so a caller can tell "wirkd not running here"
/// from "something wrote a broken pointer" (transport.md §3: "the
/// client treats a pointer whose socket refuses connection as 'wirkd
/// not running' and errors, it does not auto-spawn").
pub fn locate(estate_root: &Path) -> Result<WirkdPointer, ClientError> {
    let path = estate_root.join(".wirk").join("wirkd.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ClientError::PointerNotFound(path));
        }
        Err(err) => return Err(ClientError::Io(err)),
    };
    serde_json::from_slice(&bytes).map_err(|err| ClientError::PointerMalformed {
        path,
        reason: err.to_string(),
    })
}

/// Connects to `socket`, writes one NDJSON-framed `request` line, half-
/// closes the write side (transport.md §2: "the client half-closes
/// write after sending"), then reads exactly one reply line and parses
/// it as `Reply`. A malformed reply line is `ClientError::
/// MalformedReply`, never a panic; an `{"ok":false,...}` reply parses
/// fine and comes back as `Ok(Reply::Err { .. })` — the caller decides
/// whether that is an error for its purposes, matching `Reply::is_ok`.
pub fn call(socket: &Path, request: &Request) -> Result<Reply, ClientError> {
    parse_reply(&call_line(socket, request)?, true)
}

/// Parses one reply line. `echo_rejected` decides whether a line that
/// is not a `Reply` at all is quoted back in the diagnostic: it is for
/// an administrative call, which asked for the whole answer, and it is
/// **not** for a scoped one (the acknowledgment review). A scoped
/// request that cannot establish its contract must present no part of
/// the answer it rejected, and `MalformedReply`'s own text was the one
/// path on which a rejected scoped reply still travelled. The parse
/// error itself — position and expectation, no content — is kept
/// either way.
fn parse_reply(reply_line: &str, echo_rejected: bool) -> Result<Reply, ClientError> {
    serde_json::from_str(reply_line).map_err(|err| {
        ClientError::MalformedReply(if echo_rejected {
            format!("{err}: {reply_line:?}")
        } else {
            format!("{err}; the rejected reply is not shown")
        })
    })
}

/// `call`'s transport half: connect, send one request line, half-close
/// the write side, read exactly one reply line back, trimmed. Split out
/// of `call` (R2, same code) only so `status` can decide for itself
/// whether a line that fails to parse may be quoted in its diagnostic.
fn call_line(socket: &Path, request: &Request) -> Result<String, ClientError> {
    let mut stream = UnixStream::connect(socket)?;

    let mut line = serde_json::to_vec(request).map_err(|err| {
        ClientError::Io(io::Error::other(format!(
            "wirkd request failed to serialize: {err}"
        )))
    })?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.shutdown(Shutdown::Write)?;

    let mut reply_line = String::new();
    BufReader::new(&stream).read_line(&mut reply_line)?;
    Ok(reply_line.trim_end_matches(['\n', '\r']).to_string())
}

/// The applied-scope and named-target check every scoped consultation
/// shares, and the one place it lives (the integration review's V-5 and
/// the acknowledgment review's F-2).
///
/// `target` is `Some(work)` for a scoped request — the Work the answer
/// must be *about*, which is not the requesting Work: a consultation
/// names both, and only the target says which journal the reply
/// describes — and `None` for an administrative one.
///
/// A scoped request is only usable if the daemon that served *this*
/// request applied that scope, and the only thing that proves it is the
/// serving reply itself: the handler names the applied scope in its own
/// `scope` field, so a reply without it was served by a daemon
/// predating the gate, which answers a narrowed read in full. The same
/// reply must also name that target back in its own `work_id`; a reply
/// naming another Work, or none, answers some other question. Either is
/// refused here, unread, before any caller can present or consume it —
/// never rendered first and warned about afterwards. This checks the
/// answer against the request, never the caller against the answer.
///
/// A scoped reply that is not a `Reply` at all is refused without its
/// content too (`parse_reply`'s `echo_rejected`): a request that cannot
/// establish its contract must present no part of the answer it
/// rejected.
///
/// An `admin` request asks for the whole reply and gets it from either
/// daemon, so neither is checked: ordinary explicitly administrative use
/// keeps working across versions, which is the honest reading of what
/// was asked for. This is a contract check, not authentication — naming
/// a requester still proves nothing about who is asking (`StatusPayload`'s
/// own doc).
fn scoped_call(
    socket: &Path,
    request: &Request,
    target: Option<&WorkId>,
) -> Result<Reply, ClientError> {
    let reply = parse_reply(&call_line(socket, request)?, target.is_none())?;
    if let Some(target) = target
        && let Reply::Ok { result, .. } = &reply
    {
        if result.get("scope").and_then(|scope| scope.as_str()) != Some("requester") {
            return Err(ClientError::ScopeNotApplied(
                "this scoped reply names no applied scope",
            ));
        }
        match result.get("work_id").and_then(|work| work.as_str()) {
            Some(named) if named == target.0 => {}
            Some(_) => {
                return Err(ClientError::ScopeNotApplied(
                    "this scoped reply is about another work than the one requested",
                ));
            }
            None => {
                return Err(ClientError::ScopeNotApplied(
                    "this scoped reply names no work it is about",
                ));
            }
        }
    }
    Ok(reply)
}

/// The typed door for the `status` verb: `scoped_call` with `status`'s
/// own target, which is `payload.work_id` — the Work asked about, not
/// the requester.
pub fn status(socket: &Path, payload: StatusPayload) -> Result<Reply, ClientError> {
    let target = (!payload.admin).then(|| payload.work_id.clone());
    scoped_call(socket, &Request::status(payload), target.as_ref())
}

/// The typed door for the `work obligations` verb (the basis review's
/// F1). This verb reproduces `status`'s scoped reply shape and reuses
/// `status`'s server-side lineage gate; it goes through `scoped_call`
/// so it reuses `status`'s **caller-side** contract too. It discloses a
/// settlement basis and an admission state, so a reply about another
/// Work rendered as the answer to this one is exactly the confusion the
/// acknowledgment contract exists to prevent — and a malformed scoped
/// reply must not travel back in a diagnostic either.
pub fn work_obligations(
    socket: &Path,
    payload: WorkObligationsPayload,
) -> Result<Reply, ClientError> {
    let target = (!payload.admin).then(|| payload.work_id.clone());
    scoped_call(socket, &Request::work_obligations(payload), target.as_ref())
}

/// Item B: dials `socket`, sends `{"verb":"watch",...}`, and hands back
/// a **blocking** iterator over the Work's journal — every event
/// already appended, then one more per line as `server::
/// handle_watch_connection` pushes it, with no read timeout (ruling
/// 0044: this connection blocks on wirkd's own state, exactly the way
/// `wirk-herdr`'s Herdr subscription blocks on Herdr's). Never
/// half-closes the write side (unlike `call`, above): a still-open
/// write half is harmless (`handle_watch_connection` never reads again
/// after the request line), and shutting it here would be pure noise.
/// The iterator ends (`None`) the moment the connection's read returns
/// `Ok(0)` — wirkd stopped, or refused the request outright and closed
/// after its one `Reply` line (surfaced as the iterator's first and
/// only `Some(Err(..))`, same as a malformed line).
pub fn watch(
    socket: &Path,
    payload: WatchPayload,
) -> Result<impl Iterator<Item = Result<Event, ClientError>> + use<>, ClientError> {
    let stream = UnixStream::connect(socket)?;
    // Whether this stream has to establish its scope before it may be
    // read at all (V-5), decided from the request that is about to go
    // out — not from a separate preflight, which could reach a
    // different daemon than the one that serves the stream.
    // `Some(target)` is "this stream must acknowledge the scope, for
    // this Work" — the Work asked about, not the requester (F-2).
    let require_scope_ack = (!payload.admin).then(|| payload.work_id.clone());
    let request = Request::watch(payload);
    let mut line = serde_json::to_vec(&request).map_err(|err| {
        ClientError::Io(io::Error::other(format!(
            "wirkd watch request failed to serialize: {err}"
        )))
    })?;
    line.push(b'\n');
    (&stream).write_all(&line)?;

    Ok(WatchLines {
        reader: BufReader::new(stream),
        require_scope_ack,
        done: false,
    })
}

/// The blocking line iterator `watch` returns: each `next()` is one
/// `read_line` call, which blocks (no timeout set on this socket, per
/// `watch`'s own doc) until wirkd pushes a line or the connection ends.
struct WatchLines {
    reader: BufReader<UnixStream>,
    /// `Some(work)` for a scoped request: the first line must be
    /// `handle_watch_connection`'s scope acknowledgment **for that
    /// Work**, and nothing on this stream is yielded before it has
    /// been. Cleared once that line has been read.
    require_scope_ack: Option<WorkId>,
    /// Set once this iterator has yielded a terminal error of its own
    /// (a refusal, or an unestablished scope): no further line is read,
    /// so an unscoped journal cannot arrive behind the refusal that
    /// rejected it.
    done: bool,
}

impl WatchLines {
    /// One blocking `read_line`, trimmed. `None` is EOF.
    fn read_line(&mut self) -> Option<Result<String, ClientError>> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(Ok(line.trim_end_matches(['\n', '\r']).to_string())),
            Err(err) => Some(Err(ClientError::Io(err))),
        }
    }
}

impl Iterator for WatchLines {
    type Item = Result<Event, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        // The scope acknowledgment, consumed before any event line is
        // read (V-5). A daemon predating the scoped stream sends the
        // Work's raw journal straight away, so its first line parses as
        // an `Event` — which is exactly the answer this must not hand
        // on. The refusal names no part of that line: the whole point
        // is that the unscoped journal is not presented, not even in a
        // diagnostic.
        if let Some(target) = self.require_scope_ack.take() {
            let line = match self.read_line() {
                Some(Ok(line)) => line,
                Some(Err(err)) => {
                    self.done = true;
                    return Some(Err(err));
                }
                // EOF before the acknowledgment (F-1). Nothing was
                // read, so nothing was disclosed — but a scope that was
                // never established is not a narrow stream that ran and
                // ended, and `?` reported it as exactly that: `None`,
                // and a zero exit for every scripted consumer. The
                // daemon reaches this state itself, by returning when
                // `write_scope_ack` fails.
                None => {
                    self.done = true;
                    return Some(Err(ClientError::ScopeNotApplied(
                        "this watch stream ended before acknowledging the requested scope",
                    )));
                }
            };
            match serde_json::from_str::<Reply>(&line) {
                Ok(Reply::Ok { result, .. })
                    if result.get("scope").and_then(|scope| scope.as_str())
                        == Some("requester") =>
                {
                    // The acknowledgment names the Work this stream is
                    // about, and the client asked about one (F-2): a
                    // stream acknowledged for another Work is not the
                    // stream that was requested, whatever it carries.
                    match result.get("work_id").and_then(|work| work.as_str()) {
                        Some(named) if named == target.0 => {}
                        Some(_) => {
                            self.done = true;
                            return Some(Err(ClientError::ScopeNotApplied(
                                "this watch stream was acknowledged for another work than the one requested",
                            )));
                        }
                        None => {
                            self.done = true;
                            return Some(Err(ClientError::ScopeNotApplied(
                                "this watch stream's acknowledgment names no work",
                            )));
                        }
                    }
                }
                Ok(Reply::Err { error, .. }) => {
                    self.done = true;
                    return Some(Err(ClientError::Refused(error)));
                }
                _ => {
                    self.done = true;
                    return Some(Err(ClientError::ScopeNotApplied(
                        "this watch stream opened with no scope acknowledgment",
                    )));
                }
            }
        }
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None, // EOF: wirkd is gone, or ended this connection
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                // A bare `Reply::Err` line (`handle_watch_connection`'s
                // own early-return fast path — an unknown or path-like
                // Work id, or a malformed watch request) parses as
                // neither `Event` nor anything this iterator invents a
                // variant for. 0069 correction: this is a valid,
                // well-formed refusal wirkd sent on purpose, not a wire
                // protocol violation — surfaced as `ClientError::
                // Refused` (carrying wirkd's own `code`/`message`
                // untouched) so a caller can tell "the daemon explicitly
                // refused" apart from `MalformedReply`, which is now
                // reserved for a line that is neither shape at all.
                match serde_json::from_str::<Event>(trimmed) {
                    Ok(event) => Some(Ok(event)),
                    Err(_) => match serde_json::from_str::<Reply>(trimmed) {
                        Ok(Reply::Err { error, .. }) => {
                            self.done = true;
                            Some(Err(ClientError::Refused(error)))
                        }
                        _ => {
                            self.done = true;
                            Some(Err(ClientError::MalformedReply(format!(
                                "not a watch Event line: {trimmed:?}"
                            ))))
                        }
                    },
                }
            }
            Err(err) => Some(Err(ClientError::Io(err))),
        }
    }
}
