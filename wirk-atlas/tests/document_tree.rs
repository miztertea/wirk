//! Pins the local non-Git document collection contract — explicit admission, refresh, exact
//! resolution against the correct captured revision (unaffected by an
//! unrelated edit elsewhere in the same tree), disclosed historical
//! unavailability once the specific resolved resource itself moves on,
//! and unregistration that never touches originals.
//!
//! Mirrors `git_generations.rs`'s own fixture and coordinate style:
//! `AtlasStore::open` a throwaway estate, register/acquire against a
//! throwaway source directory, build an `ExactCoordinate` from the
//! staged generation's own resource/unit.

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, ExactCoordinate, ExtractorPolicy, Membership, ResolveOutcome,
    ResolvedEvidence, SourceGeneration,
};

fn coordinate(member: &Membership, generation: &SourceGeneration, path: &[u8]) -> ExactCoordinate {
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

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A document-tree source that sits *inside* an ambient Git repository
/// (here, in a directory the enclosing repo itself ignores) is admitted
/// as **exactly the directory named** — never as the enclosing
/// repository's own commit/tree identity, and its real files are
/// actually staged (`coverage.total` matches the fixture's own file
/// count). An explicit `git` acquisition of the same directory is a
/// different, valid thing (it enumerates relative to the directory
/// under the enclosing repository's own commit/tree and can honestly
/// report zero coverage when the directory is ignored there); what it
/// cannot do is admit the directory *as itself*, which is what this
/// policy is for.
#[test]
fn a_document_tree_inside_an_ignored_directory_of_an_ambient_git_repository_is_admitted_as_itself()
{
    let ancestor = TempDir::new().unwrap();
    git(ancestor.path(), &["init", "-q"]);
    git(ancestor.path(), &["config", "user.email", "t@example.test"]);
    git(ancestor.path(), &["config", "user.name", "t"]);
    fs::write(ancestor.path().join("tracked.txt"), "tracked\n").unwrap();
    git(ancestor.path(), &["add", "."]);
    git(ancestor.path(), &["commit", "-qm", "one"]);
    let ambient_head = {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(ancestor.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };

    // A subdirectory of the ambient repo, itself untracked and ignored
    // by it — the shape an explicit Git acquisition can only see
    // relative to the enclosing repository, honestly, at zero coverage.
    fs::write(ancestor.path().join(".gitignore"), "docs/\n").unwrap();
    let docs = ancestor.path().join("docs");
    fs::create_dir(&docs).unwrap();
    fs::write(docs.join("brief.md"), "# Client brief\n\nHello.\n").unwrap();
    fs::write(docs.join("notes.txt"), "plain notes\n").unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("client-docs", &docs, "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };

    // Never the ambient repository's own identity.
    assert_ne!(generation.revision, ambient_head);
    assert_eq!(
        generation.revision.len(),
        64,
        "a manifest hash, not a git sha"
    );
    assert!(generation.content.starts_with("sha256:"));
    assert_eq!(
        generation.acquisition_policy,
        wirk_atlas::DOCUMENT_TREE_POLICY
    );

    // Real coverage over the actual files, not zero.
    assert_eq!(generation.resources.len(), 2);
    let brief = generation
        .resources
        .iter()
        .find(|r| r.path == b"brief.md")
        .unwrap();
    assert_eq!(brief.disposition, wirk_atlas::CoverageDisposition::Indexed);
}

#[test]
fn publish_and_resolve_exact_round_trip_a_document_tree_generation() {
    const DOC: &[u8] = b"# Title\n\nBody text.\n";
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), DOC).unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    atlas.publish(&membership, &generation.id).unwrap();
    assert_eq!(
        atlas.current(&membership).unwrap().map(|g| g.id),
        Some(generation.id.clone())
    );
    let pinned = coordinate(&membership, &generation, b"a.md");
    // The markdown extractor's first unit for this document is the whole
    // section — its heading *and* the body under it — so the coordinate
    // names the entire file rather than the heading line alone. What the
    // round trip pins is the exactness: `resolve_exact` hands back
    // precisely the byte range its coordinate names, read back out of
    // the published generation rather than off the source tree.
    let expected = DOC[pinned.byte_start as usize..pinned.byte_end as usize].to_vec();
    assert_eq!(expected.len(), DOC.len(), "this unit covers the whole file");
    assert_eq!(
        atlas.resolve_exact(&membership, &pinned).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: pinned,
            bytes: expected,
        })
    );
}

