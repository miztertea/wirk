use crate::doctree;
use crate::document;
use crate::domain::{actual_line_bounds, now_unix_millis};
use crate::extract::ExtractorPolicy;
use crate::git;
use crate::http_source;
use crate::{
    AcquisitionAttempt, AtlasError, ContentFamily, CoverageDisposition, EstateScope,
    ExactCoordinate, FORMAT_VERSION, GenerationId, Membership, MembershipId, Relationship,
    ResolveOutcome, ResolvedEvidence, SourceGeneration, SourceId,
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

/// The result of `AtlasStore::remove_source`: a **catalog-only
/// unregister**.
///
/// It never touches the source's original files, and it does not sweep
/// this estate's own generation or edition directories either. Byte
/// removal has one owner — `wirk estate clean --class
/// atlas-generations|atlas-editions --all-unreferenced` — which
/// re-derives what every *other* membership still needs before removing
/// anything. A sweep inside this crate could not see a non-terminal
/// Work's delivered World or an unsettled finding, because both live in
/// `wirkd`'s own journals, so it would have no way to avoid deleting
/// bytes that facility would refuse to touch.
///
/// This struct reports what this membership's catalog entry pointed at
/// *before* the unregister, so a caller can name it to that cleanup
/// facility without a second read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalOutcome {
    pub membership: MembershipId,
    /// This membership's published generation immediately before
    /// removal, if it had one. Now unreferenced by this estate's
    /// catalog (nothing else can name it through this membership any
    /// more) — not yet removed from disk.
    pub released_generation: Option<GenerationId>,
    /// This membership's selected semantic edition immediately before
    /// removal, if any. Also now unreferenced, also not yet removed.
    pub released_edition: Option<crate::EditionId>,
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

/// A single-writer, estate-local catalog.
///
/// P4.5 B1 (ruling 0237): "single-writer" is now **enforced**, not merely
/// true by distance. Opening takes an exclusive `flock` on `atlas/.owner`
/// and holds it for the store's whole life, and it does so *before*
/// sweeping private temporaries — so a temporary this store removes is
/// abandoned because no live owner holds it, not because the doc assumed
/// so. Measured red at `bf16369`: a second `open` during a live
/// `build_semantic` removed that build's `.tmp-<ULID>` staging directory
/// mid-write and the build lost its work with `ENOENT`.
///
/// The lock is advisory, per open-file-description, and unreliable on
/// some network filesystems; the kernel releases it when the holder dies,
/// which is why no stale-lock reaper exists and why the pid recorded in
/// the file is only a hint for the refusal text.
pub struct AtlasStore {
    root: PathBuf,
    catalog: Catalog,
    /// Ownership, held for this value's life and released by dropping it.
    /// Never read; its existence *is* the claim.
    _owner: wirk_core::jobs::OwnerLock,
    /// What the ownership pass recovered from the previous owner's
    /// recorded jobs, so a caller can report it instead of assuming the
    /// estate started clean.
    recovery: wirk_core::jobs::RecoveryOutcome,
    /// P4.5 B2/B3: how this estate bounds and contains the children it
    /// spawns. Held on the store because the store is what owns the
    /// estate, and because both backend protocols need the same thing.
    jobs: JobContext,
}

/// Everything one estate's expensive children are bounded by: what this
/// host can actually do, what the operator configured, and the flag that
/// cancels a job in flight.
///
/// One value, shared by both backend protocols, so `wirk-embed/v2` and
/// the query protocol cannot drift into two different containment stories
/// — they were identically unhardened before this, and they stay
/// identical now.
#[derive(Debug, Clone)]
pub struct JobContext {
    pub estate_root: PathBuf,
    pub capabilities: &'static wirk_core::jobs::JobCapabilities,
    pub policy: wirk_core::jobs::ResourcePolicy,
    /// Where this estate's running expensive jobs announce themselves.
    ///
    /// Replaces the single `CancelToken` this held before. That token was
    /// shared by every child and sticky once set, so one cancellation
    /// silently cancelled the estate's next job too — and nothing in the
    /// product called it in any case. A registry gives each job its own
    /// token and gives an operator something addressable to cancel.
    pub registry: wirk_core::jobs::JobRegistry,
    /// P4.5 B correction (ruling 0251, F4): the Work every job started
    /// from here is run *for*, as the daemon resolved it from the
    /// request against its own journals.
    ///
    /// Shared and interior-mutable for the same reason the registry is:
    /// the caller that knows the requester is the daemon handler, and
    /// the place the job is actually created is several layers down
    /// inside a backend run. Every atlas verb that can start a job
    /// serializes on the daemon's single atlas mutex, and the handler
    /// binds this for exactly the span it holds that mutex
    /// ([`RequesterBinding`]), so the value a child reads is the one its
    /// own request resolved and never a leftover from a previous verb.
    requester: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

/// Binds a [`JobContext`]'s requester for the life of one verb and
/// clears it on drop — including on an early return or a panic, so a
/// later administrative job can never inherit a previous caller's
/// identity.
/// Owned, not borrowed from the context: the daemon handler that binds
/// a requester goes on to take `&mut` on the very store the context
/// lives in, so a guard holding a reference into it would make the two
/// mutually exclusive. It holds the shared slot directly instead.
pub struct RequesterBinding {
    slot: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl Drop for RequesterBinding {
    fn drop(&mut self) {
        *self
            .slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }
}

impl JobContext {
    pub fn detect(estate_root: &Path) -> Self {
        let (policy, _) = wirk_core::jobs::ResourcePolicy::load(estate_root);
        Self::with_policy(estate_root, policy)
    }

    pub fn with_policy(estate_root: &Path, policy: wirk_core::jobs::ResourcePolicy) -> Self {
        Self {
            estate_root: estate_root.to_path_buf(),
            // Probed once per process, not once per open.
            capabilities: wirk_core::jobs::capabilities(),
            policy,
            registry: wirk_core::jobs::JobRegistry::new(),
            requester: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// The Work jobs started from here are currently being run for.
    pub fn requester(&self) -> Option<String> {
        self.requester
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn set_requester(&self, requester: Option<String>) {
        *self
            .requester
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = requester;
    }

    /// Bind `requester` until the returned guard is dropped. The caller
    /// must already hold whatever lock serializes the verb — for the
    /// daemon that is the atlas mutex, which every job-starting verb
    /// takes.
    pub fn bind_requester(&self, requester: Option<String>) -> RequesterBinding {
        self.set_requester(requester);
        RequesterBinding {
            slot: self.requester.clone(),
        }
    }

    /// A bounded child for one job of `verb` over `scope`, optionally
    /// owning `staging` so a later recovery pass removes it if this
    /// process is killed.
    ///
    /// `scope` is the target a cancellation can name — the source alias
    /// in practice. Each child gets a **fresh** cancel token and
    /// registers itself, so cancelling one job leaves the estate able to
    /// do legitimate work immediately afterwards.
    pub fn child<'a>(
        &'a self,
        verb: &str,
        scope: &str,
        staging: Option<PathBuf>,
    ) -> wirk_core::jobs::BoundedChild<'a> {
        wirk_core::jobs::BoundedChild {
            capabilities: self.capabilities,
            policy: &self.policy,
            cancel: wirk_core::jobs::CancelToken::new(),
            job_id: ulid::Ulid::generate().to_string(),
            estate_root: Some(self.estate_root.clone()),
            staging,
            verb: verb.to_string(),
            scope: scope.to_string(),
            requester: self.requester(),
            registry: Some(self.registry.clone()),
        }
    }

    /// A registered job for expensive work that runs on **this** thread
    /// rather than in a child process — a document collection's walk,
    /// its extraction, and the revalidation a publish re-runs.
    ///
    /// The same registry, the same `scope` a cancellation names and the
    /// same requester identity [`Self::child`] carries, so `atlas
    /// cancel --source <alias>` reaches document work exactly as it
    /// reaches a semantic build. The registration is released when the
    /// returned value drops, which is every return path out of the
    /// verb.
    ///
    /// What it delivers is a **cooperative** stop, not an interruption:
    /// see [`wirk_core::jobs::JobStop`]. Work already blocked in a
    /// syscall is not interrupted by it.
    pub fn in_process(&self, verb: &str, scope: &str) -> wirk_core::jobs::InProcessJob {
        wirk_core::jobs::InProcessJob::register(
            Some(self.registry.clone()),
            verb,
            scope,
            self.requester(),
            self.policy.job_deadline_secs,
        )
    }
}

/// Where one estate's Atlas keeps each kind of thing it owns.
///
/// One owner for the layout, as a free function so a caller that is
/// *inventorying* the estate can name these paths without opening the
/// store and taking its exclusive lock (P4.5 A, ruling 0256). Reading
/// what is on disk must not require becoming the estate's single writer;
/// it must also never invent a second spelling of these joins, which is
/// why `AtlasStore::open` resolves its own root through here.
#[derive(Debug, Clone)]
pub struct AtlasLayout {
    /// `<estate>/atlas`.
    pub root: PathBuf,
    /// `<estate>/atlas/generations` — one immutable directory per
    /// acquired generation (`manifest.json` + `resources.ndjson`). The
    /// indexed *bytes* are not here: they are read live from the source
    /// repository (`crate::git::blob`), which is why removing a
    /// generation never removes anything the user authored.
    pub generations: PathBuf,
    /// `<estate>/atlas/semantic` — one directory per semantic edition.
    pub editions: PathBuf,
    /// `<estate>/atlas/catalog.json` — memberships, publications and
    /// selections. Never an optional asset.
    pub catalog: PathBuf,
    /// `<estate>/atlas/findings.ndjson` — the derived findings index.
    pub findings_index: PathBuf,
    /// `<estate>/atlas/.owner` — B1's ownership lock file.
    pub owner_lock: PathBuf,
}

pub fn atlas_layout(estate_root: &Path) -> AtlasLayout {
    let root = estate_root.join("atlas");
    AtlasLayout {
        generations: root.join("generations"),
        editions: root.join("semantic"),
        catalog: root.join("catalog.json"),
        findings_index: root.join(crate::findings::FINDINGS_INDEX_FILE),
        owner_lock: root.join(".owner"),
        root,
    }
}

/// Which acquisition policy one `acquire`/`refresh` call runs under. Private dispatch only — the public surface is the
/// named methods (`acquire`/`acquire_document_tree`, ...), never this
/// enum, so a caller cannot pass the wrong variant for the method it
/// meant to call; a source's kind is fixed on `Membership::policy` at
/// registration (`register_git`/`register_document_tree`) and this
/// enum only ever mirrors what `acquire_kind` was asked to verify
/// against it.
///
/// The two policies deliberately do not share one `identity`/
/// `resources` method pair. Git's `git ls-tree` identity and its
/// resource walk are two separate, separately cheap operations. A
/// document tree's identity and its resources both come from hashing
/// the same file bytes, so computing them as two unrelated calls would
/// read and hash every file twice — and would allow staging a
/// generation whose `revision` named one tree state and whose
/// `resources` named another, if the tree changed in between.
/// `acquire_kind` dispatches the whole document-tree capture as one
/// operation (`doctree::capture` + `doctree::finish`); this enum keeps
/// only what both policies share verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcquisitionKind {
    Git,
    DocumentTree,
}

impl AcquisitionKind {
    fn policy_label(self) -> &'static str {
        match self {
            Self::Git => git::ACQUISITION_POLICY,
            Self::DocumentTree => doctree::ACQUISITION_POLICY,
        }
    }
}

/// Where one HTTP generation's raw response bytes live, beside its
/// `manifest.json`/`resources.ndjson` — written once by `stage`, read
/// back (never re-fetched) by `resolve_exact_http`/`publish_verify_http`.
fn content_bin(generation_dir: &Path) -> PathBuf {
    generation_dir.join("content.bin")
}

impl AtlasStore {
    pub fn open(
        estate_root: impl AsRef<Path>,
        scope: impl Into<String>,
    ) -> Result<Self, AtlasError> {
        let estate_root = estate_root.as_ref();
        let layout = atlas_layout(estate_root);
        let root = layout.root.clone();
        fs::create_dir_all(&layout.generations)?;
        // B1: ownership BEFORE the sweep. Everything below this point
        // assumes no other live store holds this estate's atlas; that
        // assumption is only sound because the claim was taken first.
        let (policy, policy_note) = wirk_core::jobs::ResourcePolicy::load(estate_root);
        if let Some(note) = &policy_note {
            eprintln!("wirk: {note}");
        }
        let owner = match wirk_core::jobs::OwnerLock::acquire_within(
            &layout.owner_lock,
            "AtlasStore",
            std::time::Duration::from_millis(policy.store_ownership_wait_millis),
        )? {
            Ok(owner) => owner,
            Err(hint) => {
                return Err(AtlasError::StoreInUse(format!(
                    "{} is already owned by a live AtlasStore ({}). A second opener is refused \
                     rather than allowed to sweep private temporaries a live build is still \
                     writing into. The claim is an advisory flock held for the owner's life and \
                     released by the kernel when it dies, so nothing needs to be cleaned up by \
                     hand if that holder was killed",
                    root.display(),
                    hint.describe()
                )));
            }
        };
        // Under ownership, and only under it: kill and remove the jobs a
        // previous owner of *this estate* recorded. Scoped to this
        // estate's own `.wirk/jobs/` records, never to a name prefix
        // under a shared cgroup parent — another live estate's jobs sit
        // there too and are not ours to touch.
        //
        // This is bounded recovery, not bounded prevention: a descendant
        // that escaped its process group kept running from the moment the
        // previous owner died until now, and that interval is the restart
        // interval, which nothing here bounds.
        let recovery = wirk_core::jobs::recover_owned_jobs(estate_root);
        let jobs = JobContext::with_policy(estate_root, policy);
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
        Ok(Self {
            root,
            catalog,
            _owner: owner,
            recovery,
            jobs,
        })
    }

    /// What this store's ownership pass recovered from the previous
    /// owner's recorded jobs. Empty is the ordinary case.
    pub fn recovery(&self) -> &wirk_core::jobs::RecoveryOutcome {
        &self.recovery
    }

    /// How this estate bounds its expensive children.
    pub fn jobs(&self) -> &JobContext {
        &self.jobs
    }

    /// The registry of expensive jobs this store is running, cloned so a
    /// caller can hold it **outside** whatever lock guards this store.
    ///
    /// That is the whole point. Every atlas verb serializes on one mutex
    /// around this store, so a cancellation routed through the store
    /// would queue behind the build it means to stop. A caller takes this
    /// handle once, at startup, and cancels through it while a build
    /// holds the store.
    pub fn job_registry(&self) -> wirk_core::jobs::JobRegistry {
        self.jobs.registry.clone()
    }

    /// Cancel every expensive job this store is currently running.
    ///
    /// In-process convenience over [`Self::job_registry`]; the reachable
    /// operator path is the daemon's own cancel verb, which does not go
    /// through the store at all.
    pub fn cancel_jobs(&self, reason: &str) -> Vec<wirk_core::jobs::CancelAck> {
        self.jobs
            .registry
            .cancel(&wirk_core::jobs::JobSelector::All, reason)
    }

    pub fn register_git(
        &mut self,
        alias: &str,
        locator: impl AsRef<Path>,
        requested_ref: &str,
    ) -> Result<Membership, AtlasError> {
        self.register(alias, locator, requested_ref, git::ACQUISITION_POLICY)
    }

    /// Explicit admission of a local non-Git document collection as its
    /// own source. Shares every catalog
    /// mechanic `register_git` already has (alias validation,
    /// canonicalization, idempotent re-registration) — the only
    /// difference recorded is `Membership::policy`, which is what later
    /// makes `acquire`/`refresh`/`publish`/`resolve_exact` treat this
    /// source as a document tree rather than a Git repository. This
    /// function itself runs no `git` and performs no repository
    /// discovery of any kind, so a directory nested inside an ambient
    /// Git repository — including one the workspace ignores — is
    /// admitted as exactly the directory named, never as that ambient
    /// repository.
    ///
    /// `requested_ref` is checked against `doctree::CURRENT_OBSERVATION`
    /// and refused by name otherwise. A document tree has no revision
    /// besides its own current state; recording an arbitrary
    /// caller-supplied string here would later be read back — by `atlas
    /// status`, among others — as if it named something this policy had
    /// actually honoured.
    pub fn register_document_tree(
        &mut self,
        alias: &str,
        locator: impl AsRef<Path>,
        requested_ref: &str,
    ) -> Result<Membership, AtlasError> {
        if requested_ref != doctree::CURRENT_OBSERVATION {
            return Err(AtlasError::InvalidRequest(format!(
                "a document-tree source observes only its current state; pass {:?} for \
                 --revision (or omit it) rather than {requested_ref:?}, which this policy has \
                 nothing to check it against",
                doctree::CURRENT_OBSERVATION
            )));
        }
        self.register(alias, locator, requested_ref, doctree::ACQUISITION_POLICY)
    }

    /// Explicit admission of one public HTTP(S) URL as its own source.
    /// Shares every catalog mechanic `register_git`/
    /// `register_document_tree` already have; the only differences are
    /// that `locator` is validated as a URL rather than canonicalized as
    /// a filesystem path (`http_source::validate_url`), and
    /// `Membership::policy` records `http_source::ACQUISITION_POLICY` —
    /// which is what later makes `acquire`/`refresh`/`publish`/
    /// `resolve_exact` treat this source as one bounded fetch rather
    /// than a Git repository or a local document collection.
    ///
    /// `requested_ref` is checked against
    /// `http_source::CURRENT_OBSERVATION` and refused by name
    /// otherwise, for the same reason `register_document_tree` checks
    /// it: an HTTP source has no revision besides its own last observed
    /// fetch, and an arbitrary caller-supplied string here would later
    /// be read back as if it named something this policy had checked.
    pub fn register_http(
        &mut self,
        alias: &str,
        url: &str,
        requested_ref: &str,
    ) -> Result<Membership, AtlasError> {
        if requested_ref != http_source::CURRENT_OBSERVATION {
            return Err(AtlasError::InvalidRequest(format!(
                "an HTTP source observes only the state its last acquire/refresh actually \
                 fetched; pass {:?} for --revision (or omit it) rather than {requested_ref:?}, \
                 which this policy has nothing to check it against",
                http_source::CURRENT_OBSERVATION
            )));
        }
        http_source::validate_url(url)?;
        self.register_with_locator(
            alias,
            url.to_string(),
            requested_ref,
            http_source::ACQUISITION_POLICY,
        )
    }

    fn register(
        &mut self,
        alias: &str,
        locator: impl AsRef<Path>,
        requested_ref: &str,
        policy: &str,
    ) -> Result<Membership, AtlasError> {
        let locator = locator
            .as_ref()
            .canonicalize()?
            .to_string_lossy()
            .into_owned();
        self.register_with_locator(alias, locator, requested_ref, policy)
    }

    /// The catalog mechanics every registration shares once its
    /// `locator` is already resolved to its final recorded string form —
    /// a canonicalized filesystem path for `git`/`document-tree`, an
    /// already-validated URL for `http` — so this never re-derives or
    /// re-validates what kind of locator it was given.
    fn register_with_locator(
        &mut self,
        alias: &str,
        locator: String,
        requested_ref: &str,
        policy: &str,
    ) -> Result<Membership, AtlasError> {
        if alias.is_empty() || alias.contains('/') || alias.contains('\0') {
            return Err(AtlasError::InvalidCoordinate("invalid source alias".into()));
        }
        if let Some(existing) = self.catalog.memberships.get(alias) {
            if existing.locator == locator && existing.policy == policy {
                return Ok(existing.clone());
            }
            if existing.locator != locator {
                return Err(AtlasError::InvalidCoordinate(
                    "alias already belongs to another source".into(),
                ));
            }
            return Err(AtlasError::InvalidCoordinate(
                "alias already belongs to a source registered under a different acquisition \
                 policy; a source's kind is decided once, at first registration, and is never \
                 changed underneath the same alias"
                    .into(),
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
            policy: policy.into(),
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
        self.acquire_kind(
            "atlas acquire",
            membership,
            requested_ref,
            policy,
            AcquisitionKind::Git,
        )
    }

    /// Acquisition over the document-tree policy.
    ///
    /// Identical staging and attempt-recording mechanics to
    /// `acquire`; the only difference is where identity and resources
    /// come from (`doctree::capture`/`doctree::finish`, one walk and one
    /// read per file, instead of `git::commit_and_tree`/`git::
    /// resources`). Refused outright (`InvalidRequest`, before touching
    /// the filesystem) if `membership` was not itself registered under
    /// `doctree::ACQUISITION_POLICY` — a source's kind, once explicitly
    /// chosen at registration, is never silently reinterpreted by a
    /// later call — or if `requested_ref` is not
    /// `doctree::CURRENT_OBSERVATION`.
    pub fn acquire_document_tree(
        &mut self,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
    ) -> Result<AcquireOutcome, AtlasError> {
        // Refused by name here too, not only at registration: a later
        // `refresh` can be asked with a different, equally arbitrary
        // string.
        if requested_ref != doctree::CURRENT_OBSERVATION {
            return Err(AtlasError::InvalidRequest(format!(
                "a document-tree source observes only its current state; pass {:?} for \
                 --revision (or omit it) rather than {requested_ref:?}",
                doctree::CURRENT_OBSERVATION
            )));
        }
        self.acquire_kind(
            "atlas acquire",
            membership,
            requested_ref,
            policy,
            AcquisitionKind::DocumentTree,
        )
    }

    /// `atlas acquire --dry-run`: the same walkers `acquire`/
    /// `acquire_document_tree` use, stopped short of extraction and of
    /// writing anything. Deliberately takes no `Membership` — a
    /// pre-acquisition preview must work for a source that is not yet
    /// registered — so nothing here reads or writes the catalog:
    /// `source` names only the admission/cancellation scope a caller
    /// asked under, never a registration.
    ///
    /// `kind` defaults to `"git"`, exactly like `acquire`'s own
    /// unspecified `--kind`. `"document-tree"` is refused unless
    /// `revision` is `doctree::CURRENT_OBSERVATION`, the identical rule
    /// `acquire_document_tree` applies. `"http"` is named and refused
    /// rather than silently attempted: no preview walker exists for it
    /// yet.
    pub fn preview(
        &self,
        source: &str,
        repository: &str,
        revision: &str,
        kind: Option<&str>,
        policy: &ExtractorPolicy,
    ) -> Result<crate::preview::PreviewReport, AtlasError> {
        let repo = Path::new(repository);
        let effective_kind = kind.unwrap_or("git");
        // Only the document-tree walk runs real I/O on this thread long
        // enough to be worth `atlas cancel --source` reaching — Git's
        // preview is one `ls-tree` call. Parallel to `acquire_kind`'s
        // own `job` split.
        let job = if effective_kind == "document-tree" {
            Some(self.jobs.in_process("atlas acquire --dry-run", source))
        } else {
            None
        };
        let stop = job
            .as_ref()
            .map(|job| job.stop())
            .unwrap_or_else(wirk_core::jobs::JobStop::unbounded);
        match effective_kind {
            "git" => {
                let (commit, _content) = git::commit_and_tree(repo, revision)?;
                Ok(git::preview(repo, &commit, policy, &self.capture_limits())?.finish())
            }
            "document-tree" => {
                if revision != doctree::CURRENT_OBSERVATION {
                    return Err(AtlasError::InvalidRequest(format!(
                        "a document-tree source observes only its current state; pass {:?} \
                         for --revision (or omit it) rather than {revision:?}",
                        doctree::CURRENT_OBSERVATION
                    )));
                }
                Ok(doctree::preview(repo, policy, &self.capture_limits(), &stop)?.finish())
            }
            "http" => Err(AtlasError::InvalidRequest(
                "a pre-acquisition preview is not available for --kind http sources".into(),
            )),
            other => Err(AtlasError::InvalidRequest(format!(
                "unknown source kind {other:?}; expected \"git\", \"document-tree\", or \"http\""
            ))),
        }
    }

    fn acquire_kind(
        &mut self,
        verb: &str,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
        kind: AcquisitionKind,
    ) -> Result<AcquireOutcome, AtlasError> {
        self.check_membership(membership)?;
        if membership.policy != kind.policy_label() {
            return Err(AtlasError::InvalidRequest(format!(
                "source {:?} was registered under {}, not {}",
                membership.alias,
                membership.policy,
                kind.policy_label()
            )));
        }
        let repo = Path::new(&membership.locator);
        // Announced for the document arm only, and before any walking
        // starts. Git's identity and resource walk are `git`'s own child
        // processes, already bounded and already registered by
        // `JobContext::child`; a document collection is walked on this
        // thread, so this is the registration that makes `atlas cancel
        // --source <alias>` reach it. Held for the whole verb and
        // released when `job` drops, on every return path below.
        let job = match kind {
            AcquisitionKind::DocumentTree => Some(self.jobs.in_process(verb, &membership.alias)),
            AcquisitionKind::Git => None,
        };
        let stop = job
            .as_ref()
            .map(|job| job.stop())
            .unwrap_or_else(wirk_core::jobs::JobStop::unbounded);
        let result = (|| {
            // A document tree's identity and its resources come from
            // the same file bytes, so both are taken from one walk and
            // one bounded read per file (`doctree::capture`), never two.
            // Git's identity (`git ls-tree`) is cheap and structurally
            // independent of its resource walk, so that path stays two
            // operations.
            let (revision, content, doctree_captured) = match kind {
                AcquisitionKind::Git => {
                    let (revision, content) = git::commit_and_tree(repo, requested_ref)?;
                    (revision, content, None)
                }
                AcquisitionKind::DocumentTree => {
                    let (revision, content, captured) =
                        doctree::capture(repo, &policy, &self.capture_limits(), &stop)?;
                    (revision, content, Some(captured))
                }
            };
            let id = GenerationId(ExtractorPolicy::generation_id(
                &membership.source.0,
                &revision,
                &content,
                policy.id(),
                kind.policy_label(),
            ));
            let destination = self.generation_dir(&id)?;
            let generation = if destination.exists() {
                self.read_generation(&id)?
            } else {
                let resources = match (kind, doctree_captured) {
                    (AcquisitionKind::Git, _) => {
                        git::resources(repo, &revision, &id, &policy, &self.capture_limits())?
                    }
                    (AcquisitionKind::DocumentTree, Some(captured)) => {
                        doctree::finish(&id, &policy, captured, &stop)?
                    }
                    (AcquisitionKind::DocumentTree, None) => {
                        unreachable!("DocumentTree always produces captured resources above")
                    }
                };
                // A single unavailable document does not refuse the
                // whole document-tree generation: every other readable
                // document stays usable, and the unavailable one's own
                // disposition discloses it. Git's behaviour is
                // deliberately different and unchanged — a missing or
                // unreadable Git object still fails the whole
                // acquisition, because a committed object that cannot be
                // read means the object store itself is incomplete.
                if kind == AcquisitionKind::Git
                    && let Some(unavailable) = resources
                        .iter()
                        .find(|resource| resource.disposition == CoverageDisposition::Unavailable)
                {
                    return Err(AtlasError::SourceBytesUnavailable(
                        unavailable
                            .detail
                            .clone()
                            .unwrap_or_else(|| "a required source object is unavailable".into()),
                    ));
                }
                let generation = SourceGeneration {
                    id: id.clone(),
                    source: membership.source.clone(),
                    revision,
                    content,
                    extractor_set: policy.id().into(),
                    acquisition_policy: kind.policy_label().into(),
                    locator: membership.locator.clone(),
                    requested_ref: requested_ref.into(),
                    resources,
                    origin: None,
                };
                self.stage(&generation, None)?;
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
            Err(AtlasError::SourceBytesUnavailable(detail)) => {
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
        self.acquire_kind(
            "atlas refresh",
            membership,
            requested_ref,
            policy,
            AcquisitionKind::Git,
        )
    }

    /// `refresh`'s document-tree counterpart.
    pub fn refresh_document_tree(
        &mut self,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
    ) -> Result<AcquireOutcome, AtlasError> {
        // Refused by name here too, not only at initial acquisition: a
        // later `refresh` can be asked with a different, equally
        // arbitrary string.
        if requested_ref != doctree::CURRENT_OBSERVATION {
            return Err(AtlasError::InvalidRequest(format!(
                "a document-tree source observes only its current state; pass {:?} for \
                 --revision (or omit it) rather than {requested_ref:?}",
                doctree::CURRENT_OBSERVATION
            )));
        }
        self.acquire_kind(
            "atlas refresh",
            membership,
            requested_ref,
            policy,
            AcquisitionKind::DocumentTree,
        )
    }

    /// Acquisition over the HTTP-source policy: one bounded fetch of
    /// `membership.locator` (`http_source::capture`), refused
    /// (`InvalidRequest`, before any network access) if `membership` was
    /// not itself registered under `http_source::ACQUISITION_POLICY` or
    /// `requested_ref` is not `http_source::CURRENT_OBSERVATION`.
    pub fn acquire_http(
        &mut self,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
    ) -> Result<AcquireOutcome, AtlasError> {
        self.acquire_http_kind("atlas acquire", membership, requested_ref, policy)
    }

    /// `acquire_http`'s explicit-refresh counterpart. Identical
    /// mechanics; `refresh` never re-registers so this is never told a
    /// kind and dispatches purely on `membership.policy`, exactly as
    /// `refresh`/`refresh_document_tree` already do.
    pub fn refresh_http(
        &mut self,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
    ) -> Result<AcquireOutcome, AtlasError> {
        self.acquire_http_kind("atlas refresh", membership, requested_ref, policy)
    }

    fn acquire_http_kind(
        &mut self,
        verb: &str,
        membership: &Membership,
        requested_ref: &str,
        policy: ExtractorPolicy,
    ) -> Result<AcquireOutcome, AtlasError> {
        self.check_membership(membership)?;
        if membership.policy != http_source::ACQUISITION_POLICY {
            return Err(AtlasError::InvalidRequest(format!(
                "source {:?} was registered under {}, not {}",
                membership.alias,
                membership.policy,
                http_source::ACQUISITION_POLICY
            )));
        }
        if requested_ref != http_source::CURRENT_OBSERVATION {
            return Err(AtlasError::InvalidRequest(format!(
                "an HTTP source observes only the state its last acquire/refresh actually \
                 fetched; pass {:?} for --revision (or omit it) rather than {requested_ref:?}",
                http_source::CURRENT_OBSERVATION
            )));
        }
        let limits = http_source::FetchLimits::from_policy(&self.jobs.policy);
        let result = http_source::capture(
            verb,
            &membership.locator,
            &limits,
            &self.root,
            &self.jobs,
            &membership.alias,
        )
        .and_then(|(revision, content, origin, bytes)| {
            let id = GenerationId(ExtractorPolicy::generation_id(
                &membership.source.0,
                &revision,
                &content,
                policy.id(),
                http_source::ACQUISITION_POLICY,
            ));
            let destination = self.generation_dir(&id)?;
            let generation = if destination.exists() {
                self.read_generation(&id)?
            } else {
                let resource = http_source::finish(
                    &id,
                    &policy,
                    &membership.locator,
                    &revision,
                    &bytes,
                    origin.content_type.as_deref(),
                );
                let generation = SourceGeneration {
                    id: id.clone(),
                    source: membership.source.clone(),
                    revision,
                    content,
                    extractor_set: policy.id().into(),
                    acquisition_policy: http_source::ACQUISITION_POLICY.into(),
                    locator: membership.locator.clone(),
                    requested_ref: requested_ref.into(),
                    resources: vec![resource],
                    origin: Some(Box::new(origin)),
                };
                self.stage(&generation, Some(&bytes))?;
                generation
            };
            Ok::<_, AtlasError>(generation)
        });
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
            Err(AtlasError::SourceBytesUnavailable(detail)) => {
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
    /// Removes this source's own catalog membership, publication and
    /// semantic selection —
    /// never the source's own original files, which this crate has
    /// never copied anywhere (`AtlasLayout::generations`'s own doc
    /// comment), and never this estate's own generation/edition
    /// directories on disk either.
    ///
    /// **This is deliberately narrow.** Sweeping generation directories
    /// from here would mean matching only on `generation.source`, after
    /// the catalog commit had already erased the very membership,
    /// publication and selection records `wirk estate clean`'s own
    /// retention derivation needs in order to tell a *referenced*
    /// generation from an *orphaned* one.
    /// This crate cannot see what `wirk estate clean` can: a
    /// non-terminal Work's delivered World, or an unsettled finding,
    /// both live in `wirkd`'s own journals and findings index, entirely
    /// outside `wirk-atlas`. So this method now does only the one thing
    /// it can safely do on its own — unregister the catalog entry — and
    /// the caller (`wirk/src/wirkd/server.rs::handle_atlas_remove`) is
    /// responsible for checking, *before* calling this, that nothing
    /// still needs this membership's published generation or selected
    /// edition (the same retention derivation `estate clean` refuses
    /// against). Once unregistered, this membership's own generation
    /// and edition directories are unreferenced by this estate's
    /// catalog like any other orphaned one, and `wirk estate clean
    /// --class atlas-generations|atlas-editions --all-unreferenced`
    /// reclaims them — re-deriving retention against every *other*
    /// membership first, reporting failures and remaining work
    /// honestly, and reachable exactly the way `wirk estate storage`
    /// already inventories them. They are reachable through a public
    /// verb, not stranded.
    pub fn remove_source(&mut self, membership: &Membership) -> Result<RemovalOutcome, AtlasError> {
        self.check_membership(membership)?;
        let released_generation = self.catalog.published.get(&membership.id.0).cloned();
        let released_edition = self
            .catalog
            .semantic_selected
            .get(&membership.id.0)
            .cloned();
        let mut next = self.catalog.clone();
        next.memberships.remove(&membership.alias);
        next.published.remove(&membership.id.0);
        next.semantic_selected.remove(&membership.id.0);
        next.attempts
            .retain(|attempt| attempt.membership != membership.id);
        next.publication_revision += 1;
        self.commit_catalog(next)?;
        Ok(RemovalOutcome {
            membership: membership.id.clone(),
            released_generation,
            released_edition,
        })
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
        match staged.acquisition_policy.as_str() {
            git::ACQUISITION_POLICY => self.publish_verify_git(membership, &staged)?,
            // Registered for the same reason the acquisition is: this
            // arm re-runs the whole walk and extraction on this thread.
            // The registration is released when `job` drops at the end
            // of this arm, before the catalog is touched, so a cancel
            // that arrives after verification passes finds nothing to
            // stop rather than stopping a catalog commit half way.
            http_source::ACQUISITION_POLICY => self.publish_verify_http(&staged)?,
            doctree::ACQUISITION_POLICY => {
                let job = self.jobs.in_process("atlas publish", &membership.alias);
                self.publish_verify_doctree(&staged, &job.stop())?
            }
            other => {
                return Err(AtlasError::Generation(format!(
                    "generation names an unknown acquisition policy {other:?}"
                )));
            }
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

    fn publish_verify_git(
        &self,
        membership: &Membership,
        staged: &SourceGeneration,
    ) -> Result<(), AtlasError> {
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
        Ok(())
    }

    /// `publish`'s document-tree counterpart.
    ///
    /// `publish_verify_git` can compare cheap `git ls-tree` identity
    /// without re-running extraction. A document tree has no equivalent
    /// free structural listing, so this re-runs the same
    /// `doctree::capture` walk the acquisition ran and compares the full
    /// result against what was staged. Still one walk and one bounded
    /// read per file, and bounded by the estate's configured document
    /// capture limits.
    ///
    /// That makes a document publish genuinely acquisition-priced rather
    /// than a catalog edit, which is why its caller admits it as
    /// expensive work. Dropping the revalidation instead is not an
    /// option: it is the only thing standing between a publication and a
    /// collection that has changed underneath its staged generation.
    fn publish_verify_doctree(
        &self,
        staged: &SourceGeneration,
        stop: &wirk_core::jobs::JobStop,
    ) -> Result<(), AtlasError> {
        let root = Path::new(&staged.locator);
        let policy = ExtractorPolicy::from_id(&staged.extractor_set).ok_or_else(|| {
            AtlasError::Generation("generation names an unknown extraction edition".into())
        })?;
        let limits = self.capture_limits();
        let (revision, content, captured) = doctree::capture(root, &policy, &limits, stop)?;
        if revision != staged.revision || content != staged.content {
            return Err(AtlasError::Generation(
                "document tree has changed since this generation was staged; re-acquire before \
                 publishing"
                    .into(),
            ));
        }
        let current = doctree::finish(&staged.id, &policy, captured, stop)?;
        if current != staged.resources {
            return Err(AtlasError::Generation(
                "document tree resources do not exactly match the staged generation".into(),
            ));
        }
        Ok(())
    }

    /// `publish`'s HTTP counterpart. Unlike `publish_verify_doctree`,
    /// this never touches the network — an HTTP generation's "current
    /// state" was already fixed the moment `acquire`/`refresh` fetched
    /// it (`http_source`'s own top-level doc: `search`/`resolve` never
    /// reach the network, and neither does `publish`). What this checks
    /// is narrower and purely local: that the cached `content.bin` this
    /// generation staged is still exactly the bytes its own recorded
    /// `revision`/`content` name, catching on-disk corruption of this
    /// estate's own working cache rather than upstream drift — drift is
    /// what an explicit `refresh` is for.
    fn publish_verify_http(&self, staged: &SourceGeneration) -> Result<(), AtlasError> {
        let path = content_bin(&self.generation_dir(&staged.id)?);
        let bytes = fs::read(&path).map_err(|error| {
            AtlasError::Generation(format!(
                "cached response bytes for {} are missing or unreadable: {error}",
                staged.locator
            ))
        })?;
        let digest = http_source::hash_hex(&bytes);
        if digest != staged.revision || format!("sha256:{digest}") != staged.content {
            return Err(AtlasError::Generation(
                "cached response bytes no longer match this generation's recorded identity".into(),
            ));
        }
        Ok(())
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
                "resource has no recorded content identity".into(),
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
        match generation.acquisition_policy.as_str() {
            git::ACQUISITION_POLICY => self.resolve_exact_git(membership, &generation, coordinate),
            doctree::ACQUISITION_POLICY => self.resolve_exact_doctree(&generation, coordinate),
            http_source::ACQUISITION_POLICY => self.resolve_exact_http(&generation, coordinate),
            other => Err(AtlasError::Generation(format!(
                "generation names an unknown acquisition policy {other:?}"
            ))),
        }
    }

    fn resolve_exact_git(
        &self,
        membership: &Membership,
        generation: &SourceGeneration,
        coordinate: &ExactCoordinate,
    ) -> Result<ResolveOutcome, AtlasError> {
        // This generation's own edition, never today's: its units index
        // whatever string that edition's interpretation produced.
        let edition = crate::extract::ExtractorEdition::recorded(&generation.extractor_set)?;
        let repo = Path::new(&membership.locator);
        let (_, content) = match git::commit_and_tree(repo, &generation.revision) {
            Ok(identity) => identity,
            Err(AtlasError::SourceBytesUnavailable(detail)) => {
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
            Err(AtlasError::SourceBytesUnavailable(detail)) => {
                return Ok(ResolveOutcome::Unavailable(detail));
            }
            Err(error) => return Err(error),
        }
        match git::blob(repo, &coordinate.object_id) {
            Ok(bytes) => match document::render_if_document(edition, &coordinate.path, bytes) {
                Ok(rendered) => {
                    Self::resolved_from_bytes(coordinate, &rendered, "committed Git bytes")
                }
                Err(detail) => Ok(ResolveOutcome::Unavailable(detail)),
            },
            Err(AtlasError::SourceBytesUnavailable(detail)) => {
                Ok(ResolveOutcome::Unavailable(detail))
            }
            Err(e) => Err(e),
        }
    }

    /// `resolve_exact`'s document-tree counterpart.
    ///
    /// **The precondition is per-resource, deliberately.** Recomputing
    /// the whole tree's manifest identity first and reporting
    /// `Unavailable` whenever it differed would make editing **any**
    /// file in the collection invalidate **every** coordinate previously
    /// issued against that generation — the ordinary case in a live
    /// document collection, not an edge. It would also be a mis-analogy
    /// with `resolve_exact_git`, which re-resolves the *recorded
    /// commit*: that asks whether the recorded history is still
    /// available, not whether the whole working tree stood still.
    ///
    /// `doctree::blob` below already re-reads and re-hashes exactly the
    /// one named path and refuses unless its SHA-256 equals the
    /// coordinate's own `object_id` — proving the returned bytes are
    /// exactly the bytes the coordinate names. That is now the only
    /// check this method runs: honest `Unavailable` still fires exactly
    /// when *this resource's own bytes* moved, vanished, or stopped
    /// being an ordinary file (all three distinguished inside `blob`),
    /// which is the disclosed-rather-than-silently-wrong boundary this
    /// policy actually promises — it does not require, and this does
    /// not add, any historical byte store.
    fn resolve_exact_doctree(
        &self,
        generation: &SourceGeneration,
        coordinate: &ExactCoordinate,
    ) -> Result<ResolveOutcome, AtlasError> {
        let edition = crate::extract::ExtractorEdition::recorded(&generation.extractor_set)?;
        let root = Path::new(&generation.locator);
        match doctree::blob(
            root,
            &coordinate.path,
            &coordinate.object_id,
            &self.capture_limits(),
        ) {
            Ok(bytes) => match document::render_if_document(edition, &coordinate.path, bytes) {
                Ok(rendered) => {
                    Self::resolved_from_bytes(coordinate, &rendered, "document-tree bytes")
                }
                Err(detail) => Ok(ResolveOutcome::Unavailable(detail)),
            },
            Err(AtlasError::SourceBytesUnavailable(detail)) => {
                Ok(ResolveOutcome::Unavailable(detail))
            }
            Err(AtlasError::Io(io_error)) => Ok(ResolveOutcome::Unavailable(io_error.to_string())),
            Err(error) => Err(error),
        }
    }

    /// `resolve_exact`'s HTTP counterpart. Reads this generation's own
    /// cached `content.bin` **from disk, never the network** — the
    /// whole point of persisting the fetch once (`http_source`'s own
    /// top-level doc) — and refuses unless its SHA-256 equals the
    /// coordinate's own `object_id`, the same proof-the-returned-bytes-
    /// are-the-named-bytes discipline `resolve_exact_doctree` applies to
    /// a live file. `Unavailable` here means this estate's own working
    /// cache is missing or has been corrupted since it was staged; an
    /// operator's remedy is an explicit `refresh`, never an implicit
    /// re-fetch from this call.
    fn resolve_exact_http(
        &self,
        generation: &SourceGeneration,
        coordinate: &ExactCoordinate,
    ) -> Result<ResolveOutcome, AtlasError> {
        let edition = crate::extract::ExtractorEdition::recorded(&generation.extractor_set)?;
        let path = content_bin(&self.generation_dir(&generation.id)?);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(ResolveOutcome::Unavailable(format!(
                    "cached response bytes for {} {error}",
                    generation.locator
                )));
            }
        };
        let digest = http_source::hash_hex(&bytes);
        if digest != coordinate.object_id {
            return Ok(ResolveOutcome::Unavailable(
                "staged response bytes no longer match the recorded content hash".into(),
            ));
        }
        // Routed through the same renderer every other policy resolves
        // through: a fetched response that this extractor admitted as a
        // document was unitized against its Markdown rendering, so the
        // span a coordinate names is a span of that rendering here too.
        match document::render_if_document(edition, &coordinate.path, bytes) {
            Ok(rendered) => {
                Self::resolved_from_bytes(coordinate, &rendered, "staged HTTP response bytes")
            }
            Err(detail) => Ok(ResolveOutcome::Unavailable(detail)),
        }
    }

    /// Read one already-recorded resource through `anydoc`'s shared
    /// document model and describe what it holds: its shape, and an
    /// inventory of the assets it embeds — never their bytes.
    ///
    /// **Addressed by the resource a coordinate already names.** This
    /// takes the same `ExactCoordinate` `resolve_exact` does and uses the
    /// same part of it — estate, membership, generation, path, object id —
    /// so a caller that can resolve a hit can inspect the source behind
    /// it without a second addressing scheme. Its byte and line bounds
    /// name a span of the *rendering* and have no meaning for the
    /// structure, so they are deliberately not read here; the document is
    /// the whole resource either way.
    ///
    /// **Parent-source authority is the caller's to establish, exactly as
    /// it is for `resolve_exact`:** the membership must already have been
    /// admitted under the asking scope, and `check_membership` re-checks
    /// it against this estate's own catalog before anything is read.
    /// Bytes are read raw — the original container, not its Markdown
    /// rendering — and refused unless they still hash to the object id the
    /// coordinate names, the same proof `resolve_exact` requires.
    pub fn document_reading(
        &self,
        membership: &Membership,
        coordinate: &ExactCoordinate,
    ) -> Result<crate::document::DocumentReading, AtlasError> {
        let (edition, bytes) = self.source_bytes_for(membership, coordinate)?;
        Ok(document::inspect(edition, &coordinate.path, &bytes))
    }

    /// One embedded asset's bytes, selected by the `id` a
    /// [`Self::document_reading`] inventory listed — `anydoc`'s own
    /// `AssetId`, the selector the document model already uses, rather
    /// than a second coordinate vocabulary invented for assets.
    ///
    /// `max_bytes` bounds what this will hand back: an asset larger than
    /// the caller's own limit is refused by name and size rather than
    /// read into a reply. `Ok(None)` means this document defines no asset
    /// with that id — never some other asset's bytes.
    pub fn document_asset(
        &self,
        membership: &Membership,
        coordinate: &ExactCoordinate,
        id: usize,
        max_bytes: u64,
    ) -> Result<Option<crate::document::ResolvedAsset>, AtlasError> {
        let (edition, bytes) = self.source_bytes_for(membership, coordinate)?;
        let found = document::asset(edition, &coordinate.path, &bytes, id)
            .map_err(AtlasError::SourceBytesUnavailable)?;
        if let Some(found) = &found
            && found.descriptor.byte_len > max_bytes
        {
            return Err(AtlasError::InvalidRequest(format!(
                "embedded asset {id} is {} bytes, past the {max_bytes}-byte bound on one asset \
                 this daemon will read into memory",
                found.descriptor.byte_len
            )));
        }
        Ok(found)
    }

    /// The original source bytes one coordinate names — the container as
    /// the source holds it, before any document rendering — proven to be
    /// the bytes that coordinate names by re-hashing them against its own
    /// `object_id`. Shared by the two structured readers above, which both
    /// need the container and neither of which may accept a substitute.
    fn source_bytes_for(
        &self,
        membership: &Membership,
        coordinate: &ExactCoordinate,
    ) -> Result<(crate::extract::ExtractorEdition, Vec<u8>), AtlasError> {
        self.check_membership(membership)?;
        let generation = self.read_generation(&coordinate.generation)?;
        let edition = crate::extract::ExtractorEdition::recorded(&generation.extractor_set)?;
        if generation.source != coordinate.source {
            return Err(AtlasError::InvalidCoordinate(
                "coordinate's generation belongs to another source".into(),
            ));
        }
        let Some(resource) = generation
            .resources
            .iter()
            .find(|resource| resource.path == coordinate.path)
        else {
            return Err(AtlasError::InvalidCoordinate(
                "this generation records no resource at that path".into(),
            ));
        };
        if resource.object_id.as_deref() != Some(coordinate.object_id.as_str()) {
            return Err(AtlasError::InvalidCoordinate(
                "coordinate's object id is not what this generation recorded for that path".into(),
            ));
        }
        // Each policy's own raw read is already the identity check: a Git
        // object is addressed by its own content hash, and the
        // document-tree and HTTP arms re-hash what they read and refuse a
        // mismatch. Nothing here accepts bytes that are not the ones the
        // coordinate names.
        let bytes = crate::hydrate::raw_blob(
            &generation.acquisition_policy,
            Path::new(&generation.locator),
            self.root(),
            &generation.id,
            crate::hydrate::RecordedResource {
                path: &coordinate.path,
                object_id: &coordinate.object_id,
            },
            &self.capture_limits(),
        )?;
        Ok((edition, bytes))
    }

    /// Shared by every policy's final step: exact bytes have been read
    /// and their identity already confirmed by the caller: only the
    /// byte/line-bound arithmetic and the coordinate's own promised
    /// line bounds remain to check, identically either way.
    fn resolved_from_bytes(
        coordinate: &ExactCoordinate,
        bytes: &[u8],
        bytes_label: &str,
    ) -> Result<ResolveOutcome, AtlasError> {
        let Some((line_start, line_end)) =
            actual_line_bounds(bytes, coordinate.byte_start, coordinate.byte_end)
        else {
            return Err(AtlasError::InvalidCoordinate(format!(
                "byte bounds are invalid or split {bytes_label}"
            )));
        };
        if line_start != coordinate.line_start || line_end != coordinate.line_end {
            return Err(AtlasError::InvalidCoordinate(format!(
                "line bounds do not match {bytes_label}"
            )));
        }
        Ok(ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: coordinate.clone(),
            bytes: bytes[coordinate.byte_start as usize..coordinate.byte_end as usize].to_vec(),
        }))
    }
    /// `raw` is `Some` only for an `http-source-policy/v1` generation:
    /// the fetched response bytes, persisted once as `content.bin`
    /// beside `manifest.json`/`resources.ndjson` — this policy's whole
    /// working cache, immutable and removable through the same
    /// generation-directory lifecycle every other policy's already is
    /// (`http_source`'s own top-level doc). Every other policy reads its
    /// bytes live from its own source and passes `None`.
    fn stage(&self, generation: &SourceGeneration, raw: Option<&[u8]>) -> Result<(), AtlasError> {
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
        if let Some(bytes) = raw {
            self.write_sync(&content_bin(&temp), bytes)?;
            checkpoint("stage-content-synced");
        }
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
    /// What one document-collection capture or read is bounded by in
    /// this estate, resolved from the estate's own configured resource
    /// policy. Irrelevant to a Git source, which is bounded by its own
    /// object store.
    pub(crate) fn capture_limits(&self) -> doctree::CaptureLimits {
        doctree::CaptureLimits::from_policy(&self.jobs.policy)
    }

    pub fn generation(&self, id: &GenerationId) -> Result<SourceGeneration, AtlasError> {
        self.read_generation(id)
    }
    fn read_generation(&self, id: &GenerationId) -> Result<SourceGeneration, AtlasError> {
        let generation_dir = self.generation_dir(id)?;
        let path = generation_dir.join("manifest.json");
        if !path.exists() {
            return Err(AtlasError::Generation(id.0.clone()));
        }
        // Everything read here is *this generation's own* derived data,
        // which this store wrote itself. A manifest that will not open,
        // or will not parse, is therefore the same fact as the missing
        // one just above — "generation is incomplete or absent" — and
        // not a general I/O or JSON fault of the estate. Classifying it
        // at the read is what lets every caller treat it as source-local
        // (`query::search`'s per-membership walk, `handle_atlas_status`'s
        // per-source row) instead of aborting an answer that other,
        // healthy sources could still fill. Nothing outside this
        // generation's own directory is reclassified: catalog,
        // coordinate and store-ownership failures keep their own
        // variants and still abort their callers.
        let bytes = fs::read(&path)
            .map_err(|error| AtlasError::Generation(format!("manifest is unreadable: {error}")))?;
        let g: SourceGeneration = serde_json::from_slice(&bytes)
            .map_err(|error| AtlasError::Generation(format!("manifest is malformed: {error}")))?;
        if g.id != *id {
            return Err(AtlasError::Generation("forged manifest identifier".into()));
        }
        self.validate_generation(&g, id)?;
        let resource_path = generation_dir.join("resources.ndjson");
        let serialized = fs::read_to_string(resource_path)
            .map_err(|_| AtlasError::Generation("missing immutable resource manifest".into()))?;
        let rows: Result<Vec<crate::ResourceRecord>, _> =
            serialized.lines().map(serde_json::from_str).collect();
        let rows = rows.map_err(|error| {
            AtlasError::Generation(format!("resource manifest row is malformed: {error}"))
        })?;
        if rows != g.resources {
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
        // The revision/content *shape* a generation must carry depends on which acquisition policy produced it —
        // a Git commit sha (40 hex) and root-tree sha1, or a
        // document-tree manifest hash (64 hex) and its own sha256 tag.
        // An `acquisition_policy` naming neither known label is refused
        // outright, the same way an unknown `extractor_set` already is
        // below.
        let identity_shape_valid = match generation.acquisition_policy.as_str() {
            git::ACQUISITION_POLICY => {
                valid_sha1(&generation.revision)
                    && generation.content.starts_with("sha1:")
                    && valid_sha1(&generation.content[5..])
            }
            doctree::ACQUISITION_POLICY | http_source::ACQUISITION_POLICY => {
                valid_sha256(&generation.revision)
                    && generation.content.starts_with("sha256:")
                    && valid_sha256(&generation.content[7..])
            }
            _ => false,
        };
        if generation.id != *id
            || !valid_generation_id(&generation.id.0)
            || generation.source.0.is_empty()
            || !identity_shape_valid
            || generation.extractor_set.is_empty()
            || generation.id.0
                != ExtractorPolicy::generation_id(
                    &generation.source.0,
                    &generation.revision,
                    &generation.content,
                    &generation.extractor_set,
                    &generation.acquisition_policy,
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
                let Some((expected_family, expected_unitizer)) =
                    edition.recorded_shape(&resource.path)
                else {
                    return Err(AtlasError::Generation(
                        "indexed resource at a path this edition admits nothing at".into(),
                    ));
                };
                let mut previous_end = 0;
                let mut previous_end_line = 0;
                for unit in &resource.units {
                    if unit.unitizer != expected_unitizer
                        || unit.family != expected_family
                        || unit.id
                            != ExtractorPolicy::unit_id(
                                &generation.id,
                                &resource.path,
                                object_id,
                                unit.family,
                                unit.byte_start,
                                unit.byte_end,
                                &unit.unitizer,
                            )
                        || unit.byte_start != previous_end
                        || unit.byte_end < unit.byte_start
                        || unit.byte_end - unit.byte_start > 64 * 1024
                        || unit.line_start == 0
                        || unit.line_start > unit.line_end
                        || (unit.line_start != previous_end_line
                            && unit.line_start != previous_end_line + 1)
                        || (!edition.multiline() && unit.line_start != unit.line_end)
                    {
                        return Err(AtlasError::Generation(
                            "derived retrieval unit identity or bounds are inconsistent".into(),
                        ));
                    }
                    previous_end = unit.byte_end;
                    previous_end_line = unit.line_end;
                }
                // `resource.byte_len` is always the *original* file's
                // length (set once in `doctree::finish`/`git::resources`
                // from the bytes actually read, never recomputed here).
                // For every other family that is also the span the
                // resource's own units tile, so a gap or short tail is
                // real evidence of a bad extractor. A `Document`
                // resource's units tile `crate::document::render`'s
                // *Markdown* rendering instead — an unrelated length —
                // so this comparison would be checking the original PDF
                // or DOCX's byte count against how much Markdown it
                // rendered to, which is never expected to match and
                // proves nothing about coverage. The ordering/identity
                // checks above this line still run unconditionally: a
                // `Document` resource's units still have to be
                // contiguous, correctly identified and correctly typed.
                if expected_family != ContentFamily::Document
                    && resource.byte_len != Some(previous_end)
                {
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
    /// The generation id this membership currently publishes, **without
    /// reading it back**.
    ///
    /// [`Self::current`] resolves the whole generation from disk and
    /// therefore fails once its bytes are gone — which is correct for a
    /// reader and exactly wrong for an inventory, whose whole job is to
    /// say which ids are retained *before* deciding what may be removed.
    /// A retention set built from `current` would forget to protect a
    /// published generation the moment it became unreadable.
    pub fn published_generation(&self, membership: &Membership) -> Option<GenerationId> {
        self.catalog.published.get(&membership.id.0).cloned()
    }

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
/// The document-tree policy's revision/content shape — a bare or
/// `sha256:`-tagged 64-character hex SHA-256, the length alone already
/// enough to tell it apart from `valid_sha1`'s 40-character Git shape.
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
/// One gate for the whole product (W-B `findings.rs`, W4-A
/// `semantic.rs`, and `wirkd::server`'s own index windows): the identical
/// mechanism reused rather than a second one invented per append-only
/// log. `pub` rather than `pub(crate)` only because `server.rs` names
/// three windows of its own on this same gate; nothing else changed.
///
/// Two things a verifier can ask for at a named window, and **only**
/// these two — this gate never fabricates a reply, a row, an outcome or
/// an error, and it is inert unless its own environment variable names
/// this exact window:
///
/// - `WIRK_ATLAS_FAILPOINT=<name>` dies here, the crash-window half that
///   already existed;
/// - `WIRK_ATLAS_BARRIER=<name>=<directory>` **parks** here, the
///   scheduling half. A concurrency defect at a real window between two
///   real operations cannot be pinned by a sleep: the test has to hold
///   one real thread at the real instant and drive the other past it.
///   Exactly one caller parks — the arm file is claimed by an atomic
///   `rename`, so a second thread reaching the same window runs straight
///   through — and it resumes when, and only when, the controller that
///   armed the window says so.
///
/// **Nothing here is decided or paced by time** (ruling 0044 D134,
/// final). The park is a blocking read on a real socket the controller
/// owns: no interval, no poll, no deadline, and no path on which a
/// parked operation resumes by itself while its caller is told it
/// observed a schedule it never observed. A verifier that never releases
/// the window holds its own parked operation for as long as it lives,
/// which is the point — the termination bound belongs to the test
/// controller, which reports its exhaustion as "the window was never
/// reached" or "the operation was never observed to finish", never as a
/// verdict.
pub fn checkpoint(name: &str) {
    if std::env::var("WIRK_ATLAS_FAILPOINT").ok().as_deref() == Some(name) {
        std::process::exit(86);
    }
    barrier(name);
}

/// The release socket a verifier binds inside the barrier directory
/// before it arms the window.
pub const BARRIER_RELEASE_SOCKET: &str = "release.sock";

fn barrier(name: &str) {
    let Ok(setting) = std::env::var("WIRK_ATLAS_BARRIER") else {
        return;
    };
    let Some((window, dir)) = setting.split_once('=') else {
        return;
    };
    if window != name {
        return;
    }
    let dir = Path::new(dir);
    // One arm, one parked thread: `rename` is atomic, so of every caller
    // that reaches this window while the barrier is armed exactly one
    // takes the arm file and parks and every other returns immediately.
    if fs::rename(dir.join("arm"), dir.join("arrived")).is_err() {
        return;
    }
    // Park by connecting to the controller's listening socket and
    // blocking on a read only its end can end. `accept` returning on the
    // controller's side *is* this thread's arrival, and the read returns
    // when the controller drops its end of the connection, or dies and
    // the kernel closes it — a state change of the peer, observed,
    // exactly as `wirkd`'s own client reads treat a closed stream. No
    // read timeout is ever set on this stream, so no elapsed time exists
    // anywhere on this path.
    //
    // If the socket cannot be reached the arm was already claimed and
    // this thread is committed: continuing would silently release a real
    // operation and let it report a schedule it was never held for, so
    // it fails loudly instead. Reachable only under this environment
    // variable, and only for the one window it names.
    let socket = dir.join(BARRIER_RELEASE_SOCKET);
    let mut release = std::os::unix::net::UnixStream::connect(&socket).unwrap_or_else(|error| {
        panic!(
            "WIRK_ATLAS_BARRIER armed window {name} at {} has no reachable release socket {}: \
             {error}",
            dir.display(),
            socket.display()
        )
    });
    // Every outcome of this read is the controller's end going away,
    // which is the release; the bytes themselves carry nothing.
    let mut ignored = Vec::new();
    let _ = std::io::Read::read_to_end(&mut release, &mut ignored);
}
