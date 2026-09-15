//! Document admission, interpretation and structured reading through the
//! whole native `anydoc` 0.2.4 reader.
//!
//! `anydoc` parses each supported container into one shared
//! [`anydoc::model::Document`] — headings, paragraphs, tables, lists,
//! links, notes, equations and the bytes of every embedded asset — and
//! serializes that model to one GitHub-Flavored Markdown rendering in
//! reading order. That rendering, not the original binary bytes, is what
//! gets unitized: a `Document`-family unit's `byte_start`/`byte_end`
//! index the converted Markdown text, never an Office page/cell or a PDF
//! page. Every caller that later resolves or re-indexes a `Document`
//! unit's bytes must re-render through [`render_if_document`] rather than
//! slicing the original file, or it slices the wrong string entirely.
//!
//! [`resolved_format`] is the one place that decides how a resource is
//! read, and every other decision in this crate defers to it: what family
//! admission assigns, which unitizer stamps the units, whether hydration
//! renders or passes bytes through, and what the structured reader
//! parses. It answers for one extraction edition at a time — the one the
//! generation being read was recorded under — so widening the vocabulary
//! changes what new generations capture without changing how an existing
//! one is read back.
//!
//! PDF is the one format with no document-model form: `anydoc::to_document`
//! is unsupported there and `to_markdown_bytes` is the only PDF path, so
//! [`inspect`] reports that limit by name. A scanned or image-only PDF
//! surfaces as [`ConvertError::NeedsOcr`]; this reader performs no OCR and
//! adds no hosted service to make that case look like a success.
use anydoc::model::{Block, Document, ImageSource, Inline, LinkTarget};
use anydoc::{ConvertError, Format};
use sha2::{Digest, Sha256};

/// How many leading bytes of an otherwise-unrecognized file are read to
/// decide whether it is worth opening in full.
///
/// Every signature `Format::from_bytes` can act on lies inside this
/// window: `anydoc` 0.2.4's own detector opens on `{\rtf`, the OLE
/// compound-file magic or the ZIP local-file-header magic at offset 0,
/// or a `%PDF-` header within the first 1024 bytes (its own documented
/// bound for leading junk). A file whose first `SNIFF_BYTES` carry none
/// of those cannot be a document, so nothing beyond them is read.
pub(crate) const SNIFF_BYTES: usize = 1024;

const OLE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Whether `prefix` — the first [`SNIFF_BYTES`] of a file, or the whole
/// file when it is shorter — could possibly be a container
/// `Format::from_bytes` recognizes.
///
/// A *screen*, never a second detector: it decides only whether to read
/// the rest of the file and ask `anydoc` itself, and `anydoc`'s answer is
/// the only one that admits anything. Screening positive on a file that
/// then detects as nothing costs one bounded read and a truthful
/// `Unsupported`; screening negative on a real document would lose it, so
/// this mirrors the four entry conditions of the pinned `anydoc` 0.2.4
/// detector exactly, and `screen_agrees_with_the_dependency_detector`
/// pins that agreement against real bytes of every container family.
pub(crate) fn could_be_document(prefix: &[u8]) -> bool {
    prefix.starts_with(b"{\\rtf")
        || prefix.starts_with(&OLE_MAGIC)
        || prefix.starts_with(b"PK\x03\x04")
        || prefix[..prefix.len().min(1024)]
            .windows(5)
            .any(|window| window == b"%PDF-")
}

/// The `anydoc::Format` a path's extension names, or `None` for anything
/// `anydoc` itself does not recognize. Decided from the path alone.
/// Reuses `Format::from_extension` rather than a hand-copied subset that
/// could drift from the dependency's actual supported set: every
/// extension it documents — Word (`.doc`/`.docx`/`.docm`), PowerPoint
/// (`.ppt`/`.pps`/`.pot`/`.pptx`/`.pptm`/`.ppsx`/`.ppsm`), Excel
/// (`.xls`/`.xlsx`/`.xlsm`/`.xlsb`), OpenDocument (`.odt`/`.ods`/`.odp`),
/// RTF, EPUB, CSV and PDF — names a document here.
pub(crate) fn format_for_path(path: &[u8]) -> Option<Format> {
    let dot = path.iter().rposition(|b| *b == b'.')?;
    let ext = std::str::from_utf8(&path[dot + 1..]).ok()?;
    Format::from_extension(ext)
}

