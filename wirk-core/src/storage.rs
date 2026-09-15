//! What an estate's own derivations actually occupy, measured rather
//! than asserted (P4.5 increment A, ruling 0256).
//!
//! Three numbers, and the difference between them is the whole point:
//!
//! * **apparent** — `metadata.len()` summed. What the files say they
//!   are. A sparse file, a hole-punched image or a compressed extent all
//!   report their logical size here and occupy less.
//! * **allocated** — `st_blocks * 512` summed. What the filesystem says
//!   it handed out, counted once per directory entry.
//! * **unique allocated** — the same, counted once per `(st_dev,
//!   st_ino)`. A Run's pinned `wirk` is a *hard link* to the estate's
//!   content-addressed runtime image (`wirk_herdr::bind_runtime_image`),
//!   so 135 Runs pinning one 95 MiB image occupy 95 MiB, not 12 GiB.
//!   Charging the full size to every holder is the arithmetic this
//!   module exists to refuse.
//!
//! **None of the three is a reclaimable-byte count, and this module
//! never claims one.** Removing a name returns its blocks only when no
//! other name and no open descriptor holds the inode; on btrfs, XFS,
//! ZFS and overlayfs a reflink, a snapshot or a shared extent can make
//! removal return less than `st_blocks` suggests, and transparent
//! compression can make `st_blocks` smaller than what a rewrite would
//! need. There is no lower bound to state here, so none is stated. What
//! *is* measurable is the filesystem's own free-space figure before and
//! after a removal, which is why [`crate::jobs::available_space_bytes`]
//! is what a cleanup reports as `reclaimed_observed_bytes` — approximate
//! in its own right, because concurrent activity moves it.
//!
//! **Symlinks are never followed.** `DirEntry::metadata` is
//! `lstat`-shaped, so a link out of the estate is counted as the link it
//! is (a few bytes) and never as the tree it points at. An inventory
//! that followed one would leave the estate, which is exactly the
//! traversal ruling 0256 bounds.
//!
//! **A measurement that could not finish says so.** The walk is bounded
//! by an entry budget and records every unreadable path, so a partial
//! answer is reported as partial rather than as a small total.
//!
//! **And it says so at the caller's own disclosure scope.** An
//! unreadable path is a *diagnostic*, but it is made of the same
//! identities an ordinary row withholds: `works/<work id>/projections`
//! names another Work, `generations/<id>` names a generation. So a
//! failure is recorded as a [`Unreadable`] — the category it belongs to,
//! the path, and the reason as three separate fields — and rendered
//! twice: [`Disclosure::Administrative`] gets the path, and
//! [`Disclosure::Requester`] gets the category and the reason without
//! it. Neither rendering drops the failure, because the incompleteness
//! is the fact a cleanup refuses on (ruling 0260).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Entries one measurement may visit before it stops and says it
/// stopped.
///
/// A resource report must not become its own expensive job — the same
/// reason `wirk run`'s materialization estimate is bounded. Generous
/// enough that an ordinary estate finishes well inside it, and finite so
/// a pathological tree cannot hang the verb.
pub const WALK_BUDGET: u32 = 200_000;

/// Every class of thing this estate owns, as one vocabulary.
///
/// Named here, in the crate both the daemon and the policy loader
/// depend on, so a limit configured for a class that does not exist is
/// *reported* rather than silently ignored — the same discipline
/// `resources.json`'s `deny_unknown_fields` already applies to policy
/// keys (P4.5 B's own config defect).
///
/// The list deliberately includes classes that are **never** removable
/// (a journal, a retained Claim's bytes, an authored Route). An
/// inventory whose vocabulary only covered what can be deleted would
/// invite the reader to conclude that everything it names is disposable.
pub const CLASSES: &[&str] = &[
    "worktrees",
    "outputs-staging",
    "outputs-claims",
    "projections",
    "journals",
    "run-pins",
    "runtime-images",
    "contracts",
    "doctrine",
    "atlas-generations",
    "atlas-editions",
    "atlas-index",
    "routes",
];

/// The classes an explicit, guarded cleanup can select.
///
/// `outputs-staging` is here because it is selectable, but it is reached
/// through `wirk work clean --outputs-staging`, which already owns the
/// terminal-Work and live-actor guards (ruling 0228) — not through the
/// estate-wide verb, which has no Work to check those against.
pub const CLEANABLE_CLASSES: &[&str] = &[
    "outputs-staging",
    "runtime-images",
    "contracts",
    "doctrine",
    "atlas-generations",
    "atlas-editions",
];

