# stet-fonts

[![crates.io](https://img.shields.io/crates/v/stet-fonts.svg)](https://crates.io/crates/stet-fonts)
[![docs.rs](https://img.shields.io/docsrs/stet-fonts)](https://docs.rs/stet-fonts)

Pure-Rust parsers for Type 1, CFF / Type 2, and TrueType fonts, plus the
geometry primitives they produce.

This crate **has no stet-specific dependencies** and is usable on its own
for font-parsing workflows that don't need a PostScript interpreter or PDF
renderer. If you're building a stet-family project, the
[`stet`](https://crates.io/crates/stet) facade pulls this in automatically.

## What's inside

| Module | Purpose |
|--------|---------|
| `geometry` | `Matrix` (affine transforms), `PathSegment`, `PsPath` — the shared path/transform types |
| `type1_parser` | Adobe Type 1 binary / ASCII parser; eexec decryption |
| `charstring` | Type 1 charstring interpreter (`CharString` → `PsPath`) |
| `cff_parser` | Compact Font Format (CFF) parser — returns every font in the file, including CID-keyed FontSets |
| `type2_charstring` | Type 2 charstring interpreter for CFF glyphs |
| `truetype` | TrueType table accessors (`head`, `loca`, `glyf`, `hmtx`, `cmap`), simple & composite glyph resolution, `glyf` → `PsPath` conversion |
| `encoding` | `StandardEncoding`, `ISOLatin1Encoding`, `SymbolEncoding`, and other named encodings |
| `agl` | Adobe Glyph List — glyph name → Unicode mapping |
| `system_fonts` | Platform font directory discovery, substitution of the 35 standard PostScript names to URW equivalents |

## Standalone use

```toml
[dependencies]
stet-fonts = "0.2"
```

### Parse a Type 1 font

```rust
use stet_fonts::type1_parser::parse_type1;

let data = std::fs::read("NimbusRoman-Regular.t1")?;
let font = parse_type1(&data)?;
println!("Font name: {}", font.font_name);
println!("Glyphs: {}", font.charstrings.len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Parse a CFF font and render a glyph to a path

```rust
use stet_fonts::cff_parser::parse_cff;
use stet_fonts::type2_charstring::execute_type2_charstring;

let data = std::fs::read("HelveticaNeue.cff")?;    // bare .cff (OpenType CFF
                                                   // tables must be extracted first)
let fonts = parse_cff(&data)?;
let font = &fonts[0];

// Run the Type 2 charstring program for GID 1 (GID 0 is always .notdef).
let gid = 1usize;
let result = execute_type2_charstring(
    &font.char_strings[gid],
    &font.local_subrs,
    &font.global_subrs,
    font.default_width_x,
    font.nominal_width_x,
    /* width_only = */ false,
)?;
// result.path  — `PsPath` in font units
// result.width_x — advance width
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Read TrueType glyph outlines

```rust
use stet_fonts::truetype;

let data = std::fs::read("Arial.ttf")?;
let upm = truetype::get_units_per_em(&data);
let num_glyphs = truetype::get_num_glyphs(&data);

if let Some(glyf) = truetype::get_glyf_data(&data, /* GID */ 42) {
    // Resolver is used for composite glyphs: given a component GID,
    // return its own glyf bytes.
    let path = truetype::parse_glyf_to_path(&glyf, &|gid| {
        truetype::get_glyf_data(&data, gid)
    });
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Scope and non-goals

These parsers are the ones stet needs to render PDFs and run a PostScript
Level 3 interpreter, so the surface is shaped around that: whole-font
parsing, glyph outlines as `PsPath`, no layout or shaping, no colour
fonts, no variable-font (`fvar`/`gvar`) interpolation, no OpenType GSUB /
GPOS / BASE / GDEF tables. If you need a general-purpose font-layout
library, look at [`ttf-parser`](https://crates.io/crates/ttf-parser),
[`rustybuzz`](https://crates.io/crates/rustybuzz), or
[`skrifa`](https://crates.io/crates/skrifa) instead.

## License

Apache-2.0 OR MIT
