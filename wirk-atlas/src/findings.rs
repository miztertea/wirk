//! W-B: the estate's derived, rebuildable Findings index —
//! `<estate>/atlas/findings.ndjson`. Reads and writes reuse
//! `AtlasStore`'s own proven durability discipline verbatim
//! (`append_relationship`'s own doc: "not a second ad hoc durability
//! protocol"), per `BUILD-AMENDMENTS.md`'s correction over the draft
//! brief's "never rewritten" prose. The journal of the raising Work
//! stays canonical for Finding/Settlement/Assertion/Application; this
//! index is derived and never the only copy — a malformed row is a hard
//! error that blocks reads until `rebuild_finding_rows` recreates the
//! file from journals, and `wirkd` (the only writer, through its one
//! `WirkdState.atlas` mutex) reconciles a missing row at startup.

use std::fs;

use serde::{Deserialize, Serialize};
use ulid::Ulid;
use wirk_core::{ApplicationRef, Assertion, EventId, Finding, FindingId, Settlement, WorkId};

use crate::store::checkpoint;
use crate::{AtlasError, AtlasStore};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRowId(pub String);

impl FindingRowId {
    /// Content-addressed exactly like `RelationshipId::compute`
    /// (`domain.rs`): idempotence needs no separate dedup protocol, only
    /// a stable id over what makes a row unique — the finding, which
    /// kind of row it is, and the journal event that produced it (two
    /// settlements of the same finding can never both exist, but an
    /// assertion and a later application legitimately can, hence
    /// including `row_kind`).
    pub fn compute(finding: &FindingId, row_kind: FindingRowKind, origin_event: &EventId) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        absorb(&mut hasher, finding.0.as_bytes());
        absorb(
            &mut hasher,
            match row_kind {
                FindingRowKind::Settled => b"settled",
                FindingRowKind::Asserted => b"asserted",
                FindingRowKind::Applied => b"applied",
            },
        );
        absorb(&mut hasher, origin_event.0.as_bytes());
        Self(format!("fr-{}", hex(&hasher.finalize())))
    }
}

fn absorb(hasher: &mut sha2::Sha256, part: &[u8]) {
    use sha2::Digest;
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingRowKind {
    Settled,
    Asserted,
    Applied,
}

/// Where a row came from — the raising Work's journal is still
/// canonical; this only helps a reader find it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub work: WorkId,
    pub raised_event: EventId,
    pub row_event: EventId,
}

/// One line of the index: the complete `Finding` plus whichever of
/// `settlement`/`assertion`/`applied` this row's own `kind` names.
/// `superseded_by` is set on a settled row whose class was
/// `SupersededInOrigin`, naming the finding that replaced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRow {
    pub id: FindingRowId,
    pub kind: FindingRowKind,
    pub finding: Finding,
    pub origin: Origin,
    #[serde(default)]
    pub settlement: Option<Settlement>,
    #[serde(default)]
    pub assertion: Option<Assertion>,
    #[serde(default)]
    pub applied: Option<ApplicationRef>,
    #[serde(default)]
    pub superseded_by: Option<FindingId>,
}

/// What a batch append could not put in the index, and how much of it.
///
/// `pending` is the number of rows the index does not hold as a
/// consequence — `None` when the index could not even be *read*, which
/// is a different fact from "n rows are missing" and is reported as the
/// unknown it is, and `Some(0)` for a failure raised *after* the atomic
/// rename, where the rows are already visible and only their directory
/// entry's durability is in doubt. The raising Work's journal is
/// unaffected either way: this index is derived, and a failure here is a
/// projection that is behind, never a lost record.
#[derive(Debug)]
pub struct FindingIndexUnwritten {
    pub pending: Option<usize>,
    pub error: AtlasError,
}

impl std::fmt::Display for FindingIndexUnwritten {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.pending {
            Some(0) => write!(
                f,
                "the findings index holds every offered row, but the write did not complete cleanly: {}",
                self.error
            ),
            Some(pending) => write!(
                f,
                "{pending} row(s) did not reach the findings index: {}",
                self.error
            ),
            None => write!(
                f,
                "the findings index could not be read, so what it is missing is unknown: {}",
                self.error
            ),
        }
    }
}

impl std::error::Error for FindingIndexUnwritten {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl AtlasStore {
    /// Every row currently in the index. A malformed line is a hard
    /// error that blocks every read (`relationships()`'s own rule,
    /// reused verbatim) — never silently skipped, never a partial index.
    pub fn findings(&self) -> Result<Vec<FindingRow>, AtlasError> {
        read_rows(self.root_path())
    }

