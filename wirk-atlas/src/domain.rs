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
    /// P5.2 (ruling 0293/0264/0313): a document admitted through the
    /// whole native `anydoc` 0.2.4 reader (`crate::document`) —
    /// Word/PowerPoint/Excel (legacy and current), OpenDocument text/
    /// sheet/presentation, RTF, EPUB, CSV and PDF, every extension
    /// `anydoc` itself recognizes, not only the four examples the P5.2
    /// brief originally named. Only extraction edition
    /// `DocumentsAnyDocV5` ever produces it.
    ///
    /// **A `Document` unit's `byte_start`/`byte_end` index the
    /// document's normalized Markdown rendering, never the original
    /// file's bytes.** There is no worksheet name, page number or A1-style
    /// cell address here — `anydoc`'s own tables carry no such origin
    /// (`knowledge/evidence/p5-document-reader-reuse-2026-09-13.md`), so
    /// none is claimed. Every caller that turns this family's coordinate
    /// back into bytes (`crate::hydrate`, `AtlasStore::resolve_exact_*`)
    /// must route through `crate::document::render_if_document` first;
    /// slicing the original binary at these offsets is simply wrong, not
    /// merely imprecise.
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    pub id: MembershipId,
    pub estate: EstateScope,
    pub alias: String,
    pub source: SourceId,
    pub locator: String,
    pub requested_ref: String,
    /// Which acquisition policy this source was **explicitly** admitted
    /// under — `crate::git::ACQUISITION_POLICY` for a Git repository,
    /// subdirectory or worktree, `crate::doctree::ACQUISITION_POLICY`
    /// for a local non-Git document collection.
    ///
    /// Decided once, at registration (`AtlasStore::register_git`/
    /// `register_document_tree`), and reused by every later `refresh` of
    /// the same alias. What kind of thing someone pointed wirk at is
    /// their choice, made exactly once and never re-inferred from the
    /// path — in particular never from the presence or absence of a
    /// `.git` entry, which says where a directory happens to sit and
    /// nothing about how its owner meant it to be read.
    ///
    /// `#[serde(default)]` to the Git label: a catalog written before
    /// this field existed named only Git sources, so an old membership
    /// keeps meaning exactly what it always meant.
    #[serde(default = "Membership::default_policy")]
    pub policy: String,
}

impl Membership {
    fn default_policy() -> String {
        crate::git::ACQUISITION_POLICY.to_string()
    }
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

/// The integrity record for one generation's `resources.ndjson`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRows {
    /// SHA-256, domain-separated, over the file's bytes in written
    /// order.
    pub digest: String,
    pub count: u64,
}

/// How many of one generation's resources landed in each coverage
/// disposition.
///
/// This is what an acquisition can report about itself without holding
/// the whole collection's resource list in memory: the sink that writes
/// `resources.ndjson` counts each record as it streams past, and the
/// counts are all any caller of `acquire`/`refresh` has ever needed.
/// The records themselves stay where they were written — the immutable
/// generation directory — and a caller that wants them reads that
/// generation back ([`crate::AtlasStore::generation`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub total: u64,
    pub indexed: u64,
    pub excluded: u64,
    pub unsupported: u64,
    pub unavailable: u64,
    pub error: u64,
}

impl CoverageSummary {
    pub fn count(&mut self, disposition: CoverageDisposition) {
        self.total += 1;
        let field = match disposition {
            CoverageDisposition::Indexed => &mut self.indexed,
            CoverageDisposition::Excluded => &mut self.excluded,
            CoverageDisposition::Unsupported => &mut self.unsupported,
            CoverageDisposition::Unavailable => &mut self.unavailable,
            CoverageDisposition::Error => &mut self.error,
        };
        *field += 1;
    }

    pub fn of(resources: &[ResourceRecord]) -> Self {
        let mut summary = Self::default();
        for resource in resources {
            summary.count(resource.disposition);
        }
        summary
    }
}

