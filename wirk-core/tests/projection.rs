//! P3 W-C1: stage-projection identity, immutability and World-hash
//! coverage (`knowledge/work/p3-world-loop/loop-c-build-correct/BUILD.md`
//! §3, §5.1, bounded by ruling 0124).
//!
//! Pure `wirk-core`: no daemon, no Atlas. The real-service half — that
//! an actual reservation assembles a real projection, writes a real
//! file and serves it over the public CLI — is `wirk/tests/projection.rs`.

use std::path::{Path, PathBuf};

use wirk_core::DeliveredContent;
use wirk_core::{
    ActorWorld, ArtifactSpec, Boundary, DeterministicWorld, EvidenceCoverage, EvidenceItem,
    EvidenceProjectionRef, ExecutionTriple, ItemIdentity, Lifetime, ObservationId,
    ObservationReceipt, OrientationRequest, ProjectionContent, ProjectionFile, ProjectionId,
    ProjectionUnavailable, RunId, Statement, StatementOrigin, WaypointId, WorkId, World, WorldHash,
};

/// Recorded historical `WaypointReserved` Worlds and the `world_hash`
/// the daemon actually journaled beside each, harvested from real
/// journals in this estate's own evidence — not recomputed from today's
/// code and pinned as if it were history.
///
/// Two of them predate `source_basis` entirely (the pre-v2 `legacy`
/// encoding); one carries a Git basis (the `v2` encoding). None carries
/// a review target and none carries a projection, which is exactly the
/// point: `WorldHash::of`'s legacy predicate only ever *narrows*, and
/// both extension blocks are presence-gated, so every one of these must
/// still reproduce its journaled hex byte for byte.
const HISTORICAL: &str = include_str!("vectors/historical_world_hashes.json");

#[derive(serde::Deserialize)]
struct HistoricalVector {
    note: String,
    journal: String,
    world_hash: String,
    legacy_encoding: bool,
    world: World,
}

#[test]
fn every_recorded_historical_world_still_hashes_to_its_journaled_hex() {
    let vectors: Vec<HistoricalVector> =
        serde_json::from_str(HISTORICAL).expect("historical vectors parse");
    assert_eq!(vectors.len(), 3, "three recorded vectors, not fewer");
    for vector in &vectors {
        assert!(
            !vector.world.carries_evidence(),
            "a historical World must not carry a projection: {}",
            vector.note
        );
        assert_eq!(
            WorldHash::of(&vector.world).0,
            vector.world_hash,
            "WorldHash::of moved for a recorded historical World ({}, {})",
            vector.note,
            vector.journal
        );
        if vector.legacy_encoding {
            // `of` alone cannot show the *fallback* still applies — `of`
            // is what decides whether it does. Asserting through
            // `legacy_for_tests` as well pins the encoding, not only the
            // value.
            assert_eq!(
                WorldHash::legacy_for_tests(&vector.world).0,
                vector.world_hash,
                "a pre-v2 World must still take the pre-v2 encoding ({})",
                vector.note
            );
        }
    }
}

fn triple() -> ExecutionTriple {
    ExecutionTriple {
        estate_root: "/estate".to_string(),
        work_id: WorkId("work-1".to_string()),
        run_id: RunId("run-1".to_string()),
    }
}

fn actor_world(evidence: Option<EvidenceProjectionRef>) -> World {
    World::Actor(ActorWorld {
        repository: "/repo".to_string(),
        worktree_path: PathBuf::from("/estate/worktrees/work-1"),
        branch: "wirk/work-1".to_string(),
        base_sha: "0123456789abcdef".to_string(),
        source_basis: wirk_core::SourceBasis::Git {
            base: "0123456789abcdef".to_string(),
        },
        triple: triple(),
        intent: "look at wirk-core/src/lib.rs".to_string(),
        output_contract: wirk_core::OutputContract(vec![ArtifactSpec {
            name: "report.md".to_string(),
            required: true,
        }]),
        boundary: Boundary(vec!["**".to_string()]),
        review_targets: Vec::new(),
        evidence: evidence.map(Box::new),
    })
}

/// A reference that names a projection id and an observation without
/// describing any written file. Used by the World-hash tests, which care
/// about what the hash covers and never read a file.
fn reference(projection: &ProjectionId, observation: &str, revision: u64) -> EvidenceProjectionRef {
    let receipt = receipt_of(observation, 5);
    EvidenceProjectionRef {
        observation: receipt.observation.clone(),
        projection: projection.clone(),
        revision,
        format: wirk_core::PROJECTION_FORMAT.to_string(),
        receipt: receipt.digest(),
    }
}