    /// Appends one row, deduplicating by its own content-addressed id
    /// (idempotent: a retried settlement mint is always safe, whether or
    /// not the caller received `DurabilityUncertain` for the write that
    /// made an earlier attempt visible). Atomic whole-file rewrite
    /// through `write_sync`, the same discipline `append_relationship`
    /// uses (`store.rs`'s own doc: "P3's relationship count is small
    /// enough that whole-file rewrite is the R1/R3 minimum" — the
    /// findings index is smaller still).
    ///
    /// One row is the degenerate batch: this delegates to
    /// `append_finding_rows` rather than keeping a second copy of the
    /// read/dedup/rewrite sequence.
    pub fn append_finding_row(&mut self, row: &FindingRow) -> Result<(), AtlasError> {
        self.append_finding_rows(std::slice::from_ref(row))
            .map(|_| ())
            .map_err(|unwritten| unwritten.error)
    }

    /// Appends every row of `rows` the index does not already hold, in
    /// **one** read and **at most one** atomic rewrite.
    ///
    /// This is the same read/dedup/rewrite `append_finding_row` always
    /// performed, hoisted out of the caller's loop. A reconciliation
    /// sweep offers every row the estate's journals support on every
    /// mutation; calling the single-row form once per row re-read and
    /// re-wrote the whole file once per row, so one mutation cost
    /// O(rows²) bytes and O(rows) `fsync` pairs (ruling 0116's measured
    /// second limit). Folding the sweep into one rewrite makes it
    /// O(rows) bytes and one `fsync` pair, and a sweep that finds
    /// nothing missing writes **nothing at all** — the common case after
    /// the first mutation.
    ///
    /// Dedup is unchanged and still the row's own content-addressed id
    /// (`FindingRowId::compute`): against what the file already holds,
    /// and against earlier entries of `rows` itself, so an offered batch
    /// containing the same row twice cannot put it in the file twice.
    /// Order is preserved: existing rows first, then the missing ones in
    /// the order offered, which is the order the single-row loop
    /// produced.
    ///
    /// Returns how many rows were actually appended. On failure the
    /// error says how many rows the index does **not** hold as a result,
    /// so a caller can report its own projection health honestly instead
    /// of only naming the row it happened to be on.
    pub fn append_finding_rows(
        &mut self,
        rows: &[FindingRow],
    ) -> Result<usize, FindingIndexUnwritten> {
        let mut existing = match self.findings() {
            Ok(existing) => existing,
            // The read itself failed, so how many of `rows` are missing
            // is genuinely unknown — reported as unknown rather than
            // guessed at `rows.len()`, which would be a number nobody
            // measured.
            Err(error) => {
                return Err(FindingIndexUnwritten {
                    pending: None,
                    error,
                });
            }
        };
        let before = existing.len();
        for row in rows {
            if existing.iter().any(|held| held.id == row.id) {
                continue;
            }
            existing.push(row.clone());
        }
        let appended = existing.len() - before;
        if appended == 0 {
            return Ok(0);
        }
        match self.rewrite_rows(&existing) {
            Ok(()) => Ok(appended),
            // `DurabilityUncertain` is raised *after* the atomic rename,
            // so every one of these rows is already in the file a fresh
            // reader opens: nothing is pending, and saying `appended`
            // would report rows that landed as rows that did not. The
            // uncertainty is the directory entry's, which the error text
            // itself names. Every other failure happened before the
            // rename, so the index genuinely does not hold them.
            Err(error @ AtlasError::DurabilityUncertain(_)) => Err(FindingIndexUnwritten {
                pending: Some(0),
                error,
            }),
            Err(error) => Err(FindingIndexUnwritten {
                pending: Some(appended),
                error,
            }),
        }
    }

    /// `wirk atlas findings --rebuild`: replaces the whole file with
    /// exactly `rows` — the recreation path from journals alone
    /// (`server.rs`'s own daemon-wide journal walk builds `rows`; this
    /// method only owns the atomic file replacement).
    pub fn rebuild_finding_rows(&mut self, rows: Vec<FindingRow>) -> Result<(), AtlasError> {
        self.rewrite_rows(&rows)
    }

