// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Adobe Glyph List (AGL) — glyph name → Unicode mapping.
//!
//! Used by both PostScript font handling and PDF font decoding.
//!
//! Two lookups serve two jobs. [`glyph_name_to_unicode`] returns one BMP
//! code point from [`GLYPH_TO_UNICODE`], a subset of the list tuned for
//! glyph selection, and is what rendering uses. [`glyph_name_to_text`] (and
//! [`zapf_dingbats_glyph_name_to_text`] for that font) implements the full
//! name-to-text algorithm of the AGL specification over the complete lists
//! — suffixes, ligature components, supplementary-plane code points — and
//! is what text extraction uses.
//!
//! The complete lists are Adobe's `glyphlist.txt` (AGL 2.0) and
//! `zapfdingbats.txt`, embedded verbatim with Adobe's BSD-3-Clause notice;
//! see also `LICENSE-ADOBE-AGL` at the root of this crate.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Map from Adobe glyph name to Unicode code point, for glyph selection.
///
/// A subset of the Adobe Glyph List covering the common names. It departs
/// from the list in two places, where glyph selection is better served by
/// the Greek letter than by the math symbol the AGL gives: `Delta` is
/// U+0394 (AGL: U+2206 INCREMENT) and `Omega` U+03A9 (AGL: U+2126 OHM SIGN).
pub static GLYPH_TO_UNICODE: LazyLock<HashMap<&'static str, u16>> = LazyLock::new(|| {
    let mut m = HashMap::with_capacity(600);

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

    // Greek uppercase
    m.insert("Alpha", 0x0391);
    m.insert("Beta", 0x0392);
    m.insert("Gamma", 0x0393);
    m.insert("Delta", 0x0394);
    m.insert("Epsilon", 0x0395);
    m.insert("Zeta", 0x0396);
    m.insert("Eta", 0x0397);
    m.insert("Theta", 0x0398);
    m.insert("Iota", 0x0399);
    m.insert("Kappa", 0x039A);
    m.insert("Lambda", 0x039B);
    m.insert("Mu", 0x039C);
    m.insert("Nu", 0x039D);
    m.insert("Xi", 0x039E);
    m.insert("Omicron", 0x039F);
    m.insert("Pi", 0x03A0);
    m.insert("Rho", 0x03A1);
    m.insert("Sigma", 0x03A3);
    m.insert("Tau", 0x03A4);
    m.insert("Upsilon", 0x03A5);
    m.insert("Phi", 0x03A6);
    m.insert("Chi", 0x03A7);
    m.insert("Psi", 0x03A8);
    m.insert("Omega", 0x03A9);

    // Greek lowercase
    m.insert("alpha", 0x03B1);
    m.insert("beta", 0x03B2);
    m.insert("gamma", 0x03B3);
    m.insert("delta", 0x03B4);
    m.insert("epsilon", 0x03B5);
    m.insert("zeta", 0x03B6);
    m.insert("eta", 0x03B7);
    m.insert("theta", 0x03B8);
    m.insert("iota", 0x03B9);
    m.insert("kappa", 0x03BA);
    m.insert("lambda", 0x03BB);
    // mu already mapped as 0x00B5 (micro sign)
    m.insert("nu", 0x03BD);
    m.insert("xi", 0x03BE);
    m.insert("omicron", 0x03BF);
    m.insert("pi", 0x03C0);
    m.insert("rho", 0x03C1);
    m.insert("sigma", 0x03C3);
    m.insert("tau", 0x03C4);
    m.insert("upsilon", 0x03C5);
    m.insert("phi", 0x03C6);
    m.insert("chi", 0x03C7);
    m.insert("psi", 0x03C8);
    m.insert("omega", 0x03C9);

    // Math symbols
    m.insert("partialdiff", 0x2202);
    m.insert("summation", 0x2211);
    m.insert("product", 0x220F);
    m.insert("radical", 0x221A);
    m.insert("infinity", 0x221E);
    m.insert("integral", 0x222B);
    m.insert("approxequal", 0x2248);
    m.insert("notequal", 0x2260);
    m.insert("lessequal", 0x2264);
    m.insert("greaterequal", 0x2265);

    // Miscellaneous
    m.insert("dotlessi", 0x0131);
    m.insert("florin", 0x0192);

    // Latin Extended (accented characters for European languages)
    m.insert("Amacron", 0x0100);
    m.insert("amacron", 0x0101);
    m.insert("Abreve", 0x0102);
    m.insert("abreve", 0x0103);
    m.insert("Aogonek", 0x0104);
    m.insert("aogonek", 0x0105);
    m.insert("Cacute", 0x0106);
    m.insert("cacute", 0x0107);
    m.insert("Ccircumflex", 0x0108);
    m.insert("ccircumflex", 0x0109);
    m.insert("Cdotaccent", 0x010A);
    m.insert("cdotaccent", 0x010B);
    m.insert("Ccaron", 0x010C);
    m.insert("ccaron", 0x010D);
    m.insert("Dcaron", 0x010E);
    m.insert("dcaron", 0x010F);
    m.insert("Dcroat", 0x0110);
    m.insert("dcroat", 0x0111);
    m.insert("Emacron", 0x0112);
    m.insert("emacron", 0x0113);
    m.insert("Ebreve", 0x0114);
    m.insert("ebreve", 0x0115);
    m.insert("Edotaccent", 0x0116);
    m.insert("edotaccent", 0x0117);
    m.insert("Eogonek", 0x0118);
    m.insert("eogonek", 0x0119);
    m.insert("Ecaron", 0x011A);
    m.insert("ecaron", 0x011B);
    m.insert("Gcircumflex", 0x011C);
    m.insert("gcircumflex", 0x011D);
    m.insert("Gbreve", 0x011E);
    m.insert("gbreve", 0x011F);
    m.insert("Gdotaccent", 0x0120);
    m.insert("gdotaccent", 0x0121);
    m.insert("Hcircumflex", 0x0124);
    m.insert("hcircumflex", 0x0125);
    m.insert("Hbar", 0x0126);
    m.insert("hbar", 0x0127);
    m.insert("Itilde", 0x0128);
    m.insert("itilde", 0x0129);
    m.insert("Imacron", 0x012A);
    m.insert("imacron", 0x012B);
    m.insert("Ibreve", 0x012C);
    m.insert("ibreve", 0x012D);
    m.insert("Iogonek", 0x012E);
    m.insert("iogonek", 0x012F);
    m.insert("Idotaccent", 0x0130);
    m.insert("IJ", 0x0132);
    m.insert("ij", 0x0133);
    m.insert("Jcircumflex", 0x0134);
    m.insert("jcircumflex", 0x0135);
    m.insert("kgreenlandic", 0x0138);
    m.insert("Lacute", 0x0139);
    m.insert("lacute", 0x013A);
    m.insert("Lcaron", 0x013D);
    m.insert("lcaron", 0x013E);
    m.insert("Ldot", 0x013F);
    m.insert("ldot", 0x0140);
    m.insert("Nacute", 0x0143);
    m.insert("nacute", 0x0144);
    m.insert("Ncaron", 0x0147);
    m.insert("ncaron", 0x0148);
    m.insert("napostrophe", 0x0149);
    m.insert("Eng", 0x014A);
    m.insert("eng", 0x014B);
    m.insert("Omacron", 0x014C);
    m.insert("omacron", 0x014D);
    m.insert("Obreve", 0x014E);
    m.insert("obreve", 0x014F);
    m.insert("Ohungarumlaut", 0x0150);
    m.insert("ohungarumlaut", 0x0151);
    m.insert("Racute", 0x0154);
    m.insert("racute", 0x0155);
    m.insert("Rcaron", 0x0158);
    m.insert("rcaron", 0x0159);
    m.insert("Sacute", 0x015A);
    m.insert("sacute", 0x015B);
    m.insert("Scircumflex", 0x015C);
    m.insert("scircumflex", 0x015D);
    m.insert("Scedilla", 0x015E);
    m.insert("scedilla", 0x015F);
    m.insert("Tcaron", 0x0164);
    m.insert("tcaron", 0x0165);
    m.insert("Tbar", 0x0166);
    m.insert("tbar", 0x0167);
    m.insert("Utilde", 0x0168);
    m.insert("utilde", 0x0169);
    m.insert("Umacron", 0x016A);
    m.insert("umacron", 0x016B);
    m.insert("Ubreve", 0x016C);
    m.insert("ubreve", 0x016D);
    m.insert("Uring", 0x016E);
    m.insert("uring", 0x016F);
    m.insert("Uhungarumlaut", 0x0170);
    m.insert("uhungarumlaut", 0x0171);
    m.insert("Uogonek", 0x0172);
    m.insert("uogonek", 0x0173);
    m.insert("Wcircumflex", 0x0174);
    m.insert("wcircumflex", 0x0175);
    m.insert("Ycircumflex", 0x0176);
    m.insert("ycircumflex", 0x0177);
    m.insert("Zacute", 0x0179);
    m.insert("zacute", 0x017A);
    m.insert("Zdotaccent", 0x017B);
    m.insert("zdotaccent", 0x017C);
    m.insert("longs", 0x017F);
    m.insert("Ohorn", 0x01A0);
    m.insert("ohorn", 0x01A1);
    m.insert("Uhorn", 0x01AF);
    m.insert("uhorn", 0x01B0);
    m.insert("Gcaron", 0x01E6);
    m.insert("gcaron", 0x01E7);
    m.insert("Aringacute", 0x01FA);
    m.insert("aringacute", 0x01FB);
    m.insert("AEacute", 0x01FC);
    m.insert("aeacute", 0x01FD);
    m.insert("Oslashacute", 0x01FE);
    m.insert("oslashacute", 0x01FF);

    // Combining diacritical marks
    m.insert("gravecomb", 0x0300);
    m.insert("acutecomb", 0x0301);
    m.insert("tildecomb", 0x0303);
    m.insert("hookabovecomb", 0x0309);
    m.insert("dotbelowcomb", 0x0323);

    // Greek extended (tonos variants)
    m.insert("tonos", 0x0384);
    m.insert("dieresistonos", 0x0385);
    m.insert("Alphatonos", 0x0386);
    m.insert("anoteleia", 0x0387);
    m.insert("Epsilontonos", 0x0388);
    m.insert("Etatonos", 0x0389);
    m.insert("Iotatonos", 0x038A);
    m.insert("Omicrontonos", 0x038C);
    m.insert("Upsilontonos", 0x038E);
    m.insert("Omegatonos", 0x038F);
    m.insert("iotadieresistonos", 0x0390);
    m.insert("Iotadieresis", 0x03AA);
    m.insert("Upsilondieresis", 0x03AB);
    m.insert("alphatonos", 0x03AC);
    m.insert("epsilontonos", 0x03AD);
    m.insert("etatonos", 0x03AE);
    m.insert("iotatonos", 0x03AF);
    m.insert("upsilondieresistonos", 0x03B0);
    m.insert("sigma1", 0x03C2);
    m.insert("iotadieresis", 0x03CA);
    m.insert("upsilondieresis", 0x03CB);
    m.insert("omicrontonos", 0x03CC);
    m.insert("upsilontonos", 0x03CD);
    m.insert("omegatonos", 0x03CE);
    m.insert("theta1", 0x03D1);
    m.insert("Upsilon1", 0x03D2);
    m.insert("phi1", 0x03D5);
    m.insert("omega1", 0x03D6);

    // Vietnamese
    m.insert("Wgrave", 0x1E80);
    m.insert("wgrave", 0x1E81);
    m.insert("Wacute", 0x1E82);
    m.insert("wacute", 0x1E83);
    m.insert("Wdieresis", 0x1E84);
    m.insert("wdieresis", 0x1E85);
    m.insert("Ygrave", 0x1EF2);
    m.insert("ygrave", 0x1EF3);

    // General punctuation and symbols
    m.insert("figuredash", 0x2012);
    m.insert("underscoredbl", 0x2017);
    m.insert("quotereversed", 0x201B);
    m.insert("onedotenleader", 0x2024);
    m.insert("twodotenleader", 0x2025);
    m.insert("minute", 0x2032);
    m.insert("second", 0x2033);
    m.insert("exclamdbl", 0x203C);
    m.insert("colonmonetary", 0x20A1);
    m.insert("franc", 0x20A3);
    m.insert("lira", 0x20A4);
    m.insert("peseta", 0x20A7);
    m.insert("dong", 0x20AB);
    m.insert("Ifraktur", 0x2111);
    m.insert("weierstrass", 0x2118);
    m.insert("Rfraktur", 0x211C);
    m.insert("prescription", 0x211E);
    m.insert("estimated", 0x212E);
    m.insert("aleph", 0x2135);
    m.insert("onethird", 0x2153);
    m.insert("twothirds", 0x2154);
    m.insert("oneeighth", 0x215B);
    m.insert("threeeighths", 0x215C);
    m.insert("fiveeighths", 0x215D);
    m.insert("seveneighths", 0x215E);

    // Arrows
    m.insert("arrowleft", 0x2190);
    m.insert("arrowup", 0x2191);
    m.insert("arrowright", 0x2192);
    m.insert("arrowdown", 0x2193);
    m.insert("arrowboth", 0x2194);
    m.insert("arrowupdn", 0x2195);
    m.insert("arrowupdnbse", 0x21A8);
    m.insert("carriagereturn", 0x21B5);
    m.insert("arrowdblleft", 0x21D0);
    m.insert("arrowdblup", 0x21D1);
    m.insert("arrowdblright", 0x21D2);
    m.insert("arrowdbldown", 0x21D3);
    m.insert("arrowdblboth", 0x21D4);

    // Mathematical operators
    m.insert("universal", 0x2200);
    m.insert("existential", 0x2203);
    m.insert("emptyset", 0x2205);
    m.insert("gradient", 0x2207);
    m.insert("element", 0x2208);
    m.insert("notelement", 0x2209);
    m.insert("suchthat", 0x220B);
    m.insert("asteriskmath", 0x2217);
    m.insert("proportional", 0x221D);
    m.insert("orthogonal", 0x221F);
    m.insert("angle", 0x2220);
    m.insert("logicaland", 0x2227);
    m.insert("logicalor", 0x2228);
    m.insert("intersection", 0x2229);
    m.insert("union", 0x222A);
    m.insert("therefore", 0x2234);
    m.insert("similar", 0x223C);
    m.insert("congruent", 0x2245);
    m.insert("equivalence", 0x2261);
    m.insert("propersubset", 0x2282);
    m.insert("propersuperset", 0x2283);
    m.insert("notsubset", 0x2284);
    m.insert("reflexsubset", 0x2286);
    m.insert("reflexsuperset", 0x2287);
    m.insert("circleplus", 0x2295);
    m.insert("circlemultiply", 0x2297);
    m.insert("perpendicular", 0x22A5);
    m.insert("dotmath", 0x22C5);

    // Box drawing and block elements
    m.insert("house", 0x2302);
    m.insert("revlogicalnot", 0x2310);
    m.insert("integraltp", 0x2320);
    m.insert("integralbt", 0x2321);
    m.insert("angleleft", 0x2329);
    m.insert("angleright", 0x232A);
    m.insert("SF100000", 0x2500);
    m.insert("SF110000", 0x2502);
    m.insert("SF010000", 0x250C);
    m.insert("SF030000", 0x2510);
    m.insert("SF020000", 0x2514);
    m.insert("SF040000", 0x2518);
    m.insert("SF080000", 0x251C);
    m.insert("SF090000", 0x2524);
    m.insert("SF060000", 0x252C);
    m.insert("SF070000", 0x2534);
    m.insert("SF050000", 0x253C);
    m.insert("SF430000", 0x2550);
    m.insert("SF240000", 0x2551);
    m.insert("SF510000", 0x2552);
    m.insert("SF520000", 0x2553);
    m.insert("SF390000", 0x2554);
    m.insert("SF220000", 0x2555);
    m.insert("SF210000", 0x2556);
    m.insert("SF250000", 0x2557);
    m.insert("SF500000", 0x2558);
    m.insert("SF490000", 0x2559);
    m.insert("SF380000", 0x255A);
    m.insert("SF280000", 0x255B);
    m.insert("SF270000", 0x255C);
    m.insert("SF260000", 0x255D);
    m.insert("SF360000", 0x255E);
    m.insert("SF370000", 0x255F);
    m.insert("SF420000", 0x2560);
    m.insert("SF190000", 0x2561);
    m.insert("SF200000", 0x2562);
    m.insert("SF230000", 0x2563);
    m.insert("SF470000", 0x2564);
    m.insert("SF480000", 0x2565);
    m.insert("SF410000", 0x2566);
    m.insert("SF450000", 0x2567);
    m.insert("SF460000", 0x2568);
    m.insert("SF400000", 0x2569);
    m.insert("SF540000", 0x256A);
    m.insert("SF530000", 0x256B);
    m.insert("SF440000", 0x256C);
    m.insert("upblock", 0x2580);
    m.insert("dnblock", 0x2584);
    m.insert("block", 0x2588);
    m.insert("lfblock", 0x258C);
    m.insert("rtblock", 0x2590);
    m.insert("ltshade", 0x2591);
    m.insert("shade", 0x2592);
    m.insert("dkshade", 0x2593);

    // Geometric shapes and misc symbols
    m.insert("filledbox", 0x25A0);
    m.insert("H22073", 0x25A1);
    m.insert("H18543", 0x25AA);
    m.insert("H18551", 0x25AB);
    m.insert("filledrect", 0x25AC);
    m.insert("triagup", 0x25B2);
    m.insert("triagrt", 0x25BA);
    m.insert("triagdn", 0x25BC);
    m.insert("triaglf", 0x25C4);
    m.insert("lozenge", 0x25CA);
    m.insert("circle", 0x25CB);
    m.insert("H18533", 0x25CF);
    m.insert("invbullet", 0x25D8);
    m.insert("invcircle", 0x25D9);
    m.insert("openbullet", 0x25E6);
    m.insert("smileface", 0x263A);
    m.insert("invsmileface", 0x263B);
    m.insert("sun", 0x263C);
    m.insert("female", 0x2640);
    m.insert("male", 0x2642);
    m.insert("spade", 0x2660);
    m.insert("club", 0x2663);
    m.insert("heart", 0x2665);
    m.insert("diamond", 0x2666);
    m.insert("musicalnote", 0x266A);
    m.insert("musicalnotedbl", 0x266B);

    m
});

