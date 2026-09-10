//! W2-CORRECTION.md / ruling 0077. Each test here reproduces one of
//! source-verify-w2's independently executed failures and is meant to be
//! watched red against the pre-correction candidate before the matching
//! fix lands. Historical builder/reviewer evidence under
//! `knowledge/work/p3-sources/source-verify-w2/` and
//! `source-build-w2/` is not edited; this file is new, additive coverage.
use std::fs;
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, ExtractorPolicy, RelationshipKind, SearchRequest, SemanticRequest,
    admit_relationship, search,
};
use wirk_core::{Access, RepositoryBinding};

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repo_with(files: &[(&str, &str)]) -> (TempDir, String) {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["config", "user.email", "atlas@example.test"]);
    git(temp.path(), &["config", "user.name", "Atlas"]);
    for (name, content) in files {
        fs::write(temp.path().join(name), content).unwrap();
    }
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-qm", "one"]);
    let commit = git(temp.path(), &["rev-parse", "HEAD"]);
    (temp, commit)
}

fn work(names: &[&str], access: Access) -> wirk_atlas::QueryScope {
    wirk_atlas::QueryScope::Work(
        names
            .iter()
            .map(|name| RepositoryBinding {
                name: (*name).into(),
                access,
            })
            .collect(),
    )
}

// -- Failure 1: coverage.no_match must be derived from search completeness,
// -- not from the post-truncation/unavailable hit list. --

