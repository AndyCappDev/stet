// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output writes what a pattern tile contains: its groups, soft masks,
//! layers and nested patterns, each named in the tile's own resources.
//!
//! A tile's resources were written from a partial copy of the page's, which
//! left out forms, soft masks, layers, nested patterns and the colour spaces
//! of uncoloured ones. A group in a tile drew nothing, a soft mask drew its
//! content unmasked, a nested pattern vanished and an uncoloured one painted
//! black. Forms named no resources at all, relying on the PDF 1.1 rule that
//! a form takes the page's — which inside a tile is not where its resources
//! are.
#![cfg(feature = "pdf-output")]

use stet::{DisplayElement, Interpreter, PsDisplayList};
use stet_fonts::geometry::PathSegment;
use stet_pdf_reader::{PdfDict, PdfDocument, PdfObj};

/// The job's PDF.
fn pdf(job: &str) -> Vec<u8> {
    Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap()
}

/// Every operand-and-operator pair in a content stream that names a
/// resource, as `(category, name)`.
fn named_resources(content: &[u8]) -> Vec<(&'static str, String)> {
    let text = String::from_utf8_lossy(content);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, op) in tokens.iter().enumerate() {
        let name = |back: usize| {
            i.checked_sub(back)
                .and_then(|j| tokens[j].strip_prefix('/'))
                .map(str::to_string)
        };
        let category = match *op {
            "Do" => "XObject",
            "gs" => "ExtGState",
            "scn" | "SCN" => "Pattern",
            "cs" | "CS" => "ColorSpace",
            "BDC" => "Properties",
            _ => continue,
        };
        let Some(n) = name(1) else { continue };
        if category == "ColorSpace"
            && matches!(
                n.as_str(),
                "Pattern" | "DeviceGray" | "DeviceRGB" | "DeviceCMYK"
            )
        {
            continue;
        }
        if category == "Properties" && name(2).as_deref() != Some("OC") {
            continue;
        }
        out.push((category, n));
    }
    out
}

/// Every pattern and form in the PDF names its resources, and every
/// resource its content uses is in them. Every soft-mask ExtGState carries
/// its mask, and every layer a stream marks is in the document's
/// `/OCProperties`. Returns the number of patterns and forms checked.
fn check_resources(bytes: &[u8]) -> (usize, usize) {
    let doc = PdfDocument::from_bytes(bytes).unwrap();
    let r = doc.resolver();
    let deref_dict = |o: &PdfObj| -> PdfDict {
        match r.deref(o).unwrap() {
            PdfObj::Dict(d) => d,
            other => panic!("expected a dict, got {other:?}"),
        }
    };
    let catalog = deref_dict(r.trailer().get(b"Root").unwrap());
    let ocgs: Vec<(u32, u16)> = catalog
        .get(b"OCProperties")
        .map(&deref_dict)
        .and_then(|p| p.get_array(b"OCGs").map(|a| a.to_vec()))
        .unwrap_or_default()
        .iter()
        .filter_map(PdfObj::as_ref)
        .collect();
    let (mut patterns, mut forms) = (0, 0);
    for num in 1..r.xref_len() as u32 {
        let Ok(obj) = r.resolve(num, 0) else { continue };
        let PdfObj::Stream { dict, .. } = &obj else {
            continue;
        };
        let is_pattern = dict.get_name(b"Type") == Some(b"Pattern");
        let is_form = dict.get_name(b"Subtype") == Some(b"Form");
        if !is_pattern && !is_form {
            continue;
        }
        if is_pattern {
            patterns += 1;
        } else {
            forms += 1;
        }
        let what = if is_pattern { "pattern" } else { "form" };
        let res = dict
            .get(b"Resources")
            .map(&deref_dict)
            .unwrap_or_else(|| panic!("{what} {num} names no /Resources"));
        let content = r.stream_data(num, 0).unwrap();
        for (category, name) in named_resources(&content) {
            let found = res
                .get(category.as_bytes())
                .map(&deref_dict)
                .and_then(|d| d.get(name.as_bytes()).cloned());
            let Some(found) = found else {
                panic!("{what} {num} uses /{name} with no /{category} entry for it");
            };
            match category {
                "ExtGState" => {
                    let gs = deref_dict(&found);
                    assert!(
                        gs.entries().iter().any(|(k, _)| k != b"Type"),
                        "{what} {num}: /{name} is an empty ExtGState"
                    );
                }
                "Properties" => {
                    let ocg = found.as_ref().unwrap();
                    assert!(
                        ocgs.contains(&ocg),
                        "{what} {num}: layer /{name} is not in /OCProperties"
                    );
                }
                _ => {}
            }
        }
    }
    (patterns, forms)
}

