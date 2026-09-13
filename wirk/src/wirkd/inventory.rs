//! What this estate owns, what still needs it, and what an explicit
//! cleanup may therefore remove (P4.5 increment A, ruling 0256).
//!
//! Increment B made expensive work *bounded*: one owner of the atlas,
//! contained children, visible admission. This module answers the
//! question B left open — the estate keeps growing, and nothing told
//! anyone what it was keeping or let them get any of it back.
//!
//! ## What is owned, and what is not
//!
//! Everything measured here is something **wirk derived or executed**:
//! a checkout it materialized, a staging directory it created, a
//! content-addressed runtime image it installed, a generation manifest
//! it acquired, a semantic edition it built.
//!
//! The user's own material is **not** here, and the distinction is
//! structural rather than a promise. A source's bytes never enter the
//! estate at all: a generation holds `manifest.json` and
//! `resources.ndjson`, and the indexed content is read live from the
//! source repository (`wirk_atlas::git::blob`). So removing every
//! generation in this estate cannot remove one byte a user wrote. The
//! inventory names each registered source's locator so a reader can see
//! exactly that — named, never walked, never measured, never a
//! candidate.
//!
//! ## Retention is derived from concrete consumers
//!
//! [`Retention`] is built from the catalog and the Works' own journals,
//! never from age, path prefix, an ignore rule or a stale pid — none of
//! which is evidence of orphanhood (ruling 0124). Three tiers, following
//! `resource-contract/refine/REFINED.md` §3:
//!
//! * **Never removable.** Journals, projections, retained Claim bytes,
//!   the catalog, authored Routes. Not "protected by a check" — not
//!   offered as candidates at all.
//! * **Required references.** The published generation, the selected
//!   edition, generations named by a **non-terminal** Work's projection,
//!   generations named by an **unsettled** finding, a non-terminal
//!   Work's worker contract, a runtime image an existing Run pin shares
//!   an inode with.
//! * **Optional replay assets.** Everything else: a superseded edition,
//!   a generation only a *finished* Work's projection names, a contract
//!   digest no open Work reserves, an image nothing links to. Removable
//!   only by explicit user selection, and only after the whole retention
//!   set was read successfully.
//!
//! A journal that merely *mentions* a generation or a contract digest
//! holds a **name**, not a byte requirement: the journal stays readable
//! and its World stays byte-identical after the optional asset is gone.
//! What changes is what a *later read* of that World can deliver, and it
//! says so in the existing vocabulary (`UnavailableReason::
//! GenerationUnavailable`, `EvidenceCoverage::Degraded`) rather than
//! through anything invented here.
//!
//! ## What the numbers do and do not mean
//!
//! Measurement is [`wirk_core::storage`]'s, with its three figures and
//! its refusal to call any of them reclaimable. This module adds the
//! part that is specific to an estate: a Run's pinned `wirk` is a hard
//! link to a shared runtime image, so `unique_allocated_bytes` — summed
//! across classes through one shared [`Dedup`] — is the only figure that
//! adds up to something true about the estate as a whole.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wirk_core::storage::{Dedup, Disclosure, Measured, Unreadable, WALK_BUDGET, tell_unreadable};

/// One Work, as the retention derivation needs it.
#[derive(Debug, Clone)]
pub(crate) struct WorkFacts {
    pub(crate) id: String,
    /// `false` means this Work can still run, expand its World, or write
    /// an output — so everything it references is a required reference.
    pub(crate) terminal: bool,
    /// Every Run this Work opened, in journal order.
    pub(crate) runs: Vec<String>,
    /// `(membership id, generation id)` pairs named by this Work's
    /// delivered projections.
    pub(crate) projected_generations: BTreeSet<String>,
    /// Worker contract digests this Work's reserved Worlds name.
    pub(crate) contract_digests: BTreeSet<String>,
    /// Stored managed paths recorded by this Work's **validated** Claims,
    /// relative to its outputs directory.
    pub(crate) validated_managed_paths: BTreeSet<String>,
    /// Whether any validated Claim recorded an artifact living in the
    /// checkout rather than in managed outputs.
    pub(crate) claim_evidence_in_checkout: bool,
}

