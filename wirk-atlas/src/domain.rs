use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EstateScope(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MembershipId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GenerationId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnitId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentFamily {
    Code,
    Knowledge,
    /// P3 W3 extractor completion: the third family the installed
    /// `semble` reference distinguishes (`ContentType::CONFIG`) —
    /// manifests, lockfiles, schemas, CI and editor configuration. Only
    /// extraction edition `ContentFamiliesV3` ever produces it; a `v2`
    /// generation cannot contain one, which is why adding it does not
    /// disturb any generation already staged.
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    pub id: MembershipId,
    pub estate: EstateScope,
    pub alias: String,
    pub source: SourceId,
    pub locator: String,
    pub requested_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageDisposition {
    Indexed,
    Excluded,
    Unsupported,
    Unavailable,
    Error,
}
impl CoverageDisposition {
    pub fn is_excluded(self) -> bool {
        self == Self::Excluded
    }
    pub fn is_unsupported(self) -> bool {
        self == Self::Unsupported
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRecord {
    /// Raw Git pathname bytes.  JSON's array encoding is intentional: paths
    /// are not OS strings and never pass through line-oriented parsing.
    pub path: Vec<u8>,
    pub mode: String,
    pub object_id: Option<String>,
    pub byte_len: Option<u64>,
    pub disposition: CoverageDisposition,
    pub detail: Option<String>,
    #[serde(default)]
    pub units: Vec<TextUnit>,
}

/// A bounded UTF-8 text unit. Byte offsets are half-open and line numbers are
/// one-based, inclusive. They address the committed Git blob, never a checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextUnit {
    pub id: UnitId,
    pub family: ContentFamily,
    pub unitizer: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactCoordinate {
    pub estate: EstateScope,
    pub membership: MembershipId,
    pub source: SourceId,
    pub generation: GenerationId,
    pub path: Vec<u8>,
    pub object_id: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedEvidence {
    pub coordinate: ExactCoordinate,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceGeneration {
    pub id: GenerationId,
    pub source: SourceId,
    pub revision: String,
    pub content: String,
    pub extractor_set: String,
    pub acquisition_policy: String,
    pub locator: String,
    pub requested_ref: String,
    pub resources: Vec<ResourceRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcquisitionAttempt {
    pub at_unix_millis: u128,
    pub membership: MembershipId,
    pub requested_ref: String,
    pub outcome: String,
    pub generation: Option<GenerationId>,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    Resolved(ResolvedEvidence),
    Absent,
    Excluded(String),
    Unsupported(String),
    Unavailable(String),
}

#[derive(Debug, Error)]
pub enum AtlasError {
    #[error("Git object unavailable: {0}")]
    GitUnavailable(String),
    #[error("inconsistent or out-of-scope coordinate: {0}")]
    InvalidCoordinate(String),
    #[error("catalog is malformed or has an unsupported version: {0}")]
    Catalog(String),
    #[error("generation is incomplete or absent: {0}")]
    Generation(String),
    /// A *semantic edition* record, which is not a generation. Split out
    /// because the reason text is read by a human deciding what is wrong
    /// with their estate, and `Generation` made a removed `edition.json`
    /// report a missing source generation — a different condition with a
    /// different repair (`W4-PRODUCER-PROVENANCE-CORRECTION.md` item 4).
    #[error("semantic edition record is absent or unreadable: {0}")]
    Edition(String),
    #[error("catalog is visible but its directory entry durability is uncertain: {0}")]
    DurabilityUncertain(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationshipId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    GovernedBy,
}

/// The only relationship producer this increment admits: a caller-supplied
/// admission whose endpoints and evidence all independently resolve. A
/// lexical mention is never sufficient; see `relationship.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relationship {
    pub id: RelationshipId,
    pub kind: RelationshipKind,
    pub from: ExactCoordinate,
    pub to: ExactCoordinate,
    pub evidence: Vec<ExactCoordinate>,
    pub producer: String,
    pub published_at_unix_millis: u128,
}

impl RelationshipId {
    pub(crate) fn compute(
        kind: RelationshipKind,
        from: &ExactCoordinate,
        to: &ExactCoordinate,
        evidence: &[ExactCoordinate],
        producer: &str,
    ) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        absorb(
            &mut hasher,
            match kind {
                RelationshipKind::GovernedBy => b"governed_by",
            },
        );
        absorb(&mut hasher, &encode_coordinate(from));
        absorb(&mut hasher, &encode_coordinate(to));
        absorb(&mut hasher, &(evidence.len() as u64).to_be_bytes());
        for coordinate in evidence {
            absorb(&mut hasher, &encode_coordinate(coordinate));
        }
        absorb(&mut hasher, producer.as_bytes());
        Self(format!("r-{}", hex(&hasher.finalize())))
    }
}

fn absorb(hasher: &mut sha2::Sha256, part: &[u8]) {
    use sha2::Digest;
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}

fn encode_coordinate(coordinate: &ExactCoordinate) -> Vec<u8> {
    let mut out = Vec::new();
    for part in [
        coordinate.estate.0.as_bytes(),
        coordinate.membership.0.as_bytes(),
        coordinate.source.0.as_bytes(),
        coordinate.generation.0.as_bytes(),
        coordinate.path.as_slice(),
        coordinate.object_id.as_bytes(),
    ] {
        out.extend_from_slice(&(part.len() as u64).to_be_bytes());
        out.extend_from_slice(part);
    }
    for n in [
        coordinate.byte_start,
        coordinate.byte_end,
        coordinate.line_start,
        coordinate.line_end,
    ] {
        out.extend_from_slice(&n.to_be_bytes());
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Shared by exact resolution (`store.rs`) and the W2 path-lookup query
/// (`query.rs`): committed byte bounds map to one-based inclusive lines.
pub(crate) fn actual_line_bounds(bytes: &[u8], start: u64, end: u64) -> Option<(u64, u64)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    if start > end
        || end > bytes.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    if bytes.is_empty() && start == 0 && end == 0 {
        return Some((1, 1));
    }
    if start == end {
        return None;
    }
    let line_start = 1 + bytes[..start].iter().filter(|byte| **byte == b'\n').count() as u64;
    let line_end = 1 + bytes[..end - 1]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u64;
    Some((line_start, line_end))
}

pub(crate) fn now_unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
