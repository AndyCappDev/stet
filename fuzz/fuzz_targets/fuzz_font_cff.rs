// Parse arbitrary bytes as a CFF (Type 1C / CIDFontType0C) font, then run its
// charstrings.
//
// Two surfaces in one: the INDEX/DICT structure parser, and the Type 2
// charstring interpreter that the parsed subroutine arrays feed. The
// interpreter is where subroutine recursion, the `idx + bias` arithmetic, and
// the operand stack live, so it is worth reaching rather than stopping at a
// successful parse.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stet_fonts::{cff_parser, type2_charstring};

fuzz_target!(|data: &[u8]| {
    let Ok(fonts) = cff_parser::parse_cff(data) else {
        return;
    };
    for font in fonts.iter().take(2) {
        for cs in font.char_strings.iter().take(32) {
            let _ = type2_charstring::execute_type2_charstring(
                cs,
                &font.local_subrs,
                &font.global_subrs,
                0.0,
                0.0,
                false,
            );
        }
    }
});