/// Every content stream in the PDF — page, form and pattern — restores
/// only the states it saved.
fn assert_balanced(bytes: &[u8]) {
    let doc = PdfDocument::from_bytes(bytes).unwrap();
    let r = doc.resolver();
    let page_contents: Vec<u32> = (1..r.xref_len() as u32)
        .filter_map(|num| match r.resolve(num, 0) {
            Ok(PdfObj::Dict(d)) if d.get_name(b"Type") == Some(b"Page") => {
                d.get_ref(b"Contents").map(|(n, _)| n)
            }
            _ => None,
        })
        .collect();
    assert!(!page_contents.is_empty());
    let mut streams = 0;
    for num in 1..r.xref_len() as u32 {
        let Ok(PdfObj::Stream { dict, .. }) = r.resolve(num, 0) else {
            continue;
        };
        let content = dict.get_name(b"Type") == Some(b"Pattern")
            || dict.get_name(b"Subtype") == Some(b"Form")
            || page_contents.contains(&num);
        if !content {
            continue;
        }
        let Ok(data) = r.stream_data(num, 0) else {
            continue;
        };
        streams += 1;
        let mut depth = 0i32;
        for token in String::from_utf8_lossy(&data).split_whitespace() {
            match token {
                "q" => depth += 1,
                "Q" => {
                    depth -= 1;
                    assert!(depth >= 0, "stream {num} restores a state it never saved");
                }
                _ => {}
            }
        }
        assert_eq!(depth, 0, "stream {num} leaves states saved");
    }
    assert!(streams > 0);
}

/// The tree of containers and fills, with each fill's bounding box.
fn shape(list: &PsDisplayList, out: &mut Vec<String>) {
    for e in list.elements() {
        match e {
            DisplayElement::Fill { path, .. } => {
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for s in &path.segments {
                    if let PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) = s {
                        b = [b[0].min(*x), b[1].min(*y), b[2].max(*x), b[3].max(*y)];
                    }
                }
                out.push(format!(
                    "fill {:.1} {:.1} {:.1} {:.1}",
                    b[0], b[1], b[2], b[3]
                ));
            }
            DisplayElement::Group { elements, .. } => {
                out.push("group".into());
                shape(elements, out);
                out.push("end".into());
            }
            DisplayElement::OcgGroup { elements, .. } => {
                out.push("layer".into());
                shape(elements, out);
                out.push("end".into());
            }
            DisplayElement::SoftMasked { mask, content, .. } => {
                out.push("mask".into());
                shape(mask, out);
                out.push("content".into());
                shape(content, out);
                out.push("end".into());
            }
            DisplayElement::PatternFill { params } => {
                out.push(format!("pattern paint {}", params.paint_type));
                shape(&params.tile, out);
                out.push("end".into());
            }
            _ => {}
        }
    }
}

/// The job's PDF names its resources (see [`check_resources`]), and stet's
/// reader reads back the containers and fills the PostScript drew.
fn round_trips(job: &str) {
    let bytes = pdf(job);
    assert_balanced(&bytes);
    let (patterns, _) = check_resources(&bytes);
    assert!(patterns > 0, "the job draws no pattern");

    let ps = Interpreter::new()
        .render_to_display_list(job.as_bytes(), 72.0)
        .unwrap();
    let mut want = Vec::new();
    shape(&ps[0].display_list, &mut want);

    let doc = PdfDocument::from_bytes(&bytes).unwrap();
    let mut got = Vec::new();
    shape(&doc.render_page(0, 72.0).unwrap(), &mut got);
    assert_eq!(got, want);
}

const PAGE: &str = "%!PS\n<< /PageSize [300 300] >> setpagedevice\n";