/// A `.txt` an older edition recorded as a **document** still validates
/// and resolves byte-for-byte, under the edition that recorded it.
///
/// **Meaningful red before this change**, reproduced end to end on two
/// real frozen binaries against a real `wirkd`, not only here. The
/// earlier attempt at the glossary defect above added `.txt` to the
/// content-family table itself. That table is the vocabulary editions v3
/// through v6 all read, so the row rewrote what every one of them says a
/// `.txt` is — including generations already published. A document
/// collection holding a `notes.txt` whose bytes are real RTF acquires,
/// under the pre-change binary, as `indexed`, `ContentFamily::Document`,
/// units stamped with the document Markdown unitizer and offsets into
/// the *rendering* (78 bytes) rather than into the 140-byte original.
/// Pointed at that same estate, the post-change binary answered every
/// read — `atlas search`, `atlas resolve`, and even `atlas status` for
/// the whole estate — with
///
/// ```text
/// AtlasError generation is incomplete or absent:
///   derived retrieval unit identity or bounds are inconsistent
/// ```
///
/// which is the *forgery* refusal: `recorded_shape` now answered
/// `(Knowledge, text unitizer)` for a path the record says is a
/// `Document`. A published generation became unreadable, and the reason
/// it gave accused the record instead of naming the vocabulary that had
/// moved under it.
///
/// So the correction belongs to an edition, which is what editions are
/// for. `v6` keeps its own answer for `.txt` — sniff it, and read a real
/// container as the document it is — and `v7` carries the widened
/// vocabulary for everything acquired from here on. This test stages
/// both over the identical tree and pins that they disagree, on purpose,
/// each one internally consistent from admission through resolution.
#[test]
fn a_historical_edition_still_reads_its_own_txt_the_way_it_recorded_it() {
    // A real RTF body, under a name that says nothing about it.
    const RTF: &[u8] = b"{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\froman Times;}}\n\
\\f0\\fs24 Waypoint admission is recorded under the edition that produced it.\\par\n}\n";
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("notes.txt"), RTF).unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();

    // --- what the older edition recorded, and still reads back --------
    let historical = atlas
        .register_document_tree("archive", source.path(), "current")
        .unwrap();
    let v6 = {
        let outcome = atlas
            .acquire_document_tree(
                &historical,
                "current",
                ExtractorPolicy::documents_detected_v6(),
            )
            .unwrap();
        read_staged(&atlas, outcome)
    };
    let recorded = v6
        .resources
        .iter()
        .find(|record| record.path == b"notes.txt")
        .expect("v6 captured the file");
    assert_eq!(
        recorded.disposition,
        wirk_atlas::CoverageDisposition::Indexed,
        "v6 sniffs an unrecognized name and finds the RTF"
    );
    let unit = recorded.units.first().unwrap();
    assert_eq!(
        unit.family,
        wirk_atlas::ContentFamily::Document,
        "v6 recorded this as a document, and that is what must be preserved"
    );
    assert!(
        unit.byte_end < RTF.len() as u64,
        "the unit indexes the Markdown rendering, which is shorter than the container"
    );

    // Publishing re-verifies the whole tree under the generation's own
    // edition, and resolving reads its bytes back under that same one.
    // Both would fail if the vocabulary had been widened in place.
    atlas.publish(&historical, &v6.id).unwrap();
    let pinned = coordinate(&historical, &v6, b"notes.txt");
    let ResolveOutcome::Resolved(ResolvedEvidence { bytes, .. }) =
        atlas.resolve_exact(&historical, &pinned).unwrap()
    else {
        panic!("a published v6 coordinate must still resolve");
    };
    assert_eq!(
        String::from_utf8(bytes).unwrap().trim(),
        "Waypoint admission is recorded under the edition that produced it.",
        "the converted text the unit's offsets actually describe"
    );

    // --- and what the current edition does with the same file ---------
    let current = atlas
        .register_document_tree("live", source.path(), "current")
        .unwrap();
    let v7 = {
        let outcome = atlas
            .acquire_document_tree(&current, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    let now = v7
        .resources
        .iter()
        .find(|record| record.path == b"notes.txt")
        .expect("v7 captured the file");
    let unit = now.units.first().unwrap();
    assert_eq!(
        unit.family,
        wirk_atlas::ContentFamily::Knowledge,
        "under the current edition a .txt is the text its name claims"
    );
    atlas.publish(&current, &v7.id).unwrap();
    let pinned = coordinate(&current, &v7, b"notes.txt");
    let ResolveOutcome::Resolved(ResolvedEvidence { bytes, .. }) =
        atlas.resolve_exact(&current, &pinned).unwrap()
    else {
        panic!("the current coordinate resolves too");
    };
    assert_eq!(
        bytes,
        RTF[pinned.byte_start as usize..pinned.byte_end as usize].to_vec(),
        "and its offsets index the file's own bytes, not a rendering"
    );

    // The original is untouched by either reading.
    assert_eq!(fs::read(source.path().join("notes.txt")).unwrap(), RTF);
}

/// The glossary a colleague actually dropped into a document collection.
///
/// **Meaningful red before this change** (reproduced against a real
/// `wirkd` on a binary built before it, not only here): this exact
/// 152-byte `glossary.txt` previewed and acquired as
/// `unsupported`/`Unsupported("no extractor for path family")` beside an
/// indexed `.md`, and searching for its own words returned no hits. The
/// cause is not the document reader: the content-family extension
/// vocabulary is an extension-to-*language* map, and a name that names
/// no language carries no row in it. Ordinary text-family interpretation
/// is supposed to be preserved, so that omission was the defect, and the
/// current extraction edition carries the answer the shared table cannot.
///
/// A genuinely non-text neighbour in the same tree must keep its correct
/// refusal, which is why `photo.png` is here: widening the text
/// vocabulary must not become a catch-all that decodes binary as UTF-8.
#[test]
fn an_ordinary_plain_text_file_is_indexed_and_resolves_to_its_own_bytes() {
    const GLOSSARY: &[u8] = b"Estate: the bounded domain a colleague is working in.\n\
World: the assembled context for one piece of work.\n\
Claim: how a Run's outcome becomes checkable.\n";
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("glossary.txt"), GLOSSARY).unwrap();
    fs::write(
        source.path().join("onboarding.md"),
        "# Onboarding\n\nHello.\n",
    )
    .unwrap();
    fs::write(source.path().join("photo.png"), [0x89u8, b'P', b'N', b'G']).unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("colleague-docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };

    let glossary = generation
        .resources
        .iter()
        .find(|record| record.path == b"glossary.txt")
        .expect("the glossary is captured as a resource at all");
    assert_eq!(
        glossary.disposition,
        wirk_atlas::CoverageDisposition::Indexed,
        "an ordinary .txt is read, not refused"
    );
    assert!(
        !glossary.units.is_empty(),
        "indexed means it actually produced text units"
    );

    // The non-text neighbour keeps its truthful refusal.
    let photo = generation
        .resources
        .iter()
        .find(|record| record.path == b"photo.png")
        .expect("the png is still reported, not dropped");
    assert_eq!(
        photo.disposition,
        wirk_atlas::CoverageDisposition::Unsupported
    );

    // And the glossary's own bytes come back out of the published
    // generation, byte for byte -- useful content, not just a count.
    atlas.publish(&membership, &generation.id).unwrap();
    let pinned = coordinate(&membership, &generation, b"glossary.txt");
    let expected = GLOSSARY[pinned.byte_start as usize..pinned.byte_end as usize].to_vec();
    assert!(
        expected.starts_with(b"Estate: the bounded domain"),
        "the resolved range is the glossary's own text"
    );
    assert_eq!(
        atlas.resolve_exact(&membership, &pinned).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: pinned,
            bytes: expected,
        })
    );

    // The original file on disk is untouched by any of it.
    assert_eq!(
        fs::read(source.path().join("glossary.txt")).unwrap(),
        GLOSSARY
    );
}