/// The reference a journal would carry for exactly this file: its
/// content id, its observation, its format, and the digest over its
/// delivered receipt bytes.
fn reference_for(file: &ProjectionFile, revision: u64) -> EvidenceProjectionRef {
    EvidenceProjectionRef {
        observation: file.receipt.observation.clone(),
        projection: file.content.projection_id(),
        revision,
        format: file.content.format().to_string(),
        receipt: file.receipt.digest(),
    }
}

fn item(coordinate: &str) -> EvidenceItem {
    EvidenceItem {
        coordinate: coordinate.to_string(),
        summary: "fn reserve_next_leaf(".to_string(),
        lifetime: Lifetime::Working,
        reason: "the authored text names it".to_string(),
        identity: ItemIdentity::Generation {
            generation: "gen-1".to_string(),
            object_id: "obj-1".to_string(),
        },
    }
}

fn content(bound: Vec<EvidenceItem>) -> ProjectionContent {
    ProjectionContent {
        format: wirk_core::PROJECTION_FORMAT.to_string(),
        compilation_policy: wirk_core::ASSEMBLY_POLICY.to_string(),
        route_edition: "edition-1".to_string(),
        waypoint: WaypointId("readiness/investigate".to_string()),
        revision: 0,
        question: "which function decides boundary refusal?".to_string(),
        generations: vec![("m-1".to_string(), "gen-1".to_string())],
        publication_revision: 7,
        bound,
        assumptions: vec![Statement {
            text: "assembled under wirk.assembly/v1".to_string(),
            attributed_to: StatementOrigin::Assembly,
        }],
        retrieval: retrieval_note(),
        referenced: Vec::new(),
        reachable: Vec::new(),
        unknowns: Vec::new(),
        omitted: Vec::new(),
        next_action: "State of the delivered evidence: complete.".to_string(),
        coverage: EvidenceCoverage::Complete,
        truncated: false,
        expansion: None,
        consulted: Vec::new(),
        findings_index: wirk_core::FindingsIndexNote {
            state: wirk_core::FindingsIndexState::Synchronized,
            complete: true,
        },
    }
}

/// The current content inside a file under test, mutably: every
/// projection this build writes is v3, and a test that edits one is
/// editing that.
fn v2_mut(file: &mut ProjectionFile) -> &mut ProjectionContent {
    match &mut file.content {
        wirk_core::DeliveredContent::V3(content) => content,
        wirk_core::DeliveredContent::V2(_) | wirk_core::DeliveredContent::V1(_) => {
            panic!("this build writes v3 content")
        }
    }
}

fn retrieval_note() -> wirk_core::RetrievalNote {
    wirk_core::RetrievalNote {
        mode: "lexical".to_string(),
        semantic: "unavailable".to_string(),
        semantic_reason: Some("no query backend is configured".to_string()),
        editions: Vec::new(),
        degraded: Vec::new(),
        total_candidates: 0,
        returned: 0,
    }
}

fn receipt_of(observation: &str, observed_at: u64) -> ObservationReceipt {
    ObservationReceipt {
        observation: ObservationId(observation.to_string()),
        observed_at,
        observation_window_ms: 3,
        laps: 1,
    }
}

fn file(content: ProjectionContent, observation: &str, observed_at: u64) -> ProjectionFile {
    ProjectionFile {
        content: wirk_core::DeliveredContent::V3(Box::new(content)),
        receipt: receipt_of(observation, observed_at),
    }
}

/// C3, the correction that split content identity from the observation
/// receipt: two assemblies that delivered the same context delivered the
/// same context, whatever instant each observed it at. If the receipt
/// were hashed, this could not hold and the "identical content
/// reproduces one id" contract would be self-contradictory.
#[test]
fn two_assemblies_of_identical_content_share_a_projection_id_and_differ_in_receipt() {
    let first = file(content(vec![item("coord-a")]), "obs-1", 1_000);
    let second = file(content(vec![item("coord-a")]), "obs-2", 9_999);
    assert_eq!(
        first.content.projection_id(),
        second.content.projection_id(),
        "identical delivered content must reproduce one ProjectionId"
    );
    assert_ne!(first.receipt, second.receipt);
}

/// Ordered delivery is part of the fingerprint (`BUILD-AMENDMENTS.md`):
/// same set, different order, different delivered context. Nothing on
/// the way into the hash sorts a list.
#[test]
fn reordering_two_bound_items_changes_the_projection_id() {
    let forward = content(vec![item("coord-a"), item("coord-b")]);
    let reversed = content(vec![item("coord-b"), item("coord-a")]);
    assert_ne!(forward.projection_id(), reversed.projection_id());
}

