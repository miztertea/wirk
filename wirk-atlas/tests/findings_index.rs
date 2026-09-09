//! `wirk-atlas`'s own Findings index (W-B, `findings.rs`): append/read/
//! dedup/rebuild/malformed-line/crash-recovery against a
//! directly-constructed `AtlasStore` — no daemon, no journal. The
//! server-side journal-derivation and settlement policy live in
//! `wirk/tests/findings.rs`.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::Command;

use tempfile::TempDir;
use wirk_atlas::{AtlasStore, FindingOrigin, FindingRow, FindingRowKind, IndexBacking};
use wirk_core::{
    AdmittedEvidence, ApplicationRef, Assertion, Attribution, ClaimId, Decision, EventId, Finding,
    FindingId, FindingKind, FindingScope, GenerationPoint, ObligationRef, PeerIdentity, RunId,
    Settlement, SettlementAuthority, SettlementCheck, SettlementClass, Timestamp, WaypointId,
    WorkId, WorldHash,
};

fn finding(id: &str) -> Finding {
    Finding {
        id: FindingId(id.to_string()),
        work: WorkId("work-1".to_string()),
        run: RunId("run-1".to_string()),
        waypoint: WaypointId("leaf".to_string()),
        kind: FindingKind::VerifiedOutcome,
        scope: FindingScope::EstateLocal,
        claim: "the deterministic leaf ran".to_string(),
        evidence: Vec::<AdmittedEvidence>::new(),
        contradicts: Vec::new(),
        applies_to: Vec::new(),
        supersedes: None,
        proposed_change: None,
        obligation: Some(ObligationRef {
            id: "out-produced".to_string(),
            edition: "1".to_string(),
        }),
        confirmed_by: None,
    }
}

fn settlement() -> Settlement {
    Settlement {
        authority: SettlementAuthority {
            class: SettlementClass::DeterministicVerified,
            policy_version: 1,
            policy_digest: "deadbeef".to_string(),
        },
        check: SettlementCheck::ValidatedClaim {
            work: WorkId("work-1".to_string()),
            claim: ClaimId("claim-1".to_string()),
            claim_event: EventId("e-claim".to_string()),
            proof: Some(wirk_core::DeterministicProof {
                obligation: ObligationRef {
                    id: "out-produced".to_string(),
                    edition: "1".to_string(),
                },
                basis: "basis-hash".to_string(),
                proves: "the leaf's command ran and produced out.md".to_string(),
                waypoint: WaypointId("leaf".to_string()),
                attempt: 1,
                world_hash: WorldHash("world-hash".to_string()),
                artifacts: Vec::new(),
            }),
            unread: Default::default(),
        },
        settled_by: EventId("e-claim".to_string()),
        at: Timestamp(1),
        minted_at_startup: false,
    }
}

fn settled_row(finding_id: &str) -> FindingRow {
    FindingRow {
        id: wirk_atlas::FindingRowId::compute(
            &FindingId(finding_id.to_string()),
            FindingRowKind::Settled,
            &EventId("e-settled".to_string()),
        ),
        kind: FindingRowKind::Settled,
        finding: finding(finding_id),
        origin: FindingOrigin {
            work: WorkId("work-1".to_string()),
            raised_event: EventId("e-find".to_string()),
            row_event: EventId("e-settled".to_string()),
        },
        settlement: Some(settlement()),
        assertion: None,
        applied: None,
        superseded_by: None,
    }
}

#[test]
fn append_then_read_round_trips_a_settled_row() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let row = settled_row("finding-1");
    atlas.append_finding_row(&row).unwrap();
    let rows = atlas.findings().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], row);
}

#[test]
fn append_is_idempotent_by_content_addressed_id() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let row = settled_row("finding-1");
    atlas.append_finding_row(&row).unwrap();
    atlas.append_finding_row(&row).unwrap();
    assert_eq!(
        atlas.findings().unwrap().len(),
        1,
        "retry must not duplicate"
    );
}

