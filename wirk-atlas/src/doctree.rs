//! A second acquisition policy beside `git`'s, for a local document
//! collection that is not itself a Git repository and must never be
//! treated as one — chosen explicitly at admission
//! (`AtlasStore::register_document_tree`), never inferred from the
//! absence of a `.git` directory. A directory that happens to sit under
//! an ambient Git repository, including one the enclosing workspace
//! ignores, is walked exactly as given: this module never shells out to
//! `git` and never discovers a parent repository.
//!
//! An explicit `git` acquisition of such a directory enumerates it
//! relative to the *enclosing* repository's own commit and tree,
//! reporting however much of that directory Git tracks there. That is
//! real Git-relative behaviour, not a defect. What it cannot do is
//! admit the directory *as itself*, under an identity that depends on
//! nothing outside the walked bytes. That is the gap this module
//! closes.
//!
//! **What a coordinate into a document collection can and cannot
//! promise.** A Git generation's bytes are read live from the object
//! store, and stay readable while that store still holds them. A plain
//! filesystem has no object store: the bytes behind a coordinate are
//! read live from the current file. Once the one file a coordinate
//! names has changed, `AtlasStore::resolve_exact` reports it
//! `Unavailable` rather than returning different bytes under the same
//! coordinate. An edit to a *different* file in the same collection
//! does not have that effect — each resource is verified against its
//! own recorded content hash (`blob`), so what is still on disk
//! unchanged stays resolvable regardless of what else in the tree
//! moved.

use crate::{
    AtlasError, ContentFamily, CoverageDisposition, ExtractorPolicy, GenerationId, ResourceRecord,
};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;

/// The acquisition policy label a document-tree generation records in
/// `SourceGeneration::acquisition_policy`, parallel to
/// `git::ACQUISITION_POLICY`. `pub`, not `pub(crate)`: `lib.rs`
/// re-exports it as `DOCUMENT_TREE_POLICY`, and a re-export can never
/// be more visible than the item it names.
pub const ACQUISITION_POLICY: &str = "document-tree-policy/v1";

/// The only `requested_ref`/`--revision` spelling this policy honours.
///
/// A document tree has exactly one observable state — its current one.
/// Any other string would be recorded on `Membership`/
/// `SourceGeneration` and read back later, including by `atlas status`'s
/// `recent_attempts`, as if it named something this policy had checked.
/// `register_document_tree`/`acquire_document_tree` refuse any other
/// value by name.
pub const CURRENT_OBSERVATION: &str = "current";

/// What one capture of a document collection is allowed to read, walk
/// and hold, resolved from the estate's own resource policy rather than
/// fixed in this module.
///
/// Every bound here is a refusal, never a truncation: a collection over
/// one of them is reported as refused, and a file over the per-file
/// bound is reported `Unsupported` by name. An operator who needs a
/// different shape of collection raises the corresponding field in
/// `<estate>/.wirk/resources.json`; the defaults live on
/// `wirk_core::jobs::ResourcePolicy`, which documents the workload each
/// one is sized against.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureLimits {
    pub(crate) max_file_bytes: u64,
    pub(crate) max_total_bytes: u64,
    pub(crate) max_depth: usize,
    pub(crate) max_entries: usize,
}

impl CaptureLimits {
    pub(crate) fn from_policy(policy: &wirk_core::jobs::ResourcePolicy) -> Self {
        Self {
            max_file_bytes: policy.document_max_file_bytes,
            max_total_bytes: policy.document_max_total_bytes,
            max_depth: policy.document_max_entries_depth,
            max_entries: policy.document_max_entries,
        }
    }
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self::from_policy(&wirk_core::jobs::ResourcePolicy::default())
    }
}

