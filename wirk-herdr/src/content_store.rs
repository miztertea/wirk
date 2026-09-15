//! The one content-addressed byte store this estate keeps for
//! instruction documents, factored out of `worker_contract` (R2) when
//! P6.7 gave it a second user.
//!
//! Everything here is the discipline `worker_contract::reserve` already
//! had and `ProjectionFile::write_new` set before it: a file named by
//! the SHA-256 of its own content, written through a temp file, fsynced,
//! renamed, and the directory fsynced after. Content addressing is what
//! makes the file immutable by construction and legitimately shared —
//! two Works reserved against the same bytes want one file, not two —
//! and it is why "the bytes an actor was given" stays checkable after
//! the fact rather than merely asserted.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256, the same encoding `WorldHash` uses.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Where one stored document lives inside `dir`. The name *is* the
/// digest, so the file can never disagree with a reference that names it
/// without the mismatch being detectable.
pub fn path_in(dir: &Path, digest: &str) -> PathBuf {
    dir.join(format!("{digest}.md"))
}

/// Writes `bytes` into `dir` under their own digest if they are not
/// already there, and returns that digest.
///
/// Deliberately not `create_new`: a name that already holds bytes
/// hashing to this digest is already correct and is left alone, because
/// the name is the content. Anything else is written atomically.
pub fn store(dir: &Path, bytes: &[u8]) -> std::io::Result<String> {
    let digest = sha256_hex(bytes);
    let path = path_in(dir, &digest);
    if std::fs::read(&path).is_ok_and(|found| sha256_hex(&found) == digest) {
        return Ok(digest);
    }
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".tmp-{digest}-{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, &path)?;
    std::fs::File::open(dir).and_then(|directory| directory.sync_all())?;
    Ok(digest)
}

/// Why stored bytes could not be honoured.
///
/// A refusal, not a degradation: the store is a long-lived directory and
/// the reader is a separately pinned binary, so a file a reservation
/// names can legitimately be gone, truncated or rewritten by the time a
/// launch reads it — and an actor operating under bytes nobody reserved
/// is exactly the failure a digest exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Unreadable { path: String, reason: String },
    DigestMismatch { path: String, found: String },
}

/// Reads the bytes `digest` names out of `dir` and proves they are those
/// bytes. Nothing downstream of this may assume it was called.
pub fn read_verified(dir: &Path, digest: &str) -> Result<(PathBuf, String), StoreError> {
    let path = path_in(dir, digest);
    let bytes = std::fs::read(&path).map_err(|error| StoreError::Unreadable {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let found = sha256_hex(&bytes);
    if found != digest {
        return Err(StoreError::DigestMismatch {
            path: path.display().to_string(),
            found,
        });
    }
    let text = String::from_utf8(bytes).map_err(|error| StoreError::Unreadable {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    Ok((path, text))
}
