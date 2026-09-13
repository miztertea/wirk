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

/// What one *observation* of a staged declared output found.
///
/// Distinct from `contained_regular_file`'s `Option`, which folds
/// "absent" and "could not be established" into the same `None`. A
/// progress observation has to tell those apart: an output that is
/// definitively not there yet is a *known* state that can be compared
/// across turn ends, while an output that could not be inspected, or
/// that no longer addresses a file inside this Run's own area, is
/// *unknown* — it is neither evidence of a change nor evidence that
/// nothing changed.
pub enum StagedObservation {
    /// A regular file at exactly this Run's own canonical staged
    /// address, held open. The descriptor is the same file object whose
    /// identity was checked, so the bytes read from it are the bytes
    /// that were checked.
    Open(File),
    /// No entry by that name, or no staging area yet, *at this Run's
    /// own address*. A known state — and a state a missing entry alone
    /// does not establish (`staging_address`).
    Absent,
    /// The name does not address a regular file inside this Run's own
    /// staging area: a symlink at the name, or an ancestor — the
    /// staging directory itself included — redirected away from the
    /// address the estate's own trusted root derives.
    OutOfBoundary,
    /// The area could not be inspected at all.
    Unreadable,
}

/// Whether this Run's own staging directory is *established* at the
/// address the trusted estate root derives — the question a missing
/// entry cannot answer for itself.
///
/// `symlink_metadata` answering `NotFound` says only that nothing is
/// there under whatever directory the lookup happened to traverse into.
/// That is known absence when the traversal stayed on this Run's own
/// address, and unknown when it did not: a staging root or an ancestor
/// replaced by a symlink — to an empty directory, or to nothing at all
/// — produces the identical `NotFound`. So the address itself is walked
/// one derived component at a time from the canonical estate root, each
/// required to be a real directory and not a link, which is the same
/// anchor and the same "no redirection below it" rule
/// `observe_staged_output` applies to a file that *is* there.
///
/// A component that genuinely does not exist is `Missing`, not a
/// failure: a Run that has never written an output has no staging
/// directory, and that is a true, known "nothing staged".
enum StagingAddress {
    /// Every derived component exists and is a real directory.
    Established,
    /// A derived component does not exist at all.
    Missing,
    /// A derived component is not a real directory: a symlink
    /// (dangling or not), or some other entry in a directory's place.
    Redirected,
    /// A derived component could not be inspected.
    Unreadable,
}

fn staging_address(trusted_root: &Path, staging: &Path) -> StagingAddress {
    let Ok(relative) = staging.strip_prefix(trusted_root) else {
        return StagingAddress::Redirected;
    };
    let mut walked = trusted_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return StagingAddress::Redirected;
        };
        walked.push(part);
        match fs::symlink_metadata(&walked) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => return StagingAddress::Redirected,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return StagingAddress::Missing;
            }
            Err(_) => return StagingAddress::Unreadable,
        }
    }
    StagingAddress::Established
}