/// Opens `name` directly inside the directory `parent` already holds
/// open, following no symlink at that final component and consulting no
/// path outside `parent`'s own descriptor.
///
/// The same descriptor-relative `openat`/`O_NOFOLLOW` idiom
/// `wirk::wirkd::server::open_no_follow` uses for staged-output reads,
/// reimplemented here rather than called across the crate boundary:
/// `wirk` depends on `wirk-atlas`, not the reverse. `libc` is already a
/// pinned `wirk-atlas` dependency; no new crate.
///
/// **`O_NONBLOCK` on the non-directory open is load-bearing.**
/// `O_NOFOLLOW` refuses a symlink at the final component but does not
/// refuse a FIFO, and opening a FIFO for reading blocks until a writer
/// appears. Without the flag, a regular file replaced by a FIFO between
/// the `lstat_at` that classified it and this open would park this
/// thread indefinitely — holding whatever lock the caller took — rather
/// than return. With it the open returns immediately whatever the entry
/// turned out to be, and the `fstat` on the returned descriptor is what
/// decides whether it is still an ordinary file. The flag is a no-op
/// for the regular files this walk actually wants.
fn open_no_follow(
    parent: BorrowedFd<'_>,
    name: &[u8],
    directory: bool,
) -> std::io::Result<OwnedFd> {
    let name = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if directory {
        flags |= libc::O_DIRECTORY;
    } else {
        flags |= libc::O_NONBLOCK;
    }
    // SAFETY: `name` is NUL-terminated and outlives the call, `parent`
    // is a live borrowed descriptor, and the result is either -1 or a
    // fresh descriptor owned by this process alone.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor just returned by `openat` and
    // is not owned anywhere else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `lstat`, but relative to an already-open directory descriptor and
/// exactly one path component — so the check that decides whether to
/// follow or refuse an entry is not a second, separate lookup from the
/// one that later opens it.
fn lstat_at(parent: BorrowedFd<'_>, name: &[u8]) -> std::io::Result<libc::stat> {
    let name = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `stat` is a valid, zeroed `libc::stat` the kernel fills in;
    // `parent`/`name` name a single component resolved with no symlink
    // followed at any point (`AT_SYMLINK_NOFOLLOW`).
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(stat)
}

/// The window between classifying an entry and opening it.
///
/// Both operations are real and separated by a real instant, and what
/// can change in that instant — a regular file becoming a FIFO, a
/// directory becoming a symlink — is what the flags and post-open
/// checks around it exist to survive. A test drives that instant
/// directly by arming this window rather than by racing a sleep.
pub const OPEN_WINDOW: &str = "doctree-entry-classified";

/// Reads every name a directory descriptor holds, counting each one
/// against the walk's entry budget *while streaming* the directory
/// rather than after listing it.
///
/// Two things are deliberate here. Every name is counted — files,
/// directories, symlinks, special files and names that vanish before
/// they can be inspected alike — because the cost being bounded is the
/// walk's, and a tree made entirely of directories costs exactly as
/// much to walk as one made entirely of files. And the budget is tested
/// inside the iteration, so the returned `Vec` can never hold more than
/// the remaining budget: one enormously wide directory is refused
/// partway through instead of being listed, allocated and sorted in
/// full before anything checks it.
///
/// The listing is read **from the descriptor this walk already holds
/// open**, never by naming a path again. `rustix::fs::Dir::read_from`
/// is the supported interface for that: it reopens `"."` relative to
/// the given descriptor, so the stream it iterates is the directory
/// that descriptor already refers to, and a rename or symlink swap of
/// an ancestor after the descriptor was opened cannot retarget it. The
/// reopen also gives the stream its own file offset, so iterating here
/// does not disturb the caller's descriptor.
///
/// This replaced a `/proc/self/fd/<n>` round-trip, which obtained the
/// same no-re-resolution property by depending on a Linux-only
/// filesystem for the one operation that enumerates a collection at
/// all. The rest of this walk is POSIX `openat`/`fstatat`, so that read
/// was the single facility whose absence would have failed every
/// capture with a confusing path rather than a named refusal. Nothing
/// about the containment or the bounds is relaxed by the change: the
/// budget is still tested inside the iteration, and classification is
/// still `lstat_at`'s, not the `d_type` this stream also reports —
/// `d_type` is `DT_UNKNOWN` on filesystems that do not carry it, and
/// the metadata that decides whether to follow or refuse an entry must
/// be the same lookup the later open is relative to.
///
/// `"."` and `".."` are skipped before they are counted. This stream
/// reports them where `std::fs::read_dir` filtered them, and charging
/// them to the budget would quietly make every configured entry bound
/// mean two fewer real entries per directory.
fn read_dir_names(
    dir: BorrowedFd<'_>,
    examined: &mut usize,
    limits: &CaptureLimits,
    stop: &wirk_core::jobs::JobStop,
) -> Result<Vec<Vec<u8>>, AtlasError> {
    let stream =
        rustix::fs::Dir::read_from(dir).map_err(|err| AtlasError::Io(std::io::Error::from(err)))?;
    let mut names = Vec::new();
    for entry in stream {
        let entry = entry.map_err(|err| AtlasError::Io(std::io::Error::from(err)))?;
        let name = entry.file_name().to_bytes();
        if name == b".".as_slice() || name == b"..".as_slice() {
            continue;
        }
        stop.check().map_err(stopped)?;
        *examined = examined.saturating_add(1);
        if *examined > limits.max_entries {
            return Err(AtlasError::InvalidRequest(format!(
                "document-tree source exceeds the {}-entry bounded traversal budget; raise \
                 document_max_entries in this estate's .wirk/resources.json to walk a larger \
                 collection",
                limits.max_entries
            )));
        }
        names.push(name.to_vec());
    }
    // Sorted so one collection walks in one order whatever order the
    // filesystem happened to report, which is what makes a refusal
    // reproducible. The list is already bounded by the budget above, so
    // this sorts at most that many names.
    names.sort();
    Ok(names)
}

/// Reads at most `limits.max_file_bytes + 1` bytes from `name` inside
/// `dir`, opened by descriptor exactly as `open_no_follow` documents,
/// then confirms the descriptor that was actually read names an
/// ordinary file of exactly `expected_len` bytes.
///
/// **The bound is on the read, not only on the earlier metadata.** The
/// `max_file_bytes` comparison made at listing time is against a length
/// that can be stale by the time this runs; `Read::take` bounds this
/// read itself, so a file that grew in that window is caught at
/// `max_file_bytes + 1` bytes rather than after an unbounded read had
/// already allocated to its new size.
fn read_bounded_no_follow(
    dir: BorrowedFd<'_>,
    name: &[u8],
    expected_len: u64,
    limits: &CaptureLimits,
) -> std::io::Result<Vec<u8>> {
    let opened = open_no_follow(dir, name, false)?;
    let mut file = File::from(opened);
    // `fstat` on the descriptor just opened, never a path: this is the
    // same file object the read below reads, so there is no window
    // between "confirmed a regular file" and "read from it" for a
    // replacement to exploit. It is also what catches an entry that
    // became a FIFO or a device after it was classified — the open
    // itself no longer blocks on one.
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no longer an ordinary file",
        ));
    }
    let mut bytes = Vec::with_capacity(expected_len.min(limits.max_file_bytes) as usize);
    file.by_ref()
        .take(limits.max_file_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "grew past the {}-byte bounded document read size while being read",
                limits.max_file_bytes
            ),
        ));
    }
    if bytes.len() as u64 != expected_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "changed size while being read",
        ));
    }
    Ok(bytes)
}

/// The first [`crate::document::SNIFF_BYTES`] of `name`, read through
/// the same descriptor-relative no-follow chain
/// [`read_bounded_no_follow`] uses. Never reads more than that: this is
/// the screen that decides whether a file whose name settles nothing is
/// worth reading in full, and it must not itself become the unbounded
/// read it exists to avoid.
fn sniff_no_follow(dir: BorrowedFd<'_>, name: &[u8]) -> std::io::Result<Vec<u8>> {
    let opened = open_no_follow(dir, name, false)?;
    let mut file = File::from(opened);
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no longer an ordinary file",
        ));
    }
    let mut prefix = Vec::with_capacity(crate::document::SNIFF_BYTES);
    file.by_ref()
        .take(crate::document::SNIFF_BYTES as u64)
        .read_to_end(&mut prefix)?;
    Ok(prefix)
}

/// One phrasing for every point this policy's work can be stopped at,
/// so an operator reads the same sentence whether the walk, the
/// extraction or a publish's revalidation was the part that stopped.
fn stopped(stop: wirk_core::jobs::JobStopped) -> AtlasError {
    AtlasError::Cancelled(format!("document-tree work {stop}"))
}

