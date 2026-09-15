//! Admitted text that is larger than the extractor's former built-in
//! 1 MiB ceiling has to come back out of the index as *content*, not as
//! an honest refusal with the file's identity attached.
//!
//! Rulings 0398/0401/0402/0403: `extract::MAX_TEXT_BYTES` was a product
//! -chosen size policy, not a capability of this reader, and a
//! generation that records a large admitted file as
//! `CoverageDisposition::Error` with zero units has acquired it without
//! indexing it. These checks therefore refuse to be satisfied by a
//! changed classification: each one searches for a distinguishing string
//! that lives *past* the former cutoff and resolves the coordinate the
//! search returns back to its exact bytes, through the same public
//! `acquire`/`publish`/`search`/`resolve_exact` surface an operator uses.
//!
//! Both admitted text paths are covered: plain text chunked directly,
//! and a real document format (`.csv`) whose *converted* Markdown is
//! what gets chunked. A genuine conversion or malformed-input failure is
//! still an `Error` — that is what `document_formats.rs` covers — and
//! nothing here weakens it.

use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, CoverageDisposition, ExtractorPolicy, ResolveOutcome,
    SearchRequest, SemanticRequest, SourceGeneration, search,
};
use wirk_core::{Access, RepositoryBinding};

/// The acquisition reports identity and coverage; the generation's own
/// resource list lives in the immutable generation directory, which is
/// what these checks read it back from.
fn read_staged(atlas: &AtlasStore, outcome: AcquireOutcome) -> SourceGeneration {
    match outcome {
        AcquireOutcome::Staged(staged) => atlas
            .generation(&staged.id)
            .expect("the generation just staged reads back"),
        other => panic!("expected Staged, got {other:?}"),
    }
}

fn request(alias: &str, query: &str) -> SearchRequest {
    SearchRequest {
        scope: wirk_atlas::QueryScope::Work(vec![RepositoryBinding {
            name: alias.into(),
            access: Access::Read,
        }]),
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
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    }
}

/// Plain Markdown well past the former 1 MiB extractor ceiling, whose
/// distinguishing sentence sits in the final kilobyte — so a "capped
/// prefix" implementation cannot pass this.
#[test]
fn plain_text_past_the_former_extractor_ceiling_is_searchable_and_resolvable_past_the_cutoff() {
    let source = TempDir::new().unwrap();
    let mut body = String::from("# a large collection document\n\n");
    let mut filler = 0u32;
    while body.len() < 3 * 1024 * 1024 {
        body.push_str(&format!(
            "ordinary prose line {filler} with nothing distinguishing in it\n"
        ));
        filler += 1;
    }
    // The only occurrence of this string in the whole estate, and it is
    // past the former cutoff by megabytes.
    let marker_line = "the tailmarker sentence is the only distinguishing line here\n";
    let marker_offset = body.len();
    body.push_str(marker_line);
    assert!(
        marker_offset > 1024 * 1024,
        "the distinguishing content must sit past the former 1 MiB ceiling: {marker_offset}"
    );
    std::fs::write(source.path().join("large.md"), &body).unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-large-text").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };

    let record = generation
        .resources
        .iter()
        .find(|resource| resource.path == b"large.md")
        .expect("large.md is a member of the collection");
    assert_eq!(
        record.disposition,
        CoverageDisposition::Indexed,
        "a large admitted text file must be indexed, not recorded as an extraction error: {:?}",
        record.detail
    );
    assert!(
        !record.units.is_empty(),
        "an indexed resource carries retrievable units"
    );
    assert_eq!(
        record.byte_len,
        Some(body.len() as u64),
        "the whole file was read"
    );
    let covered = record
        .units
        .iter()
        .map(|unit| unit.byte_end)
        .max()
        .unwrap_or_default();
    assert_eq!(
        covered,
        body.len() as u64,
        "the derived units must tile the whole file, not a bounded prefix"
    );

    atlas.publish(&membership, &generation.id).unwrap();
    let answer = search(&atlas, &request("docs", "tailmarker")).unwrap();
    assert_eq!(
        answer.hits.len(),
        1,
        "the distinguishing sentence past the former cutoff must be findable: {:?}",
        answer.coverage
    );
    assert!(
        !answer.coverage.source_extraction_incomplete,
        "nothing in this collection failed extraction: {:?}",
        answer.coverage
    );

    let coordinate = answer.hits[0].coordinate.clone();
    assert!(
        coordinate.byte_start >= 1024 * 1024,
        "the resolved coordinate must actually name content past the former ceiling: {}",
        coordinate.byte_start
    );
    let ResolveOutcome::Resolved(evidence) = atlas
        .resolve_exact(&membership, &coordinate)
        .expect("resolve the coordinate the search returned")
    else {
        panic!("a published coordinate over unchanged bytes resolves");
    };
    let text = String::from_utf8(evidence.bytes).expect("utf-8 unit bytes");
    assert!(
        text.contains("tailmarker"),
        "the resolved bytes are the distinguishing content, not a neighbouring chunk"
    );
    assert_eq!(
        &body[coordinate.byte_start as usize..coordinate.byte_end as usize],
        text,
        "resolution returns exactly the source bytes the coordinate names"
    );
}

/// The converted-document half of the same requirement: a real `.csv`
/// whose *rendered Markdown* is well past the former ceiling, with its
/// distinguishing row in the last handful of lines.
#[test]
fn converted_document_text_past_the_former_extractor_ceiling_is_searchable_and_resolvable() {
    let source = TempDir::new().unwrap();
    let mut csv = String::from("name,quantity\n");
    for row in 0..120_000u32 {
        csv.push_str(&format!("Item{row},{}\n", row % 100));
    }
    csv.push_str("tailmarkerwidget,7\n");
    assert!(
        csv.len() > 1024 * 1024,
        "the fixture's own text must exceed the former 1 MiB ceiling: {}",
        csv.len()
    );
    std::fs::write(source.path().join("inventory.csv"), &csv).unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-large-doc").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    let record = generation
        .resources
        .iter()
        .find(|resource| resource.path == b"inventory.csv")
        .expect("inventory.csv is a member of the collection");
    assert_eq!(
        record.disposition,
        CoverageDisposition::Indexed,
        "a large convertible document must be indexed, not refused on converted size: {:?}",
        record.detail
    );
    assert!(
        record
            .units
            .iter()
            .map(|unit| unit.byte_end)
            .max()
            .unwrap_or_default()
            > 1024 * 1024,
        "the converted Markdown actually indexed must itself run past the former ceiling"
    );

    atlas.publish(&membership, &generation.id).unwrap();
    let answer = search(&atlas, &request("docs", "tailmarkerwidget")).unwrap();
    assert_eq!(
        answer.hits.len(),
        1,
        "the distinguishing row past the former cutoff must be findable: {:?}",
        answer.coverage
    );
    let coordinate = answer.hits[0].coordinate.clone();
    assert!(
        coordinate.byte_start >= 1024 * 1024,
        "the resolved coordinate names converted text past the former ceiling: {}",
        coordinate.byte_start
    );
    let ResolveOutcome::Resolved(evidence) = atlas
        .resolve_exact(&membership, &coordinate)
        .expect("resolve the coordinate the search returned")
    else {
        panic!("a published coordinate over unchanged bytes resolves");
    };
    let text = String::from_utf8(evidence.bytes).expect("utf-8 unit bytes");
    assert!(
        text.contains("tailmarkerwidget"),
        "the resolved converted text is the distinguishing row"
    );
}