/// The categories a failure can belong to.
///
/// Each is a fixed sentence-fragment naming a *kind* of record, written
/// where the code already knows what it is reading. A scoped caller gets
/// one of these instead of the path, so the diagnostic stays readable
/// and actionable without becoming an existence disclosure (ruling
/// 0260). They are constants rather than derived from the path precisely
/// because deriving them would put the identity back.
pub(crate) const WORK_JOURNAL: &str = "a Work's own journal";
pub(crate) const WORK_PROJECTIONS: &str = "a Work's delivered projections";
pub(crate) const WORK_CLAIMS: &str = "a Work's retained Claim bytes";
pub(crate) const WORK_STAGING: &str = "a Run's output staging";
pub(crate) const WORK_CHECKOUT: &str = "a Work's checkout";
pub(crate) const WORKS_DIRECTORY: &str = "this estate's works directory";
pub(crate) const RUN_PIN: &str = "a Run's pinned wirk";
pub(crate) const RUN_RESIDUE: &str = "a Run's per-harness residue";
pub(crate) const HARNESS_RESIDUE_ROOT: &str = "a harness residue root under .wirk/";
pub(crate) const RUNTIME_IMAGE: &str = "a runtime image";
pub(crate) const RUNTIME_IMAGES_ROOT: &str = "this estate's runtime image directory";
pub(crate) const WORKER_CONTRACT: &str = "a worker contract";
pub(crate) const CONTRACTS_ROOT: &str = "this estate's worker contract directory";
pub(crate) const ATLAS_GENERATION: &str = "an atlas generation";
pub(crate) const ATLAS_GENERATIONS_ROOT: &str = "this estate's atlas generation directory";
pub(crate) const ATLAS_EDITION: &str = "an atlas semantic edition";
pub(crate) const ATLAS_EDITIONS_ROOT: &str = "this estate's atlas edition directory";
pub(crate) const ATLAS_INDEX: &str =
    "this estate's atlas catalog, findings index or ownership lock";
pub(crate) const ATLAS: &str = "this estate's atlas";
pub(crate) const FINDINGS_INDEX: &str = "the findings index";
pub(crate) const ROUTE_DEFINITIONS: &str = "this estate's Route definitions";
pub(crate) const QUERY_INDEX_CACHE: &str = "the uid-shared query index cache";

/// Who still needs what, derived from records rather than guessed.
#[derive(Debug, Default)]
pub(crate) struct Retention {
    /// generation id -> the concrete consumers that retain it.
    pub(crate) generations: BTreeMap<String, Vec<String>>,
    /// edition id -> the concrete consumers that retain it.
    pub(crate) editions: BTreeMap<String, Vec<String>>,
    /// contract digest -> the concrete consumers that retain it.
    pub(crate) contracts: BTreeMap<String, Vec<String>>,
    /// Every Work this estate holds a journal for.
    pub(crate) works: Vec<WorkFacts>,
    /// Each registered source's alias and locator — named so a reader can
    /// see that the originals are outside this estate. Never measured.
    pub(crate) sources: Vec<(String, String)>,
    /// What could not be read while deriving all of the above.
    ///
    /// **This is a gate, not a footnote.** A retention set with a hole in
    /// it cannot prove any asset is unreferenced, so a cleanup refuses
    /// while this is non-empty rather than removing something whose
    /// consumer it merely failed to see.
    ///
    /// Kept as [`Unreadable`] rather than as formatted lines because
    /// these failures are made of foreign identities: a Work id, a
    /// generation id, an image digest. A scoped read is owed the gap and
    /// the reason, not the name (ruling 0260).
    pub(crate) unreadable: Vec<Unreadable>,
}

impl Retention {
    /// Whether every record the retention set depends on was read.
    pub(crate) fn complete(&self) -> bool {
        self.unreadable.is_empty()
    }

    pub(crate) fn retain(map: &mut BTreeMap<String, Vec<String>>, id: &str, consumer: String) {
        let holders = map.entry(id.to_string()).or_default();
        if !holders.contains(&consumer) {
            holders.push(consumer);
        }
    }

    fn holders(map: &BTreeMap<String, Vec<String>>, id: &str) -> Vec<String> {
        map.get(id).cloned().unwrap_or_default()
    }
}

/// Every per-Run residue directory that actually exists under this
/// estate, whatever Work it belonged to.
///
/// Read from the filesystem rather than from the Works' journals, and
/// the difference is not cosmetic: residue whose Work's journal has been
/// removed, or was never replayable, still occupies bytes and still
/// hard-links a runtime image. Enumerating from journals would have left
/// exactly that unmeasured — and, worse, would have offered the image it
/// pins as unreferenced.
fn run_residue_dirs(estate_root: &Path) -> (Vec<PathBuf>, Vec<Unreadable>) {
    let mut dirs = Vec::new();
    let mut unreadable = Vec::new();
    let wirk_dir = estate_root.join(".wirk");
    for (parent, skip) in [
        (wirk_dir.join("runtime"), Some("images")),
        (wirk_dir.join("claude"), None),
        (wirk_dir.join("opencode"), None),
    ] {
        match read_dir_names(&parent) {
            Ok(names) => {
                for name in names {
                    if Some(name.as_str()) == skip {
                        continue;
                    }
                    dirs.push(parent.join(name));
                }
            }
            Err(Some(reason)) => {
                unreadable.push(Unreadable::at(HARNESS_RESIDUE_ROOT, &parent, reason))
            }
            Err(None) => {}
        }
    }
    (dirs, unreadable)
}