#[test]
fn distinct_rows_for_the_same_finding_both_persist() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();
    let asserted = FindingRow {
        id: wirk_atlas::FindingRowId::compute(
            &FindingId("finding-1".to_string()),
            FindingRowKind::Asserted,
            &EventId("e-assert".to_string()),
        ),
        kind: FindingRowKind::Asserted,
        finding: finding("finding-1"),
        origin: FindingOrigin {
            work: WorkId("work-1".to_string()),
            raised_event: EventId("e-find".to_string()),
            row_event: EventId("e-assert".to_string()),
        },
        settlement: None,
        assertion: Some(Assertion {
            decision: Decision::Accepted,
            by: "root".to_string(),
            reason: None,
            peer: PeerIdentity {
                uid: 1000,
                gid: 1000,
            },
            at: Timestamp(2),
            author: None,
        }),
        applied: None,
        superseded_by: None,
    };
    atlas.append_finding_row(&asserted).unwrap();
    assert_eq!(atlas.findings().unwrap().len(), 2);
}

#[test]
fn applied_row_carries_both_generations_and_the_asserted_judgement() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let applied = FindingRow {
        id: wirk_atlas::FindingRowId::compute(
            &FindingId("finding-1".to_string()),
            FindingRowKind::Applied,
            &EventId("e-applied".to_string()),
        ),
        kind: FindingRowKind::Applied,
        finding: finding("finding-1"),
        origin: FindingOrigin {
            work: WorkId("work-1".to_string()),
            raised_event: EventId("e-find".to_string()),
            row_event: EventId("e-applied".to_string()),
        },
        settlement: None,
        assertion: None,
        applied: Some(ApplicationRef {
            source: "wirk".to_string(),
            before: GenerationPoint {
                generation: "g-before".to_string(),
                object_id: Some("obj-before".to_string()),
            },
            after: GenerationPoint {
                generation: "g-after".to_string(),
                object_id: Some("obj-after".to_string()),
            },
            revision: "deadbeef".to_string(),
            attribution: Attribution::Asserted {
                by: "root".to_string(),
                peer: PeerIdentity {
                    uid: 1000,
                    gid: 1000,
                },
                producer: wirk_core::ApplicationProducer {
                    work: WorkId("work-1".to_string()),
                    run: RunId("run-1".to_string()),
                    world_hash: wirk_core::WorldHash("hash".to_string()),
                },
            },
            implements_finding: wirk_core::AssertedJudgement {
                by: "root".to_string(),
                peer: PeerIdentity {
                    uid: 1000,
                    gid: 1000,
                },
                at: Timestamp(3),
            },
        }),
        superseded_by: None,
    };
    atlas.append_finding_row(&applied).unwrap();
    let rows = atlas.findings().unwrap();
    let stored = rows[0].applied.as_ref().unwrap();
    assert_eq!(stored.before.generation, "g-before");
    assert_eq!(stored.after.generation, "g-after");
    assert_ne!(stored.before.object_id, stored.after.object_id);
}

#[test]
fn rebuild_replaces_the_whole_file() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();
    atlas.append_finding_row(&settled_row("finding-2")).unwrap();
    assert_eq!(atlas.findings().unwrap().len(), 2);

    atlas
        .rebuild_finding_rows(vec![settled_row("finding-1")])
        .unwrap();
    assert_eq!(atlas.findings().unwrap().len(), 1);
}

#[test]
fn a_fresh_estate_has_no_findings() {
    let estate = TempDir::new().unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    assert!(atlas.findings().unwrap().is_empty());
}

#[test]
fn malformed_index_row_is_a_hard_error_and_rebuild_repairs_it() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();
    std::fs::write(
        estate.path().join("atlas").join("findings.ndjson"),
        b"{not valid json\n",
    )
    .unwrap();
    let err = atlas.findings().unwrap_err();
    assert!(
        err.to_string().contains("malformed"),
        "expected a malformed-row error, got: {err}"
    );
    // `--rebuild`'s own mechanism: an atomic rewrite from journals alone
    // (here, a hand-built row set standing in for the daemon's own
    // journal walk) repairs it — reads are blocked until this runs.
    atlas
        .rebuild_finding_rows(vec![settled_row("finding-1")])
        .unwrap();
    assert_eq!(atlas.findings().unwrap().len(), 1);
}

// ---- crash recovery (reuses store.rs's own WIRK_ATLAS_FAILPOINT) ------