fn special_label(file_type: libc::mode_t) -> &'static str {
    match file_type & libc::S_IFMT {
        libc::S_IFSOCK => "socket",
        libc::S_IFIFO => "fifo",
        libc::S_IFCHR => "character device",
        libc::S_IFBLK => "block device",
        _ => "unsupported filesystem entry",
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn absorb(hasher: &mut Sha256, part: &[u8]) {
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}

fn display_path(root: &Path, relative: &[u8]) -> String {
    format!("{}/{}", root.display(), String::from_utf8_lossy(relative))
}

/// What one walked entry actually was, decided by a single `lstat`/
/// `openat`/bounded-read sequence — never re-derived by a second,
/// separate filesystem lookup later (`finish`, below, works entirely
/// from this, in memory).
pub(crate) enum CapturedKind {
    Symlink,
    /// A socket, fifo, or device: named and reported `Unsupported`,
    /// exactly like the symlink arm, rather than left absent from
    /// coverage.
    Special(&'static str),
    /// Excluded by fixed secret-like policy, decided from the path alone
    /// and never opened.
    Excluded,
    /// No extractor for this path's family, or the file exceeds the
    /// bounded read size — also decided before any read.
    Unsupported(&'static str),
    /// Named and sized at listing time, but its bytes could not be read:
    /// permission denied, vanished, changed size mid-read, or no longer
    /// an ordinary file. Does not abort the rest of the collection — a
    /// single unreadable document leaves every other document usable.
    Unavailable(String),
    /// Successfully read, in-bound bytes, ready for `finish` to run
    /// binary detection and extraction over without reading the file
    /// again.
    Content {
        digest: String,
        bytes: Vec<u8>,
    },
}

/// Written by hand rather than derived so a `Content` arm renders its
/// identity — digest and length — instead of the document's own bytes.
/// This type is what a capture holds for every admitted local file, and
/// the only thing that ever formats it is a panic message or a
/// diagnostic; neither is a place to spill the contents of someone's
/// document.
impl std::fmt::Debug for CapturedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Symlink => f.write_str("Symlink"),
            Self::Special(what) => write!(f, "Special({what})"),
            Self::Excluded => f.write_str("Excluded"),
            Self::Unsupported(why) => write!(f, "Unsupported({why})"),
            Self::Unavailable(why) => write!(f, "Unavailable({why})"),
            Self::Content { digest, bytes } => f
                .debug_struct("Content")
                .field("digest", digest)
                .field("bytes", &format_args!("<{} bytes>", bytes.len()))
                .finish(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Captured {
    relative: Vec<u8>,
    kind: CapturedKind,
    byte_len: Option<u64>,
}

/// One filesystem walk, one bounded read per in-bound file, entirely
/// through descriptor-relative `openat`/`O_NOFOLLOW` opens chained from
/// an already-open parent.
///
/// **No ancestor component is ever resolved twice.** Checking the final
/// component of a path and then reopening the whole path by name would
/// leave every intermediate directory unprotected, and a concurrent
/// replacement between the two lookups is a real window, not a
/// theoretical one. Here each directory is opened from its parent's own
/// descriptor and never named again. A directory swapped for a symlink
/// between this walk's `lstat` and its `openat` is refused (`ELOOP`)
/// rather than followed; a component that vanishes in that same window
/// is reported absent rather than treated as an inspection failure.
///
/// Returns the manifest identity — a bare 64-hex SHA-256 `revision` and
/// the same value `"sha256:"`-tagged as `content`, this policy's whole
/// replacement for `git commit`/`tree` — folded from every walked
/// entry's `(relative path, outcome)` in one pass, together with the
/// per-entry data `finish` needs to build final `ResourceRecord`s
/// without touching the filesystem again.
///
/// **What the identity does and does not claim.** It is a faithful
/// record of what this walk actually observed, entry by entry, in one
/// pass. It is not a snapshot: the filesystem is not frozen while the
/// walk runs, so a tree being edited concurrently can be observed
/// part-way through a change, and the identity then names exactly that
/// mixed observation rather than any instant that ever existed as a
/// whole. What each resource's own recorded content hash still proves
/// is per-file and exact, which is what resolution depends on.
///
/// Traversal and aggregate bytes are bounded by `limits`; either
/// overrun refuses the whole capture visibly, never reporting a
/// silently partial one as complete.
///
/// `stop` makes the walk reachable by an operator's cancellation: it is
/// checked once per examined name, so the walk stops within one entry
/// of being asked to. It is cooperative, not a time bound — a read
/// already blocked in the kernel is not interrupted by it, which is why
/// the open of a non-directory is non-blocking and every read is
/// bounded in bytes; but neither of those stops a syscall that is
/// already blocking on a stalled locator (a hung NFS or FUSE mount),
/// which is not reached by `stop` until it returns and the next
/// checkpoint is examined. A stopped capture stages nothing: this
/// returns before any generation directory is written.
pub(crate) fn capture(
    root: &Path,
    policy: &ExtractorPolicy,
    limits: &CaptureLimits,
    stop: &wirk_core::jobs::JobStop,
) -> Result<(String, String, Vec<Captured>), AtlasError> {
    let root_file = File::open(root)?;
    if !root_file.metadata()?.file_type().is_dir() {
        return Err(AtlasError::InvalidRequest(format!(
            "document-tree source {} is not a directory",
            root.display()
        )));
    }
    // The root itself is opened by path, following a symlink if `root`
    // names one — a registration-time choice, not a leak inside the
    // tree: every entry *under* the root, at every depth, is reached
    // only through the no-follow descriptor chain below.
    let root_fd: OwnedFd = root_file.into();
    let mut out = Vec::new();
    let mut total_bytes = 0u64;
    let mut examined = 0usize;
    recurse(
        root_fd.as_fd(),
        b"",
        0,
        &mut out,
        &mut total_bytes,
        &mut examined,
        policy,
        limits,
        stop,
    )?;
    out.sort_by(|a, b| a.relative.cmp(&b.relative));

    let mut hasher = Sha256::new();
    absorb(&mut hasher, b"wirk-atlas-document-tree-manifest/v2");
    for entry in &out {
        absorb(&mut hasher, &entry.relative);
        match &entry.kind {
            CapturedKind::Symlink => absorb(&mut hasher, b"symlink"),
            CapturedKind::Special(label) => {
                absorb(&mut hasher, b"special");
                absorb(&mut hasher, label.as_bytes());
            }
            CapturedKind::Excluded => {
                absorb(&mut hasher, b"excluded");
                absorb(&mut hasher, &entry.byte_len.unwrap_or(0).to_be_bytes());
            }
            CapturedKind::Unsupported(detail) => {
                absorb(&mut hasher, b"unsupported");
                absorb(&mut hasher, detail.as_bytes());
                absorb(&mut hasher, &entry.byte_len.unwrap_or(0).to_be_bytes());
            }
            CapturedKind::Unavailable(_) => {
                // The detail string is diagnostic only (an OS error
                // message, not stable content) and deliberately not
                // folded: two runs seeing the same missing/unreadable
                // file should fold the same identity even if the
                // kernel's own wording for "why" differs between them.
                absorb(&mut hasher, b"unavailable");
                absorb(&mut hasher, &entry.byte_len.unwrap_or(0).to_be_bytes());
            }
            CapturedKind::Content { digest, .. } => absorb(&mut hasher, digest.as_bytes()),
        }
    }
    let digest = hex(&hasher.finalize());
    Ok((digest.clone(), format!("sha256:{digest}"), out))
}

#[allow(clippy::too_many_arguments)]
fn recurse(
    dir: BorrowedFd<'_>,
    prefix: &[u8],
    depth: usize,
    out: &mut Vec<Captured>,
    total_bytes: &mut u64,
    examined: &mut usize,
    policy: &ExtractorPolicy,
    limits: &CaptureLimits,
    stop: &wirk_core::jobs::JobStop,
) -> Result<(), AtlasError> {
    if depth >= limits.max_depth {
        return Err(AtlasError::InvalidRequest(format!(
            "document-tree source exceeds the {}-directory bounded traversal depth; raise \
             document_max_entries_depth in this estate's .wirk/resources.json to walk a deeper \
             collection",
            limits.max_depth
        )));
    }
    let names = read_dir_names(dir, examined, limits, stop)?;
    for name in names {
        stop.check().map_err(stopped)?;
        if name.contains(&0) {
            // Not a real filesystem name on any platform this product
            // runs on; refused rather than silently mis-joined into a
            // path another check would trust.
            return Err(AtlasError::InvalidRequest(
                "document-tree entry name contains a NUL byte".into(),
            ));
        }
        let mut relative = prefix.to_vec();
        if !relative.is_empty() {
            relative.push(b'/');
        }
        relative.extend_from_slice(&name);

        let stat = match lstat_at(dir, &name) {
            Ok(stat) => stat,
            // Vanished between the listing above and this stat: not a
            // member of the tree any more, absent from coverage the same
            // way a name that was never listed would be. It was still
            // counted against the traversal budget, because walking it
            // cost the walk exactly as much as any other name did.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(AtlasError::Io(err)),
        };
        let file_type = stat.st_mode & libc::S_IFMT;

        if file_type == libc::S_IFLNK {
            out.push(Captured {
                relative,
                kind: CapturedKind::Symlink,
                byte_len: None,
            });
            continue;
        }
        // Between this classification and the open that acts on it, the
        // entry can become something else. Everything below survives
        // that by construction; this is where a test holds the instant
        // open and makes the replacement happen.
        crate::store::checkpoint(OPEN_WINDOW);
        if file_type == libc::S_IFDIR {
            match open_no_follow(dir, &name, true) {
                Ok(sub) => recurse(
                    sub.as_fd(),
                    &relative,
                    depth + 1,
                    out,
                    total_bytes,
                    examined,
                    policy,
                    limits,
                    stop,
                )?,
                // Swapped for a symlink between the lstat above and this
                // open: `O_NOFOLLOW` refuses it (`ELOOP`) rather than
                // descending into wherever it now points. Reported the
                // same way a symlink found directly by `lstat` is, never
                // followed either way.
                Err(err) if err.raw_os_error() == Some(libc::ELOOP) => out.push(Captured {
                    relative,
                    kind: CapturedKind::Symlink,
                    byte_len: None,
                }),
                // Swapped for something that is no longer a directory:
                // named as unavailable rather than silently dropped.
                Err(err) if err.raw_os_error() == Some(libc::ENOTDIR) => out.push(Captured {
                    relative,
                    kind: CapturedKind::Unavailable(err.to_string()),
                    byte_len: None,
                }),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(AtlasError::Io(err)),
            }
            continue;
        }
        if file_type != libc::S_IFREG {
            out.push(Captured {
                relative,
                kind: CapturedKind::Special(special_label(file_type)),
                byte_len: None,
            });
            continue;
        }

        let len = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
        // Exclusion is decided from the path alone, before anything about
        // this file is opened: excluded, secret-like content is never
        // read, and an unrecognized name is never treated as the same
        // kind of refusal.
        if ExtractorPolicy::excluded(&relative) {
            out.push(Captured {
                relative,
                kind: CapturedKind::Excluded,
                byte_len: Some(len),
            });
            continue;
        }
        match policy.admission(&relative) {
            crate::extract::PathAdmission::Family(_) => {}
            crate::extract::PathAdmission::No => {
                out.push(Captured {
                    relative,
                    kind: CapturedKind::Unsupported("no extractor for path family"),
                    byte_len: Some(len),
                });
                continue;
            }
            // The name settles nothing. The per-file size bound is
            // checked first, so a file too large to admit is never even
            // sniffed; then one bounded prefix decides whether reading
            // the rest could possibly be worth it. Both refusals below
            // cost one open and at most `SNIFF_BYTES`, and neither
            // charges the aggregate budget, which only the full read
            // does.
            crate::extract::PathAdmission::Candidate => {
                if len > limits.max_file_bytes {
                    out.push(Captured {
                        relative,
                        kind: CapturedKind::Unsupported(
                            "file exceeds the bounded document read size",
                        ),
                        byte_len: Some(len),
                    });
                    continue;
                }
                match sniff_no_follow(dir, &name) {
                    Ok(prefix) if crate::document::could_be_document(&prefix) => {}
                    Ok(_) => {
                        out.push(Captured {
                            relative,
                            kind: CapturedKind::Unsupported("no extractor for path family"),
                            byte_len: Some(len),
                        });
                        continue;
                    }
                    Err(err) => {
                        out.push(Captured {
                            relative,
                            kind: CapturedKind::Unavailable(err.to_string()),
                            byte_len: Some(len),
                        });
                        continue;
                    }
                }
            }
        }
        if len > limits.max_file_bytes {
            out.push(Captured {
                relative,
                kind: CapturedKind::Unsupported("file exceeds the bounded document read size"),
                byte_len: Some(len),
            });
            continue;
        }
        // The aggregate budget is charged before the read it bounds,
        // from the length already checked: a visible refusal before the
        // read, not an unbounded read followed by a complaint.
        *total_bytes = total_bytes.saturating_add(len);
        if *total_bytes > limits.max_total_bytes {
            return Err(AtlasError::InvalidRequest(format!(
                "document-tree source exceeds the {}-byte bounded aggregate read budget for one \
                 capture; raise document_max_total_bytes in this estate's .wirk/resources.json \
                 to admit a larger collection",
                limits.max_total_bytes
            )));
        }
        match read_bounded_no_follow(dir, &name, len, limits) {
            Ok(bytes) => {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let digest = hex(&hasher.finalize());
                out.push(Captured {
                    relative,
                    kind: CapturedKind::Content { digest, bytes },
                    byte_len: Some(len),
                });
            }
            // A permission-denied file, one that vanished mid-read, or
            // one that became a FIFO or device after it was classified,
            // does not abort the whole collection: every other readable
            // document stays usable.
            Err(err) => out.push(Captured {
                relative,
                kind: CapturedKind::Unavailable(err.to_string()),
                byte_len: Some(len),
            }),
        }
    }
    Ok(())
}

/// Attaches generation-dependent identity — retrieval unit ids, which
/// fold in the `GenerationId` this policy cannot know until the whole
/// tree has been walked and its manifest identity finalized — to what
/// `capture` already read. Entirely from memory: no filesystem access
/// and no second read of any file, with the same bytes and digest used
/// for both the manifest fold and this.
///
/// `stop` is checked once per entry here as well, and for a different
/// reason than in `capture`. This pass does no I/O, but it is where
/// extraction actually runs (`ExtractorPolicy::units`), so on a
/// collection of large text documents it is the more expensive half in
/// CPU and the one that holds the most memory. A walk that could be
/// stopped and an extraction that could not would leave the operator's
/// cancellation reaching only the cheaper part of the work.
pub(crate) fn finish(
    generation: &GenerationId,
    policy: &ExtractorPolicy,
    captured: Vec<Captured>,
    stop: &wirk_core::jobs::JobStop,
) -> Result<Vec<ResourceRecord>, AtlasError> {
    let mut records = Vec::with_capacity(captured.len());
    for entry in captured {
        stop.check().map_err(stopped)?;
        let is_symlink = matches!(entry.kind, CapturedKind::Symlink);
        let (disposition, detail, object_id, byte_len, units) = match entry.kind {
            CapturedKind::Symlink => (
                CoverageDisposition::Unsupported,
                Some("symlink".to_string()),
                None,
                entry.byte_len,
                vec![],
            ),
            CapturedKind::Special(label) => (
                CoverageDisposition::Unsupported,
                Some(label.to_string()),
                None,
                entry.byte_len,
                vec![],
            ),
            CapturedKind::Excluded => (
                CoverageDisposition::Excluded,
                Some("fixed secret-like policy".to_string()),
                None,
                entry.byte_len,
                vec![],
            ),
            CapturedKind::Unsupported(detail) => (
                CoverageDisposition::Unsupported,
                Some(detail.to_string()),
                None,
                entry.byte_len,
                vec![],
            ),
            CapturedKind::Unavailable(detail) => (
                CoverageDisposition::Unavailable,
                Some(detail),
                None,
                entry.byte_len,
                vec![],
            ),
            // The null-byte heuristic only screens the plain-text path:
            // an admitted document format's own bytes are expected to be
            // binary, and `policy.units` (`crate::document::render`) reads
            // them directly rather than treating them as UTF-8. Decided
            // from the same bytes the family decision now reads, so a
            // detected document is never rejected here as a binary blob.
            CapturedKind::Content { digest, bytes }
                if policy.family(&entry.relative, &bytes) != Some(ContentFamily::Document)
                    && bytes.contains(&0) =>
            {
                (
                    CoverageDisposition::Unsupported,
                    Some("binary blob".to_string()),
                    Some(digest),
                    Some(bytes.len() as u64),
                    vec![],
                )
            }
            CapturedKind::Content { digest, bytes } => {
                match policy.units(generation, &entry.relative, &digest, &bytes) {
                    Ok(units) => (
                        CoverageDisposition::Indexed,
                        None,
                        Some(digest),
                        Some(bytes.len() as u64),
                        units,
                    ),
                    Err(detail) => (
                        CoverageDisposition::Error,
                        Some(detail.to_string()),
                        Some(digest),
                        Some(bytes.len() as u64),
                        vec![],
                    ),
                }
            }
        };
        records.push(ResourceRecord {
            path: entry.relative,
            mode: (if is_symlink { "120000" } else { "100644" }).into(),
            object_id,
            byte_len,
            disposition,
            detail,
            units,
        });
    }
    records.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(records)
}

/// Exact bytes for one already-validated resource, read live from the
/// document tree exactly as `git::blob` reads live from the Git object
/// store — through the same no-follow descriptor chain `capture` uses,
/// walked fresh from `root` for this one path so that no intermediate
/// component is trusted from an earlier, now-stale open.
///
/// Re-hashing this one path and comparing it to the coordinate's own
/// `object_id` is the whole precondition, and it is deliberately the
/// whole precondition: it already proves the returned bytes are exactly
/// the bytes the coordinate names, and nothing about the rest of the
/// tree needs to be true for that to hold. A whole-tree manifest check
/// here would instead make editing any other file in the collection
/// invalidate every coordinate previously issued against it.
pub(crate) fn blob(
    root: &Path,
    relative_path: &[u8],
    expected_object_id: &str,
    limits: &CaptureLimits,
) -> Result<Vec<u8>, AtlasError> {
    if relative_path.is_empty() {
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "{} names no path",
            display_path(root, relative_path)
        )));
    }
    let root_file = File::open(root)?;
    let mut dir: OwnedFd = root_file.into();
    let segments: Vec<&[u8]> = relative_path.split(|byte| *byte == b'/').collect();
    let Some((last, ancestors)) = segments.split_last() else {
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "{} names no path",
            display_path(root, relative_path)
        )));
    };
    for segment in ancestors {
        dir = open_no_follow(dir.as_fd(), segment, true)
            .map_err(|err| path_unavailable(root, relative_path, &err))?;
    }
    let stat =
        lstat_at(dir.as_fd(), last).map_err(|err| path_unavailable(root, relative_path, &err))?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "{} is no longer an ordinary file",
            display_path(root, relative_path)
        )));
    }
    let len = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    if len > limits.max_file_bytes {
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "{} now exceeds the bounded document read size",
            display_path(root, relative_path)
        )));
    }
    // The same window `capture` names, on the live resolution path: the
    // entry has been classified as an ordinary file and is about to be
    // opened, and the open below neither follows a symlink nor blocks
    // on a FIFO that replaced it.
    crate::store::checkpoint(OPEN_WINDOW);
    let bytes = read_bounded_no_follow(dir.as_fd(), last, len, limits).map_err(|err| {
        AtlasError::SourceBytesUnavailable(format!("{} {err}", display_path(root, relative_path)))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hex(&hasher.finalize());
    if digest != expected_object_id {
        return Err(AtlasError::SourceBytesUnavailable(format!(
            "{} no longer matches the recorded content hash",
            display_path(root, relative_path)
        )));
    }
    Ok(bytes)
}

/// The document counterpart of `git::blobs`: the bytes behind many
/// resources of one generation, keyed by the content identity each one
/// was recorded under.
///
/// **One read per distinct identity, not per hit.** A search answers
/// from many units cut out of far fewer files, and the same file backs
/// every unit taken from it, so `wanted` is deduplicated by object id
/// before anything is opened. Where two different paths hold byte-
/// identical content they share one object id and therefore one entry:
/// the first path that reads successfully supplies the bytes, which is
/// correct precisely because the identity is the content hash — any
/// path that verifies against it holds exactly those bytes.
///
/// **A resource that cannot be read is omitted, not fatal**, matching
/// `git::blobs`: the caller sees a missing key, degrades its own
/// coverage disclosure and skips that unit, rather than losing every
/// other answer in the collection to one unreadable file.
pub(crate) fn blobs(
    root: &Path,
    wanted: &[(Vec<u8>, String)],
    limits: &CaptureLimits,
) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out: std::collections::BTreeMap<String, Vec<u8>> = std::collections::BTreeMap::new();
    for (path, object_id) in wanted {
        if out.contains_key(object_id) {
            continue;
        }
        if let Ok(bytes) = blob(root, path, object_id, limits) {
            out.insert(object_id.clone(), bytes);
        }
    }
    out
}