/// Every `(dev, ino)` an existing Run pin file occupies.
///
/// This is what makes a runtime image removable or not, and it is a
/// *measurement of the filesystem*, not an inference from a Run's state:
/// a pin that still exists keeps its image's bytes reachable whatever
/// any journal says, and a Run whose pins were already cleaned holds
/// nothing even though its journal still names it.
fn pinned_inodes(estate_root: &Path) -> (BTreeSet<(u64, u64)>, Vec<Unreadable>) {
    use std::os::unix::fs::MetadataExt;
    let mut inodes = BTreeSet::new();
    let (dirs, mut unreadable) = run_residue_dirs(estate_root);
    for dir in dirs {
        let pinned = dir.join("bin").join("wirk");
        match std::fs::symlink_metadata(&pinned) {
            Ok(metadata) if metadata.is_file() => {
                inodes.insert((metadata.dev(), metadata.ino()));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => unreadable.push(Unreadable::at(RUN_PIN, &pinned, err.to_string())),
        }
    }
    (inodes, unreadable)
}

/// One measured, possibly removable thing inside a class.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    /// The identity an operator selects by: a generation id, an edition
    /// id, a contract digest, a Work id, an image digest.
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) measured: Measured,
    /// The concrete consumers that retain it. Empty means nothing this
    /// estate records still needs it.
    pub(crate) retained_by: Vec<String>,
}

impl Item {
    pub(crate) fn removable(&self) -> bool {
        self.retained_by.is_empty()
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "path": self.path.display().to_string(),
            "apparent_bytes": self.measured.apparent_bytes,
            "allocated_bytes": self.measured.allocated_bytes,
            "unique_allocated_bytes": self.measured.unique_allocated_bytes,
            "files": self.measured.files,
            "retained_by": self.retained_by,
            "removable": self.removable(),
            // Items are only ever rendered into an administrative
            // response, so this line is the administrative one.
            "measurement_limit": self.measured.limit_note(Disclosure::Administrative),
        })
    }
}

/// One class of owned thing, measured.
#[derive(Debug, Clone)]
pub(crate) struct ClassReport {
    pub(crate) class: &'static str,
    pub(crate) path: PathBuf,
    /// What this class is for, in one sentence, so the report is
    /// readable by someone who has not read this module.
    pub(crate) what: &'static str,
    pub(crate) measured: Measured,
    pub(crate) items: Vec<Item>,
    /// `true` when an explicit cleanup can select from this class.
    pub(crate) cleanable: bool,
    /// Why not, when it cannot.
    pub(crate) retention_rule: &'static str,
    pub(crate) soft_limit_bytes: Option<u64>,
}

impl ClassReport {
    fn over_soft_limit(&self) -> bool {
        self.soft_limit_bytes
            .is_some_and(|limit| self.measured.unique_allocated_bytes > limit)
    }

    fn to_json(&self, itemized: bool) -> Value {
        let disclosure = if itemized {
            Disclosure::Administrative
        } else {
            Disclosure::Requester
        };
        let mut value = json!({
            "class": self.class,
            "what": self.what,
            "path": self.path.display().to_string(),
            "apparent_bytes": self.measured.apparent_bytes,
            "allocated_bytes": self.measured.allocated_bytes,
            "unique_allocated_bytes": self.measured.unique_allocated_bytes,
            "entries": self.measured.files + self.measured.directories + self.measured.symlinks,
            "shared_entries": self.measured.shared_entries,
            "present": self.measured.present,
            "cleanable": self.cleanable,
            "retention_rule": self.retention_rule,
            "removable_items": self.items.iter().filter(|item| item.removable()).count(),
            "retained_items": self.items.iter().filter(|item| !item.removable()).count(),
            "soft_limit_bytes": self.soft_limit_bytes,
            "over_soft_limit": self.over_soft_limit(),
            // The class's own measurement gap is a free-form diagnostic
            // made of the same identities the rows above withhold, so it
            // is rendered at the same disclosure they are (ruling 0260).
            "measurement_limit": self.measured.limit_note(disclosure),
        });
        if itemized && let Value::Object(map) = &mut value {
            map.insert(
                "items".to_string(),
                Value::Array(self.items.iter().map(Item::to_json).collect()),
            );
        }
        value
    }
}

