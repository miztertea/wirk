//! P6.7 (ruling 0393): the estate owner's own scoped doctrine documents
//! — chosen explicitly, resolved before an Actor reservation, and
//! delivered beside the shared worker contract without ever being
//! confused with it.
//!
//! **Why this is not the worker contract.** `worker_contract` is
//! product-shipped protocol: static per build, identical in every
//! estate, and nobody's to choose. What an estate's *owner* wants every
//! actor here to operate under — this estate's rules, this estate's
//! standards — is a different document with a different author, and it
//! earns a different identity: the owner's own `id` and `version` beside
//! wirk's digest of the exact bytes. Composing both into one transport
//! document at launch (§`compose`) does not merge those identities;
//! `ActorWorld.doctrine` and `ActorWorld.contract` stay separately named
//! for exactly that reason.
//!
//! **Why declared rather than discovered.** Wirk does not crawl for
//! `AGENTS.md` ancestors, and does not promote a file into governing
//! doctrine because it happened to be nearby or because a source
//! retrieval surfaced it. An owner names the documents, one at a time,
//! through `wirk estate doctrine set`. Silence means no doctrine, and
//! that is an ordinary estate whose launches are byte-identical to what
//! they were before this module existed.
//!
//! Three facts, three homes, the same three `worker_contract` already
//! uses:
//!
//! * **Declaration** is `<estate_root>/.wirk/doctrine.json` — the
//!   owner's selection, read at every reservation so a deliberate change
//!   reaches the *next* applicable reservation without a daemon restart.
//!   Nothing else in the estate is authoritative about what applies.
//! * **Content** is `<estate_root>/.wirk/doctrine/<digest>.md`, written
//!   durably at reservation before anything references it, through the
//!   shared `content_store`.
//! * **Reference** is `ActorWorld.doctrine` (`EstateDoctrineRef`),
//!   hashed into `WorldHash::of`, so what a stage operates under is
//!   fixed at reservation and identical across every attempt that
//!   reservation backs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wirk_core::EstateDoctrineRef;

use crate::content_store;

/// The version recorded when the owner declared none. Never inferred
/// from the bytes: wirk does not know what edition someone's document
/// is, and guessing one would be a fabricated provenance claim. The
/// digest is the content identity; this is the owner's label for it.
pub const UNDECLARED_VERSION: &str = "undeclared";

// **There is deliberately no maximum document size.**
//
// A transport's limit is not a statement about how long an estate
// owner's own rules may be. The constraint is handled where it actually
// exists, at the codex arm, by the mechanism already there:
// `codex_composition` renders the launch through codex's own dry
// renderer *before* the element is ever added to a launch, and a render
// that cannot be spawned is a disclosed fallback to prompt delivery, not
// a dropped rule (`worker_contract::codex_composition`; measured in
// `wirk-herdr/tests/codex_live_composition.rs`). The claude arm hands
// over a file path and has no argv size question at all.

/// One document the estate owner selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredDocument {
    /// The owner's own name for it, unique in the estate. Used to
    /// replace and to remove; never parsed for meaning.
    pub id: String,
    /// Where the bytes are, absolute. Outside the estate is ordinary and
    /// expected: an owner's doctrine usually lives with the owner's
    /// other documents, not inside wirk's runtime directory, and it is
    /// explicitly *not* required to be an ancestor of any worktree.
    pub path: PathBuf,
    /// The owner's declared edition, or `None` for
    /// [`UNDECLARED_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The repository binding name this applies to, or `None` for every
    /// Actor reservation in the estate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl DeclaredDocument {
    fn version(&self) -> String {
        self.version
            .clone()
            .unwrap_or_else(|| UNDECLARED_VERSION.to_string())
    }
}

/// The estate's whole selection, in the owner's own order — which is the
/// order documents are delivered in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    #[serde(default)]
    pub documents: Vec<DeclaredDocument>,
}

impl Declaration {
    /// The documents that apply to a Work with these repository
    /// bindings: every estate-wide document, plus every document scoped
    /// to a binding this Work actually holds. Declaration order.
    ///
    /// Scope is by the Work's *declared bindings*, not by a path match
    /// or a name that looks familiar — a coordinate is not authority
    /// (0393). A document scoped to a repository this Work has no
    /// binding for is not disclosed to it and is not delivered to it.
    pub fn applicable<'a>(&'a self, repositories: &[String]) -> Vec<&'a DeclaredDocument> {
        self.documents
            .iter()
            .filter(|document| match &document.repository {
                None => true,
                Some(name) => repositories.iter().any(|bound| bound == name),
            })
            .collect()
    }
}

