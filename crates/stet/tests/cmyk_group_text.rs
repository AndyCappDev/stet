// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Text inside a CMYK transparency group does not change how the group
//! composites.
//!
//! A non-isolated `/CS /DeviceCMYK` group with an inversion-sensitive blend
//! mode composites in CMYK when everything it paints carries a CMYK colour.
//! PostScript `show` records a `Text` element beside the glyph fills, and
//! the check that decides the path treated that non-painting element as
//! non-CMYK content — so one line of text anywhere in the group sent the
//! whole group to sRGB blending, and its fills changed colour.
#![cfg(feature = "render")]

use stet::Interpreter;

/// Two identical Difference-blended CMYK groups over cyan; the second also
/// shows a character away from its square.
const GROUPS: &str = r#"%!PS
<< /PageSize [400 100] >> setpagedevice
/Helvetica findfont 10 scalefont setfont
1 0 0 0 setcmykcolor 0 0 400 100 rectfill
% A: fills only.
<< /Isolated false /CS /DeviceCMYK >> begintransparencygroup
  /Normal setblendmode
  0 1 0 0 setcmykcolor 10 10 80 80 rectfill
  /Difference setblendmode
endtransparencygroup
% B: the same, plus text away from the square.
/Normal setblendmode
<< /Isolated false /CS /DeviceCMYK >> begintransparencygroup
  /Normal setblendmode
  0 1 0 0 setcmykcolor 210 10 80 80 rectfill
  300 90 moveto (x) show
  /Difference setblendmode
endtransparencygroup
showpage
"#;

#[test]
fn text_does_not_switch_a_cmyk_group_to_srgb_blending() {
    let mut interp = Interpreter::new();
    let pages = interp.render(GROUPS.as_bytes(), 72.0).unwrap();
    let page = &pages[0];
    let pixel = |x: u32, y: u32| {
        let i = ((y * page.width + x) * 4) as usize;
        [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
    };
    let without_text = pixel(50, 50);
    let with_text = pixel(250, 50);
    assert_eq!(with_text, without_text);
}
