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
//! the file's content; an HTTP source holds them in the one response its
//! own last explicit fetch staged, addressed by the SHA-256 of those
//! bytes.
//!
//! Routing that choice through one place is what keeps the policies from
//! drifting. Dispatching on `acquisition_policy` — the field the
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

use crate::extract::ExtractorEdition;
use crate::{AtlasError, GenerationId, doctree, document, git, http_source};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// The one recorded resource a single read names: the path it was
/// recorded at, and the object identity recorded for it.
///
/// A pair rather than two arguments because it is already a pair
/// everywhere else — [`blobs`] takes a slice of exactly it, and
/// `document::resource_key` keys on both halves precisely because one
/// without the other addresses the wrong bytes.
#[derive(Clone, Copy)]
pub(crate) struct RecordedResource<'a> {
    pub path: &'a [u8],
    pub object_id: &'a str,
}

/// Where one HTTP generation's staged response bytes live — the same
/// join `AtlasStore` itself uses to write `content.bin`
/// (`store::content_bin`/`store::stage`), restated here because this
/// module reads bytes back for a caller that holds only the atlas root
/// and a generation id, never a mutable store handle.
fn http_content_path(atlas_root: &Path, generation: &GenerationId) -> std::path::PathBuf {
    atlas_root
        .join("generations")
        .join(&generation.0)
        .join("content.bin")
}

/// The raw, unrendered bytes behind `wanted`, keyed by object id, read
/// from whichever source this generation was acquired from. The HTTP arm
/// reads its one staged response **from disk, never the network**:
/// `atlas_root`/`generation` locate `AtlasStore::stage`'s own
/// `content.bin`, and `locator` (the source URL) is not
/// filesystem-addressable and is not used there.
fn raw_blobs(
    acquisition_policy: &str,
    locator: &Path,
    atlas_root: &Path,
    generation: &GenerationId,
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
        http_source::ACQUISITION_POLICY => {
            let mut out = BTreeMap::new();
            let Ok(bytes) = std::fs::read(http_content_path(atlas_root, generation)) else {
                return Ok(out);
            };
            let digest = http_source::hash_hex(&bytes);
            for (_, object_id) in wanted {
                if *object_id == digest {
                    out.entry(object_id.clone())
                        .or_insert_with(|| bytes.clone());
                }
            }
            Ok(out)
        }
        other => Err(AtlasError::Generation(format!(
            "generation names an unknown acquisition policy {other:?}"
        ))),
    }
}

/// The bytes behind many resources of one generation or edition, keyed
/// by [`document::hydration_key`] — the object id each was recorded
/// under, folded together with which derived interpretation (a document
/// format, or none) the requesting resource carries.
///
/// `wanted` carries the path beside the object id because a document
/// collection is addressed by path and a Git object store is not; the
/// Git arm simply ignores the paths for the raw fetch itself. The raw
/// fetch is still deduplicated by object id alone — one file or object
/// backing many units is read once, regardless of how many distinct
/// paths name it — but two `wanted` entries that share an object id
/// while naming different interpretations (one a document format,
/// another not; or two different document formats) must not collapse
/// onto one result: they are two different byte strings once rendered,
/// and a cache keyed by object id alone can hold only one of them,
/// silently handing the other's callers the wrong content.
pub(crate) fn blobs(
    edition: ExtractorEdition,
    acquisition_policy: &str,
    locator: &Path,
    atlas_root: &Path,
    generation: &GenerationId,
    wanted: &[(Vec<u8>, String)],
    limits: &doctree::CaptureLimits,
) -> Result<BTreeMap<Vec<u8>, Arc<Vec<u8>>>, AtlasError> {
    let raw = raw_blobs(
        acquisition_policy,
        locator,
        atlas_root,
        generation,
        wanted,
        limits,
    )?;
    Ok(render_documents(edition, wanted, raw))
}