/// `<estate_root>/.wirk/doctrine.json`.
pub fn declaration_path(estate_root: &Path) -> PathBuf {
    estate_root.join(".wirk").join("doctrine.json")
}

/// `<estate_root>/.wirk/doctrine/` — estate-owned, beside `.wirk/contracts`.
/// Never the worktree (0050's boundary check never sees it), never `~/`.
pub fn store_dir(estate_root: &Path) -> PathBuf {
    estate_root.join(".wirk").join("doctrine")
}

/// Where one selected document's bytes live.
pub fn stored_path(estate_root: &Path, digest: &str) -> PathBuf {
    content_store::path_in(&store_dir(estate_root), digest)
}

/// Reads the estate's declaration. **An absent file is an empty
/// declaration**, not an error: an estate with no doctrine is the
/// ordinary case and must not be refused.
///
/// A file that exists but cannot be parsed *is* an error. Treating
/// malformed selection as "no selection" would silently drop governing
/// doctrine an owner believes is in force, which is the one failure mode
/// this whole path exists to prevent.
pub fn read_declaration(estate_root: &Path) -> Result<Declaration, String> {
    let path = declaration_path(estate_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Declaration::default());
        }
        Err(error) => {
            return Err(format!(
                "this estate's doctrine declaration at {} could not be read: {error}",
                path.display()
            ));
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "this estate's doctrine declaration at {} is not readable as a declaration: {error} \
             — fix or remove the file rather than leaving it ambiguous; wirk will not treat an \
             unparseable selection as an empty one",
            path.display()
        )
    })
}

/// Writes the declaration atomically. Same temp/rename/fsync discipline
/// as the content store, because a half-written selection is a selection
/// nobody can act on.
pub fn write_declaration(estate_root: &Path, declaration: &Declaration) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = estate_root.join(".wirk");
    std::fs::create_dir_all(&dir)?;
    let path = declaration_path(estate_root);
    let mut bytes = serde_json::to_vec_pretty(declaration)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    bytes.push(b'\n');
    let temp = dir.join(format!(".tmp-doctrine-{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, &path)?;
    std::fs::File::open(&dir).and_then(|directory| directory.sync_all())?;
    Ok(())
}

/// Reads one declared document's bytes.
///
/// One read, no stat-then-read pair: no size rule applies, so there is
/// no metadata check to race the read it would have guarded. What is
/// left is the one property delivery actually requires — the bytes are
/// text.
///
/// Every refusal names the document's own id and its path, because the
/// recovery is always one of two things the owner can do immediately:
/// fix the file, or `wirk estate doctrine remove --id <id>`.
pub fn read_declared(document: &DeclaredDocument) -> Result<Vec<u8>, String> {
    let path = &document.path;
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "estate doctrine document `{}` at {} could not be read: {error} — fix the path or \
             remove the declaration with `wirk estate doctrine remove --id {}`",
            document.id,
            path.display(),
            document.id
        )
    })?;
    if std::str::from_utf8(&bytes).is_err() {
        return Err(format!(
            "estate doctrine document `{}` at {} is not UTF-8 text, so it cannot be delivered as \
             instructions",
            document.id,
            path.display()
        ));
    }
    Ok(bytes)
}

