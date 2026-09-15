//! What an estate's own `.wirk/resources.json` actually does to the work
//! it governs, observed through the public acquisition surface rather
//! than by reading the loader's return value.
//!
//! Ruling 0402: "apply a supported value with its actual semantics or
//! report an unusable configuration before executing the operations that
//! depend on it... A warning does not enforce an explicit constraint."
//! So the three things worth checking are behavioural:
//!
//! * with no policy file, the ordinary useful task runs and indexes;
//! * with an explicit bound written — including the most restrictive
//!   value the field has, `0` — that bound is honoured by name;
//! * with a policy file that exists and cannot be used, the work it
//!   would have bounded does not run and leaves no side effect behind.

use std::fs;
use tempfile::TempDir;
use wirk_atlas::{AcquireOutcome, AtlasStore, CoverageDisposition, ExtractorPolicy};

fn collection() -> TempDir {
    let source = TempDir::new().expect("source dir");
    fs::write(
        source.path().join("brief.md"),
        "# brief\n\nthe policymarker line.\n",
    )
    .expect("write brief");
    source
}

fn write_policy(estate: &TempDir, body: &str) {
    fs::create_dir_all(estate.path().join(".wirk")).expect("wirk dir");
    fs::write(estate.path().join(".wirk").join("resources.json"), body).expect("write policy");
}

/// The control: no policy file, and the ordinary task succeeds. Without
/// this the two checks below would pass just as well against an estate
/// that never works at all.
#[test]
fn with_no_policy_file_the_ordinary_acquisition_indexes_the_collection() {
    let source = collection();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-policy-absent").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let AcquireOutcome::Staged(staged) = atlas
        .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
        .unwrap()
    else {
        panic!("an ordinary collection acquires");
    };
    assert_eq!(staged.coverage.indexed, 1, "{:?}", staged.coverage);
    assert_eq!(staged.coverage.unsupported, 0, "{:?}", staged.coverage);
}

/// An explicit `0` is the most restrictive value `document_max_file_bytes`
/// has, and it means what it says: admit no file over zero bytes. The
/// file is reported `Unsupported` **by name**, not skipped and not
/// indexed.
///
/// Watched failing against `16840cf`, where `reject_zero_bound!`
/// replaced the written `0` with `None` after a warning, the bound was
/// not in force, and this same acquisition indexed the file.
#[test]
fn an_explicit_zero_document_bound_refuses_the_file_it_says_it_refuses() {
    let source = collection();
    let estate = TempDir::new().unwrap();
    write_policy(&estate, r#"{ "document_max_file_bytes": 0 }"#);
    let mut atlas = AtlasStore::open(estate.path(), "estate-policy-zero").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let AcquireOutcome::Staged(staged) = atlas
        .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
        .unwrap()
    else {
        panic!("the collection is still walked; its file is what the bound refuses");
    };
    assert_eq!(
        staged.coverage.indexed, 0,
        "a bound the operator wrote must actually be in force: {:?}",
        staged.coverage
    );
    let generation = atlas.generation(&staged.id).unwrap();
    let brief = generation
        .resources
        .iter()
        .find(|resource| resource.path == b"brief.md")
        .expect("the file is still a member of the collection");
    assert_eq!(brief.disposition, CoverageDisposition::Unsupported);
    assert_eq!(
        brief.detail.as_deref(),
        Some("file exceeds the bounded document read size"),
        "refused by name, never silently skipped"
    );
    assert!(brief.units.is_empty());
}

/// A policy file that exists and cannot be parsed does not become
/// "running on defaults". The store refuses to open at all, so the
/// acquisition it would have bounded never happens and nothing is
/// staged.
///
/// Watched failing against `16840cf`, where `ResourcePolicy::load`
/// returned the built-in defaults with a note, `AtlasStore::open`
/// printed it, and every subsequent operation ran with none of the
/// operator's bounds in force.
#[test]
fn an_unusable_policy_file_refuses_the_work_it_was_meant_to_bound() {
    let estate = TempDir::new().unwrap();
    write_policy(&estate, r#"{ "document_max_file_bytes": "#);
    let Err(error) = AtlasStore::open(estate.path(), "estate-policy-unusable") else {
        panic!("an estate whose policy cannot be read must not open unbounded");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("estate resource policy is unusable"),
        "the refusal says what it is: {rendered}"
    );
    assert!(
        rendered.contains("not in force"),
        "and what it costs the operator: {rendered}"
    );
    // The side effect the old behaviour would have permitted: no
    // generation was staged, because nothing ran.
    let generations = estate.path().join("atlas").join("generations");
    let staged_any = fs::read_dir(&generations)
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    assert_eq!(
        staged_any, 0,
        "no work may be admitted on an estate whose configured bounds are not in force"
    );
}
