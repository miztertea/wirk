//! P5.2 (ruling 0293/0264, `knowledge/work/p5-document-formats/`): the
//! whole native `anydoc` 0.2.4 reader admitted through the real
//! document-tree product path — `AtlasStore::register_document_tree` /
//! `acquire_document_tree` / `publish` / `search` / `resolve_exact`, the
//! same calls `wirkd` makes, not a standalone converter demo.
//!
//! Fixtures: this crate's own committed `tests/fixtures/anydoc_corpus/`
//! (hand-authored, source-known, copied from the estate's read-only
//! reference checkout with its provenance recorded — see that
//! directory's own `MANIFEST.md`) plus a handful of small source-known
//! additions this file builds itself, for the two precision-gap
//! scenarios the corpus does not already cover: a non-A1 spreadsheet
//! origin and text repeated across two sheets.

use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use wirk_atlas::{
    AcquireOutcome, AtlasStore, ContentFamily, CoverageDisposition, ExtractorPolicy,
    PinnedProducer, QueryScope, SearchRequest, SemanticRequest, SourceGeneration, search,
};
use wirk_core::{Access, RepositoryBinding};

/// This crate's own committed fixture subset (`tests/fixtures/README.md`,
/// `tests/fixtures/anydoc_corpus/MANIFEST.md`): a normal test suite must
/// run from the product checkout and its declared dependencies alone, so
/// these bytes are copied in rather than located by walking up toward a
/// workspace-only reference checkout that a product-only checkout — CI's
/// own, or any other clone of this repository — does not have.
fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("anydoc_corpus")
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

fn resource<'a>(generation: &'a SourceGeneration, name: &str) -> &'a wirk_atlas::ResourceRecord {
    generation
        .resources
        .iter()
        .find(|r| r.path == name.as_bytes())
        .unwrap_or_else(|| panic!("no resource named {name:?} in {:?}", generation.resources))
}

fn request(scope: QueryScope, query: &str, families: Vec<ContentFamily>) -> SearchRequest {
    SearchRequest {
        scope,
        requested_source: None,
        query: query.into(),
        families,
        semantic: SemanticRequest::Disabled,
        limit: 10,
        capacity: None,
        pinned: None,
        offset: 0,
        semantic_query: None,
        pinned_editions: None,
        pinned_mode: None,
        pinned_producer: PinnedProducer::Unrecorded,
    }
}

/// Every fixture this test needs, real bytes and a source-known reason,
/// laid out flat in one document-tree collection: the corpus's own
/// Office/PDF fixtures under their original names, plus this file's own
/// small CSV/XLSX additions.
fn build_tree(dir: &Path) {
    let corpus = corpus_root();
    for (sub, name) in [
        ("docx_fixtures", "01-plain-headings-paragraphs.docx"),
        ("docx_fixtures", "03-table.docx"),
        ("docx_fixtures", "05-malformed-unclosed-element.docx"),
        ("docx_fixtures", "06-hostile-entry-expansion.docx"),
        ("office_fixtures", "18-xlsx-sheet.xlsx"),
        ("office_fixtures", "19-xlsx-malformed-unclosed-element.xlsx"),
        ("office_fixtures", "22-pdf-text.pdf"),
        ("office_fixtures", "23-pdf-scanned-needs-ocr.pdf"),
        // ruling 0313: the full native family beyond the original four
        // examples — RTF (plain, deep-nesting, and one genuinely RTF
        // file wearing a `.doc` extension, "common in the wild" per
        // this corpus's own MANIFEST.md), OpenDocument text/sheet/
        // presentation (plain, malformed, encrypted), PowerPoint
        // (plain, malformed, hostile zip-bomb-shaped) and EPUB (plain,
        // malformed).
        ("office_fixtures", "07-rtf-plain.rtf"),
        ("office_fixtures", "08-doc-rtf-in-disguise.doc"),
        ("office_fixtures", "09-rtf-deep-nesting.rtf"),
        ("office_fixtures", "10-odt-headings.odt"),
        ("office_fixtures", "11-ods-sheet.ods"),
        ("office_fixtures", "12-odp-slides.odp"),
        ("office_fixtures", "13-odt-malformed-unclosed-element.odt"),
        ("office_fixtures", "14-odt-encrypted.odt"),
        ("office_fixtures", "15-pptx-slides.pptx"),
        ("office_fixtures", "16-pptx-malformed-unclosed-element.pptx"),
        ("office_fixtures", "17-pptx-hostile-entry-expansion.pptx"),
        ("office_fixtures", "20-epub-chapters.epub"),
        ("office_fixtures", "21-epub-malformed-unclosed-element.epub"),
    ] {
        fs::copy(corpus.join(sub).join(name), dir.join(name))
            .unwrap_or_else(|e| panic!("copying {name}: {e}"));
    }

    fs::write(
        dir.join("sample.csv"),
        "name,quantity\nWidget,12\nGadget,7\n",
    )
    .unwrap();

    // Not `anydoc`-admitted at all: the pre-existing `v4` text
    // vocabulary this edition still carries forward unchanged. Exercised
    // here alongside the new document formats so the coverage matrix is
    // real and end-to-end, not only the newly added formats in
    // isolation.
    fs::write(
        dir.join("notes.md"),
        "# Colleague notes\n\nAlphamarker appears in real Markdown too.\n",
    )
    .unwrap();
    fs::write(
        dir.join("page.html"),
        "<html><body><p>Alphamarker appears in real HTML too.</p></body></html>\n",
    )
    .unwrap();

    // Deliberately unreadable-as-a-document: a `.pdf` extension over
    // plain text with no PDF header. anydoc detects format by content
    // first, extension only as fallback for signature-less formats
    // (`Format::from_bytes` then `Format::from_path`); this still names
    // `.pdf` explicitly (no signature it could fall back from), so the
    // parser genuinely runs and genuinely fails.
    fs::write(
        dir.join("garbage.pdf"),
        "not really a pdf, just text pretending to be one\n",
    )
    .unwrap();

    // Large: over a megabyte of real delimited text, which used to be
    // past the extractor's own rendered-text ceiling and is now simply
    // a large document. It is read, converted and indexed whole.
    let mut large = String::from("name,quantity\n");
    for i in 0..200_000u32 {
        large.push_str(&format!("Item{i},{}\n", i % 100));
    }
    assert!(large.len() > 1024 * 1024);
    fs::write(dir.join("large.csv"), large).unwrap();

    // Non-A1 origin + text repeated across sheets: real precision-gap
    // scenarios the corpus's existing `18-xlsx-sheet.xlsx` does not
    // exercise. Built with the `zip`/`quick-xml`-shaped OOXML any
    // spreadsheet tool produces; ground truth is only what this
    // function itself wrote, so the assertions below check exactly that
    // and nothing inferred from anydoc's own behavior.
    fs::write(dir.join("non_a1_repeated.xlsx"), build_xlsx()).unwrap();

    // The decisive hydration-keying fixture: byte-identical content
    // reachable under two paths that name two different derived
    // interpretations (one a document format, one plain text), so the
    // two resources share one content-addressed object id but must
    // never share one hydration cache entry. Two independent pairs,
    // named so the document-tree walk's own path sort
    // (`doctree.rs`'s `records.sort_by(|a, b| a.path.cmp(&b.path))`)
    // presents the pairs to `hydrate::blobs` in opposite orders: pair
    // one's `.csv` path sorts before its `.md` twin, pair two's `.md`
    // twin sorts before its `.csv` path. Content is a real small CSV
    // table each time, so the CSV twin renders a genuine Markdown table
    // and the Markdown twin passes the same bytes through unchanged —
    // two genuinely different strings from one raw blob, exactly the
    // shape the defect confused.
    fs::write(
        dir.join("alpha_twin.csv"),
        "site,phase,lead\nHarbour,Build,Nia\n",
    )
    .unwrap();
    fs::write(
        dir.join("alpha_twin.md"),
        "site,phase,lead\nHarbour,Build,Nia\n",
    )
    .unwrap();
    fs::write(dir.join("beta_1_twin.md"), "name,role\nAda,Lead\n").unwrap();
    fs::write(dir.join("beta_2_twin.csv"), "name,role\nAda,Lead\n").unwrap();

    // The raw-container-vs-rendered-text-budget fixture: a real DOCX
    // with one ordinary embedded image, deliberately padded so the
    // *original* file is comfortably over a megabyte in raw bytes while
    // its actual document text stays a few short paragraphs — the
    // modest-text, image-heavy shape completion guidance 0313 asks this
    // reader not to reject on container size alone. No built-in bound
    // refuses either half now; what this pins is that the two are still
    // told apart.
    let padded = build_docx_with_embedded_image();
    assert!(
        padded.len() > 1024 * 1024,
        "fixture must be genuinely large in raw container bytes: {}",
        padded.len()
    );
    fs::write(dir.join("image_heavy_modest_text.docx"), padded).unwrap();
}