#[test]
fn child_append_crash() {
    let Some(root) = std::env::var_os("WB_FINDINGS_CRASH_ROOT") else {
        return;
    };
    let mut atlas = AtlasStore::open(root, "estate").unwrap();
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();
}

#[test]
fn interrupted_findings_write_reopens_clean_and_repairable() {
    let estate = TempDir::new().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("child_append_crash")
        .arg("--nocapture")
        .env("WB_FINDINGS_CRASH_ROOT", estate.path())
        .env("WIRK_ATLAS_FAILPOINT", "findings-file-synced")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86), "{output:?}");
    // Opening the store cleans any abandoned `.tmp-` sibling
    // (`AtlasStore::open`'s own sweep) — no torn write is left behind to
    // trip a later read.
    let mut atlas = AtlasStore::open(estate.path(), "estate").expect("reopens clean");
    assert!(atlas.findings().unwrap().is_empty());
    // The verb that crashed never got past the temp write, so the row it
    // meant to append is simply not there yet — retrying it (as the
    // daemon's own startup reconciliation would) succeeds normally.
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();
    assert_eq!(atlas.findings().unwrap().len(), 1);
}

// ---- the batch sweep (ruling 0116's measured second limit) --------------

/// Bytes **this thread** has passed to `write(2)`, from the kernel's own
/// accounting — the work an append actually did, rather than a timing
/// claim about how long it took.
///
/// `/proc/thread-self/io`, not `/proc/self/io`. The process-wide file
/// counts every thread in the test binary, and the harness runs this
/// binary's tests in parallel threads by default: a sibling test's own
/// file writes landed inside the measured window, and the second
/// measurement here — "a sweep that finds nothing missing must not touch
/// the file", which demands an exact zero — read them as this sweep's.
/// That made the test fail on the base commit and on candidates alike,
/// about two runs in five, with no defect in the code under measurement
/// (`loop-c1-build/raw/11-preexisting-flake.txt`, five runs in each of
/// two trees). The per-task file is the same kernel accounting narrowed
/// to the thread that did the work, so the measurement is of this test
/// and nothing else — and the suite keeps its parallelism and the
/// assertions keep their exact bounds.
///
/// `wchar` rather than `write_bytes` on purpose: `write_bytes` counts
/// only what reaches a block device, and a test running under a `TMPDIR`
/// on tmpfs measures a flat zero. `wchar` counts the bytes the code
/// wrote whatever it wrote them to, which is exactly the quantity that
/// went quadratic. The `fsync` pairs the same rewrite issues are counted
/// against the real daemon on a real disk in this stage's own
/// measurement run, not here.
///
/// Linux-only, and the test that uses it says so: neither `/proc` file
/// is portable, and inventing a portable fake of it would measure the
/// fake.
#[cfg(target_os = "linux")]
fn write_chars() -> u64 {
    let io = std::fs::read_to_string("/proc/thread-self/io")
        .expect("/proc/thread-self/io is readable on linux");
    io.lines()
        .find_map(|line| line.strip_prefix("wchar:"))
        .and_then(|value| value.trim().parse().ok())
        .expect("/proc/thread-self/io names wchar")
}

/// P3 native closeout item 1b: the index file's own identity, as the
/// filesystem reports it.
///
/// `write_chars()` reads `wchar:` from `/proc/thread-self/io`, which
/// counts **every** byte the thread wrote, to any file. That is a fine
/// order-of-magnitude witness for the quadratic shape below, and it is
/// used that way — deliberately tolerantly. It is not an attribution:
/// the exact `== 0` assertion this replaces failed on 68 bytes written
/// by something else entirely on the same thread
/// (`p3-world-loop/loop-c1-build/raw/10-full-suite.txt:1133`), and those
/// 68 bytes were never located.
///
/// The file's inode is the file-specific fact the contract is actually
/// about. `rewrite_rows` publishes the index by writing a
/// `.tmp-findings-<ulid>` and `rename`ing it over `findings.ndjson`, so
/// **every** rewrite through the real seam replaces the inode — a
/// byte-identical rewrite included, which is exactly the case size and
/// mtime cannot see. A sweep that writes nothing leaves the inode it
/// found. That the observation really does catch a same-content rewrite
/// is not asserted from this comment: the test below drives one through
/// the real `rebuild_finding_rows` seam and watches the inode move.
#[cfg(target_os = "linux")]
fn index_inode(estate: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(estate.join("atlas").join("findings.ndjson"))
        .expect("the index file exists")
        .ino()
}

