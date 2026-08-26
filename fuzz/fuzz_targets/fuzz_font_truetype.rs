// Parse arbitrary bytes as a TrueType/OpenType font.
//
// Exercises the table directory walk, `cmap` (all supported subtable
// formats), `hmtx`, `loca`/`glyf`, and the composite-glyph recursion. The
// glyph loop is capped: this target is looking for a crash in the parser, not
// measuring how long a font with 65535 glyphs takes to walk.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stet_fonts::truetype;

fuzz_target!(|data: &[u8]| {
    let _ = truetype::get_units_per_em(data);
    let _ = truetype::find_table(data, b"glyf");
    let _ = truetype::parse_cmap(data);

    let num_glyphs = truetype::get_num_glyphs(data);
    let resolver = |gid: u16| truetype::get_glyf_data(data, gid);
    for gid in 0..num_glyphs.min(32) as u16 {
        let _ = truetype::get_advance_width(data, gid);
        if let Some(glyf) = truetype::get_glyf_data(data, gid) {
            let _ = truetype::parse_glyf_to_path(&glyf, &resolver);
        }
    }
});