/// Pinned at the public `AtlasStore` level, not just at `doctree::blob`
/// directly: an edit to a *different* file in the same collection must
/// not invalidate a coordinate for a file that did not change. The
/// refresh test below mutates the very file it later resolves, which
/// cannot tell this behaviour from the
/// one it replaced.
#[test]
fn resolving_an_unchanged_resource_survives_an_unrelated_edit_elsewhere_in_the_tree() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("stable.md"), "# stable\n").unwrap();
    fs::write(source.path().join("other.md"), "# v1\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    atlas.publish(&membership, &generation.id).unwrap();
    let pinned = coordinate(&membership, &generation, b"stable.md");

    // An edit to a completely different file...
    fs::write(source.path().join("other.md"), "# v2, changed\n").unwrap();

    // ...must not disturb resolution of the unchanged one, against the
    // *same*, still-published generation (no refresh/publish here).
    assert_eq!(
        atlas.resolve_exact(&membership, &pinned).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: pinned,
            bytes: b"# stable\n".to_vec(),
        })
    );
}

/// The brief's own required demonstration: "old versus current
/// revision and disclosed historical availability". A document tree
/// keeps no durable copy of historical bytes the way Git's object
/// store does, so once the *specific resolved file* changes, the *old*
/// generation is still a distinct, separately-readable record
/// (`atlas.generation`), but resolving text against it is an honest
/// `Unavailable`, never wrong bytes and never a crash. The *current*
/// generation continues to resolve normally.
#[test]
fn refresh_yields_a_new_generation_and_the_old_one_discloses_unavailable_once_the_resolved_file_moves_on()
 {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), "# v1\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let old = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    atlas.publish(&membership, &old.id).unwrap();
    let old_pinned = coordinate(&membership, &old, b"a.md");

    fs::write(source.path().join("a.md"), "# v2\n").unwrap();
    let refreshed = {
        let outcome = atlas
            .refresh_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    assert_ne!(refreshed.id, old.id, "content changed, so identity changed");

    // The old generation is still its own immutable, separately
    // readable record...
    assert_eq!(atlas.generation(&old.id).unwrap().id, old.id);
    // ...but resolving exact text against it is now an honest,
    // disclosed unavailability — the one file it names has moved on,
    // and this policy keeps no historical byte store to answer from.
    match atlas.resolve_exact(&membership, &old_pinned).unwrap() {
        ResolveOutcome::Unavailable(_) => {}
        other => panic!("expected Unavailable for a stale document-tree coordinate, got {other:?}"),
    }

    // Publishing and resolving the *current* state still works.
    atlas.publish(&membership, &refreshed.id).unwrap();
    let new_pinned = coordinate(&membership, &refreshed, b"a.md");
    assert_eq!(
        atlas.resolve_exact(&membership, &new_pinned).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: new_pinned,
            bytes: b"# v2\n".to_vec(),
        })
    );
}