/// A real, minimal DOCX — hand-built `[Content_Types].xml`/
/// `word/document.xml`/`word/_rels/document.xml.rels` plus one
/// `word/media/image1.png` part — with two short real paragraphs and
/// one embedded "image" (a large block of incompressible-looking bytes
/// standing in for a real photo's payload; `anydoc`'s docx parser reads
/// `word/document.xml` for text and does not need the image bytes to
/// be a real decodable PNG to extract that text, so this is a faithful
/// stand-in for "an Office file with an embedded image and modest
/// useful text", not a claim about image decoding). The image part is
/// stored, not deflated (`CompressionMethod::Stored`), so its raw size
/// on disk is exactly its byte count and cannot silently shrink the
/// fixture back under the bound this test needs it past.
fn build_docx_with_embedded_image() -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Default Extension=\"png\" ContentType=\"image/png\"/>\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        </Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
        </Relationships>";
    let document = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
        <w:body>\
        <w:p><w:r><w:t>Quarterly summary: modest real text.</w:t></w:r></w:p>\
        <w:p><w:r><w:t>One embedded photo accompanies this note.</w:t></w:r></w:p>\
        <w:p><w:r><w:drawing><w:inline><a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:blipFill><a:blip r:embed=\"rId1\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></w:inline></w:drawing></w:r></w:p>\
        </w:body></w:document>";
    let document_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/>\
        </Relationships>";
    // A real PNG signature/IHDR/IEND framing a large IDAT-shaped filler
    // so the part is unambiguously "an embedded image asset" by
    // structure, padded past the raw-bytes bound this fixture exists to
    // clear — not a claim that this exact byte stream decodes as a
    // displayable photo.
    let mut image = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
    ];
    image.extend_from_slice(b"IHDR-stand-in-not-a-real-header,");
    while image.len() < 1_400_000 {
        image.extend_from_slice(b"anydoc-embedded-image-payload-stand-in-bytes-");
    }
    image.extend_from_slice(b"IEND");

    let mut buffer = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let text_options = SimpleFileOptions::default();
        let image_options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, content) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("word/document.xml", document),
            ("word/_rels/document.xml.rels", document_rels),
        ] {
            zip.start_file(name, text_options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.start_file("word/media/image1.png", image_options)
            .unwrap();
        zip.write_all(&image).unwrap();
        zip.finish().unwrap();
    }
    buffer
}