#[test]
fn a_world_carrying_a_projection_hashes_it_and_every_covered_field_moves_it() {
    let id = content(vec![item("coord-a")]).projection_id();
    let other = content(vec![item("coord-b")]).projection_id();
    let bare = WorldHash::of(&actor_world(None));
    let bound = WorldHash::of(&actor_world(Some(reference(&id, "obs-1", 0))));
    assert_ne!(bare, bound, "a projection is part of the World's content");

    for (label, changed) in [
        ("projection id", reference(&other, "obs-1", 0)),
        ("revision", reference(&id, "obs-1", 1)),
        (
            "format",
            EvidenceProjectionRef {
                format: "wirk.projection/v99".to_string(),
                ..reference(&id, "obs-1", 0)
            },
        ),
    ] {
        assert_ne!(
            bound,
            WorldHash::of(&actor_world(Some(changed))),
            "changing the {label} must change the World hash"
        );
    }

    // The observation id is provenance, not content: re-observing the
    // identical delivered context must not change a stage's resume key.
    assert_eq!(
        bound,
        WorldHash::of(&actor_world(Some(reference(&id, "obs-2", 0)))),
        "the observation id must not be covered"
    );
}

/// The legacy predicate narrowed, so an `Unknown`-basis World carrying a
/// projection takes the v2 encoding rather than an encoding that would
/// silently drop the projection out of the hash. `WorldHash::of` stays
/// total: the refusals live at the writers and the reader, never here.
#[test]
fn an_unknown_basis_world_carrying_a_projection_leaves_the_legacy_encoding() {
    let id = content(vec![item("coord-a")]).projection_id();
    let World::Actor(mut actor) = actor_world(Some(reference(&id, "obs-1", 0))) else {
        unreachable!()
    };
    actor.source_basis = wirk_core::SourceBasis::Unknown;
    let world = World::Actor(actor);
    assert_ne!(
        WorldHash::of(&world),
        WorldHash::legacy_for_tests(&world),
        "a World carrying a projection is not a pre-v2 World"
    );
}

#[test]
fn a_deterministic_world_hashes_exactly_as_it_always_did() {
    let world = World::Deterministic(DeterministicWorld {
        command: vec!["cargo".to_string(), "test".to_string()],
        base_sha: "abc123".to_string(),
        source_basis: wirk_core::SourceBasis::Unknown,
        cwd: "/var/tmp/w1".into(),
        env: Default::default(),
        expected_artifacts: wirk_core::OutputContract(vec![]),
    });
    assert!(!world.carries_evidence());
    assert_eq!(
        WorldHash::of(&world).0,
        "8f942c918b200550dc43a21ed6ee6ca4bb313608a2178fe9f4fdd458adeb22de"
    );
}

// ---- the file: written once, read back proved --------------------------

fn estate() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp estate")
}

fn write(estate: &Path, work: &str, file: &ProjectionFile) -> PathBuf {
    file.write_new(estate, &WorkId(work.to_string()))
        .expect("write projection")
}

#[test]
fn a_written_projection_reads_back_and_proves_its_own_identity() {
    let dir = estate();
    let written = file(content(vec![item("coord-a")]), "obs-1", 5);
    let reference = reference_for(&written, 0);
    write(dir.path(), "work-1", &written);
    let read =
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference)
            .expect("read back");
    assert_eq!(read, written);
}

/// Corruption is an explicit unavailability, never a silent empty
/// projection and never a re-assembly against today's estate: a file
/// whose content no longer re-hashes to the id the journal recorded is
/// not the delivered context, whatever else it may be.
#[test]
fn a_projection_whose_content_was_edited_reads_unavailable() {
    let dir = estate();
    let written = file(content(vec![item("coord-a")]), "obs-1", 5);
    let reference = reference_for(&written, 0);
    let path = write(dir.path(), "work-1", &written);

    let mut edited = written.clone();
    v2_mut(&mut edited).bound[0].summary = "fn something_else(".to_string();
    std::fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();

    assert_eq!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference),
        Err(ProjectionUnavailable::ContentMismatch)
    );
}

/// The receipt is not hashed, so it needs its own integrity check or a
/// delivered projection's provenance could be substituted by editing a
/// file. The reference carries the observation id; the file must agree
/// with it.
#[test]
fn a_projection_whose_receipt_was_swapped_reads_unavailable() {
    let dir = estate();
    let written = file(content(vec![item("coord-a")]), "obs-1", 5);
    let reference = reference_for(&written, 0);
    let path = write(dir.path(), "work-1", &written);

    let mut edited = written.clone();
    edited.receipt.observation = ObservationId("obs-forged".to_string());
    edited.receipt.observed_at = 1;
    std::fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();

    assert_eq!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference),
        Err(ProjectionUnavailable::ContentMismatch)
    );
}

