# stet-pdf-reader

[![crates.io](https://img.shields.io/crates/v/stet-pdf-reader.svg)](https://crates.io/crates/stet-pdf-reader)
[![docs.rs](https://img.shields.io/docsrs/stet-pdf-reader)](https://docs.rs/stet-pdf-reader)

PDF parser and renderer. Reads PDF files and converts pages to display lists
for rendering.

This crate has **no dependency on stet-core** — it depends only on `stet-fonts`,
`stet-graphics` and (for the `render` feature) `stet-render`, making it usable
independently of the PostScript interpreter. That holds with default features
on: `stet-render` is taken without its `ps-device` feature, so the PostScript
VM is absent from the dependency graph even when rendering to RGBA.

## Rendering Correctness

The display list produced by `stet-pdf-reader` preserves overprint, spot
colors, knockout groups, and ICC-based CMYK blend math — features most
lightweight PDF renderers drop because they don't affect on-screen preview.
Combined with binary clip coverage in the `stet-render` backend, this makes
the output suitable for prepress and proofing, not just viewing. See the
[repository README](https://github.com/AndyCappDev/stet#rendering-correctness)
for detail.

## Contents

- **`PdfDocument`** — Main entry point: parse a PDF, enumerate pages, render to display lists
- **Parser** — PDF object parser, cross-reference table, incremental updates
- **Filters** — FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode, DCTDecode, JPXDecode, CCITTFaxDecode, JBIG2Decode
- **Crypto** — PDF encryption (RC4, AES-128, AES-256)
- **Content interpreter** — PDF page content stream → display list conversion
- **Font handling** — Type 1, TrueType, CFF, CID fonts with encoding/CMap support
- **Text extraction** — Unicode text with every glyph's device-space position (`set_text_extraction`), assembled into words and lines with `stet_graphics::text`
- **Structural API** — metadata, outline, annotations, form fields, page boxes, embedded files, layers

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `jpx` | yes | JPEG 2000 (JPXDecode) via `hayro-jpeg2000` |
| `render` | yes | `render_page_to_rgba()` via `stet-render`, taken without its `ps-device` feature so no PostScript VM is linked |

## Usage

```rust
use stet_pdf_reader::PdfDocument;

let data = std::fs::read("document.pdf")?;
let doc = PdfDocument::from_bytes(&data)?;

println!("{} pages", doc.page_count());

for page in 0..doc.page_count() {
    let (w, h) = doc.page_size(page)?;
    println!("Page {}: {:.0}x{:.0} pt", page + 1, w, h);

    let display_list = doc.render_page(page, 300.0)?;
    println!("  {} display elements", display_list.elements().len());
}
```

### Extracting text

The assembly helpers live in `stet-graphics`, the display-list crate this
one builds on:

```rust
use stet_graphics::text::{text_lines, text_runs};
use stet_pdf_reader::{LayerSet, PdfDocument, TextExtraction};

let mut doc = PdfDocument::from_bytes(&data)?;
doc.set_text_extraction(TextExtraction::Runs); // or Glyphs, for glyph positions
let list = doc.render_page(0, 72.0)?;
for line in text_lines(text_runs(&list, &LayerSet::new())) {
    println!("{}", line.text);
}
```

`TextExtraction::Runs` records each run's text and extent;
`TextExtraction::Glyphs` adds every glyph's position, for word boxes and
selection. The runs are `DisplayElement::TextRun` elements, nested like the
content they came from — see the
[Display List Reference](https://github.com/AndyCappDev/stet/blob/main/docs/DISPLAY-LIST.md#textrun).

## Acknowledgements

JPEG 2000, JBIG2, and CCITT-Fax stream decoding use the
[`hayro-jpeg2000`](https://crates.io/crates/hayro-jpeg2000),
[`hayro-jbig2`](https://crates.io/crates/hayro-jbig2), and
[`hayro-ccitt`](https://crates.io/crates/hayro-ccitt) crates from the
[hayro](https://github.com/LaurenzV/hayro) PDF renderer by Laurenz
Stampfl. These are the best pure-Rust decoders available for those three
PDF stream filters, and `stet-pdf-reader` would not support the full PDF
stream-filter surface without them. Thanks to the hayro project for
factoring them out as reusable crates.

## License

Apache-2.0 OR MIT, except the 35 embedded URW++ base 35 font programs,
which are under the GNU AGPL v3 with a font exception — see
[`LICENSE-URW-FONTS`](LICENSE-URW-FONTS).