/// A minimal, real XLSX: two sheets, both built with `[Content_Types].xml`
/// / `xl/workbook.xml` / `xl/worksheets/sheet{1,2}.xml` — not exported
/// from any spreadsheet application. Sheet1's data starts at `C5` (not
/// `A1`); `"Status: Active"` appears once on each sheet at a different
/// cell, source-known ground truth for "repeated text across sheets".
fn build_xlsx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn sheet_xml(rows: &[(&str, Vec<(&str, &str)>)]) -> String {
        let mut body = String::new();
        for (row_index, cells) in rows {
            body.push_str(&format!("<row r=\"{row_index}\">"));
            for (cell_ref, text) in cells {
                body.push_str(&format!(
                    "<c r=\"{cell_ref}\" t=\"inlineStr\"><is><t>{text}</t></is></c>"
                ));
            }
            body.push_str("</row>");
        }
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
             <sheetData>{body}</sheetData></worksheet>"
        )
    }

    let sheet1 = sheet_xml(&[
        ("5", vec![("C5", "Region"), ("D5", "Total")]),
        ("6", vec![("C6", "North"), ("D6", "12")]),
        ("7", vec![("C7", "South"), ("D7", "7")]),
        ("8", vec![("C8", "Status: Active")]),
    ]);
    let sheet2 = sheet_xml(&[
        ("2", vec![("B2", "Region"), ("C2", "Total")]),
        ("3", vec![("B3", "East"), ("C3", "5")]),
        ("4", vec![("B4", "Status: Active")]),
    ]);

    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
        <Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>\
        <Override PartName=\"/xl/worksheets/sheet2.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>\
        </Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
        </Relationships>";
    let workbook = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
        <sheets>\
        <sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/>\
        <sheet name=\"Sheet2\" sheetId=\"2\" r:id=\"rId2\"/>\
        </sheets></workbook>";
    let workbook_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
        <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet2.xml\"/>\
        </Relationships>";

    let mut buffer = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options = SimpleFileOptions::default();
        for (name, content) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
            ("xl/worksheets/sheet1.xml", sheet1.as_str()),
            ("xl/worksheets/sheet2.xml", sheet2.as_str()),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer
}

fn open_and_acquire(
    dir: &Path,
) -> (
    TempDir,
    AtlasStore,
    wirk_atlas::Membership,
    SourceGeneration,
) {
    let estate = TempDir::new().unwrap();
    let mut atlas = AtlasStore::open(estate.path(), "estate").unwrap();
    let membership = atlas
        .register_document_tree("docs", dir, "current")
        .unwrap();
    let generation = {
        let outcome = atlas
            .acquire_document_tree(&membership, "current", ExtractorPolicy::default())
            .unwrap();
        read_staged(&atlas, outcome)
    };
    (estate, atlas, membership, generation)
}

