use std::fs;
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, ExactCoordinate, ExtractorPolicy, ResolveOutcome, ResolvedEvidence,
    SourceGeneration,
};

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

fn repo() -> (TempDir, String) {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["config", "user.email", "atlas@example.test"]);
    git(temp.path(), &["config", "user.name", "Atlas"]);
    fs::write(temp.path().join("kept.rs"), "fn exact() {}\n").unwrap();
    fs::write(temp.path().join("secret.pem"), "not-indexed\n").unwrap();
    fs::write(temp.path().join("binary.bin"), b"a\0b").unwrap();
    fs::write(temp.path().join("unusual\nname.rs"), "fn unusual() {}\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("kept.rs", temp.path().join("link")).unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-qm", "one"]);
    let first = git(temp.path(), &["rev-parse", "HEAD"]);
    (temp, first)
}

#[test]
fn acquisition_reads_pinned_bytes_and_accounts_for_non_text_without_checkout_traversal() {
    let (repo, first) = repo();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    fs::write(repo.path().join("kept.rs"), "fn DRIFT() {}\n").unwrap();
    let staged = atlas
        .acquire(&membership, &first, ExtractorPolicy::default())
        .unwrap();
    let generation = match staged {
        AcquireOutcome::Staged(g) => g,
        other => panic!("{other:?}"),
    };
    let pinned = coordinate(&membership, &generation, b"kept.rs");
    assert_eq!(
        atlas.resolve_exact(&membership, &pinned).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: pinned,
            bytes: b"fn exact() {}\n".to_vec(),
        })
    );
    let dispositions: Vec<_> = generation.resources.iter().map(|r| r.disposition).collect();
    assert!(dispositions.iter().any(|d| d.is_excluded()));
    assert!(dispositions.iter().any(|d| d.is_unsupported()));
    assert!(
        generation
            .resources
            .iter()
            .any(|resource| resource.path == b"unusual\nname.rs")
    );
}

