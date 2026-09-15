//! One admitted source whose published generation cannot be read back
//! must not hide another, healthy admitted source's coverage from an
//! estate-wide `search`.
//!
//! `query::search`'s unpinned per-membership walk propagated
//! `store.current(&source.membership)?` and returned `Err` for the
//! *whole* multi-source answer the instant it reached a broken
//! membership, losing a healthy source's genuine hit with it. The
//! pinned walk a few lines above already tolerated the identical
//! condition via `coverage.generation_unavailable`; this is the
//! unpinned walk reaching the same tolerance.
//!
//! The faults below are the real ways a published generation's own
//! derived data stops reading: its directory moved aside, its
//! `manifest.json` no longer parseable JSON, its `manifest.json` no
//! longer an ordinary readable file, and its `resources.ndjson` rows
//! no longer parseable. All four are `AtlasStore`'s own business and
//! all four now classify as `AtlasError::Generation` at the read, so
//! one classification decides them all.
use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, DOCUMENT_TREE_CURRENT_OBSERVATION, ExtractorPolicy, PinnedProducer,
    QueryScope, SearchRequest, SemanticRequest, search,
};

fn staged(outcome: AcquireOutcome) -> wirk_atlas::SourceGeneration {
    match outcome {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    }
}

fn request(scope: QueryScope, query: &str) -> SearchRequest {
    SearchRequest {
        scope,
        requested_source: None,
        query: query.into(),
        families: vec![],
        semantic: SemanticRequest::Disabled,
        limit: 10,
        capacity: None,
        pinned: None,
        offset: 0,
        semantic_query: None,
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: PinnedProducer::Unrecorded,
    }
}

/// A tiny owned document-tree source with one text file, admitted and
/// published under `alias`. Returns the membership and the generation
/// id actually published, so the caller can locate that source's own
/// generation directory and read it back directly.
fn publish_document_tree(
    atlas: &mut AtlasStore,
    alias: &str,
    docs_dir: &std::path::Path,
    file_name: &str,
    body: &str,
) -> (wirk_atlas::Membership, wirk_atlas::GenerationId) {
    fs::create_dir_all(docs_dir).unwrap();
    fs::write(docs_dir.join(file_name), body).unwrap();
    let membership = atlas
        .register_document_tree(alias, docs_dir, DOCUMENT_TREE_CURRENT_OBSERVATION)
        .unwrap();
    let generation = staged(
        atlas
            .acquire_document_tree(
                &membership,
                DOCUMENT_TREE_CURRENT_OBSERVATION,
                ExtractorPolicy::default(),
            )
            .unwrap(),
    );
    atlas.publish(&membership, &generation.id).unwrap();
    (membership, generation.id)
}

/// Where `atlas` keeps one published generation's own derived data.
fn generation_dir(estate: &std::path::Path, generation: &wirk_atlas::GenerationId) -> PathBuf {
    estate.join("atlas").join("generations").join(&generation.0)
}