/// A renamed/deleted resource behaves the same honest way: the old
/// coordinate's own path no longer names a member of the refreshed
/// generation, but that is decided by `resolve_exact`'s own path/
/// object-id checks, never by a crash or a wrong read.
#[test]
fn a_deleted_resource_discloses_unavailable_after_refresh_and_publish() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), "# keep\n").unwrap();
    fs::write(source.path().join("b.md"), "# gone soon\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let old = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    atlas.publish(&membership, &old.id).unwrap();
    let old_pinned = coordinate(&membership, &old, b"b.md");

    fs::remove_file(source.path().join("b.md")).unwrap();
    let refreshed = {
        let outcome = atlas
            .refresh_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    atlas.publish(&membership, &refreshed.id).unwrap();

    match atlas.resolve_exact(&membership, &old_pinned).unwrap() {
        ResolveOutcome::Unavailable(_) => {}
        other => panic!("expected Unavailable for a deleted resource, got {other:?}"),
    }
}

/// Explicit admission never manufactures a Git repository and never
/// infers its kind from `.git`'s absence: acquiring the *same*
/// membership through the Git-only method is refused outright rather
/// than silently reinterpreted.
#[test]
fn acquiring_a_document_tree_membership_through_the_git_method_is_refused() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), "# hi\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let error = atlas
        .acquire(&membership, "current", ExtractorPolicy::default())
        .unwrap_err();
    assert!(error.to_string().contains("document-tree-policy/v1"));
}

/// Re-registering the same alias under a different policy is refused —
/// a source's kind, once chosen, is never silently reinterpreted.
#[test]
fn re_registering_an_existing_alias_under_a_different_policy_is_refused() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), "# hi\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let error = atlas
        .register_git("docs", source.path(), "current")
        .unwrap_err();
    assert!(error.to_string().contains("acquisition policy"));
}

/// Same-name files in two different document-tree sources must never
/// collapse identity or permit cross-boundary resolution: identical
/// relative paths and identical bytes in two different roots still
/// produce two distinct memberships/sources, and a coordinate minted
/// from one is refused against the other.
#[test]
fn identical_files_in_two_document_tree_sources_do_not_collapse_identity() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    fs::write(a.path().join("readme.md"), "# same\n").unwrap();
    fs::write(b.path().join("readme.md"), "# same\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let member_a = atlas
        .register_document_tree("source-a", a.path(), "current")
        .unwrap();
    let member_b = atlas
        .register_document_tree("source-b", b.path(), "current")
        .unwrap();
    let generation_a = {
        let outcome = atlas
            .acquire_document_tree(&member_a, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    let generation_b = {
        let outcome = atlas
            .acquire_document_tree(&member_b, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    assert_ne!(member_a.source, member_b.source);
    assert_ne!(generation_a.id, generation_b.id);
    let coordinate_a = coordinate(&member_a, &generation_a, b"readme.md");
    // A coordinate minted against source A, presented against
    // membership B, is refused rather than silently resolved.
    let error = atlas.resolve_exact(&member_b, &coordinate_a).unwrap_err();
    assert!(error.to_string().contains("outside this membership"));
}

/// An input over a bound **this estate configured** is reported
/// `Unsupported` with a disclosed reason rather than read unboundedly,
/// and this estate's `resolve_exact` never touches it (its disposition
/// alone answers a coordinate request for it).
///
/// The bound is written into the estate's own `resources.json` here.
/// Since ruling 0401 there is no built-in per-file bound to inherit: a
/// collection an operator admitted is read as admitted unless they
/// asked for otherwise, and this is the "asked for otherwise" case.
#[test]
fn an_oversize_file_is_disclosed_bounded_and_never_indexed() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("normal.md"), "# ok\n").unwrap();
    fs::write(source.path().join("huge.md"), vec![b'x'; 4_097]).unwrap();
    let estate = TempDir::new().unwrap();
    fs::create_dir_all(estate.path().join(".wirk")).unwrap();
    fs::write(
        estate.path().join(".wirk").join("resources.json"),
        r#"{ "document_max_file_bytes": 4096 }"#,
    )
    .unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    let huge = generation
        .resources
        .iter()
        .find(|r| r.path == b"huge.md")
        .unwrap();
    assert_eq!(
        huge.disposition,
        wirk_atlas::CoverageDisposition::Unsupported
    );
    assert!(
        huge.detail
            .as_deref()
            .unwrap_or_default()
            .contains("bounded")
    );
}

/// `remove_source` is a **catalog-only unregister**. It clears the membership (so
/// `resolve_exact`/`acquire` against it refuse afterward), reports the
/// generation it released, and never touches the source's own original
/// files. It also does not delete this estate's own generation
/// directory itself, because that byte removal belongs entirely to
/// `wirk estate clean` (exercised at the real CLI/daemon level, where
/// `derive_retention` actually lives; not reachable from this crate's
/// own tests).
#[test]
fn remove_source_unregisters_the_catalog_and_leaves_generation_bytes_and_originals_untouched() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), "# original\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    atlas.publish(&membership, &generation.id).unwrap();

    let outcome = atlas.remove_source(&membership).unwrap();
    assert_eq!(outcome.released_generation, Some(generation.id.clone()));
    assert!(atlas.memberships().all(|member| member.alias != "docs"));

    // The membership itself is gone: further use of the stale handle is
    // refused, not silently served.
    assert!(atlas.current(&membership).is_err());
    assert!(
        atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .is_err()
    );

    // The generation's own bytes are untouched — reclaiming them is
    // `wirk estate clean`'s job, not this call's.
    assert_eq!(atlas.generation(&generation.id).unwrap().id, generation.id);

    // The source's own original file is, as always, completely
    // untouched.
    assert_eq!(
        fs::read_to_string(source.path().join("a.md")).unwrap(),
        "# original\n"
    );
}