/// What one `acquire`/`refresh` reports about the generation it staged:
/// the generation's identity, its origin disclosure where it has one,
/// and its [`CoverageSummary`] — deliberately **not** its resource
/// list, and not its locator or requested ref either — those are facts
/// about the membership the caller passed in, restating them here only
/// grew the value every acquisition returns.
///
/// Rulings 0401/0403: an acquisition that builds the whole collection's
/// `ResourceRecord`s in memory just to hand them back is retaining the
/// derived index of the entire collection for the length of the
/// acquisition, on top of the document it is extracting. Streaming each
/// record into the generation directory as it is produced is what makes
/// residency a function of one document rather than of the collection,
/// and this type is the shape of the answer that is left. A caller that
/// genuinely wants the records reads the immutable generation back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedGeneration {
    pub id: GenerationId,
    pub source: SourceId,
    pub revision: String,
    pub content: String,
    pub extractor_set: String,
    pub acquisition_policy: String,
    pub origin: Option<Box<HttpOrigin>>,
    pub coverage: CoverageSummary,
}

impl StagedGeneration {
    /// The report for a generation that was already staged by an earlier
    /// acquisition and has just been read back, rather than written now.
    pub fn read_back(generation: &SourceGeneration) -> Self {
        Self {
            id: generation.id.clone(),
            source: generation.source.clone(),
            revision: generation.revision.clone(),
            content: generation.content.clone(),
            extractor_set: generation.extractor_set.clone(),
            acquisition_policy: generation.acquisition_policy.clone(),
            origin: generation.origin.clone(),
            coverage: CoverageSummary::of(&generation.resources),
        }
    }
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
    /// The generation's complete resource list, **read back** from its
    /// own `resources.ndjson` and never written into `manifest.json`.
    ///
    /// `skip_serializing` is the whole point: the rows are already
    /// written, one at a time, by the sink that produced them
    /// (`AtlasStore`'s staging session), so serializing them a second
    /// time inside the manifest would mean holding the entire
    /// collection's derived index in memory at publication —
    /// precisely the retention rulings 0401/0403 asked to end. A
    /// manifest written before this change still carries the field and
    /// still deserializes; `read_generation` replaces it with the rows
    /// either way, so the two sources can never disagree.
    #[serde(default, skip_serializing)]
    pub resources: Vec<ResourceRecord>,
    /// The SHA-256 of `resources.ndjson`'s bytes, folded as they were
    /// written, and the number of rows that went past.
    ///
    /// This is what replaced the manifest's duplicate resource array as
    /// the generation's *row-set* integrity. Validating each row's
    /// shape, identity and sort order says nothing about whether the set
    /// is the one that was staged: a whole line removed from the file
    /// leaves the survivors individually valid and uniquely sorted, so
    /// without this a truncated or hand-edited `resources.ndjson` would
    /// read back as a smaller generation with nothing marking it
    /// incomplete. Thirty-two bytes and a count, folded in the same pass
    /// that produced the rows — no second copy of the collection, no
    /// index and no archive.
    ///
    /// `#[serde(default)]` so a manifest written before this field
    /// deserializes; such a generation still carries its own resource
    /// array, and `read_generation` checks it against that instead.
    #[serde(default)]
    pub rows: Option<ResourceRows>,
    /// What one `http-source-policy/v1` fetch actually observed about
    /// its origin — set only by `crate::http_source::capture`, `None`
    /// for every other policy. `#[serde(default)]`: a generation
    /// written before this field existed still deserializes. Boxed so
    /// this rarely-populated field does not grow every generation
    /// (including every Git/document-tree one, which never sets it) by
    /// `HttpOrigin`'s own size, and so `AcquireOutcome::Staged`'s
    /// `SourceGeneration` payload does not tower over its
    /// `Unavailable(String)` sibling.
    #[serde(default)]
    pub origin: Option<Box<HttpOrigin>>,
}

