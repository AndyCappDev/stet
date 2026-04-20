// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Embedded resource files for WASM builds.
//!
//! All PostScript resources (init scripts, encodings, fonts, etc.) are compiled
//! into the WASM binary via `include_bytes!()`, eliminating the need for
//! filesystem access.

use std::collections::HashMap;
use std::sync::Arc;

use stet_core::file_store::FileStore;
use stet_pdf_reader::FontProvider;

// Init scripts
const SYSDICT_PS: &[u8] = include_bytes!("../../stet/resources/Init/sysdict.ps");
const RESOURCE_CATEGORIES_PS: &[u8] =
    include_bytes!("../../stet/resources/Init/resourcecategories.ps");
const FONT_CATEGORY_PS: &[u8] = include_bytes!("../../stet/resources/Init/fontcategory.ps");
const FONT_MAPPING_PS: &[u8] = include_bytes!("../../stet/resources/Init/fontmapping.ps");

// Encodings
const STANDARD_ENCODING_PS: &[u8] =
    include_bytes!("../../stet/resources/Encoding/StandardEncoding.ps");
const ISO_LATIN1_ENCODING_PS: &[u8] =
    include_bytes!("../../stet/resources/Encoding/ISOLatin1Encoding.ps");
const SYMBOL_ENCODING_PS: &[u8] = include_bytes!("../../stet/resources/Encoding/SymbolEncoding.ps");

// ProcSet
const CID_INIT_PS: &[u8] = include_bytes!("../../stet/resources/ProcSet/CIDInit.ps");
const FONT_SET_INIT_PS: &[u8] = include_bytes!("../../stet/resources/ProcSet/FontSetInit.ps");

// CMap
const IDENTITY_H_PS: &[u8] = include_bytes!("../../stet/resources/CMap/Identity-H.ps");
const IDENTITY_V_PS: &[u8] = include_bytes!("../../stet/resources/CMap/Identity-V.ps");

// Fonts — Core 13 families (17 font files covering the standard PostScript fonts)
const NIMBUS_ROMAN_REGULAR: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusRoman-Regular.t1");
const NIMBUS_ROMAN_BOLD: &[u8] = include_bytes!("../../stet/resources/Font/NimbusRoman-Bold.t1");
const NIMBUS_ROMAN_ITALIC: &[u8] = include_bytes!("../../stet/resources/Font/NimbusRoman-Italic.t1");
const NIMBUS_ROMAN_BOLD_ITALIC: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusRoman-BoldItalic.t1");
const NIMBUS_SANS_REGULAR: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusSans-Regular.t1");
const NIMBUS_SANS_BOLD: &[u8] = include_bytes!("../../stet/resources/Font/NimbusSans-Bold.t1");
const NIMBUS_SANS_ITALIC: &[u8] = include_bytes!("../../stet/resources/Font/NimbusSans-Italic.t1");
const NIMBUS_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusSans-BoldItalic.t1");
const NIMBUS_MONO_REGULAR: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusMonoPS-Regular.t1");
const NIMBUS_MONO_BOLD: &[u8] = include_bytes!("../../stet/resources/Font/NimbusMonoPS-Bold.t1");
const NIMBUS_MONO_ITALIC: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusMonoPS-Italic.t1");
const NIMBUS_MONO_BOLD_ITALIC: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusMonoPS-BoldItalic.t1");
const STANDARD_SYMBOLS: &[u8] = include_bytes!("../../stet/resources/Font/StandardSymbolsPS.t1");
const DINGBATS: &[u8] = include_bytes!("../../stet/resources/Font/D050000L.t1");
const NIMBUS_SANS_NARROW_REGULAR: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusSansNarrow-Regular.t1");
const NIMBUS_SANS_NARROW_BOLD: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusSansNarrow-Bold.t1");
const NIMBUS_SANS_NARROW_OBLIQUE: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusSansNarrow-Oblique.t1");
const NIMBUS_SANS_NARROW_BOLD_OBLIQUE: &[u8] =
    include_bytes!("../../stet/resources/Font/NimbusSansNarrow-BoldOblique.t1");

// Extended fonts
const P052_ROMAN: &[u8] = include_bytes!("../../stet/resources/Font/P052-Roman.t1");
const P052_BOLD: &[u8] = include_bytes!("../../stet/resources/Font/P052-Bold.t1");
const P052_ITALIC: &[u8] = include_bytes!("../../stet/resources/Font/P052-Italic.t1");
const P052_BOLD_ITALIC: &[u8] = include_bytes!("../../stet/resources/Font/P052-BoldItalic.t1");
const C059_ROMAN: &[u8] = include_bytes!("../../stet/resources/Font/C059-Roman.t1");
const C059_BOLD: &[u8] = include_bytes!("../../stet/resources/Font/C059-Bold.t1");
const C059_ITALIC: &[u8] = include_bytes!("../../stet/resources/Font/C059-Italic.t1");
const C059_BD_ITA: &[u8] = include_bytes!("../../stet/resources/Font/C059-BdIta.t1");
const URW_BOOKMAN_LIGHT: &[u8] = include_bytes!("../../stet/resources/Font/URWBookman-Light.t1");
const URW_BOOKMAN_DEMI: &[u8] = include_bytes!("../../stet/resources/Font/URWBookman-Demi.t1");
const URW_BOOKMAN_LIGHT_ITALIC: &[u8] =
    include_bytes!("../../stet/resources/Font/URWBookman-LightItalic.t1");