/// `name` a pattern of a 100-unit cell drawn by `paint`, one cell per 120.
fn cell(name: &str, paint: &str) -> String {
    format!(
        "/{name} << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 100 100] \
         /XStep 120 /YStep 120 /PaintProc {{ pop {paint} }} >> matrix makepattern def\n"
    )
}

const FILL: &str = "setpattern 20 20 250 250 rectfill showpage\n";

#[test]
fn a_nested_pattern_is_written() {
    round_trips(&format!(
        "{PAGE}/inner << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] \
         /XStep 10 /YStep 10 /PaintProc {{ pop 1 0 0 setrgbcolor 0 0 5 5 rectfill }} >> def\n\
         {}outer {FILL}",
        cell(
            "outer",
            "inner matrix makepattern setpattern 0 0 100 100 rectfill"
        )
    ));
}

#[test]
fn an_uncoloured_nested_pattern_is_written_with_its_colour_space() {
    round_trips(&format!(
        "{PAGE}/inner << /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 10 10] \
         /XStep 10 /YStep 10 /PaintProc {{ pop 0 0 5 5 rectfill }} >> def\n\
         {}outer {FILL}",
        cell(
            "outer",
            "[/Pattern /DeviceRGB] setcolorspace 0 0.5 0 inner matrix makepattern setcolor \
             0 0 100 100 rectfill"
        )
    ));
}

#[test]
fn a_group_in_a_tile_is_written() {
    round_trips(&format!(
        "{PAGE}{}outer {FILL}",
        cell(
            "outer",
            "<< /Isolated true >> begintransparencygroup 0 0 1 setrgbcolor 10 10 60 60 rectfill \
             1 0 0 setrgbcolor 40 40 50 50 rectfill endtransparencygroup"
        )
    ));
}

#[test]
fn a_soft_mask_in_a_tile_is_written_with_its_mask() {
    round_trips(&format!(
        "{PAGE}{}outer {FILL}",
        cell(
            "outer",
            "<< /Subtype /Luminosity /BBox [0 0 100 100] >> beginsoftmask \
             1 setgray 0 0 50 100 rectfill endsoftmask \
             0 0.6 0 setrgbcolor 0 0 100 100 rectfill clearsoftmask"
        )
    ));
}

#[test]
fn a_layer_in_a_tile_is_written_and_declared() {
    round_trips(&format!(
        "{PAGE}/L << /Name (Tiles) >> defineocg def\n{}outer {FILL}",
        cell(
            "outer",
            "L beginoptionalcontent 0.8 0.4 0 setrgbcolor 0 0 100 100 rectfill \
             endoptionalcontent"
        )
    ));
}

/// Containers and patterns three tiles deep: a group holding a nested
/// pattern whose own tile holds a soft mask.
#[test]
fn containers_nest_through_tiles() {
    round_trips(&format!(
        "{PAGE}{}/mid << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 20 20] \
         /XStep 20 /YStep 20 /PaintProc {{ pop inner setpattern 0 0 20 20 rectfill }} >> \
         matrix makepattern def\n{}outer {FILL}",
        cell(
            "inner",
            "<< /Subtype /Alpha /BBox [0 0 100 100] >> beginsoftmask \
             0 setgray 0 0 50 100 rectfill endsoftmask \
             0 0 1 setrgbcolor 0 0 100 100 rectfill clearsoftmask"
        ),
        cell(
            "outer",
            "<< >> begintransparencygroup mid setpattern 0 0 100 100 rectfill \
             endtransparencygroup"
        )
    ));
}

/// A form on the page names its resources too, rather than relying on the
/// page's.
#[test]
fn page_level_forms_name_their_resources() {
    let bytes = pdf(&format!(
        "{PAGE}<< >> begintransparencygroup 0 0 1 setrgbcolor 10 10 60 60 rectfill \
         << /Subtype /Alpha /BBox [0 0 300 300] >> beginsoftmask \
         0 setgray 0 0 150 300 rectfill endsoftmask \
         1 0 0 setrgbcolor 40 40 50 50 rectfill clearsoftmask \
         endtransparencygroup showpage\n"
    ));
    let (_, forms) = check_resources(&bytes);
    assert_eq!(forms, 2, "the group and the mask");
}