/// What one HTTP acquisition/refresh actually observed about the
/// response it read, disclosed for a caller to read — never the
/// identity a coordinate resolves against (`SourceGeneration::revision`/
/// `content`, the SHA-256 of the response bytes, are that) and never
/// presented as an upstream publication or revision date.
/// `fetched_at_unix_millis` is this process's own clock at the moment
/// the fetch completed, not a claim about when the origin last changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpOrigin {
    /// The URL this fetch was explicitly asked to acquire —
    /// `Membership::locator` for this source, restated here so a
    /// generation is self-describing without a membership lookup.
    pub requested_url: String,
    /// The URL the response actually came from, after following
    /// redirects (curl's own `%{url_effective}`). Equal to
    /// `requested_url` when nothing redirected.
    pub final_url: String,
    /// The final HTTP status code (after redirects).
    pub status: u16,
    /// The origin's own `ETag`, verbatim, when it disclosed one.
    pub etag: Option<String>,
    /// The origin's own `Last-Modified`, verbatim, when it disclosed
    /// one. A caller's own timeline, never this product's.
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    /// This process's own clock at the moment the fetch completed.
    pub fetched_at_unix_millis: u128,
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
    /// The bytes a coordinate or resource names could not be read from
    /// the source that holds them.
    ///
    /// Deliberately source-neutral: a Git source reaches it when the
    /// object store no longer holds the object, and a local document
    /// collection reaches it when the file has changed, vanished, or is
    /// no longer an ordinary readable file. It is a disclosure, not a
    /// failure — the caller reports the resource unavailable rather
    /// than returning different bytes under the same coordinate.
    #[error("source bytes unavailable: {0}")]
    SourceBytesUnavailable(String),
    #[error("inconsistent or out-of-scope coordinate: {0}")]
    InvalidCoordinate(String),
    /// The request names something this product cannot run as asked — a
    /// result capacity outside the policy's bounds, say. Split out from
    /// `InvalidCoordinate` because nothing is wrong with any coordinate,
    /// and a caller reading the reason is deciding what to change about
    /// their own request, not about their estate.
    #[error("request cannot be run as asked: {0}")]
    InvalidRequest(String),
    /// This estate's own `.wirk/resources.json` exists and cannot be
    /// used, so the bounds it configures are not in force. Ruling 0402:
    /// a configuration failure that only printed a warning let an estate
    /// run every operation that file was supposed to bound. Opening the
    /// store refuses instead, and this is what it refuses with.
    #[error("estate resource policy is unusable: {0}")]
    UnusablePolicy(String),
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
    /// P4.5 B1 (ruling 0237): another live `AtlasStore` already owns this
    /// estate's `atlas/`. Before this existed, a second opener swept
    /// `.tmp-*` directories it merely assumed were abandoned and
    /// destroyed a concurrent build's live staging mid-write. The
    /// ownership claim is an `flock` held for the store's whole life, so
    /// "abandoned" is now true by enforcement rather than by distance.
    #[error("atlas store is already owned by a live holder: {0}")]
    StoreInUse(String),
    /// An operator cancelled this job, or it reached a checkpoint after
    /// its deadline had passed.
    ///
    /// Its own variant because it is neither a failure of the work nor a
    /// bad request: nothing about the estate or the collection is wrong,
    /// and the same verb run again will do the same thing. Whatever was
    /// already published stays published — a cancelled acquisition
    /// stages nothing and a cancelled publish advances nothing.
    #[error("job stopped: {0}")]
    Cancelled(String),
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
/// P3 W-C1: `pub` so the one product caller that builds an
/// `ExactCoordinate` outside this crate — `wirkd`'s stage-projection
/// assembler, resolving a path against an already-captured
/// `SourceGeneration` rather than re-reading the catalog — computes the
/// *same* line bounds `AtlasStore::resolve_exact` will later validate
/// against the committed bytes. R2: one implementation of the
/// arithmetic, not a second that can drift out of agreement with the
/// resolver that checks it.
pub fn actual_line_bounds(bytes: &[u8], start: u64, end: u64) -> Option<(u64, u64)> {
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