    fn rewrite_rows(&self, rows: &[FindingRow]) -> Result<(), AtlasError> {
        let mut bytes = Vec::new();
        for row in rows {
            bytes.extend_from_slice(&serde_json::to_vec(row)?);
            bytes.push(b'\n');
        }
        let temp = self
            .root_path()
            .join(format!(".tmp-findings-{}", Ulid::generate()));
        self.write_sync(&temp, &bytes)?;
        checkpoint("findings-file-synced");
        fs::rename(&temp, self.root_path().join("findings.ndjson"))?;
        checkpoint("findings-renamed");
        // The rename above already made every row here visible to a
        // fresh reader; a failure syncing the containing directory after
        // that point is reported as visible-but-unconfirmed, exactly as
        // `persist_catalog`/`append_relationship` report the identical
        // window — never a bare I/O error indistinguishable from "never
        // wrote".
        std::fs::File::open(self.root_path())
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                AtlasError::DurabilityUncertain(format!(
                    "findings index ({} rows) is visible; directory sync failed: {error}",
                    rows.len()
                ))
            })
    }
}

/// A row the index durably holds that a walk of the estate's canonical
/// journals did not produce.
///
/// **Why this is evidence and not noise.** A row's id is
/// content-addressed over the finding, the row kind and the *journal
/// event* that produced it (`FindingRowId::compute`), the Work journals
/// this index is derived from are append-only — `Journal::append` is
/// their only writer and nothing in the product removes a journal, a
/// Work directory or an event — and the lifecycle has exactly four
/// Finding events (raised, settled, asserted, applied) and no
/// retraction, withdrawal or expiry of any of them. So a complete walk
/// of an intact estate reproduces every row a previous walk produced,
/// always. A row the walk did not reproduce therefore means the
/// canonical material behind it is absent or unaccounted for, and the
/// walk that missed it is not a complete observation of the estate.
///
/// What this is **not**: authority. It says a walk was incomplete. It
/// never says what the missing events were, and nothing reconstructs a
/// Work or an event from it — the journal remains the only record.
/// Row *content* is deliberately not compared, only the id: a
/// re-serialised or differently-ordered row is not missing evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaccountedFindingRow {
    /// The Work whose journal a complete walk would have read this row
    /// out of.
    pub work: WorkId,
    pub row: FindingRowId,
}

/// Every row of `held` whose id `offered` does not contain, in the
/// index's own order.
///
/// **`held` must be what the index held *before* the walk that produced
/// `offered` began.** The index legitimately moves forward under a walk:
/// an ordinary sweep walks outside the Atlas lock, so a concurrent
/// mutation can journal and index a row this walk is simply older than,
/// and that is a healthy estate, not a missing one. Comparing against
/// the pre-walk basis excludes exactly those and nothing else — every
/// row that was in the index *before* the walk started is backed by a
/// journal event that already existed, and an append-only journal
/// cannot have stopped holding it.
///
/// One pass over each side: no re-read of anything, and no per-row scan
/// of the index.
pub fn unaccounted_finding_rows(
    held: &[FindingRow],
    offered: &[FindingRow],
) -> Vec<UnaccountedFindingRow> {
    let produced: std::collections::HashSet<&str> =
        offered.iter().map(|row| row.id.0.as_str()).collect();
    held.iter()
        .filter(|held| !produced.contains(held.id.0.as_str()))
        .map(|held| UnaccountedFindingRow {
            work: held.origin.work.clone(),
            row: held.id.clone(),
        })
        .collect()
}

/// The index file's own name, and the two names a copy of it that could
/// not be parsed is kept under. Public because the operator recovery
/// this drives is described in the daemon's own reply text, and a name
/// an operator is told to type is part of the contract, not an
/// implementation detail.
///
/// A preserved copy is **never** a second index: nothing reads it, no
/// query consults it, no row is ever taken out of it, and it is never
/// used to reconstruct a canonical event. Its whole job is to be the
/// bytes that were there, kept where an operator can look at them,
/// instead of being replaced away by the one command that exists to
/// repair the file.
pub const FINDINGS_INDEX_FILE: &str = "findings.ndjson";
/// Bytes of a standing index at least one line of which was not a row,
/// kept aside before a rebuild replaced the file.
pub const PRESERVED_INDEX_PREFIX: &str = "findings.ndjson.unreadable-";
/// The same bytes after an administrator reviewed them and said so.
/// Retirement is a rename: the bytes are never removed by this product.
pub const RETIRED_INDEX_PREFIX: &str = "findings.ndjson.retired-";