/// Ruling 0126, F1. The receipt is separable from content identity, and
/// separable is not unchecked: `observed_at`, `observation_window_ms`
/// and `laps` were covered by nothing at all in the first candidate — no
/// content hash, no journal, no World hash — so a delivered
/// projection's provenance could be rewritten on disk and served as
/// fact. Every receipt byte is now covered by the digest the journal
/// reference carries.
#[test]
fn every_receipt_field_is_covered_by_the_journaled_digest() {
    for (field, mutate) in [
        (
            "observed_at",
            (|receipt: &mut ObservationReceipt| receipt.observed_at = 1)
                as fn(&mut ObservationReceipt),
        ),
        (
            "observation_window_ms",
            |receipt: &mut ObservationReceipt| receipt.observation_window_ms = 999_999,
        ),
        ("laps", |receipt: &mut ObservationReceipt| receipt.laps = 42),
    ] {
        let dir = estate();
        let written = file(content(vec![item("coord-a")]), "obs-1", 1_788_854_577_861);
        let reference = reference_for(&written, 0);
        let path = write(dir.path(), "work-1", &written);

        // The positive control, on the same file, before it is touched:
        // a real receipt reads back and is delivered.
        assert_eq!(
            ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference)
                .expect("an untouched receipt is delivered")
                .receipt,
            written.receipt
        );

        let mut edited = written.clone();
        mutate(&mut edited.receipt);
        assert_ne!(
            edited.receipt, written.receipt,
            "the {field} mutation must bite"
        );
        std::fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();

        assert_eq!(
            ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference),
            Err(ProjectionUnavailable::ReceiptMismatch),
            "receipt.{field} changed without the journal must not be delivered as fact"
        );
    }
}

/// The reference is what `WorldHash::of` covers, and it duplicates
/// `format` and `revision` out of the content. A duplicate that
/// disagrees with the file is a reference that does not describe its own
/// delivery: refused, rather than read through on the strength of the
/// content hash and this binary's current format constant alone (ruling
/// 0126, F1).
#[test]
fn a_reference_that_disagrees_with_its_own_content_reads_unavailable() {
    let dir = estate();
    let mut written = file(content(vec![item("coord-a")]), "obs-1", 5);
    v2_mut(&mut written).revision = 3;
    let honest = reference_for(&written, 3);
    write(dir.path(), "work-1", &written);

    assert!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &honest).is_ok(),
        "an agreeing reference still reads"
    );

    for (label, changed) in [
        (
            "revision",
            EvidenceProjectionRef {
                revision: 0,
                ..honest.clone()
            },
        ),
        (
            "format",
            EvidenceProjectionRef {
                format: "wirk.projection/v99".to_string(),
                ..honest.clone()
            },
        ),
    ] {
        assert_eq!(
            ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &changed),
            Err(ProjectionUnavailable::ReferenceMismatch),
            "a reference whose {label} contradicts the content must not be read through"
        );
    }
}

/// The J2 call ruling 0126 left to this stage, pinned so a later wave
/// has to decide it again rather than drift into it: the receipt digest
/// makes provenance *verifiable*, and it is deliberately not part of
/// content identity and not part of the World hash. Re-observing the
/// identical delivered context therefore still yields one `ProjectionId`
/// and still does not move a stage's resume key.
#[test]
fn the_receipt_digest_is_neither_content_identity_nor_a_resume_key() {
    let first = file(content(vec![item("coord-a")]), "obs-1", 1_000);
    let second = file(content(vec![item("coord-a")]), "obs-1", 9_999);
    assert_ne!(
        first.receipt.digest(),
        second.receipt.digest(),
        "a different observation is a different receipt digest"
    );
    assert_eq!(
        first.content.projection_id(),
        second.content.projection_id(),
        "the receipt digest must not enter content identity"
    );
    assert_eq!(
        WorldHash::of(&actor_world(Some(reference_for(&first, 0)))),
        WorldHash::of(&actor_world(Some(reference_for(&second, 0)))),
        "the receipt digest must not move a stage's resume key"
    );
}

#[test]
fn a_missing_or_unreadable_projection_file_is_explicit() {
    let dir = estate();
    let written = file(content(vec![item("coord-a")]), "obs-1", 5);
    let reference = reference_for(&written, 0);
    assert_eq!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference),
        Err(ProjectionUnavailable::FileMissing)
    );
    let path = write(dir.path(), "work-1", &written);
    std::fs::write(&path, b"{ not a projection").unwrap();
    assert_eq!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference),
        Err(ProjectionUnavailable::FileUnreadable)
    );
}

/// The one file is written once. Nothing overwrites anything, so
/// "immutable" holds literally rather than by convention.
#[test]
fn a_projection_file_is_never_rewritten() {
    let dir = estate();
    let written = file(content(vec![item("coord-a")]), "obs-1", 5);
    write(dir.path(), "work-1", &written);
    let second = file(content(vec![item("coord-b")]), "obs-1", 6);
    assert!(
        second
            .write_new(dir.path(), &WorkId("work-1".to_string()))
            .is_err(),
        "a second write at the same observation id must refuse, not overwrite"
    );
}

