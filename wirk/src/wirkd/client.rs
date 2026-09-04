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
use std::time::Duration;

use super::{Reply, Request, WirkdPointer};

/// `client::call`'s read timeout: the peer is a Unix-domain socket on
/// the same host, not a network hop, so this only guards against a
/// wedged or malicious peer that connects and never writes a reply
/// line — a real wirkd answers in microseconds. Long enough that no
/// real reply is ever cut off, short enough a test using this default
/// fails promptly rather than hanging.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

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
    stream.set_read_timeout(Some(READ_TIMEOUT))?;

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