/// The whole answer.
#[derive(Debug)]
pub(crate) struct Survey {
    pub(crate) estate_root: PathBuf,
    pub(crate) classes: Vec<ClassReport>,
    /// Allocated bytes over every class, each inode counted once. The
    /// only figure here that adds up.
    pub(crate) estate_unique_allocated_bytes: u64,
    pub(crate) distinct_inodes: usize,
    /// Free space where the estate lives, or why it could not be read.
    pub(crate) available_bytes: Result<u64, String>,
    /// Registered sources: named, never measured, never candidates.
    pub(crate) sources: Vec<(String, String)>,
    /// Assets wirk owns that are **not** in this estate and are shared
    /// between every estate this user runs.
    pub(crate) host_shared: Vec<(String, PathBuf, Measured, &'static str)>,
    /// `true` when the shared walk budget ran out anywhere.
    pub(crate) truncated: bool,
    pub(crate) retention_complete: bool,
    pub(crate) retention_unreadable: Vec<Unreadable>,
}

impl Survey {
    pub(crate) fn class(&self, class: &str) -> Option<&ClassReport> {
        self.classes.iter().find(|report| report.class == class)
    }

    /// The report, as the daemon puts it on the wire.
    ///
    /// `itemized` is the disclosure decision, made by the caller: an
    /// administrative read names every generation, edition, contract and
    /// Work; a Work-scoped read gets the same class totals with the
    /// per-item identities withheld, because a source alias, a
    /// generation id or another Work's id are exactly the existence
    /// facts ruling 0095 removed from Work-scoped verbs.
    pub(crate) fn to_json(&self, itemized: bool) -> Value {
        let over: Vec<&str> = self
            .classes
            .iter()
            .filter(|report| report.over_soft_limit())
            .map(|report| report.class)
            .collect();
        json!({
            "estate": self.estate_root.display().to_string(),
            "scope": if itemized { "administrative" } else { "requester" },
            "classes": self.classes.iter().map(|report| report.to_json(itemized)).collect::<Vec<_>>(),
            "estate_unique_allocated_bytes": self.estate_unique_allocated_bytes,
            "distinct_inodes": self.distinct_inodes,
            "available_bytes": self.available_bytes.as_ref().ok(),
            "available_bytes_unavailable": self.available_bytes.as_ref().err(),
            "over_soft_limit": over,
            "soft_limits_are_disclosure_only":
                "being over a storage soft limit is reported and refuses nothing. Nothing here \
                 reclaims anything on its own: every removal is an explicit, guarded \
                 `wirk estate clean` or `wirk work clean`, and there is no age rule and no \
                 automatic collection",
            "sources": if itemized {
                Value::Array(self.sources.iter().map(|(alias, locator)| json!({
                    "alias": alias,
                    "locator": locator,
                    "owned": false,
                })).collect())
            } else {
                Value::Null
            },
            "sources_note":
                "a registered source's own bytes are never copied into this estate: a generation \
                 holds a manifest and a resource list, and indexed content is read live from the \
                 repository. These locators are named so the distinction is visible; they are \
                 never walked, never measured and never removable here",
            "host_shared": self.host_shared.iter().map(|(name, path, measured, note)| json!({
                "name": name,
                "path": path.display().to_string(),
                "apparent_bytes": measured.apparent_bytes,
                "allocated_bytes": measured.allocated_bytes,
                "unique_allocated_bytes": measured.unique_allocated_bytes,
                "present": measured.present,
                "charged_to_this_estate": false,
                "note": note,
            })).collect::<Vec<_>>(),
            "measurement": {
                "truncated": self.truncated,
                "walk_budget_entries": WALK_BUDGET,
                "apparent_is": "summed metadata len(); a sparse or compressed file occupies less",
                "allocated_is": "summed st_blocks * 512, once per directory entry",
                "unique_allocated_is":
                    "summed st_blocks * 512, once per (st_dev, st_ino); a Run's pinned wirk is a \
                     hard link to a shared runtime image, so only this figure adds up across \
                     classes",
                "not_a_reclaim_estimate":
                    "none of these three is a count of bytes a removal would return, and no lower \
                     bound is claimed: reflinks, shared extents, snapshots and compression on \
                     btrfs/XFS/ZFS/overlayfs all break that inference. Actual reclamation is \
                     measured per operation from the filesystem's own free space before and after",
            },
            "retention": {
                "complete": self.retention_complete,
                // Told at the caller's own disclosure. What is never
                // conditional is `complete`: the hole is the fact a
                // cleanup refuses on, and withholding *that* would be
                // the worse defect the correction must not introduce.
                "unreadable": tell_unreadable(
                    &self.retention_unreadable,
                    if itemized { Disclosure::Administrative } else { Disclosure::Requester },
                ),
                "identities_withheld": if itemized {
                    Value::Null
                } else {
                    Value::String(wirk_core::storage::IDENTITIES_WITHHELD.to_string())
                },
                "derived_from":
                    "the catalog's publications and selections, each Work's own journal and \
                     delivered projections, the findings index, and the inodes existing Run pins \
                     occupy. Never age, never a path prefix, never an ignored directory and never \
                     a stale pid",
            },
        })
    }
}

/// Measure every class this estate owns.
///
/// One [`Dedup`] threads through every class in a fixed order, so the
/// estate total charges each inode once. Per-class `unique_allocated` is
/// therefore "what this class was the first to be charged for", which is
/// exactly what a reader wants for the image/pin pair and is why the
/// class order below puts the shared image *after* the pins that link to
/// it: the image is charged to the class that actually holds the bytes
/// for as long as any pin does.
pub(crate) fn survey(
    estate_root: &Path,
    retention: &Retention,
    policy: &wirk_core::jobs::ResourcePolicy,
) -> Survey {
    let mut dedup = Dedup::new();
    let mut budget = WALK_BUDGET;
    let layout = wirk_atlas::atlas_layout(estate_root);
    let (pinned_inodes, mut pin_unreadable) = pinned_inodes(estate_root);

    let mut classes: Vec<ClassReport> = Vec::new();

    // ---- Per-Work classes -------------------------------------------
    // Journals first, and deliberately: the Trail is what every other
    // judgement here is derived from, and it is never a candidate.
    let mut journals = Measured::absent();
    let mut projections = Measured::absent();
    let mut claims = Measured::absent();
    let mut staging = Measured::absent();
    let mut worktrees = Measured::absent();
    let mut run_pins = Measured::absent();
    let mut staging_items: Vec<Item> = Vec::new();
    let mut worktree_items: Vec<Item> = Vec::new();

    for work in &retention.works {
        let work_dir = estate_root.join("works").join(&work.id);
        accumulate(
            &mut journals,
            wirk_core::storage::measure(
                &work_dir.join("journal.ndjson"),
                WORK_JOURNAL,
                &mut dedup,
                &mut budget,
            ),
        );
        accumulate(
            &mut projections,
            wirk_core::storage::measure(
                &work_dir.join("projections"),
                WORK_PROJECTIONS,
                &mut dedup,
                &mut budget,
            ),
        );
        accumulate(
            &mut claims,
            wirk_core::storage::measure(
                &work_dir.join("outputs").join("claims"),
                WORK_CLAIMS,
                &mut dedup,
                &mut budget,
            ),
        );

        // Staging is per Run, and selectable per Work.
        let staging_root = work_dir.join("outputs").join("staging");
        let this =
            wirk_core::storage::measure(&staging_root, WORK_STAGING, &mut dedup, &mut budget);
        if this.present {
            staging_items.push(Item {
                id: work.id.clone(),
                path: staging_root,
                measured: this.clone(),
                retained_by: staging_retention(work),
            });
        }
        accumulate(&mut staging, this);

        let worktree = estate_root.join("worktrees").join(&work.id);
        let this = wirk_core::storage::measure(&worktree, WORK_CHECKOUT, &mut dedup, &mut budget);
        if this.present {
            worktree_items.push(Item {
                id: work.id.clone(),
                path: worktree,
                measured: this.clone(),
                retained_by: if work.terminal {
                    Vec::new()
                } else {
                    vec![format!("work {} is not terminal", work.id)]
                },
            });
        }
        accumulate(&mut worktrees, this);
    }

    // Per-Run residue, enumerated from disk for the reason
    // `run_residue_dirs` states: bytes that are there are measured, even
    // where the Work that produced them no longer replays.
    let (residue_dirs, mut residue_unreadable) = run_residue_dirs(estate_root);
    for dir in &residue_dirs {
        accumulate(
            &mut run_pins,
            wirk_core::storage::measure(dir, RUN_RESIDUE, &mut dedup, &mut budget),
        );
    }
    pin_unreadable.append(&mut residue_unreadable);

    classes.push(ClassReport {
        class: "journals",
        path: estate_root.join("works"),
        what: "each Work's own event journal — the Trail itself",
        measured: journals,
        items: Vec::new(),
        cleanable: false,
        retention_rule: "never removable by any operation here",
        soft_limit_bytes: policy.storage_soft_limits.get("journals").copied(),
    });
    classes.push(ClassReport {
        class: "projections",
        path: estate_root.join("works"),
        what: "the delivered, immutable World each orienting Waypoint actually received",
        measured: projections,
        items: Vec::new(),
        cleanable: false,
        retention_rule:
            "never removable by any operation here: a projection is the historical World a \
             Waypoint was given, and removing it would rewrite what an actor was shown",
        soft_limit_bytes: policy.storage_soft_limits.get("projections").copied(),
    });
    classes.push(ClassReport {
        class: "outputs-claims",
        path: estate_root.join("works"),
        what: "the write-once bytes each validated Claim snapshotted",
        measured: claims,
        items: Vec::new(),
        cleanable: false,
        retention_rule:
            "never removable by any operation here: these are the artifacts a Claim was \
             validated against",
        soft_limit_bytes: policy.storage_soft_limits.get("outputs-claims").copied(),
    });
    classes.push(ClassReport {
        class: "outputs-staging",
        path: estate_root.join("works"),
        what: "each Run's own mutable output scratch, before any Claim",
        measured: staging,
        items: staging_items,
        cleanable: true,
        retention_rule:
            "retained while the Work is not terminal, and while any validated Claim recorded an \
             artifact that resolves into staging. Selected through `wirk work clean --work <id> \
             --outputs-staging`, which owns the terminal-Work and live-actor guards; a Claim's \
             own bytes live in claims/ and are a separate copy, never touched",
        soft_limit_bytes: policy.storage_soft_limits.get("outputs-staging").copied(),
    });
    classes.push(ClassReport {
        class: "worktrees",
        path: estate_root.join("worktrees"),
        what: "the git checkout each Actor World was materialized into",
        measured: worktrees,
        items: worktree_items,
        cleanable: false,
        retention_rule:
            "retained while the Work is not terminal. Removed by `wirk work clean`, which \
             additionally refuses on a live actor or pane, uncommitted or ignored content, and \
             checkout-backed Claim evidence (ruling 0228)",
        soft_limit_bytes: policy.storage_soft_limits.get("worktrees").copied(),
    });
    classes.push(ClassReport {
        class: "run-pins",
        path: estate_root.join(".wirk"),
        what: "each Run's own pinned wirk, plus its per-harness residue",
        measured: run_pins,
        items: Vec::new(),
        cleanable: false,
        retention_rule:
            "removed by `wirk work clean` together with the Work's checkout. The pinned wirk is a \
             hard link to a shared runtime image, so it is charged bytes here only where it is \
             the first holder of that inode",
        soft_limit_bytes: policy.storage_soft_limits.get("run-pins").copied(),
    });

    // ---- Runtime images ---------------------------------------------
    // After the pins, so the shared inode is charged to whichever came
    // first and never to both.
    let images_root = estate_root.join(".wirk").join("runtime").join("images");
    let mut image_items = Vec::new();
    let mut images = Measured::absent();
    match read_dir_names(&images_root) {
        Ok(digests) => {
            for digest in digests {
                let dir = images_root.join(&digest);
                let this =
                    wirk_core::storage::measure(&dir, RUNTIME_IMAGE, &mut dedup, &mut budget);
                let mut retained_by = Vec::new();
                match image_inode(&dir.join("wirk")) {
                    Ok(Some(inode)) if pinned_inodes.contains(&inode) => retained_by
                        .push("an existing Run pin shares this image's inode".to_string()),
                    Ok(_) => {}
                    Err(err) => {
                        // Could not establish whether a pin holds it:
                        // retained, because "I could not check" is never
                        // "nothing needs it".
                        retained_by.push(format!("this image's identity could not be read: {err}"));
                        pin_unreadable.push(Unreadable::at(RUNTIME_IMAGE, &dir, err.to_string()));
                    }
                }
                image_items.push(Item {
                    id: digest,
                    path: dir,
                    measured: this.clone(),
                    retained_by,
                });
                accumulate(&mut images, this);
            }
        }
        Err(Some(reason)) => {
            images
                .unreadable
                .push(Unreadable::at(RUNTIME_IMAGES_ROOT, &images_root, reason))
        }
        Err(None) => {}
    }
    classes.push(ClassReport {
        class: "runtime-images",
        path: images_root,
        what: "one content-addressed copy per distinct wirk binary that has launched an actor here",
        measured: images,
        items: image_items,
        cleanable: true,
        retention_rule:
            "retained while any existing Run pin shares its inode — measured from the filesystem, \
             not inferred from a Run's state. A pin keeps its bytes reachable whether or not the \
             image directory remains, so removing an unreferenced image never breaks a live pin",
        soft_limit_bytes: policy.storage_soft_limits.get("runtime-images").copied(),
    });

    // ---- Worker contracts -------------------------------------------
    let contracts_root = wirk_herdr::worker_contract::contracts_dir(estate_root);
    let mut contract_items = Vec::new();
    let mut contracts = Measured::absent();
    match read_dir_names(&contracts_root) {
        Ok(names) => {
            for name in names {
                let path = contracts_root.join(&name);
                let this =
                    wirk_core::storage::measure(&path, WORKER_CONTRACT, &mut dedup, &mut budget);
                let digest = name.strip_suffix(".md").unwrap_or(&name).to_string();
                contract_items.push(Item {
                    retained_by: Retention::holders(&retention.contracts, &digest),
                    id: digest,
                    path,
                    measured: this.clone(),
                });
                accumulate(&mut contracts, this);
            }
        }
        Err(Some(reason)) => {
            contracts
                .unreadable
                .push(Unreadable::at(CONTRACTS_ROOT, &contracts_root, reason))
        }
        Err(None) => {}
    }
    classes.push(ClassReport {
        class: "contracts",
        path: contracts_root,
        what: "the content-addressed worker contract each reserved Actor World names",
        measured: contracts,
        items: contract_items,
        cleanable: true,
        retention_rule:
            "retained while a non-terminal Work reserves it, and always for this build's own \
             digest. A finished Work's journal still names its digest afterwards and stays \
             readable: the reference is a name, not a byte requirement",
        soft_limit_bytes: policy.storage_soft_limits.get("contracts").copied(),
    });

    // ---- Atlas ------------------------------------------------------
    let mut generation_items = Vec::new();
    let mut generations = Measured::absent();
    match read_dir_names(&layout.generations) {
        Ok(ids) => {
            for id in ids {
                let path = layout.generations.join(&id);
                let this =
                    wirk_core::storage::measure(&path, ATLAS_GENERATION, &mut dedup, &mut budget);
                generation_items.push(Item {
                    retained_by: Retention::holders(&retention.generations, &id),
                    id,
                    path,
                    measured: this.clone(),
                });
                accumulate(&mut generations, this);
            }
        }
        Err(Some(reason)) => generations.unreadable.push(Unreadable::at(
            ATLAS_GENERATIONS_ROOT,
            &layout.generations,
            reason,
        )),
        Err(None) => {}
    }
    classes.push(ClassReport {
        class: "atlas-generations",
        path: layout.generations.clone(),
        what: "one immutable manifest and resource list per acquired generation (no source bytes)",
        measured: generations,
        items: generation_items,
        cleanable: true,
        retention_rule:
            "retained while it is a membership's published generation, while a non-terminal \
             Work's projection names it, and while an unsettled finding was recorded against it. \
             A finished Work's projection names it without requiring its bytes: after an explicit \
             removal that Work's journal and projection are byte-unchanged, and a later \
             `world expand` reports generation_unavailable with degraded coverage",
        soft_limit_bytes: policy.storage_soft_limits.get("atlas-generations").copied(),
    });

    let mut edition_items = Vec::new();
    let mut editions = Measured::absent();
    match read_dir_names(&layout.editions) {
        Ok(ids) => {
            for id in ids {
                let path = layout.editions.join(&id);
                let this =
                    wirk_core::storage::measure(&path, ATLAS_EDITION, &mut dedup, &mut budget);
                edition_items.push(Item {
                    retained_by: Retention::holders(&retention.editions, &id),
                    id,
                    path,
                    measured: this.clone(),
                });
                accumulate(&mut editions, this);
            }
        }
        Err(Some(reason)) => editions.unreadable.push(Unreadable::at(
            ATLAS_EDITIONS_ROOT,
            &layout.editions,
            reason,
        )),
        Err(None) => {}
    }
    classes.push(ClassReport {
        class: "atlas-editions",
        path: layout.editions.clone(),
        what: "one vector store per built semantic edition",
        measured: editions,
        items: edition_items,
        cleanable: true,
        retention_rule:
            "retained while it is a membership's selected edition. A superseded edition is an \
             optional ranking asset: removing it changes how a query ranks, not which generation \
             a World resolves against, so it is never on its own a reason for \
             generation_unavailable",
        soft_limit_bytes: policy.storage_soft_limits.get("atlas-editions").copied(),
    });

    let mut index = Measured::absent();
    for path in [&layout.catalog, &layout.findings_index, &layout.owner_lock] {
        accumulate(
            &mut index,
            wirk_core::storage::measure(path, ATLAS_INDEX, &mut dedup, &mut budget),
        );
    }
    classes.push(ClassReport {
        class: "atlas-index",
        path: layout.root.clone(),
        what: "the catalog, the derived findings index and this estate's atlas ownership lock",
        measured: index,
        items: Vec::new(),
        cleanable: false,
        retention_rule: "never removable by any operation here",
        soft_limit_bytes: policy.storage_soft_limits.get("atlas-index").copied(),
    });

    let routes_root = estate_root.join("routes");
    let routes =
        wirk_core::storage::measure(&routes_root, ROUTE_DEFINITIONS, &mut dedup, &mut budget);
    classes.push(ClassReport {
        class: "routes",
        path: routes_root,
        what: "Route definitions, authored by the operator — an input, not a derivation",
        measured: routes,
        items: Vec::new(),
        cleanable: false,
        retention_rule: "authored input: never removable by any operation here",
        soft_limit_bytes: policy.storage_soft_limits.get("routes").copied(),
    });

    // ---- Not this estate's, and said so ------------------------------
    let mut host_dedup = Dedup::new();
    let mut host_budget = WALK_BUDGET;
    let cache_root = wirk_atlas::query_index_cache_root();
    let cache = wirk_core::storage::measure(
        &cache_root,
        QUERY_INDEX_CACHE,
        &mut host_dedup,
        &mut host_budget,
    );
    let host_shared = vec![(
        "query-index-cache".to_string(),
        cache_root,
        cache,
        "shared by every estate this uid runs, outside any estate, and already self-bounded: the \
         query path keeps its newest entries and prunes the rest on each use. Not charged to this \
         estate and not removable through it — no estate has the authority to collect another's \
         reuse, and each entry is reproducible from its own view anyway",
    )];

    let truncated = classes.iter().any(|report| report.measured.truncated);
    let mut retention_unreadable = retention.unreadable.clone();
    retention_unreadable.append(&mut pin_unreadable);

    Survey {
        estate_unique_allocated_bytes: classes
            .iter()
            .map(|report| report.measured.unique_allocated_bytes)
            .fold(0u64, u64::saturating_add),
        distinct_inodes: dedup.distinct_inodes(),
        available_bytes: wirk_core::jobs::available_space_bytes(estate_root),
        estate_root: estate_root.to_path_buf(),
        classes,
        sources: retention.sources.clone(),
        host_shared,
        truncated,
        retention_complete: retention_unreadable.is_empty(),
        retention_unreadable,
    }
}

/// Why a Work's staging area is still needed, if it is.
fn staging_retention(work: &WorkFacts) -> Vec<String> {
    let mut retained = Vec::new();
    if !work.terminal {
        retained.push(format!("work {} is not terminal", work.id));
    }
    // A validated Claim whose recorded managed path resolves into
    // staging rather than into `claims/`. The ordinary path never does —
    // `store_claimed_bytes` snapshots an independent copy — so this is a
    // guard against a receipt shaped otherwise, not an expected case.
    for stored in &work.validated_managed_paths {
        if !stored.starts_with("claims/") {
            retained.push(format!(
                "a validated Claim of work {} recorded the managed artifact {stored}, which does \
                 not resolve into claims/",
                work.id
            ));
        }
    }
    if work.claim_evidence_in_checkout {
        retained.push(format!(
            "a validated Claim of work {} recorded evidence in the checkout",
            work.id
        ));
    }
    retained
}

fn accumulate(into: &mut Measured, other: Measured) {
    into.apparent_bytes = into.apparent_bytes.saturating_add(other.apparent_bytes);
    into.allocated_bytes = into.allocated_bytes.saturating_add(other.allocated_bytes);
    into.unique_allocated_bytes = into
        .unique_allocated_bytes
        .saturating_add(other.unique_allocated_bytes);
    into.files += other.files;
    into.directories += other.directories;
    into.symlinks += other.symlinks;
    into.shared_entries += other.shared_entries;
    into.present |= other.present;
    into.truncated |= other.truncated;
    into.unreadable.extend(other.unreadable);
}

/// The immediate entry names under `path`, sorted.
///
/// `Err(None)` is "there is no such directory", an ordinary zero.
/// `Err(Some(reason))` is "it is there and could not be read", which is a
/// gap a caller must disclose.
fn read_dir_names(path: &Path) -> Result<Vec<String>, Option<String>> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(None),
        // The reason alone. The path is supplied by the caller, which
        // knows the category it belongs to, so that one failure can be
        // told at either disclosure (ruling 0260).
        Err(err) => return Err(Some(err.to_string())),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // A private temporary a live writer is still filling. Never a
        // candidate and never an item: increment B's own store sweep
        // owns those, under the ownership lock.
        if name.starts_with('.') {
            continue;
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}

fn image_inode(path: &Path) -> Result<Option<(u64, u64)>, String> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some((metadata.dev(), metadata.ino()))),
        Ok(_) => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}