/// §3.4: the id is not authority. Two Works whose assemblies delivered
/// identical content share one `ProjectionId`, and each reads its own
/// file under its own `works/<work_id>/` — neither can reach the other's
/// by naming it, because there is no path from an id to a file.
#[test]
fn the_same_content_id_in_two_works_reads_two_files_and_neither_names_the_other() {
    let dir = estate();
    let mine = file(content(vec![item("coord-a")]), "obs-mine", 1);
    let theirs = file(content(vec![item("coord-a")]), "obs-theirs", 2);
    assert_eq!(mine.content.projection_id(), theirs.content.projection_id());
    write(dir.path(), "work-mine", &mine);
    write(dir.path(), "work-theirs", &theirs);

    // The other Work's own reference, presented by this Work: its
    // observation names a file that does not exist under this Work.
    let stolen = reference_for(&theirs, 0);
    assert_eq!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-mine".to_string()), &stolen),
        Err(ProjectionUnavailable::FileMissing)
    );
    // And its own reference reads its own file.
    let own = reference_for(&mine, 0);
    assert_eq!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-mine".to_string()), &own)
            .unwrap()
            .receipt
            .observation,
        ObservationId("obs-mine".to_string())
    );
}

#[test]
fn the_same_reference_in_a_second_estate_resolves_to_nothing() {
    let first = estate();
    let second = estate();
    let written = file(content(vec![item("coord-a")]), "obs-1", 5);
    let reference = reference_for(&written, 0);
    write(first.path(), "work-1", &written);
    assert_eq!(
        ProjectionFile::read_referenced(second.path(), &WorkId("work-1".to_string()), &reference),
        Err(ProjectionUnavailable::FileMissing)
    );
}

/// An observation id becomes a filename, so nothing that could leave
/// `projections/` is one — checked before the join, not after.
#[test]
fn an_observation_id_that_is_not_a_filename_is_refused_before_any_path_join() {
    let dir = estate();
    for bad in ["../../etc/passwd", "obs/1", "", "obs.1"] {
        let malformed = EvidenceProjectionRef {
            observation: ObservationId(bad.to_string()),
            projection: ProjectionId("0".repeat(64)),
            revision: 0,
            format: wirk_core::PROJECTION_FORMAT.to_string(),
            receipt: "0".repeat(64),
        };
        assert_eq!(
            ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &malformed),
            Err(ProjectionUnavailable::MalformedReference),
            "{bad:?} must not reach a path join"
        );
    }
}

#[test]
fn a_projection_naming_an_unknown_format_reads_unavailable_rather_than_being_decoded() {
    let dir = estate();
    let mut written = file(content(vec![item("coord-a")]), "obs-1", 5);
    v2_mut(&mut written).format = "wirk.projection/v99".to_string();
    let reference = reference_for(&written, 0);
    write(dir.path(), "work-1", &written);
    assert_eq!(
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference),
        Err(ProjectionUnavailable::UnknownFormat)
    );
}

// ---- the authored request ----------------------------------------------

/// An `orient` block on a Waypoint that opens no actor Run would be
/// authored configuration nothing consumes — refused at load, the same
/// posture `ActorSelectionOnNonActor` already takes.
#[test]
fn an_orientation_request_on_a_non_actor_waypoint_is_refused_at_load() {
    let dir = estate();
    let path = dir.path().join("route.json");
    std::fs::write(
        &path,
        r#"{"id":"r","waypoints":[{"id":"r/build","kind":"Deterministic","declared_outputs":[{"name":"out.md","required":true}],"command":["true"],"orient":{"question":"why?"}}]}"#,
    )
    .unwrap();
    let error = wirk_core::load_route(&path).expect_err("must refuse");
    assert!(
        matches!(error, wirk_core::RouteError::OrientationOnNonActor { .. }),
        "{error:?}"
    );
}

#[test]
fn a_route_without_orientation_journals_byte_identical_waypoint_definitions() {
    let dir = estate();
    let path = dir.path().join("route.json");
    let text = r#"{"id":"r","waypoints":[{"id":"r/act","kind":"Actor","declared_outputs":[{"name":"out.md","required":true}],"intent":"do it"}]}"#;
    std::fs::write(&path, text).unwrap();
    let route = wirk_core::load_route(&path).expect("loads");
    let serialized = serde_json::to_string(&route.waypoints).expect("serializes");
    assert!(
        !serialized.contains("orient"),
        "a Route that declares no orientation must serialize no orient field: {serialized}"
    );
    assert!(route.waypoints[0].orient.is_none());
}

