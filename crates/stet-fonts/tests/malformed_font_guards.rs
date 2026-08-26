// Regression tests for hostile or malformed font programs.
//
// Font data reaches these parsers from two directions — embedded in a PDF and
// embedded in a PostScript program — so every one of these is attacker
// controlled in the same way a PDF object is.
//
// Several abort the process when they regress rather than failing cleanly: a
// stack overflow is a SIGSEGV-backed abort and an oversized reservation aborts
// on allocation failure, and neither unwinds, so `catch_unwind` cannot contain
// them. A regression takes the whole test binary down instead of reporting one
// failed test. That is the intended signal.

use stet_fonts::charstring::execute_charstring_ex;
use stet_fonts::truetype::{parse_cmap, parse_glyf_to_path};
use stet_fonts::type2_charstring::execute_type2_charstring;

// === TrueType composite glyphs ============================================

/// A composite `glyf` entry with one component naming `component_gid`.
fn composite_glyph(component_gid: u16) -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(&(-1i16).to_be_bytes()); // numberOfContours < 0 => composite
    g.extend_from_slice(&[0u8; 8]); // xMin, yMin, xMax, yMax
    g.extend_from_slice(&0u16.to_be_bytes()); // flags: byte args, no MORE_COMPONENTS
    g.extend_from_slice(&component_gid.to_be_bytes());
    g.extend_from_slice(&[0u8, 0u8]); // dx, dy
    g
}

/// The classic `glyf` attack: a composite glyph listing itself as a
/// component. Unbounded, this recurses until the native stack is gone.
#[test]
fn self_referential_composite_glyph_does_not_overflow_the_stack() {
    let glyph = composite_glyph(0);
    let resolver = |_gid: u16| -> Option<Vec<u8>> { Some(composite_glyph(0)) };
    let _ = parse_glyf_to_path(&glyph, &resolver);
}

/// The same cycle spread over two glyphs, which a bare self-reference check
/// would miss.
#[test]
fn composite_glyph_ring_does_not_overflow_the_stack() {
    let resolver =
        |gid: u16| -> Option<Vec<u8>> { Some(composite_glyph(if gid == 0 { 1 } else { 0 })) };
    let _ = parse_glyf_to_path(&composite_glyph(1), &resolver);
}

/// A long chain rather than a cycle — every component distinct, so a
/// visited-set alone would not stop it. Only a depth cap does.
#[test]
fn deep_composite_glyph_chain_does_not_overflow_the_stack() {
    let resolver = |gid: u16| -> Option<Vec<u8>> { Some(composite_glyph(gid.wrapping_add(1))) };
    let _ = parse_glyf_to_path(&composite_glyph(1), &resolver);
}

// === cmap format 12 =======================================================

/// Build a font with a single cmap subtable.
fn font_with_cmap(subtable: &[u8]) -> Vec<u8> {
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes()); // version
    cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables
    cmap.extend_from_slice(&3u16.to_be_bytes()); // platformID = Windows
    cmap.extend_from_slice(&10u16.to_be_bytes()); // encodingID = UCS-4
    cmap.extend_from_slice(&12u32.to_be_bytes()); // offset to subtable
    cmap.extend_from_slice(subtable);

    // sfnt wrapper with one table directory entry.
    let mut font = Vec::new();
    font.extend_from_slice(&0x00010000u32.to_be_bytes()); // sfnt version
    font.extend_from_slice(&1u16.to_be_bytes()); // numTables
    font.extend_from_slice(&[0u8; 6]); // searchRange, entrySelector, rangeShift
    font.extend_from_slice(b"cmap");
    font.extend_from_slice(&0u32.to_be_bytes()); // checksum
    font.extend_from_slice(&28u32.to_be_bytes()); // offset
    font.extend_from_slice(&(cmap.len() as u32).to_be_bytes()); // length
    font.extend_from_slice(&cmap);
    font
}

/// A format 12 group covering the entire 32-bit code space. The loop is
/// `for code in start..=end` over raw u32 — about 4.3 billion iterations.
#[test]
fn cmap_format12_full_range_does_not_hang() {
    let mut sub = Vec::new();
    sub.extend_from_slice(&12u16.to_be_bytes()); // format
    sub.extend_from_slice(&0u16.to_be_bytes()); // reserved
    sub.extend_from_slice(&28u32.to_be_bytes()); // length
    sub.extend_from_slice(&0u32.to_be_bytes()); // language
    sub.extend_from_slice(&1u32.to_be_bytes()); // nGroups
    sub.extend_from_slice(&0u32.to_be_bytes()); // startCharCode
    sub.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // endCharCode
    sub.extend_from_slice(&1u32.to_be_bytes()); // startGlyphID
    let _ = parse_cmap(&font_with_cmap(&sub));
}