/// One healthy source's genuine hit must still reach an estate-wide
/// caller, coverage must say plainly that something else is missing,
/// and restoring the moved directory must recover the exact baseline —
/// each an independent assertion the fix must satisfy simultaneously,
/// not just "does not panic".
#[test]
fn healthy_source_survives_a_sibling_sources_unreadable_generation() {
    let estate = TempDir::new().unwrap();
    let sources = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();

    publish_document_tree(
        &mut atlas,
        "alpha",
        &sources.path().join("docs-a"),
        "alpha.txt",
        "the alpha estate boundary concept lives here\n",
    );
    let (_, beta_generation) = publish_document_tree(
        &mut atlas,
        "beta",
        &sources.path().join("docs-b"),
        "beta.txt",
        "the beta estate boundary concept lives here too\n",
    );

    // ---- baseline: both sources healthy ------------------------------
    let baseline = search(
        &atlas,
        &request(QueryScope::EstateOrientation, "boundary concept"),
    )
    .unwrap();
    assert_eq!(
        baseline.hits.len(),
        2,
        "baseline must see both sources: {baseline:?}"
    );
    assert!(
        !baseline.coverage.generation_unavailable,
        "baseline must not already report an unavailable generation"
    );

    // ---- fault injection: move beta's published generation aside -----
    // On this test's own owned fixture estate, against the real store,
    // so the assertion is on `query::search`'s own return value.
    let generation_dir = generation_dir(estate.path(), &beta_generation);
    let backup_dir = estate.path().join("beta-generation-backup");
    assert!(
        generation_dir.is_dir(),
        "fixture must find beta's real generation directory"
    );
    fs::rename(&generation_dir, &backup_dir).unwrap();

    // ---- the unfixed walk turned one broken membership into an `Err`
    // for the entire estate-wide answer, losing alpha's genuine hit
    // along with beta's.
    let after_fault = search(
        &atlas,
        &request(QueryScope::EstateOrientation, "boundary concept"),
    )
    .expect(
        "an unavailable sibling generation must not abort the whole estate-wide search; \
             alpha's healthy coverage must still reach the caller",
    );
    assert_eq!(
        after_fault.hits.len(),
        1,
        "exactly alpha's hit must survive, not beta's (unreadable) and not a spurious duplicate: {after_fault:?}"
    );
    assert!(
        after_fault.coverage.generation_unavailable,
        "the answer must explicitly disclose that one admitted generation went unread, not \
         silently look like a complete two-source search: {after_fault:?}"
    );
    assert!(
        !after_fault.coverage.no_match,
        "a genuine hit exists; this must never be reported as a false no-match"
    );

    // ---- explicit alpha-only scoping keeps working, unaffected -------
    let mut alpha_only = request(QueryScope::EstateOrientation, "boundary concept");
    alpha_only.requested_source = Some("alpha".into());
    let alpha_answer = search(&atlas, &alpha_only)
        .expect("naming the healthy source explicitly must never be affected by beta's state");
    assert_eq!(alpha_answer.hits.len(), 1);
    assert!(!alpha_answer.coverage.generation_unavailable);

    // ---- restoring the directory recovers the exact baseline ---------
    fs::rename(&backup_dir, &generation_dir).unwrap();
    let recovered = search(
        &atlas,
        &request(QueryScope::EstateOrientation, "boundary concept"),
    )
    .unwrap();
    assert_eq!(
        recovered.hits.len(),
        2,
        "recovery must restore both sources' hits: {recovered:?}"
    );
    assert!(
        !recovered.coverage.generation_unavailable,
        "recovery must clear the unavailable disclosure entirely"
    );
}