#[test]
fn an_authored_orientation_request_round_trips() {
    let dir = estate();
    let path = dir.path().join("route.json");
    std::fs::write(
        &path,
        r#"{"id":"r","waypoints":[{"id":"r/act","kind":"Actor","declared_outputs":[{"name":"out.md","required":true}],"intent":"do it","orient":{"question":"which function refuses?","sources":["wirk"]}}]}"#,
    )
    .unwrap();
    let route = wirk_core::load_route(&path).expect("loads");
    assert_eq!(
        route.waypoints[0].orient,
        Some(OrientationRequest {
            question: "which function refuses?".to_string(),
            sources: vec!["wirk".to_string()],
            budget: wirk_core::PresentationBudget::default(),
            // A Route that names no backend decodes to none, and
            // serializes back to exactly the bytes it arrived as, so no
            // C1-era Route's `route_edition` moves (ruling 0128 F3).
            semantic: None,
        })
    );
}

/// The exact bytes of a W-C1 `wirk.projection/v1` file, as C1 wrote
/// them: the v1 field set, in the v1 order, with no v2 field anywhere.
const V1_DOCUMENT: &str = r#"{"content":{"format":"wirk.projection/v1","compilation_policy":"wirk.assembly/v1","route_edition":"edition-1","waypoint":"readiness/investigate","revision":0,"question":"which function decides boundary refusal?","generations":[["m-1","gen-1"]],"publication_revision":7,"bound":[{"coordinate":"coord-a","summary":"fn reserve_next_leaf(","lifetime":"working","reason":"the authored text names it","identity":{"kind":"generation","generation":"gen-1","object_id":"obj-1"}}],"assumptions":[{"text":"assembled under wirk.assembly/v1","attributed_to":"assembly"}],"unknowns":[],"omitted":[],"coverage":{"state":"complete"}},"receipt":{"observation":"obs-1","observed_at":5,"observation_window_ms":3,"laps":1}}"#;

/// W-C2 advances the delivered-context format to `wirk.projection/v2`,
/// and the reader keeps decoding v1 **as v1**.
///
/// This is not a nicety. A projection is an immutable record of what one
/// stage was handed; its `ProjectionId` is the sha256 of its canonical
/// bytes under its own format's domain tag, and a journal already
/// carries that id. Re-parsing a v1 file under the v2 shape — even with
/// defaulted fields — would produce different canonical bytes and a
/// different id, so every projection C1 delivered would read
/// `content-mismatch` from the day v2 landed.
///
/// Three things are pinned here, over bytes written out by hand rather
/// than produced by the struct under test:
///
/// 1. the document decodes as `DeliveredContent::V1`, not as v2;
/// 2. its canonical bytes round-trip exactly — so `ProjectionContentV1`
///    really is the frozen shape and not a lookalike;
/// 3. `read_referenced` delivers it against the id and receipt digest a
///    C1-era journal would have recorded.
#[test]
fn a_v1_projection_still_reads_as_v1_after_the_format_advanced() {
    let parsed: ProjectionFile = serde_json::from_str(V1_DOCUMENT).expect("the v1 document parses");
    let wirk_core::DeliveredContent::V1(v1) = &parsed.content else {
        panic!(
            "a v1 document must not decode as a later shape: {:?}",
            parsed.content
        );
    };
    assert_eq!(parsed.content.format(), wirk_core::PROJECTION_FORMAT_V1);
    assert_eq!(v1.compilation_policy, wirk_core::ASSEMBLY_POLICY_V1);
    assert_ne!(
        wirk_core::PROJECTION_FORMAT,
        wirk_core::PROJECTION_FORMAT_V1,
        "this test is vacuous unless the format actually advanced"
    );

    // The canonical bytes are the document's own bytes: nothing was
    // added, dropped or reordered by decoding it.
    assert_eq!(
        String::from_utf8(serde_json::to_vec(&parsed).unwrap()).unwrap(),
        V1_DOCUMENT,
        "decoding a v1 file must not change its canonical bytes"
    );

    // The id and the receipt digest a C1-era journal recorded, as
    // literals: sha256 over `b"wirk.projection/v1\0"` and the document's
    // own `content` bytes, and over `b"wirk.projection.receipt/v1\0"`
    // and its `receipt` bytes. Computed outside this crate, so the
    // assertion below cannot be satisfied by moving the hasher — which
    // is exactly the mutation it was watched red against.
    const V1_ID: &str = "0ecb9b6c2cde87311048cc9bc0bcfb069912ffa5458cd9e24f1063e73312935e";
    const V1_RECEIPT: &str = "b4b19773fd511e75e5addfa50d7d5c67bf260b0a8bbfbe55b5eb6045596b7b5f";
    assert_eq!(
        v1.projection_id(),
        ProjectionId(V1_ID.to_string()),
        "a v1 projection's id is taken under the v1 domain tag over its v1 canonical bytes"
    );
    assert_eq!(parsed.receipt.digest(), V1_RECEIPT);

    // And it delivers, against the reference a C1-era journal carries.
    let dir = estate();
    let reference = EvidenceProjectionRef {
        observation: ObservationId("obs-1".to_string()),
        projection: ProjectionId(V1_ID.to_string()),
        revision: 0,
        format: wirk_core::PROJECTION_FORMAT_V1.to_string(),
        receipt: V1_RECEIPT.to_string(),
    };
    let path = wirk_core::projections_dir(dir.path(), &WorkId("work-1".to_string()));
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("obs-1.json"), V1_DOCUMENT).unwrap();
    let read =
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference)
            .expect("a v1 projection still delivers");
    assert_eq!(read, parsed);

    // The v1 id is taken under the v1 domain tag. Re-hashing the same
    // delivered context under v2's tag is a different id, which is
    // exactly why the two shapes may not share one hasher.
    let as_v2 = ProjectionContent {
        format: wirk_core::PROJECTION_FORMAT.to_string(),
        compilation_policy: wirk_core::ASSEMBLY_POLICY.to_string(),
        route_edition: v1.route_edition.clone(),
        waypoint: v1.waypoint.clone(),
        revision: v1.revision,
        question: v1.question.clone(),
        generations: v1.generations.clone(),
        publication_revision: v1.publication_revision,
        retrieval: retrieval_note(),
        bound: v1.bound.clone(),
        referenced: Vec::new(),
        reachable: Vec::new(),
        assumptions: v1.assumptions.clone(),
        unknowns: v1.unknowns.clone(),
        omitted: v1.omitted.clone(),
        next_action: String::new(),
        coverage: v1.coverage,
        truncated: false,
        expansion: None,
        consulted: Vec::new(),
        findings_index: wirk_core::FindingsIndexNote::unobserved(),
    };
    assert_ne!(as_v2.projection_id(), v1.projection_id());
}