fn asserted_row(finding_id: &str, event: &str) -> FindingRow {
    FindingRow {
        id: wirk_atlas::FindingRowId::compute(
            &FindingId(finding_id.to_string()),
            FindingRowKind::Asserted,
            &EventId(event.to_string()),
        ),
        kind: FindingRowKind::Asserted,
        finding: finding(finding_id),
        origin: FindingOrigin {
            work: WorkId("work-1".to_string()),
            raised_event: EventId("e-find".to_string()),
            row_event: EventId(event.to_string()),
        },
        settlement: None,
        assertion: Some(Assertion {
            decision: Decision::Deferred,
            by: "a reviewer".to_string(),
            reason: None,
            peer: PeerIdentity {
                uid: 1000,
                gid: 1000,
            },
            at: Timestamp(2),
            author: None,
        }),
        applied: None,
        superseded_by: None,
    }
}

/// The sweep `wirkd` runs after **every** settle/assert/apply offers
/// every row the estate's journals support, and the single-row append it
/// used to call re-read and atomically rewrote the whole index once per
/// offered row. One mutation therefore wrote O(rows²) bytes and issued
/// O(rows) `fsync` pairs — ruling 0116's second recorded limit, measured
/// there at ~120ms per `finding assert` on a 39-row / 88KB index.
///
/// This pins the repair as **work done**, not as elapsed time: the batch
/// form's storage writes for one sweep stay within a small constant of
/// the file it produces, while the per-row loop's grow with the square
/// of the index. The bound is deliberately loose (4x the final file, and
/// a demanded 4x separation between the two) so it fails on the shape,
/// never on filesystem slop.
#[cfg(target_os = "linux")]
#[test]
fn one_sweep_rewrites_the_index_once_not_once_per_row() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();

    // A realistic sweep: 60 rows offered against an empty index, then
    // the same 60 offered again with one new row, which is what every
    // mutation after the first actually looks like.
    let rows: Vec<FindingRow> = (0..60)
        .map(|n| asserted_row("finding-1", &format!("e-{n}")))
        .collect();

    let before = write_chars();
    assert_eq!(atlas.append_finding_rows(&rows).unwrap().appended, 60);
    let batch_bytes = write_chars() - before;

    let file = estate.path().join("atlas").join("findings.ndjson");
    let size = std::fs::metadata(&file).unwrap().len();
    assert_eq!(atlas.findings().unwrap().len(), 60);
    assert!(
        batch_bytes <= size * 4,
        "one sweep of {} rows writes one file, not one per row: wrote {batch_bytes} bytes for a {size}-byte index",
        rows.len()
    );

    // The same offered rows again: nothing is missing, so the sweep must
    // write nothing at all. This is the common case — every mutation
    // after the first re-offers everything already indexed.
    //
    // Measured on the file, not on the thread (item 1b). The inode is
    // the index file's own identity, and `rewrite_rows` can only publish
    // through `rename`, so a no-op sweep must leave the very same file
    // in place — no tolerance, no size or mtime comparison, and nothing
    // any unrelated write on this thread can perturb.
    let inode_before = index_inode(estate.path());
    assert_eq!(
        atlas.append_finding_rows(&rows).unwrap().appended,
        0,
        "an already-complete index needs no rewrite"
    );
    assert_eq!(
        index_inode(estate.path()),
        inode_before,
        "a sweep that finds nothing missing must not touch the file"
    );

    // And the measurement itself, proved rather than asserted: a real
    // rewrite of the *identical* rows through the real replacement seam
    // produces a file of identical size and identical content — the
    // rewrite size and mtime would hide — and the inode moves. So the
    // assertion above is a contract about writes, not about slop.
    let size_before = std::fs::metadata(&file).unwrap().len();
    let same_rows: Vec<FindingRow> = atlas.findings().unwrap();
    let inode_before_rewrite = index_inode(estate.path());
    atlas.rebuild_finding_rows(same_rows).unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().len(),
        size_before,
        "the control rewrite is deliberately content-identical"
    );
    assert_ne!(
        index_inode(estate.path()),
        inode_before_rewrite,
        "an identical rewrite must be visible to this measurement, or the no-op assertion above \
         proves nothing"
    );

    // And the shape being replaced, measured on the identical rows in
    // the identical store: one whole-file rewrite per row.
    let second = TempDir::new().unwrap();
    let mut per_row = AtlasStore::open(second.path(), "estate").unwrap();
    let before = write_chars();
    for row in &rows {
        per_row.append_finding_row(row).unwrap();
    }
    let per_row_bytes = write_chars() - before;
    assert_eq!(per_row.findings().unwrap().len(), 60);
    assert!(
        per_row_bytes > batch_bytes * 4,
        "the per-row loop is the quadratic shape this replaces: {per_row_bytes} bytes against the batch's {batch_bytes}"
    );
}