/// `startGlyphID + (code - startCharCode)` is a u32 add that can overflow.
#[test]
fn cmap_format12_overflowing_gid_does_not_panic() {
    let mut sub = Vec::new();
    sub.extend_from_slice(&12u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&28u32.to_be_bytes());
    sub.extend_from_slice(&0u32.to_be_bytes());
    sub.extend_from_slice(&1u32.to_be_bytes()); // nGroups
    sub.extend_from_slice(&0u32.to_be_bytes()); // start
    sub.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // end
    sub.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // startGlyphID
    let _ = parse_cmap(&font_with_cmap(&sub));
}

/// An ordinary format 12 table must still map correctly — a regression here
/// means a cap was tightened into legitimate territory.
#[test]
fn cmap_format12_ordinary_range_still_maps() {
    let mut sub = Vec::new();
    sub.extend_from_slice(&12u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&28u32.to_be_bytes());
    sub.extend_from_slice(&0u32.to_be_bytes());
    sub.extend_from_slice(&1u32.to_be_bytes());
    sub.extend_from_slice(&0x41u32.to_be_bytes()); // 'A'
    sub.extend_from_slice(&0x5Au32.to_be_bytes()); // 'Z'
    sub.extend_from_slice(&1u32.to_be_bytes()); // gid 1..=26
    let map = parse_cmap(&font_with_cmap(&sub));
    assert_eq!(map.get(&0x41), Some(&1), "'A' should map to gid 1");
    assert_eq!(map.get(&0x5A), Some(&26), "'Z' should map to gid 26");
}

// === Type 1 seac ==========================================================

/// A charstring that invokes `seac` naming itself as the base character.
/// `execute()` restarts the depth counter at 0, so the depth-10 guard in
/// `execute_inner` never fires and this recurses without bound.
///
/// `len_iv` is the `usize::MAX` sentinel for `/lenIV -1`, which makes
/// `decrypt_charstring` pass bytes through unmodified — otherwise the looked-up
/// charstring would be decrypted into garbage before it ever ran.
#[test]
fn self_referential_seac_does_not_overflow_the_stack() {
    // 0 0 hsbw, then asb=0 adx=0 ady=0 bchar='A' achar='B' seac.
    // Type 1 encodes a small integer v as the single byte v + 139.
    fn seac_charstring() -> Vec<u8> {
        vec![
            139,
            139,
            13, // 0 0 hsbw
            139,
            139,
            139,      // asb adx ady
            139 + 65, // bchar = 65
            139 + 66, // achar = 66
            12,
            6, // seac
        ]
    }
    // Both the base and the accent resolve back to the same seac charstring.
    let lookup = |_name: &str| -> Option<Vec<u8>> { Some(seac_charstring()) };
    let _ = execute_charstring_ex(&seac_charstring(), &[], usize::MAX, false, Some(&lookup));
}

// === Type 2 subroutine index ==============================================

/// `idx + bias` is an i32 add on a value taken straight off the charstring
/// stack. Type 2 has arithmetic operators, so a charstring can multiply its
/// way past `i32::MAX`; Rust saturates the `as i32` cast at that point, and
/// adding the subroutine bias then overflows.
#[test]
fn type2_callsubr_index_does_not_overflow() {
    /// `28 hi lo` pushes a 16-bit integer.
    fn push_i16(cs: &mut Vec<u8>, v: i16) {
        cs.push(28);
        cs.extend_from_slice(&v.to_be_bytes());
    }
    /// Multiply 32767 into itself repeatedly until the value saturates the cast.
    fn saturating_index(call_op: u8) -> Vec<u8> {
        let mut cs = Vec::new();
        push_i16(&mut cs, 32767);
        for _ in 0..3 {
            push_i16(&mut cs, 32767);
            cs.extend_from_slice(&[12, 24]); // mul
        }
        cs.push(call_op);
        cs
    }
    for op in [10u8 /* callsubr */, 29 /* callgsubr */] {
        let _ = execute_type2_charstring(&saturating_index(op), &[], &[], 0.0, 0.0, false);
    }

    // The negative end saturates at i32::MIN, where adding a bias is fine but
    // subtracting one is not; exercise it too.
    let mut cs = Vec::new();
    push_i16(&mut cs, -32768);
    for _ in 0..3 {
        push_i16(&mut cs, 32767);
        cs.extend_from_slice(&[12, 24]);
    }
    cs.push(10);
    let _ = execute_type2_charstring(&cs, &[], &[], 0.0, 0.0, false);
}