/// `blobs`'/`blob`'s shared last step: a document resource's raw bytes
/// are never handed back as-is — a `Document` unit's offsets index the
/// Markdown `crate::document::render` produces, not the original file,
/// so every caller of this module (lexical indexing, semantic editions,
/// ranked retrieval) has to see that same Markdown or it tokenizes,
/// embeds or snippets the wrong string. A resource whose bytes no longer
/// convert (the file changed, or the recorded content simply is not what
/// it was recorded as) is dropped rather than returned raw, matching
/// this module's existing tolerance: the caller degrades its own
/// coverage disclosure rather than being handed bytes that do not match
/// what was indexed.
///
/// Iterates `wanted` itself, one render per distinct `(object id,
/// interpretation)` pair — not `raw.into_iter()` keyed by object id
/// alone — so two `wanted` entries sharing raw bytes under different
/// interpretations each render and land under their own
/// `document::hydration_key`, rather than one interpretation winning and
/// silently standing in for the other's callers.
fn render_documents(
    edition: ExtractorEdition,
    wanted: &[(Vec<u8>, String)],
    raw: BTreeMap<String, Vec<u8>>,
) -> BTreeMap<Vec<u8>, Arc<Vec<u8>>> {
    let mut result: BTreeMap<Vec<u8>, Arc<Vec<u8>>> = BTreeMap::new();
    // One render per distinct `(object id, resolved interpretation)`, shared
    // by every resource that reads those bytes that way: the exact keying
    // above costs addressing, not a second copy of the same rendering.
    let mut rendered: BTreeMap<(String, String), Arc<Vec<u8>>> = BTreeMap::new();
    for (path, object_id) in wanted {
        let key = document::resource_key(path, object_id);
        if result.contains_key(&key) {
            continue;
        }
        let Some(bytes) = raw.get(object_id.as_str()) else {
            continue;
        };
        let interpretation = match document::resolved_format(edition, path, bytes) {
            Some(format) => format!("{format:?}"),
            None => "raw".to_string(),
        };
        let shared = match rendered.get(&(object_id.clone(), interpretation.clone())) {
            Some(shared) => Some(shared.clone()),
            None => match document::render_if_document(edition, path, bytes.clone()) {
                Ok(bytes) => {
                    let shared = Arc::new(bytes);
                    rendered.insert((object_id.clone(), interpretation), shared.clone());
                    Some(shared)
                }
                Err(_) => None,
            },
        };
        if let Some(shared) = shared {
            result.insert(key, shared);
        }
    }
    result
}

/// The bytes behind exactly one recorded resource, rendered the same way
/// [`blobs`] renders its batch. `atlas_root`/`generation` are the HTTP
/// arm's own staged-response address, mirroring [`blobs`]'s.
pub(crate) fn blob(
    edition: ExtractorEdition,
    acquisition_policy: &str,
    locator: &Path,
    atlas_root: &Path,
    generation: &GenerationId,
    resource: RecordedResource<'_>,
    limits: &doctree::CaptureLimits,
) -> Result<Vec<u8>, AtlasError> {
    let raw = raw_blob(
        acquisition_policy,
        locator,
        atlas_root,
        generation,
        resource,
        limits,
    )?;
    document::render_if_document(edition, resource.path, raw)
        .map_err(AtlasError::SourceBytesUnavailable)
}

/// [`blob`] without the document rendering: exactly the bytes the source
/// holds. The document-model reader ([`crate::document::inspect`]) needs
/// the original container, not its Markdown rendering, and so does any
/// caller re-checking a recorded `object_id` against what the source
/// still has.
pub(crate) fn raw_blob(
    acquisition_policy: &str,
    locator: &Path,
    atlas_root: &Path,
    generation: &GenerationId,
    resource: RecordedResource<'_>,
    limits: &doctree::CaptureLimits,
) -> Result<Vec<u8>, AtlasError> {
    let RecordedResource { path, object_id } = resource;
    match acquisition_policy {
        doctree::ACQUISITION_POLICY => doctree::blob(locator, path, object_id, limits),
        git::ACQUISITION_POLICY => git::blob(locator, object_id),
        http_source::ACQUISITION_POLICY => {
            let bytes =
                std::fs::read(http_content_path(atlas_root, generation)).map_err(|error| {
                    AtlasError::SourceBytesUnavailable(format!(
                        "staged response bytes are missing or unreadable: {error}"
                    ))
                })?;
            let digest = http_source::hash_hex(&bytes);
            if digest != object_id {
                return Err(AtlasError::SourceBytesUnavailable(
                    "staged response bytes no longer match the recorded content hash".into(),
                ));
            }
            Ok(bytes)
        }
        other => Err(AtlasError::Generation(format!(
            "generation names an unknown acquisition policy {other:?}"
        ))),
    }
}
