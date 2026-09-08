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
    pub fn append_finding_row(&mut self, row: &FindingRow) -> Result<(), AtlasError> {
        let mut existing = self.findings()?;
        if existing.iter().any(|r| r.id == row.id) {
            return Ok(());
        }
        existing.push(row.clone());
        self.rewrite_rows(&existing)
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

fn read_rows(root: &std::path::Path) -> Result<Vec<FindingRow>, AtlasError> {
    let path = root.join("findings.ndjson");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: FindingRow = serde_json::from_str(line).map_err(|error| {
            AtlasError::Catalog(format!(
                "findings.ndjson line {} is malformed: {error}",
                index + 1
            ))
        })?;
        out.push(row);
    }
    Ok(out)
}