#[test]
fn zero_presentation_budget_is_truncated_not_reported_as_no_match() {
    // Three separate files, not three lines of one file: the default (`v4`)
    // extractor packs consecutive short lines of one file into a single
    // unit up to its 65536-byte budget (`wirk-atlas/src/extract.rs`), so
    // three short lines of one small file would derive as one unit, not
    // three. Three distinct resources still derive three distinct units
    // regardless of packing, which is what this positive control needs.
    let (repo, rev) = repo_with(&[
        ("one.md", "alpha one\n"),
        ("two.md", "alpha two\n"),
        ("three.md", "alpha three\n"),
    ]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas.register_git("docs", repo.path(), "HEAD").unwrap();
    let generation = match atlas
        .acquire(&membership, &rev, ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas.publish(&membership, &generation.id).unwrap();

    let request = |limit: usize| SearchRequest {
        scope: work(&["docs"], Access::Read),
        requested_source: None,
        query: "alpha".into(),
        families: vec![],
        semantic: SemanticRequest::Disabled,
        limit,
        capacity: None,
        pinned: None,
        offset: 0,
        semantic_query: None,
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    };

    let full = search(&atlas, &request(10)).unwrap();
    assert_eq!(full.hits.len(), 3, "positive control: three matching units");
    assert!(!full.coverage.no_match);
    assert!(!full.truncated);

    let partial = search(&atlas, &request(1)).unwrap();
    assert!(partial.truncated);
    assert!(partial.coverage.partial);
    assert!(!partial.coverage.no_match, "one-of-three is not no_match");

    let zero = search(&atlas, &request(0)).unwrap();
    assert!(
        zero.truncated,
        "limit=0 over 3 real matches must be truncated"
    );
    assert!(
        !zero.coverage.no_match,
        "CONTRACT FAILURE (source-verify-w2 Failure 1 / probe_a2): a zero \
         presentation budget reported coverage.no_match=true while \
         truncated=true, turning found evidence into reported absence"
    );
}

#[test]
fn an_unavailable_source_is_not_reported_as_no_match() {
    let (repo, rev) = repo_with(&[("lib.rs", "fn alpha() {}\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let generation = match atlas
        .acquire(&membership, &rev, ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas.publish(&membership, &generation.id).unwrap();
    drop(repo); // the TempDir is removed on drop, so the blob read now fails.

    let answer = search(
        &atlas,
        &SearchRequest {
            scope: work(&["source"], Access::Read),
            requested_source: None,
            query: "alpha".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            capacity: None,
            pinned: None,
            offset: 0,
            semantic_query: None,
            pinned_editions: None,
            pinned_mode: None,
            pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
        },
    )
    .unwrap();
    assert!(answer.coverage.source_unavailable, "positive control");
    assert!(
        !answer.coverage.no_match,
        "CONTRACT FAILURE (source-verify-w2 Failure 1 / probe_b2): a source \
         whose blob could not be read was reported as coverage.no_match, an \
         unindexed/unreadable family reported as authoritative absence"
    );
}

// -- Failure 3: append_relationship's post-rename directory-fsync failure
// -- must report DurabilityUncertain, matching persist_catalog. Reuses the
// -- candidate's own existing real fsync-failure injection
// -- (tests/fixtures/fail_nth_dir_fsync.c) exactly as W1's
// -- recovery_contract.rs already does for the catalog path; no new
// -- failpoint infrastructure is added. --

const DIR_FSYNC_HANDOFF: &str = "W2_CORRECTION_DIR_FSYNC_HANDOFF";

#[test]
fn child_admits_relationship_under_injected_dir_fsync_failure() {
    let Some(handoff) = std::env::var_os(DIR_FSYNC_HANDOFF) else {
        return;
    };
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&handoff).unwrap()).unwrap();
    let estate = std::path::PathBuf::from(value["estate"].as_str().unwrap());
    let from: wirk_atlas::ExactCoordinate = serde_json::from_value(value["from"].clone()).unwrap();
    let to: wirk_atlas::ExactCoordinate = serde_json::from_value(value["to"].clone()).unwrap();
    let mut atlas = AtlasStore::open(&estate, "estate-a").unwrap();
    let scope = work(&["code", "knowledge"], Access::Read);
    let result = admit_relationship(
        &mut atlas,
        &scope,
        None,
        RelationshipKind::GovernedBy,
        from.clone(),
        to,
        vec![from],
        "w2-correction/v1",
    );
    println!("CHILD-RESULT={result:?}");
    let log = estate.join("atlas/relationships.ndjson");
    println!(
        "CHILD-ONDISK=exists={} bytes={}",
        log.exists(),
        fs::read(&log).map(|b| b.len()).unwrap_or(0)
    );
}

#[test]
fn append_relationship_reports_durability_uncertain_on_post_rename_dir_fsync_failure() {
    let (code_repo, code_rev) = repo_with(&[("lib.rs", "pub fn validate_claim() {}\n")]);
    let (knowledge_repo, knowledge_rev) =
        repo_with(&[("contract.md", "a claim is validated first\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let code_member = atlas
        .register_git("code", code_repo.path(), "HEAD")
        .unwrap();
    let code_generation = match atlas
        .acquire(&code_member, &code_rev, ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas.publish(&code_member, &code_generation.id).unwrap();
    let knowledge_member = atlas
        .register_git("knowledge", knowledge_repo.path(), "HEAD")
        .unwrap();
    let knowledge_generation = match atlas
        .acquire(
            &knowledge_member,
            &knowledge_rev,
            ExtractorPolicy::default(),
        )
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas
        .publish(&knowledge_member, &knowledge_generation.id)
        .unwrap();

    let scope = work(&["code", "knowledge"], Access::Read);
    let pick = |atlas: &AtlasStore, source: &str, query: &str| {
        let answer = search(
            atlas,
            &SearchRequest {
                scope: scope.clone(),
                requested_source: Some(source.into()),
                query: query.into(),
                families: vec![],
                semantic: SemanticRequest::Disabled,
                limit: 5,
                capacity: None,
                pinned: None,
                offset: 0,
                semantic_query: None,
                pinned_editions: None,
                pinned_mode: None,
                pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
            },
        )
        .unwrap();
        assert!(!answer.hits.is_empty(), "positive control {source}/{query}");
        answer.hits[0].coordinate.clone()
    };
    let from = pick(&atlas, "code", "validate_claim");
    let to = pick(&atlas, "knowledge", "validated");
    drop(atlas);

    let handoff = estate.path().join("handoff.json");
    fs::write(
        &handoff,
        serde_json::to_vec(&serde_json::json!({
            "estate": estate.path().to_string_lossy(),
            "from": from,
            "to": to,
        }))
        .unwrap(),
    )
    .unwrap();

    let shim = estate.path().join("fail_nth_dir_fsync.so");
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fail_nth_dir_fsync.c");
    let compiled = Command::new("/usr/bin/cc")
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-o")
        .arg(&shim)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    // Directory fsync #1 is AtlasStore::open's recovery confirm in the
    // child; #2 is append_relationship's post-rename directory sync, the
    // window this test targets.
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_admits_relationship_under_injected_dir_fsync_failure",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("LD_PRELOAD", &shim)
        .env("W1_FAIL_DIRECTORY_FSYNC_CALL", "2")
        .env(DIR_FSYNC_HANDOFF, &handoff)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&child.stdout).to_string()
        + &String::from_utf8_lossy(&child.stderr);
    println!("--- child output ---\n{text}\n--- end child ---");

    let reported = text
        .lines()
        .find(|line| line.contains("CHILD-RESULT="))
        .expect("child never reported a result")
        .to_owned();
    let on_disk = text
        .lines()
        .find(|line| line.contains("CHILD-ONDISK="))
        .expect("child never reported disk state")
        .to_owned();
    println!("reported={reported} on_disk={on_disk}");

    // Whatever the child reported, a fresh handle must see either the
    // relationship durably present or absent, never a torn log.
    let reopened = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let rows = reopened.relationships();
    assert!(
        rows.is_ok(),
        "post-failure reopen found a torn relationship log"
    );

    assert!(
        reported.contains("DurabilityUncertain"),
        "CONTRACT FAILURE (source-verify-w2 Failure 3 / probe_e1): {reported} \
         (disk state {on_disk}); W1's catalog path reports this exact \
         post-rename directory-sync window as AtlasError::DurabilityUncertain \
         so a caller cannot mistake a visible-but-unconfirmed write for one \
         that never happened"
    );

    // The relationship must actually be visible (rename already made it so)
    // despite the reported uncertainty.
    let rows = rows.unwrap();
    assert_eq!(rows.len(), 1, "relationship must be visible after rename");

    // Retrying the identical admission after a DurabilityUncertain report
    // must be idempotent: no duplicate, no lost row.
    let mut retry_atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let retried = admit_relationship(
        &mut retry_atlas,
        &scope,
        None,
        RelationshipKind::GovernedBy,
        from.clone(),
        to,
        vec![from],
        "w2-correction/v1",
    )
    .unwrap();
    assert_eq!(retried.id, rows[0].id);
    assert_eq!(
        retry_atlas.relationships().unwrap().len(),
        1,
        "retry duplicated a row"
    );
}

// -- Ruling 0077: recording an evidenced GovernedBy assertion is distinct
// -- from source mutation authority. Read grants suffice to admit a
// -- relationship at this trusted-library scope (deliberately, not a
// -- defect); this positive test proves that doing so never mutates either
// -- underlying Git repository or grants any further access. --

fn recursive_snapshot(root: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    use sha2::{Digest, Sha256};
    fn walk(
        dir: &std::path::Path,
        root: &std::path::Path,
        out: &mut std::collections::BTreeMap<String, String>,
    ) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                walk(&path, root, out);
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let bytes = fs::read(&path).unwrap();
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let digest = hasher.finalize();
                let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
                out.insert(relative, hex);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn admit_relationship_with_read_only_grants_never_mutates_either_repository() {
    let (code_repo, code_rev) = repo_with(&[("lib.rs", "pub fn validate_claim() {}\n")]);
    let (knowledge_repo, knowledge_rev) =
        repo_with(&[("contract.md", "a claim is validated first\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let code_member = atlas
        .register_git("code", code_repo.path(), "HEAD")
        .unwrap();
    let code_generation = match atlas
        .acquire(&code_member, &code_rev, ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas.publish(&code_member, &code_generation.id).unwrap();
    let knowledge_member = atlas
        .register_git("knowledge", knowledge_repo.path(), "HEAD")
        .unwrap();
    let knowledge_generation = match atlas
        .acquire(
            &knowledge_member,
            &knowledge_rev,
            ExtractorPolicy::default(),
        )
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas
        .publish(&knowledge_member, &knowledge_generation.id)
        .unwrap();

    let scope = work(&["code", "knowledge"], Access::Read);
    let pick = |atlas: &AtlasStore, source: &str, query: &str| {
        let answer = search(
            atlas,
            &SearchRequest {
                scope: scope.clone(),
                requested_source: Some(source.into()),
                query: query.into(),
                families: vec![],
                semantic: SemanticRequest::Disabled,
                limit: 5,
                capacity: None,
                pinned: None,
                offset: 0,
                semantic_query: None,
                pinned_editions: None,
                pinned_mode: None,
                pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
            },
        )
        .unwrap();
        assert!(!answer.hits.is_empty(), "positive control {source}/{query}");
        answer.hits[0].coordinate.clone()
    };
    let from = pick(&atlas, "code", "validate_claim");
    let to = pick(&atlas, "knowledge", "validated");

    let code_before = recursive_snapshot(code_repo.path());
    let knowledge_before = recursive_snapshot(knowledge_repo.path());

    // Positive control: both grants are Read, and the relationship still
    // publishes. This is ruling 0077's adjudicated behaviour, not a defect
    // to gate on Write — see admission.rs/relationship.rs doc comments.
    admit_relationship(
        &mut atlas,
        &scope,
        None,
        RelationshipKind::GovernedBy,
        from.clone(),
        to.clone(),
        vec![to.clone()],
        "w2-correction/v1",
    )
    .unwrap();
    assert_eq!(atlas.relationships().unwrap().len(), 1, "positive control");

    let code_after = recursive_snapshot(code_repo.path());
    let knowledge_after = recursive_snapshot(knowledge_repo.path());
    assert_eq!(
        code_before, code_after,
        "admit_relationship must never mutate a Read source repository"
    );
    assert_eq!(
        knowledge_before, knowledge_after,
        "admit_relationship must never mutate a Read source repository"
    );

    // EstateOrientation (no per-source access at all) can publish too, at
    // this same trusted-library scope; it still touches only Atlas state.
    let orientation_from = pick(&atlas, "code", "validate_claim");
    let orientation_to = pick(&atlas, "knowledge", "validated");
    admit_relationship(
        &mut atlas,
        &wirk_atlas::QueryScope::EstateOrientation,
        None,
        RelationshipKind::GovernedBy,
        orientation_from,
        orientation_to.clone(),
        vec![orientation_to],
        "w2-correction/orientation-probe",
    )
    .unwrap();
    assert_eq!(recursive_snapshot(code_repo.path()), code_after);
    assert_eq!(recursive_snapshot(knowledge_repo.path()), knowledge_after);
}
