// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Font parsing and geometry types for the stet PostScript interpreter.
//!
//! This crate provides:
//! - Affine transformation matrices and path geometry (`geometry`)
//! - Font encoding tables (`encoding`)
//! - Adobe Glyph List mapping (`agl`)
//! - Type 1 font parsing (`type1_parser`)
//! - CFF font parsing (`cff_parser`)
//! - Type 1 charstring interpretation (`charstring`)
//! - Type 2 charstring interpretation (`type2_charstring`)
//! - TrueType glyph parsing (`truetype`)
//! - System font discovery (`system_fonts`)

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