/// A document-tree source has no
/// revision beside its own current state. Any other `--revision`-style
/// string is refused by name, at both registration and acquisition,
/// rather than silently recorded as if it had been honoured.
#[test]
fn a_document_tree_refuses_any_requested_revision_other_than_current() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), "# hi\n").unwrap();
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();

    let error = atlas
        .register_document_tree("docs", source.path(), "v2")
        .unwrap_err();
    assert!(error.to_string().contains("current"));

    let membership = atlas
        .register_document_tree(
            "docs",
            source.path(),
            wirk_atlas::DOCUMENT_TREE_CURRENT_OBSERVATION,
        )
        .unwrap();
    let error = atlas
        .acquire_document_tree(&membership, "deadbeef", ExtractorPolicy::default())
        .unwrap_err();
    assert!(error.to_string().contains("current"));
    let error = atlas
        .refresh_document_tree(&membership, "deadbeef", ExtractorPolicy::default())
        .unwrap_err();
    assert!(error.to_string().contains("current"));
}

/// Same relative path admitted independently in two different estates
/// must resolve independently: an `ExactCoordinate` names its own
/// `EstateScope`, and `resolve_exact` checks it.
#[test]
fn the_same_relative_path_in_two_estates_resolves_independently() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("readme.md"), "# same everywhere\n").unwrap();

    let estate_a = TempDir::new().unwrap();
    let mut atlas_a = AtlasStore::open(estate_a.path(), "estate-a").unwrap();
    let member_a = atlas_a
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation_a = {
        let outcome = atlas_a
            .acquire_document_tree(&member_a, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas_a, outcome)
    };

    let estate_b = TempDir::new().unwrap();
    let mut atlas_b = AtlasStore::open(estate_b.path(), "estate-b").unwrap();
    let member_b = atlas_b
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation_b = {
        let outcome = atlas_b
            .acquire_document_tree(&member_b, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas_b, outcome)
    };

    let coordinate_a = coordinate(&member_a, &generation_a, b"readme.md");
    assert_eq!(
        atlas_a.resolve_exact(&member_a, &coordinate_a).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: coordinate_a.clone(),
            bytes: b"# same everywhere\n".to_vec(),
        })
    );
    // A coordinate minted in estate A's own scope is outside estate B's
    // membership entirely, refused rather than resolved.
    let error = atlas_b.resolve_exact(&member_b, &coordinate_a).unwrap_err();
    assert!(error.to_string().contains("outside this membership"));

    let coordinate_b = coordinate(&member_b, &generation_b, b"readme.md");
    assert_eq!(
        atlas_b.resolve_exact(&member_b, &coordinate_b).unwrap(),
        ResolveOutcome::Resolved(ResolvedEvidence {
            coordinate: coordinate_b,
            bytes: b"# same everywhere\n".to_vec(),
        })
    );
}

/// One unreadable document does not
/// refuse admission of the rest of the collection — the acquisition
/// still stages, and the unreadable file's own disposition discloses
/// it, rather than the whole generation coming back `Unavailable`.
#[cfg(unix)]
#[test]
fn acquisition_stages_despite_one_unreadable_document() {
    use std::os::unix::fs::PermissionsExt;
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("readable.md"), "# ok\n").unwrap();
    let blocked = source.path().join("blocked.md");
    fs::write(&blocked, "# secret\n").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let result = atlas.acquire_document_tree(&membership, "current", ExtractorPolicy::default());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o644)).unwrap();
    let generation = {
        let outcome = result.unwrap();
        read_staged(&atlas, outcome)
    };

    let blocked_record = generation
        .resources
        .iter()
        .find(|r| r.path == b"blocked.md")
        .unwrap();
    assert_eq!(
        blocked_record.disposition,
        wirk_atlas::CoverageDisposition::Unavailable
    );
    let readable_record = generation
        .resources
        .iter()
        .find(|r| r.path == b"readable.md")
        .unwrap();
    assert_eq!(
        readable_record.disposition,
        wirk_atlas::CoverageDisposition::Indexed
    );
}