/// Whether `name` is a class this estate measures.
pub fn is_class(name: &str) -> bool {
    CLASSES.contains(&name)
}

/// Whether `name` is a class an explicit cleanup may select.
pub fn is_cleanable_class(name: &str) -> bool {
    CLEANABLE_CLASSES.contains(&name)
}

/// The set of `(st_dev, st_ino)` a measurement has already charged.
///
/// Shared deliberately: the same value threaded through several
/// measurements makes the *second* holder of a hard-linked inode
/// contribute zero unique bytes, which is what makes an estate total
/// add up instead of double-counting every runtime image.
#[derive(Debug, Default)]
pub struct Dedup {
    seen: HashSet<(u64, u64)>,
}

impl Dedup {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` the first time this inode is offered, `false` afterwards.
    fn first_sight(&mut self, dev: u64, ino: u64) -> bool {
        self.seen.insert((dev, ino))
    }

    pub fn distinct_inodes(&self) -> usize {
        self.seen.len()
    }
}

/// Who is being told, and therefore how much of an identity a
/// diagnostic may carry.
///
/// This is the same decision `Survey::to_json`'s `itemized` makes about
/// ordinary rows, named once so the failure paths make it the same way
/// rather than each inventing an answer (ruling 0260).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disclosure {
    /// An operator who named `--admin`, or who has no Work identity of
    /// their own. Gets every path, because repairing an unreadable path
    /// requires knowing which one it is.
    Administrative,
    /// A Work-scoped caller. Gets the category and the reason; the
    /// identity — which Work, which source, which generation, which
    /// image — is withheld exactly as ruling 0095 withholds it from the
    /// ordinary rows.
    Requester,
}

/// One thing that could not be read, kept in parts so it can be told at
/// either [`Disclosure`].
///
/// `kind` is a fixed category written by the code that knew what it was
/// reading — never derived from the path, because deriving it would put
/// the identity back. `reason` is the underlying error, which for an OS
/// error and for a parse error alike names no path of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// What kind of record this was, in a reader's words. Safe at every
    /// scope: it names a category, never which member of it.
    pub kind: &'static str,
    /// Exactly what could not be read. `None` where the failure is not
    /// about one path at all.
    pub path: Option<PathBuf>,
    /// Why, with no path of its own.
    pub reason: String,
}

impl Unreadable {
    /// A failure at a concrete path.
    pub fn at(kind: &'static str, path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self {
            kind,
            path: Some(path.into()),
            reason: reason.into(),
        }
    }

    /// A failure with no single path behind it — a held lock, a whole
    /// index.
    pub fn of(kind: &'static str, reason: impl Into<String>) -> Self {
        Self {
            kind,
            path: None,
            reason: reason.into(),
        }
    }

    /// The administrative rendering: the path, then the reason.
    pub fn administrative(&self) -> String {
        match &self.path {
            Some(path) => format!("{}: {}", path.display(), self.reason),
            None => self.reason.clone(),
        }
    }

    /// The scoped rendering: the category, then the reason. Truthful
    /// about what failed and why; silent about which one it was.
    pub fn scoped(&self) -> String {
        format!("{}: {}", self.kind, self.reason)
    }

    /// One line at `disclosure`.
    pub fn tell(&self, disclosure: Disclosure) -> String {
        match disclosure {
            Disclosure::Administrative => self.administrative(),
            Disclosure::Requester => self.scoped(),
        }
    }
}

/// Render `unreadable` at `disclosure`.
///
/// At requester scope identical `(kind, reason)` pairs are folded into
/// one line with a count, because without the paths to tell them apart
/// twelve repetitions of the same sentence would be noise, not
/// disclosure. The count is kept: how much is unreadable is itself the
/// useful part.
pub fn tell_unreadable(unreadable: &[Unreadable], disclosure: Disclosure) -> Vec<String> {
    match disclosure {
        Disclosure::Administrative => unreadable.iter().map(Unreadable::administrative).collect(),
        Disclosure::Requester => {
            let mut folded: Vec<(String, usize)> = Vec::new();
            for entry in unreadable {
                let line = entry.scoped();
                match folded.iter_mut().find(|(seen, _)| seen == &line) {
                    Some((_, count)) => *count += 1,
                    None => folded.push((line, 1)),
                }
            }
            folded
                .into_iter()
                .map(|(line, count)| {
                    if count == 1 {
                        line
                    } else {
                        format!("{count} of: {line}")
                    }
                })
                .collect()
        }
    }
}

/// What a scoped caller is told *instead of* the identities, so the
/// withholding is visible as a withholding rather than read as an
/// absence.
pub const IDENTITIES_WITHHELD: &str = "which Work, source, generation, edition or image each of these was is withheld from a \
     Work-scoped read, the same existence facts ruling 0095 removes from the ordinary rows. The \
     failure, its category and its reason are not withheld, because a cleanup refuses on exactly \
     this incompleteness. An operator with the authority to repair it reads the paths through \
     `wirk estate storage --admin`";

/// One measured subtree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Measured {
    /// Summed `metadata.len()`.
    pub apparent_bytes: u64,
    /// Summed `st_blocks * 512`, once per directory entry.
    pub allocated_bytes: u64,
    /// Summed `st_blocks * 512`, once per `(st_dev, st_ino)` — the
    /// figure that does not charge a shared inode to every holder.
    pub unique_allocated_bytes: u64,
    pub files: u64,
    pub directories: u64,
    /// Counted, never followed.
    pub symlinks: u64,
    /// Entries whose own inode was already charged by an earlier
    /// measurement sharing this [`Dedup`] — reported so "unique is
    /// smaller than allocated" is explained rather than merely true.
    pub shared_entries: u64,
    /// Whether the path existed at all. A class with no directory yet is
    /// an ordinary, honest zero — not an error.
    pub present: bool,
    /// What could not be read, with the reason. Never silently dropped:
    /// a total that skipped something says which something — at the
    /// disclosure the reader is entitled to, which is why these are
    /// kept in parts rather than as formatted lines.
    pub unreadable: Vec<Unreadable>,
    /// `true` when the entry budget ran out before the walk finished, so
    /// every number above is a floor rather than a total.
    pub truncated: bool,
}

impl Measured {
    /// An absent path: present `false`, everything zero, nothing
    /// unreadable. Distinct from a failed read, which lands in
    /// `unreadable`.
    pub fn absent() -> Self {
        Self::default()
    }

    /// Whether any number here is known to be incomplete.
    pub fn partial(&self) -> bool {
        self.truncated || !self.unreadable.is_empty()
    }

    /// A one-line statement of this measurement's own limits, or `None`
    /// when it completed cleanly. Written for a reader, and never
    /// implying more precision than the walk achieved.
    ///
    /// `disclosure` decides how much of each failure's identity the line
    /// carries. The *existence* of the limit, and the count, are the
    /// same at either scope: a scoped caller whose totals are floors
    /// must be told they are floors (ruling 0260).
    pub fn limit_note(&self, disclosure: Disclosure) -> Option<String> {
        if !self.partial() {
            return None;
        }
        let mut parts = Vec::new();
        if self.truncated {
            parts.push(format!(
                "the {WALK_BUDGET}-entry walk budget was reached, so these figures are floors, \
                 not totals"
            ));
        }
        if !self.unreadable.is_empty() {
            let told = tell_unreadable(&self.unreadable, disclosure);
            parts.push(format!(
                "{} path(s) could not be read and contribute nothing: {}",
                self.unreadable.len(),
                told.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
            ));
        }
        Some(parts.join("; "))
    }

    fn charge(&mut self, metadata: &std::fs::Metadata, dedup: &mut Dedup) {
        use std::os::unix::fs::MetadataExt;
        let allocated = metadata.blocks().saturating_mul(512);
        self.apparent_bytes = self.apparent_bytes.saturating_add(metadata.len());
        self.allocated_bytes = self.allocated_bytes.saturating_add(allocated);
        if dedup.first_sight(metadata.dev(), metadata.ino()) {
            self.unique_allocated_bytes = self.unique_allocated_bytes.saturating_add(allocated);
        } else {
            self.shared_entries += 1;
        }
    }
}

/// Measure `path`, charging shared inodes once against `dedup`.
///
/// `budget` is decremented across calls so a report measuring many
/// classes is bounded as a whole, not per class. A directory that is not
/// there yields [`Measured::absent`]; a directory that is there but
/// unreadable yields a `Measured` whose `unreadable` names it, because
/// those are different facts and a report that conflated them would
/// under-report an estate rather than disclose a gap.
pub fn measure(path: &Path, kind: &'static str, dedup: &mut Dedup, budget: &mut u32) -> Measured {
    let mut measured = Measured::absent();
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        // `NotFound` is the ordinary "this class has produced nothing
        // yet" case and is not a gap in the measurement.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return measured,
        Err(err) => {
            measured
                .unreadable
                .push(Unreadable::at(kind, path, err.to_string()));
            return measured;
        }
    };
    measured.present = true;
    if metadata.is_symlink() {
        measured.symlinks += 1;
        measured.charge(&metadata, dedup);
        return measured;
    }
    if metadata.is_file() {
        measured.files += 1;
        measured.charge(&metadata, dedup);
        return measured;
    }
    measured.directories += 1;
    measured.charge(&metadata, dedup);
    walk(path, kind, &mut measured, dedup, budget);
    measured
}

fn walk(
    directory: &Path,
    kind: &'static str,
    measured: &mut Measured,
    dedup: &mut Dedup,
    budget: &mut u32,
) {
    if *budget == 0 {
        measured.truncated = true;
        return;
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) => {
            measured
                .unreadable
                .push(Unreadable::at(kind, directory, err.to_string()));
            return;
        }
    };
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in entries {
        if *budget == 0 {
            measured.truncated = true;
            return;
        }
        *budget -= 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                measured
                    .unreadable
                    .push(Unreadable::at(kind, directory, err.to_string()));
                continue;
            }
        };
        // `DirEntry::metadata` does not traverse a symlink, which is the
        // property this walk depends on: a link pointing out of the
        // estate is charged as the link and never as its target.
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                measured
                    .unreadable
                    .push(Unreadable::at(kind, entry.path(), err.to_string()));
                continue;
            }
        };
        if metadata.is_symlink() {
            measured.symlinks += 1;
        } else if metadata.is_dir() {
            measured.directories += 1;
            children.push(entry.path());
        } else {
            measured.files += 1;
        }
        measured.charge(&metadata, dedup);
    }
    // Iterative in the child dimension, recursive in depth: the read_dir
    // handle is dropped before descending, so a wide estate does not
    // hold one open descriptor per level of a deep one.
    for child in children {
        walk(&child, kind, measured, dedup, budget);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wirk-storage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn an_absent_path_is_an_honest_zero_not_an_error() {
        let mut dedup = Dedup::new();
        let mut budget = WALK_BUDGET;
        let measured = measure(
            Path::new("/nonexistent-wirk-storage"),
            "a test path",
            &mut dedup,
            &mut budget,
        );
        assert!(!measured.present);
        assert!(measured.unreadable.is_empty());
        assert_eq!(measured.apparent_bytes, 0);
        assert!(!measured.partial());
    }

    #[test]
    fn a_hard_link_is_charged_once_across_measurements() {
        let root = temp();
        let image = root.join("image");
        let pin = root.join("pin");
        std::fs::create_dir_all(&image).expect("image dir");
        std::fs::create_dir_all(&pin).expect("pin dir");
        std::fs::write(image.join("wirk"), vec![7u8; 64 * 1024]).expect("image bytes");
        std::fs::hard_link(image.join("wirk"), pin.join("wirk")).expect("hard link");

        let mut dedup = Dedup::new();
        let mut budget = WALK_BUDGET;
        let first = measure(&image, "a runtime image", &mut dedup, &mut budget);
        let second = measure(&pin, "a Run's pinned wirk", &mut dedup, &mut budget);

        // Both see the linked bytes (each measurement also charges its
        // own directory inode, which is why this is `>=`, not `==`).
        assert!(first.apparent_bytes >= 64 * 1024);
        assert!(second.apparent_bytes >= 64 * 1024);
        assert!(first.unique_allocated_bytes >= 64 * 1024);
        assert_eq!(second.files, 1);
        assert_eq!(
            second.shared_entries, 1,
            "the linked file must be recognised as an inode already charged"
        );
        // The directory's own inode is new to the dedup set, so the
        // second measurement's unique figure is the directory alone —
        // the 64 KiB file contributes nothing to it.
        assert!(
            second.unique_allocated_bytes < 64 * 1024,
            "the second holder of one inode must not be charged its bytes again: {}",
            second.unique_allocated_bytes
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_symlink_out_of_the_tree_is_counted_never_followed() {
        let root = temp();
        let inside = root.join("inside");
        let outside = root.join("outside");
        std::fs::create_dir_all(&inside).expect("inside");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("big"), vec![3u8; 128 * 1024]).expect("outside bytes");
        std::os::unix::fs::symlink(&outside, inside.join("escape")).expect("symlink");

        let mut dedup = Dedup::new();
        let mut budget = WALK_BUDGET;
        let measured = measure(&inside, "a test path", &mut dedup, &mut budget);

        assert_eq!(measured.symlinks, 1);
        assert_eq!(measured.files, 0, "the escaped tree must not be walked");
        assert!(
            measured.apparent_bytes < 128 * 1024,
            "following the link would have charged the outside tree: {}",
            measured.apparent_bytes
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_budget_exhaustion_is_disclosed_rather_than_reported_as_a_total() {
        let root = temp();
        for index in 0..24 {
            std::fs::write(root.join(format!("f{index}")), b"x").expect("file");
        }
        let mut dedup = Dedup::new();
        let mut budget = 6u32;
        let measured = measure(&root, "a test path", &mut dedup, &mut budget);
        assert!(measured.truncated);
        assert!(measured.partial());
        assert!(
            measured
                .limit_note(Disclosure::Administrative)
                .unwrap_or_default()
                .contains("floors, not totals")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Ruling 0260. The same failure, told twice: the operator who has
    /// to go and fix it gets the path; the Work-scoped caller gets the
    /// category and the reason, and still gets told there is a gap.
    #[test]
    fn one_unreadable_is_told_with_its_path_administratively_and_without_it_at_requester_scope() {
        let unreadable = vec![
            Unreadable::at(
                "a Work's own journal",
                "/estate/works/work-1a2b3c-7",
                "could not be replayed, so what it still references is unknown",
            ),
            Unreadable::at(
                "a Work's own journal",
                "/estate/works/work-9z8y7x-2",
                "could not be replayed, so what it still references is unknown",
            ),
            Unreadable::of("this estate's atlas", "is held by a running job"),
        ];

        let administrative = tell_unreadable(&unreadable, Disclosure::Administrative);
        assert_eq!(administrative.len(), 3, "no failure is folded away here");
        assert!(administrative[0].contains("/estate/works/work-1a2b3c-7"));
        assert!(administrative[1].contains("/estate/works/work-9z8y7x-2"));
        assert_eq!(administrative[2], "is held by a running job");

        let scoped = tell_unreadable(&unreadable, Disclosure::Requester);
        let joined = scoped.join(" | ");
        assert!(
            !joined.contains("work-1a2b3c-7") && !joined.contains("work-9z8y7x-2"),
            "a scoped rendering must carry no foreign identity: {joined}"
        );
        assert!(
            joined.contains("could not be replayed"),
            "the reason is not what is withheld: {joined}"
        );
        assert_eq!(
            scoped.len(),
            2,
            "two identical categories fold into one counted line: {scoped:?}"
        );
        assert!(
            scoped[0].starts_with("2 of: "),
            "the count survives the fold: {scoped:?}"
        );
    }

    /// The failure itself is never withheld — only which one it was. A
    /// scoped `limit_note` still says the totals are incomplete.
    #[test]
    fn a_scoped_limit_note_still_discloses_that_the_measurement_is_incomplete() {
        let measured = Measured {
            unreadable: vec![Unreadable::at(
                "a Work's delivered projections",
                "/estate/works/work-secret-3/projections",
                "Permission denied (os error 13)",
            )],
            ..Measured::absent()
        };
        assert!(measured.partial());
        let scoped = measured
            .limit_note(Disclosure::Requester)
            .expect("a partial measurement has a note at every scope");
        assert!(scoped.contains("1 path(s) could not be read"), "{scoped}");
        assert!(scoped.contains("Permission denied"), "{scoped}");
        assert!(!scoped.contains("work-secret-3"), "{scoped}");
        assert!(
            measured
                .limit_note(Disclosure::Administrative)
                .expect("note")
                .contains("work-secret-3"),
            "the administrative note keeps the path"
        );
    }
}
