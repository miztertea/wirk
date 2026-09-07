use std::fs;
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, ExactCoordinate, ExtractorPolicy, ResolveOutcome, ResolvedEvidence,
    SourceGeneration,
};

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn fixture() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "a@b"]);
    git(repo.path(), &["config", "user.name", "A"]);
    fs::write(repo.path().join("code.rs"), "fn café() {}\nlet x = 1;\n").unwrap();
    fs::write(repo.path().join("readme.md"), "# heading\nbody\n").unwrap();
    fs::write(repo.path().join("secret.pem"), "nope").unwrap();
    fs::write(repo.path().join("data.bin"), b"x\0y").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "fixture"]);
    repo
}
fn exact(
    member: &wirk_atlas::Membership,
    generation: &SourceGeneration,
    path: &[u8],
    unit_index: usize,
) -> ExactCoordinate {
    let record = generation
        .resources
        .iter()
        .find(|r| r.path == path)
        .unwrap();
    let unit = &record.units[unit_index];
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
fn unavailable_coordinate(
    member: &wirk_atlas::Membership,
    generation: &SourceGeneration,
    path: &[u8],
) -> ExactCoordinate {
    let object_id = generation
        .resources
        .iter()
        .find(|resource| resource.path == path)
        .and_then(|resource| resource.object_id.clone())
        .unwrap_or_default();
    ExactCoordinate {
        estate: member.estate.clone(),
        membership: member.id.clone(),
        source: member.source.clone(),
        generation: generation.id.clone(),
        path: path.to_vec(),
        object_id,
        byte_start: 0,
        byte_end: 0,
        line_start: 0,
        line_end: 0,
    }
}

#[test]
fn utf8_rust_and_markdown_units_have_exact_committed_byte_and_line_coordinates() {
    let repo = fixture();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let generation = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let code = generation
        .resources
        .iter()
        .find(|r| r.path == b"code.rs")
        .unwrap();
    assert_eq!(
        (
            code.units[0].byte_start,
            code.units[0].byte_end,
            code.units[0].line_start,
            code.units[0].line_end
        ),
        (0, 14, 1, 1)
    );
    let code_coordinate = exact(&member, &generation, b"code.rs", 0);
    assert_eq!(
        atlas.resolve_exact(&member, &code_coordinate).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: code_coordinate,
            bytes: "fn café() {}\n".as_bytes().to_vec(),
        })
    );
    let markdown_coordinate = exact(&member, &generation, b"readme.md", 1);
    assert_eq!(
        atlas.resolve_exact(&member, &markdown_coordinate).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: markdown_coordinate,
            bytes: b"body\n".to_vec(),
        })
    );
}

#[test]
fn exact_resolution_keeps_terminal_coverage_types_and_refuses_forged_scope() {
    let repo = fixture();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let generation = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    assert!(matches!(
        atlas
            .resolve_exact(
                &member,
                &unavailable_coordinate(&member, &generation, b"secret.pem")
            )
            .unwrap(),
        ResolveOutcome::Excluded(_)
    ));
    assert!(matches!(
        atlas
            .resolve_exact(
                &member,
                &unavailable_coordinate(&member, &generation, b"data.bin")
            )
            .unwrap(),
        ResolveOutcome::Unsupported(_)
    ));
    let mut forged = exact(&member, &generation, b"code.rs", 0);
    forged.generation.0 = "../../catalog.json".into();
    assert!(atlas.resolve_exact(&member, &forged).is_err());
    let other = AtlasStore::open(TempDir::new().unwrap().path(), "other").unwrap();
    assert!(
        other
            .resolve_exact(&member, &exact(&member, &generation, b"code.rs", 0))
            .is_err()
    );
}

#[test]
fn attempts_are_timestamped_diagnostic_and_do_not_change_generation_identity() {
    let repo = fixture();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let one = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    let two = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    assert_eq!(one.id, two.id);
    assert_eq!(atlas.attempts().len(), 2);
    assert!(
        atlas
            .attempts()
            .iter()
            .all(|attempt| attempt.at_unix_millis > 0 && attempt.diagnostic.is_none())
    );
    assert!(matches!(
        atlas
            .acquire(&member, "not-a-ref", ExtractorPolicy::default())
            .unwrap(),
        AcquireOutcome::Unavailable(_)
    ));
    let failure = atlas.attempts().last().unwrap();
    assert_eq!(failure.outcome, "unavailable");
    assert!(
        failure
            .diagnostic
            .as_deref()
            .is_some_and(|text| !text.is_empty())
    );
}

#[test]
fn corrupt_or_unaccounted_resource_rows_are_not_claimed_as_a_generation() {
    let repo = fixture();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let generation = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    atlas.publish(&member, &generation.id).unwrap();
    drop(atlas);
    let rows = estate
        .path()
        .join("atlas/generations")
        .join(&generation.id.0)
        .join("resources.ndjson");
    let original = fs::read(&rows).unwrap();
    let first = original
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap()
        .to_vec();
    let mut forged = original;
    forged.extend_from_slice(&first);
    fs::write(rows, forged).unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    assert!(atlas.current(&member).is_err());
}

#[test]
fn forged_manifest_identity_is_refused_before_resolution() {
    let repo = fixture();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("source", repo.path(), "HEAD").unwrap();
    let generation = atlas
        .acquire(&member, "HEAD", ExtractorPolicy::default())
        .unwrap()
        .staged()
        .unwrap();
    atlas.publish(&member, &generation.id).unwrap();
    drop(atlas);
    let manifest = estate
        .path()
        .join("atlas/generations")
        .join(&generation.id.0)
        .join("manifest.json");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        text.replace(&generation.revision, &"0".repeat(40)),
    )
    .unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    assert!(atlas.current(&member).is_err());
}