/// A document tree nested deeper than the 128 directories that used to
/// be the built-in traversal depth is **acquired**, through the real
/// public acquisition path, and the file at the bottom is indexed.
///
/// A depth number never proved the thing it was written for. What it
/// stood in for is a directory that is its own ancestor, which the walk
/// now detects by identity; what depth actually costs is one open
/// descriptor per level, which the operating system reports as its own
/// limit. Refusing an operator's real collection for being 129 deep was
/// a product-chosen threshold refusing admitted work (ruling 0401).
///
/// Watched failing against the previous default, where this acquisition
/// returned "exceeds the 128-directory bounded traversal depth".
///
/// A depth an operator *does* configure still refuses outright, not
/// silently truncated into a generation reported as complete — the
/// second half of this check.
#[test]
fn a_deeply_nested_document_tree_is_acquired_and_a_configured_depth_still_refuses() {
    let source = TempDir::new().unwrap();
    let mut path = source.path().to_path_buf();
    for i in 0..200 {
        path.push(format!("d{i}"));
        fs::create_dir(&path).unwrap();
    }
    fs::write(path.join("deep.md"), "# deep but ordinary\n").unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .expect("a deep collection is acquired, not refused for its depth");
        read_staged(&atlas, outcome)
    };
    let deep = generation
        .resources
        .iter()
        .find(|r| r.path.ends_with(b"deep.md"))
        .expect("the file at the bottom is in the generation");
    assert_eq!(deep.disposition, wirk_atlas::CoverageDisposition::Indexed);

    // The same tree, against an estate that asked for a depth bound.
    let bounded_estate = TempDir::new().unwrap();
    fs::create_dir_all(bounded_estate.path().join(".wirk")).unwrap();
    fs::write(
        bounded_estate.path().join(".wirk").join("resources.json"),
        r#"{ "document_max_entries_depth": 8 }"#,
    )
    .unwrap();
    let mut bounded = AtlasStore::open(bounded_estate.path(), "estate-b").unwrap();
    let bounded_membership = bounded
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let error = bounded
        .acquire_document_tree(&bounded_membership, "current", ExtractorPolicy::default())
        .unwrap_err();
    assert!(
        error.to_string().contains("bounded traversal depth"),
        "a configured depth still refuses by name: {error}"
    );
}

/// An old catalog written before `Membership::policy` existed still
/// opens and its membership still means exactly what it always meant
/// (a Git source) — the additive-migration contract this crate already
/// holds itself to for `semantic_selected` (`semantic_lifecycle.rs`
/// `d_old_catalog_without_semantic_field_still_opens`), reused here for
/// the new field.
#[test]
fn a_catalog_membership_without_a_policy_field_still_opens_as_a_git_source() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "t@example.test"]);
    git(repo.path(), &["config", "user.name", "t"]);
    fs::write(repo.path().join("a.rs"), "fn a() {}\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "one"]);

    let estate = TempDir::new().unwrap();
    {
        let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
        atlas.register_git("source", repo.path(), "HEAD").unwrap();
    }
    let catalog_path = estate.path().join("atlas").join("catalog.json");
    let mut catalog: serde_json::Value =
        serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    catalog["memberships"]["source"]
        .as_object_mut()
        .unwrap()
        .remove("policy")
        .unwrap();
    fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();

    let mut reopened = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let membership = reopened
        .memberships()
        .find(|member| member.alias == "source")
        .unwrap()
        .clone();
    assert_eq!(membership.policy, "git-tree-policy/v1");
    assert_ne!(membership.policy, wirk_atlas::DOCUMENT_TREE_POLICY);
    // And it still functions exactly as a Git source.
    let outcome = reopened
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap();
    assert!(matches!(outcome, AcquireOutcome::Staged(_)));
}

// ---------------------------------------------------------------------
// The entry-classified window, driven rather than slept on
// ---------------------------------------------------------------------
//
// `DOCTREE_OPEN_WINDOW` is the real instant between the `lstat` that
// classifies an entry and the `openat` that acts on it. What the flags
// and post-open checks around it exist to survive is a replacement in
// that instant, and a replacement cannot be pinned by a sleep: one real
// thread has to be held at the real window while another does the
// substitution. `wirk_atlas::checkpoint`'s barrier is process-level, so
// the held thread is a child process re-running one worker test here —
// the same shape `findings_index.rs` already drives its own crash and
// durability windows with, reused rather than reinvented.
//
// Each fixture below contains **exactly one** entry that reaches the
// window, so the single thread the barrier arms is parked on that entry
// and no other.

/// Every bound in these controls belongs to this controller, never to
/// the gate: the parked thread has no notion of elapsed time, so an
/// exhausted bound here reports a state that was never observed rather
/// than returning a verdict.
const SUPERVISION: Duration = Duration::from_secs(60);

fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo").arg(path).status().unwrap();
    assert!(status.success(), "mkfifo {}", path.display());
}

