# `document_formats.rs`'s own fixture subset

The 21 files `wirk-atlas/tests/document_formats.rs` actually reads (4 of
`docx_fixtures/`'s 6, 17 of `office_fixtures/`'s 17), copied byte-for-byte
so this normal test suite runs from the product checkout alone. 376 KiB
total (`du -sh` at copy time).

## Provenance

Every file here is a synthetic test fixture, hand-authored as raw
OOXML/ODF/RTF/PDF source (or, for the two zip-bomb-shaped hostile cases,
one legitimate text part padded with a repeated filler byte) by
`build_docx_fixtures.py` / `build_office_fixtures.py` — no document
library, no export from Word/LibreOffice, no third-party content.

Origin: `github.com/miztertea/sergeant-rs`, commit
`e18f4e9081486046451c280686496d16adc74e88`, path
`tests/fixtures/anydoc_corpus/` —
https://github.com/miztertea/sergeant-rs/tree/e18f4e9081486046451c280686496d16adc74e88/tests/fixtures/anydoc_corpus
— where the build scripts, the hand-verified `manifest.json` counts, and
the independent cross-check transcript live in full. That repository is
MIT-licensed (its own `LICENSE`, copyright miztertea); its build scripts
and this fixture set were both authored by this repository's own
maintainer, and the fixtures contain no content from any other party.
The 21 files below are byte-for-byte copies of that commit's fixtures,
verified by SHA-256 against it at copy time.

## Why not all of it

`02-nested-list-numbering.docx` and `04-footnotes-headers-footers.docx`
cover corpus scenarios `document_formats.rs` does not currently assert
on; omitted rather than carried as dead weight. If a future test needs
either, copy it the same way this set was copied, and add it here.