const URW_BOOKMAN_DEMI_ITALIC: &[u8] =
    include_bytes!("../../stet/resources/Font/URWBookman-DemiItalic.t1");
const URW_GOTHIC_BOOK: &[u8] = include_bytes!("../../stet/resources/Font/URWGothic-Book.t1");
const URW_GOTHIC_DEMI: &[u8] = include_bytes!("../../stet/resources/Font/URWGothic-Demi.t1");
const URW_GOTHIC_BOOK_OBLIQUE: &[u8] =
    include_bytes!("../../stet/resources/Font/URWGothic-BookOblique.t1");
const URW_GOTHIC_DEMI_OBLIQUE: &[u8] =
    include_bytes!("../../stet/resources/Font/URWGothic-DemiOblique.t1");
const Z003_MEDIUM_ITALIC: &[u8] = include_bytes!("../../stet/resources/Font/Z003-MediumItalic.t1");

/// Register all embedded resource files into the FileStore's virtual filesystem.
///
/// This maps paths like "resources/Init/sysdict.ps" and "Init/sysdict.ps"
/// (both with and without the "resources/" prefix) so that `op_run` and
/// `.loadfont` can find them.
pub fn register_embedded_resources(files: &mut FileStore) {
    let entries: &[(&str, &[u8])] = &[
        // Init scripts
        ("Init/sysdict.ps", SYSDICT_PS),
        ("Init/resourcecategories.ps", RESOURCE_CATEGORIES_PS),
        ("Init/fontcategory.ps", FONT_CATEGORY_PS),
        ("Init/fontmapping.ps", FONT_MAPPING_PS),
        // Encodings
        ("Encoding/StandardEncoding.ps", STANDARD_ENCODING_PS),
        ("Encoding/ISOLatin1Encoding.ps", ISO_LATIN1_ENCODING_PS),
        ("Encoding/SymbolEncoding.ps", SYMBOL_ENCODING_PS),
        // ProcSet
        ("ProcSet/CIDInit.ps", CID_INIT_PS),
        ("ProcSet/FontSetInit.ps", FONT_SET_INIT_PS),
        // CMap
        ("CMap/Identity-H.ps", IDENTITY_H_PS),
        ("CMap/Identity-V.ps", IDENTITY_V_PS),
        // Core fonts
        ("Font/NimbusRoman-Regular.t1", NIMBUS_ROMAN_REGULAR),
        ("Font/NimbusRoman-Bold.t1", NIMBUS_ROMAN_BOLD),
        ("Font/NimbusRoman-Italic.t1", NIMBUS_ROMAN_ITALIC),
        ("Font/NimbusRoman-BoldItalic.t1", NIMBUS_ROMAN_BOLD_ITALIC),
        ("Font/NimbusSans-Regular.t1", NIMBUS_SANS_REGULAR),
        ("Font/NimbusSans-Bold.t1", NIMBUS_SANS_BOLD),
        ("Font/NimbusSans-Italic.t1", NIMBUS_SANS_ITALIC),
        ("Font/NimbusSans-BoldItalic.t1", NIMBUS_SANS_BOLD_ITALIC),
        ("Font/NimbusMonoPS-Regular.t1", NIMBUS_MONO_REGULAR),
        ("Font/NimbusMonoPS-Bold.t1", NIMBUS_MONO_BOLD),
        ("Font/NimbusMonoPS-Italic.t1", NIMBUS_MONO_ITALIC),
        (
            "Font/NimbusMonoPS-BoldItalic.t1",
            NIMBUS_MONO_BOLD_ITALIC,
        ),
        ("Font/StandardSymbolsPS.t1", STANDARD_SYMBOLS),
        ("Font/D050000L.t1", DINGBATS),
        (
            "Font/NimbusSansNarrow-Regular.t1",
            NIMBUS_SANS_NARROW_REGULAR,
        ),
        (
            "Font/NimbusSansNarrow-Bold.t1",
            NIMBUS_SANS_NARROW_BOLD,
        ),
        (
            "Font/NimbusSansNarrow-Oblique.t1",
            NIMBUS_SANS_NARROW_OBLIQUE,
        ),
        (
            "Font/NimbusSansNarrow-BoldOblique.t1",
            NIMBUS_SANS_NARROW_BOLD_OBLIQUE,
        ),
        // Extended fonts
        ("Font/P052-Roman.t1", P052_ROMAN),
        ("Font/P052-Bold.t1", P052_BOLD),
        ("Font/P052-Italic.t1", P052_ITALIC),
        ("Font/P052-BoldItalic.t1", P052_BOLD_ITALIC),
        ("Font/C059-Roman.t1", C059_ROMAN),
        ("Font/C059-Bold.t1", C059_BOLD),
        ("Font/C059-Italic.t1", C059_ITALIC),
        ("Font/C059-BdIta.t1", C059_BD_ITA),
        ("Font/URWBookman-Light.t1", URW_BOOKMAN_LIGHT),
        ("Font/URWBookman-Demi.t1", URW_BOOKMAN_DEMI),
        ("Font/URWBookman-LightItalic.t1", URW_BOOKMAN_LIGHT_ITALIC),
        ("Font/URWBookman-DemiItalic.t1", URW_BOOKMAN_DEMI_ITALIC),
        ("Font/URWGothic-Book.t1", URW_GOTHIC_BOOK),
        ("Font/URWGothic-Demi.t1", URW_GOTHIC_DEMI),
        (
            "Font/URWGothic-BookOblique.t1",
            URW_GOTHIC_BOOK_OBLIQUE,
        ),
        (
            "Font/URWGothic-DemiOblique.t1",
            URW_GOTHIC_DEMI_OBLIQUE,
        ),
        ("Font/Z003-MediumItalic.t1", Z003_MEDIUM_ITALIC),
    ];

    for &(path, data) in entries {
        // Register both with and without "resources/" prefix
        files.add_embedded_file(path, data);
        let full_path = format!("resources/{}", path);
        files.add_embedded_file(&full_path, data);
    }
}