/// **The one interpretation authority.** How this resource is read under
/// `edition`: `Some(format)` to parse it through `anydoc` as that format,
/// `None` to treat its bytes as ordinary text.
///
/// **Scoped to an edition, not to this binary.** A generation's units
/// index whatever string this function chose when that generation was
/// written — the original bytes, or the Markdown a container rendered to
/// — so every later read has to reach the same answer or it slices a
/// string the offsets do not describe. The edition each generation
/// records is what makes that possible, and it is threaded to here from
/// the record rather than re-derived from today's vocabulary
/// (`crate::extract::ExtractorEdition`, ruling 0095).
///
/// Three rules, in order, each answering a case the others get wrong:
///
/// 1. **The path names a document format.** Content still decides which
///    one, so a file whose extension says `.docx` but whose bytes are a
///    real RTF converts by what it actually is; the extension is the
///    fallback for a format that carries no signature to detect, which is
///    CSV's own case.
/// 2. **The path names a text family** (`.md`, `.rs`, `.toml`, … — the
///    `edition`'s own content-family vocabulary, which is where `.txt`
///    joined it). Read as text, and the
///    bytes are never sniffed. This is what keeps ordinary Markdown and
///    code interpretation intact, and in particular what keeps a
///    byte-identical CSV/Markdown pair two different resources: the `.csv`
///    twin renders a table, the `.md` twin stays the Markdown it is.
/// 3. **The path names nothing either vocabulary recognizes** — no
///    extension at all, or one outside both catalogues. Only here do the
///    bytes decide, through `Format::from_bytes`. An extensionless RFP or
///    a spreadsheet saved as `report.bin` is a document this reader can
///    genuinely read, and refusing it for the shape of its name was a
///    narrower answer than the estate's own exclusions ever asked for.
///    An unrecognized extension is not exclusion authority; a path
///    `ExtractorPolicy::excluded` refuses is never opened at all, and that
///    check runs first, before any byte of any candidate is read.
pub(crate) fn resolved_format(
    edition: crate::extract::ExtractorEdition,
    path: &[u8],
    bytes: &[u8],
) -> Option<Format> {
    if let Some(named) = format_for_path(path) {
        return Some(Format::from_bytes(bytes).unwrap_or(named));
    }
    if crate::extract::path_names_text_family(edition, path) {
        return None;
    }
    Format::from_bytes(bytes)
}

/// Convert one whole document to its Markdown rendering, or a truthful,
/// specific reason it could not be. `format` is what
/// [`resolved_format`] already decided; passing it explicitly keeps this
/// from re-deciding and disagreeing with the family a resource was
/// admitted under. The `.code()` tag is folded into the message so a
/// later reader can branch on the stable reason without re-parsing prose.
pub(crate) fn render(format: Format, bytes: &[u8]) -> Result<String, String> {
    anydoc::to_markdown_bytes(bytes, format).map_err(|error: ConvertError| {
        format!("document conversion failed ({}): {error}", error.code())
    })
}

/// Re-render a resource's bytes through [`render`] when the generation's
/// own `edition` reads it as a document, otherwise pass the bytes through
/// unchanged. The one place every byte-reading caller
/// (`hydrate::blob`/`blobs`, `AtlasStore::resolve_exact_*`) routes
/// through, so a `Document` unit is always resolved, re-indexed or
/// embedded against the same Markdown text it was unitized from — never
/// against the original binary the unit's offsets do not describe.
///
/// `edition` is the recorded one
/// (`SourceGeneration::extractor_set`/`ChunkerIdentity::extractor_set`),
/// never today's default. Re-deriving it here is precisely how a
/// vocabulary change turns into silently wrong bytes: the resource was
/// unitized under the edition that captured it, and that is the only
/// interpretation whose offsets mean anything.
pub(crate) fn render_if_document(
    edition: crate::extract::ExtractorEdition,
    path: &[u8],
    bytes: Vec<u8>,
) -> Result<Vec<u8>, String> {
    match resolved_format(edition, path, &bytes) {
        Some(format) => render(format, &bytes).map(String::into_bytes),
        None => Ok(bytes),
    }
}

