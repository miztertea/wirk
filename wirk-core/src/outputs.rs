//! Work-owned declared outputs: the durable area a Run's actor writes a
//! declared output into when that output cannot live in the Run's own
//! checkout (ruling 0145).
//!
//! **Why this exists.** A Waypoint may declare a required output while
//! its Work's execution binding is `Access::Read`. Both are legitimate
//! — that combination is exactly an independent read-only reviewer that
//! must return a receipt — but jointly unsatisfiable while the only
//! place a Claim may name an artifact is the repository checkout, whose
//! diff a Read binding requires to be empty (0050 D150). The answer
//! ruling 0145 decided is *not* to relax the Read rule, and not to
//! accept a caller-supplied path outside the checkout: it is a second,
//! explicitly addressed place a declared output may live, **outside
//! every repository**, whose location the daemon derives from ids it
//! already holds.
//!
//! **Three separations, each testable.**
//!
//! 1. **Staging is not canonical.** `staging/<run>/<name>` is the
//!    actor's own mutable scratch: it may be written, rewritten and
//!    truncated for as long as the Run lives, exactly like a file in a
//!    worktree. `claims/<claim>/<name>` is the immutable snapshot the
//!    daemon takes of the bytes it actually validated. A later rewrite
//!    of the staged file is therefore not a rewrite of the evidence,
//!    and never silently re-attributed to the earlier Claim
//!    (`ArtifactReceipt`'s own contract).
//!
//! 2. **Durable before referenced.** The snapshot is written to a temp
//!    file, `fsync`ed, renamed into place and its directory `fsync`ed
//!    *before* the `ClaimRecorded` naming it exists — the identical
//!    discipline `ProjectionFile::write_new` already uses for
//!    `works/<work>/projections/` (R2: reuse it, do not invent a second
//!    durability protocol, and take no new dependency). A crash between
//!    the two leaves a file no event references, which nothing reads and
//!    nothing deletes; the reverse order would leave a journaled
//!    reference to a file that never existed.
//!
//! 3. **Addressing is derived, never supplied.** Nothing here accepts a
//!    caller path. A managed output is addressed by *(bound Work, bound
//!    Run, declared name)*, all three of which the daemon has already
//!    verified against the journal before it gets here, and the name is
//!    checked to be one ordinary filename component before it is ever
//!    joined. So the escape rules `artifact_join_escapes` and
//!    `artifact_canonical_containment` enforce for a checkout artifact
//!    have no caller-supplied input to act on at all — and a *symlink*
//!    placed inside the area by the actor is still a way out, so the
//!    containment rules are reapplied against this area's own root.
//!
//!    Where the answer only has to be true *now* — reporting whether a
//!    recorded snapshot is still available — that reapplication is
//!    canonical `lstat` plus prefix (`contained_regular_file`,
//!    `resolve_stored`). Where the answer has to stay true for the read
//!    that follows it — validating the actor's staged bytes at Claim
//!    time — a check followed by a second lookup would not be enough,
//!    because the actor owns its staging directory and may replace the
//!    entry in between. That path does not use these functions: wirkd
//!    walks to the staged file one component at a time with `openat` and
//!    `O_NOFOLLOW` and reads the descriptor it ends up holding, so no
//!    component — final or ancestor — can be a symlink and no lookup is
//!    repeated (`wirkd::server::read_staged_output`).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which root an `ArtifactReceipt`'s recorded `path` is relative to.
///
/// This is a field and not an inference from the path string. A
/// syntactically permissive `String` does not make its consumers support
/// a second namespace (ruling 0145), and every consumer that resolves a
/// receipt has to decide which root to resolve against — so the record
/// says which, explicitly, and a reader that does not understand the
/// answer cannot accidentally resolve it against the wrong one.
///
/// `Worktree` is the default so that every pre-0145 journal — which had
/// exactly one root and named none — reads back unchanged, at the same
/// path, with the same digest and the same availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStore {
    /// The Run's own checkout: `path` is worktree-relative where the
    /// join resolved inside it, otherwise the claimed path verbatim.
    /// Every historical receipt is one of these.
    #[default]
    Worktree,
    /// This Work's own managed output area: `path` is relative to
    /// `works/<work>/outputs/` and is always `claims/<claim>/<name>`.
    WorkOutputs,
}