/// The batch is the single-row append's own contract, unchanged: dedup
/// is still the row's content-addressed id, against the file and against
/// the batch itself, and the rows that were already there keep their
/// order and their content.
#[test]
fn a_batch_append_dedupes_against_the_file_and_against_itself() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();

    let offered = vec![
        settled_row("finding-1"),
        asserted_row("finding-1", "e-a"),
        asserted_row("finding-1", "e-a"),
        asserted_row("finding-1", "e-b"),
    ];
    assert_eq!(
        atlas.append_finding_rows(&offered).unwrap().appended,
        2,
        "the row already held and the repeat inside the batch are both dropped"
    );
    let rows = atlas.findings().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0],
        settled_row("finding-1"),
        "an existing row keeps its place and its content"
    );
    let mut ids: Vec<String> = rows.iter().map(|row| row.id.0.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "no row is written twice: {rows:?}");

    // Offering the whole set again appends nothing and changes nothing.
    assert_eq!(atlas.append_finding_rows(&offered).unwrap().appended, 0);
    assert_eq!(atlas.findings().unwrap(), rows);
}

/// Ruling 0137 on the write side: an append reports the index **file**
/// it read or wrote, not only how many rows it added.
///
/// The caller that records projection health used to take that fact from
/// a directory listing made after this call returned, which is a
/// different moment and, when the file is deleted in between, a
/// different answer — recorded, it became "this estate never wrote an
/// index" and a later read of the missing file answered complete. The
/// append is the observation that actually looked: it opens the file
/// before it appends, and it renames one into place when it writes.
#[test]
fn an_append_reports_the_index_file_it_read_or_wrote() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let file = estate.path().join("atlas").join("findings.ndjson");

    // Nothing offered to an estate that never published: no file read,
    // none written, and that is the fact reported.
    let empty = atlas.append_finding_rows(&[]).unwrap();
    assert_eq!(empty.appended, 0);
    assert_eq!(empty.backing, IndexBacking::Absent);
    assert!(!file.exists(), "and nothing was created to say it");

    // The first real append publishes the file.
    let rows = vec![settled_row("finding-1")];
    let published = atlas.append_finding_rows(&rows).unwrap();
    assert_eq!(published.appended, 1);
    assert_eq!(published.backing, IndexBacking::Present);

    // The common case after the first mutation: every offered row is
    // already held, not one byte is written — and the file this call
    // read is every bit as real as one it rewrote.
    let dedup = atlas.append_finding_rows(&rows).unwrap();
    assert_eq!(dedup.appended, 0);
    assert_eq!(
        dedup.backing,
        IndexBacking::Present,
        "a sweep that wrote nothing still read the file"
    );

    // And the pre-publication state again, from the other side: the file
    // goes, and the very next append says what it found rather than what
    // it found last time.
    std::fs::remove_file(&file).unwrap();
    let republished = atlas.append_finding_rows(&rows).unwrap();
    assert_eq!(republished.appended, 1, "the row is written again");
    assert_eq!(republished.backing, IndexBacking::Present);
}

