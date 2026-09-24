// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `rendering_intent` byte carried by display-list parameter structs.
//!
//! [`FillParams`](crate::device::FillParams),
//! [`StrokeParams`](crate::device::StrokeParams),
//! [`ImageParams`](crate::device::ImageParams) and
//! [`TextParams`](crate::device::TextParams) all store the ICC rendering
//! intent as a `u8`. This module is the single definition of what each value
//! means. Every producer (the PostScript interpreter, the PDF reader) and
//! every consumer (the rasterizer, the PDF writer) must go through it: the
//! PDF reader once used its own numbering, so a display list's intent meant
//! different things depending on which crate built it and which read it.
//!
//! The encoding is part of the display list's public contract and is
//! documented in `docs/DISPLAY-LIST.md`.

/// `/RelativeColorimetric`. The initial rendering intent in both PostScript
/// (PLRM 7.3) and PDF (ISO 32000-1 Table 52), and so the value a producer
/// uses when nothing has selected one.
pub const RELATIVE_COLORIMETRIC: u8 = 0;
/// `/AbsoluteColorimetric`.
pub const ABSOLUTE_COLORIMETRIC: u8 = 1;
/// `/Perceptual`.
pub const PERCEPTUAL: u8 = 2;
/// `/Saturation`.
pub const SATURATION: u8 = 3;

/// Map an intent name, as it appears after a PDF `ri` operator, in an
/// ExtGState `/RI` entry, in an image's `/Intent`, or as the operand of
/// PostScript `setrenderingintent`, to its byte. Returns `None` for a name
/// that is not one of the four standard intents; what to do then is the
/// caller's decision (PostScript raises `rangecheck`, PDF falls back).
pub fn from_name(name: &[u8]) -> Option<u8> {
    match name {
        b"RelativeColorimetric" => Some(RELATIVE_COLORIMETRIC),
        b"AbsoluteColorimetric" => Some(ABSOLUTE_COLORIMETRIC),
        b"Perceptual" => Some(PERCEPTUAL),
        b"Saturation" => Some(SATURATION),
        _ => None,
    }
}

/// Map a byte back to its intent name, or `None` for a value outside the
/// encoding.
pub fn name(intent: u8) -> Option<&'static [u8]> {
    match intent {
        RELATIVE_COLORIMETRIC => Some(b"RelativeColorimetric"),
        ABSOLUTE_COLORIMETRIC => Some(b"AbsoluteColorimetric"),
        PERCEPTUAL => Some(b"Perceptual"),
        SATURATION => Some(b"Saturation"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&[u8], u8); 4] = [
        (b"RelativeColorimetric", 0),
        (b"AbsoluteColorimetric", 1),
        (b"Perceptual", 2),
        (b"Saturation", 3),
    ];

    #[test]
    fn values_match_the_documented_encoding() {
        // These numbers are public: third-party renderers read them per
        // docs/DISPLAY-LIST.md. Changing one is a breaking change.
        for (n, b) in ALL {
            assert_eq!(from_name(n), Some(b), "{}", String::from_utf8_lossy(n));
        }
    }

    #[test]
    fn name_round_trips() {
        for (n, b) in ALL {
            assert_eq!(name(b), Some(n));
            assert_eq!(from_name(name(b).unwrap()), Some(b));
        }
    }

    #[test]
    fn unknown_values_are_rejected() {
        assert_eq!(from_name(b"Colorimetric"), None);
        assert_eq!(from_name(b""), None);
        assert_eq!(name(4), None);
        assert_eq!(name(255), None);
    }
}