impl ArtifactStore {
    pub fn label(self) -> &'static str {
        match self {
            ArtifactStore::Worktree => "worktree",
            ArtifactStore::WorkOutputs => "work_outputs",
        }
    }
}

/// Why a declared output name cannot address a managed output.
///
/// Named causes rather than one bool: ruling 0145 requires malformed
/// output addresses to be "refused with precise diagnostics", and a
/// Route author reading `Refused: OutOfBoundary` needs to be told which
/// rule their name broke, not that something was wrong with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputNameError {
    Empty,
    TooLong,
    /// Contains a path separator, or is otherwise more (or less) than
    /// one ordinary filename component — `a/b`, `.`, `..`, `/abs`.
    NotOneComponent,
    /// Begins with `.`: the area's own temporaries are `.tmp-*`, and a
    /// dotted name would also hide the actor's own output from an
    /// ordinary listing of the directory it is told to write into.
    Hidden,
    /// Outside `[A-Za-z0-9._-]`. Control bytes and NUL are the reason
    /// the rule exists; the rest of the restriction is deliberate, so
    /// that what a Route declares and what appears on disk are the same
    /// bytes under every locale and every filesystem normalization.
    UnsupportedCharacter,
}

impl OutputNameError {
    pub fn detail(self) -> &'static str {
        match self {
            OutputNameError::Empty => "an output name may not be empty",
            OutputNameError::TooLong => "an output name may be at most 128 bytes",
            OutputNameError::NotOneComponent => {
                "an output name must be one ordinary filename component: no `/`, no `\\`, and \
                 never `.` or `..`"
            }
            OutputNameError::Hidden => "an output name may not begin with `.`",
            OutputNameError::UnsupportedCharacter => {
                "an output name may use only ASCII letters, digits, `.`, `-` and `_`"
            }
        }
    }
}

/// The upper bound on one declared output name, in bytes. Well under
/// every filesystem's own component limit, so a name this accepts always
/// fits the two directories it is joined onto.
const MAX_OUTPUT_NAME: usize = 128;

/// Whether `name` may address a managed output, and why not when it may
/// not. Lexical only: no filesystem read, so a name is answerable before
/// anything exists.
pub fn check_output_name(name: &str) -> Result<(), OutputNameError> {
    if name.is_empty() {
        return Err(OutputNameError::Empty);
    }
    if name.len() > MAX_OUTPUT_NAME {
        return Err(OutputNameError::TooLong);
    }
    if name.starts_with('.') {
        return Err(OutputNameError::Hidden);
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
    {
        return Err(OutputNameError::UnsupportedCharacter);
    }
    // The charset above already excludes `/`, `\` and NUL, and the
    // `.`-prefix rule already excludes `.` and `..`. This is the
    // independent confirmation of the property that actually matters
    // when the name is joined — that `Path` sees exactly one `Normal`
    // component — rather than a re-derivation of it from the charset.
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(single)), None) if single == std::ffi::OsStr::new(name) => Ok(()),
        _ => Err(OutputNameError::NotOneComponent),
    }
}

/// An id (Work, Run, Claim) that is about to become a directory name.
/// Same rule and same reason as `ObservationId::is_well_formed` (R2):
/// nothing that could leave `works/<work>/outputs/` is one, checked
/// where it is joined rather than trusted because the daemon minted it.
fn well_formed_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `works/<work>/outputs` — this Work's whole managed output area, and
/// the root every recorded managed `path` is relative to.
pub fn outputs_dir(estate_root: &Path, work_id: &crate::WorkId) -> Option<PathBuf> {
    well_formed_id(&work_id.0).then(|| estate_root.join("works").join(&work_id.0).join("outputs"))
}