/// A clip restored inside a group, while the page has one open: the
/// group's clip opens its own `q` and its restore closes that one, not the
/// page's. The group's Form opened none, so the page's `Q` landed in it.
#[test]
fn a_form_keeps_to_its_own_clips() {
    let job = format!(
        "{PAGE}gsave 0 0 200 200 rectclip << >> begintransparencygroup \
         gsave 10 10 50 50 rectclip 0 0 1 setrgbcolor 0 0 100 100 rectfill grestore \
         1 0 0 setrgbcolor 40 40 100 100 rectfill endtransparencygroup grestore \
         0 1 0 setrgbcolor 250 250 20 20 rectfill showpage\n"
    );
    assert_balanced(&pdf(&job));
}

/// Every pattern fill's matrix, pattern space to device space, at every
/// depth.
fn pattern_matrices(list: &PsDisplayList, out: &mut Vec<[f64; 6]>) {
    for e in list.elements() {
        match e {
            DisplayElement::PatternFill { params } => {
                let m = &params.pattern_matrix;
                out.push([m.a, m.b, m.c, m.d, m.tx, m.ty]);
                pattern_matrices(&params.tile, out);
            }
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                pattern_matrices(elements, out)
            }
            DisplayElement::SoftMasked { mask, content, .. } => {
                pattern_matrices(mask, out);
                pattern_matrices(content, out);
            }
            _ => {}
        }
    }
}

/// The job's PDF places its patterns where the PostScript does. A
/// pattern's matrix maps to the space of the content stream it is painted
/// in, which for a form is the form's (ISO 32000-1 § 8.7.3.1).
fn patterns_placed(job: &str) {
    let ps = Interpreter::new()
        .render_to_display_list(job.as_bytes(), 72.0)
        .unwrap();
    let mut want = Vec::new();
    pattern_matrices(&ps[0].display_list, &mut want);
    assert!(!want.is_empty());

    let bytes = pdf(job);
    let doc = PdfDocument::from_bytes(&bytes).unwrap();
    let mut got = Vec::new();
    pattern_matrices(&doc.render_page(0, 72.0).unwrap(), &mut got);
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert!(
            g.iter().zip(w).all(|(a, b)| (a - b).abs() < 1e-3),
            "PDF {g:?}, PS {w:?}"
        );
    }
}

/// A pattern filled inside a group was placed by the page's transform
/// twice: flipped and shifted at 72 dpi, and shrunk as well above it.
#[test]
fn a_pattern_in_a_group_is_placed_in_the_groups_space() {
    patterns_placed(&format!(
        "{PAGE}{}<< >> begintransparencygroup p setpattern 20 20 200 200 rectfill \
         endtransparencygroup showpage\n",
        cell("p", "0 0 1 setrgbcolor 0 0 60 60 rectfill")
    ));
}

/// The pattern objects the page's content selects, and those its forms'
/// content selects.
fn pattern_objects(bytes: &[u8]) -> (Vec<u32>, Vec<u32>) {
    let doc = PdfDocument::from_bytes(bytes).unwrap();
    let r = doc.resolver();
    let dict = |o: &PdfObj| match r.deref(o).unwrap() {
        PdfObj::Dict(d) => d,
        PdfObj::Stream { dict, .. } => dict,
        other => panic!("expected a dict, got {other:?}"),
    };
    let selected = |content: &[u8], resources: &PdfDict| -> Vec<u32> {
        let patterns = dict(resources.get(b"Pattern").unwrap());
        named_resources(content)
            .into_iter()
            .filter(|(category, _)| *category == "Pattern")
            .map(|(_, name)| patterns.get_ref(name.as_bytes()).unwrap().0)
            .collect()
    };
    let (mut page, mut forms) = (Vec::new(), Vec::new());
    for num in 1..r.xref_len() as u32 {
        match r.resolve(num, 0) {
            Ok(PdfObj::Dict(d)) if d.get_name(b"Type") == Some(b"Page") => {
                let (contents, _) = d.get_ref(b"Contents").unwrap();
                let res = dict(d.get(b"Resources").unwrap());
                page.extend(selected(&r.stream_data(contents, 0).unwrap(), &res));
            }
            Ok(PdfObj::Stream { dict: d, .. }) if d.get_name(b"Subtype") == Some(b"Form") => {
                let res = dict(d.get(b"Resources").unwrap());
                forms.extend(selected(&r.stream_data(num, 0).unwrap(), &res));
            }
            _ => {}
        }
    }
    (page, forms)
}

