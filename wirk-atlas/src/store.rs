use crate::domain::{actual_line_bounds, now_unix_millis};
use crate::extract::ExtractorPolicy;
use crate::git;
use crate::{
    AcquisitionAttempt, AtlasError, CoverageDisposition, EstateScope, ExactCoordinate,
    FORMAT_VERSION, GenerationId, Membership, MembershipId, Relationship, ResolveOutcome,
    ResolvedEvidence, SourceGeneration, SourceId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use ulid::Ulid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    Staged(SourceGeneration),
    Unavailable(String),
}
impl AcquireOutcome {
    pub fn staged(self) -> Option<SourceGeneration> {
        match self {
            Self::Staged(g) => Some(g),
            Self::Unavailable(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Catalog {
    version: u32,
    estate: EstateScope,
    publication_revision: u64,
    memberships: BTreeMap<String, Membership>,
    published: BTreeMap<String, GenerationId>,
    attempts: Vec<AcquisitionAttempt>,
    /// P3 W4 A: membership id -> the semantic edition it currently
    /// selects. Additive and `default`ed, deliberately without a
    /// `FORMAT_VERSION` bump: a W3 catalog written before this field
    /// existed still opens, still passes every estate check, and still
    /// resolves every generation reference it already held — the
    /// migration the brief requires is "an old catalog keeps working",
    /// not "an old catalog is rewritten".
    #[serde(default)]
    semantic_selected: BTreeMap<String, crate::EditionId>,
}

/// A single-writer, estate-local catalog.  Opening cleans only abandoned
/// private temp siblings; published generation directories are never edited.
pub struct AtlasStore {
    root: PathBuf,
    catalog: Catalog,
}

impl AtlasStore {
    pub fn open(
        estate_root: impl AsRef<Path>,
        scope: impl Into<String>,
    ) -> Result<Self, AtlasError> {
        let root = estate_root.as_ref().join("atlas");
        fs::create_dir_all(root.join("generations"))?;
        // P3 W4 A: `atlas/semantic/` holds edition directories and is
        // staged through the same private-temporary discipline, so
        // reopening cleans its abandoned temporaries too — a build
        // interrupted before its rename leaves nothing behind.
        for directory in [root.clone(), root.join("semantic")] {
            if !directory.exists() {
                continue;
            }
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with(".tmp-") {
                    let kind = entry.file_type()?;
                    if kind.is_dir() {
                        fs::remove_dir_all(entry.path())?;
                    } else {
                        // A catalog write's temporary is a file. Removing
                        // the entry itself also avoids following a
                        // hostile symlink.
                        fs::remove_file(entry.path())?;
                    }
                }
            }
        }
        let catalog_path = root.join("catalog.json");
        let scope = EstateScope(scope.into());
        let catalog = if catalog_path.exists() {
            let found: Catalog = serde_json::from_slice(&fs::read(&catalog_path)?)?;
            if found.version != FORMAT_VERSION || found.estate != scope {
                return Err(AtlasError::Catalog(
                    "wrong format version or estate scope".into(),
                ));
            }
            // A successful reopen is also recovery: confirming the containing
            // directory makes a previously visible post-rename catalog durable.
            File::open(&root)?.sync_all()?;
            found
        } else {
            Catalog {
                version: FORMAT_VERSION,
                estate: scope,
                publication_revision: 0,
                memberships: BTreeMap::new(),
                published: BTreeMap::new(),
                attempts: vec![],
                semantic_selected: BTreeMap::new(),
            }
        };
        Ok(Self { root, catalog })
    }

    pub fn register_git(
        &mut self,
        alias: &str,
        locator: impl AsRef<Path>,
        requested_ref: &str,
    ) -> Result<Membership, AtlasError> {
        if alias.is_empty() || alias.contains('/') || alias.contains('\0') {
            return Err(AtlasError::InvalidCoordinate("invalid source alias".into()));
        }
        let locator = locator
            .as_ref()
            .canonicalize()?
            .to_string_lossy()
            .into_owned();
        if let Some(existing) = self.catalog.memberships.get(alias) {
            if existing.locator == locator {
                return Ok(existing.clone());
            }
            return Err(AtlasError::InvalidCoordinate(
                "alias already belongs to another source".into(),
            ));
        }
        let source = SourceId(Ulid::generate().to_string());
        let id = MembershipId(Self::hash(&[
            b"membership/v1",
            self.catalog.estate.0.as_bytes(),
            alias.as_bytes(),
            source.0.as_bytes(),
        ]));
        let membership = Membership {
            id,
            estate: self.catalog.estate.clone(),
            alias: alias.into(),
            source,
            locator,
            requested_ref: requested_ref.into(),
        };
        let mut next = self.catalog.clone();
        next.memberships.insert(alias.into(), membership.clone());
        self.commit_catalog(next)?;
        Ok(membership)
    }

    pub fn acquire(
        &mut self,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
    ) -> Result<AcquireOutcome, AtlasError> {
        self.check_membership(membership)?;
        let repo = Path::new(&membership.locator);
        let result = (|| {
            let (revision, content) = git::commit_and_tree(repo, requested_ref)?;
            let id = GenerationId(ExtractorPolicy::generation_id(
                &membership.source.0,
                &revision,
                &content,
                policy.id(),
            ));
            let destination = self.generation_dir(&id)?;
            let generation = if destination.exists() {
                self.read_generation(&id)?
            } else {
                let resources = git::resources(repo, &revision, &id, &policy)?;
                if let Some(unavailable) = resources
                    .iter()
                    .find(|resource| resource.disposition == CoverageDisposition::Unavailable)
                {
                    return Err(AtlasError::GitUnavailable(
                        unavailable
                            .detail
                            .clone()
                            .unwrap_or_else(|| "required Git object is unavailable".into()),
                    ));
                }
                let generation = SourceGeneration {
                    id: id.clone(),
                    source: membership.source.clone(),
                    revision,
                    content,
                    extractor_set: policy.id().into(),
                    acquisition_policy: "git-tree-policy/v1".into(),
                    locator: membership.locator.clone(),
                    requested_ref: requested_ref.into(),
                    resources,
                };
                self.stage(&generation)?;
                generation
            };
            Ok::<_, AtlasError>(generation)
        })();
        match result {
            Ok(generation) => {
                self.record_attempt(AcquisitionAttempt {
                    at_unix_millis: now_unix_millis(),
                    membership: membership.id.clone(),
                    requested_ref: requested_ref.into(),
                    outcome: "staged".into(),
                    generation: Some(generation.id.clone()),
                    diagnostic: None,
                })?;
                Ok(AcquireOutcome::Staged(generation))
            }
            Err(AtlasError::GitUnavailable(detail)) => {
                self.record_attempt(AcquisitionAttempt {
                    at_unix_millis: now_unix_millis(),
                    membership: membership.id.clone(),
                    requested_ref: requested_ref.into(),
                    outcome: "unavailable".into(),
                    generation: None,
                    diagnostic: Some(detail.clone()),
                })?;
                Ok(AcquireOutcome::Unavailable(detail))
            }
            Err(error) => {
                self.record_attempt(AcquisitionAttempt {
                    at_unix_millis: now_unix_millis(),
                    membership: membership.id.clone(),
                    requested_ref: requested_ref.into(),
                    outcome: "error".into(),
                    generation: None,
                    diagnostic: Some(error.to_string()),
                })?;
                Err(error)
            }
        }
    }

    /// Explicit refresh is acquisition only.  It never changes the current
    /// catalog snapshot; callers must separately publish the returned stage.
    pub fn refresh(
        &mut self,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
    ) -> Result<AcquireOutcome, AtlasError> {
        self.acquire(membership, requested_ref, policy)
    }

    pub fn publish(
        &mut self,
        membership: &Membership,
        generation: &GenerationId,
    ) -> Result<(), AtlasError> {
        self.check_membership(membership)?;
        let staged = self.read_generation(generation)?;
        if staged.source != membership.source
            || staged.locator != membership.locator
            || staged.resources.iter().any(|r| r.path.is_empty())
        {
            return Err(AtlasError::Generation(
                "generation does not belong to this membership or is incomplete".into(),
            ));
        }
        let (revision, content) =
            git::commit_and_tree(Path::new(&membership.locator), &staged.revision)?;
        if revision != staged.revision || content != staged.content {
            return Err(AtlasError::Generation(
                "generation commit or root tree is not present at its registered source".into(),
            ));
        }
        let tree_entries = git::tree_entries(Path::new(&membership.locator), &staged.revision)?;
        if tree_entries.len() != staged.resources.len()
            || tree_entries.iter().zip(&staged.resources).any(
                |((path, mode, object_id), resource)| {
                    path != &resource.path
                        || mode != &resource.mode
                        || resource.object_id.as_ref() != Some(object_id)
                },
            )
        {
            return Err(AtlasError::Generation(
                "generation resources do not exactly enumerate the committed tree".into(),
            ));
        }
        if self.catalog.published.get(&membership.id.0) == Some(generation) {
            return Ok(());
        }
        let mut next = self.catalog.clone();
        next.published
            .insert(membership.id.0.clone(), generation.clone());
        next.publication_revision += 1;
        self.commit_catalog(next)
    }
    pub fn current(&self, membership: &Membership) -> Result<Option<SourceGeneration>, AtlasError> {
        self.check_membership(membership)?;
        self.catalog
            .published
            .get(&membership.id.0)
            .map(|id| self.read_generation(id))
            .transpose()
    }
    pub fn resolve_exact(
        &self,
        membership: &Membership,
        coordinate: &ExactCoordinate,
    ) -> Result<ResolveOutcome, AtlasError> {
        self.check_membership(membership)?;
        if coordinate.estate != membership.estate
            || coordinate.membership != membership.id
            || coordinate.source != membership.source
        {
            return Err(AtlasError::InvalidCoordinate(
                "coordinate is outside this membership".into(),
            ));
        }
        if !valid_path(&coordinate.path) {
            return Err(AtlasError::InvalidCoordinate(
                "path is not repository-relative".into(),
            ));
        }
        let generation = self.read_generation(&coordinate.generation)?;
        if generation.source != membership.source || generation.locator != membership.locator {
            return Err(AtlasError::InvalidCoordinate(
                "generation is outside this source".into(),
            ));
        }
        let Some(record) = generation
            .resources
            .iter()
            .find(|r| r.path == coordinate.path)
        else {
            return Ok(ResolveOutcome::Absent);
        };
        let Some(record_object_id) = &record.object_id else {
            return Ok(ResolveOutcome::Unavailable(
                "resource has no Git object identity".into(),
            ));
        };
        if coordinate.object_id != *record_object_id {
            return Err(AtlasError::InvalidCoordinate(
                "coordinate blob does not match generation resource".into(),
            ));
        }
        match record.disposition {
            CoverageDisposition::Excluded
            | CoverageDisposition::Unsupported
            | CoverageDisposition::Unavailable
            | CoverageDisposition::Error => {
                if coordinate.byte_start != 0
                    || coordinate.byte_end != 0
                    || coordinate.line_start != 0
                    || coordinate.line_end != 0
                {
                    return Err(AtlasError::InvalidCoordinate(
                        "non-indexed resource has no text coordinate".into(),
                    ));
                }
                let detail = record
                    .detail
                    .clone()
                    .unwrap_or_else(|| "resource unavailable".into());
                return Ok(match record.disposition {
                    CoverageDisposition::Excluded => ResolveOutcome::Excluded(detail),
                    CoverageDisposition::Unsupported => ResolveOutcome::Unsupported(detail),
                    CoverageDisposition::Unavailable | CoverageDisposition::Error => {
                        ResolveOutcome::Unavailable(detail)
                    }
                    CoverageDisposition::Indexed => unreachable!(),
                });
            }
            CoverageDisposition::Indexed => {}
        }
        let repo = Path::new(&membership.locator);
        let (_, content) = match git::commit_and_tree(repo, &generation.revision) {
            Ok(identity) => identity,
            Err(AtlasError::GitUnavailable(detail)) => {
                return Ok(ResolveOutcome::Unavailable(detail));
            }
            Err(error) => return Err(error),
        };
        if content != generation.content {
            return Ok(ResolveOutcome::Unavailable(
                "committed root tree no longer matches generation provenance".into(),
            ));
        }
        match git::blob_at_path(repo, &generation.revision, &coordinate.path) {
            Ok(Some(oid)) if oid == coordinate.object_id => {}
            Ok(_) => {
                return Ok(ResolveOutcome::Unavailable(
                    "committed path no longer names the recorded blob".into(),
                ));
            }
            Err(AtlasError::GitUnavailable(detail)) => {
                return Ok(ResolveOutcome::Unavailable(detail));
            }
            Err(error) => return Err(error),
        }
        match git::blob(repo, &coordinate.object_id) {
            Ok(bytes) => {
                let Some((line_start, line_end)) =
                    actual_line_bounds(&bytes, coordinate.byte_start, coordinate.byte_end)
                else {
                    return Err(AtlasError::InvalidCoordinate(
                        "byte bounds are invalid or split committed UTF-8".into(),
                    ));
                };
                if line_start != coordinate.line_start || line_end != coordinate.line_end {
                    return Err(AtlasError::InvalidCoordinate(
                        "line bounds do not match committed Git bytes".into(),
                    ));
                }
                Ok(ResolveOutcome::Resolved(ResolvedEvidence {
                    coordinate: coordinate.clone(),
                    bytes: bytes[coordinate.byte_start as usize..coordinate.byte_end as usize]
                        .to_vec(),
                }))
            }
            Err(AtlasError::GitUnavailable(detail)) => Ok(ResolveOutcome::Unavailable(detail)),
            Err(e) => Err(e),
        }
    }
    fn stage(&self, generation: &SourceGeneration) -> Result<(), AtlasError> {
        let temp = self.root.join(format!(".tmp-{}", Ulid::generate()));
        fs::create_dir(&temp)?;
        self.write_sync(
            &temp.join("manifest.json"),
            &serde_json::to_vec_pretty(generation)?,
        )?;
        checkpoint("stage-manifest-synced");
        let mut resources = Vec::new();
        for resource in &generation.resources {
            resources.extend_from_slice(&serde_json::to_vec(resource)?);
            resources.push(b'\n');
        }
        self.write_sync(&temp.join("resources.ndjson"), &resources)?;
        File::open(&temp)?.sync_all()?;
        checkpoint("stage-directory-synced");
        fs::rename(&temp, self.generation_dir(&generation.id)?)?;
        File::open(self.root.join("generations"))?.sync_all()?;
        Ok(())
    }
    fn persist_catalog(&self, catalog: &Catalog) -> Result<(), AtlasError> {
        let temporary = self.root.join(format!(".tmp-catalog-{}", Ulid::generate()));
        self.write_sync(&temporary, &serde_json::to_vec_pretty(catalog)?)?;
        checkpoint("catalog-file-synced");
        fs::rename(temporary, self.root.join("catalog.json"))?;
        checkpoint("catalog-renamed");
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                AtlasError::DurabilityUncertain(format!(
                    "renamed catalog revision {} is visible; directory sync failed: {error}",
                    catalog.publication_revision
                ))
            })?;
        Ok(())
    }

    fn commit_catalog(&mut self, next: Catalog) -> Result<(), AtlasError> {
        match self.persist_catalog(&next) {
            Ok(()) => {
                self.catalog = next;
                Ok(())
            }
            Err(error @ AtlasError::DurabilityUncertain(_)) => {
                // Rename already made these bytes authoritative for this live
                // handle. Preserve that visible truth while reporting that
                // directory durability still needs recovery confirmation.
                self.catalog = next;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }
    /// W-B (`findings.rs`): the estate root, so a second append-only log
    /// beside `relationships.ndjson` can reuse this store's own
    /// temp-file/fsync/rename/directory-fsync discipline verbatim rather
    /// than inventing a second durability protocol (`append_relationship`'s
    /// own doc: "not a second ad hoc durability protocol").
    pub(crate) fn root_path(&self) -> &Path {
        &self.root
    }
    pub(crate) fn write_sync(&self, path: &Path, bytes: &[u8]) -> Result<(), AtlasError> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
    fn generation_dir(&self, id: &GenerationId) -> Result<PathBuf, AtlasError> {
        if !valid_generation_id(&id.0) {
            return Err(AtlasError::Generation(
                "invalid generation identifier".into(),
            ));
        }
        Ok(self.root.join("generations").join(&id.0))
    }
    /// Reads an immutable generation by its content-addressed id, whether
    /// or not it is any membership's *current* published one — the
    /// mechanism continuation (W3-CORRECTION.md item 1) relies on to pin
    /// an answer to the exact generations it began with, immune to a
    /// later `refresh`/`publish` on the same estate.
    pub fn generation(&self, id: &GenerationId) -> Result<SourceGeneration, AtlasError> {
        self.read_generation(id)
    }
    fn read_generation(&self, id: &GenerationId) -> Result<SourceGeneration, AtlasError> {
        let generation_dir = self.generation_dir(id)?;
        let path = generation_dir.join("manifest.json");
        if !path.exists() {
            return Err(AtlasError::Generation(id.0.clone()));
        }
        let g: SourceGeneration = serde_json::from_slice(&fs::read(path)?)?;
        if g.id != *id {
            return Err(AtlasError::Generation("forged manifest identifier".into()));
        }
        self.validate_generation(&g, id)?;
        let resource_path = generation_dir.join("resources.ndjson");
        let serialized = fs::read_to_string(resource_path)
            .map_err(|_| AtlasError::Generation("missing immutable resource manifest".into()))?;
        let rows: Result<Vec<crate::ResourceRecord>, _> =
            serialized.lines().map(serde_json::from_str).collect();
        if rows.map_err(AtlasError::Json)? != g.resources {
            return Err(AtlasError::Generation(
                "resource manifest is inconsistent with generation".into(),
            ));
        }
        Ok(g)
    }
    fn validate_generation(
        &self,
        generation: &SourceGeneration,
        id: &GenerationId,
    ) -> Result<(), AtlasError> {
        if generation.id != *id
            || !valid_generation_id(&generation.id.0)
            || generation.source.0.is_empty()
            || !valid_sha1(&generation.revision)
            || !generation.content.starts_with("sha1:")
            || !valid_sha1(&generation.content[5..])
            || generation.extractor_set.is_empty()
            || generation.acquisition_policy != "git-tree-policy/v1"
            || generation.id.0
                != ExtractorPolicy::generation_id(
                    &generation.source.0,
                    &generation.revision,
                    &generation.content,
                    &generation.extractor_set,
                )
        {
            return Err(AtlasError::Generation(
                "forged or inconsistent generation identity".into(),
            ));
        }
        // P3 W3 extractor completion: families are re-derived under the
        // edition this generation itself names, never under today's
        // default — a `v2` generation staged before the vocabulary
        // widened must still validate exactly as it did when written.
        // An `extractor_set` naming no known edition is refused outright
        // rather than validated under whatever happens to be current.
        let Some(edition) = crate::extract::ExtractorEdition::from_id(&generation.extractor_set)
        else {
            return Err(AtlasError::Generation(
                "generation names an unknown extraction edition".into(),
            ));
        };
        let mut previous: Option<&[u8]> = None;
        for resource in &generation.resources {
            if !valid_path(&resource.path)
                || previous.is_some_and(|p| p >= resource.path.as_slice())
            {
                return Err(AtlasError::Generation(
                    "resources are not uniquely sorted complete rows".into(),
                ));
            }
            if resource.disposition == CoverageDisposition::Indexed && resource.units.is_empty() {
                return Err(AtlasError::Generation(
                    "indexed resource lacks extracted units".into(),
                ));
            }
            if resource.disposition == CoverageDisposition::Indexed {
                let Some(object_id) = resource.object_id.as_deref() else {
                    return Err(AtlasError::Generation(
                        "indexed resource lacks a blob identity".into(),
                    ));
                };
                let mut previous_end = 0;
                for unit in &resource.units {
                    if unit.unitizer != "utf8-line-chunks-65536/v1"
                        || Some(unit.family) != edition.family(&resource.path)
                        || unit.id
                            != ExtractorPolicy::unit_id(
                                &generation.id,
                                &resource.path,
                                object_id,
                                unit.family,
                                unit.byte_start,
                                unit.byte_end,
                            )
                        || unit.byte_start != previous_end
                        || unit.byte_end < unit.byte_start
                        || unit.byte_end - unit.byte_start > 64 * 1024
                        || unit.line_start == 0
                        || unit.line_start != unit.line_end
                    {
                        return Err(AtlasError::Generation(
                            "derived retrieval unit identity or bounds are inconsistent".into(),
                        ));
                    }
                    previous_end = unit.byte_end;
                }
                if resource.byte_len != Some(previous_end) {
                    return Err(AtlasError::Generation(
                        "derived retrieval units do not cover the complete blob".into(),
                    ));
                }
            }
            previous = Some(&resource.path);
        }
        Ok(())
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// `check_membership` under a name `semantic.rs` can call across the
    /// module boundary; the rule itself is unchanged.
    pub(crate) fn check_membership_public(&self, member: &Membership) -> Result<(), AtlasError> {
        self.check_membership(member)
    }

    /// The edition this membership currently selects, or `None`.
    /// Reads the same private in-memory catalog snapshot every other
    /// query reads, so a selection is visible to this handle exactly when
    /// its catalog write committed.
    pub fn selected_semantic(&self, membership: &Membership) -> Option<crate::EditionId> {
        self.catalog
            .semantic_selected
            .get(&membership.id.0)
            .cloned()
    }

    /// One atomic catalog advance, through the same
    /// temp/fsync/rename/directory-fsync discipline every other
    /// publication uses. `publication_revision` advances with it: a
    /// semantic selection changes what the estate publicly asserts.
    pub(crate) fn commit_semantic_selection(
        &mut self,
        membership: &Membership,
        edition: crate::EditionId,
    ) -> Result<(), AtlasError> {
        if self.catalog.semantic_selected.get(&membership.id.0) == Some(&edition) {
            return Ok(());
        }
        let mut next = self.catalog.clone();
        next.semantic_selected
            .insert(membership.id.0.clone(), edition);
        next.publication_revision += 1;
        self.commit_catalog(next)
    }

    pub fn attempts(&self) -> &[AcquisitionAttempt] {
        &self.catalog.attempts
    }
    /// A query's coherence does not come from this borrow (a second,
    /// independent `AtlasStore` handle on the same estate root can publish
    /// concurrently and is invisible to it — source-verify-w2 `probe_b1`).
    /// It comes from `self.catalog` being a private in-memory snapshot that
    /// only `&mut self` calls on *this instance* change; every membership
    /// yielded here, and every generation a caller resolves from it, reads
    /// that one already-resident snapshot for the duration of the call.
    pub fn memberships(&self) -> impl Iterator<Item = &Membership> {
        self.catalog.memberships.values()
    }
    pub fn publication_revision(&self) -> u64 {
        self.catalog.publication_revision
    }
    pub fn relationships(&self) -> Result<Vec<Relationship>, AtlasError> {
        let path = self.root.join("relationships.ndjson");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path)?;
        let mut out = Vec::new();
        for (index, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let relationship: Relationship = serde_json::from_str(line).map_err(|error| {
                AtlasError::Catalog(format!(
                    "relationships.ndjson line {} is malformed: {error}",
                    index + 1
                ))
            })?;
            out.push(relationship);
        }
        Ok(out)
    }
    /// Rewrites the whole append-only log through the same
    /// temp-file/fsync/rename/directory-fsync discipline `persist_catalog`
    /// uses, including its post-rename failure reporting (below) — not a
    /// second ad hoc durability protocol; P3's relationship count is small
    /// enough that whole-file rewrite is the R1/R3 minimum. Retrying an
    /// identical relationship (matched by content-addressed `RelationshipId`
    /// above) is always safe: it is a no-op once the row is visible, whether
    /// or not the caller received `DurabilityUncertain` for the write that
    /// made it so.
    pub fn append_relationship(&mut self, relationship: &Relationship) -> Result<(), AtlasError> {
        let mut existing = self.relationships()?;
        if existing.iter().any(|r| r.id == relationship.id) {
            return Ok(());
        }
        existing.push(relationship.clone());
        let mut bytes = Vec::new();
        for r in &existing {
            bytes.extend_from_slice(&serde_json::to_vec(r)?);
            bytes.push(b'\n');
        }
        let temp = self
            .root
            .join(format!(".tmp-relationships-{}", Ulid::generate()));
        self.write_sync(&temp, &bytes)?;
        fs::rename(&temp, self.root.join("relationships.ndjson"))?;
        // The rename above already made this relationship visible to any
        // fresh reader; a failure syncing the containing directory after
        // that point is reported as visible-but-unconfirmed, exactly as
        // `persist_catalog` reports the identical window for the catalog —
        // never as a bare I/O error indistinguishable from "never wrote".
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                AtlasError::DurabilityUncertain(format!(
                    "relationship {} is visible; directory sync failed: {error}",
                    relationship.id.0
                ))
            })?;
        Ok(())
    }
    fn record_attempt(&mut self, attempt: AcquisitionAttempt) -> Result<(), AtlasError> {
        let mut next = self.catalog.clone();
        next.attempts.push(attempt);
        self.commit_catalog(next)
    }
    fn check_membership(&self, member: &Membership) -> Result<(), AtlasError> {
        match self.catalog.memberships.get(&member.alias) {
            Some(stored) if stored == member && stored.estate == self.catalog.estate => Ok(()),
            _ => Err(AtlasError::InvalidCoordinate(
                "membership is not in this estate catalog".into(),
            )),
        }
    }
    fn hash(parts: &[&[u8]]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for p in parts {
            h.update((p.len() as u64).to_be_bytes());
            h.update(p);
        }
        let bytes = h.finalize();
        format!(
            "m-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    }
}

fn valid_generation_id(id: &str) -> bool {
    id.len() == 66
        && id.starts_with("g-")
        && id[2..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn valid_sha1(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn valid_path(path: &[u8]) -> bool {
    !path.is_empty()
        && !path.starts_with(b"/")
        && !path
            .split(|byte| *byte == b'/')
            .any(|part| part.is_empty() || part == b".." || part == b".")
}

/// Deliberately process-level so a verifier can exercise real crash windows
/// from a child process, rather than substituting a fake store failure.
/// `pub(crate)` (W-B `findings.rs`, W4-A `semantic.rs`): the identical
/// failpoint mechanism, reused rather than a second one invented for a
/// second append-only log or for the semantic edition writer
/// (`WIRK_ATLAS_FAILPOINT`'s own env var, one gate for the whole crate).
pub(crate) fn checkpoint(name: &str) {
    if std::env::var("WIRK_ATLAS_FAILPOINT").ok().as_deref() == Some(name) {
        std::process::exit(86);
    }
}