fn path_unavailable(root: &Path, relative: &[u8], err: &std::io::Error) -> AtlasError {
    match err.kind() {
        std::io::ErrorKind::NotFound => AtlasError::SourceBytesUnavailable(format!(
            "{} no longer exists",
            display_path(root, relative)
        )),
        _ if err.raw_os_error() == Some(libc::ELOOP) => {
            AtlasError::SourceBytesUnavailable(format!(
                "{} is no longer an ordinary path (a symlink was found and refused)",
                display_path(root, relative)
            ))
        }
        _ => AtlasError::SourceBytesUnavailable(format!("{} {err}", display_path(root, relative))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    /// The built-in defaults, which is what an estate with no
    /// `resources.json` of its own runs under.
    fn limits() -> CaptureLimits {
        CaptureLimits::default()
    }

    /// For the tests that are not about stopping: no cancellation and no
    /// deadline, so a capture is bounded only by `limits`.
    fn unstopped() -> wirk_core::jobs::JobStop {
        wirk_core::jobs::JobStop::unbounded()
    }

    fn captured_names(entries: &[Captured]) -> Vec<String> {
        entries
            .iter()
            .map(|e| String::from_utf8_lossy(&e.relative).into_owned())
            .collect()
    }

    #[test]
    fn the_manifest_identity_is_a_pure_function_of_the_walked_bytes() {
        // Same-name, same-content files in two different document-tree
        // roots must never collapse to the same identity by *this*
        // function alone: the manifest hash is a pure function of the
        // walked bytes, so two separate roots with identical content
        // are told apart by their caller (`AtlasStore` binds the root
        // into `Membership::locator`/`SourceGeneration::locator`
        // separately; `resolve_exact` checks the locator too).
        let a = tempfile::tempdir().expect("tempdir a");
        let b = tempfile::tempdir().expect("tempdir b");
        std::fs::write(a.path().join("readme.txt"), b"hello world\n").expect("write a");
        std::fs::write(b.path().join("readme.txt"), b"hello world\n").expect("write b");
        let policy = ExtractorPolicy::markdown_only();
        let (revision_a, content_a, _) =
            capture(a.path(), &policy, &limits(), &unstopped()).expect("capture a");
        let (revision_b, content_b, _) =
            capture(b.path(), &policy, &limits(), &unstopped()).expect("capture b");
        assert_eq!(revision_a, revision_b);
        assert_eq!(content_a, content_b);
    }

    #[test]
    fn revision_is_not_git_shaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.md"), b"# hi\n").expect("write");
        let policy = ExtractorPolicy::markdown_only();
        let (revision, content, _) =
            capture(dir.path(), &policy, &limits(), &unstopped()).expect("capture");
        assert_eq!(revision.len(), 64, "a git commit sha is 40 hex characters");
        assert!(revision.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(content.starts_with("sha256:"));
    }

    #[test]
    fn a_symlink_is_reported_unsupported_and_never_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"target\n").expect("write");
        std::os::unix::fs::symlink("real.txt", dir.path().join("link.txt")).expect("symlink");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let (_, _, captured) =
            capture(dir.path(), &policy, &limits(), &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let link = records
            .iter()
            .find(|r| r.path == b"link.txt")
            .expect("link.txt present");
        assert_eq!(link.disposition, CoverageDisposition::Unsupported);
        assert_eq!(link.detail.as_deref(), Some("symlink"));
        assert!(link.object_id.is_none());
        assert_eq!(link.mode, "120000");
    }

    #[test]
    fn an_oversize_file_is_bounded_not_read_fully() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Sized from the bound actually in force, so this stays the
        // oversize case whatever the estate's policy says.
        let limits = limits();
        std::fs::write(
            dir.path().join("huge.md"),
            vec![b'a'; (limits.max_file_bytes + 1) as usize],
        )
        .expect("write huge file");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let (_, _, captured) =
            capture(dir.path(), &policy, &limits, &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let huge = records
            .iter()
            .find(|r| r.path == b"huge.md")
            .expect("huge.md present");
        assert_eq!(huge.disposition, CoverageDisposition::Unsupported);
        assert!(
            huge.detail
                .as_deref()
                .unwrap_or_default()
                .contains("bounded")
        );
        assert!(huge.object_id.is_none());
    }

    #[test]
    fn excluded_secret_like_content_is_never_opened() {
        // Pins the bundled decision in `recurse`: `.env` matches
        // `ExtractorPolicy::excluded` before any read, so its actual
        // bytes never reach a hasher or an extractor, only its length
        // does.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".env"), b"SECRET=do-not-read-me\n").expect("write");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let (_, _, captured) =
            capture(dir.path(), &policy, &limits(), &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let env = records
            .iter()
            .find(|r| r.path == b".env")
            .expect(".env present");
        assert_eq!(env.disposition, CoverageDisposition::Excluded);
        assert!(env.object_id.is_none(), "excluded content is never hashed");
    }

    #[test]
    fn a_fifo_is_reported_unsupported_not_silently_absent() {
        // A special file must appear in coverage by name, not vanish
        // from it.
        let dir = tempfile::tempdir().expect("tempdir");
        let fifo_path = dir.path().join("pipe");
        let cpath = std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let (_, _, captured) =
            capture(dir.path(), &policy, &limits(), &unstopped()).expect("capture");
        assert!(captured_names(&captured).contains(&"pipe".to_string()));
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let pipe = records
            .iter()
            .find(|r| r.path == b"pipe")
            .expect("pipe present in coverage");
        assert_eq!(pipe.disposition, CoverageDisposition::Unsupported);
        assert_eq!(pipe.detail.as_deref(), Some("fifo"));
    }

    #[test]
    fn an_unreadable_file_is_disclosed_without_aborting_the_rest_of_the_collection() {
        // One unreadable document must not refuse admission of every
        // other, readable document.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("readable.md"), b"# ok\n").expect("write readable");
        let blocked = dir.path().join("blocked.md");
        std::fs::write(&blocked, b"# secret\n").expect("write blocked");
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let result = capture(dir.path(), &policy, &limits(), &unstopped());
        // Restore permissions so the tempdir can be cleaned up
        // regardless of the assertion outcome below.
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644))
            .expect("restore permissions");
        let (_, _, captured) = result.expect("capture must not abort on one unreadable file");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let blocked_record = records
            .iter()
            .find(|r| r.path == b"blocked.md")
            .expect("blocked.md present");
        assert_eq!(blocked_record.disposition, CoverageDisposition::Unavailable);
        let readable = records
            .iter()
            .find(|r| r.path == b"readable.md")
            .expect("readable.md present");
        assert_eq!(readable.disposition, CoverageDisposition::Indexed);
    }

    #[test]
    fn a_directory_replaced_by_a_symlink_is_refused_not_followed() {
        // The no-follow guarantee holds for an intermediate component
        // discovered mid-walk, not only the final one. This does not
        // induce the concurrent-replacement race itself — that needs two
        // real threads at a real syscall window, and `OPEN_WINDOW` is
        // where a test drives one. What this pins is that a directory
        // entry which *is already* a symlink by the time `recurse` opens
        // it is refused by `open_no_follow`'s `O_NOFOLLOW`, the same
        // mechanism a genuine race would hit.
        let dir = tempfile::tempdir().expect("tempdir");
        let real_dir = dir.path().join("real");
        std::fs::create_dir(&real_dir).expect("mkdir");
        std::fs::write(real_dir.join("inside.md"), b"# nested\n").expect("write nested");
        std::os::unix::fs::symlink(&real_dir, dir.path().join("via_link")).expect("symlink");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let (_, _, captured) =
            capture(dir.path(), &policy, &limits(), &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        assert!(
            records.iter().any(|r| r.path == b"real/inside.md"),
            "the real directory is walked normally"
        );
        let link = records
            .iter()
            .find(|r| r.path == b"via_link")
            .expect("via_link present as its own entry");
        assert_eq!(link.disposition, CoverageDisposition::Unsupported);
        assert_eq!(link.detail.as_deref(), Some("symlink"));
        assert!(
            !records.iter().any(|r| r.path.starts_with(b"via_link/")),
            "nothing under the symlink target was walked a second time through it"
        );
    }

    #[test]
    fn blob_resolves_an_unchanged_file_after_an_unrelated_edit() {
        // `blob`'s own per-file hash check does not care what else in
        // the tree changed.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("stable.md"), b"# stable\n").expect("write stable");
        std::fs::write(dir.path().join("other.md"), b"# v1\n").expect("write other v1");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let (_, _, captured) =
            capture(dir.path(), &policy, &limits(), &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let stable = records
            .iter()
            .find(|r| r.path == b"stable.md")
            .expect("stable.md present");
        let object_id = stable.object_id.clone().expect("indexed file has an id");
        std::fs::write(dir.path().join("other.md"), b"# v2, changed\n").expect("edit other");
        let bytes = blob(dir.path(), b"stable.md", &object_id, &limits())
            .expect("stable.md still resolves after an unrelated edit");
        assert_eq!(bytes, b"# stable\n");
    }

    #[test]
    fn blob_reports_unavailable_for_a_changed_file_not_the_whole_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("moved.md"), b"# v1\n").expect("write v1");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());
        let (_, _, captured) =
            capture(dir.path(), &policy, &limits(), &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let object_id = records
            .iter()
            .find(|r| r.path == b"moved.md")
            .and_then(|r| r.object_id.clone())
            .expect("moved.md indexed with an id");
        std::fs::write(dir.path().join("moved.md"), b"# v2\n").expect("edit moved");
        let err = blob(dir.path(), b"moved.md", &object_id, &limits())
            .expect_err("stale coordinate refused");
        assert!(matches!(err, AtlasError::SourceBytesUnavailable(_)));
    }

    /// D4's decisive unit control. Directories are never emitted as
    /// resources, so a budget compared against emitted resources cannot
    /// see them at all: a tree made entirely of directories would walk
    /// without bound however wide it got. Counting every *examined*
    /// name is what makes the bound describe the work.
    #[test]
    fn directories_count_against_the_traversal_budget() {
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..12 {
            std::fs::create_dir(dir.path().join(format!("d{n}"))).expect("mkdir");
        }
        let policy = ExtractorPolicy::markdown_only();
        let mut limits = limits();
        limits.max_entries = 5;
        let err = capture(dir.path(), &policy, &limits, &unstopped())
            .expect_err("a tree of directories alone must still trip the entry budget");
        let AtlasError::InvalidRequest(detail) = err else {
            panic!("expected a visible refusal, got {err:?}");
        };
        assert!(
            detail.contains("entry bounded traversal budget"),
            "refusal should name the budget it hit: {detail}"
        );
        assert!(
            detail.contains("document_max_entries"),
            "refusal should name the setting an operator can raise: {detail}"
        );
    }

    /// The other half of D4: the budget is tested *while* a directory is
    /// streamed, so one very wide directory is refused partway through
    /// rather than listed, allocated and sorted in full first.
    #[test]
    fn one_wide_directory_is_refused_before_it_is_fully_listed() {
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..200 {
            std::fs::write(dir.path().join(format!("f{n}.md")), b"# x\n").expect("write");
        }
        let policy = ExtractorPolicy::markdown_only();
        let mut limits = limits();
        limits.max_entries = 10;
        let err = capture(dir.path(), &policy, &limits, &unstopped())
            .expect_err("wide directory refused");
        assert!(matches!(err, AtlasError::InvalidRequest(_)));
    }

    /// D5: the bounds are the estate's, not this module's. A collection
    /// the default refuses is admitted under a policy that raises the
    /// relevant field, and nothing else about the capture changes.
    #[test]
    fn a_raised_policy_bound_admits_a_collection_the_default_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..20 {
            std::fs::write(dir.path().join(format!("f{n}.md")), b"# x\n").expect("write");
        }
        let policy = ExtractorPolicy::markdown_only();
        let mut tight = limits();
        tight.max_entries = 5;
        assert!(
            capture(dir.path(), &policy, &tight, &unstopped()).is_err(),
            "the tight bound must refuse this collection"
        );

        let mut raised = tight;
        raised.max_entries = 500;
        let (_, _, captured) =
            capture(dir.path(), &policy, &raised, &unstopped()).expect("a raised bound admits it");
        assert_eq!(captured.len(), 20);
    }

    /// D5, the per-file half: a file over the per-file bound is named in
    /// coverage as unsupported rather than dropped, and raising the
    /// bound indexes it.
    #[test]
    fn a_file_over_the_per_file_bound_is_named_then_admitted_when_raised() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("big.md"), vec![b'x'; 4096]).expect("write big");
        let policy = ExtractorPolicy::markdown_only();
        let generation = GenerationId("g-test".into());

        let mut tight = limits();
        tight.max_file_bytes = 1024;
        let (_, _, captured) = capture(dir.path(), &policy, &tight, &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let big = records
            .iter()
            .find(|r| r.path == b"big.md")
            .expect("the oversized file is still named");
        assert_eq!(big.disposition, CoverageDisposition::Unsupported);
        assert_eq!(big.byte_len, Some(4096));

        let mut raised = tight;
        raised.max_file_bytes = 1024 * 1024;
        let (_, _, captured) =
            capture(dir.path(), &policy, &raised, &unstopped()).expect("capture");
        let records = finish(&generation, &policy, captured, &unstopped()).expect("finish");
        let big = records
            .iter()
            .find(|r| r.path == b"big.md")
            .expect("still named");
        assert_eq!(big.disposition, CoverageDisposition::Indexed);
    }

    /// D5, the aggregate half: many individually in-bound files still
    /// refuse the whole capture visibly rather than staging a partial
    /// generation reported as complete.
    #[test]
    fn the_aggregate_budget_refuses_the_whole_capture_visibly() {
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..10 {
            std::fs::write(dir.path().join(format!("f{n}.md")), vec![b'x'; 1024]).expect("write");
        }
        let policy = ExtractorPolicy::markdown_only();
        let mut limits = limits();
        limits.max_total_bytes = 4096;
        let err = capture(dir.path(), &policy, &limits, &unstopped())
            .expect_err("aggregate budget refuses");
        let AtlasError::InvalidRequest(detail) = err else {
            panic!("expected a visible refusal, got {err:?}");
        };
        assert!(
            detail.contains("document_max_total_bytes"),
            "refusal should name the setting an operator can raise: {detail}"
        );
    }

    /// D6, at the level a single thread can pin: `open_no_follow` on a
    /// non-directory carries `O_NONBLOCK`, so opening a FIFO with no
    /// writer returns instead of parking. Without the flag this test
    /// would not fail — it would hang, which is exactly the defect.
    #[test]
    fn opening_a_writerless_fifo_returns_rather_than_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fifo_path = dir.path().join("pipe");
        let cpath = std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let parent = File::open(dir.path()).expect("open parent");
        let opened = open_no_follow(parent.as_fd(), b"pipe", false)
            .expect("a writerless fifo opens immediately under O_NONBLOCK");
        let meta = File::from(opened).metadata().expect("fstat");
        assert!(
            !meta.file_type().is_file(),
            "the post-open fstat is what refuses it, and it must be reachable"
        );
    }
}