/// The real coverage matrix: every admitted format lands `Indexed` with
/// real content, every genuinely bad input lands `Error` (visibly, by
/// name), never a silent empty success.
#[test]
fn the_admitted_matrix_indexes_real_content_and_names_every_real_failure() {
    let tree = TempDir::new().unwrap();
    build_tree(tree.path());
    let (_estate, _atlas, _membership, generation) = open_and_acquire(tree.path());

    // -- DOCX: plain preamble/headings, and a real table. --
    let headings = resource(&generation, "01-plain-headings-paragraphs.docx");
    assert_eq!(headings.disposition, CoverageDisposition::Indexed);
    assert!(!headings.units.is_empty());

    let table = resource(&generation, "03-table.docx");
    assert_eq!(table.disposition, CoverageDisposition::Indexed);
    assert!(!table.units.is_empty());

    // -- DOCX: a genuine zip-bomb shape is refused, not expanded. --
    let hostile_docx = resource(&generation, "06-hostile-entry-expansion.docx");
    assert_eq!(
        hostile_docx.disposition,
        CoverageDisposition::Error,
        "a zip-bomb-shaped docx must be refused, not expanded: {:?}",
        hostile_docx.detail
    );
    assert!(
        hostile_docx
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("resourceLimit")),
        "{:?}",
        hostile_docx.detail
    );

    // -- A real, disclosed candidate limitation, not asserted away: the
    // -- corpus's `05-malformed-unclosed-element.docx`/
    // -- `19-xlsx-malformed-unclosed-element.xlsx` were hand-built to
    // -- make the *predecessor's own* strict OOXML reader refuse them.
    // -- `anydoc`'s quick-xml-based parsing is lenient about an
    // -- unclosed element and actually recovers a (wrong-shaped, but
    // -- non-empty) result instead of erroring — confirmed by direct
    // -- `anydoc::to_markdown` probing before this assertion was
    // -- written, not assumed from the corpus's own intent for a
    // -- different parser. This is the honest reuse-fitness finding
    // -- P5.2 asked for, not a bug in this admission path: whatever
    // -- `anydoc` returns is what gets indexed, truthfully, either way.
    let leniently_recovered_docx = resource(&generation, "05-malformed-unclosed-element.docx");
    assert_eq!(
        leniently_recovered_docx.disposition,
        CoverageDisposition::Indexed
    );
    let leniently_recovered_xlsx = resource(&generation, "19-xlsx-malformed-unclosed-element.xlsx");
    assert_eq!(
        leniently_recovered_xlsx.disposition,
        CoverageDisposition::Indexed
    );

    // -- XLSX: a plain sheet indexes. --
    let xlsx = resource(&generation, "18-xlsx-sheet.xlsx");
    assert_eq!(xlsx.disposition, CoverageDisposition::Indexed);

    // -- PDF: real text indexes; a scanned PDF is a visible, named
    // -- refusal (NeedsOcr), never an empty success. --
    let pdf_text = resource(&generation, "22-pdf-text.pdf");
    assert_eq!(pdf_text.disposition, CoverageDisposition::Indexed);
    let pdf_scanned = resource(&generation, "23-pdf-scanned-needs-ocr.pdf");
    assert_eq!(pdf_scanned.disposition, CoverageDisposition::Error);
    assert!(
        pdf_scanned
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("needsOcr")),
        "a scanned PDF must name OCR as the reason, not fail silently: {:?}",
        pdf_scanned.detail
    );

    // -- A file wearing a document extension over content that is not
    // -- that format at all: a real, named conversion failure. --
    let garbage = resource(&generation, "garbage.pdf");
    assert_eq!(garbage.disposition, CoverageDisposition::Error);
    assert!(
        garbage
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("document conversion failed"))
    );

    // -- CSV: a small real table indexes. --
    let csv = resource(&generation, "sample.csv");
    assert_eq!(csv.disposition, CoverageDisposition::Indexed);

    // -- Markdown/HTML: the pre-existing text vocabulary, unaffected by
    // -- document admission, in the same real collection. --
    let notes = resource(&generation, "notes.md");
    assert_eq!(notes.disposition, CoverageDisposition::Indexed);
    assert_eq!(
        notes.units.first().map(|unit| unit.family),
        Some(ContentFamily::Knowledge)
    );
    let page = resource(&generation, "page.html");
    assert_eq!(page.disposition, CoverageDisposition::Indexed);
    assert_eq!(
        page.units.first().map(|unit| unit.family),
        Some(ContentFamily::Knowledge)
    );

    // -- Large: indexed, whole, like any other admitted document.
    // -- Rulings 0402/0403 removed the rendered-Markdown ceiling this
    // -- case used to be refused by: it was a product-chosen size
    // -- policy, and refusing a perfectly convertible table for being
    // -- long produced an `Error` with nothing retrievable behind it.
    // -- `large_content_indexing.rs` holds the searchable/resolvable
    // -- half of the same requirement; here the point is that the
    // -- matrix's own large member is a success, not a named failure.
    let large = resource(&generation, "large.csv");
    assert_eq!(
        large.disposition,
        CoverageDisposition::Indexed,
        "a large convertible table is indexed, not refused: {:?}",
        large.detail
    );
    let large_markdown_len: u64 = large
        .units
        .iter()
        .map(|unit| unit.byte_end)
        .max()
        .unwrap_or_default();
    assert!(
        large_markdown_len > 1024 * 1024,
        "the converted Markdown actually indexed runs past the former ceiling:          {large_markdown_len}"
    );

    // -- The distinguishing case completion guidance 0313 named: raw
    // -- container bytes comfortably over a megabyte (an embedded image
    // -- inflates the file), but the actual document text is a few short
    // -- paragraphs. Its units index the converted text, so the reader
    // -- must not be describing the container's size as its content.
    let image_heavy = resource(&generation, "image_heavy_modest_text.docx");
    assert_eq!(
        image_heavy.disposition,
        CoverageDisposition::Indexed,
        "an image-heavy but text-light document must not be refused on raw container size \
         alone: {:?}",
        image_heavy.detail
    );
    assert!(
        image_heavy.byte_len.is_some_and(|len| len > 1024 * 1024),
        "the fixture's own original size must genuinely be large for this case to mean \
         anything: {:?}",
        image_heavy.byte_len
    );
    let image_heavy_markdown_len: u64 = image_heavy
        .units
        .iter()
        .map(|unit| unit.byte_end)
        .max()
        .unwrap_or_default();
    assert!(
        image_heavy_markdown_len < 4096,
        "the converted Markdown must reflect the real modest text, not the padded container: {}",
        image_heavy_markdown_len
    );

    // -- RTF: plain paragraphs and a deliberately deep-nested-groups
    // -- adversarial case, both a real, disclosed parse, no crash. --
    let rtf_plain = resource(&generation, "07-rtf-plain.rtf");
    assert_eq!(rtf_plain.disposition, CoverageDisposition::Indexed);
    let rtf_nested = resource(&generation, "09-rtf-deep-nesting.rtf");
    assert_eq!(
        rtf_nested.disposition,
        CoverageDisposition::Indexed,
        "400 nested RTF groups is adversarial, not malformed, and anydoc parses it clean: {:?}",
        rtf_nested.detail
    );

    // -- The real "RTF wearing a .doc extension" case this corpus's own
    // -- MANIFEST.md names as common in the wild: content detection
    // -- (`Format::from_bytes`) recognizes the actual RTF signature
    // -- ahead of the `.doc` extension's own `Format::Doc`, so this
    // -- converts as RTF and recovers the identical real text `07`
    // -- does — extension-mislabeling handled by the reader itself, not
    // -- a silent failure or a wrong-format garble. --
    let doc_disguise = resource(&generation, "08-doc-rtf-in-disguise.doc");
    assert_eq!(
        doc_disguise.disposition,
        CoverageDisposition::Indexed,
        "content detection must recover the real RTF under a misleading .doc extension: {:?}",
        doc_disguise.detail
    );

    // -- OpenDocument text/sheet/presentation: plain, malformed
    // -- (anydoc's same lenient-recovery disclosed limitation as
    // -- DOCX/XLSX), and encrypted (a distinct, honestly named gap). --
    let odt = resource(&generation, "10-odt-headings.odt");
    assert_eq!(odt.disposition, CoverageDisposition::Indexed);
    let ods = resource(&generation, "11-ods-sheet.ods");
    assert_eq!(ods.disposition, CoverageDisposition::Indexed);
    let odp = resource(&generation, "12-odp-slides.odp");
    assert_eq!(odp.disposition, CoverageDisposition::Indexed);
    let odt_encrypted = resource(&generation, "14-odt-encrypted.odt");
    assert_eq!(odt_encrypted.disposition, CoverageDisposition::Error);
    assert!(
        odt_encrypted
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("encrypted")),
        "an encrypted ODT must be named as encrypted, not conflated with a parse failure: {:?}",
        odt_encrypted.detail
    );

    // -- PowerPoint: plain slides index; a genuine zip-bomb shape is
    // -- refused exactly like the DOCX case. --
    let pptx = resource(&generation, "15-pptx-slides.pptx");
    assert_eq!(pptx.disposition, CoverageDisposition::Indexed);
    let pptx_hostile = resource(&generation, "17-pptx-hostile-entry-expansion.pptx");
    assert_eq!(
        pptx_hostile.disposition,
        CoverageDisposition::Error,
        "a zip-bomb-shaped pptx must be refused, not expanded: {:?}",
        pptx_hostile.detail
    );
    assert!(
        pptx_hostile
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("resourceLimit")),
        "{:?}",
        pptx_hostile.detail
    );

    // -- EPUB: real spine chapters index. --
    let epub = resource(&generation, "20-epub-chapters.epub");
    assert_eq!(epub.disposition, CoverageDisposition::Indexed);

    // -- Non-A1 origin / repeated-across-sheets XLSX indexes too. --
    let sheets = resource(&generation, "non_a1_repeated.xlsx");
    assert_eq!(sheets.disposition, CoverageDisposition::Indexed);
}

