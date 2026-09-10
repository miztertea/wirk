//! Ruling 0135 C4-R12, with the ruling's own qualification: *the actual
//! unexpected `server.rs` extraction failure and its missing query
//! disclosure require correction; treating every intentionally
//! unsupported file as failed would obscure the map.*
//!
//! `source-coverage-native` removed the one observed extraction failure
//! (the per-blob unit-count budget) and left the general disclosure
//! unbuilt, so an admitted generation that genuinely failed to extract
//! a resource still reported `coverage.complete: true` to every search
//! over it. These tests are the general contract, watched red against
//! that candidate:
//!
//! * a genuine extraction failure is disclosed by every search that
//!   reads the generation carrying it,
//! * an intentionally unsupported or excluded resource is *not*,
//! * a zero-hit search over such a generation is not proven absence,
//! * a requester the source was never disclosed to learns nothing,
//! * a repaired, republished generation recovers while the old
//!   generation, read by pin, keeps saying what was true of it,
//! * and the packed multi-line units the current default edition
//!   derives name exactly the committed bytes and lines they span.
use std::fs;
use std::process::Command;
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, CoverageDisposition, ExtractorPolicy, SearchRequest,
    SemanticRequest, search,
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

fn repo_with(files: &[(&str, String)]) -> (TempDir, String) {
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

/// Real, valid UTF-8 Rust text past `wirk-atlas/src/extract.rs`'s
/// `MAX_TEXT_BYTES` (1 MiB) — the same budget the actual product file
/// `wirk/src/wirkd/server.rs` (902,199 bytes at `73d6d2d`) is
/// approaching. Nothing about it is a stub: the extractor reads it,
/// measures it and refuses it exactly as it refuses any other blob over
/// the budget.
fn oversize_source() -> String {
    let mut text = String::with_capacity(1_200_000);
    let mut line = 0u32;
    while text.len() <= 1024 * 1024 {
        text.push_str(&format!(
            "pub fn oversize_{line}(argument: u32) -> u32 {{ argument.wrapping_add({line}) }}\n"
        ));
        line += 1;
    }
    assert!(
        text.len() > 1024 * 1024,
        "the fixture must exceed the budget"
    );
    text
}

fn work(names: &[&str]) -> wirk_atlas::QueryScope {
    wirk_atlas::QueryScope::Work(
        names
            .iter()
            .map(|name| RepositoryBinding {
                name: (*name).into(),
                access: Access::Read,
            })
            .collect(),
    )
}

fn request(scope: wirk_atlas::QueryScope, query: &str) -> SearchRequest {
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
        pinned_producer: wirk_atlas::PinnedProducer::Unrecorded,
    }
}

fn staged(outcome: AcquireOutcome) -> wirk_atlas::SourceGeneration {
    match outcome {
        AcquireOutcome::Staged(generation) => generation,
        other => panic!("{other:?}"),
    }
}

/// Positive control shared by several tests: this repository really does
/// produce one `Error` disposition, for the reason the extractor states,
/// and everything else in it really is indexed.
fn assert_one_real_extraction_error(generation: &wirk_atlas::SourceGeneration) {
    let failed: Vec<_> = generation
        .resources
        .iter()
        .filter(|resource| resource.disposition == CoverageDisposition::Error)
        .collect();
    assert_eq!(
        failed.len(),
        1,
        "the fixture must produce exactly one genuine extraction failure: {:?}",
        generation
            .resources
            .iter()
            .map(|r| (String::from_utf8_lossy(&r.path).to_string(), r.disposition))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        failed[0].detail.as_deref(),
        Some("text blob exceeds bounded extractor size"),
        "the failure must be the extractor's own diagnostic, not an inferred one"
    );
}

// -- 1. A genuine extraction failure is disclosed to every search over it. --