/// The cache key `hydrate::blobs` records one resource's hydrated bytes
/// under: the object id the resource was recorded with, folded together
/// with that resource's own path.
///
/// Keyed by the pair and not by the object id alone because one raw byte
/// string can back two resources that render to two different strings — a
/// CSV and a Markdown twin with identical content, or a document and a
/// plain-text reading of the same bytes. A cache keyed by object id alone
/// holds only one of them and silently hands the other's callers the wrong
/// content. Keyed by the pair and not by a
/// path-derived interpretation tag because interpretation now depends on
/// the bytes as well as the path, and the consumer looking an entry back
/// up holds only the recorded resource. Renders are still shared: entries
/// whose bytes and interpretation agree hold the same `Arc`, so the exact
/// key costs addressing, not memory.
///
/// Raw bytes, not a `String`: a path is a byte string that need not be
/// UTF-8, and two distinct paths must never fold onto one key through a
/// lossy conversion. Object ids are hex digests, so the `NUL` separator
/// cannot occur in the first field.
pub(crate) fn resource_key(path: &[u8], object_id: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(object_id.len() + 1 + path.len());
    key.extend_from_slice(object_id.as_bytes());
    key.push(0);
    key.extend_from_slice(path);
    key
}

/// One embedded asset, described without its bytes: what a caller needs to
/// decide whether to ask for them. `digest` is the SHA-256 of the payload
/// as the source stored it, so a caller can tell two assets apart, and can
/// check that bytes it later reads are the ones this inventory described.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocumentAsset {
    /// The asset's own index in the document's asset list — `anydoc`'s own
    /// `AssetId`, which is what `ImageSource::Asset` in the body refers to.
    /// This is the selector [`asset`] takes; it is scoped to one
    /// document's parse and is not an estate-wide coordinate.
    pub id: usize,
    pub media_type: String,
    /// The package part or stream the asset came from, as the source names
    /// it — the document's own provenance for these bytes.
    pub origin_part: String,
    pub byte_len: u64,
    pub digest: String,
}

/// One heading in reading order: how a caller sees the document's shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocumentHeading {
    pub level: u8,
    pub text: String,
}

/// One table's shape. Deliberately dimensions and header text rather than
/// the whole grid: the whole grid is already in the Markdown rendering
/// `search`/`resolve` serve, and repeating it here would be a second copy
/// of the same content under a second addressing scheme.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocumentTable {
    pub rows: usize,
    pub columns: usize,
    pub header: Vec<String>,
}

/// What the structured reader found in one document: its shape and its
/// embedded assets, never their bytes. Binary payloads are deliberately
/// absent — an inventory is safe to put in front of a model, a megabyte of
/// image bytes is not — and [`asset`] is the explicit, separate step that
/// reads one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocumentOutline {
    /// The format the document was actually parsed as, as `anydoc` names
    /// it — the resolved one, which for a mislabeled file is not what its
    /// extension claimed.
    pub format: String,
    pub blocks: usize,
    pub headings: Vec<DocumentHeading>,
    pub tables: Vec<DocumentTable>,
    pub lists: usize,
    pub code_blocks: usize,
    pub equations: usize,
    /// Note bodies (footnotes and endnotes) the document defines.
    pub notes: usize,
    /// Every external link target the body points at, in reading order.
    /// Internal anchors are not links out and are not listed.
    pub links: Vec<String>,
    /// How many images the body places, including ones whose bytes the
    /// source no longer holds — so a caller can tell "no images" from
    /// "images whose parts are missing".
    pub images: usize,
    pub assets: Vec<DocumentAsset>,
}

/// What one structured read produced. Every arm is a truthful answer a
/// caller can act on, and none of them is an empty success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentReading {
    Read(DocumentOutline),
    /// This resource is not read as a document at all — its path names a
    /// text family, or its bytes and name name nothing `anydoc` parses.
    NotADocument,
    /// The format is one `anydoc` supports, but it has no document-model
    /// form. PDF is the only such format: it converts to Markdown directly
    /// and `to_document` is unsupported for it, so `search`/`resolve` serve
    /// a PDF's text and this reader has no structure or assets to offer.
    ModelUnavailable(String),
    /// The parse itself failed, with the dependency's own stable reason
    /// code — malformed, encrypted, resource-limited, or needing OCR this
    /// reader does not perform.
    Failed(String),
}

/// One embedded asset's bytes, with the descriptor that names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAsset {
    pub descriptor: DocumentAsset,
    pub bytes: Vec<u8>,
}

