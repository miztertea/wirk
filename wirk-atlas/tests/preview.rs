//! `AtlasStore::preview` — `atlas acquire --dry-run`'s underlying
//! contract (P6.3-B): classifies inputs the same way a real acquisition
//! would, and leaves the catalog exactly as it found it. No membership,
//! no generation, no `atlas/` directory tree change of any kind.
//!
//! Mirrors `document_tree.rs`/`git_generations.rs`'s own fixture style:
//! `AtlasStore::open` a throwaway estate, act against a throwaway
//! source directory, assert on what came back and on what did not
//! change.

use std::fs;
use std::path::Path;
use tempfile::TempDir;
use wirk_atlas::{AcquireOutcome, AtlasStore, ExtractorPolicy};

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn atlas_generations_dir(estate: &Path) -> std::path::PathBuf {
    wirk_atlas::atlas_layout(estate).generations
}

/// **Meaningful red before this change**: `AtlasStore` had no `preview`
/// method at all — a caller who wanted to know what an acquisition of a
/// mixed document tree would find had no way to ask without actually
/// acquiring it. This test is the decisive green: a mixed tree previews
/// each input into the bucket a real acquisition's own admission check
/// would put it in, touching nothing on disk.
///
/// `photo.png` carries no extension the default (detecting) extractor
/// edition recognizes by name alone, so it is sniffed the same bounded
/// way `capture` always sniffs an unrecognized name
/// (`could_be_document`) — its content does not look like a supported
/// document format, so it lands `unsupported`, the same disposition a
/// real acquisition's `finish` would also give it. This is
/// `content_sniffed: true`'s whole point: unlike Git's preview, a
/// document-tree preview never leaves an entry `unclassified`, because
/// `capture`'s own walk already resolves every entry fully.
///
/// This test previously asserted `notes.txt` into that same bucket and
/// explained it as correct. It was not: the extension vocabulary simply
/// had no answer for `.txt`, and the assertion recorded the classifier's
/// answer rather than checking it. The current edition names `.txt` a
/// text family, so a plain `.txt` is a `candidate` here, beside the
/// `.md` — which is what a colleague dropping a glossary into a document
/// collection should see. The `photo.png` half is deliberately kept: the
/// widening must not have become a catch-all.
#[test]
fn document_tree_preview_classifies_without_writing_anything() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("readable.md"), "# Title\n\nBody.\n").unwrap();
    fs::write(source.path().join("notes.txt"), "plain notes\n").unwrap();
    fs::write(source.path().join("secret.pem"), "not a real key\n").unwrap();
    fs::write(source.path().join("photo.png"), [0x89u8, b'P', b'N', b'G']).unwrap();

    let estate = TempDir::new().unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    // Captured after `open` (which lays out an empty `atlas/` tree of
    // its own) so this measures what `preview` itself changes, not
    // what opening the store already created.
    let generations_before = fs::read_dir(atlas_generations_dir(estate.path()))
        .map(Iterator::count)
        .unwrap_or(0);

    let report = atlas
        .preview(
            "docs",
            &source.path().display().to_string(),
            wirk_atlas::DOCUMENT_TREE_CURRENT_OBSERVATION,
            Some("document-tree"),
            &ExtractorPolicy::default(),
        )
        .unwrap();

    assert_eq!(report.kind, "document-tree");
    assert!(report.content_sniffed, "the doctree walk always reads");
    // readable.md and notes.txt: recognized Knowledge-family
    // extensions, real acquisition would attempt extraction.
    assert_eq!(report.candidate.count, 2);
    // secret.pem: excluded by the estate's own fixed secret-like
    // policy, never opened.
    assert_eq!(report.excluded.count, 1);
    // photo.png: no extension family, and its sniffed content does not
    // look like a supported document.
    assert_eq!(report.unsupported.count, 1);
    assert_eq!(
        report.unclassified.count, 0,
        "capture's own walk always resolves an entry fully"
    );
    assert_eq!(report.total.count, 4);
    assert_eq!(
        report.revision,
        wirk_atlas::DOCUMENT_TREE_CURRENT_OBSERVATION
    );

    // Non-mutation: no membership registered, no `atlas/generations`
    // entry written.
    assert_eq!(atlas.memberships().count(), 0);
    let generations_after = fs::read_dir(atlas_generations_dir(estate.path()))
        .map(Iterator::count)
        .unwrap_or(0);
    assert_eq!(
        generations_before, generations_after,
        "preview staged no generation"
    );

    // The source's own files are untouched.
    assert_eq!(
        fs::read(source.path().join("readable.md")).unwrap(),
        b"# Title\n\nBody.\n"
    );
}