/// The three ways a generation directory that is *still there* stops
/// reading back. Each is applied to beta alone, and each must leave
/// alpha's genuine hit reaching the caller with the unread portion
/// disclosed — the same outcome as the moved directory above.
///
/// This also establishes the classification directly: `store.current`
/// is asked for beta between each fault, and must answer
/// `AtlasError::Generation`. Before the store change, malformed JSON
/// reached the caller as `AtlasError::Json` and an unreadable
/// `manifest.json` as `AtlasError::Io`, neither of which the
/// source-local branches in `query::search` and `handle_atlas_status`
/// match — so one source's own broken derived data still aborted every
/// admitted source's answer.
#[test]
fn unreadable_generation_data_is_source_local_whatever_broke_it() {
    let estate = TempDir::new().unwrap();
    let sources = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();

    publish_document_tree(
        &mut atlas,
        "alpha",
        &sources.path().join("docs-a"),
        "alpha.txt",
        "the alpha estate boundary concept lives here\n",
    );
    let (beta, beta_generation) = publish_document_tree(
        &mut atlas,
        "beta",
        &sources.path().join("docs-b"),
        "beta.txt",
        "the beta estate boundary concept lives here too\n",
    );
    let dir = generation_dir(estate.path(), &beta_generation);
    let manifest = dir.join("manifest.json");
    let resources = dir.join("resources.ndjson");
    let manifest_bytes = fs::read(&manifest).unwrap();
    let resource_bytes = fs::read(&resources).unwrap();

    // Each fault names the file it breaks; every one is undone the same
    // way, by rewriting this generation's directory from the bytes read
    // above.
    let faults = [
        "manifest.json is no longer parseable JSON",
        "manifest.json is no longer an ordinary readable file",
        "a resources.ndjson row is no longer parseable",
    ];

    for what in faults {
        match what {
            "manifest.json is no longer parseable JSON" => {
                fs::write(&manifest, b"{ not json at all").unwrap();
            }
            // A directory in the manifest's place: `exists()` still says
            // yes and the read fails at `open`/`read` — the
            // `std::io::Error` half, reproduced without depending on
            // this process's uid, which would make a permission-based
            // fault vacuous when the suite runs as root.
            "manifest.json is no longer an ordinary readable file" => {
                fs::remove_file(&manifest).unwrap();
                fs::create_dir(&manifest).unwrap();
            }
            _ => fs::write(&resources, b"{ not a resource row\n").unwrap(),
        }

        let error = atlas
            .current(&beta)
            .expect_err(&format!("beta must not read back when {what}"));
        assert!(
            matches!(error, wirk_atlas::AtlasError::Generation(_)),
            "{what} is this one generation's own derived data failing to read, which is what \
             `AtlasError::Generation` means; classifying it as raw I/O or JSON leaves every \
             caller's source-local branch unable to recognise it: {error:?}"
        );

        let answer = search(
            &atlas,
            &request(QueryScope::EstateOrientation, "boundary concept"),
        )
        .unwrap_or_else(|error| panic!("{what} must not abort the whole search: {error:?}"));
        assert_eq!(
            answer.hits.len(),
            1,
            "alpha's genuine hit must survive {what}: {answer:?}"
        );
        assert!(
            answer.coverage.generation_unavailable,
            "{what} must be disclosed, not silently dropped: {answer:?}"
        );
        assert!(
            !answer.coverage.no_match && !answer.coverage.is_complete(),
            "{what} leaves the answer short; it is neither complete nor a proven absence: \
             {answer:?}"
        );

        fs::remove_dir_all(&dir).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(&manifest, &manifest_bytes).unwrap();
        fs::write(&resources, &resource_bytes).unwrap();
        let recovered = search(
            &atlas,
            &request(QueryScope::EstateOrientation, "boundary concept"),
        )
        .unwrap_or_else(|error| panic!("restoring after {what} must recover: {error:?}"));
        assert_eq!(
            recovered.hits.len(),
            2,
            "restored after {what}: {recovered:?}"
        );
        assert!(!recovered.coverage.generation_unavailable);
    }
}

/// Every admitted generation unreadable at once. There is no healthy
/// sibling left to preserve, so the only thing left to get right is what
/// the answer *says*: an empty hit list here is missing evidence, never
/// a searched-and-empty corpus, and never a complete answer.
#[test]
fn every_admitted_generation_unavailable_is_incomplete_not_a_no_match() {
    let estate = TempDir::new().unwrap();
    let sources = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();

    let (_, alpha_generation) = publish_document_tree(
        &mut atlas,
        "alpha",
        &sources.path().join("docs-a"),
        "alpha.txt",
        "the alpha estate boundary concept lives here\n",
    );
    let (_, beta_generation) = publish_document_tree(
        &mut atlas,
        "beta",
        &sources.path().join("docs-b"),
        "beta.txt",
        "the beta estate boundary concept lives here too\n",
    );

    let baseline = search(
        &atlas,
        &request(QueryScope::EstateOrientation, "boundary concept"),
    )
    .unwrap();
    assert_eq!(baseline.hits.len(), 2);
    assert!(baseline.coverage.is_complete());
    let captured_generations = baseline.generations.clone();

    // Two different faults at once, so this is not the single-fault case
    // repeated: alpha's directory is moved aside, beta's manifest is
    // corrupted in place.
    let alpha_dir = generation_dir(estate.path(), &alpha_generation);
    let alpha_backup = estate.path().join("alpha-generation-backup");
    fs::rename(&alpha_dir, &alpha_backup).unwrap();
    let beta_manifest = generation_dir(estate.path(), &beta_generation).join("manifest.json");
    let beta_manifest_bytes = fs::read(&beta_manifest).unwrap();
    fs::write(&beta_manifest, b"{ not json at all").unwrap();

    let answer = search(
        &atlas,
        &request(QueryScope::EstateOrientation, "boundary concept"),
    )
    .expect("nothing readable is still an answer, not a whole-call failure");
    assert!(answer.hits.is_empty(), "nothing was readable: {answer:?}");
    assert!(
        answer.coverage.generation_unavailable && answer.coverage.partial,
        "the caller must be told the corpus went unread: {answer:?}"
    );
    assert!(
        !answer.coverage.no_match,
        "nothing was searched, so this can never assert the corpus held nothing: {answer:?}"
    );
    assert!(
        !answer.coverage.is_complete(),
        "an answer over an entirely unread corpus is not complete: {answer:?}"
    );
    assert!(
        !answer.coverage.denied && !answer.coverage.no_sources,
        "both sources are registered and admitted; this is unreadable data, not a denial and \
         not a fresh estate: {answer:?}"
    );
    assert!(
        answer.generations.is_empty(),
        "no generation was read, so none may be offered as the vector this answer read: \
         {answer:?}"
    );

    // ---- restoration returns ordinary operation, on the same identity -
    fs::rename(&alpha_backup, &alpha_dir).unwrap();
    fs::write(&beta_manifest, &beta_manifest_bytes).unwrap();
    let recovered = search(
        &atlas,
        &request(QueryScope::EstateOrientation, "boundary concept"),
    )
    .unwrap();
    assert_eq!(recovered.hits.len(), 2, "{recovered:?}");
    assert!(recovered.coverage.is_complete(), "{recovered:?}");
    assert_eq!(
        recovered.generations, captured_generations,
        "restoration must return the same published generations, not re-derive new ones"
    );
}

