// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Glyph name → Unicode mapping for PDF ToUnicode CMaps.
//!
//! Covers the Adobe Glyph List (AGL) standard names plus common extras.
//!
//! Used by the font embedder for encoding-based ToUnicode mapping.
//! Will be wired up when Context-aware font embedding is added.


use std::collections::HashMap;
use std::sync::LazyLock;

/// Map from Adobe glyph name to Unicode code point.
pub static GLYPH_TO_UNICODE: LazyLock<HashMap<&'static str, u16>> = LazyLock::new(|| {
    let mut m = HashMap::with_capacity(512);

    // ASCII range
    m.insert("space", 0x0020);
    m.insert("exclam", 0x0021);
    m.insert("quotedbl", 0x0022);
    m.insert("numbersign", 0x0023);
    m.insert("dollar", 0x0024);
    m.insert("percent", 0x0025);
    m.insert("ampersand", 0x0026);
    m.insert("quotesingle", 0x0027);
    m.insert("parenleft", 0x0028);
    m.insert("parenright", 0x0029);
    m.insert("asterisk", 0x002A);
    m.insert("plus", 0x002B);
    m.insert("comma", 0x002C);
    m.insert("hyphen", 0x002D);
    m.insert("period", 0x002E);
    m.insert("slash", 0x002F);
    m.insert("zero", 0x0030);
    m.insert("one", 0x0031);
    m.insert("two", 0x0032);
    m.insert("three", 0x0033);
    m.insert("four", 0x0034);
    m.insert("five", 0x0035);
    m.insert("six", 0x0036);
    m.insert("seven", 0x0037);
    m.insert("eight", 0x0038);
    m.insert("nine", 0x0039);
    m.insert("colon", 0x003A);
    m.insert("semicolon", 0x003B);
    m.insert("less", 0x003C);
    m.insert("equal", 0x003D);
    m.insert("greater", 0x003E);
    m.insert("question", 0x003F);
    m.insert("at", 0x0040);
    for c in b'A'..=b'Z' {
        let name = (c as char).to_string();
        m.insert(Box::leak(name.into_boxed_str()), c as u16);
    }
    m.insert("bracketleft", 0x005B);
    m.insert("backslash", 0x005C);
    m.insert("bracketright", 0x005D);
    m.insert("asciicircum", 0x005E);
    m.insert("underscore", 0x005F);
    m.insert("grave", 0x0060);
    for c in b'a'..=b'z' {
        let name = (c as char).to_string();
        m.insert(Box::leak(name.into_boxed_str()), c as u16);
    }
    m.insert("braceleft", 0x007B);
    m.insert("bar", 0x007C);
    m.insert("braceright", 0x007D);
    m.insert("asciitilde", 0x007E);

    // Latin supplement
    m.insert("exclamdown", 0x00A1);
    m.insert("cent", 0x00A2);
    m.insert("sterling", 0x00A3);
    m.insert("currency", 0x00A4);
    m.insert("yen", 0x00A5);
    m.insert("brokenbar", 0x00A6);
    m.insert("section", 0x00A7);
    m.insert("dieresis", 0x00A8);
    m.insert("copyright", 0x00A9);
    m.insert("ordfeminine", 0x00AA);
    m.insert("guillemotleft", 0x00AB);
    m.insert("logicalnot", 0x00AC);
    m.insert("registered", 0x00AE);
    m.insert("macron", 0x00AF);
    m.insert("degree", 0x00B0);
    m.insert("plusminus", 0x00B1);
    m.insert("twosuperior", 0x00B2);
    m.insert("threesuperior", 0x00B3);
    m.insert("acute", 0x00B4);
    m.insert("mu", 0x00B5);
    m.insert("paragraph", 0x00B6);
    m.insert("periodcentered", 0x00B7);
    m.insert("cedilla", 0x00B8);
    m.insert("onesuperior", 0x00B9);
    m.insert("ordmasculine", 0x00BA);
    m.insert("guillemotright", 0x00BB);
    m.insert("onequarter", 0x00BC);
    m.insert("onehalf", 0x00BD);
    m.insert("threequarters", 0x00BE);
    m.insert("questiondown", 0x00BF);
    m.insert("Agrave", 0x00C0);
    m.insert("Aacute", 0x00C1);
    m.insert("Acircumflex", 0x00C2);
    m.insert("Atilde", 0x00C3);
    m.insert("Adieresis", 0x00C4);
    m.insert("Aring", 0x00C5);
    m.insert("AE", 0x00C6);
    m.insert("Ccedilla", 0x00C7);
    m.insert("Egrave", 0x00C8);
    m.insert("Eacute", 0x00C9);
    m.insert("Ecircumflex", 0x00CA);
    m.insert("Edieresis", 0x00CB);
    m.insert("Igrave", 0x00CC);
    m.insert("Iacute", 0x00CD);
    m.insert("Icircumflex", 0x00CE);
    m.insert("Idieresis", 0x00CF);
    m.insert("Eth", 0x00D0);
    m.insert("Ntilde", 0x00D1);
    m.insert("Ograve", 0x00D2);
    m.insert("Oacute", 0x00D3);
    m.insert("Ocircumflex", 0x00D4);
    m.insert("Otilde", 0x00D5);
    m.insert("Odieresis", 0x00D6);
    m.insert("multiply", 0x00D7);
    m.insert("Oslash", 0x00D8);
    m.insert("Ugrave", 0x00D9);
    m.insert("Uacute", 0x00DA);
    m.insert("Ucircumflex", 0x00DB);
    m.insert("Udieresis", 0x00DC);
    m.insert("Yacute", 0x00DD);
    m.insert("Thorn", 0x00DE);
    m.insert("germandbls", 0x00DF);
    m.insert("agrave", 0x00E0);
    m.insert("aacute", 0x00E1);
    m.insert("acircumflex", 0x00E2);
    m.insert("atilde", 0x00E3);
    m.insert("adieresis", 0x00E4);
    m.insert("aring", 0x00E5);
    m.insert("ae", 0x00E6);
    m.insert("ccedilla", 0x00E7);
    m.insert("egrave", 0x00E8);
    m.insert("eacute", 0x00E9);
    m.insert("ecircumflex", 0x00EA);
    m.insert("edieresis", 0x00EB);
    m.insert("igrave", 0x00EC);
    m.insert("iacute", 0x00ED);
    m.insert("icircumflex", 0x00EE);
    m.insert("idieresis", 0x00EF);
    m.insert("eth", 0x00F0);
    m.insert("ntilde", 0x00F1);
    m.insert("ograve", 0x00F2);
    m.insert("oacute", 0x00F3);
    m.insert("ocircumflex", 0x00F4);
    m.insert("otilde", 0x00F5);
    m.insert("odieresis", 0x00F6);
    m.insert("divide", 0x00F7);
    m.insert("oslash", 0x00F8);
    m.insert("ugrave", 0x00F9);
    m.insert("uacute", 0x00FA);
    m.insert("ucircumflex", 0x00FB);
    m.insert("udieresis", 0x00FC);
    m.insert("yacute", 0x00FD);
    m.insert("thorn", 0x00FE);
    m.insert("ydieresis", 0x00FF);

    // Common typographic
    m.insert("bullet", 0x2022);
    m.insert("ellipsis", 0x2026);
    m.insert("emdash", 0x2014);
    m.insert("endash", 0x2013);
    m.insert("fi", 0xFB01);
    m.insert("fl", 0xFB02);
    m.insert("ff", 0xFB00);
    m.insert("ffi", 0xFB03);
    m.insert("ffl", 0xFB04);
    m.insert("quotedblleft", 0x201C);
    m.insert("quotedblright", 0x201D);
    m.insert("quoteleft", 0x2018);
    m.insert("quoteright", 0x2019);
    m.insert("quotesinglbase", 0x201A);
    m.insert("quotedblbase", 0x201E);
    m.insert("dagger", 0x2020);
    m.insert("daggerdbl", 0x2021);
    m.insert("perthousand", 0x2030);
    m.insert("guilsinglleft", 0x2039);
    m.insert("guilsinglright", 0x203A);
    m.insert("trademark", 0x2122);
    m.insert("minus", 0x2212);
    m.insert("fraction", 0x2044);
    m.insert("Euro", 0x20AC);

    // Latin Extended-A
    m.insert("OE", 0x0152);
    m.insert("oe", 0x0153);
    m.insert("Scaron", 0x0160);
    m.insert("scaron", 0x0161);
    m.insert("Ydieresis", 0x0178);
    m.insert("Zcaron", 0x017D);
    m.insert("zcaron", 0x017E);
    m.insert("Lslash", 0x0141);
    m.insert("lslash", 0x0142);

    // Spacing modifiers / diacriticals
    m.insert("circumflex", 0x02C6);
    m.insert("tilde", 0x02DC);
    m.insert("caron", 0x02C7);
    m.insert("breve", 0x02D8);
    m.insert("dotaccent", 0x02D9);
    m.insert("ring", 0x02DA);
    m.insert("ogonek", 0x02DB);
    m.insert("hungarumlaut", 0x02DD);

    // Miscellaneous
    m.insert("dotlessi", 0x0131);
    m.insert("florin", 0x0192);

    m
});

/// Look up Unicode for a glyph name, handling uniXXXX and uXXXX conventions.
pub fn glyph_name_to_unicode(name: &str) -> Option<u16> {
    // Direct lookup
    if let Some(&cp) = GLYPH_TO_UNICODE.get(name) {
        return Some(cp);
    }

    // uniXXXX convention
    if name.starts_with("uni") && name.len() == 7 {
        if let Ok(cp) = u16::from_str_radix(&name[3..], 16) {
            return Some(cp);
        }
    }

    // uXXXX convention
    if name.starts_with('u') && name.len() >= 5 && name.len() <= 6 {
        if let Ok(cp) = u16::from_str_radix(&name[1..], 16) {
            return Some(cp);
        }
    }

    None
}