/// One line of the index file that is not a `FindingRow`.
///
/// The line *number*, not its content: the bytes stay in the preserved
/// copy, which is where an operator reads them, and a malformed line
/// could hold anything at all — including a fragment of a row whose
/// disclosure scope this crate cannot judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedFindingLine {
    pub line: usize,
    pub error: String,
}

/// What a line-by-line **recovery** read could still make out of an
/// index that `findings()` refuses whole.
///
/// This exists because one malformed line is not evidence that no valid
/// row is in the file. `findings()`'s all-or-nothing rule is right for
/// every ordinary read — a partial index answering a query is exactly
/// the silent subset this whole component refuses to produce — but the
/// destructive rebuild needs to know *what the index is known to hold*
/// before it replaces it, and answering "nothing at all" there is how a
/// published row gets deleted at exit 0 (ruling 0130).
///
/// **Evidence, never authority, and never a read path.** The rows here
/// are only ever compared by id against a canonical walk
/// (`unaccounted_finding_rows`). Nothing returns them to a caller,
/// nothing writes them back into the index, and nothing reconstructs a
/// Work or a journal event from them. `malformed` is the honest
/// remainder: lines whose content is unknown and therefore whose
/// absence from a walk cannot be checked at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalvagedFindingIndex {
    pub rows: Vec<FindingRow>,
    pub malformed: Vec<MalformedFindingLine>,
}

impl AtlasStore {
    /// Every row of the standing index that still parses, plus every
    /// line that does not — the recovery read behind `--rebuild`'s
    /// preservation check.
    ///
    /// Fails only when the file itself cannot be read (a real `EACCES`,
    /// a directory where the file should be). "Absent" is not a failure
    /// and not a salvage: an absent index holds no rows and no malformed
    /// lines, exactly as `findings()` already reports it.
    pub fn salvage_findings(&self) -> Result<SalvagedFindingIndex, AtlasError> {
        let path = self.root_path().join(FINDINGS_INDEX_FILE);
        if !path.exists() {
            return Ok(SalvagedFindingIndex {
                rows: Vec::new(),
                malformed: Vec::new(),
            });
        }
        let content = fs::read_to_string(&path)?;
        let mut rows = Vec::new();
        let mut malformed = Vec::new();
        for_each_index_line(&content, |line, parsed| match parsed {
            Ok(row) => rows.push(row),
            Err(error) => malformed.push(MalformedFindingLine { line, error }),
        });
        Ok(SalvagedFindingIndex { rows, malformed })
    }