/// Real search over the admitted collection: the actual product path
/// (`wirk_atlas::search`), not a direct unit inspection. Content from a
/// DOCX table and a PDF is genuinely retrievable; a scanned/malformed
/// input contributes nothing to find and disclosed as incomplete
/// coverage, never a silent gap.
#[test]
fn search_actually_finds_content_admitted_from_docx_pdf_csv_and_xlsx() {
    let tree = TempDir::new().unwrap();
    build_tree(tree.path());
    let (_estate, mut atlas, membership, generation) = open_and_acquire(tree.path());
    atlas.publish(&membership, &generation.id).unwrap();

    let scope = QueryScope::Work(vec![RepositoryBinding {
        name: "docs".into(),
        access: Access::Read,
    }]);

    let widget = search(&atlas, &request(scope.clone(), "Widget", vec![])).unwrap();
    assert!(
        widget
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"03-table.docx"),
        "the DOCX table's real cell text must be searchable: {:?}",
        widget
            .hits
            .iter()
            .map(|h| String::from_utf8_lossy(&h.coordinate.path).to_string())
            .collect::<Vec<_>>()
    );

    let quantity = search(&atlas, &request(scope.clone(), "Gadget", vec![])).unwrap();
    assert!(
        quantity
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"sample.csv"),
        "the CSV's real row text must be searchable"
    );

    let alphamarker = search(&atlas, &request(scope.clone(), "Alphamarker", vec![])).unwrap();
    let alphamarker_paths: Vec<_> = alphamarker
        .hits
        .iter()
        .map(|hit| hit.coordinate.path.clone())
        .collect();
    assert!(
        alphamarker_paths.contains(&b"notes.md".to_vec()),
        "real Markdown content must remain searchable alongside admitted documents"
    );
    assert!(
        alphamarker_paths.contains(&b"page.html".to_vec()),
        "real HTML content must remain searchable alongside admitted documents"
    );

    let region = search(&atlas, &request(scope.clone(), "Region", vec![])).unwrap();
    let sheet_hits: Vec<_> = region
        .hits
        .iter()
        .filter(|hit| hit.coordinate.path == b"non_a1_repeated.xlsx")
        .collect();
    assert!(
        !sheet_hits.is_empty(),
        "a non-A1-origin cell's text must still be searchable"
    );

    let status = search(&atlas, &request(scope.clone(), "Status", vec![])).unwrap();
    assert!(
        status
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"non_a1_repeated.xlsx"),
        "text repeated across two sheets must still surface at least once"
    );

    // -- The real "RTF wearing a .doc extension" content-detection case:
    // -- both the plain `.rtf` and the mislabeled `.doc` recover the
    // -- identical real paragraph text through search. --
    let rtf_text = search(
        &atlas,
        &request(scope.clone(), "First rtf paragraph", vec![]),
    )
    .unwrap();
    let rtf_paths: Vec<_> = rtf_text
        .hits
        .iter()
        .map(|hit| hit.coordinate.path.clone())
        .collect();
    assert!(
        rtf_paths.contains(&b"07-rtf-plain.rtf".to_vec()),
        "{rtf_paths:?}"
    );
    assert!(
        rtf_paths.contains(&b"08-doc-rtf-in-disguise.doc".to_vec()),
        "content detection must recover the mislabeled .doc's real RTF text too: {rtf_paths:?}"
    );

    // -- OpenDocument/PowerPoint/EPUB real content, through the same
    // -- real search path. --
    let odt_hit = search(&atlas, &request(scope.clone(), "Odt Introduction", vec![])).unwrap();
    assert!(
        odt_hit
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"10-odt-headings.odt"),
        "{:?}",
        odt_hit.hits
    );
    let ods_hit = search(&atlas, &request(scope.clone(), "Gadget", vec![])).unwrap();
    assert!(
        ods_hit
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"11-ods-sheet.ods"),
        "{:?}",
        ods_hit.hits
    );
    let odp_hit = search(&atlas, &request(scope.clone(), "Odp Slide One", vec![])).unwrap();
    assert!(
        odp_hit
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"12-odp-slides.odp"),
        "{:?}",
        odp_hit.hits
    );
    let pptx_hit = search(&atlas, &request(scope.clone(), "Pptx Slide One", vec![])).unwrap();
    assert!(
        pptx_hit
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"15-pptx-slides.pptx"),
        "{:?}",
        pptx_hit.hits
    );
    let epub_hit = search(&atlas, &request(scope.clone(), "Epub Chapter One", vec![])).unwrap();
    assert!(
        epub_hit
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"20-epub-chapters.epub"),
        "{:?}",
        epub_hit.hits
    );

    let coverage = search(&atlas, &request(scope, "alphamarker-never-present", vec![])).unwrap();
    assert!(
        coverage.coverage.source_extraction_incomplete,
        "this collection genuinely has extraction errors (malformed/hostile/scanned/oversize \
         inputs); a search over it must disclose that, never claim complete coverage"
    );
}