/// A malformed document (unreadable as the format its extension claims)
/// previews as `candidate`, same as a well-formed one: whether
/// extraction actually succeeds is not decided by this preview, only by
/// a real acquisition. Named explicitly so a future change cannot
/// silently start promising more than the preview can honestly know.
#[test]
fn a_malformed_document_previews_as_candidate_not_error() {
    let source = TempDir::new().unwrap();
    // Not a real DOCX: the extension is recognized, the bytes are not
    // a valid Office container. What a real acquisition's `finish`
    // would then record is decided by `finish`, which this preview
    // never calls and this test never runs — an actual run of this
    // fixture through acquisition reported no error at all, so the
    // assertion here is only that the preview does not pretend to
    // know.
    fs::write(source.path().join("broken.docx"), b"not a real docx").unwrap();

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let report = atlas
        .preview(
            "docs",
            &source.path().display().to_string(),
            wirk_atlas::DOCUMENT_TREE_CURRENT_OBSERVATION,
            Some("document-tree"),
            &ExtractorPolicy::default(),
        )
        .unwrap();
    assert_eq!(report.candidate.count, 1);
    assert_eq!(report.unsupported.count, 0);

    // Confirm the real acquisition this preview deliberately did not
    // run actually does disagree on disposition, so the distinction
    // above is proven, not asserted from documentation alone.
    let membership = atlas
        .register_document_tree("docs", source.path(), "current")
        .unwrap();
    let generation = match atlas
        .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(staged) => atlas
            .generation(&staged.id)
            .expect("the generation just staged reads back"),
        other => panic!("expected Staged, got {other:?}"),
    };
    let record = generation
        .resources
        .iter()
        .find(|record| record.path == b"broken.docx")
        .unwrap();
    assert_eq!(record.disposition, wirk_atlas::CoverageDisposition::Error);
}

/// A document-tree preview is refused, by name, for any `--revision`
/// other than the sentinel — the identical rule `acquire_document_tree`
/// already applies, so a caller cannot silently ask a preview to mean
/// something a real acquisition would refuse.
#[test]
fn document_tree_preview_refuses_a_non_sentinel_revision() {
    let source = TempDir::new().unwrap();
    fs::write(source.path().join("a.md"), "# a\n").unwrap();
    let estate = TempDir::new().unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let err = atlas
        .preview(
            "docs",
            &source.path().display().to_string(),
            "HEAD",
            Some("document-tree"),
            &ExtractorPolicy::default(),
        )
        .unwrap_err();
    assert!(matches!(err, wirk_atlas::AtlasError::InvalidRequest(_)));
}

/// `--kind http` is named and refused rather than silently attempted:
/// no preview walker exists for it in this increment.
#[test]
fn http_preview_is_refused_by_name() {
    let estate = TempDir::new().unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let err = atlas
        .preview(
            "api",
            "https://example.test/doc",
            "current",
            Some("http"),
            &ExtractorPolicy::default(),
        )
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("http"),
        "refusal should name the unsupported kind: {message}"
    );
}

