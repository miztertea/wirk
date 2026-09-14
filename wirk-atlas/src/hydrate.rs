//! Reading the bytes an already-recorded resource names, from whichever
//! kind of source actually holds them.
//!
//! Every verb that answers with content — lexical search, path lookup,
//! semantic edition building, edition verification and ranked retrieval
//! — needs the same thing: the bytes behind a set of `(path, object id)`
//! pairs a generation or an edition already recorded. What differs is
//! only where those bytes live. A Git source holds them in its object
//! store, addressed by the blob's own Git object id; a local document
//! collection holds them in ordinary files, addressed by the SHA-256 of
//! the file's content.
//!
//! Routing that choice through one place is what keeps the two policies
//! from drifting. Dispatching on `acquisition_policy` — the field the
//! generation or edition itself records, not a guess from the locator's
//! shape — means a source is read the way it was actually acquired, and
//! a policy this build does not know is refused by name rather than
//! read as if it were Git.
//!
//! Both entry points preserve the tolerance their callers were written
//! against: a batched read omits what it could not read and lets the
//! caller degrade its own coverage disclosure, while a single read
//! surfaces `AtlasError::SourceBytesUnavailable` for the caller to
//! report as an unavailable resource.

use crate::{AtlasError, doctree, git};
use std::collections::BTreeMap;
use std::path::Path;

/// The bytes behind many resources of one generation or edition, keyed
/// by the identity each was recorded under.
///
/// `wanted` carries the path beside the object id because a document
/// collection is addressed by path and a Git object store is not; the
/// Git arm simply ignores the paths. Entries are deduplicated by object
/// id by both arms, so one file or object backing many units is read
/// once.
pub(crate) fn blobs(
    acquisition_policy: &str,
    locator: &Path,
    wanted: &[(Vec<u8>, String)],
    limits: &doctree::CaptureLimits,
) -> Result<BTreeMap<String, Vec<u8>>, AtlasError> {
    match acquisition_policy {
        doctree::ACQUISITION_POLICY => Ok(doctree::blobs(locator, wanted, limits)),
        git::ACQUISITION_POLICY => {
            let mut oids: Vec<String> = Vec::with_capacity(wanted.len());
            let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
            for (_, object_id) in wanted {
                if seen.insert(object_id.as_str()) {
                    oids.push(object_id.clone());
                }
            }
            git::blobs(locator, &oids)
        }
        other => Err(AtlasError::Generation(format!(
            "generation names an unknown acquisition policy {other:?}"
        ))),
    }
}

/// The bytes behind exactly one recorded resource.
pub(crate) fn blob(
    acquisition_policy: &str,
    locator: &Path,
    path: &[u8],
    object_id: &str,
    limits: &doctree::CaptureLimits,
) -> Result<Vec<u8>, AtlasError> {
    match acquisition_policy {
        doctree::ACQUISITION_POLICY => doctree::blob(locator, path, object_id, limits),
        git::ACQUISITION_POLICY => git::blob(locator, object_id),
        other => Err(AtlasError::Generation(format!(
            "generation names an unknown acquisition policy {other:?}"
        ))),
    }
}
