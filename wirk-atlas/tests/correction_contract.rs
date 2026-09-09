use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasError, AtlasStore, ExactCoordinate, ExtractorPolicy, ResolveOutcome,
    ResolvedEvidence,
};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repo_with(text: &str) -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "atlas@example.test"]);
    git(repo.path(), &["config", "user.name", "Atlas"]);
    fs::write(repo.path().join("code.rs"), text).unwrap();
    git(repo.path(), &["add", "code.rs"]);
    git(repo.path(), &["commit", "-qm", "one"]);
    repo
}

#[test]
fn native_git_never_lazy_fetches_missing_partial_clone_objects() {
    let source = repo_with("fn promised_blob() {}\n");
    let blob = git(source.path(), &["rev-parse", "HEAD:code.rs"]);
    let bare = TempDir::new().unwrap();
    fs::remove_dir(bare.path()).unwrap();
    let cloned = Command::new("git")
        .args([
            "clone",
            "--bare",
            source.path().to_str().unwrap(),
            bare.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(cloned.status.success());
    git(bare.path(), &["config", "uploadpack.allowFilter", "true"]);
    git(
        bare.path(),
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );

    let partial = TempDir::new().unwrap();
    fs::remove_dir(partial.path()).unwrap();
    let url = format!("file://{}", bare.path().display());
    let cloned = Command::new("git")
        .args([
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &url,
            partial.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(cloned.status.success());
    let object_missing = || {
        !Command::new("git")
            .arg("-C")
            .arg(partial.path())
            .args(["cat-file", "-e", &blob])
            .env("GIT_NO_LAZY_FETCH", "1")
            .status()
            .unwrap()
            .success()
    };
    assert!(
        object_missing(),
        "partial-clone fixture must begin incomplete"
    );

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas
        .register_git("source", partial.path(), "HEAD")
        .unwrap();
    let outcome = atlas.acquire(&member, "HEAD", ExtractorPolicy::default());
    println!("blob={blob} outcome={outcome:?}");
    assert!(
        matches!(outcome, Ok(AcquireOutcome::Unavailable(_))),
        "missing promisor object must make acquisition unavailable: {outcome:?}"
    );
    assert!(
        object_missing(),
        "Atlas must not materialize the missing blob"
    );
}

#[test]
fn publication_binds_locator_but_refresh_accepts_each_explicit_ref() {
    let registered = repo_with("fn same_blob() {}\n");
    let substituted = repo_with("fn same_blob() {}\n");
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas
        .register_git("source", registered.path(), "HEAD")
        .unwrap();
    let generation = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let manifest = estate
        .path()
        .join("atlas/generations")
        .join(&generation.id.0)
        .join("manifest.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["locator"] = Value::from(substituted.path().to_string_lossy().into_owned());
    fs::write(&manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(atlas.publish(&member, &generation.id).is_err());
    value["locator"] = Value::from(member.locator.clone());
    fs::write(&manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    fs::write(registered.path().join("code.rs"), "fn second() {}\n").unwrap();
    git(registered.path(), &["add", "code.rs"]);
    git(registered.path(), &["commit", "-qm", "second"]);
    let first = git(registered.path(), &["rev-parse", "HEAD^"]);
    let refreshed = atlas
        .refresh(&member, &first, ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    assert_eq!(refreshed.id, generation.id);
    assert_eq!(atlas.attempts().last().unwrap().requested_ref, first);
    println!(
        "estate={:?} membership={:?} source={:?} generation={:?} refresh_ref={}",
        member.estate, member.id, member.source, refreshed.id, first
    );
}

#[test]
fn exact_source_spans_are_blob_qualified_and_independent_of_retrieval_units() {
    let repo = repo_with("first();\nsecond();\nthird();\n");
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let generation = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let record = generation
        .resources
        .iter()
        .find(|resource| resource.path == b"code.rs")
        .unwrap();
    // Packed by the current default edition (`v4`): all three short lines
    // fit in one unit far under the 65536-byte budget. The point of this
    // test is that the hand-built `line_start: 1, line_end: 2` coordinate
    // below resolves against committed Git bytes independent of whatever
    // shape the derived units happen to have — not that units are
    // one-per-line.
    assert_eq!(record.units.len(), 1);
    let coordinate = ExactCoordinate {
        estate: member.estate.clone(),
        membership: member.id.clone(),
        source: member.source.clone(),
        generation: generation.id.clone(),
        path: b"code.rs".to_vec(),
        object_id: record.object_id.clone().unwrap(),
        byte_start: 0,
        byte_end: 19,
        line_start: 1,
        line_end: 2,
    };
    let resolved = atlas.resolve_exact(&member, &coordinate).unwrap();
    println!("coordinate={coordinate:?} resolved={resolved:?}");
    assert_eq!(
        resolved,
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: coordinate.clone(),
            bytes: b"first();\nsecond();\n".to_vec(),
        })
    );

    let mut forged_line = coordinate.clone();
    forged_line.line_end = 99;
    assert!(matches!(
        atlas.resolve_exact(&member, &forged_line),
        Err(AtlasError::InvalidCoordinate(_))
    ));
    let mut forged_blob = coordinate.clone();
    forged_blob.object_id = "0".repeat(40);
    assert!(matches!(
        atlas.resolve_exact(&member, &forged_blob),
        Err(AtlasError::InvalidCoordinate(_))
    ));
    let mut forged_estate = coordinate;
    forged_estate.estate.0 = "other-estate".into();
    assert!(matches!(
        atlas.resolve_exact(&member, &forged_estate),
        Err(AtlasError::InvalidCoordinate(_))
    ));
}

#[test]
fn long_utf8_lines_are_deterministic_bounded_lossless_derived_units() {
    let text = format!("{}\n", "é".repeat(400_000));
    let repo = repo_with(&text);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let generation = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let record = generation
        .resources
        .iter()
        .find(|resource| resource.path == b"code.rs")
        .unwrap();
    let mut rebuilt = Vec::new();
    for unit in &record.units {
        assert!(unit.byte_end - unit.byte_start <= 64 * 1024);
        assert_eq!(unit.unitizer, "utf8-multiline-chunks-65536/v1");
        assert!(text.is_char_boundary(unit.byte_start as usize));
        assert!(text.is_char_boundary(unit.byte_end as usize));
        rebuilt
            .extend_from_slice(&text.as_bytes()[unit.byte_start as usize..unit.byte_end as usize]);
    }
    assert!(record.units.len() > 1);
    assert_eq!(rebuilt, text.as_bytes());
    let ids: std::collections::BTreeSet<_> =
        record.units.iter().map(|unit| unit.id.clone()).collect();
    assert_eq!(ids.len(), record.units.len());
    println!(
        "blob={} bytes={} units={} largest={} first_unit={:?} last_unit={:?}",
        record.object_id.as_deref().unwrap(),
        text.len(),
        record.units.len(),
        record
            .units
            .iter()
            .map(|unit| unit.byte_end - unit.byte_start)
            .max()
            .unwrap(),
        record.units.first().unwrap(),
        record.units.last().unwrap()
    );
}