#[test]
fn a_search_over_a_generation_that_failed_to_extract_a_resource_never_calls_itself_complete() {
    let (repo, revision) = repo_with(&[
        ("small.rs", "pub fn alphamarker() {}\n".to_string()),
        ("huge.rs", oversize_source()),
    ]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let membership = atlas.register_git("code", repo.path(), "HEAD").unwrap();
    let generation = staged(
        atlas
            .acquire(&membership, &revision, ExtractorPolicy::default())
            .unwrap(),
    );
    assert_one_real_extraction_error(&generation);
    atlas.publish(&membership, &generation.id).unwrap();

    let answer = search(&atlas, &request(work(&["code"]), "alphamarker")).unwrap();
    assert_eq!(
        answer.hits.len(),
        1,
        "positive control: the small file is found"
    );
    assert!(
        answer.coverage.source_extraction_incomplete,
        "CONTRACT FAILURE (ruling 0135 C4-R12): a generation this answer read \
         records a resource the extractor could not turn into retrieval units, \
         and the answer said nothing about it"
    );
    assert!(
        answer.coverage.partial,
        "an unsearched resource is a partial answer"
    );
    assert!(
        !answer.coverage.is_complete(),
        "CONTRACT FAILURE: `complete: true` beside a real extraction error is \
         the exact untruth ruling 0135 records as observed"
    );
}

// -- 2. Unsupported and excluded are not failures (ruling 0135, R12
// -- qualification: "preserve separate categories"). --

#[test]
fn intentionally_unsupported_or_excluded_resources_are_never_reported_as_extraction_failures() {
    let (repo, revision) = repo_with(&[
        ("lib.rs", "pub fn alphamarker() {}\n".to_string()),
        ("table.xyz", "alphamarker\n".to_string()),
        (
            "deploy.pem",
            "-----BEGIN KEY-----\nalphamarker\n".to_string(),
        ),
    ]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let membership = atlas.register_git("code", repo.path(), "HEAD").unwrap();
    let generation = staged(
        atlas
            .acquire(&membership, &revision, ExtractorPolicy::default())
            .unwrap(),
    );
    // Positive control: this generation really does index fewer resources
    // than it holds, for reasons that are not failures.
    let counted = |wanted: CoverageDisposition| {
        generation
            .resources
            .iter()
            .filter(|resource| resource.disposition == wanted)
            .count()
    };
    assert_eq!(counted(CoverageDisposition::Indexed), 1);
    assert_eq!(counted(CoverageDisposition::Unsupported), 1);
    assert_eq!(counted(CoverageDisposition::Excluded), 1);
    assert_eq!(counted(CoverageDisposition::Error), 0);
    atlas.publish(&membership, &generation.id).unwrap();

    let answer = search(&atlas, &request(work(&["code"]), "alphamarker")).unwrap();
    assert!(
        !answer.coverage.source_extraction_incomplete,
        "indexed < total is not a synonym for a failure: an unsupported family \
         and a deliberately excluded path are declared coverage, not a hole \
         (ruling 0135, R12 qualification)"
    );
    assert!(
        answer.coverage.is_complete(),
        "a fully-extracted generation with declared exclusions is complete: {:?}",
        answer.coverage
    );
}

// -- 3. A hole is not proven absence. --

#[test]
fn a_zero_hit_search_over_a_partially_extracted_generation_is_not_reported_as_no_match() {
    let (repo, revision) = repo_with(&[
        ("small.rs", "pub fn alphamarker() {}\n".to_string()),
        ("huge.rs", oversize_source()),
    ]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let membership = atlas.register_git("code", repo.path(), "HEAD").unwrap();
    let generation = staged(
        atlas
            .acquire(&membership, &revision, ExtractorPolicy::default())
            .unwrap(),
    );
    assert_one_real_extraction_error(&generation);
    atlas.publish(&membership, &generation.id).unwrap();

    let answer = search(&atlas, &request(work(&["code"]), "gammamarkerabsent")).unwrap();
    assert!(answer.hits.is_empty(), "positive control: nothing matches");
    assert!(
        !answer.coverage.no_match,
        "CONTRACT FAILURE: `no_match` asserts the admitted corpus was fully \
         searched and genuinely held nothing; part of this corpus was never \
         turned into anything searchable, so absence was never checked"
    );
    assert!(answer.coverage.source_extraction_incomplete);
}

// -- 4. The negative control: a real requester, really denied. --

#[test]
fn a_requester_bound_only_to_a_healthy_source_learns_nothing_about_another_sources_failure() {
    let (open_repo, open_revision) =
        repo_with(&[("open.rs", "pub fn alphamarker() {}\n".to_string())]);
    let (closed_repo, closed_revision) = repo_with(&[
        ("closed.rs", "pub fn alphamarker() {}\n".to_string()),
        ("quarantinedhuge.rs", oversize_source()),
    ]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let open = atlas
        .register_git("open", open_repo.path(), "HEAD")
        .unwrap();
    let closed = atlas
        .register_git("closed", closed_repo.path(), "HEAD")
        .unwrap();
    let open_generation = staged(
        atlas
            .acquire(&open, &open_revision, ExtractorPolicy::default())
            .unwrap(),
    );
    let closed_generation = staged(
        atlas
            .acquire(&closed, &closed_revision, ExtractorPolicy::default())
            .unwrap(),
    );
    assert_one_real_extraction_error(&closed_generation);
    atlas.publish(&open, &open_generation.id).unwrap();
    atlas.publish(&closed, &closed_generation.id).unwrap();

    // The requester is bound to `open` and to nothing else. This is a
    // real scope over a source that really exists and really is broken —
    // not a name nothing is registered under.
    let answer = search(&atlas, &request(work(&["open"]), "alphamarker")).unwrap();
    assert_eq!(
        answer.hits.len(),
        1,
        "positive control: its own source is searched"
    );
    assert!(
        !answer.coverage.source_extraction_incomplete,
        "the failure belongs to a source this requester was never shown; \
         disclosing it here would be a disclosure the scope refused"
    );
    assert!(answer.coverage.is_complete());
    let rendered = format!("{answer:?}");
    for needle in ["closed", "quarantinedhuge", "exceeds bounded"] {
        assert!(
            !rendered.contains(needle),
            "the denied source leaked {needle:?} into an answer built for a \
             requester bound only to `open`"
        );
    }

    // And the admitted requester really is told, so the silence above is
    // scope and not a disabled control.
    let admitted = search(&atlas, &request(work(&["closed"]), "alphamarker")).unwrap();
    assert!(admitted.coverage.source_extraction_incomplete);
}

// -- 5. Repair recovers; what was captured stays captured. --

#[test]
fn a_repaired_republished_generation_recovers_while_the_pinned_old_one_keeps_its_own_truth() {
    let (repo, first_revision) = repo_with(&[
        ("small.rs", "pub fn alphamarker() {}\n".to_string()),
        ("huge.rs", oversize_source()),
    ]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let membership = atlas.register_git("code", repo.path(), "HEAD").unwrap();
    let broken = staged(
        atlas
            .acquire(&membership, &first_revision, ExtractorPolicy::default())
            .unwrap(),
    );
    assert_one_real_extraction_error(&broken);
    atlas.publish(&membership, &broken.id).unwrap();
    let before = search(&atlas, &request(work(&["code"]), "alphamarker")).unwrap();
    assert!(before.coverage.source_extraction_incomplete);

    // A real refresh of the source itself: a new commit that no longer
    // carries a blob past the extractor's budget. Nothing already
    // recorded is edited.
    fs::remove_file(repo.path().join("huge.rs")).unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "drop the oversize blob"]);
    let second_revision = git(repo.path(), &["rev-parse", "HEAD"]);
    let repaired = staged(
        atlas
            .acquire(&membership, &second_revision, ExtractorPolicy::default())
            .unwrap(),
    );
    assert!(
        repaired
            .resources
            .iter()
            .all(|resource| resource.disposition != CoverageDisposition::Error),
        "positive control: the repaired revision has no extraction failure"
    );
    atlas.publish(&membership, &repaired.id).unwrap();

    let after = search(&atlas, &request(work(&["code"]), "alphamarker")).unwrap();
    assert!(
        !after.coverage.source_extraction_incomplete,
        "a repaired, republished generation must recover"
    );
    assert!(after.coverage.is_complete());

    // The old generation is still exactly what it was. A continuation
    // pinned to it reads the vector it captured, and that vector still
    // holds a resource nothing could extract.
    let mut pinned_request = request(work(&["code"]), "alphamarker");
    pinned_request.pinned = Some(
        [(membership.id.clone(), broken.id.clone())]
            .into_iter()
            .collect(),
    );
    let pinned = search(&atlas, &pinned_request).unwrap();
    assert!(
        pinned.coverage.source_extraction_incomplete,
        "publication must not rewrite what an already-captured generation says \
         about itself"
    );
}

// -- 6. The shape the current default edition actually derives. --

#[test]
fn packed_multiline_units_name_exactly_the_committed_bytes_and_lines_they_span() {
    // `coordinates_contract.rs`'s per-line-index test now constructs the
    // historical `v3` edition on purpose, so the *default* edition's
    // coordinates need their own contract. This is it: real committed
    // bytes, real 1-based lines, no gaps and no overlap.
    let text = oversize_source();
    let truncated: String = text.lines().take(4000).map(|l| format!("{l}\n")).collect();
    assert!(
        truncated.len() > 64 * 1024,
        "the fixture must force more than one packed unit"
    );
    let (repo, revision) = repo_with(&[("packed.rs", truncated.clone())]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let membership = atlas.register_git("code", repo.path(), "HEAD").unwrap();
    let generation = staged(
        atlas
            .acquire(&membership, &revision, ExtractorPolicy::default())
            .unwrap(),
    );
    let resource = generation
        .resources
        .iter()
        .find(|resource| resource.path == b"packed.rs")
        .expect("the fixture resource");
    assert_eq!(resource.disposition, CoverageDisposition::Indexed);
    assert!(
        resource.units.len() > 1,
        "the fixture must derive more than one unit"
    );
    let bytes = truncated.as_bytes();
    let mut previous_end = 0u64;
    for unit in &resource.units {
        assert_eq!(
            unit.byte_start, previous_end,
            "units must be contiguous: {unit:?}"
        );
        assert!(
            unit.byte_end - unit.byte_start <= 64 * 1024,
            "a packed unit must stay inside the byte budget its name promises"
        );
        // The line coordinates must name exactly the lines these bytes
        // are, counted from the committed blob and not from the unit's
        // own bookkeeping.
        let before = bytes[..unit.byte_start as usize]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u64;
        let inside = bytes[unit.byte_start as usize..unit.byte_end as usize]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u64;
        assert_eq!(unit.line_start, before + 1, "{unit:?}");
        assert_eq!(unit.line_end, before + inside, "{unit:?}");
        previous_end = unit.byte_end;
    }
    assert_eq!(
        previous_end as usize,
        bytes.len(),
        "the packed units must cover the whole committed blob"
    );
}

/// Ruling 0142: a hit now reports *where* the query's terms are inside
/// its unit, and the presentation layer windows the reply around those
/// locations. That is only sound if a reported location is a token the
/// ranker actually scored — not a substring some second detector found.
///
/// Watched red before `tokens_with_offsets` existed (`EvidenceHit` had
/// no `matches` at all, so this does not compile against the base) and
/// asserted here against the tokenizer's own rule rather than against
/// `str::contains`: `alphamarkers` and `xalphamarker` contain the query
/// as text and are different tokens, so neither may be reported.
#[test]
fn reported_term_matches_are_tokens_the_ranker_scored_not_substrings() {
    let mut source = String::new();
    source.push_str("// alphamarker at the very top of the file\n");
    // Padding, so the unit is packed and the later occurrences sit far
    // past any head-of-unit budget.
    for line in 0..600 {
        source.push_str(&format!(
            "pub fn filler_{line}(argument: u32) -> u32 {{ argument.wrapping_add({line}) }}\n"
        ));
    }
    source.push_str("pub struct Alphamarker;\n");
    source.push_str("// alphamarkers and xalphamarker are other tokens\n");
    source.push_str("pub fn uses(value: &alphamarker) -> u32 { 0 }\n");

    let (repo, _) = repo_with(&[("code.rs", source.clone())]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("code", repo.path(), "HEAD").unwrap();
    let generation = staged(
        atlas
            .acquire(&member, "HEAD", ExtractorPolicy::default())
            .unwrap(),
    );
    atlas.publish(&member, &generation.id).unwrap();

    let answer = search(&atlas, &request(work(&["code"]), "alphamarker")).unwrap();
    let hit = answer
        .hits
        .iter()
        .find(|hit| hit.coordinate.path == b"code.rs")
        .expect("the file is indexed and matches");

    assert!(
        hit.snippet.len() > 2 * 1024,
        "the fixture must produce a packed unit larger than any display budget"
    );
    assert!(
        !hit.matches.is_empty(),
        "a lexical hit names where its query's terms are"
    );
    // Every reported location is the query's own token, taken from the
    // unit's committed text.
    for found in &hit.matches {
        let start = found.offset as usize;
        let end = start + found.len as usize;
        assert_eq!(hit.snippet[start..end].to_lowercase(), found.term);
        assert_eq!(found.term, "alphamarker");
    }
    // Ascending, and exactly the standalone occurrences: the three that
    // are their own token, and neither `alphamarkers` nor `xalphamarker`.
    let offsets: Vec<u64> = hit.matches.iter().map(|found| found.offset).collect();
    let mut sorted = offsets.clone();
    sorted.sort_unstable();
    assert_eq!(offsets, sorted);
    assert_eq!(
        offsets.len(),
        3,
        "`// alphamarker`, `Alphamarker` and `&alphamarker` are tokens; `alphamarkers` and \
         `xalphamarker` are not"
    );
    let excluded = source.find("alphamarkers").unwrap() as u64;
    assert!(
        !offsets.contains(&excluded),
        "a longer token that merely contains the query is not a match"
    );
    // The deepest match is genuinely far past a head-of-unit snippet,
    // which is the whole reason a window is needed.
    assert!(
        *offsets.last().unwrap() > 2 * 1024,
        "the fixture must place a match beyond the display budget"
    );
}

/// The same file, searched for a term it does not contain as a token,
/// reports no location at all — a hit's `matches` is a fact about this
/// query, not a property of the unit.
#[test]
fn a_unit_that_matches_nothing_reports_no_term_location() {
    let (repo, _) = repo_with(&[("code.rs", "pub fn alphamarker() {}\n".to_string())]);
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let member = atlas.register_git("code", repo.path(), "HEAD").unwrap();
    let generation = staged(
        atlas
            .acquire(&member, "HEAD", ExtractorPolicy::default())
            .unwrap(),
    );
    atlas.publish(&member, &generation.id).unwrap();

    let answer = search(&atlas, &request(work(&["code"]), "gammamarkerabsent")).unwrap();
    assert!(answer.hits.is_empty());
    assert!(answer.coverage.no_match);
}