/// The **actual historical** v2 document: one real native C3 Run's own
/// delivered projection, harvested verbatim out of the estate that Run
/// wrote it into (`.wirk/native-c3-use`, ruling 0135's trial), together
/// with the exact `ProjectionId` and receipt digest that Run's journal
/// recorded for it.
///
/// A hand-written lookalike would prove nothing here: what has to hold is
/// that a document a *shipped binary* wrote still decodes under the
/// shape that wrote it and still re-hashes to the id a real journal is
/// carrying today. This is why `ProjectionContentV2` is frozen — if
/// `consulted` or `findings_index` were added to it instead, every one of
/// these files would re-hash to something its own journal has never
/// recorded, and every native Run of the C3 trial would read
/// `content-mismatch`.
const V2_DOCUMENT: &str = include_str!("fixtures/native-c3-projection-v2.json");

/// The reference `work-18d3618230ce24ef-0`'s journal carries for it, as
/// literals — read out of that journal, not recomputed here.
const V2_OBSERVATION: &str = "obs-18d361a1bd2a4fb9-2";
const V2_ID: &str = "283bef8256916c6cdeccea04bd4e0c53a3acd8f0cd1d2ff1cedead4b4e25e857";
const V2_RECEIPT: &str = "c95cf65b73e89e6e5e6c42b81908fa0406c629418c4c37684aafdbf09583ac55";

#[test]
fn a_native_c3_projection_still_reads_as_v2_after_the_format_advanced() {
    let parsed: ProjectionFile = serde_json::from_str(V2_DOCUMENT).expect("the v2 document parses");
    let wirk_core::DeliveredContent::V2(v2) = &parsed.content else {
        panic!(
            "a v2 document must not decode as a later shape: {:?}",
            parsed.content
        );
    };
    assert_eq!(parsed.content.format(), wirk_core::PROJECTION_FORMAT_V2);
    assert_eq!(v2.compilation_policy, wirk_core::ASSEMBLY_POLICY_V2);
    assert_ne!(
        wirk_core::PROJECTION_FORMAT,
        wirk_core::PROJECTION_FORMAT_V2,
        "this test is vacuous unless the format actually advanced"
    );

    // The canonical bytes are the document's own bytes: nothing was
    // added, dropped or reordered by decoding it.
    assert_eq!(
        String::from_utf8(serde_json::to_vec(&parsed).unwrap()).unwrap(),
        V2_DOCUMENT,
        "decoding a v2 file must not change its canonical bytes"
    );
    assert_eq!(v2.projection_id(), ProjectionId(V2_ID.to_string()));
    assert_eq!(parsed.receipt.digest(), V2_RECEIPT);

    // And it delivers, against the reference that Run's journal carries.
    let dir = estate();
    let reference = EvidenceProjectionRef {
        observation: ObservationId(V2_OBSERVATION.to_string()),
        projection: ProjectionId(V2_ID.to_string()),
        revision: 0,
        format: wirk_core::PROJECTION_FORMAT_V2.to_string(),
        receipt: V2_RECEIPT.to_string(),
    };
    let path = wirk_core::projections_dir(dir.path(), &WorkId("work-1".to_string()));
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(format!("{V2_OBSERVATION}.json")), V2_DOCUMENT).unwrap();
    let read =
        ProjectionFile::read_referenced(dir.path(), &WorkId("work-1".to_string()), &reference)
            .expect("a native C3 projection still delivers");
    assert_eq!(read, parsed);

    // The same delivered context in the *current* shape is a different
    // id, under a different domain tag — which is the whole reason the
    // two may not share one hasher or one struct.
    let lifted = v2.lift();
    assert_ne!(lifted.projection_id(), v2.projection_id());
    // Lifting says what that revision actually observed, and does not
    // invent a health state for a document that carries none.
    assert!(lifted.consulted.is_empty());
    assert_eq!(
        lifted.findings_index,
        wirk_core::FindingsIndexNote::unobserved()
    );
    // Everything else survives it, so an expansion of a pre-W-C4
    // revision expands the context that revision really delivered.
    assert_eq!(lifted.bound, v2.bound);
    assert_eq!(lifted.referenced, v2.referenced);
    assert_eq!(lifted.reachable, v2.reachable);
    assert_eq!(lifted.generations, v2.generations);
    assert_eq!(lifted.assumptions, v2.assumptions);
}

