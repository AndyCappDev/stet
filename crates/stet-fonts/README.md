# stet-fonts

Font parsing and geometry types for the stet PostScript interpreter.

This is a low-level crate. Most users should use the [`stet`](https://crates.io/crates/stet)
facade crate instead.

## Contents

- **Geometry** — `Matrix` (affine transforms), `PathSegment`, `PsPath`
- **Type 1 parser** — Adobe Type 1 font binary/ASCII parsing
- **CFF parser** — Compact Font Format (CFF/Type 2) parsing
- **TrueType parser** — TrueType glyf table, composite glyph resolution
- **Type 2 charstring interpreter** — CFF charstring → path conversion
- **Encoding tables** — StandardEncoding, ISOLatin1Encoding, SymbolEncoding, etc.
- **Adobe Glyph List** — glyph name → Unicode mapping
- **System fonts** — platform font discovery
- **Font substitutions** — Adobe font names → URW equivalents (e.g., Times-Roman → NimbusRoman-Regular)

## Usage

```rust
use stet_fonts::geometry::{Matrix, PsPath, PathSegment};
use stet_fonts::type1_parser;
use stet_fonts::encoding;
```

## License

Apache-2.0 OR MIT