/// A failed batch says how many rows the index does **not** hold, which
/// is what lets `wirkd` report its own projection health rather than
/// naming whichever row it happened to be on when it stopped.
#[test]
fn a_refused_write_reports_how_many_rows_did_not_land() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();

    let atlas_dir = estate.path().join("atlas");
    let mut readonly = std::fs::metadata(&atlas_dir).unwrap().permissions();
    readonly.set_readonly(true);
    std::fs::set_permissions(&atlas_dir, readonly).unwrap();

    let unwritten = atlas
        .append_finding_rows(&[
            settled_row("finding-1"),
            asserted_row("finding-1", "e-a"),
            asserted_row("finding-1", "e-b"),
        ])
        .expect_err("an unwritable atlas directory refuses the rewrite");
    assert_eq!(
        unwritten.pending,
        Some(2),
        "the row already indexed is not pending; the two new ones are: {unwritten}"
    );

    let mut writable = std::fs::metadata(&atlas_dir).unwrap().permissions();
    writable.set_mode(0o755);
    std::fs::set_permissions(&atlas_dir, writable).unwrap();
    // The file itself is untouched by the failure: whole-file
    // replacement never leaves a partial index behind.
    assert_eq!(atlas.findings().unwrap(), vec![settled_row("finding-1")]);
}

// ---- the durability window's own row count ----------------------------
//
// `rewrite_rows` raises `DurabilityUncertain` *after* the atomic rename,
// so the rows are already in the file a fresh reader opens and the doubt
// is the containing directory entry's alone. The batch API nonetheless
// reported every one of them as `pending`, which is a count of rows that
// did not land describing rows that did (ruling 0125's executed case 3,
// "the API wart"). The daemon happens to match the variant before it
// consults the count, so nothing in tree read the wrong number — but a
// second caller of this API would have had to know that.
//
// Driven the way `interrupted_findings_write_reopens_clean_and_repairable`
// drives the crash half of the same gate (R2): a real child process, a
// real window, and a real kernel `EACCES` on the directory this store is
// about to `fsync`.

#[test]
fn child_batch_durability_window() {
    let Some(root) = std::env::var_os("WB_FINDINGS_DURABILITY_ROOT") else {
        return;
    };
    let mut atlas = AtlasStore::open(root, "estate").unwrap();
    let unwritten = atlas
        .append_finding_rows(&[settled_row("finding-1"), settled_row("finding-2")])
        .expect_err("the directory sync was denied");
    println!("PENDING={:?}", unwritten.pending);
    println!("DISPLAY={unwritten}");
}

#[test]
fn a_batch_whose_rows_are_visible_reports_nothing_pending() {
    let estate = TempDir::new().unwrap();
    let atlas_dir = estate.path().join("atlas");
    let barrier = estate.path().join("barrier");
    std::fs::create_dir_all(&barrier).unwrap();
    // Bound before the child is spawned, so the window it is told to
    // park on is always there when it reaches it.
    let listener = UnixListener::bind(barrier.join(wirk_atlas::BARRIER_RELEASE_SOCKET)).unwrap();
    listener.set_nonblocking(true).unwrap();

    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("child_batch_durability_window")
        .arg("--nocapture")
        .env("WB_FINDINGS_DURABILITY_ROOT", estate.path())
        .env(
            "WIRK_ATLAS_BARRIER",
            format!("findings-renamed={}", barrier.display()),
        )
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Armed only once the child exists, so the one thread that parks is
    // the one performing this rewrite.
    std::fs::write(barrier.join("arm"), b"").unwrap();

    // The gate parks on a blocking read of this listener's connection and
    // has no notion of elapsed time (ruling 0044 D134, final): `accept`
    // returning is the child's arrival, and dropping the connection is
    // the release. The bound below is this controller's own, and its
    // exhaustion is a failure reporting a state never observed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let parked = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept on the release socket failed: {error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the child was never observed to reach the window after its rename"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    // The rename has happened; the directory `fsync` has not. Denying
    // the directory now is the real window, from the kernel.
    let restore = std::fs::metadata(&atlas_dir).unwrap().permissions();
    std::fs::set_permissions(&atlas_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    drop(parked);

    let output = child.wait_with_output().unwrap();
    std::fs::set_permissions(&atlas_dir, restore).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("PENDING=Some(0)"),
        "rows that are already in the file are not pending: {stdout}"
    );
    assert!(
        stdout.contains("directory sync failed"),
        "and the error names the window it really is: {stdout}"
    );
    assert!(
        stdout.contains("holds every offered row"),
        "as does the way it renders itself: {stdout}"
    );

    // The rows really are visible to a fresh reader, which is the whole
    // reason nothing is pending.
    let atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    assert_eq!(atlas.findings().unwrap().len(), 2);
}