    /// Copies the standing index aside under `PRESERVED_INDEX_PREFIX`
    /// and returns the name it was kept under, or `None` when there is
    /// no file to preserve.
    ///
    /// A **copy**, not a move: the caller is about to replace
    /// `findings.ndjson` through its own atomic rename, and a rebuild
    /// that then fails must leave the estate holding the file it started
    /// with. Written through `write_sync` and `rename` — `rewrite_rows`'
    /// own discipline, not a second one — so the preserved copy is on
    /// disk before the replacement it is preserving against begins.
    pub fn preserve_unreadable_index(&self) -> Result<Option<String>, AtlasError> {
        let path = self.root_path().join(FINDINGS_INDEX_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        let name = format!("{PRESERVED_INDEX_PREFIX}{}", Ulid::generate());
        let temp = self
            .root_path()
            .join(format!(".tmp-findings-preserve-{}", Ulid::generate()));
        self.write_sync(&temp, &bytes)?;
        fs::rename(&temp, self.root_path().join(&name))?;
        fs::File::open(self.root_path()).and_then(|directory| directory.sync_all())?;
        checkpoint("findings-index-preserved");
        Ok(Some(name))
    }

    /// Renames every preserved copy to `RETIRED_INDEX_PREFIX`, and
    /// returns the pairs. **Nothing is deleted.** An administrator who
    /// has reviewed what could not be parsed says so with this; the
    /// bytes stay in the estate either way, and the recovery text never
    /// asks anyone to remove the only remaining copy of them.
    ///
    /// A failure part-way through is reported as
    /// `PreservedIndexRetirementFailed`, which carries the renames that
    /// *did* happen and the one that did not — the same shape
    /// `append_finding_rows` already uses to say what a failed batch
    /// left behind, and for the same reason: a caller that is going to
    /// describe the estate afterwards needs to know what actually
    /// changed, not only that something did not.
    ///
    /// The failure is boxed: it carries every pair it renamed and is far
    /// larger than the success, and it is the cold path
    /// (`clippy::result_large_err`).
    pub fn retire_preserved_unreadable_indexes(
        &self,
    ) -> Result<Vec<(String, String)>, Box<PreservedIndexRetirementFailed>> {
        let names = match preserved_unreadable_index_names(self.root_path()) {
            Ok(names) => names,
            Err(error) => {
                return Err(Box::new(PreservedIndexRetirementFailed {
                    retired: Vec::new(),
                    retired_unconfirmed: None,
                    on: None,
                    operation: "list the estate's atlas directory for preserved copies",
                    error,
                }));
            }
        };
        let mut retired: Vec<(String, String)> = Vec::new();
        for name in names {
            let suffix = name
                .strip_prefix(PRESERVED_INDEX_PREFIX)
                .expect("listed by that prefix");
            let to = format!("{RETIRED_INDEX_PREFIX}{suffix}");
            if let Err((operation, error)) = self.move_without_replacing(&name, &to) {
                // What already landed is durable before this returns:
                // the all-success path below is not the only path that
                // renamed something, and a rename this call reports as
                // done must not be a rename nobody synced.
                let retired_unconfirmed = if retired.is_empty() {
                    None
                } else {
                    self.sync_atlas_directory().err()
                };
                return Err(Box::new(PreservedIndexRetirementFailed {
                    retired,
                    retired_unconfirmed,
                    on: Some((name, to)),
                    operation,
                    error,
                }));
            }
            retired.push((name, to));
        }
        if !retired.is_empty()
            && let Err(error) = self.sync_atlas_directory()
        {
            return Err(Box::new(PreservedIndexRetirementFailed {
                retired,
                retired_unconfirmed: None,
                on: None,
                operation: "confirm the rename(s) on disk",
                error,
            }));
        }
        Ok(retired)
    }

    /// Moves `from` to `to` inside the atlas directory **without ever
    /// replacing what is already at `to`**.
    ///
    /// `fs::rename` is `rename(2)`, which silently replaces an existing
    /// regular file at the destination — and the destination here is
    /// derived from the preserved copy's own name, so it is entirely
    /// predictable. The product's own refusal text documents
    /// `findings.ndjson.unreadable-<a name of your choosing>` as the
    /// manual preservation, so an operator naming copies by hand across
    /// repeated maintenance can reach a retired name that is already
    /// taken. A verb whose whole contract is "Nothing is deleted" must
    /// not be the one thing that deletes them.
    ///
    /// **R3, stdlib.** `create_new` is `O_CREAT | O_EXCL`: the kernel
    /// makes the destination name this call's, or tells it the name is
    /// taken. It is a claim, not a look — there is no window between
    /// deciding the name is free and the name becoming ours, so this is
    /// not an exists-check dressed up as a guarantee. The `rename` that
    /// follows replaces this call's own empty placeholder and nothing
    /// else. (`renameat2(RENAME_NOREPLACE)` is the single-syscall form
    /// of the same thing, but it is a new dependency for this crate and
    /// returns `EINVAL` on filesystems that do not implement it, so it
    /// would need this fallback anyway.)
    ///
    /// What it does **not** claim: two syscalls means a process of the
    /// same uid that replaces the placeholder in between still loses
    /// what it put there — the same-user boundary this product states
    /// everywhere else (`PeerIdentity`'s own doc) and does not pretend
    /// past. And a crash between the two leaves an empty file at `to`,
    /// which a later retirement refuses to overwrite exactly as it
    /// refuses any other occupied name: an operator is told, and no
    /// preserved byte is at risk either way.
    fn move_without_replacing(
        &self,
        from: &str,
        to: &str,
    ) -> Result<(), (&'static str, AtlasError)> {
        let to_path = self.root_path().join(to);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to_path)
            .map_err(|error| ("claim the retired name for it", AtlasError::Io(error)))?;
        fs::rename(self.root_path().join(from), &to_path).map_err(|error| {
            // The placeholder is this call's own and holds nothing; the
            // bytes it was reserving the name for are still at `from`.
            // Leaving it would refuse the operator's own retry for no
            // reason. If even this fails the name stays claimed, which
            // is the safe direction and is what the error above says.
            let _ = fs::remove_file(&to_path);
            ("rename it to the retired name", AtlasError::Io(error))
        })
    }