/// Look up Unicode for a glyph name, handling uniXXXX and uXXXX conventions.
pub fn glyph_name_to_unicode(name: &str) -> Option<u16> {
    // Direct lookup
    if let Some(&cp) = GLYPH_TO_UNICODE.get(name) {
        return Some(cp);
    }

    // uniXXXX convention
    if name.starts_with("uni")
        && name.len() == 7
        && let Ok(cp) = u16::from_str_radix(&name[3..], 16)
    {
        return Some(cp);
    }

    // uXXXX convention
    if name.starts_with('u')
        && name.len() >= 5
        && name.len() <= 6
        && let Ok(cp) = u16::from_str_radix(&name[1..], 16)
    {
        return Some(cp);
    }

    None
}

/// Adobe Glyph List 2.0, verbatim.
const AGL_SOURCE: &str = include_str!("glyphlist.txt");
/// ITC Zapf Dingbats Glyph List, verbatim.
const ZAPF_DINGBATS_SOURCE: &str = include_str!("zapfdingbats.txt");

/// The full AGL: glyph name → text.
static AGL: LazyLock<HashMap<&'static str, Box<str>>> =
    LazyLock::new(|| parse_glyph_list(AGL_SOURCE));
/// The ZapfDingbats list (`a1` … `a191`): glyph name → text.
static ZAPF_DINGBATS: LazyLock<HashMap<&'static str, Box<str>>> =
    LazyLock::new(|| parse_glyph_list(ZAPF_DINGBATS_SOURCE));

