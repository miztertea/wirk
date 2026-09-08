//! `wirk-atlas`'s own Findings index (W-B, `findings.rs`): append/read/
//! dedup/rebuild/malformed-line/crash-recovery against a
//! directly-constructed `AtlasStore` — no daemon, no journal. The
//! server-side journal-derivation and settlement policy live in
//! `wirk/tests/findings.rs`.

use std::process::Command;

use tempfile::TempDir;
use wirk_atlas::{AtlasStore, FindingOrigin, FindingRow, FindingRowKind};
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