/// `works/<work>/outputs/staging/<run>` — the actor's own mutable
/// scratch for this Run. Created on demand by `ensure_staging_dir`; a
/// Run that never writes an output never has one.
pub fn staging_dir(
    estate_root: &Path,
    work_id: &crate::WorkId,
    run_id: &crate::RunId,
) -> Option<PathBuf> {
    if !well_formed_id(&run_id.0) {
        return None;
    }
    Some(
        outputs_dir(estate_root, work_id)?
            .join("staging")
            .join(&run_id.0),
    )
}

/// Creates this Run's staging directory if it does not exist and hands
/// back its path — what `wirk output dir` prints and what the actor
/// writes into.
pub fn ensure_staging_dir(
    estate_root: &Path,
    work_id: &crate::WorkId,
    run_id: &crate::RunId,
) -> std::io::Result<PathBuf> {
    let Some(dir) = staging_dir(estate_root, work_id, run_id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the Work or Run id cannot name a directory",
        ));
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Where the actor writes `name` for this Run, before any Claim.
pub fn staged_path(
    estate_root: &Path,
    work_id: &crate::WorkId,
    run_id: &crate::RunId,
    name: &str,
) -> Option<PathBuf> {
    check_output_name(name).ok()?;
    Some(staging_dir(estate_root, work_id, run_id)?.join(name))
}

/// The stored form recorded on the receipt: `claims/<claim>/<name>`,
/// relative to `outputs_dir`. Deliberately relative, so the record does
/// not carry this host's estate path and a moved estate still resolves.
pub fn stored_relative(claim_id: &crate::ClaimId, name: &str) -> Option<String> {
    check_output_name(name).ok()?;
    well_formed_id(&claim_id.0).then(|| format!("claims/{}/{}", claim_id.0, name))
}

/// Resolves a recorded managed `path` back to the file it addresses,
/// canonically contained in this Work's own outputs area.
///
/// `None` — never a path outside the area, and never a guess — when the
/// ids are unusable, when the recorded relative path is not the shape
/// this module writes, or when what is there resolves (through a
/// symlink, a bind, a replaced directory) anywhere but inside the area.
/// Every consumer treats `None` as *explicitly unavailable*.
///
/// **Absent is not unresolved.** A recorded path whose file is simply
/// gone resolves to the address it names, so its caller reads it,
/// fails, and reports *absent* — which is the true fact, and the same
/// one a deleted worktree artifact reports. Reserving `None` for a
/// containment failure keeps "the bytes are gone" distinguishable from
/// "this address no longer leads inside the area that owns it".
pub fn resolve_stored(
    estate_root: &Path,
    work_id: &crate::WorkId,
    stored: &str,
) -> Option<PathBuf> {
    let root = outputs_dir(estate_root, work_id)?;
    // The recorded path is this module's own product, so it is checked
    // against this module's own shape rather than parsed permissively:
    // exactly `claims/<id>/<name>`, three components, each already
    // subject to its own rule.
    let mut parts = stored.split('/');
    let (Some("claims"), Some(claim), Some(name), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if !well_formed_id(claim) || check_output_name(name).is_err() {
        return None;
    }
    let candidate = root.join("claims").join(claim).join(name);
    match fs::symlink_metadata(&candidate) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(candidate),
        Err(_) => None,
        Ok(_) => contained_regular_file(&root, &candidate),
    }
}

/// `candidate`, if and only if it is a regular file whose canonical path
/// is inside `root`'s canonical path.
///
/// Two independent checks, because they answer different questions and
/// each has a case the other misses:
///
/// * `symlink_metadata` proves the entry *itself* is a regular file, so
///   a symlink named where an output should be is refused without ever
///   being followed — including a dangling one, and including one whose
///   target does happen to sit inside the area.
/// * canonical `starts_with` proves the resolved location is inside the
///   area, which is the check that catches an *ancestor* directory
///   having been replaced by a symlink after the file was written.
///
/// The second is `artifact_canonical_containment`'s own discipline
/// (canonicalize both, compare prefixes) applied to this area's root
/// instead of a worktree's (R2). A failed inspection returns `None` the
/// same as a proven escape: this function's callers report
/// unavailability, never a boundary verdict, so the two need not be
/// distinguished here.
pub fn contained_regular_file(root: &Path, candidate: &Path) -> Option<PathBuf> {
    if !fs::symlink_metadata(candidate).ok()?.file_type().is_file() {
        return None;
    }
    let canonical_root = fs::canonicalize(root).ok()?;
    let canonical = fs::canonicalize(candidate).ok()?;
    canonical.starts_with(&canonical_root).then_some(canonical)
}

