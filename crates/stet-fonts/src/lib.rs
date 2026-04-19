// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure-Rust parsers for Type 1, CFF / Type 2, and TrueType fonts, plus
//! the shared geometry primitives they produce.
//!
//! This crate has **no stet-specific dependencies**, so it is usable on
//! its own for font-parsing workflows that don't need a PostScript
//! interpreter or a PDF renderer. The rest of the stet workspace uses it
//! as the foundational font/geometry layer.
//!
//! # What's here
//!
//! - [`geometry`] — `Matrix` (affine transforms), `PathSegment`, `PsPath`
//! - [`type1_parser`] / [`charstring`] — Adobe Type 1 parsing and
//!   charstring interpretation (eexec decryption included)
//! - [`cff_parser`] / [`type2_charstring`] — Compact Font Format parser
//!   and Type 2 charstring interpreter
//! - [`truetype`] — TrueType table accessors, simple + composite glyph
//!   resolution, `glyf` → [`PsPath`] conversion
//! - [`encoding`] — StandardEncoding, ISOLatin1Encoding, SymbolEncoding,
//!   MacRoman, etc.
//! - [`agl`] — Adobe Glyph List (glyph name ↔ Unicode)
//! - [`system_fonts`] — platform font directory discovery and 35-standard
//!   PostScript → URW substitution
//!
//! # Quick example
//!
//! Parse a Type 1 font and inspect its glyph dictionary:
//!
//! ```no_run
//! use stet_fonts::type1_parser::parse_type1;
//!
//! let data = std::fs::read("NimbusRoman-Regular.t1")?;
//! let font = parse_type1(&data)?;
//! println!("font: {} ({} glyphs)", font.font_name, font.charstrings.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Scope and non-goals
//!
//! The surface is shaped around what stet needs to render PDFs and run a
//! PostScript Level 3 interpreter — whole-font parsing with glyph
//! outlines as `PsPath`. There is no text layout, no shaping, no colour
//! fonts, no variable-font interpolation, and no OpenType
//! GSUB/GPOS/BASE/GDEF tables. For a general-purpose font-layout
//! library, consider `ttf-parser`, `rustybuzz`, or `skrifa`.

pub mod agl;
pub mod cff_parser;
pub mod charstring;
pub mod encoding;
pub mod geometry;
pub mod system_fonts;
pub mod truetype;
pub mod type1_parser;
pub mod type2_charstring;

// Re-export core geometry types at crate root for convenience.
pub use geometry::{Matrix, PathSegment, PsPath};

/// Standard PostScript font name → URW replacement filename.
pub const FONT_SUBSTITUTIONS: &[(&str, &str)] = &[
    // Times family → Nimbus Roman
    ("Times-Roman", "NimbusRoman-Regular"),
    ("Times-Bold", "NimbusRoman-Bold"),
    ("Times-Italic", "NimbusRoman-Italic"),
    ("Times-BoldItalic", "NimbusRoman-BoldItalic"),
    // Helvetica family → Nimbus Sans
    ("Helvetica", "NimbusSans-Regular"),
    ("Helvetica-Bold", "NimbusSans-Bold"),
    ("Helvetica-Oblique", "NimbusSans-Italic"),
    ("Helvetica-BoldOblique", "NimbusSans-BoldItalic"),
    // Helvetica Narrow → Nimbus Sans Narrow
    ("Helvetica-Narrow", "NimbusSansNarrow-Regular"),
    ("Helvetica-Narrow-Bold", "NimbusSansNarrow-Bold"),
    ("Helvetica-Narrow-Oblique", "NimbusSansNarrow-Oblique"),
    (
        "Helvetica-Narrow-BoldOblique",
        "NimbusSansNarrow-BoldOblique",
    ),
    // Courier family → Nimbus Mono PS
    ("Courier", "NimbusMonoPS-Regular"),
    ("Courier-Bold", "NimbusMonoPS-Bold"),
    ("Courier-Oblique", "NimbusMonoPS-Italic"),
    ("Courier-BoldOblique", "NimbusMonoPS-BoldItalic"),
    // Arial family → Nimbus Sans (metric-compatible)
    ("ArialMT", "NimbusSans-Regular"),
    ("Arial-BoldMT", "NimbusSans-Bold"),
    ("Arial-ItalicMT", "NimbusSans-Italic"),
    ("Arial-BoldItalicMT", "NimbusSans-BoldItalic"),
    ("Arial", "NimbusSans-Regular"),
    ("Arial-Bold", "NimbusSans-Bold"),
    ("Arial-Italic", "NimbusSans-Italic"),
    ("Arial-BoldItalic", "NimbusSans-BoldItalic"),
    // Symbol fonts
    ("Symbol", "StandardSymbolsPS"),
    ("ZapfDingbats", "D050000L"),
    // Palatino → P052
    ("Palatino-Roman", "P052-Roman"),
    ("Palatino-Bold", "P052-Bold"),
    ("Palatino-Italic", "P052-Italic"),
    ("Palatino-BoldItalic", "P052-BoldItalic"),
    // New Century Schoolbook → C059
    ("NewCenturySchlbk-Roman", "C059-Roman"),
    ("NewCenturySchlbk-Bold", "C059-Bold"),
    ("NewCenturySchlbk-Italic", "C059-Italic"),
    ("NewCenturySchlbk-BoldItalic", "C059-BdIta"),
    // Bookman → URWBookman
    ("Bookman-Light", "URWBookman-Light"),
    ("Bookman-LightItalic", "URWBookman-LightItalic"),
    ("Bookman-Demi", "URWBookman-Demi"),
    ("Bookman-DemiItalic", "URWBookman-DemiItalic"),
    // AvantGarde → URWGothic
    ("AvantGarde-Book", "URWGothic-Book"),
    ("AvantGarde-BookOblique", "URWGothic-BookOblique"),
    ("AvantGarde-Demi", "URWGothic-Demi"),
    ("AvantGarde-DemiOblique", "URWGothic-DemiOblique"),
    // TimesNewRoman variants (common in Windows-generated PDFs)
    ("TimesNewRoman", "NimbusRoman-Regular"),
    ("TimesNewRomanPS", "NimbusRoman-Regular"),
    ("TimesNewRomanPSMT", "NimbusRoman-Regular"),
    ("TimesNewRoman-Bold", "NimbusRoman-Bold"),
    ("TimesNewRoman,Bold", "NimbusRoman-Bold"),
    ("TimesNewRomanPS-Bold", "NimbusRoman-Bold"),
    ("TimesNewRomanPS-BoldMT", "NimbusRoman-Bold"),
    ("TimesNewRoman-Italic", "NimbusRoman-Italic"),
    ("TimesNewRoman,Italic", "NimbusRoman-Italic"),
    ("TimesNewRomanPS-Italic", "NimbusRoman-Italic"),
    ("TimesNewRomanPS-ItalicMT", "NimbusRoman-Italic"),
    ("TimesNewRoman-BoldItalic", "NimbusRoman-BoldItalic"),
    ("TimesNewRomanPS-BoldItalic", "NimbusRoman-BoldItalic"),
    ("TimesNewRomanPS-BoldItalicMT", "NimbusRoman-BoldItalic"),
    // ZapfChancery
    ("ZapfChancery-MediumItalic", "Z003-MediumItalic"),
];

/// Look up a font substitution: "Helvetica" → "NimbusSans-Regular".
pub fn find_substitution(name: &str) -> Option<&'static str> {
    for &(ps_name, urw_name) in FONT_SUBSTITUTIONS {
        if ps_name == name {
            return Some(urw_name);
        }
    }
    None
}
