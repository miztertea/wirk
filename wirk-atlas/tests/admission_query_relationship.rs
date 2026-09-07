use std::fs;
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, ExactCoordinate, ExtractorPolicy, PathLookupOutcome,
    PathLookupRequest, QueryScope, RelationshipKind, RelationshipView, SearchAnswer, SearchRequest,
    SemanticRequest, SemanticStatus, SourceGeneration, admit_relationship, relationships_for,
    resolve_path, search,
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

fn coordinate(
    member: &wirk_atlas::Membership,
    generation: &SourceGeneration,
    path: &[u8],
) -> ExactCoordinate {
    let record = generation
        .resources
        .iter()
        .find(|record| record.path == path)
        .unwrap();
    let unit = record.units.first().unwrap();
    ExactCoordinate {
        estate: member.estate.clone(),
        membership: member.id.clone(),
        source: member.source.clone(),
        generation: generation.id.clone(),
        path: path.to_vec(),
        object_id: record.object_id.clone().unwrap(),
        byte_start: unit.byte_start,
        byte_end: unit.byte_end,
        line_start: unit.line_start,
        line_end: unit.line_end,
    }
}

fn acquire_and_publish(
    atlas: &mut AtlasStore,
    alias: &str,
    repo: &std::path::Path,
    rev: &str,
) -> (wirk_atlas::Membership, SourceGeneration) {
    let membership = atlas.register_git(alias, repo, "HEAD").unwrap();
    let generation = match atlas
        .acquire(&membership, rev, ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas.publish(&membership, &generation.id).unwrap();
    (membership, generation)
}

fn work(names: &[&str], access: Access) -> QueryScope {
    QueryScope::Work(
        names
            .iter()
            .map(|name| RepositoryBinding {
                name: (*name).into(),
                access,
            })
            .collect(),
    )
}

#[test]
fn work_filters_sources_before_ranking() {
    let (trusted_repo, trusted_rev) = repo_with(&[("lib.rs", "fn validate_claim() { true }\n")]);
    let (distractor_repo, distractor_rev) = repo_with(&[(
        "lib.rs",
        "fn validate_claim() {} // validate_claim validate_claim validate_claim validate_claim\n",
    )]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    acquire_and_publish(&mut atlas, "trusted", trusted_repo.path(), &trusted_rev);
    acquire_and_publish(
        &mut atlas,
        "distractor",
        distractor_repo.path(),
        &distractor_rev,
    );

    let answer = search(
        &atlas,
        &SearchRequest {
            scope: work(&["trusted"], Access::Read),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();

    assert_eq!(answer.admission.admitted, 1);
    assert_eq!(answer.admission.denied, 1);
    assert!(!answer.hits.is_empty());
    assert!(answer.hits.iter().all(|hit| {
        hit.coordinate.source.0 != "distractor"
            && atlas
                .memberships()
                .find(|m| m.id == hit.coordinate.membership)
                .unwrap()
                .alias
                == "trusted"
    }));
}

#[test]
fn read_binding_allows_retrieval_but_grants_no_write() {
    let (repo, rev) = repo_with(&[("lib.rs", "fn validate_claim() {}\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    acquire_and_publish(&mut atlas, "source", repo.path(), &rev);

    let answer = search(
        &atlas,
        &SearchRequest {
            scope: work(&["source"], Access::Read),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();

    assert!(!answer.hits.is_empty());
    assert_eq!(answer.admission.admitted, 1);
    assert_eq!(answer.admission.denied, 0);
}

#[test]
fn query_captures_one_coherent_generation_vector_during_refresh() {
    let (repo, first) = repo_with(&[("lib.rs", "fn validate_claim() { old() }\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let (member, generation_a) = acquire_and_publish(&mut atlas, "source", repo.path(), &first);

    let answer_a = search(
        &atlas,
        &SearchRequest {
            scope: work(&["source"], Access::Read),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();
    assert_eq!(
        answer_a.generations,
        vec![(member.id.clone(), generation_a.id.clone())]
    );

    fs::write(repo.path(), "").ok();
    fs::write(
        repo.path().join("lib.rs"),
        "fn validate_claim() { new() }\n",
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "two"]);
    let second = git(repo.path(), &["rev-parse", "HEAD"]);
    let staged = match atlas
        .acquire(&member, &second, ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    atlas.publish(&member, &staged.id).unwrap();

    let old_coordinate = coordinate(&member, &generation_a, b"lib.rs");
    assert!(matches!(
        atlas.resolve_exact(&member, &old_coordinate).unwrap(),
        wirk_atlas::ResolveOutcome::Resolved(_)
    ));

    let answer_b = search(
        &atlas,
        &SearchRequest {
            scope: work(&["source"], Access::Read),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();
    assert_eq!(answer_b.generations, vec![(member.id, staged.id)]);
    assert_ne!(answer_a.generations, answer_b.generations);
}

#[test]
fn semantic_requested_unavailable_and_disabled_are_distinct() {
    let (repo, rev) = repo_with(&[("lib.rs", "fn validate_claim() {}\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    acquire_and_publish(&mut atlas, "source", repo.path(), &rev);

    let requested = search(
        &atlas,
        &SearchRequest {
            scope: work(&["source"], Access::Read),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Requested,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();
    let disabled = search(
        &atlas,
        &SearchRequest {
            scope: work(&["source"], Access::Read),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();

    assert!(matches!(requested.semantic, SemanticStatus::Unavailable(_)));
    assert_eq!(disabled.semantic, SemanticStatus::Disabled);
    assert_ne!(requested.semantic, disabled.semantic);
}

#[test]
fn exact_path_lookup_is_independent_of_unit_boundaries_and_honors_budget() {
    let big = "x".repeat(200_000) + "\n";
    let (repo, rev) = repo_with(&[("lib.rs", &big)]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    acquire_and_publish(&mut atlas, "source", repo.path(), &rev);

    let bounded = resolve_path(
        &atlas,
        &PathLookupRequest {
            scope: work(&["source"], Access::Read),
            source: "source".into(),
            path: b"lib.rs".to_vec(),
            budget_bytes: 1_000,
        },
    )
    .unwrap();
    match bounded {
        PathLookupOutcome::Resolved {
            bytes,
            total_bytes,
            truncated,
            ..
        } => {
            assert!(truncated);
            assert_eq!(bytes.len(), 1_000);
            assert_eq!(total_bytes, big.len() as u64);
        }
        other => panic!("{other:?}"),
    }

    let full = resolve_path(
        &atlas,
        &PathLookupRequest {
            scope: work(&["source"], Access::Read),
            source: "source".into(),
            path: b"lib.rs".to_vec(),
            budget_bytes: 10_000_000,
        },
    )
    .unwrap();
    match full {
        PathLookupOutcome::Resolved {
            bytes, truncated, ..
        } => {
            assert!(!truncated);
            assert_eq!(bytes.len(), big.len());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn denied_and_absent_disposition_distinctions_stay_honest() {
    let (repo, rev) = repo_with(&[
        ("kept.rs", "fn ok() {}\n"),
        ("secret.pem", "nope\n"),
        ("binary.bin", "a\0b"),
    ]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    acquire_and_publish(&mut atlas, "trusted", repo.path(), &rev);
    let (distractor_repo, distractor_rev) = repo_with(&[("kept.rs", "fn other() {}\n")]);
    acquire_and_publish(
        &mut atlas,
        "distractor",
        distractor_repo.path(),
        &distractor_rev,
    );
    fs::remove_dir_all(distractor_repo.path()).unwrap();

    let denied = resolve_path(
        &atlas,
        &PathLookupRequest {
            scope: work(&["trusted"], Access::Read),
            source: "distractor".into(),
            path: b"kept.rs".to_vec(),
            budget_bytes: 1_000,
        },
    )
    .unwrap();
    assert_eq!(denied, PathLookupOutcome::Denied);

    let excluded = resolve_path(
        &atlas,
        &PathLookupRequest {
            scope: work(&["trusted"], Access::Read),
            source: "trusted".into(),
            path: b"secret.pem".to_vec(),
            budget_bytes: 1_000,
        },
    )
    .unwrap();
    assert!(matches!(excluded, PathLookupOutcome::Excluded(_)));

    let unsupported = resolve_path(
        &atlas,
        &PathLookupRequest {
            scope: work(&["trusted"], Access::Read),
            source: "trusted".into(),
            path: b"binary.bin".to_vec(),
            budget_bytes: 1_000,
        },
    )
    .unwrap();
    assert!(matches!(unsupported, PathLookupOutcome::Unsupported(_)));

    let absent = resolve_path(
        &atlas,
        &PathLookupRequest {
            scope: work(&["trusted"], Access::Read),
            source: "trusted".into(),
            path: b"never-existed.rs".to_vec(),
            budget_bytes: 1_000,
        },
    )
    .unwrap();
    assert_eq!(absent, PathLookupOutcome::Absent);
}

#[test]
fn estate_scope_never_crosses_even_with_colliding_aliases() {
    let (repo_a, rev_a) = repo_with(&[("lib.rs", "fn validate_claim() { a() }\n")]);
    let (repo_b, rev_b) = repo_with(&[("lib.rs", "fn validate_claim() { b_secret() }\n")]);
    let estate_a_root = TempDir::new().unwrap();
    let estate_b_root = TempDir::new().unwrap();
    let mut atlas_a = AtlasStore::open(estate_a_root.path(), "estate-a").unwrap();
    let mut atlas_b = AtlasStore::open(estate_b_root.path(), "estate-b").unwrap();
    let (member_a, _) = acquire_and_publish(&mut atlas_a, "wirk", repo_a.path(), &rev_a);
    let (member_b, generation_b) = acquire_and_publish(&mut atlas_b, "wirk", repo_b.path(), &rev_b);

    let foreign_coordinate = coordinate(&member_b, &generation_b, b"lib.rs");
    assert!(
        atlas_a
            .resolve_exact(&member_a, &foreign_coordinate)
            .is_err()
    );

    let answer = search(
        &atlas_a,
        &SearchRequest {
            scope: work(&["wirk"], Access::Read),
            requested_source: None,
            query: "b_secret".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();
    assert!(answer.hits.is_empty());
}

#[test]
fn relationship_requires_and_resolves_all_exact_evidence() {
    let (code_repo, code_rev) = repo_with(&[("lib.rs", "fn validate_claim() {}\n")]);
    let (knowledge_repo, knowledge_rev) =
        repo_with(&[("contract.md", "validate_claim governs completion.\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let (code_member, code_generation) =
        acquire_and_publish(&mut atlas, "wirk", code_repo.path(), &code_rev);
    let (knowledge_member, knowledge_generation) = acquire_and_publish(
        &mut atlas,
        "workspace",
        knowledge_repo.path(),
        &knowledge_rev,
    );

    let from = coordinate(&code_member, &code_generation, b"lib.rs");
    let mut to = coordinate(&knowledge_member, &knowledge_generation, b"contract.md");

    let scope = work(&["wirk", "workspace"], Access::Read);
    let ok = admit_relationship(
        &mut atlas,
        &scope,
        None,
        RelationshipKind::GovernedBy,
        from.clone(),
        to.clone(),
        vec![to.clone()],
        "explicit-admission/v1",
    );
    assert!(ok.is_ok(), "{ok:?}");
    assert_eq!(atlas.relationships().unwrap().len(), 1);

    to.object_id = "0".repeat(40);
    let broken = admit_relationship(
        &mut atlas,
        &scope,
        None,
        RelationshipKind::GovernedBy,
        from,
        to.clone(),
        vec![to],
        "explicit-admission/v1",
    );
    assert!(broken.is_err());
    assert_eq!(atlas.relationships().unwrap().len(), 1);
}

#[test]
fn retry_does_not_fabricate_duplicate_relationships() {
    let (code_repo, code_rev) = repo_with(&[("lib.rs", "fn validate_claim() {}\n")]);
    let (knowledge_repo, knowledge_rev) =
        repo_with(&[("contract.md", "validate_claim governs completion.\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let (code_member, code_generation) =
        acquire_and_publish(&mut atlas, "wirk", code_repo.path(), &code_rev);
    let (knowledge_member, knowledge_generation) = acquire_and_publish(
        &mut atlas,
        "workspace",
        knowledge_repo.path(),
        &knowledge_rev,
    );
    let from = coordinate(&code_member, &code_generation, b"lib.rs");
    let to = coordinate(&knowledge_member, &knowledge_generation, b"contract.md");
    let scope = work(&["wirk", "workspace"], Access::Read);

    for _ in 0..3 {
        admit_relationship(
            &mut atlas,
            &scope,
            None,
            RelationshipKind::GovernedBy,
            from.clone(),
            to.clone(),
            vec![to.clone()],
            "explicit-admission/v1",
        )
        .unwrap();
    }
    assert_eq!(atlas.relationships().unwrap().len(), 1);
}

#[test]
fn filtered_relationship_does_not_leak_its_hidden_endpoint() {
    let (code_repo, code_rev) = repo_with(&[("lib.rs", "fn validate_claim() {}\n")]);
    let (knowledge_repo, knowledge_rev) =
        repo_with(&[("contract.md", "validate_claim governs completion.\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let (code_member, code_generation) =
        acquire_and_publish(&mut atlas, "wirk", code_repo.path(), &code_rev);
    let (knowledge_member, knowledge_generation) = acquire_and_publish(
        &mut atlas,
        "workspace",
        knowledge_repo.path(),
        &knowledge_rev,
    );
    let from = coordinate(&code_member, &code_generation, b"lib.rs");
    let to = coordinate(&knowledge_member, &knowledge_generation, b"contract.md");
    let full_scope = work(&["wirk", "workspace"], Access::Read);
    admit_relationship(
        &mut atlas,
        &full_scope,
        None,
        RelationshipKind::GovernedBy,
        from.clone(),
        to.clone(),
        vec![to],
        "explicit-admission/v1",
    )
    .unwrap();

    let disclosed = relationships_for(&atlas, &full_scope, None, &from).unwrap();
    assert_eq!(disclosed.len(), 1);
    assert!(matches!(disclosed[0], RelationshipView::Disclosed(_)));

    let narrow_scope = work(&["wirk"], Access::Read);
    let filtered = relationships_for(&atlas, &narrow_scope, None, &from).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], RelationshipView::Filtered);
}

#[test]
fn claim_implementation_and_governing_workspace_contract_resolve_together() {
    let (code_repo, code_rev) = repo_with(&[(
        "lib.rs",
        "pub fn validate_claim(evidence: &str) -> bool {\n    !evidence.is_empty()\n}\n",
    )]);
    let (knowledge_repo, knowledge_rev) = repo_with(&[(
        "contract.md",
        "# Claim contract\n\nA Claim is accepted only when validate_claim confirms real evidence.\n",
    )]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let (code_member, code_generation) =
        acquire_and_publish(&mut atlas, "wirk", code_repo.path(), &code_rev);
    let (knowledge_member, knowledge_generation) = acquire_and_publish(
        &mut atlas,
        "workspace",
        knowledge_repo.path(),
        &knowledge_rev,
    );

    let scope = work(&["wirk", "workspace"], Access::Read);
    let answer: SearchAnswer = search(
        &atlas,
        &SearchRequest {
            scope: scope.clone(),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();
    assert!(answer.coverage.is_complete() || !answer.truncated);
    let membership_ids: Vec<_> = answer
        .hits
        .iter()
        .map(|hit| hit.coordinate.membership.clone())
        .collect();
    assert!(membership_ids.contains(&code_member.id));
    assert!(membership_ids.contains(&knowledge_member.id));

    let from = coordinate(&code_member, &code_generation, b"lib.rs");
    let to = coordinate(&knowledge_member, &knowledge_generation, b"contract.md");
    admit_relationship(
        &mut atlas,
        &scope,
        None,
        RelationshipKind::GovernedBy,
        from.clone(),
        to.clone(),
        vec![to.clone()],
        "explicit-admission/v1",
    )
    .unwrap();

    drop(atlas);
    let atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let views = relationships_for(&atlas, &scope, None, &from).unwrap();
    assert_eq!(views.len(), 1);
    match &views[0] {
        RelationshipView::Disclosed(relationship) => {
            assert_eq!(relationship.kind, RelationshipKind::GovernedBy);
            assert_eq!(relationship.to, to);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn coverage_reports_generation_unavailable_when_a_source_never_published() {
    let (repo, _rev) = repo_with(&[("lib.rs", "fn validate_claim() {}\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    atlas.register_git("source", repo.path(), "HEAD").unwrap();

    let answer = search(
        &atlas,
        &SearchRequest {
            scope: work(&["source"], Access::Read),
            requested_source: None,
            query: "validate_claim".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();
    assert!(answer.coverage.generation_unavailable);
    assert!(answer.hits.is_empty());
}

/// Not a parity measurement: one real acquisition/publish/search/path-lookup
/// pass against this worktree's own product checkout (a real, sizeable Wirk
/// tree, not a synthetic fixture), printed so the numbers land in the
/// stage's raw evidence rather than being asserted on (a CI-timing
/// assertion would be flaky; the point here is visibility).
#[test]
fn real_query_timings_are_recorded_as_scoped_evidence_not_parity() {
    let wirk_checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    if !wirk_checkout.join(".git").exists() {
        eprintln!("skipping timing probe: no .git at {wirk_checkout:?}");
        return;
    }
    let head = git(&wirk_checkout, &["rev-parse", "HEAD"]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas.register_git("wirk", &wirk_checkout, "HEAD").unwrap();

    let acquire_start = std::time::Instant::now();
    let generation = match atlas
        .acquire(&membership, &head, ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    };
    let acquire_elapsed = acquire_start.elapsed();
    atlas.publish(&membership, &generation.id).unwrap();
    let indexed_resources = generation
        .resources
        .iter()
        .filter(|r| r.disposition == wirk_atlas::CoverageDisposition::Indexed)
        .count();
    let indexed_units: usize = generation.resources.iter().map(|r| r.units.len()).sum();

    let scope = work(&["wirk"], Access::Read);
    let search_start = std::time::Instant::now();
    let answer = search(
        &atlas,
        &SearchRequest {
            scope: scope.clone(),
            requested_source: None,
            query: "validate claim boundary".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: None,
            offset: 0,
        },
    )
    .unwrap();
    let search_elapsed = search_start.elapsed();

    let path_start = std::time::Instant::now();
    let path_outcome = resolve_path(
        &atlas,
        &PathLookupRequest {
            scope,
            source: "wirk".into(),
            path: b"wirk-core/src/lib.rs".to_vec(),
            budget_bytes: 65_536,
        },
    );
    let path_elapsed = path_start.elapsed();

    eprintln!(
        "atlas-w2-timing resources={} indexed_resources={} indexed_units={} \
         acquire_ms={:.3} search_ms={:.3} search_hits={} coverage_complete={} \
         path_lookup_ms={:.3} path_lookup_ok={}",
        generation.resources.len(),
        indexed_resources,
        indexed_units,
        acquire_elapsed.as_secs_f64() * 1000.0,
        search_elapsed.as_secs_f64() * 1000.0,
        answer.hits.len(),
        answer.coverage.is_complete(),
        path_elapsed.as_secs_f64() * 1000.0,
        path_outcome.is_ok(),
    );
}

/// Ruling 0095 / W3-SECOND-CORRECTION.md item 1, the library half of the
/// correction-verify VERDICT §3 X1.
///
/// `AtlasStore::generation` is a *global* lookup by generation id: it
/// reads whichever manifest that id names, whatever source staged it.
/// `search`'s pinned branch used that result directly, then read the
/// resource blobs through the *admitted* membership's locator — so a
/// pinned generation belonging to a denied alias over the same
/// repository produced real hits, with the denied source's path,
/// revision, generation id and snippet bytes, and `coverage.complete`.
///
/// `wirkd` now also refuses to accept such a pin at all (the token
/// carries an HMAC only the daemon can produce), but the two checks are
/// deliberately independent: this one holds at the library boundary,
/// where no token exists, so any future caller of `search` inherits it.
#[test]
fn a_pinned_generation_must_belong_to_the_membership_it_is_pinned_to() {
    use std::collections::BTreeMap;

    // One repository, two revisions, two aliases: `open` is granted and
    // published at the first revision, `closed` is denied and published
    // at the second, which is the one holding the canary.
    let (repo, first) = repo_with(&[("lib.rs", "fn shared() {}\n")]);
    fs::write(
        repo.path().join("secret.rs"),
        "fn shared() { SECRET_CANARY }\n",
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "two"]);
    let second = git(repo.path(), &["rev-parse", "HEAD"]);

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let (open_member, open_generation) =
        acquire_and_publish(&mut atlas, "open", repo.path(), &first);
    let (_, closed_generation) = acquire_and_publish(&mut atlas, "closed", repo.path(), &second);
    assert_ne!(open_generation.source, closed_generation.source);

    let request =
        |pinned: BTreeMap<wirk_atlas::MembershipId, wirk_atlas::GenerationId>| SearchRequest {
            scope: work(&["open"], Access::Read),
            requested_source: None,
            query: "shared".into(),
            families: vec![],
            semantic: SemanticRequest::Disabled,
            limit: 10,
            pinned: Some(pinned),
            offset: 0,
        };

    // The honest pin — the generation this membership really published —
    // still works exactly as before.
    let mut honest = BTreeMap::new();
    honest.insert(open_member.id.clone(), open_generation.id.clone());
    let answer = search(&atlas, &request(honest)).unwrap();
    assert_eq!(
        answer.generations,
        vec![(open_member.id.clone(), open_generation.id.clone())]
    );
    assert!(!answer.hits.is_empty());
    assert!(
        !answer
            .hits
            .iter()
            .any(|hit| hit.snippet.contains("SECRET_CANARY"))
    );

    // The forged pin: the admitted membership, another source's
    // generation. Refused before any manifest, blob or snippet is read.
    let mut forged = BTreeMap::new();
    forged.insert(open_member.id.clone(), closed_generation.id.clone());
    let error = search(&atlas, &request(forged)).unwrap_err();
    assert!(
        matches!(&error, wirk_atlas::AtlasError::InvalidCoordinate(detail)
            if detail.contains("does not belong to this membership's source")),
        "expected the pin to be refused, got: {error:?}"
    );
}

/// Ruling 0095, VERDICT §3 X2: a window that starts at or past the end of
/// its own ranked list is `spent`, not `no_match`. Pinned at the library
/// boundary because `coverage` is where the distinction lives.
#[test]
fn an_offset_past_the_last_candidate_is_spent_not_no_match() {
    let (repo, rev) = repo_with(&[("a.rs", "fn shared() {}\n"), ("b.rs", "fn shared() {}\n")]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    acquire_and_publish(&mut atlas, "open", repo.path(), &rev);

    let at = |offset: usize, query: &str| {
        search(
            &atlas,
            &SearchRequest {
                scope: work(&["open"], Access::Read),
                requested_source: None,
                query: query.into(),
                families: vec![],
                semantic: SemanticRequest::Disabled,
                limit: 10,
                pinned: None,
                offset,
            },
        )
        .unwrap()
    };

    let first = at(0, "shared");
    assert_eq!(first.budget.total_candidates, 2);
    assert_eq!(first.budget.returned, 2);
    assert!(first.coverage.is_complete());

    let spent = at(2, "shared");
    assert_eq!(spent.budget.returned, 0);
    assert_eq!(spent.budget.total_candidates, 2);
    assert!(spent.coverage.spent);
    assert!(
        !spent.coverage.no_match,
        "an exhausted window must not assert the corpus was empty"
    );

    // A genuine zero-hit search at offset 0 is still `no_match`.
    let empty = at(0, "zzzznothing");
    assert!(empty.coverage.no_match);
    assert!(!empty.coverage.spent);
}