#[test]
fn failed_refresh_retains_publication_and_refuses_out_of_scope_membership() {
    let (repo, first) = repo();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let staged = atlas
        .acquire(&member, &first, ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    atlas.publish(&member, &staged.id).unwrap();
    assert!(matches!(
        atlas
            .acquire(&member, "definitely-not-a-ref", ExtractorPolicy::default())
            .unwrap(),
        AcquireOutcome::Unavailable(_)
    ));
    assert_eq!(atlas.current(&member).unwrap().unwrap().id, staged.id);
    let other = AtlasStore::open(TempDir::new().unwrap().path(), "estate-b").unwrap();
    assert!(other.current(&member).is_err());
    let mut invalid = coordinate(&member, &staged, b"kept.rs");
    invalid.path = b"../kept.rs".to_vec();
    assert!(atlas.resolve_exact(&member, &invalid).is_err());
}

#[test]
fn generations_are_revision_and_extractor_specific_and_publication_is_explicit_restart_safe() {
    let (repo, first) = repo();
    git(
        repo.path(),
        &["commit", "--allow-empty", "-qm", "same-tree-other-commit"],
    );
    let second = git(repo.path(), &["rev-parse", "HEAD"]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas.register_git("same", repo.path(), "HEAD").unwrap();
    let one = atlas
        .acquire(&membership, &first, ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let two = atlas
        .acquire(&membership, &second, ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let three = atlas
        .acquire(&membership, &second, ExtractorPolicy::markdown_only())
        .unwrap()
        .staged()
        .unwrap();
    assert_eq!(one.content, two.content);
    assert_ne!(one.id, two.id);
    assert_ne!(two.id, three.id);
    assert!(atlas.current(&membership).unwrap().is_none());
    atlas.publish(&membership, &one.id).unwrap();
    assert_eq!(atlas.current(&membership).unwrap().unwrap().id, one.id);
    drop(atlas);
    let atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    assert_eq!(atlas.current(&membership).unwrap().unwrap().id, one.id);
    let historical = coordinate(&membership, &two, b"kept.rs");
    assert_eq!(
        atlas.resolve_exact(&membership, &historical).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: historical,
            bytes: b"fn exact() {}\n".to_vec(),
        })
    );
}

/// P3 W3 extractor completion (`W3-EXTRACTOR-COMPLETION.md`): widening
/// the family vocabulary changes `extractor_set`, and therefore every
/// new generation id. The brief expects that — and it also requires that
/// generations already staged under the old edition keep validating and
/// resolving *exactly*. This is the test that would catch a regression
/// where `validate_generation` re-derives families from today's default
/// instead of from the edition each generation names.
#[test]
fn a_v2_generation_still_validates_and_resolves_under_the_v3_default() {
    let (repo, _first) = repo();
    fs::write(repo.path().join("tool.py"), "def marker():\n    return 1\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "add python"]);
    let second = git(repo.path(), &["rev-parse", "HEAD"]);

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas.register_git("same", repo.path(), "HEAD").unwrap();

    // Staged as the historical edition would have staged it.
    let old = atlas
        .acquire(&membership, &second, ExtractorPolicy::rust_markdown_v2())
        .unwrap()
        .staged()
        .unwrap();
    assert_eq!(
        old.extractor_set,
        "utf8-lines/rust-markdown+utf8-line-chunks-65536/v2"
    );
    assert!(
        !old.resources
            .iter()
            .any(|r| r.path == b"tool.py"
                && r.disposition == wirk_atlas::CoverageDisposition::Indexed),
        "the old edition did not admit Python; this fixture must reproduce that"
    );

    // The current default admits it, under a different identity.
    let new = atlas
        .acquire(&membership, &second, ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    assert_ne!(old.id, new.id, "a new edition is a new generation");
    assert_ne!(old.extractor_set, new.extractor_set);
    assert_eq!(old.revision, new.revision);
    assert_eq!(old.content, new.content, "same tree, same content identity");
    assert!(
        new.resources
            .iter()
            .any(|r| r.path == b"tool.py"
                && r.disposition == wirk_atlas::CoverageDisposition::Indexed),
        "the current edition must admit Python"
    );

    // Re-open the store from disk so both manifests go through
    // `validate_generation` again, from cold.
    drop(atlas);
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let reread_old = atlas
        .generation(&old.id)
        .expect("the v2 manifest still validates");
    assert_eq!(reread_old, old);
    let reread_new = atlas
        .generation(&new.id)
        .expect("the v3 manifest validates");
    assert_eq!(reread_new, new);

    // An old coordinate resolves to exactly the bytes it always did,
    // whichever generation is currently published.
    let historical = coordinate(&membership, &old, b"kept.rs");
    let expected = ResolveOutcome::Resolved(ResolvedEvidence {
        coordinate: historical.clone(),
        bytes: b"fn exact() {}\n".to_vec(),
    });
    atlas.publish(&membership, &new.id).unwrap();
    assert_eq!(atlas.current(&membership).unwrap().unwrap().id, new.id);
    assert_eq!(
        atlas.resolve_exact(&membership, &historical).unwrap(),
        expected,
        "publishing a new edition must not disturb an old coordinate"
    );

    // And a pinned search still reads the old vector, families and all.
    use std::collections::BTreeMap;
    let mut pinned = BTreeMap::new();
    pinned.insert(membership.id.clone(), old.id.clone());
    let answer = wirk_atlas::search(
        &atlas,
        &wirk_atlas::SearchRequest {
            scope: wirk_atlas::QueryScope::EstateOrientation,
            requested_source: None,
            query: "marker".into(),
            families: vec![],
            semantic: wirk_atlas::SemanticRequest::Disabled,
            limit: 10,
            pinned: Some(pinned),
            offset: 0,
            semantic_query: None,
            pinned_editions: None,
            pinned_mode: None,
            pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
        },
    )
    .unwrap();
    assert_eq!(
        answer.generations,
        vec![(membership.id.clone(), old.id.clone())]
    );
    assert!(
        !answer
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"tool.py"),
        "a continuation pinned to the v2 generation must keep seeing the v2 corpus"
    );
}

/// The v3 vocabulary is the installed reference's, not a hand-picked
/// list: `semble.index.files.get_extensions()` for code/docs/config.
/// These are spot values from the generated table plus the two suffix
/// rules that decide whole classes of file
/// (`semble.index.files.detect_language` uses `Path(...).suffix.lower()`).
#[test]
fn the_v3_family_vocabulary_follows_the_reference() {
    let (repo, _rev) = repo();
    for (name, body) in [
        ("a.py", "marker\n"),
        ("b.sh", "marker\n"),
        ("c.js", "marker\n"),
        ("d.c", "marker\n"),
        ("e.TOML", "marker\n"),
        ("f.yml", "marker\n"),
        ("g.rst", "marker\n"),
        ("h.json", "marker\n"),
        ("i.csv", "marker\n"),
        (".gitignore", "marker\n"),
        ("j.gitignore", "marker\n"),
        ("k.", "marker\n"),
        ("l.unknownext", "marker\n"),
    ] {
        fs::write(repo.path().join(name), body).unwrap();
    }
    git(repo.path(), &["add", "-A", "-f"]);
    git(repo.path(), &["commit", "-qm", "vocabulary"]);
    let rev = git(repo.path(), &["rev-parse", "HEAD"]);

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas.register_git("v", repo.path(), "HEAD").unwrap();
    let generation = atlas
        .acquire(&membership, &rev, ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();

    let family = |name: &str| {
        generation
            .resources
            .iter()
            .find(|r| r.path == name.as_bytes())
            .unwrap_or_else(|| panic!("{name} missing from the generation"))
            .units
            .first()
            .map(|unit| unit.family)
    };
    use wirk_atlas::ContentFamily::*;
    assert_eq!(family("a.py"), Some(Code));
    assert_eq!(family("b.sh"), Some(Code));
    assert_eq!(family("c.js"), Some(Code));
    assert_eq!(family("d.c"), Some(Code));
    // suffix matching is case-insensitive, exactly as `.suffix.lower()`
    assert_eq!(family("e.TOML"), Some(Config));
    assert_eq!(family("f.yml"), Some(Config));
    assert_eq!(family("g.rst"), Some(Knowledge));
    // the reference's own `_DATA_LANGUAGES`: no ContentType claims them,
    // so `semble --content code docs config` does not index them and
    // neither do we. Matching the reference is the point.
    assert_eq!(family("h.json"), None);
    assert_eq!(family("i.csv"), None);
    // a dotfile has no suffix in Python's sense, so the `.gitignore`
    // row matches `j.gitignore` and not `.gitignore` itself
    assert_eq!(family(".gitignore"), None);
    assert_eq!(family("j.gitignore"), Some(Config));
    // a trailing dot also yields no suffix
    assert_eq!(family("k."), None);
    assert_eq!(family("l.unknownext"), None);
}