/// W-C4's own compatibility half, the mirror of the W-C3 test below:
/// adding two *required* fields moves the id of everything written from
/// here on, which is exactly why the tag advanced — and it must not move
/// the id of anything already delivered.
#[test]
fn the_two_consulted_fields_are_required_and_never_reach_an_older_document() {
    let current = content(vec![item("coord-a")]);
    let document = serde_json::to_string(&current).expect("serialize");
    assert!(
        document.contains("\"consulted\":") && document.contains("\"findings_index\":"),
        "both fields are required and are on the wire of every document: {document}"
    );

    // A v3 document does not decode as v2, and a v2 document does not
    // decode as v3: `deny_unknown_fields` on both sides, so the untagged
    // order below cannot silently prefer one.
    assert!(
        serde_json::from_str::<wirk_core::ProjectionContentV2>(&document).is_err(),
        "a v3 document carries fields the frozen v2 shape denies"
    );
    let historical: ProjectionFile =
        serde_json::from_str(V2_DOCUMENT).expect("the v2 document parses");
    let wirk_core::DeliveredContent::V2(v2) = &historical.content else {
        panic!("the historical fixture is v2");
    };
    let v2_document = serde_json::to_string(&**v2).expect("serialize");
    assert!(
        serde_json::from_str::<ProjectionContent>(&v2_document).is_err(),
        "a v2 document lacks fields the current shape requires"
    );
}

/// W-C3, the compatibility property the whole design rests on: adding
/// `expansion` must not move the `ProjectionId` of a projection that was
/// already delivered.
///
/// Two halves, and both are needed. First, a revision-0 document written
/// by this binary carries **no `expansion` key at all** — not
/// `"expansion":null` — so its canonical bytes are the bytes a C2 binary
/// wrote for the same context. Second, a document that never had the key
/// (which is every v2 file written before this wave) parses under this
/// binary's shape and re-hashes to exactly the same id, so the reference
/// its journal recorded still resolves.
///
/// This is why the format tag stays `wirk.projection/v2` rather than
/// advancing: an added field that is absent from the document does not
/// change the document. Give `expansion` a plain `#[serde(default)]`
/// instead of `skip_serializing_if` and both halves fail at once —
/// watched, and the reason the attribute is there rather than being
/// tidied away as noise.
#[test]
fn adding_expansion_does_not_move_an_already_delivered_projection_id() {
    let delivered = content(vec![item("src/server.rs")]);
    assert_eq!(delivered.revision, 0);
    assert!(delivered.expansion.is_none());

    let bytes = delivered.canonical_bytes();
    let text = String::from_utf8(bytes.clone()).expect("canonical bytes are utf-8");
    assert!(
        !text.contains("expansion"),
        "a revision-0 document must not carry the key at all: {text}"
    );

    // The document a pre-C3 binary wrote: the same JSON, with no
    // `expansion` key, parsed under this binary's shape.
    let reparsed: ProjectionContent =
        serde_json::from_slice(&bytes).expect("a keyless document still parses");
    assert!(reparsed.expansion.is_none());
    assert_eq!(
        reparsed.projection_id(),
        delivered.projection_id(),
        "a document written before this field existed must re-hash to the id its journal \
         recorded"
    );
    // And the file-level read path agrees: the same document, through
    // `DeliveredContent`, is still a v2 whose id is unchanged.
    let file: DeliveredContent =
        serde_json::from_slice(&bytes).expect("a keyless document is still a v2");
    assert_eq!(file.declared_format(), wirk_core::PROJECTION_FORMAT);
    assert_eq!(file.projection_id(), delivered.projection_id());
}
