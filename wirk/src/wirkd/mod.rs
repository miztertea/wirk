//! wirkd wire protocol: the envelope and verb types (W2, `orient/
//! transport.md` §2, §4; `orient/build-brief.md` W2), plus the server
//! loop (W3, `server` submodule) and the client (`client` submodule)
//! that both serialize against them. One NDJSON-framed JSON object per
//! request and per reply, the Journal's own line-delimited convention
//! reused (R2).
//!
//! Five verbs (transport.md §2): `ping`, `submit`, `claim`, `status`,
//! `stop`. `ping` and `stop` carry no payload fields; `submit`,
//! `claim`, `status` each have a typed payload struct so a caller does
//! not hand-build JSON, but `Request.payload` itself stays a
//! `serde_json::Value` — the one shape both a typed payload (via
//! `Request::submit`/`claim`/`status`) and a scripted fake server's
//! literal JSON (the W2 test) can produce identically.

// `wirk/tests/wirkd_client.rs` compiles this module into its own crate
// root via `#[path]` (not through `main.rs`) to unit-test the wire
// types and the client directly; that test binary never calls into
// `server`'s items, so its own dead-code analysis would otherwise flag
// them there even though `main.rs` uses every one of them for real.
// Allowed at the module level, not scattered per item (`#![...]`
// cascades to `client`/`server` as this module's own descendants).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use wirk_core::{ClaimKind, ExecutionTriple, RepositoryBinding, WorkId};

pub mod client;
pub mod server;

// ---- Request ---------------------------------------------------------

/// The five verbs a request names (transport.md §2). Serialized as its
/// lowercase name (`"ping"`, `"submit"`, ...), matching the envelope's
/// `{"verb": "<name>", ...}` shape verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verb {
    Ping,
    Submit,
    Claim,
    Status,
    Stop,
}

/// One NDJSON-framed request line: `{"verb": "<name>", "payload": {...}}`
/// (transport.md §2). `payload` is untyped on the wire so `ping`/`stop`
/// (no fields) and `submit`/`claim`/`status` (typed below) share one
/// envelope shape; the `Request::*` constructors are the typed door in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub verb: Verb,
    pub payload: Value,
}

impl Request {
    /// `{"verb":"ping","payload":{}}` — no fields (transport.md §2).
    pub fn ping() -> Self {
        Request {
            verb: Verb::Ping,
            payload: Value::Object(serde_json::Map::new()),
        }
    }

    /// `{"verb":"stop","payload":{}}` — no fields (transport.md §2).
    pub fn stop() -> Self {
        Request {
            verb: Verb::Stop,
            payload: Value::Object(serde_json::Map::new()),
        }
    }

    /// Serializes `payload` into the envelope's untyped `payload` field.
    /// Panics only if `SubmitPayload`/`ClaimPayload`/`StatusPayload`'s
    /// own `Serialize` impl fails, which none of the three can (plain
    /// data, no maps with non-string keys, no floats) — `expect` names
    /// that invariant rather than threading an infallible `Result`.
    pub fn submit(payload: SubmitPayload) -> Self {
        Request {
            verb: Verb::Submit,
            payload: serde_json::to_value(payload).expect("SubmitPayload always serializes"),
        }
    }

    pub fn claim(payload: ClaimPayload) -> Self {
        Request {
            verb: Verb::Claim,
            payload: serde_json::to_value(payload).expect("ClaimPayload always serializes"),
        }
    }

    pub fn status(payload: StatusPayload) -> Self {
        Request {
            verb: Verb::Status,
            payload: serde_json::to_value(payload).expect("StatusPayload always serializes"),
        }
    }
}

/// `submit`'s payload (transport.md §2): the intent text, the
/// repository bindings the Work declares, and the base ref its worktree
/// is cut from. `Route` authoring is not built yet (build-brief.md §3
/// W3, R6) — `submit` names only what W3's hardcoded "smoke" Route
/// needs to open a Run against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitPayload {
    pub intent: String,
    pub repositories: Vec<RepositoryBinding>,
    pub base_ref: String,
}

/// `claim`'s payload (transport.md §2): the injected
/// `ExecutionTriple`, the `ClaimKind` (`Done` or `Question`, 0027 D87),
/// and the artifacts by name-to-path pair — `BTreeMap` for a
/// deterministic wire order, matching `ArtifactRef.name` ->
/// `ArtifactRef.path` (`wirk-core::ArtifactRef`) one pair at a time.
/// `Claim.id` is minted server-side (transport.md §2: "the client sends
/// an empty id"), so no id field travels here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimPayload {
    pub triple: ExecutionTriple,
    pub kind: ClaimKind,
    pub artifacts: BTreeMap<String, String>,
}

/// `status`'s payload (transport.md §2): the `Work` to report on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub work_id: WorkId,
}

// ---- Reply -------------------------------------------------------------

/// One NDJSON-framed reply line: `{"ok": true, "result": {...}}` or
/// `{"ok": false, "error": {...}}` (transport.md §2). `untagged`: serde
/// tries `Ok` first (needs a `result` field), then `Err` (needs
/// `error`) — the two shapes are disjoint on the wire, so this never
/// picks the wrong one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reply {
    Ok { ok: bool, result: Value },
    Err { ok: bool, error: ErrorDetail },
}

impl Reply {
    /// `true` for `Reply::Ok`, `false` for `Reply::Err` — matches the
    /// envelope's own `ok` field, exposed so a caller need not match on
    /// the enum just to check success.
    pub fn is_ok(&self) -> bool {
        matches!(self, Reply::Ok { .. })
    }
}

/// The error reply's `error` object (transport.md §2): `code` names the
/// `ClaimRefusal` variant or another short string, `message` is a
/// human-readable line, `detail` is the bounded diagnostic text 0027
/// D92's `FailureCause.detail` also carries (issue 279) — optional,
/// omitted from the wire when absent rather than serialized as `null`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---- Pointer file (0022 D79) --------------------------------------------

/// `<estate_root>/.wirk/wirkd.json`, written atomically once the
/// listener is bound (transport.md §3) — read-only from this module's
/// side; W3 writes it, `client::locate` reads it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirkdPointer {
    pub schema: String,
    pub socket: PathBuf,
    pub pid: u32,
    pub protocol_version: u32,
}
