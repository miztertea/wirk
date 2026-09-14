# Test fixtures

Small, real files the integration tests need and that no read-only
reference corpus on an ordinary checkout already provides. Each one is
here because a test's claim is only as real as the bytes it runs on.

## `legacy-excel-97.xls`

A genuine legacy OLE compound-file workbook (Excel 97 / BIFF8), not an
OOXML package and not another format wearing an `.xls` name. It is the
only real binary-Office fixture in reach: it exercises `anydoc`'s OLE
detection path (compound-file signature, then the `Workbook` stream) and
its legacy spreadsheet parser, neither of which the OOXML fixtures touch.

Produced once, from the three-column CSV its own cells still hold:

```sh
printf 'Region,Phase,Lead\nHarbour,Build,Nia\nDelta,Design,Ravi\nLegacymarker,Review,Ilse\n' > legacy.csv
soffice --headless --convert-to xls:"MS Excel 97" --outdir . legacy.csv
```

Legacy binary Word (`.doc`) and PowerPoint (`.ppt`) have no equivalent
fixture: no corpus in reach holds one and no writer for either is
installed here. `anydoc` names its own parsers for them; this repository
has not run one against a real file of either format.

## `embedded-diagram.png`

A real 8x8 RGB PNG — signature, `IHDR`, one deflate `IDAT`, `IEND`, every
chunk CRC correct — standing in for the diagram an ordinary working
document embeds. Small enough to read in full, real enough that what
comes back out of an Office package is a decodable image rather than a
byte string shaped like one. Its bytes are the ground truth the embedded
asset tests compare against.