/// One pattern used on the page and inside a group and a soft mask needs a
/// pattern object for each space. Viewers disagree on one object shared
/// between them — Ghostscript, and stet's reader, keep its first
/// placement; poppler places it per stream — so the placement check alone
/// cannot see a shared one.
#[test]
fn a_pattern_used_on_the_page_and_in_forms_is_placed_in_each() {
    let job = format!(
        "{PAGE}{}p setpattern 0 0 50 50 rectfill \
         << >> begintransparencygroup p setpattern 60 60 100 100 rectfill \
         endtransparencygroup \
         << /Subtype /Luminosity /BBox [0 0 300 300] >> beginsoftmask \
         p setpattern 0 0 300 150 rectfill endsoftmask \
         0 setgray 170 170 100 100 rectfill clearsoftmask showpage\n",
        cell("p", "1 setgray 0 0 60 60 rectfill")
    );
    let (page, forms) = pattern_objects(&pdf(&job));
    assert!(!page.is_empty() && !forms.is_empty());
    assert!(
        page.iter().all(|p| !forms.contains(p)),
        "page {page:?} and forms {forms:?} share a pattern object"
    );
    patterns_placed(&job);
}

/// The colour operators in each uncoloured (PaintType 2) pattern's
/// content: there should be none, since such a tile takes its colour from
/// where the pattern is used (ISO 32000-1 § 8.7.3.3). poppler obeys one if
/// it is there, painting the pattern in the colour the PostScript happened
/// to set rather than the one it filled with.
fn uncoloured_tile_colours(bytes: &[u8]) -> Vec<(u32, String)> {
    let doc = PdfDocument::from_bytes(bytes).unwrap();
    let r = doc.resolver();
    let mut found = Vec::new();
    let mut tiles = 0;
    for num in 1..r.xref_len() as u32 {
        let Ok(PdfObj::Stream { dict, .. }) = r.resolve(num, 0) else {
            continue;
        };
        if dict.get_name(b"Type") != Some(b"Pattern") || dict.get_int(b"PaintType") != Some(2) {
            continue;
        }
        tiles += 1;
        let content = r.stream_data(num, 0).unwrap();
        for token in String::from_utf8_lossy(&content).split_whitespace() {
            if matches!(
                token,
                "g" | "rg" | "k" | "G" | "RG" | "K" | "cs" | "CS" | "sc" | "scn" | "SC" | "SCN"
            ) {
                found.push((num, token.to_string()));
            }
        }
    }
    assert!(tiles > 0, "the job draws no uncoloured pattern");
    found
}

/// An uncoloured pattern whose cell fills, strokes and shows text. Its
/// PaintProc may set no colour (PLRM), but each mark still carries the
/// colour current when the tile was captured, and that was written.
const UNCOLOURED: &str = "/u << /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 40 40] \
    /XStep 40 /YStep 40 /PaintProc { pop 0 0 10 10 rectfill \
    2 setlinewidth 15 15 10 10 rectstroke \
    /Courier findfont 12 scalefont setfont 2 28 moveto (Ab) show } >> def\n";

#[test]
fn an_uncoloured_pattern_sets_no_colour() {
    let job = format!(
        "{PAGE}{UNCOLOURED}[/Pattern /DeviceRGB] setcolorspace \
         0 0.5 0 u matrix makepattern setcolor 20 20 250 250 rectfill showpage\n"
    );
    assert_eq!(uncoloured_tile_colours(&pdf(&job)), vec![]);
    round_trips(&job);
}

#[test]
fn an_uncoloured_pattern_in_a_tile_sets_no_colour() {
    let job = format!(
        "{PAGE}{UNCOLOURED}{}outer {FILL}",
        cell(
            "outer",
            "[/Pattern /DeviceRGB] setcolorspace 0 0.5 0 u matrix makepattern setcolor \
             0 0 100 100 rectfill"
        )
    );
    assert_eq!(uncoloured_tile_colours(&pdf(&job)), vec![]);
    round_trips(&job);
}