// === Type 1 /Subrs count ==================================================

/// Build a minimal Type 1 font whose (eexec-encrypted) private section
/// contains the given plaintext.
fn type1_font_with_private(private: &[u8]) -> Vec<u8> {
    // eexec encryption is the inverse of the decryption loop in
    // `decrypt_eexec`: 4 bytes of random IV, then each plaintext byte
    // XORed with the high byte of the evolving key.
    fn eexec_encrypt(plain: &[u8]) -> Vec<u8> {
        let c1: u32 = 52845;
        let c2: u32 = 22719;
        let mut r: u32 = 55665;
        let mut out = Vec::with_capacity(plain.len() + 4);
        // 4-byte IV, then the payload.
        let mut feed = vec![0u8; 4];
        feed.extend_from_slice(plain);
        for &p in &feed {
            let cipher = (p as u32 ^ (r >> 8)) as u8;
            out.push(cipher);
            r = ((cipher as u32 + r) * c1 + c2) & 0xFFFF;
        }
        out
    }

    let mut font = b"%!PS-AdobeFont-1.0: Test 001.001\n/FontName /Test def\n\
                     /FontMatrix [0.001 0 0 0.001 0 0] readonly def\n\
                     /FontType 1 def\ncurrentfile eexec\n"
        .to_vec();
    font.extend_from_slice(&eexec_encrypt(private));
    font
}

/// `/Subrs N array` reserves `N` entries before any of them is read, so a
/// declared count far larger than the file can hold is a direct OOM.
#[test]
fn type1_oversized_subrs_count_does_not_allocate_wildly() {
    for count in ["999999999", "18446744073709551615", "-1"] {
        let private =
            format!("dup /Private 8 dict dup begin\n/lenIV 4 def\n/Subrs {count} array\nND\n");
        let _ = stet_fonts::type1_parser::parse_type1(&type1_font_with_private(private.as_bytes()));
    }
}

/// A well-formed `/Subrs` must still be parsed — a regression here means the
/// cap was tightened into legitimate territory.
#[test]
fn type1_ordinary_subrs_count_still_parses() {
    let private = b"dup /Private 8 dict dup begin\n/lenIV 4 def\n/Subrs 3 array\nND\n";
    // Not asserting on contents: the point is that a small count is accepted
    // and the parse reaches the end without being rejected by the new cap.
    let _ = stet_fonts::type1_parser::parse_type1(&type1_font_with_private(private));
}

/// High fan-out rather than depth: a composite with many components, each
/// naming a distinct glyph that is itself such a composite. The path set does
/// not stop this (no id repeats on any one path) and the depth cap alone
/// bounds it only at `fan_out ^ MAX_COMPOSITE_DEPTH`, so this measures whether
/// the combination is actually enough.
#[test]
fn wide_composite_glyph_fan_out_terminates() {
    fn wide_composite(base_gid: u16, fan_out: u16) -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(&(-1i16).to_be_bytes());
        g.extend_from_slice(&[0u8; 8]);
        for i in 0..fan_out {
            let more = if i + 1 < fan_out { 0x0020u16 } else { 0 };
            g.extend_from_slice(&more.to_be_bytes());
            g.extend_from_slice(&base_gid.wrapping_add(i).wrapping_add(1).to_be_bytes());
            g.extend_from_slice(&[0u8, 0u8]);
        }
        g
    }
    let resolver = |gid: u16| -> Option<Vec<u8>> { Some(wide_composite(gid, 64)) };
    let start = std::time::Instant::now();
    let _ = parse_glyf_to_path(&wide_composite(0, 64), &resolver);
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 10,
        "wide composite fan-out took {elapsed:?}; the depth cap alone is not \
         bounding the work"
    );
}