/// Wait for the parked worker to arrive at the window.
fn arrival(listener: &UnixListener, never: &str) -> std::os::unix::net::UnixStream {
    let deadline = Instant::now() + SUPERVISION;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept on the release socket failed: {error}"),
        }
        assert!(Instant::now() < deadline, "{never}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Reap the worker under a bound of this controller's own.
///
/// The defect each of these controls is aimed at is a *parked* walk —
/// a thread blocked in `open` on a FIFO with no writer. A plain
/// `wait_with_output` would inherit that park and wedge the whole test
/// binary instead of failing, so the wait is bounded and an exhausted
/// bound kills the worker and says exactly what was not observed.
fn worker_output(mut child: std::process::Child) -> String {
    let deadline = Instant::now() + SUPERVISION;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let mut stdout = String::new();
                if let Some(pipe) = child.stdout.as_mut() {
                    std::io::Read::read_to_string(pipe, &mut stdout).unwrap();
                }
                assert!(status.success(), "the worker failed: {status:?}\n{stdout}");
                return stdout;
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "the capture was never observed to finish after the window was released: \
                         it is parked, which is the defect the non-blocking open exists to stop"
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn armed_worker(
    test: &str,
    barrier: &Path,
    envs: &[(&str, &Path)],
) -> (UnixListener, std::process::Child) {
    fs::create_dir_all(barrier).unwrap();
    // Bound before the worker exists, so the window it is told to park
    // on always has a reachable release socket when it gets there.
    let listener = UnixListener::bind(barrier.join(wirk_atlas::BARRIER_RELEASE_SOCKET)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg(test)
        .arg("--nocapture")
        .env(
            "WIRK_ATLAS_BARRIER",
            format!("{}={}", wirk_atlas::DOCTREE_OPEN_WINDOW, barrier.display()),
        )
        .stdout(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    // Armed before the worker is spawned: the worker can reach
    // `OPEN_WINDOW` as soon as it starts, and an arm written after spawn
    // races it — if the worker parks first, `barrier()`'s rename finds
    // no `arm` file, runs straight through, and the controller times out
    // waiting for a release that was never requested.
    fs::write(barrier.join("arm"), b"").unwrap();
    let child = command.spawn().unwrap();
    (listener, child)
}

/// The worker half of the capture controls: acquires the collection
/// named by the environment and prints what each entry was resolved to
/// be. Inert without its own variables, so an ordinary run of this
/// binary skips it.
#[test]
fn worker_captures_a_document_collection() {
    let (Some(estate), Some(docs)) = (
        std::env::var_os("WIRK_DOCTREE_WINDOW_ESTATE"),
        std::env::var_os("WIRK_DOCTREE_WINDOW_DOCS"),
    ) else {
        return;
    };
    let mut atlas = AtlasStore::open(&estate, "estate-window").unwrap();
    let membership = atlas
        .register_document_tree("docs", Path::new(&docs), "current")
        .unwrap();
    match atlas.acquire_document_tree(&membership, "current", ExtractorPolicy::default()) {
        Ok(AcquireOutcome::Staged(staged)) => {
            let generation = atlas
                .generation(&staged.id)
                .expect("the generation just staged reads back");
            for record in &generation.resources {
                println!(
                    "ENTRY={} DISPOSITION={:?} DETAIL={:?}",
                    String::from_utf8_lossy(&record.path),
                    record.disposition,
                    record.detail
                );
            }
            println!("OUTCOME=staged");
        }
        Ok(other) => println!("OUTCOME=other {other:?}"),
        Err(error) => println!("OUTCOME=error {error}"),
    }
}

/// A regular file replaced by a FIFO **inside** the classify/open window
/// must leave the walk running and the entry disclosed. Without
/// `O_NONBLOCK` on that open the walk parks on a FIFO with no writer,
/// holding whatever lock its caller took; the supervision bound above
/// is what turns that into a failure rather than a wedged suite.
#[test]
fn a_regular_file_that_becomes_a_fifo_at_the_open_window_is_disclosed_and_never_parks() {
    let dir = TempDir::new().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    let target = docs.join("swap.md");
    fs::write(&target, "# swap\n\nreal body\n").unwrap();

    let barrier = dir.path().join("barrier");
    let (listener, child) = armed_worker(
        "worker_captures_a_document_collection",
        &barrier,
        &[
            ("WIRK_DOCTREE_WINDOW_ESTATE", estate.as_path()),
            ("WIRK_DOCTREE_WINDOW_DOCS", docs.as_path()),
        ],
    );
    let parked = arrival(
        &listener,
        "the capture never reached the entry-classified window",
    );

    // The real substitution, at the real instant, by this process: the
    // entry the worker has already classified as an ordinary file is a
    // FIFO with no writer by the time it opens it.
    fs::remove_file(&target).unwrap();
    mkfifo(&target);
    drop(parked);

    let stdout = worker_output(child);
    assert!(
        stdout.contains("OUTCOME=staged"),
        "one substituted entry does not refuse the collection: {stdout}"
    );
    assert!(
        stdout.contains("ENTRY=swap.md DISPOSITION=Unavailable"),
        "the substituted entry is disclosed as unavailable, not silently dropped and not read as \
         if it were still the file that was classified: {stdout}"
    );
}

/// A directory replaced by a symlink in the same window must be refused
/// and never followed — `O_NOFOLLOW | O_DIRECTORY` reports `ELOOP`,
/// which the walk records as the symlink it now is. The decisive half is
/// the negative one: nothing behind the symlink appears in coverage.
#[test]
fn a_directory_that_becomes_a_symlink_at_the_open_window_is_refused_and_not_followed() {
    let dir = TempDir::new().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    let target = docs.join("swap");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("inner.md"), "# inner\n").unwrap();

    // Outside the collection entirely. If the swap were followed this
    // file would be walked, which is the escape being ruled out.
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("outside-the-collection.md"), "# elsewhere\n").unwrap();

    let barrier = dir.path().join("barrier");
    let (listener, child) = armed_worker(
        "worker_captures_a_document_collection",
        &barrier,
        &[
            ("WIRK_DOCTREE_WINDOW_ESTATE", estate.as_path()),
            ("WIRK_DOCTREE_WINDOW_DOCS", docs.as_path()),
        ],
    );
    let parked = arrival(
        &listener,
        "the capture never reached the entry-classified window",
    );

    fs::remove_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
    drop(parked);

    let stdout = worker_output(child);
    assert!(
        stdout.contains("OUTCOME=staged"),
        "the collection is still captured: {stdout}"
    );
    // Recorded `Unavailable`, not `Unsupported`: the walk classified
    // `swap` as a directory from its own `lstat`, and the swap lands
    // between that and the `openat(O_NOFOLLOW|O_DIRECTORY)`, which then
    // fails `ENOTDIR`. That error is what the object this walk actually
    // touched returned, and it is what gets recorded. Calling it a
    // symlink instead would mean going back to the path for a second
    // lookup after the failed open — reintroducing exactly the
    // re-derivation `CapturedKind`'s single lstat/openat sequence exists
    // to avoid, and reporting a classification of whatever is at the
    // name *now* rather than of what was refused. The disposition that
    // matters is that the entry is refused and carries its reason; the
    // escape itself is ruled out by the two assertions below.
    assert!(
        stdout.contains("ENTRY=swap DISPOSITION=Unavailable"),
        "the swapped directory is refused and its reason recorded: {stdout}"
    );
    assert!(
        stdout.contains("Not a directory"),
        "the refusal names what the open actually returned: {stdout}"
    );
    assert!(
        !stdout.contains("outside-the-collection.md"),
        "nothing behind the symlink was walked: {stdout}"
    );
    assert!(
        !stdout.contains("inner.md"),
        "and the directory that was replaced is not reported as though it had been descended \
         into either: {stdout}"
    );
}

/// The same window on the **live resolution** path.
///
/// A coordinate whose file becomes a FIFO between the resolver's own
/// classify and open must come back unavailable rather than parking the
/// resolve. The resolver uses the same `open_no_follow` the walk does,
/// and this is the arm that proves it on the path a query actually
/// takes — where a parked thread would be holding the daemon's atlas
/// mutex for as long as the FIFO has no writer.
///
/// The acquisition and publication happen in **this** process, before
/// the worker is spawned at all, so the only arrival at the window is
/// the resolve. The store's own ownership lock is what orders the two:
/// the worker cannot open the estate until this process has dropped it.
#[test]
fn worker_resolves_one_document_coordinate() {
    let Some(estate) = std::env::var_os("WIRK_DOCTREE_RESOLVE_ESTATE") else {
        return;
    };
    let atlas = AtlasStore::open(&estate, "estate-resolve").unwrap();
    let membership = atlas
        .memberships()
        .find(|member| member.alias == "docs")
        .expect("the controller registered this source before spawning")
        .clone();
    let generation = atlas
        .current(&membership)
        .unwrap()
        .expect("the controller published a generation before spawning");
    let coordinate = coordinate(&membership, &generation, b"swap.md");
    match atlas.resolve_exact(&membership, &coordinate) {
        Ok(outcome) => println!("RESOLVE=ok {outcome:?}"),
        Err(error) => println!("RESOLVE=error {error}"),
    }
}

#[test]
fn a_resolved_document_that_becomes_a_fifo_at_the_open_window_is_unavailable_and_never_parks() {
    let dir = TempDir::new().unwrap();
    let estate = dir.path().join("estate");
    fs::create_dir_all(&estate).unwrap();
    let docs = dir.path().join("docs");
    fs::create_dir_all(&docs).unwrap();
    let target = docs.join("swap.md");
    fs::write(&target, "# swap\n\nreal body\n").unwrap();

    // Acquired and published here, with no barrier armed anywhere, and
    // the store dropped so the worker can take ownership.
    {
        let mut atlas = AtlasStore::open(&estate, "estate-resolve").unwrap();
        let membership = atlas
            .register_document_tree("docs", &docs, "current")
            .unwrap();
        let generation = {
            let outcome = atlas
                .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
                .unwrap();
            read_staged(&atlas, outcome)
        };
        atlas.publish(&membership, &generation.id).unwrap();
    }

    let barrier = dir.path().join("barrier");
    let (listener, child) = armed_worker(
        "worker_resolves_one_document_coordinate",
        &barrier,
        &[("WIRK_DOCTREE_RESOLVE_ESTATE", estate.as_path())],
    );
    let parked = arrival(
        &listener,
        "the resolve never reached the entry-classified window",
    );

    fs::remove_file(&target).unwrap();
    mkfifo(&target);
    drop(parked);

    let stdout = worker_output(child);
    // A disclosure, not a failure: the resolver reports the coordinate
    // unavailable rather than returning different bytes under it, and
    // reports it as an outcome rather than as a broken estate.
    assert!(
        stdout.contains("RESOLVE=ok Unavailable"),
        "a coordinate whose file is no longer an ordinary file resolves to a disclosed \
         unavailability, not to different bytes and not to a parked thread: {stdout}"
    );
    assert!(
        stdout.contains("no longer an ordinary file"),
        "and the disclosure names what actually happened at the window: {stdout}"
    );
}