/// Resolves and durably stores the doctrine that applies to a Work with
/// these repository bindings, and returns the references a reservation
/// records.
///
/// Called by wirkd when it reserves an Actor World, **before** the World
/// that references them is built — the same "durable first, then
/// referenced" ordering the contract and the projection already have.
///
/// `Ok(vec![])` is the ordinary answer for an estate that declared none.
/// An `Err` refuses the reservation rather than reserving a World whose
/// doctrine could never be honoured: the alternative is an actor that
/// silently operates without rules its owner believes are in force,
/// which is worse than a submit that says which document is broken.
pub fn reserve(
    estate_root: &Path,
    repositories: &[String],
) -> Result<Vec<EstateDoctrineRef>, String> {
    let declaration = read_declaration(estate_root)?;
    let applicable = declaration.applicable(repositories);
    if applicable.is_empty() {
        return Ok(Vec::new());
    }
    let dir = store_dir(estate_root);
    let mut reserved = Vec::with_capacity(applicable.len());
    for document in applicable {
        let bytes = read_declared(document)?;
        let digest = content_store::store(&dir, &bytes).map_err(|error| {
            format!(
                "estate doctrine document `{}` could not be stored under {}: {error}",
                document.id,
                dir.display()
            )
        })?;
        reserved.push(EstateDoctrineRef {
            id: document.id.clone(),
            version: document.version(),
            digest,
            repository: document.repository.clone(),
        });
    }
    Ok(reserved)
}

/// Reads one reserved document's bytes and proves they are the bytes the
/// reservation named.
pub fn read_verified(
    estate_root: &Path,
    reference: &EstateDoctrineRef,
) -> Result<String, DoctrineError> {
    match content_store::read_verified(&store_dir(estate_root), &reference.digest) {
        Ok((_, text)) => Ok(text),
        Err(content_store::StoreError::Unreadable { path, reason }) => {
            Err(DoctrineError::Unreadable {
                id: reference.id.clone(),
                path,
                reason,
            })
        }
        Err(content_store::StoreError::DigestMismatch { path, found }) => {
            Err(DoctrineError::DigestMismatch {
                id: reference.id.clone(),
                path,
                expected: reference.digest.clone(),
                found,
            })
        }
    }
}

/// Why reserved doctrine could not be honoured at launch. A refusal for
/// the same reason `ContractError` is one: an actor operating under
/// bytes nobody reserved is what the digest exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DoctrineError {
    #[error(
        "the estate doctrine document `{id}` this Run was reserved with is not readable at \
         {path}: {reason} — refusing to launch an actor without the doctrine it was reserved under"
    )]
    Unreadable {
        id: String,
        path: String,
        reason: String,
    },
    #[error(
        "the estate doctrine document `{id}` at {path} hashes to {found}, not the {expected} this \
         Run's World reserved — refusing to launch an actor under bytes nobody reserved"
    )]
    DigestMismatch {
        id: String,
        path: String,
        expected: String,
        found: String,
    },
}

/// The composed transport document: the shared worker contract first,
/// then each reserved doctrine document under a heading that names whose
/// it is, which edition, and the digest of the bytes below it.
///
/// One document rather than several because two of the three native
/// mechanisms take exactly one value — claude's
/// `--append-system-prompt-file` is one path, codex's
/// `developer_instructions` is one string — and delivering a second
/// through a mechanism wirk has not shown to compose would be the guess
/// 0202 forbids. The headings are what keeps the identities separate
/// inside the envelope; `ActorWorld.doctrine` is what keeps them
/// separate outside it.
///
/// The contract leads, unchanged and whole: it is wirk's own protocol
/// and it is the same in every estate. The estate's documents follow in
/// the owner's declared order, and the preamble says plainly that they
/// are the estate owner's, that they are additive, and that the
/// repository's own instruction files and the reader's own configuration
/// are not replaced by them.
pub fn compose(contract_text: &str, documents: &[(EstateDoctrineRef, String)]) -> String {
    let mut out = String::with_capacity(contract_text.len() + 1024);
    out.push_str(contract_text.trim_end());
    out.push_str("\n\n---\n\n# Estate doctrine\n\n");
    out.push_str(
        "The documents below were selected by this estate's owner and resolved when this stage \
         was reserved. They are the owner's own rules for work in this estate, not wirk's \
         protocol above and not evidence retrieved from any source. They are additive: they do \
         not replace the instruction files of any repository you are working in, your own \
         configuration, or the assignment that follows. Where a document's stated scope does not \
         cover what you are doing, say so rather than stretching it.\n\n",
    );
    for (reference, text) in documents {
        out.push_str(&format!(
            "## {} (version {}, sha256 {})\n\nScope: {}.\n\n{}\n\n",
            reference.id,
            reference.version,
            reference.digest,
            match &reference.repository {
                Some(name) => format!("declared for the `{name}` repository binding"),
                None => "declared estate-wide".to_string(),
            },
            text.trim()
        ));
    }
    out
}