/// Ruling 0137, at the read that decides it: **the four backing states
/// are four different answers, and absence is not one of the empties.**
///
/// `read_rows` used to answer a missing file with `Ok(vec![])`, the same
/// value it answers a present-and-empty file with, and the caller that
/// paired that list with a health record could not tell them apart. The
/// four states are asserted here against a real store on a real
/// directory, in the one place the distinction is made.
#[test]
fn a_missing_index_is_not_a_present_and_empty_one() {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let index = estate.path().join("atlas").join("findings.ndjson");

    // 1. Missing: no file has ever been written here.
    assert!(!index.exists());
    let read = atlas.read_findings().unwrap();
    assert!(read.rows.is_empty());
    assert_eq!(
        read.backing,
        wirk_atlas::IndexBacking::Absent,
        "a file that is not there is reported as absent, not as an empty index"
    );

    // 2. Present and empty: a real file holding no rows.
    atlas.rebuild_finding_rows(Vec::new()).unwrap();
    assert!(index.exists(), "a rebuild of nothing still writes the file");
    let read = atlas.read_findings().unwrap();
    assert!(read.rows.is_empty());
    assert_eq!(
        read.backing,
        wirk_atlas::IndexBacking::Present,
        "an index that was opened and holds no rows is a measured empty"
    );

    // 3. Present with rows, then taken away underneath the store: the
    //    read reports absence rather than the rows it saw a moment ago,
    //    and never an error about a file it was not asked to require.
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();
    let read = atlas.read_findings().unwrap();
    assert_eq!(read.rows.len(), 1);
    assert_eq!(read.backing, wirk_atlas::IndexBacking::Present);
    let kept = std::fs::read(&index).unwrap();
    std::fs::remove_file(&index).unwrap();
    let read = atlas.read_findings().unwrap();
    assert!(read.rows.is_empty());
    assert_eq!(read.backing, wirk_atlas::IndexBacking::Absent);

    // 4. The two that were already errors stay errors, and are not
    //    folded into absence by any of the above.
    std::fs::write(&index, b"not json at all\n").unwrap();
    let err = atlas.read_findings().unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");
    std::fs::write(&index, &kept).unwrap();
    std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = atlas.read_findings().unwrap_err();
    assert!(
        err.to_string().contains("Permission denied"),
        "an unreadable index is its own refusal, never an absent one: {err}"
    );
    std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        atlas.findings().unwrap().len(),
        1,
        "and the row is still there"
    );
}

/// The listing the health record is formed from answers both of its
/// questions from one `read_dir`, and neither of them by guessing.
#[test]
fn one_atlas_listing_answers_preserved_copies_and_the_index_file() {
    let estate = TempDir::new().unwrap();
    let listing = wirk_atlas::atlas_directory_listing(estate.path()).unwrap();
    assert!(!listing.index_present, "no atlas directory, no index file");
    assert!(listing.preserved.is_empty());

    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    atlas.append_finding_row(&settled_row("finding-1")).unwrap();
    let listing = wirk_atlas::atlas_directory_listing(estate.path()).unwrap();
    assert!(listing.index_present);
    assert!(listing.preserved.is_empty());

    let atlas_dir = estate.path().join("atlas");
    std::fs::write(
        atlas_dir.join("findings.ndjson.unreadable-01"),
        b"kept bytes\n",
    )
    .unwrap();
    std::fs::remove_file(atlas_dir.join("findings.ndjson")).unwrap();
    let listing = wirk_atlas::atlas_directory_listing(estate.path()).unwrap();
    assert!(
        !listing.index_present,
        "a preserved copy is not the standing index"
    );
    assert_eq!(listing.preserved, vec!["findings.ndjson.unreadable-01"]);
}
