// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Embedded resource files for the stet facade crate.
//!
//! All PostScript resources (init scripts, encodings, fonts, etc.) are compiled
//! into the binary via `include_bytes!()`, eliminating the need for a
//! `resources/` directory at runtime.

use stet_core::file_store::FileStore;

/// Embedded CC0-licensed CMYK ICC profile for CMYK→sRGB conversion.
pub const DEFAULT_CMYK_ICC: &[u8] = include_bytes!("../resources/default_cmyk.icc");

/// All embedded resource entries: (relative_path, data).
const ENTRIES: &[(&str, &[u8])] = &[
    // Init scripts
    (
        "Init/sysdict.ps",
        include_bytes!("../resources/Init/sysdict.ps"),
    ),
    (
        "Init/resourcecategories.ps",
        include_bytes!("../resources/Init/resourcecategories.ps"),
    ),
    (
        "Init/fontcategory.ps",
        include_bytes!("../resources/Init/fontcategory.ps"),
    ),
    (
        "Init/fontmapping.ps",
        include_bytes!("../resources/Init/fontmapping.ps"),
    ),
    // Encodings
    (
        "Encoding/StandardEncoding.ps",
        include_bytes!("../resources/Encoding/StandardEncoding.ps"),
    ),
    (
        "Encoding/ISOLatin1Encoding.ps",
        include_bytes!("../resources/Encoding/ISOLatin1Encoding.ps"),
    ),
    (
        "Encoding/SymbolEncoding.ps",
        include_bytes!("../resources/Encoding/SymbolEncoding.ps"),
    ),
    // ProcSet
    (
        "ProcSet/CIDInit.ps",
        include_bytes!("../resources/ProcSet/CIDInit.ps"),
    ),
    (
        "ProcSet/FontSetInit.ps",
        include_bytes!("../resources/ProcSet/FontSetInit.ps"),
    ),
    // CMap
    (
        "CMap/Identity-H.ps",
        include_bytes!("../resources/CMap/Identity-H.ps"),
    ),
    (
        "CMap/Identity-V.ps",
        include_bytes!("../resources/CMap/Identity-V.ps"),
    ),
    // ColorSpace
    (
        "ColorSpace/DefaultCMYK.ps",
        include_bytes!("../resources/ColorSpace/DefaultCMYK.ps"),
    ),
    (
        "ColorSpace/DefaultGray.ps",
        include_bytes!("../resources/ColorSpace/DefaultGray.ps"),
    ),
    (
        "ColorSpace/DefaultRGB.ps",
        include_bytes!("../resources/ColorSpace/DefaultRGB.ps"),
    ),
    // OutputDevice
    (
        "OutputDevice/png.ps",
        include_bytes!("../resources/OutputDevice/png.ps"),
    ),
    (
        "OutputDevice/pdf.ps",
        include_bytes!("../resources/OutputDevice/pdf.ps"),
    ),
    (
        "OutputDevice/viewer.ps",
        include_bytes!("../resources/OutputDevice/viewer.ps"),
    ),
    // FontSet
    (
        "FontSet/NimbusRoman-Regular-CFF.ps",
        include_bytes!("../resources/FontSet/NimbusRoman-Regular-CFF.ps"),
    ),
    // Fonts — Core 13 families
    (
        "Font/NimbusRoman-Regular.t1",
        include_bytes!("../resources/Font/NimbusRoman-Regular.t1"),
    ),
    (
        "Font/NimbusRoman-Bold.t1",
        include_bytes!("../resources/Font/NimbusRoman-Bold.t1"),
    ),
    (
        "Font/NimbusRoman-Italic.t1",
        include_bytes!("../resources/Font/NimbusRoman-Italic.t1"),
    ),
    (
        "Font/NimbusRoman-BoldItalic.t1",
        include_bytes!("../resources/Font/NimbusRoman-BoldItalic.t1"),
    ),
    (
        "Font/NimbusSans-Regular.t1",
        include_bytes!("../resources/Font/NimbusSans-Regular.t1"),
    ),
    (
        "Font/NimbusSans-Bold.t1",
        include_bytes!("../resources/Font/NimbusSans-Bold.t1"),
    ),
    (
        "Font/NimbusSans-Italic.t1",
        include_bytes!("../resources/Font/NimbusSans-Italic.t1"),
    ),
    (
        "Font/NimbusSans-BoldItalic.t1",
        include_bytes!("../resources/Font/NimbusSans-BoldItalic.t1"),
    ),
    (
        "Font/NimbusMonoPS-Regular.t1",
        include_bytes!("../resources/Font/NimbusMonoPS-Regular.t1"),
    ),
    (
        "Font/NimbusMonoPS-Bold.t1",
        include_bytes!("../resources/Font/NimbusMonoPS-Bold.t1"),
    ),
    (
        "Font/NimbusMonoPS-Italic.t1",
        include_bytes!("../resources/Font/NimbusMonoPS-Italic.t1"),
    ),
    (
        "Font/NimbusMonoPS-BoldItalic.t1",
        include_bytes!("../resources/Font/NimbusMonoPS-BoldItalic.t1"),
    ),
    (
        "Font/StandardSymbolsPS.t1",
        include_bytes!("../resources/Font/StandardSymbolsPS.t1"),
    ),
    (
        "Font/D050000L.t1",
        include_bytes!("../resources/Font/D050000L.t1"),
    ),
    (
        "Font/NimbusSansNarrow-Regular.t1",
        include_bytes!("../resources/Font/NimbusSansNarrow-Regular.t1"),
    ),
    (
        "Font/NimbusSansNarrow-Bold.t1",
        include_bytes!("../resources/Font/NimbusSansNarrow-Bold.t1"),
    ),
    (
        "Font/NimbusSansNarrow-Oblique.t1",
        include_bytes!("../resources/Font/NimbusSansNarrow-Oblique.t1"),
    ),
    (
        "Font/NimbusSansNarrow-BoldOblique.t1",
        include_bytes!("../resources/Font/NimbusSansNarrow-BoldOblique.t1"),
    ),
    // Extended fonts
    (
        "Font/P052-Roman.t1",
        include_bytes!("../resources/Font/P052-Roman.t1"),
    ),
    (
        "Font/P052-Bold.t1",
        include_bytes!("../resources/Font/P052-Bold.t1"),
    ),
    (
        "Font/P052-Italic.t1",
        include_bytes!("../resources/Font/P052-Italic.t1"),
    ),
    (
        "Font/P052-BoldItalic.t1",
        include_bytes!("../resources/Font/P052-BoldItalic.t1"),
    ),
    (
        "Font/C059-Roman.t1",
        include_bytes!("../resources/Font/C059-Roman.t1"),
    ),
    (
        "Font/C059-Bold.t1",
        include_bytes!("../resources/Font/C059-Bold.t1"),
    ),
    (
        "Font/C059-Italic.t1",
        include_bytes!("../resources/Font/C059-Italic.t1"),
    ),
    (
        "Font/C059-BdIta.t1",
        include_bytes!("../resources/Font/C059-BdIta.t1"),
    ),
    (
        "Font/URWBookman-Light.t1",
        include_bytes!("../resources/Font/URWBookman-Light.t1"),
    ),
    (
        "Font/URWBookman-Demi.t1",
        include_bytes!("../resources/Font/URWBookman-Demi.t1"),
    ),
    (
        "Font/URWBookman-LightItalic.t1",
        include_bytes!("../resources/Font/URWBookman-LightItalic.t1"),
    ),
    (
        "Font/URWBookman-DemiItalic.t1",
        include_bytes!("../resources/Font/URWBookman-DemiItalic.t1"),
    ),
    (
        "Font/URWGothic-Book.t1",
        include_bytes!("../resources/Font/URWGothic-Book.t1"),
    ),
    (
        "Font/URWGothic-Demi.t1",
        include_bytes!("../resources/Font/URWGothic-Demi.t1"),
    ),
    (
        "Font/URWGothic-BookOblique.t1",
        include_bytes!("../resources/Font/URWGothic-BookOblique.t1"),
    ),
    (
        "Font/URWGothic-DemiOblique.t1",
        include_bytes!("../resources/Font/URWGothic-DemiOblique.t1"),
    ),
    (
        "Font/Z003-MediumItalic.t1",
        include_bytes!("../resources/Font/Z003-MediumItalic.t1"),
    ),
];

/// Register all embedded resource files into the FileStore's virtual filesystem.
///
/// Maps paths like "Init/sysdict.ps" and "resources/Init/sysdict.ps"
/// (both with and without the "resources/" prefix) so that `run` and
/// `.loadfont` can find them.
pub fn register_all(files: &mut FileStore) {
    for &(path, data) in ENTRIES {
        files.add_embedded_file(path, data);
        let full_path = format!("resources/{}", path);
        files.add_embedded_file(&full_path, data);
    }
}
