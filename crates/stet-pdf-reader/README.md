# stet-pdf-reader

PDF parser and renderer. Reads PDF files and converts pages to display lists
for rendering.

This crate has **no dependency on stet-core** — it depends only on `stet-fonts`
and `stet-graphics`, making it usable independently of the PostScript interpreter.

## Contents

- **`PdfDocument`** — Main entry point: parse a PDF, enumerate pages, render to display lists
- **Parser** — PDF object parser, cross-reference table, incremental updates
- **Filters** — FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode, DCTDecode, JPXDecode, CCITTFaxDecode, JBIG2Decode
- **Crypto** — PDF encryption (RC4, AES-128, AES-256)
- **Content interpreter** — PDF page content stream → display list conversion
- **Font handling** — Type 1, TrueType, CFF, CID fonts with encoding/CMap support

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `jpx` | yes | JPEG 2000 (JPXDecode) via `hayro-jpeg2000` |
| `render` | yes | `render_page_to_rgba()` via `stet-render` |

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

Apache-2.0 OR MIT