/// Git's own half: classification from `git ls-tree -l` metadata alone,
/// no blob ever read, no membership registered, no generation staged —
/// and the counts match what a real `acquire` over the same commit
/// would classify by disposition (`candidate` covering `Indexed`, since
/// every file here is a real, extractable text file).
#[test]
fn git_preview_classifies_from_tree_metadata_without_reading_a_blob() {
    let source = TempDir::new().unwrap();
    git(source.path(), &["init", "-q"]);
    git(source.path(), &["config", "user.email", "t@example.test"]);
    git(source.path(), &["config", "user.name", "t"]);
    fs::write(source.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(source.path().join("notes.md"), "# notes\n").unwrap();
    git(source.path(), &["add", "."]);
    git(source.path(), &["commit", "-qm", "one"]);

    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let generations_before = fs::read_dir(atlas_generations_dir(estate.path()))
        .map(Iterator::count)
        .unwrap_or(0);
    let report = atlas
        .preview(
            "code",
            &source.path().display().to_string(),
            "HEAD",
            Some("git"),
            &ExtractorPolicy::default(),
        )
        .unwrap();
    assert_eq!(report.kind, "git");
    assert!(!report.content_sniffed, "git's preview reads no blob");
    assert_eq!(report.candidate.count, 2);
    assert_eq!(report.total.count, 2);
    assert_eq!(atlas.memberships().count(), 0);
    let generations_after = fs::read_dir(atlas_generations_dir(estate.path()))
        .map(Iterator::count)
        .unwrap_or(0);
    assert_eq!(
        generations_before, generations_after,
        "preview staged no generation"
    );

    // Confirm the real acquisition agrees: both preview `candidate`
    // as real `Indexed`, since both are well-formed text.
    let membership = atlas.register_git("code", source.path(), "HEAD").unwrap();
    let generation = match atlas
        .acquire(&membership, "HEAD", ExtractorPolicy::default())
        .unwrap()
    {
        AcquireOutcome::Staged(staged) => atlas
            .generation(&staged.id)
            .expect("the generation just staged reads back"),
        other => panic!("expected Staged, got {other:?}"),
    };
    assert_eq!(generation.resources.len(), 2);
    assert!(
        generation
            .resources
            .iter()
            .all(|record| record.disposition == wirk_atlas::CoverageDisposition::Indexed)
    );
}

/// Omitting `--kind` previews as `git`, the identical default `atlas
/// acquire` itself applies.
#[test]
fn omitted_kind_previews_as_git() {
    let source = TempDir::new().unwrap();
    git(source.path(), &["init", "-q"]);
    git(source.path(), &["config", "user.email", "t@example.test"]);
    git(source.path(), &["config", "user.name", "t"]);
    fs::write(source.path().join("a.rs"), "fn a() {}\n").unwrap();
    git(source.path(), &["add", "."]);
    git(source.path(), &["commit", "-qm", "one"]);

    let estate = TempDir::new().unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate-a").unwrap();
    let report = atlas
        .preview(
            "code",
            &source.path().display().to_string(),
            "HEAD",
            None,
            &ExtractorPolicy::default(),
        )
        .unwrap();
    assert_eq!(report.kind, "git");
}

/// **The preview's own cost disclosure, checked against the walk rather
/// than against its description of itself.**
///
/// `atlas acquire --dry-run`'s human text used to say this walk read
/// the full bytes of every candidate, excluded and unsupported input.
/// `doctree::recurse` does not: it excludes a secret-like path from the
/// name before any open, refuses an oversize file on its listed size
/// without an open, spends at most one open and a bounded leading
/// prefix on a name that settles nothing, and charges the aggregate
/// read budget only for an input it actually reads whole.
///
/// **Meaningful red**: every assertion below is one the superseded
/// prose predicts the other way. If excluded and unsupported inputs
/// really were opened and read, `secret.pem` and `oversize.md` — mode
/// 000 here, unreadable to this process — would come back
/// `unavailable` rather than `excluded`/`unsupported`, and `blob.bin`
/// at eight times `document_max_total_bytes` would refuse the whole
/// capture instead of previewing. Watched failing against those
/// expectations before the text was corrected.
#[test]
fn document_tree_preview_reads_only_what_classification_requires() {
    use std::os::unix::fs::PermissionsExt;

    let source = TempDir::new().unwrap();
    let unreadable = |name: &str, bytes: Vec<u8>| {
        let path = source.path().join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    };
    // Excluded from its name by the fixed secret-like policy, and
    // unreadable: reaching its bytes at all would report it unavailable.
    unreadable("secret.pem", b"not a real key\n".to_vec());
    // A recognized extension, but over `document_max_file_bytes`: the
    // size check precedes the open, so its mode is never consulted.
    unreadable("oversize.md", vec![b'a'; 5_000]);
    // No extension family and no document-looking prefix: one open and
    // a bounded sniff decide it, and its 2_048 bytes never reach the
    // 256-byte aggregate budget that a full read would charge.
    fs::write(source.path().join("blob.bin"), vec![0xFFu8; 2_048]).unwrap();
    fs::write(source.path().join("readable.md"), "# T\n").unwrap();

    let estate = TempDir::new().unwrap();
    fs::create_dir_all(estate.path().join(".wirk")).unwrap();
    fs::write(
        estate.path().join(".wirk").join("resources.json"),
        r#"{"document_max_file_bytes": 4096, "document_max_total_bytes": 256}"#,
    )
    .unwrap();
    let atlas = AtlasStore::open(estate.path(), "estate-cost").unwrap();

    let report = atlas
        .preview(
            "docs",
            &source.path().display().to_string(),
            wirk_atlas::DOCUMENT_TREE_CURRENT_OBSERVATION,
            Some("document-tree"),
            &ExtractorPolicy::default(),
        )
        .expect("a sniffed, never fully read input does not charge the aggregate budget");

    assert_eq!(
        report.unavailable.count, 0,
        "nothing was opened that classification did not require: both unreadable inputs were \
         settled from the name and the listed size alone"
    );
    assert_eq!(
        report.excluded.count, 1,
        "secret.pem is excluded before any open, not unavailable"
    );
    assert_eq!(
        report.unsupported.count, 2,
        "oversize.md is refused on its size without an open; blob.bin on a bounded prefix"
    );
    assert_eq!(report.candidate.count, 1, "only readable.md is read whole");
    assert_eq!(report.total.count, 4);
    // The byte columns are each input's listed size, not bytes read:
    // 5_000 unread bytes of oversize.md are still reported.
    assert_eq!(report.unsupported.bytes, 5_000 + 2_048);
}