/// Why a managed output could not be snapshotted.
#[derive(Debug)]
pub enum OutputWriteError {
    /// The name, Work, Run or Claim cannot address a managed output.
    Unaddressable(String),
    /// The claims directory for this Claim already holds this name. A
    /// Claim id is minted once per Claim, so this is a minting bug, not
    /// a rewrite to perform — the same reasoning
    /// `ProjectionFile::write_new` states for an observation id.
    AlreadyWritten(String),
    Io(std::io::Error),
    /// The rename made the snapshot visible; only the directory `fsync`
    /// after it failed. The bytes are there and re-hash. Reporting this
    /// as "never wrote" would be false, so it is separate from `Io` and
    /// the caller logs it and proceeds — exactly as
    /// `PreparedProjection::commit` already does.
    DurabilityUncertain(String),
}

impl std::fmt::Display for OutputWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputWriteError::Unaddressable(detail) => write!(f, "{detail}"),
            OutputWriteError::AlreadyWritten(path) => {
                write!(f, "a managed output already exists at {path}")
            }
            OutputWriteError::Io(err) => write!(f, "{err}"),
            OutputWriteError::DurabilityUncertain(detail) => write!(f, "{detail}"),
        }
    }
}

impl From<std::io::Error> for OutputWriteError {
    fn from(err: std::io::Error) -> Self {
        OutputWriteError::Io(err)
    }
}

