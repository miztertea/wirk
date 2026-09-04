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

use wirk_core::Event;

use super::{Reply, Request, WatchPayload, WirkdPointer};

/// Everything that can go wrong locating or calling wirkd. Kept as one
/// flat enum, no `thiserror` (not on `wirk`'s allow-list this wave,
/// R3 stdlib `impl Display`/`impl std::error::Error` suffices for four
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
    /// The reply line was read but is not valid `Reply` JSON — a
    /// protocol violation, not the application-level refusal
    /// `Reply::Err` already carries.
    MalformedReply(String),
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
    let reply_line = reply_line.trim_end_matches(['\n', '\r']);

    serde_json::from_str(reply_line)
        .map_err(|err| ClientError::MalformedReply(format!("{err}: {reply_line:?}")))
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
    })
}

/// The blocking line iterator `watch` returns: each `next()` is one
/// `read_line` call, which blocks (no timeout set on this socket, per
/// `watch`'s own doc) until wirkd pushes a line or the connection ends.
struct WatchLines {
    reader: BufReader<UnixStream>,
}

impl Iterator for WatchLines {
    type Item = Result<Event, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None, // EOF: wirkd is gone, or ended this connection
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                // A bare `Reply::Err` line (a malformed watch request,
                // `handle_watch_connection`'s own early-return fast
                // path) parses as neither `Event` nor anything this
                // iterator invents a variant for — surfaced as
                // `MalformedReply` so the caller sees wirkd's own
                // `code`/`message` rather than a generic decode error.
                match serde_json::from_str::<Event>(trimmed) {
                    Ok(event) => Some(Ok(event)),
                    Err(_) => match serde_json::from_str::<Reply>(trimmed) {
                        Ok(Reply::Err { error, .. }) => Some(Err(ClientError::MalformedReply(
                            format!("{}: {}", error.code, error.message),
                        ))),
                        _ => Some(Err(ClientError::MalformedReply(format!(
                            "not a watch Event line: {trimmed:?}"
                        )))),
                    },
                }
            }
            Err(err) => Some(Err(ClientError::Io(err))),
        }
    }
}