/// Parse one resource into `anydoc`'s shared document model and describe
/// what it holds. Reads structure, not a string: this is the reader
/// capability the Markdown serializer cannot offer, and it is what makes
/// a document's embedded diagram reachable at all.
pub(crate) fn inspect(
    edition: crate::extract::ExtractorEdition,
    path: &[u8],
    bytes: &[u8],
) -> DocumentReading {
    let Some(format) = resolved_format(edition, path, bytes) else {
        return DocumentReading::NotADocument;
    };
    if format == Format::Pdf {
        return DocumentReading::ModelUnavailable(
            "anydoc converts PDF to Markdown directly and exposes no document model for it, so \
             this source has no structured form or embedded assets to read; its text is served by \
             search and resolve"
                .into(),
        );
    }
    match anydoc::to_document(bytes, format) {
        Ok(document) => DocumentReading::Read(outline_of(format, &document)),
        Err(error) => {
            DocumentReading::Failed(format!("document parse failed ({}): {error}", error.code()))
        }
    }
}

/// One embedded asset's bytes, selected by the `id` an [`inspect`]
/// inventory listed. `None` for an id this document does not define —
/// never a different asset's bytes.
pub(crate) fn asset(
    edition: crate::extract::ExtractorEdition,
    path: &[u8],
    bytes: &[u8],
    id: usize,
) -> Result<Option<ResolvedAsset>, String> {
    let Some(format) = resolved_format(edition, path, bytes) else {
        return Err("this resource is not read as a document".into());
    };
    if format == Format::Pdf {
        return Err(
            "anydoc exposes no document model for PDF, so it has no embedded assets to read".into(),
        );
    }
    let document = anydoc::to_document(bytes, format)
        .map_err(|error| format!("document parse failed ({}): {error}", error.code()))?;
    Ok(document
        .assets
        .iter()
        .find(|candidate| candidate.id.0 == id)
        .map(|found| ResolvedAsset {
            descriptor: describe(found),
            bytes: found.bytes.clone(),
        }))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn describe(asset: &anydoc::model::Asset) -> DocumentAsset {
    let mut hasher = Sha256::new();
    hasher.update(&asset.bytes);
    DocumentAsset {
        id: asset.id.0,
        media_type: asset.media_type.clone(),
        origin_part: asset.origin_part.clone(),
        byte_len: asset.bytes.len() as u64,
        digest: hex(&hasher.finalize()),
    }
}

fn outline_of(format: Format, document: &Document) -> DocumentOutline {
    let mut outline = DocumentOutline {
        format: format!("{format:?}"),
        blocks: document.blocks.len(),
        headings: Vec::new(),
        tables: Vec::new(),
        lists: 0,
        code_blocks: 0,
        equations: 0,
        notes: document.notes.len(),
        links: Vec::new(),
        images: 0,
        assets: document.assets.iter().map(describe).collect(),
    };
    walk(&document.blocks, &mut outline);
    for note in &document.notes {
        walk(&note.blocks, &mut outline);
    }
    outline
}

fn walk(blocks: &[Block], outline: &mut DocumentOutline) {
    for block in blocks {
        match block {
            Block::Heading { level, content, .. } => {
                outline.headings.push(DocumentHeading {
                    level: *level,
                    text: anydoc::model::inlines_to_plain_text(content),
                });
                scan(content, outline);
            }
            Block::Paragraph(inlines) => scan(inlines, outline),
            Block::List(list) => {
                outline.lists += 1;
                for item in &list.items {
                    walk(&item.blocks, outline);
                }
            }
            Block::Table(table) => {
                let columns = table.grid.iter().map(Vec::len).max().unwrap_or(0);
                let header = table
                    .grid
                    .first()
                    .map(|row| row.iter().map(cell_text).collect())
                    .unwrap_or_default();
                outline.tables.push(DocumentTable {
                    rows: table.grid.len(),
                    columns,
                    header,
                });
                for row in &table.grid {
                    for slot in row {
                        if let anydoc::model::CellSlot::Origin(cell) = slot {
                            walk(&cell.blocks, outline);
                        }
                    }
                }
            }
            Block::BlockQuote(inner) => walk(inner, outline),
            Block::CodeBlock { .. } => outline.code_blocks += 1,
            Block::Math(_) => outline.equations += 1,
            Block::Rule => {}
        }
    }
}

fn cell_text(slot: &anydoc::model::CellSlot) -> String {
    match slot {
        anydoc::model::CellSlot::Origin(cell) => {
            let mut text = String::new();
            for block in &cell.blocks {
                if let Block::Paragraph(inlines) = block {
                    text.push_str(&anydoc::model::inlines_to_plain_text(inlines));
                }
            }
            text
        }
        _ => String::new(),
    }
}

fn scan(inlines: &[Inline], outline: &mut DocumentOutline) {
    for inline in inlines {
        match inline {
            Inline::Link { content, target } => {
                match target {
                    LinkTarget::External(url) | LinkTarget::Relative(url) if !url.is_empty() => {
                        outline.links.push(url.clone());
                    }
                    _ => {}
                }
                scan(content, outline);
            }
            Inline::Image { source, .. } => {
                outline.images += 1;
                let _ = matches!(source, ImageSource::Asset(_));
            }
            Inline::Math(_) => outline.equations += 1,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractorEdition;

    /// The edition new generations are captured under. Every assertion
    /// below that does not deliberately name an older one asks this.
    const CURRENT: ExtractorEdition = ExtractorEdition::DocumentsDetectedTextV7;

    #[test]
    fn recognizes_every_extension_anydoc_itself_documents() {
        let cases: &[(&[u8], Format)] = &[
            (b"a/b.doc", Format::Doc),
            (b"a/b.DOC", Format::Doc),
            (b"a/b.docx", Format::Docx),
            (b"a/b.docm", Format::Docx),
            (b"a/b.odt", Format::Odt),
            (b"a/b.pdf", Format::Pdf),
            (b"a/b.ppt", Format::Ppt),
            (b"a/b.pps", Format::Ppt),
            (b"a/b.pot", Format::Ppt),
            (b"a/b.pptx", Format::Pptx),
            (b"a/b.pptm", Format::Pptx),
            (b"a/b.ppsx", Format::Pptx),
            (b"a/b.ppsm", Format::Pptx),
            (b"a/b.rtf", Format::Rtf),
            (b"a/b.epub", Format::Epub),
            (b"a/b.xlsx", Format::Excel),
            (b"a/b.xlsm", Format::Excel),
            (b"a/b.xlsb", Format::Excel),
            (b"a/b.xls", Format::Excel),
            (b"a/b.ods", Format::Ods),
            (b"a/b.odp", Format::Odp),
            (b"a/b.csv", Format::Csv),
        ];
        for (path, expected) in cases {
            assert_eq!(format_for_path(path), Some(*expected), "{path:?}");
        }
        assert_eq!(format_for_path(b"a/b.rs"), None);
        assert_eq!(format_for_path(b"a/b.md"), None);
        assert_eq!(format_for_path(b"noext"), None);
    }

    #[test]
    fn a_text_family_extension_is_never_sniffed_into_a_document() {
        // A real RTF body under a `.md` name stays Markdown: rule 2 of
        // `resolved_format`, and the reason a byte-identical CSV/Markdown
        // pair remains two different readings.
        let rtf = b"{\\rtf1\\ansi hello}";
        assert_eq!(resolved_format(CURRENT, b"notes.md", rtf), None);
        assert_eq!(resolved_format(CURRENT, b"notes.rs", rtf), None);
        // The same bytes under no recognized extension are read for what
        // they are.
        assert_eq!(resolved_format(CURRENT, b"notes", rtf), Some(Format::Rtf));
        assert_eq!(
            resolved_format(CURRENT, b"notes.rfp", rtf),
            Some(Format::Rtf)
        );
    }

    #[test]
    fn content_overrides_a_misleading_document_extension() {
        let rtf = b"{\\rtf1\\ansi hello}";
        assert_eq!(
            resolved_format(CURRENT, b"report.docx", rtf),
            Some(Format::Rtf)
        );
        // CSV carries no signature, so its extension is what names it.
        assert_eq!(
            resolved_format(CURRENT, b"rows.csv", b"a,b\n1,2\n"),
            Some(Format::Csv)
        );
    }

    /// `.txt` is the plainest text name there is, and the current
    /// edition reads it as text rather than sniffing it as a container —
    /// while `v6`, which has no answer for `.txt`, keeps the answer it
    /// always gave.
    ///
    /// Both halves matter. The first is the preserved text-family
    /// interpretation: before `v7` an ordinary glossary fell
    /// to rule 3, was screened for a container, found none in prose and
    /// was captured `Unsupported("no extractor for path family")`. The
    /// second is why that correction is an edition and not a row in the
    /// shared vocabulary: a `v6` generation whose `.txt` held real RTF
    /// was recorded as a `Document` over its Markdown rendering, and its
    /// units index that rendering. Answering `None` for it under `v6`
    /// would hand every later reader the original container's bytes
    /// against offsets describing a string those bytes never contained.
    #[test]
    fn an_ordinary_text_file_is_read_as_text_under_the_current_edition() {
        const V6: ExtractorEdition = ExtractorEdition::DocumentsDetectedV6;
        let prose = b"Estate: the bounded domain.\n";
        let rtf = b"{\\rtf1\\ansi hello}";

        // Prose is text either way: `v6` sniffed it and found nothing.
        assert_eq!(resolved_format(CURRENT, b"glossary.txt", prose), None);
        assert_eq!(resolved_format(V6, b"glossary.txt", prose), None);
        assert!(crate::extract::path_names_text_family(
            CURRENT,
            b"glossary.txt"
        ));
        assert!(!crate::extract::path_names_text_family(V6, b"glossary.txt"));

        // Container bytes under a `.txt` name are where the editions
        // part, and each keeps its own answer.
        assert_eq!(resolved_format(CURRENT, b"notes.txt", rtf), None);
        assert_eq!(resolved_format(V6, b"notes.txt", rtf), Some(Format::Rtf));

        // A genuinely non-text name is unaffected in both: still not
        // text family, still decided by content.
        assert!(!crate::extract::path_names_text_family(
            CURRENT,
            b"photo.png"
        ));
        assert_eq!(
            resolved_format(CURRENT, b"report.pdf", b"%PDF-1.4\n"),
            Some(Format::Pdf)
        );
    }

    #[test]
    fn plain_text_under_an_unknown_extension_is_not_a_document() {
        assert_eq!(
            resolved_format(CURRENT, b"payload.bin", b"just words\n"),
            None
        );
        assert_eq!(resolved_format(CURRENT, b"payload", b""), None);
    }

    #[test]
    fn the_screen_accepts_exactly_what_the_detector_can_act_on() {
        // Every container family's own entry condition, and the negatives
        // that must not cost a full read.
        assert!(could_be_document(b"{\\rtf1\\ansi"));
        assert!(could_be_document(b"PK\x03\x04rest"));
        assert!(could_be_document(&OLE_MAGIC));
        assert!(could_be_document(b"%PDF-1.7"));
        let mut junk = vec![b' '; 500];
        junk.extend_from_slice(b"%PDF-1.4");
        assert!(could_be_document(&junk));
        assert!(!could_be_document(b""));
        assert!(!could_be_document(b"name,quantity\nWidget,12\n"));
        assert!(!could_be_document(b"# A heading\n\nProse.\n"));
        assert!(!could_be_document(b"\x7fELF\x02\x01\x01"));
    }

    #[test]
    fn render_surfaces_malformed_input_by_name_not_as_empty_success() {
        let err = render(Format::Pdf, b"not a real pdf").unwrap_err();
        assert!(err.contains("document conversion failed ("), "{err}");
    }

    #[test]
    fn resource_key_separates_two_paths_sharing_one_object_id() {
        let csv = resource_key(b"twin.csv", "obj-1");
        let md = resource_key(b"twin.md", "obj-1");
        assert_ne!(csv, md);
        assert_eq!(csv, resource_key(b"twin.csv", "obj-1"));
        // A non-UTF-8 path is not folded onto another by a lossy
        // conversion.
        assert_ne!(
            resource_key(b"a\xff", "obj-1"),
            resource_key(b"a\xfe", "obj-1")
        );
    }

    #[test]
    fn inspect_reports_the_pdf_model_limit_rather_than_an_empty_success() {
        match inspect(CURRENT, b"a.pdf", b"%PDF-1.7\n") {
            DocumentReading::ModelUnavailable(detail) => {
                assert!(detail.contains("no document model"), "{detail}");
            }
            other => panic!("expected ModelUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn inspect_names_a_non_document_rather_than_guessing_at_one() {
        assert_eq!(
            inspect(CURRENT, b"notes.md", b"# hi\n"),
            DocumentReading::NotADocument
        );
    }
}