/// Opens this Run's staged `name` for observation, or says why it
/// could not be.
///
/// **Why not `contained_regular_file(staging_dir, ..)`.** That call
/// canonicalizes *both* the root it is given and the candidate, and its
/// callers here pass the staging directory itself as the root. A
/// staging directory — or any ancestor of it — replaced by a symlink
/// therefore moves both sides of the comparison together, and the
/// prefix test passes against a directory outside the Work's own area.
/// The root of a containment check has to be an anchor the thing being
/// checked cannot move. Here that anchor is the **estate root**: the
/// operator's own directory, established before any Run and not inside
/// any actor's area. It is canonicalized once — so an estate legitimately
/// reached through a symlinked path still resolves — and every component
/// below it is then required to be exactly what `staged_path` derives,
/// with no redirection of its own.
///
/// **What the identity check closes.** `symlink_metadata` proves the
/// entry itself is a regular file, but the open that follows is a
/// second, fresh path lookup. Comparing the opened descriptor's
/// `(dev, ino)` against the entry that was inspected proves the file
/// read is the file checked; a swap in between answers `OutOfBoundary`
/// rather than digesting whatever replaced it.
///
/// **This is an observation, not Claim evidence.** It is deliberately
/// weaker than `wirkd::server::read_staged_output`, whose `openat`/
/// `O_NOFOLLOW` walk holds every ancestor open and repeats no lookup at
/// all. That walk is what validates the bytes a Claim records. This one
/// answers a repeated "did anything change?" and reports *unknown*
/// whenever it cannot establish the answer, which is the safe direction
/// for a question whose false answer would be "the actor is making
/// progress".
pub fn observe_staged_output(
    estate_root: &Path,
    work_id: &crate::WorkId,
    run_id: &crate::RunId,
    name: &str,
) -> StagedObservation {
    use std::os::unix::fs::MetadataExt;

    let Ok(trusted_root) = fs::canonicalize(estate_root) else {
        return StagedObservation::Unreadable;
    };
    // The address the actor actually writes to, and the canonical
    // address it must resolve to. Both are derived by the same function
    // from the same ids; only the root differs.
    let (Some(candidate), Some(expected)) = (
        staged_path(estate_root, work_id, run_id, name),
        staged_path(&trusted_root, work_id, run_id, name),
    ) else {
        return StagedObservation::OutOfBoundary;
    };
    let inspected = match fs::symlink_metadata(&candidate) {
        Ok(meta) if meta.file_type().is_file() => (meta.dev(), meta.ino()),
        Ok(_) => return StagedObservation::OutOfBoundary,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Nothing is there — but *there* has to be this Run's own
            // address before "nothing" is a known state rather than a
            // reading of some other directory (`staging_address`).
            let Some(staging) = expected.parent() else {
                return StagedObservation::OutOfBoundary;
            };
            return match staging_address(&trusted_root, staging) {
                StagingAddress::Established | StagingAddress::Missing => StagedObservation::Absent,
                StagingAddress::Redirected => StagedObservation::OutOfBoundary,
                StagingAddress::Unreadable => StagedObservation::Unreadable,
            };
        }
        Err(_) => return StagedObservation::Unreadable,
    };
    let Ok(canonical) = fs::canonicalize(&candidate) else {
        return StagedObservation::Unreadable;
    };
    if canonical != expected {
        return StagedObservation::OutOfBoundary;
    }
    let Ok(file) = File::open(&canonical) else {
        return StagedObservation::Unreadable;
    };
    let Ok(opened) = file.metadata() else {
        return StagedObservation::Unreadable;
    };
    if !opened.file_type().is_file() || (opened.dev(), opened.ino()) != inspected {
        return StagedObservation::OutOfBoundary;
    }
    StagedObservation::Open(file)
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

    /// The case `contained_regular_file(staging_dir, ..)` cannot see:
    /// the staging directory itself (or an ancestor of it) redirected
    /// out of the Work's own area. Canonicalizing that directory as the
    /// containment root moves the root along with the redirection, so
    /// the prefix test passes. Anchored at the estate root instead, the
    /// same swap is out of boundary.
    #[test]
    fn a_redirected_staging_root_or_ancestor_is_out_of_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let estate = dir.path().join("estate");
        let work = WorkId("work-1".into());
        let run = RunId("run-1".into());

        // Elsewhere entirely: a directory no id of this Work addresses.
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("elsewhere");
        fs::write(elsewhere.join("report.md"), b"foreign bytes\n").expect("foreign file");

        // The old check's own root, proving it passes on this input.
        let staging = staging_dir(&estate, &work, &run).expect("staging path");
        fs::create_dir_all(staging.parent().unwrap()).expect("staging parent");
        std::os::unix::fs::symlink(&elsewhere, &staging).expect("redirect the staging root");
        let candidate = staged_path(&estate, &work, &run, "report.md").expect("staged path");
        assert!(
            contained_regular_file(&staging, &candidate).is_some(),
            "the prefix check anchored at the moved root passes — this is the defect"
        );
        assert!(
            matches!(
                observe_staged_output(&estate, &work, &run, "report.md"),
                StagedObservation::OutOfBoundary
            ),
            "anchored at the estate root, the redirected staging root is out of boundary"
        );

        // The same, one level up: `outputs/staging` redirected.
        fs::remove_file(&staging).expect("undo the root swap");
        let staging_parent = staging.parent().unwrap().to_path_buf();
        fs::remove_dir_all(&staging_parent).expect("clear staging parent");
        std::os::unix::fs::symlink(&elsewhere, &staging_parent).expect("redirect an ancestor");
        fs::create_dir_all(elsewhere.join(&run.0)).expect("target run dir");
        fs::write(elsewhere.join(&run.0).join("report.md"), b"foreign\n").expect("foreign");
        assert!(
            matches!(
                observe_staged_output(&estate, &work, &run, "report.md"),
                StagedObservation::OutOfBoundary
            ),
            "a redirected ancestor is out of boundary too"
        );
    }

    /// Legitimate use is unaffected, including an estate whose own path
    /// is reached through a symlink: the estate root is canonicalized
    /// once as the trusted anchor, so only redirection *below* it is a
    /// boundary failure.
    #[test]
    fn ordinary_and_symlinked_estate_paths_observe_the_real_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let estate = dir.path().join("estate");
        let work = WorkId("work-1".into());
        let run = RunId("run-1".into());
        let staging = ensure_staging_dir(&estate, &work, &run).expect("staging");
        fs::write(staging.join("report.md"), b"real bytes\n").expect("write");

        for root in [estate.clone(), {
            let link = dir.path().join("estate-link");
            std::os::unix::fs::symlink(&estate, &link).expect("symlink the estate path");
            link
        }] {
            let StagedObservation::Open(mut file) =
                observe_staged_output(&root, &work, &run, "report.md")
            else {
                panic!("a real declared output under {root:?} must be observable");
            };
            let mut read = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut read).expect("read");
            assert_eq!(read, b"real bytes\n");
        }

        // Absent is a known state, and distinct from a boundary failure.
        assert!(matches!(
            observe_staged_output(&estate, &work, &run, "missing.md"),
            StagedObservation::Absent
        ));
        // A symlink at the declared name itself is never followed.
        let target = dir.path().join("target.md");
        fs::write(&target, b"never read\n").expect("target");
        std::os::unix::fs::symlink(&target, staging.join("linked.md")).expect("symlink");
        assert!(matches!(
            observe_staged_output(&estate, &work, &run, "linked.md"),
            StagedObservation::OutOfBoundary
        ));
        // A directory where an output should be is not an output.
        fs::create_dir(staging.join("adir")).expect("dir");
        assert!(matches!(
            observe_staged_output(&estate, &work, &run, "adir"),
            StagedObservation::OutOfBoundary
        ));
        // An unaddressable name never becomes a path.
        assert!(matches!(
            observe_staged_output(&estate, &work, &run, ".."),
            StagedObservation::OutOfBoundary
        ));
    }

    /// Absence is only a *known* state when it was established at this
    /// Run's own address. `symlink_metadata` answering `NotFound` says
    /// nothing about which directory the name was looked up under: a
    /// staging root — or an ancestor of it — redirected to an empty or
    /// dangling destination produces exactly that `NotFound`, and
    /// reporting it as `Absent` states "definitively nothing there yet"
    /// about a directory outside the Work's own area. The present-file
    /// case is caught by the canonical comparison below it; this is the
    /// missing-file case, which never reaches that comparison.
    #[test]
    fn a_missing_entry_under_a_redirected_address_is_out_of_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = WorkId("work-1".into());
        let run = RunId("run-1".into());

        // Empty and dangling destinations, each in turn, for the
        // staging root itself and for its `outputs/staging` ancestor.
        // A fresh estate per case: a redirection is not undone by the
        // next one, and each has to be observed on its own.
        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).expect("empty elsewhere");
        let dangling = dir.path().join("nothing-here");
        for (case, destination) in [
            ("root-empty", empty.clone()),
            ("root-dangling", dangling.clone()),
            ("ancestor-empty", empty.clone()),
            ("ancestor-dangling", dangling.clone()),
        ] {
            let estate = dir.path().join(format!("estate-{case}"));
            let staging = staging_dir(&estate, &work, &run).expect("staging path");
            let redirected = if case.starts_with("root") {
                staging.clone()
            } else {
                staging.parent().expect("staging parent").to_path_buf()
            };
            fs::create_dir_all(redirected.parent().expect("parent")).expect("area");
            std::os::unix::fs::symlink(&destination, &redirected).expect("redirect");
            assert!(
                matches!(
                    observe_staged_output(&estate, &work, &run, "report.md"),
                    StagedObservation::OutOfBoundary
                ),
                "{case}: a name missing from a redirected address is unknown, not known absence"
            );
        }

        // Positive controls, on an estate nothing redirected: genuine
        // absence stays known, before any staging directory exists and
        // after a real output is legitimately deleted from a real one.
        let estate = dir.path().join("estate-ordinary");
        fs::create_dir_all(&estate).expect("estate root");
        assert!(
            matches!(
                observe_staged_output(&estate, &work, &run, "report.md"),
                StagedObservation::Absent
            ),
            "a Run that has never written an output is known to have none"
        );
        let staging = ensure_staging_dir(&estate, &work, &run).expect("staging");
        assert!(matches!(
            observe_staged_output(&estate, &work, &run, "report.md"),
            StagedObservation::Absent
        ));
        fs::write(staging.join("report.md"), b"drafted\n").expect("write");
        assert!(matches!(
            observe_staged_output(&estate, &work, &run, "report.md"),
            StagedObservation::Open(_)
        ));
        fs::remove_file(staging.join("report.md")).expect("delete");
        assert!(
            matches!(
                observe_staged_output(&estate, &work, &run, "report.md"),
                StagedObservation::Absent
            ),
            "a legitimate deletion inside a real staging directory is still known absence"
        );

        // And the same, through an estate legitimately reached by a
        // symlinked path: the trusted anchor is the estate root
        // *canonicalized*, so only redirection below it is a boundary
        // failure and ordinary absence there stays known.
        let link = dir.path().join("estate-ordinary-link");
        std::os::unix::fs::symlink(&estate, &link).expect("symlink the estate path");
        assert!(matches!(
            observe_staged_output(&link, &work, &run, "report.md"),
            StagedObservation::Absent
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