/// The decisive fix: `hydrate::blobs` used to key its batched cache by
/// object id alone, so two resources that dedupe to one raw byte
/// string under different derived interpretations (a document path and
/// a plain-text path) collapsed onto one cache entry — whichever path
/// happened to be inserted first won, and the other path's every
/// caller (lexical search here) silently read that wrong string
/// instead. This test is the red/green control: run against the
/// pre-fix `hydrate::render_documents` (keyed by `object_id` alone,
/// `path_by_object.entry(...).or_insert(path)` picking one path per
/// object id), `alpha_twin.md`'s snippet came back as a Markdown table
/// (`alpha_twin.csv`'s own rendering) and `beta_2_twin.csv`'s came back
/// as raw, unconverted CSV text (`beta_1_twin.md`'s own rendering) —
/// watched fail before this fix, per this estate's posture on what
/// counts as a test.
///
/// Two independent pairs, built so the document-tree walk's own
/// path-sorted resource order (`doctree.rs`) presents them to
/// `hydrate::blobs` in opposite orders — `alpha_twin.csv` sorts before
/// `alpha_twin.md`, `beta_1_twin.md` sorts before `beta_2_twin.csv` —
/// so the fix is proven regardless of which interpretation a naive
/// "first one wins" cache would have picked.
#[test]
fn hydration_keys_by_derived_interpretation_not_object_id_alone() {
    let tree = TempDir::new().unwrap();
    build_tree(tree.path());
    let (_estate, mut atlas, membership, generation) = open_and_acquire(tree.path());
    atlas.publish(&membership, &generation.id).unwrap();

    // Ground truth: the two pairs really do dedupe to one object id
    // each, or this test would not be exercising the defect at all.
    let alpha_csv = resource(&generation, "alpha_twin.csv");
    let alpha_md = resource(&generation, "alpha_twin.md");
    assert_eq!(
        alpha_csv.object_id, alpha_md.object_id,
        "not a real dedup case"
    );
    let beta_md = resource(&generation, "beta_1_twin.md");
    let beta_csv = resource(&generation, "beta_2_twin.csv");
    assert_eq!(
        beta_md.object_id, beta_csv.object_id,
        "not a real dedup case"
    );

    let scope = QueryScope::Work(vec![RepositoryBinding {
        name: "docs".into(),
        access: Access::Read,
    }]);

    // Unfiltered search: the CSV twin's cell text must come back as a
    // real Markdown table cell (rendered), the Markdown twin's as the
    // literal raw line (passed through) — each resource's own
    // rendering, not the other's.
    let harbour = search(&atlas, &request(scope.clone(), "Harbour", vec![])).unwrap();
    let by_path = |path: &[u8]| {
        harbour
            .hits
            .iter()
            .find(|hit| hit.coordinate.path == path)
            .unwrap_or_else(|| panic!("no hit for {:?} among {:?}", path, harbour.hits))
    };
    assert!(
        by_path(b"alpha_twin.csv").snippet.contains('|'),
        "the CSV twin must render as a real Markdown table: {:?}",
        by_path(b"alpha_twin.csv").snippet
    );
    assert!(
        !by_path(b"alpha_twin.md").snippet.contains('|'),
        "the Markdown twin must keep its own raw, unconverted text, not the CSV twin's table \
         rendering: {:?}",
        by_path(b"alpha_twin.md").snippet
    );

    let ada = search(&atlas, &request(scope.clone(), "Ada", vec![])).unwrap();
    let by_path_ada = |path: &[u8]| {
        ada.hits
            .iter()
            .find(|hit| hit.coordinate.path == path)
            .unwrap_or_else(|| panic!("no hit for {:?} among {:?}", path, ada.hits))
    };
    assert!(
        by_path_ada(b"beta_2_twin.csv").snippet.contains('|'),
        "{:?}",
        by_path_ada(b"beta_2_twin.csv").snippet
    );
    assert!(
        !by_path_ada(b"beta_1_twin.md").snippet.contains('|'),
        "{:?}",
        by_path_ada(b"beta_1_twin.md").snippet
    );

    // Family-filtered queries must isolate each twin correctly too —
    // narrowing the query must never be the only way to get a correct
    // answer, but it must still agree with the unfiltered one.
    let document_only = search(
        &atlas,
        &request(scope.clone(), "Harbour", vec![ContentFamily::Document]),
    )
    .unwrap();
    assert!(
        document_only
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"alpha_twin.csv"),
        "{:?}",
        document_only.hits
    );
    assert!(
        !document_only
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"alpha_twin.md"),
        "the knowledge-family twin must not appear under a document-only filter: {:?}",
        document_only.hits
    );
    let knowledge_only = search(
        &atlas,
        &request(scope, "Harbour", vec![ContentFamily::Knowledge]),
    )
    .unwrap();
    assert!(
        knowledge_only
            .hits
            .iter()
            .any(|hit| hit.coordinate.path == b"alpha_twin.md"),
        "{:?}",
        knowledge_only.hits
    );

    // `resolve` must agree with what `search` found for each twin,
    // independently.
    let resolve_one = |resource: &wirk_atlas::ResourceRecord| {
        let unit = resource.units.first().expect("at least one unit");
        let coordinate = wirk_atlas::ExactCoordinate {
            estate: membership.estate.clone(),
            membership: membership.id.clone(),
            source: membership.source.clone(),
            generation: generation.id.clone(),
            path: resource.path.clone(),
            object_id: resource.object_id.clone().unwrap(),
            byte_start: unit.byte_start,
            byte_end: unit.byte_end,
            line_start: unit.line_start,
            line_end: unit.line_end,
        };
        match atlas.resolve_exact(&membership, &coordinate).unwrap() {
            wirk_atlas::ResolveOutcome::Resolved(evidence) => {
                String::from_utf8(evidence.bytes).unwrap()
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    };
    assert!(
        resolve_one(alpha_csv).contains('|'),
        "resolve on the CSV twin must agree with search: a real Markdown table"
    );
    assert!(
        !resolve_one(alpha_md).contains('|'),
        "resolve on the Markdown twin must agree with search: its own raw text"
    );
}

/// `resolve_exact` on a `Document` unit returns the *converted Markdown*
/// text the unit's offsets actually index — not the original binary —
/// and that text contains the document's real content. This is the
/// concrete proof for `ContentFamily::Document`'s own claim: resolving
/// through `crate::document::render_if_document` (via
/// `AtlasStore::resolve_exact_doctree`) rather than slicing the
/// original `.docx` bytes.
#[test]
fn resolve_exact_on_a_document_unit_returns_real_converted_text_not_the_original_binary() {
    let tree = TempDir::new().unwrap();
    build_tree(tree.path());
    let (_estate, atlas, membership, generation) = open_and_acquire(tree.path());

    let record = resource(&generation, "03-table.docx");
    assert_eq!(record.disposition, CoverageDisposition::Indexed);
    let mut found_widget = false;
    for unit in &record.units {
        let coordinate = wirk_atlas::ExactCoordinate {
            estate: membership.estate.clone(),
            membership: membership.id.clone(),
            source: membership.source.clone(),
            generation: generation.id.clone(),
            path: record.path.clone(),
            object_id: record.object_id.clone().unwrap(),
            byte_start: unit.byte_start,
            byte_end: unit.byte_end,
            line_start: unit.line_start,
            line_end: unit.line_end,
        };
        match atlas.resolve_exact(&membership, &coordinate).unwrap() {
            wirk_atlas::ResolveOutcome::Resolved(evidence) => {
                let text = String::from_utf8(evidence.bytes)
                    .expect("a Document unit's resolved bytes must be the UTF-8 Markdown they were unitized from");
                if text.contains("Widget") {
                    found_widget = true;
                }
                // The original `.docx` is a zip archive; nothing zip-shaped
                // (a local-file-header signature) should ever appear in
                // resolved Document text, because resolution renders
                // through anydoc rather than slicing the raw file.
                assert!(
                    !text.as_bytes().starts_with(b"PK"),
                    "resolved Document bytes must be Markdown, not the raw docx archive"
                );
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }
    assert!(
        found_widget,
        "resolving every unit of the table docx should recover its real cell text somewhere"
    );
}

// ---------------------------------------------------------------------
// Detected admission, and the structured reader.
//
// Everything below runs through the same real product path the tests
// above do — `register_document_tree`/`acquire_document_tree`/`publish`/
// `search`/`resolve_exact`, plus `document_reading`/`document_asset` —
// never a standalone converter call.
// ---------------------------------------------------------------------

/// This crate's own committed fixtures: the one real legacy OLE workbook
/// and the one real PNG, both described in `tests/fixtures/README.md`.
fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// A real, minimal DOCX carrying two short paragraphs and one genuinely
/// valid embedded PNG — the shape of an ordinary working document with a
/// diagram in it. The image part is this crate's own
/// `embedded-diagram.png` fixture, byte for byte, so a test can compare
/// what comes back out of the package against what went in.
fn build_docx_with_real_diagram() -> (Vec<u8>, Vec<u8>) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let image = fixture("embedded-diagram.png");
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Default Extension=\"png\" ContentType=\"image/png\"/>\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        </Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
        </Relationships>";
    let document = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
        <w:body>\
        <w:p><w:pPr><w:outlineLvl w:val=\"0\"/></w:pPr><w:r><w:t>Harbour RFP</w:t></w:r></w:p>\
        <w:p><w:r><w:t>Rfpmarker: the notes accompanying the site diagram.</w:t></w:r></w:p>\
        <w:p><w:r><w:drawing><w:inline><a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:blipFill><a:blip r:embed=\"rId1\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></w:inline></w:drawing></w:r></w:p>\
        </w:body></w:document>";
    let document_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/>\
        </Relationships>";

    let mut buffer = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options = SimpleFileOptions::default();
        for (name, content) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("word/document.xml", document),
            ("word/_rels/document.xml.rels", document_rels),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.start_file("word/media/image1.png", options).unwrap();
        zip.write_all(&image).unwrap();
        zip.finish().unwrap();
    }
    (buffer, image)
}

/// The collection the detection tests walk: real document bytes filed
/// under names that settle nothing, names that lie, and names the text
/// vocabulary already owns.
fn build_detection_tree(dir: &Path) -> Vec<u8> {
    let corpus = corpus_root();
    let odt = fs::read(corpus.join("office_fixtures").join("10-odt-headings.odt")).unwrap();
    let rtf = fs::read(corpus.join("office_fixtures").join("07-rtf-plain.rtf")).unwrap();

    // No extension at all, and real ODT bytes: nothing about the name
    // says "document", and `anydoc`'s own detector says it is one.
    fs::write(dir.join("harbour-rfp"), &odt).unwrap();
    // An extension outside both vocabularies over real RTF bytes.
    fs::write(dir.join("archive.payload"), &rtf).unwrap();
    // A real legacy OLE workbook, once under its own name and once under
    // no extension at all: the same bytes admitted by name and by
    // detection, through the OLE parser either way.
    let xls = fixture("legacy-excel-97.xls");
    fs::write(dir.join("ledger.xls"), &xls).unwrap();
    fs::write(dir.join("ledger-no-extension"), &xls).unwrap();
    // The text vocabulary still wins over content: real RTF bytes under a
    // `.md` name stay Markdown, which is what keeps ordinary Markdown and
    // code interpretation intact.
    fs::write(dir.join("disguised.md"), &rtf).unwrap();
    // Unrecognized name, ordinary prose: screened out on its prefix and
    // never read in full.
    fs::write(dir.join("plainfile"), b"Just prose under a bare name.\n").unwrap();
    // Unrecognized name, real binary that is no document at all.
    fs::write(
        dir.join("blob.payload"),
        b"\x7fELF\x02\x01\x01\x00\x00binary\x00",
    )
    .unwrap();

    let (docx, image) = build_docx_with_real_diagram();
    fs::write(dir.join("rfp-with-diagram.docx"), &docx).unwrap();
    image
}

/// A name that settles nothing is no longer the same answer as an
/// excluded path: content decides, inside the bounds the walk already
/// had, and a name the text vocabulary owns still wins over content.
#[test]
fn detection_admits_extensionless_and_mislabeled_documents_without_widening_text() {
    let tree = TempDir::new().unwrap();
    build_detection_tree(tree.path());
    let (_estate, _atlas, _membership, generation) = open_and_acquire(tree.path());

    for (name, expected_family) in [
        ("harbour-rfp", ContentFamily::Document),
        ("archive.payload", ContentFamily::Document),
        ("ledger.xls", ContentFamily::Document),
        ("ledger-no-extension", ContentFamily::Document),
        ("rfp-with-diagram.docx", ContentFamily::Document),
        // Name over content, deliberately.
        ("disguised.md", ContentFamily::Knowledge),
    ] {
        let record = resource(&generation, name);
        assert_eq!(
            record.disposition,
            CoverageDisposition::Indexed,
            "{name}: {:?}",
            record.detail
        );
        assert_eq!(
            record.units.first().map(|unit| unit.family),
            Some(expected_family),
            "{name}"
        );
    }

    // Screened out on a bounded prefix: an unrecognized name over content
    // no container signature can start.
    for name in ["plainfile", "blob.payload"] {
        let record = resource(&generation, name);
        assert_eq!(
            record.disposition,
            CoverageDisposition::Unsupported,
            "{name}: {:?}",
            record.detail
        );
        assert_eq!(
            record.detail.as_deref(),
            Some("no extractor for path family"),
            "{name}"
        );
    }
}

/// A real legacy binary workbook — OLE compound file, `Workbook` stream,
/// BIFF records — reaches search under its own name and under no name at
/// all, and the text that comes back is the workbook's own.
#[test]
fn a_real_legacy_ole_workbook_is_read_by_name_and_by_detection() {
    let tree = TempDir::new().unwrap();
    build_detection_tree(tree.path());
    let (_estate, mut atlas, membership, generation) = open_and_acquire(tree.path());
    atlas.publish(&membership, &generation.id).unwrap();

    let scope = QueryScope::Work(vec![RepositoryBinding {
        name: "docs".into(),
        access: Access::Read,
    }]);
    let answer = search(
        &atlas,
        &request(scope, "Legacymarker", vec![ContentFamily::Document]),
    )
    .unwrap();
    let paths: Vec<String> = answer
        .hits
        .iter()
        .map(|hit| String::from_utf8_lossy(&hit.coordinate.path).into_owned())
        .collect();
    assert!(
        paths.iter().any(|path| path == "ledger.xls"),
        "legacy workbook by name: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path == "ledger-no-extension"),
        "the same workbook admitted by detection alone: {paths:?}"
    );
}

/// The reader capability the Markdown serializer cannot offer: the
/// document's own structure, an inventory of what it embeds, and the
/// bytes of one embedded asset on demand — addressed by the resource
/// identity a search hit already carries.
#[test]
fn the_structured_reader_describes_a_document_and_hands_back_its_real_embedded_image() {
    let tree = TempDir::new().unwrap();
    let image = build_detection_tree(tree.path());
    let (_estate, atlas, membership, generation) = open_and_acquire(tree.path());

    let record = resource(&generation, "rfp-with-diagram.docx");
    let coordinate = wirk_atlas::ExactCoordinate {
        estate: membership.estate.clone(),
        membership: membership.id.clone(),
        source: membership.source.clone(),
        generation: generation.id.clone(),
        path: record.path.clone(),
        object_id: record.object_id.clone().unwrap(),
        byte_start: 0,
        byte_end: 0,
        line_start: 1,
        line_end: 1,
    };

    let reading = atlas.document_reading(&membership, &coordinate).unwrap();
    let wirk_atlas::DocumentReading::Read(outline) = reading else {
        panic!("expected a readable document, got {reading:?}");
    };
    assert_eq!(outline.format, "Docx");
    assert!(
        outline
            .headings
            .iter()
            .any(|heading| heading.text == "Harbour RFP"),
        "{:?}",
        outline.headings
    );
    assert_eq!(outline.assets.len(), 1, "{:?}", outline.assets);
    let descriptor = &outline.assets[0];
    assert_eq!(descriptor.media_type, "image/png");
    assert_eq!(descriptor.byte_len, image.len() as u64);

    // The bytes, on demand and only on demand — and exactly the ones that
    // went into the package.
    let asset = atlas
        .document_asset(&membership, &coordinate, descriptor.id, Some(1024 * 1024))
        .unwrap()
        .expect("the inventory listed this asset");
    assert_eq!(asset.bytes, image);
    assert_eq!(asset.descriptor.digest, descriptor.digest);
    assert!(
        asset.bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "a real PNG, not a stand-in"
    );

    // An id this document does not define is absent, never another
    // asset's bytes.
    assert!(
        atlas
            .document_asset(&membership, &coordinate, 99, Some(1024 * 1024))
            .unwrap()
            .is_none()
    );

    // A caller's own bound is honoured before anything is handed back.
    let refused = atlas
        .document_asset(&membership, &coordinate, descriptor.id, Some(8))
        .unwrap_err();
    assert!(
        refused.to_string().contains("past the 8-byte bound"),
        "{refused}"
    );
}

/// The two native limits stay explicit rather than being smoothed into a
/// success: PDF has no document model at all, and a resource read as text
/// is not a document.
#[test]
fn the_reader_names_the_pdf_model_limit_and_a_non_document_by_name() {
    let tree = TempDir::new().unwrap();
    build_tree(tree.path());
    let (_estate, atlas, membership, generation) = open_and_acquire(tree.path());

    for (name, expected) in [
        ("22-pdf-text.pdf", "model_unavailable"),
        ("notes.md", "not_a_document"),
    ] {
        let record = resource(&generation, name);
        let coordinate = wirk_atlas::ExactCoordinate {
            estate: membership.estate.clone(),
            membership: membership.id.clone(),
            source: membership.source.clone(),
            generation: generation.id.clone(),
            path: record.path.clone(),
            object_id: record.object_id.clone().unwrap(),
            byte_start: 0,
            byte_end: 0,
            line_start: 1,
            line_end: 1,
        };
        match (
            expected,
            atlas.document_reading(&membership, &coordinate).unwrap(),
        ) {
            ("model_unavailable", wirk_atlas::DocumentReading::ModelUnavailable(detail)) => {
                assert!(detail.contains("no document model"), "{name}: {detail}");
            }
            ("not_a_document", wirk_atlas::DocumentReading::NotADocument) => {}
            (_, other) => panic!("{name}: expected {expected}, got {other:?}"),
        }
    }
}
