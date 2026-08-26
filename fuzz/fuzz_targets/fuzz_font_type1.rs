// Parse arbitrary bytes as a Type 1 font program, then run its charstrings.
//
// Covers the eexec decryption, the PostScript-ish header parse (`/Subrs`,
// `/CharStrings`, `/Encoding`, `/FontMatrix`), and the Type 1 charstring
// interpreter — including `seac`, whose composite lookup re-enters the
// interpreter and needs the charstring table to be reachable, so the lookup
// is wired up rather than passed as `None`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stet_fonts::{charstring, type1_parser};

fuzz_target!(|data: &[u8]| {
    let Ok(font) = type1_parser::parse_type1(data) else {
        return;
    };
    let lookup = |name: &str| font.charstrings.get(name).cloned();
    for cs in font.charstrings.values().take(32) {
        let decrypted = charstring::decrypt_charstring(cs, font.len_iv);
        let _ = charstring::execute_charstring_ex(
            &decrypted,
            &font.subrs,
            font.len_iv,
            false,
            Some(&lookup),
        );
    }
});