/// Snapshots `bytes` as this Claim's immutable copy of `name`, durably,
/// before anything references it. Returns the file's real path.
///
/// The caller has already read and digested `bytes` from the staging
/// area: exactly those bytes are written here and exactly that digest is
/// recorded, so the receipt is bound to the content that was inspected
/// and not to whatever the staged path holds a moment later. That is the
/// "snapshot before journal reference, digest exact bytes" ruling 0145
/// requires, and it is why a post-Claim rewrite of the staged file
/// changes nothing about the evidence.
pub fn store_claimed_bytes(
    estate_root: &Path,
    work_id: &crate::WorkId,
    claim_id: &crate::ClaimId,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf, OutputWriteError> {
    if let Err(err) = check_output_name(name) {
        return Err(OutputWriteError::Unaddressable(err.detail().to_string()));
    }
    let Some(root) = outputs_dir(estate_root, work_id) else {
        return Err(OutputWriteError::Unaddressable(
            "the Work id cannot name a directory".to_string(),
        ));
    };
    if !well_formed_id(&claim_id.0) {
        return Err(OutputWriteError::Unaddressable(
            "the Claim id cannot name a directory".to_string(),
        ));
    }
    let dir = root.join("claims").join(&claim_id.0);
    fs::create_dir_all(&dir)?;
    let final_path = dir.join(name);
    // `fs::rename` replaces its destination, so `create_new` on the temp
    // file alone does not make the *final* path write-once.
    if final_path.try_exists().unwrap_or(true) {
        return Err(OutputWriteError::AlreadyWritten(
            final_path.display().to_string(),
        ));
    }
    let temp = dir.join(format!(".tmp-{name}"));
    // A retried snapshot after a crash could find its own temp behind;
    // `create_new` would then fail forever on a name nothing references.
    let _ = fs::remove_file(&temp);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, &final_path)?;
    File::open(&dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| {
            OutputWriteError::DurabilityUncertain(format!(
                "the managed output {} was renamed into place but its directory fsync failed: \
                 {err}",
                final_path.display()
            ))
        })?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaimId, RunId, WorkId};

    #[test]
    fn only_one_ordinary_filename_component_addresses_a_managed_output() {
        for good in ["report.md", "a", "A-B_c.1.md", "OUTPUTS-BUILT.md"] {
            assert!(check_output_name(good).is_ok(), "{good} should be usable");
        }
        for (bad, expected) in [
            ("", OutputNameError::Empty),
            (".", OutputNameError::Hidden),
            ("..", OutputNameError::Hidden),
            (".hidden", OutputNameError::Hidden),
            ("../escape", OutputNameError::Hidden),
            ("a/b", OutputNameError::UnsupportedCharacter),
            ("a\\b", OutputNameError::UnsupportedCharacter),
            ("/abs", OutputNameError::UnsupportedCharacter),
            ("with space", OutputNameError::UnsupportedCharacter),
            ("nul\0byte", OutputNameError::UnsupportedCharacter),
            ("réport.md", OutputNameError::UnsupportedCharacter),
        ] {
            assert_eq!(
                check_output_name(bad),
                Err(expected),
                "{bad:?} must be refused with its own reason"
            );
        }
        assert_eq!(
            check_output_name(&"a".repeat(MAX_OUTPUT_NAME + 1)),
            Err(OutputNameError::TooLong)
        );
    }

    /// A recorded managed path is this module's own product, and is
    /// checked against this module's own shape rather than parsed
    /// permissively: anything else resolves to nothing at all.
    #[test]
    fn only_this_modules_own_recorded_shape_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let estate = dir.path();
        let work = WorkId("work-1".to_string());
        for stored in [
            "report.md",
            "claims/report.md",
            "staging/run-1/report.md",
            "claims/c1/nested/report.md",
            "claims/../../../etc/passwd",
            "../../etc/passwd",
            "claims/c1/.hidden",
            "",
        ] {
            assert!(
                resolve_stored(estate, &work, stored).is_none(),
                "{stored:?} must not resolve"
            );
        }
        // The shape this module writes resolves — to an address, even
        // before anything is there, so its caller reports *absent*
        // rather than an addressing failure.
        let resolved = resolve_stored(estate, &work, "claims/claim-1/report.md")
            .expect("the module's own shape resolves");
        assert!(
            resolved.ends_with("outputs/claims/claim-1/report.md"),
            "{resolved:?}"
        );
    }

    #[test]
    fn a_snapshot_is_written_once_and_holds_the_bytes_it_was_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let estate = dir.path();
        let work = WorkId("work-1".to_string());
        let claim = ClaimId("claim-1".to_string());

        let path = store_claimed_bytes(estate, &work, &claim, "report.md", b"first\n")
            .expect("the first snapshot is written");
        assert_eq!(std::fs::read(&path).unwrap(), b"first\n");
        // Write-once at the final name: a second snapshot under one
        // Claim is a minting bug, not a rewrite to perform.
        assert!(matches!(
            store_claimed_bytes(estate, &work, &claim, "report.md", b"second\n"),
            Err(OutputWriteError::AlreadyWritten(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"first\n");
        // No temporary is left behind for a reader to trip over.
        let entries: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["report.md".to_string()], "{entries:?}");

        assert!(matches!(
            store_claimed_bytes(estate, &work, &claim, "../escape", b"x"),
            Err(OutputWriteError::Unaddressable(_))
        ));
    }

    #[test]
    fn staging_is_derived_per_work_and_per_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let estate = dir.path();
        let a = staging_dir(estate, &WorkId("work-a".into()), &RunId("run-1".into())).unwrap();
        let b = staging_dir(estate, &WorkId("work-b".into()), &RunId("run-1".into())).unwrap();
        let c = staging_dir(estate, &WorkId("work-a".into()), &RunId("run-2".into())).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, c);
        // An id that could leave the area never becomes a directory.
        assert!(staging_dir(estate, &WorkId("../..".into()), &RunId("run-1".into())).is_none());
        assert!(staging_dir(estate, &WorkId("work-a".into()), &RunId("a/b".into())).is_none());
        assert!(
            staged_path(
                estate,
                &WorkId("work-a".into()),
                &RunId("run-1".into()),
                ".."
            )
            .is_none()
        );
    }
}