/// A continuation captured while both sources were healthy names its own
/// generations. A fault on one of them must leave that captured identity
/// intact — the pinned page discloses the unread half rather than
/// silently re-pinning to something else — and restoration must replay
/// the captured vector exactly.
#[test]
fn a_captured_continuation_keeps_its_generation_identity_through_restoration() {
    let estate = TempDir::new().unwrap();
    let sources = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();

    publish_document_tree(
        &mut atlas,
        "alpha",
        &sources.path().join("docs-a"),
        "alpha.txt",
        "the alpha estate boundary concept lives here\n",
    );
    let (_, beta_generation) = publish_document_tree(
        &mut atlas,
        "beta",
        &sources.path().join("docs-b"),
        "beta.txt",
        "the beta estate boundary concept lives here too\n",
    );

    let captured = search(
        &atlas,
        &request(QueryScope::EstateOrientation, "boundary concept"),
    )
    .unwrap()
    .generations;
    assert_eq!(captured.len(), 2);
    let pinned: std::collections::BTreeMap<_, _> = captured.iter().cloned().collect();

    let mut pinned_request = request(QueryScope::EstateOrientation, "boundary concept");
    pinned_request.pinned = Some(pinned.clone());

    let dir = generation_dir(estate.path(), &beta_generation);
    let manifest = dir.join("manifest.json");
    let manifest_bytes = fs::read(&manifest).unwrap();
    fs::write(&manifest, b"{ not json at all").unwrap();

    let during_fault = search(&atlas, &pinned_request)
        .expect("a continuation pinned across a broken generation still answers");
    assert_eq!(during_fault.hits.len(), 1, "{during_fault:?}");
    assert!(during_fault.coverage.generation_unavailable);
    let expected: Vec<_> = captured
        .iter()
        .filter(|(_, generation)| generation != &beta_generation)
        .cloned()
        .collect();
    assert_eq!(
        during_fault.generations, expected,
        "the page reports the generations it actually read, and does not invent one for the \
         source it could not: {during_fault:?}"
    );

    fs::write(&manifest, &manifest_bytes).unwrap();
    let replayed = search(&atlas, &pinned_request).expect("the same pinned request after recovery");
    assert_eq!(
        replayed.generations, captured,
        "the captured continuation identity must survive the fault and its repair unchanged"
    );
    assert_eq!(replayed.hits.len(), 2, "{replayed:?}");
    assert!(replayed.coverage.is_complete(), "{replayed:?}");
}