/// Parse Adobe's `name;XXXX[ XXXX…]` list format, skipping `#` comments.
///
/// Entries whose value lies in the Private Use Area are left out. They are
/// Adobe's corporate-use assignments for small capitals, old-style figures
/// and the like (`Asmall` → U+F761), since retired from AGLFN; a PUA
/// character in extracted text displays as nothing meaningful and matches
/// nothing a user types, so such a name is treated as carrying no text.
fn parse_glyph_list(source: &'static str) -> HashMap<&'static str, Box<str>> {
    let mut map = HashMap::new();
    for line in source.lines() {
        if line.starts_with('#') {
            continue;
        }
        let Some((name, values)) = line.split_once(';') else {
            continue;
        };
        let text: Option<String> = values
            .split_whitespace()
            .map(|hex| {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .filter(|&cp| !is_private_use(cp))
                    .and_then(char::from_u32)
            })
            .collect();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            map.insert(name, text.into_boxed_str());
        }
    }
    map
}

fn is_private_use(cp: u32) -> bool {
    matches!(cp, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

/// Unicode text for a glyph name, by the algorithm of the Adobe Glyph List
/// specification ("Unicode and Glyph Names"), for text extraction.
///
/// - Everything from the first period is dropped: `a.sc` → `a`.
/// - The rest splits on underscores into components mapped in turn:
///   `f_f_i` → `ffi`.
/// - A component is looked up in the AGL, else read as `uniXXXX…` (one or
///   more groups of four hex digits, each a BMP scalar value), else as
///   `uXXXX` to `uXXXXXX` (one scalar value, which may be outside the BMP),
///   else maps to nothing.
///
/// Returns `None` when no component maps to anything, as for `.notdef` or
/// a name like `g123`: the name says nothing about the text, and guessing
/// would be worse than nothing for search.
///
/// Two deliberate differences from the specification. Hex digits may be
/// lowercase (the specification requires uppercase), matching what
/// [`glyph_name_to_unicode`] already accepts for glyph selection, so text
/// is extracted for every glyph name the renderer resolves. And AGL entries
/// in the Private Use Area give no text (see the list parser).
///
/// Names map as the AGL says, which is not always what
/// [`glyph_name_to_unicode`] picks for drawing: `Delta` is U+2206
/// INCREMENT, and ligature names give presentation forms (`fi` is U+FB01),
/// so a consumer matching against plain text should apply compatibility
/// normalization.
///
/// For glyphs of the ZapfDingbats font use
/// [`zapf_dingbats_glyph_name_to_text`], whose names (`a20`) mean nothing
/// in the AGL.
pub fn glyph_name_to_text(name: &str) -> Option<String> {
    name_to_text(name, false)
}

/// [`glyph_name_to_text`] for a glyph of the ZapfDingbats font: each
/// component is looked up in the ITC Zapf Dingbats Glyph List before the
/// AGL, as the specification directs, so `a20` is ✔ U+2714.
pub fn zapf_dingbats_glyph_name_to_text(name: &str) -> Option<String> {
    name_to_text(name, true)
}

fn name_to_text(name: &str, zapf_dingbats: bool) -> Option<String> {
    let base = name.split('.').next().unwrap_or("");
    let mut text = String::new();
    for component in base.split('_') {
        push_component_text(component, zapf_dingbats, &mut text);
    }
    (!text.is_empty()).then_some(text)
}

/// The character a glyph name outside the Adobe Glyph List spells as a
/// number, by the rule Poppler (after xpdf) applies to simple fonts: a
/// heuristic, for use only after [`glyph_name_to_text`] finds nothing. It is
/// there for the bitmap Type 3 fonts dvips writes (`a80` for code 80, `P`), and
/// fonts named like them: `nn` (two to four digits), `Ann` and `ABnn`
/// (decimal after one or two characters), and, when `hex`, `xx` and `Axx`
/// (two hex digits). The number is a Latin-1 code point; control
/// characters, which such a name gives for codes below 32 (`g12`), are
/// dropped rather than invented as text.
pub fn numeric_glyph_name_to_char(name: &str, hex: bool) -> Option<char> {
    let b = name.as_bytes();
    let n = b.len();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    // `atoi`: the leading decimal digits of `s`.
    let leading = |s: &str| -> Option<u32> {
        let end = s
            .bytes()
            .position(|c| !c.is_ascii_digit())
            .unwrap_or(s.len());
        s[..end].parse().ok()
    };
    let code = if hex
        && n == 3
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(u8::is_ascii_hexdigit)
    {
        u32::from_str_radix(&name[1..], 16).ok()
    } else if hex && n == 2 && b.iter().all(u8::is_ascii_hexdigit) {
        u32::from_str_radix(name, 16).ok()
    } else if !hex && (2..=4).contains(&n) && digit(0) && digit(1) {
        leading(name)
    } else if (3..=5).contains(&n) && digit(1) && digit(2) && name.is_char_boundary(1) {
        leading(&name[1..])
    } else if (4..=6).contains(&n) && digit(2) && digit(3) && name.is_char_boundary(2) {
        leading(&name[2..])
    } else {
        None
    }?;
    char::from_u32(code)
        .filter(|_| code <= 0xFF)
        .filter(|c| !c.is_control())
}

/// Whether a font's numeric glyph names, given all of its `names`, are
/// hexadecimal for [`numeric_glyph_name_to_char`]: some name is a letter and
/// two hex digits with at least one of those a letter (`a1f`), as Poppler
/// decides it.
pub fn glyph_names_are_hex<'a>(names: impl IntoIterator<Item = &'a str>) -> bool {
    names.into_iter().any(looks_hex)
}

/// A letter and two hex digits, at least one of them a letter.
fn looks_hex(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 3
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(u8::is_ascii_hexdigit)
        && b[1..].iter().any(u8::is_ascii_alphabetic)
}

/// Append the text of one underscore-separated glyph-name component.
fn push_component_text(component: &str, zapf_dingbats: bool, out: &mut String) {
    let listed = zapf_dingbats
        .then(|| ZAPF_DINGBATS.get(component))
        .flatten()
        .or_else(|| AGL.get(component));
    if let Some(text) = listed {
        out.push_str(text);
        return;
    }
    if let Some(hex) = component.strip_prefix("uni")
        && !hex.is_empty()
        && hex.len() % 4 == 0
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        // Each group must be a BMP scalar value; one surrogate and the
        // `uni` form does not apply to the component at all.
        let chars: Option<Vec<char>> = hex
            .as_bytes()
            .chunks(4)
            .map(|group| {
                let group = std::str::from_utf8(group).ok()?;
                char::from_u32(u32::from_str_radix(group, 16).ok()?)
            })
            .collect();
        if let Some(chars) = chars {
            out.extend(chars);
            return;
        }
    }
    if let Some(hex) = component.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
        && let Ok(v) = u32::from_str_radix(hex, 16)
        && let Some(c) = char::from_u32(v)
    {
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn numeric_glyph_names() {
        let dec = |name| numeric_glyph_name_to_char(name, false);
        // dvips bitmap fonts: `a` and the code.
        assert_eq!(dec("a80"), Some('P'));
        assert_eq!(dec("a116"), Some('t'));
        // `nn`, `Ann`, `ABnn`.
        assert_eq!(dec("65"), Some('A'));
        assert_eq!(dec("g65"), Some('A'));
        assert_eq!(dec("cs233"), Some('é'));
        assert_eq!(dec("G233x"), Some('é'));
        // Control characters and codes past Latin-1 give nothing.
        assert_eq!(dec("g12"), None);
        assert_eq!(dec("a300"), None);
        // Not numeric names.
        assert_eq!(dec("Aring"), None);
        assert_eq!(dec("cid12345"), None);
        assert_eq!(dec("a"), None);
        // Hex fonts: `xx` and `Axx`; decimal `nn` no longer applies.
        let hex = |name| numeric_glyph_name_to_char(name, true);
        assert_eq!(hex("a41"), Some('A'));
        assert_eq!(hex("41"), Some('A'));
        assert_eq!(hex("ce9"), Some('é'));
        assert_eq!(hex("123"), None);
        assert!(glyph_names_are_hex(["a80", "a1f"]));
        assert!(!glyph_names_are_hex(["a80", "a1g", "space"]));
    }

    use super::*;

    fn text(name: &str) -> Option<String> {
        glyph_name_to_text(name)
    }

    #[test]
    fn glyph_list_names() {
        assert_eq!(text("A").as_deref(), Some("A"));
        assert_eq!(text("space").as_deref(), Some(" "));
        assert_eq!(text("Aacute").as_deref(), Some("\u{C1}"));
        // Ligature names map to the glyph list's presentation form.
        assert_eq!(text("fi").as_deref(), Some("\u{FB01}"));
    }

    #[test]
    fn suffix_after_period_is_dropped() {
        assert_eq!(text("a.sc").as_deref(), Some("a"));
        assert_eq!(text("one.oldstyle.alt").as_deref(), Some("1"));
        assert_eq!(text("uni0041.ss01").as_deref(), Some("A"));
    }

    #[test]
    fn underscore_components_concatenate() {
        assert_eq!(text("f_f_i").as_deref(), Some("ffi"));
        assert_eq!(text("f_f_i.liga").as_deref(), Some("ffi"));
        assert_eq!(text("T_h").as_deref(), Some("Th"));
    }

    #[test]
    fn uni_form_with_several_code_points() {
        assert_eq!(text("uni0066006C").as_deref(), Some("fl"));
        assert_eq!(text("uni00e9").as_deref(), Some("\u{E9}"));
    }

    #[test]
    fn u_form_reaches_beyond_the_bmp() {
        assert_eq!(text("u1D400").as_deref(), Some("\u{1D400}"));
        assert_eq!(text("u0041").as_deref(), Some("A"));
        assert_eq!(text("u10FFFF").as_deref(), Some("\u{10FFFF}"));
    }

    #[test]
    fn specification_example() {
        // The worked example from the AGL specification: every rule at once.
        assert_eq!(
            text("Lcommaaccent_uni20AC0308_u1040C.alternate").as_deref(),
            Some("\u{13B}\u{20AC}\u{308}\u{1040C}")
        );
    }

    #[test]
    fn full_lists_load() {
        // 4281 entries in AGL 2.0, less the 192 in the Private Use Area.
        assert_eq!(AGL.len(), 4281 - 192);
        assert_eq!(ZAPF_DINGBATS.len(), 201);
    }

    #[test]
    fn names_beyond_the_rendering_subset() {
        assert_eq!(text("afii10017").as_deref(), Some("\u{410}"));
        assert_eq!(text("Gcommaaccent").as_deref(), Some("\u{122}"));
        // An AGL entry that is a sequence of code points.
        assert_eq!(text("dalethatafpatah").as_deref(), Some("\u{5D3}\u{5B2}"));
    }

    #[test]
    fn private_use_entries_give_no_text() {
        assert_eq!(text("Asmall"), None);
        assert_eq!(text("Asmall.sc"), None);
        // An explicit `uni` name in the PUA is the producer's statement.
        assert_eq!(text("uniF761").as_deref(), Some("\u{F761}"));
    }

    #[test]
    fn delta_and_omega_follow_the_agl() {
        assert_eq!(text("Delta").as_deref(), Some("\u{2206}"));
        assert_eq!(text("Omega").as_deref(), Some("\u{2126}"));
        assert_eq!(text("Deltagreek").as_deref(), Some("\u{394}"));
        // The glyph-selection lookup deliberately picks the Greek letters.
        assert_eq!(glyph_name_to_unicode("Delta"), Some(0x394));
    }

    #[test]
    fn zapf_dingbats_names_need_the_zapf_dingbats_lookup() {
        assert_eq!(text("a20"), None);
        assert_eq!(
            zapf_dingbats_glyph_name_to_text("a20").as_deref(),
            Some("\u{2714}")
        );
        assert_eq!(
            zapf_dingbats_glyph_name_to_text("a1").as_deref(),
            Some("\u{2701}")
        );
        // AGL names still resolve for that font.
        assert_eq!(
            zapf_dingbats_glyph_name_to_text("space").as_deref(),
            Some(" ")
        );
    }

    #[test]
    fn surrogates_and_out_of_range_values_map_to_nothing() {
        assert_eq!(text("uniD800"), None);
        assert_eq!(text("uni0041D800"), None);
        assert_eq!(text("uD800"), None);
        assert_eq!(text("u110000"), None);
    }

    #[test]
    fn meaningless_names_map_to_nothing() {
        assert_eq!(text(".notdef"), None);
        assert_eq!(text(""), None);
        assert_eq!(text("g123"), None);
        assert_eq!(text("glyph42"), None);
        // `uni` with a digit count that is not a multiple of four.
        assert_eq!(text("uni004"), None);
        // One unknown component does not discard the known ones.
        assert_eq!(text("f_xyzzy").as_deref(), Some("f"));
    }

    #[test]
    fn agrees_with_the_rendering_subset_except_delta_and_omega() {
        for (&name, &cp) in GLYPH_TO_UNICODE.iter() {
            if name == "Delta" || name == "Omega" {
                continue;
            }
            let expected = char::from_u32(cp as u32).map(String::from);
            assert_eq!(text(name), expected, "{name}");
        }
        for name in ["uni00E9", "uni00e9", "u0041", "u20AC"] {
            let narrow = glyph_name_to_unicode(name).and_then(|cp| char::from_u32(cp as u32));
            assert_eq!(text(name), narrow.map(String::from), "{name}");
        }
    }
}