    /// `fsync` of the atlas directory, so a rename this call is about to
    /// report as done is a rename that survives the machine. The same
    /// discipline `rewrite_rows` and `preserve_unreadable_index` use,
    /// not a second one.
    fn sync_atlas_directory(&self) -> Result<(), AtlasError> {
        fs::File::open(self.root_path())
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                AtlasError::DurabilityUncertain(format!(
                    "the rename(s) are visible to a fresh reader; directory sync failed: {error}"
                ))
            })
    }
}

/// What a retirement did before it could not go on.
///
/// The bare `AtlasError` this replaced said only what the kernel said —
/// `I/O: Is a directory (os error 21)` — with no file name, no operation
/// and no word about the renames that had *already* happened. The
/// administrator was left to work out which of their copies had moved
/// from a directory listing, and the health record that came after went
/// on naming a file that was no longer there.
///
/// Every field here is a fact about this call: what landed, whether it
/// is confirmed on disk, and what stopped it. None of it is a claim
/// about bytes being gone — no path in this product removes a preserved
/// copy.
#[derive(Debug)]
pub struct PreservedIndexRetirementFailed {
    /// `(preserved name, retired name)` for every copy actually renamed,
    /// in the order it happened.
    pub retired: Vec<(String, String)>,
    /// `Some` when those renames are visible to a fresh reader but their
    /// directory entries could not be confirmed on disk.
    pub retired_unconfirmed: Option<AtlasError>,
    /// The copy the failure was on, and the name it would have taken.
    /// `None` when nothing was being renamed at the time — the listing,
    /// or the directory sync after every rename had landed.
    pub on: Option<(String, String)>,
    /// What was being attempted, as a verb phrase.
    pub operation: &'static str,
    pub error: AtlasError,
}

impl std::fmt::Display for PreservedIndexRetirementFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.on {
            Some((from, to)) => write!(
                f,
                "the preserved copy `{from}` could not be retired as `{to}` — the failed operation was to {}: {}",
                self.operation, self.error
            )?,
            None => write!(
                f,
                "the retirement could not {}: {}",
                self.operation, self.error
            )?,
        }
        if self.retired.is_empty() {
            write!(f, "; no copy was renamed")?;
        } else {
            write!(
                f,
                "; {} copy(ies) were retired before it: {}",
                self.retired.len(),
                self.retired
                    .iter()
                    .map(|(from, to)| format!("`{from}` as `{to}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
            if let Some(unconfirmed) = &self.retired_unconfirmed {
                write!(f, " ({unconfirmed})")?;
            }
        }
        write!(
            f,
            "; no byte of any preserved copy was removed, and retirement can be run again"
        )
    }
}

impl std::error::Error for PreservedIndexRetirementFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// The preserved copies an estate is currently holding, sorted, by name
/// only. Takes the **estate** root so a caller outside the Atlas mutex
/// can ask without holding it: this reads a directory listing and no
/// index state at all.
pub fn preserved_unreadable_indexes(
    estate_root: &std::path::Path,
) -> Result<Vec<String>, AtlasError> {
    preserved_unreadable_index_names(&estate_root.join("atlas"))
}

fn preserved_unreadable_index_names(
    atlas_root: &std::path::Path,
) -> Result<Vec<String>, AtlasError> {
    if !atlas_root.exists() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(atlas_root)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with(PRESERVED_INDEX_PREFIX) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// The one parser. `findings()` stops at the first failure and
/// `salvage_findings` keeps going, but neither owns a second copy of
/// what a line of this file means.
fn for_each_index_line(content: &str, mut visit: impl FnMut(usize, Result<FindingRow, String>)) {
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        visit(
            index + 1,
            serde_json::from_str::<FindingRow>(line).map_err(|error| error.to_string()),
        );
    }
}

fn read_rows(root: &std::path::Path) -> Result<Vec<FindingRow>, AtlasError> {
    let path = root.join(FINDINGS_INDEX_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    let mut out = Vec::new();
    let mut first_malformed = None;
    for_each_index_line(&content, |line, parsed| match parsed {
        Ok(row) => out.push(row),
        Err(error) => {
            if first_malformed.is_none() {
                first_malformed = Some((line, error));
            }
        }
    });
    // Unchanged contract, unchanged message: one malformed line is a
    // hard error that blocks every ordinary read. Only the *recovery*
    // read above is allowed to see past it.
    if let Some((line, error)) = first_malformed {
        return Err(AtlasError::Catalog(format!(
            "findings.ndjson line {line} is malformed: {error}"
        )));
    }
    Ok(out)
}