/// Build a font provider for the PDF reader from embedded font data.
///
/// Maps URW font file names (e.g. "NimbusSans-Regular") to raw .t1 bytes.
pub fn build_font_provider() -> FontProvider {
    let font_entries: &[(&str, &[u8])] = &[
        ("NimbusRoman-Regular", NIMBUS_ROMAN_REGULAR),
        ("NimbusRoman-Bold", NIMBUS_ROMAN_BOLD),
        ("NimbusRoman-Italic", NIMBUS_ROMAN_ITALIC),
        ("NimbusRoman-BoldItalic", NIMBUS_ROMAN_BOLD_ITALIC),
        ("NimbusSans-Regular", NIMBUS_SANS_REGULAR),
        ("NimbusSans-Bold", NIMBUS_SANS_BOLD),
        ("NimbusSans-Italic", NIMBUS_SANS_ITALIC),
        ("NimbusSans-BoldItalic", NIMBUS_SANS_BOLD_ITALIC),
        ("NimbusMonoPS-Regular", NIMBUS_MONO_REGULAR),
        ("NimbusMonoPS-Bold", NIMBUS_MONO_BOLD),
        ("NimbusMonoPS-Italic", NIMBUS_MONO_ITALIC),
        ("NimbusMonoPS-BoldItalic", NIMBUS_MONO_BOLD_ITALIC),
        ("StandardSymbolsPS", STANDARD_SYMBOLS),
        ("D050000L", DINGBATS),
        ("NimbusSansNarrow-Regular", NIMBUS_SANS_NARROW_REGULAR),
        ("NimbusSansNarrow-Bold", NIMBUS_SANS_NARROW_BOLD),
        ("NimbusSansNarrow-Oblique", NIMBUS_SANS_NARROW_OBLIQUE),
        ("NimbusSansNarrow-BoldOblique", NIMBUS_SANS_NARROW_BOLD_OBLIQUE),
        ("P052-Roman", P052_ROMAN),
        ("P052-Bold", P052_BOLD),
        ("P052-Italic", P052_ITALIC),
        ("P052-BoldItalic", P052_BOLD_ITALIC),
        ("C059-Roman", C059_ROMAN),
        ("C059-Bold", C059_BOLD),
        ("C059-Italic", C059_ITALIC),
        ("C059-BdIta", C059_BD_ITA),
        ("URWBookman-Light", URW_BOOKMAN_LIGHT),
        ("URWBookman-Demi", URW_BOOKMAN_DEMI),
        ("URWBookman-LightItalic", URW_BOOKMAN_LIGHT_ITALIC),
        ("URWBookman-DemiItalic", URW_BOOKMAN_DEMI_ITALIC),
        ("URWGothic-Book", URW_GOTHIC_BOOK),
        ("URWGothic-Demi", URW_GOTHIC_DEMI),
        ("URWGothic-BookOblique", URW_GOTHIC_BOOK_OBLIQUE),
        ("URWGothic-DemiOblique", URW_GOTHIC_DEMI_OBLIQUE),
        ("Z003-MediumItalic", Z003_MEDIUM_ITALIC),
    ];
    let map: HashMap<String, &'static [u8]> = font_entries
        .iter()
        .map(|&(name, data)| (name.to_string(), data))
        .collect();
    Arc::new(move |name: &str| map.get(name).map(|data| data.to_vec()))
}
