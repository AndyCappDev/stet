// Regression tests for hostile or malformed PDF input.
//
// Three families, all found by the same audit: unbounded recursion, unbounded
// arithmetic on file-supplied integers, and unbounded allocation from
// file-supplied counts.
//
// Several of these abort the process when they regress rather than failing
// cleanly: a stack overflow is a `SIGSEGV`-backed abort, and an oversized
// `Vec` reservation aborts on allocation failure. Neither can be contained by
// `catch_unwind`, so a regression takes the whole test binary down instead of
// reporting one failed test. That is the intended signal.
//
// The bar is uniform: parsing and rendering must *return*, in bounded time,
// with or without an error. Except where a test asserts otherwise, nothing
// here asserts what gets drawn.

use stet_pdf_reader::PdfDocument;

/// Assemble a PDF from `(object number, body)` pairs with a correct xref.
fn build_pdf(objs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (num, body) in objs {
        offsets.insert(*num, out.len());
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let max = offsets.keys().copied().max().unwrap_or(0) + 1;
    out.extend_from_slice(format!("xref\n0 {max}\n0000000000 65535 f \n").as_bytes());
    for i in 1..max {
        let off = offsets.get(&i).copied().unwrap_or(0);
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<</Size {max}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn stream_obj(body: &[u8]) -> Vec<u8> {
    let mut v = format!("<</Length {}>>\nstream\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    v.extend_from_slice(b"endstream");
    v
}

/// A one-page document whose page dictionary is spliced in from `page_extra`,
/// carrying `contents` as its content stream plus any extra objects.
fn one_page_doc(page_extra: &[u8], contents: &[u8], extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<</Type/Catalog/Pages 2 0 R>>".to_vec()),
        (2, b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec()),
        (3, {
            let mut d = b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R".to_vec();
            d.extend_from_slice(page_extra);
            d.extend_from_slice(b">>");
            d
        }),
        (4, stream_obj(contents)),
    ];
    objs.extend_from_slice(extra);
    build_pdf(&objs)
}

/// Load, then walk every page. Returns without asserting on success: a
/// hostile file is allowed to fail, it just is not allowed to abort.
fn load_and_render(data: &[u8]) {
    if let Ok(doc) = PdfDocument::from_bytes(data) {
        for page in 0..doc.page_count() {
            let _ = doc.render_page(page, 72.0);
        }
    }
}

/// Deeply nested arrays in an object body — the shape from the `lopdf`
/// advisory RUSTSEC-2026-0187 and hayro #1347.
#[test]
fn deeply_nested_arrays_do_not_overflow_the_stack() {
    const DEPTH: usize = 200_000;
    let mut junk = b"[".repeat(DEPTH);
    junk.extend_from_slice(&b"]".repeat(DEPTH));

    let mut catalog = b"<</Type/Catalog/Pages 2 0 R/Junk ".to_vec();
    catalog.extend_from_slice(&junk);
    catalog.extend_from_slice(b">>");

    load_and_render(&build_pdf(&[
        (1, catalog),
        (2, b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec()),
        (
            3,
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]>>".to_vec(),
        ),
    ]));
}

/// The same, built from dictionaries rather than arrays — `parse_dict_body`
/// and `parse_object_from_token` are mutually recursive, so both directions
/// need the cap.
#[test]
fn deeply_nested_dictionaries_do_not_overflow_the_stack() {
    const DEPTH: usize = 200_000;
    let mut junk = b"<</A ".repeat(DEPTH);
    junk.extend_from_slice(b"0");
    junk.extend_from_slice(&b">>".repeat(DEPTH));

    let mut catalog = b"<</Type/Catalog/Pages 2 0 R/Junk ".to_vec();
    catalog.extend_from_slice(&junk);
    catalog.extend_from_slice(b">>");

    load_and_render(&build_pdf(&[
        (1, catalog),
        (2, b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec()),
        (
            3,
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]>>".to_vec(),
        ),
    ]));
}

/// Content streams use a separate operand parser (`parse_inline_array`) that
/// needed its own cap.
#[test]
fn deeply_nested_content_stream_array_does_not_overflow_the_stack() {
    const DEPTH: usize = 200_000;
    let mut cs = b"[".repeat(DEPTH);
    cs.extend_from_slice(&b"]".repeat(DEPTH));
    cs.extend_from_slice(b" TJ\n");

    load_and_render(&one_page_doc(b"", &cs, &[]));
}

/// Type 4 (PostScript calculator) functions recurse once per `{` body.
#[test]
fn deeply_nested_calculator_function_does_not_overflow_the_stack() {
    const DEPTH: usize = 200_000;
    let mut code = b"{".to_vec();
    code.extend_from_slice(&b"{".repeat(DEPTH));
    code.extend_from_slice(&b"}".repeat(DEPTH));
    code.extend_from_slice(b" }");

    load_and_render(&one_page_doc(
        b"/Resources<</Shading<</S1 5 0 R>>>>",
        b"/S1 sh\n",
        &[
            (
                5,
                b"<</ShadingType 2/ColorSpace/DeviceGray/Coords[0 0 100 100]/Function 6 0 R>>"
                    .to_vec(),
            ),
            (6, {
                let mut v = format!(
                    "<</FunctionType 4/Domain[0 1]/Range[0 1]/Length {}>>\nstream\n",
                    code.len()
                )
                .into_bytes();
                v.extend_from_slice(&code);
                v.extend_from_slice(b"\nendstream");
                v
            }),
        ],
    ));
}

fn stitching_fn(functions: &[u8]) -> Vec<u8> {
    let mut v = b"<</FunctionType 3/Domain[0 1]/Range[0 1]/Functions".to_vec();
    v.extend_from_slice(functions);
    v.extend_from_slice(b"/Bounds[]/Encode[0 1]>>");
    v
}

fn shading_page(extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![(
        5,
        b"<</ShadingType 2/ColorSpace/DeviceGray/Coords[0 0 100 100]/Function 6 0 R>>".to_vec(),
    )];
    objs.extend(extra);
    one_page_doc(b"/Resources<</Shading<</S1 5 0 R>>>>", b"/S1 sh\n", &objs)
}

/// A Type 3 stitching function listing itself in `/Functions`. This one is
/// worse than a depth bug: the recursion is infinite, so no file size is
/// needed and a depth cap alone would not be a correct fix — it needs cycle
/// detection.
#[test]
fn self_referential_stitching_function_does_not_overflow_the_stack() {
    load_and_render(&shading_page(vec![(6, stitching_fn(b"[6 0 R]"))]));
}

/// The same cycle spread over two objects, which a bare self-reference check
/// would miss.
#[test]
fn stitching_function_ring_does_not_overflow_the_stack() {
    load_and_render(&shading_page(vec![
        (6, stitching_fn(b"[7 0 R]")),
        (7, stitching_fn(b"[6 0 R]")),
    ]));
}

/// The cycle guard tracks the *current path*, not every object ever seen, so
/// naming one function twice in a single `/Functions` array stays legal.
/// Without the pop-on-exit this would be rejected as a false cycle.
#[test]
fn stitching_function_may_reference_one_child_twice() {
    let data = shading_page(vec![
        (
            6,
            b"<</FunctionType 3/Domain[0 1]/Range[0 1]/Functions[7 0 R 7 0 R]\
              /Bounds[0.5]/Encode[0 1 0 1]>>"
                .to_vec(),
        ),
        (
            7,
            b"<</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>".to_vec(),
        ),
    ]);

    let doc = PdfDocument::from_bytes(&data).expect("document should parse");
    let list = doc.render_page(0, 72.0).expect("page should render");
    assert!(
        !list.is_empty(),
        "a stitching function naming the same child twice is legal and must still draw"
    );
}

/// A Type 3 font whose CharProc shows its own glyph.
#[test]
fn self_referential_type3_glyph_does_not_overflow_the_stack() {
    let glyph = b"10 0 0 0 10 10 d1\nBT /F1 1 Tf (A) Tj ET\n";
    load_and_render(&one_page_doc(
        b"/Resources<</Font<</F1 5 0 R>>>>",
        b"BT /F1 12 Tf 10 50 Td (A) Tj ET\n",
        &[
            (
                5,
                b"<</Type/Font/Subtype/Type3/FontBBox[0 0 10 10]\
                  /FontMatrix[0.001 0 0 0.001 0 0]/CharProcs<</A 6 0 R>>\
                  /Encoding<</Type/Encoding/Differences[65/A]>>/FirstChar 65/LastChar 65\
                  /Widths[10]/Resources<</Font<</F1 5 0 R>>>>>>"
                    .to_vec(),
            ),
            (6, stream_obj(glyph)),
        ],
    ));
}

/// Two Type 3 fonts whose glyphs show each other.
#[test]
fn type3_glyph_ring_does_not_overflow_the_stack() {
    let t3 = |charproc: u32| -> Vec<u8> {
        format!(
            "<</Type/Font/Subtype/Type3/FontBBox[0 0 10 10]\
             /FontMatrix[0.001 0 0 0.001 0 0]/CharProcs<</A {charproc} 0 R>>\
             /Encoding<</Type/Encoding/Differences[65/A]>>/FirstChar 65/LastChar 65\
             /Widths[10]/Resources<</Font<</F1 5 0 R/F2 7 0 R>>>>>>"
        )
        .into_bytes()
    };
    load_and_render(&one_page_doc(
        b"/Resources<</Font<</F1 5 0 R/F2 7 0 R>>>>",
        b"BT /F1 12 Tf 10 50 Td (A) Tj ET\n",
        &[
            (5, t3(6)),
            (6, stream_obj(b"10 0 0 0 10 10 d1\nBT /F2 1 Tf (A) Tj ET\n")),
            (7, t3(8)),
            (8, stream_obj(b"10 0 0 0 10 10 d1\nBT /F1 1 Tf (A) Tj ET\n")),
        ],
    ));
}

/// A soft-mask group whose form re-selects the ExtGState that names it. The
/// mask form is interpreted directly rather than through the Form XObject
/// path, so it does not inherit that path's guard.
#[test]
fn self_referential_soft_mask_does_not_overflow_the_stack() {
    load_and_render(&one_page_doc(
        b"/Resources<</ExtGState<</G1 5 0 R>>/XObject<</X1 7 0 R>>>>",
        b"/G1 gs 0 0 100 100 re f\n",
        &[
            (
                5,
                b"<</Type/ExtGState/SMask<</S/Luminosity/G 7 0 R>>>>".to_vec(),
            ),
            (7, {
                let body = b"/G1 gs 0 0 100 100 re f\n";
                let mut v = format!(
                    "<</Type/XObject/Subtype/Form/BBox[0 0 100 100]\
                     /Group<</S/Transparency/CS/DeviceGray>>\
                     /Resources<</ExtGState<</G1 5 0 R>>>>/Length {}>>\nstream\n",
                    body.len()
                )
                .into_bytes();
                v.extend_from_slice(body);
                v.extend_from_slice(b"endstream");
                v
            }),
        ],
    ));
}

// === Image-dictionary integer validation ===================================
//
// `/Width`, `/Height`, and `/BitsPerComponent` are arbitrary file-supplied
// integers that every downstream buffer size and loop bound derives from.
// Before validation landed, `width * height` was computed in `u32` and
// wrapped: `65537 * 65536` is `2^32 + 65536`, so the product came back as
// 65536 — a buffer far smaller than the loops that fill it. In debug builds
// that is an "attempt to multiply with overflow" panic; in release it is a
// silently undersized allocation. Separately, the loop counts alone were a
// denial of service: these files spent 9-19 seconds in release.

/// Render a page and report whether any image element reached the display
/// list. An oversized image must be *dropped*, not drawn from a buffer whose
/// size disagrees with the loops that fill it.
fn renders_an_image(data: &[u8]) -> bool {
    use stet_graphics::display_list::DisplayElement;
    let Ok(doc) = PdfDocument::from_bytes(data) else {
        return false;
    };
    let Ok(list) = doc.render_page(0, 72.0) else {
        return false;
    };
    list.elements()
        .iter()
        .any(|e| matches!(e, DisplayElement::Image { .. }))
}

fn image_doc(width: &str, height: &str, bpc: &str, cs: &[u8]) -> Vec<u8> {
    // A tiny Flate payload — far smaller than the declared dimensions, which
    // is the point: the declared size drives allocation, not the actual data.
    let payload: &[u8] = &[
        0x78, 0x9c, 0x63, 0x60, 0x18, 0x05, 0xa3, 0x60, 0x14, 0x8c, 0x02, 0x08, 0x00, 0x00, 0x04,
        0x00, 0x00, 0x01,
    ];
    let mut xobj = format!(
        "<</Type/XObject/Subtype/Image/Width {width}/Height {height}\
         /BitsPerComponent {bpc}/ColorSpace "
    )
    .into_bytes();
    xobj.extend_from_slice(cs);
    xobj.extend_from_slice(
        format!("/Filter/FlateDecode/Length {}>>\nstream\n", payload.len()).as_bytes(),
    );
    xobj.extend_from_slice(payload);
    xobj.extend_from_slice(b"\nendstream");

    one_page_doc(
        b"/Resources<</XObject<</Im1 5 0 R>>>>",
        b"q 100 0 0 100 0 0 cm /Im1 Do Q\n",
        &[(5, xobj)],
    )
}

/// `width * height` overflowing `u32` to a *nonzero* remainder — the case
/// that yields an undersized buffer rather than an empty one.
#[test]
fn image_dimensions_that_overflow_u32_are_rejected() {
    assert!(
        !renders_an_image(&image_doc("65537", "65536", "8", b"/DeviceGray")),
        "65537 x 65536 overflows u32 to 65536; the image must be dropped"
    );
}

/// The same product landing exactly on `2^32`, i.e. wrapping to zero.
#[test]
fn image_dimensions_that_wrap_to_zero_are_rejected() {
    assert!(
        !renders_an_image(&image_doc("65536", "65536", "8", b"/DeviceGray")),
        "65536 x 65536 overflows u32 to 0; the image must be dropped"
    );
}

/// The Indexed path multiplies again when expanding palette indices.
#[test]
fn oversized_indexed_image_is_rejected() {
    assert!(
        !renders_an_image(&image_doc(
            "65537",
            "65536",
            "8",
            b"[/Indexed /DeviceRGB 1 <000000FFFFFF>]",
        )),
        "the Indexed expansion path must not receive an overflowing size"
    );
}

/// Sub-8 BPC reaches `expand_bits_to_bytes`, whose reservation is a
/// *three-way* product (`width * height * components`) and so overflows a
/// `u32` sooner than the two-way one.
#[test]
fn oversized_low_bpc_image_is_rejected() {
    assert!(
        !renders_an_image(&image_doc("65537", "65536", "4", b"/DeviceGray")),
        "the sub-8-bpc expansion path must not receive an overflowing size"
    );
}

/// `/Width` beyond `u32` used to be truncated by the `as u32` cast, turning
/// 4294967297 into a 1-pixel image rather than an error.
#[test]
fn image_dimension_beyond_u32_is_rejected_not_truncated() {
    assert!(
        !renders_an_image(&image_doc("4294967297", "4294967297", "8", b"/DeviceGray")),
        "4294967297 must be rejected, not truncated to a 1-pixel image"
    );
}

/// `/BitsPerComponent` reaches `1u32 << bpc`, which panics in debug builds
/// at 32 or more.
#[test]
fn out_of_range_bits_per_component_is_rejected() {
    assert!(
        !renders_an_image(&image_doc("8", "8", "99", b"/DeviceGray")),
        "/BitsPerComponent 99 reaches `1u32 << bpc` and must be rejected"
    );
}

/// Negative and zero dimensions must not become huge unsigned values.
#[test]
fn non_positive_image_dimensions_are_rejected() {
    assert!(!renders_an_image(&image_doc(
        "-1",
        "-1",
        "8",
        b"/DeviceGray"
    )));
    assert!(!renders_an_image(&image_doc("0", "0", "8", b"/DeviceGray")));
}

/// The largest image in the sample corpus is 34862x4332 (`issue16263.pdf`).
/// The caps must stay clear of anything real, so a dimension pair of that
/// order has to survive validation — a regression here means the bound was
/// tightened into legitimate territory.
#[test]
fn corpus_scale_image_dimensions_are_accepted() {
    let data = image_doc("34862", "4332", "8", b"/DeviceGray");
    assert!(
        renders_an_image(&data),
        "a 34862x4332 image is within real-world range and must still draw"
    );
}

// === Filter predictor parameters ==========================================
//
// `/Columns`, `/Colors`, and `/BitsPerComponent` in `/DecodeParms` were cast
// straight to `usize` and multiplied. Zero made `row_bytes` zero, which
// reaches `slice::chunks(0)` — "chunk size must be non-zero", a panic in
// *release* builds too, not just debug. Negative values became astronomical
// under the cast and produced multi-exabyte allocation aborts.

fn predictor_doc(parms: &[u8]) -> Vec<u8> {
    // Flate-compressed payload; the predictor is applied after inflation.
    let payload: &[u8] = &[
        0x78, 0x9c, 0x63, 0x60, 0x18, 0x05, 0xa3, 0x60, 0x14, 0x8c, 0x02, 0x08, 0x00, 0x00, 0x04,
        0x00, 0x00, 0x01,
    ];
    let mut xobj = b"<</Type/XObject/Subtype/Image/Width 16/Height 16/BitsPerComponent 8\
                     /ColorSpace/DeviceGray/Filter/FlateDecode/DecodeParms"
        .to_vec();
    xobj.extend_from_slice(parms);
    xobj.extend_from_slice(format!("/Length {}>>\nstream\n", payload.len()).as_bytes());
    xobj.extend_from_slice(payload);
    xobj.extend_from_slice(b"\nendstream");

    one_page_doc(
        b"/Resources<</XObject<</Im1 5 0 R>>>>",
        b"q 100 0 0 100 0 0 cm /Im1 Do Q\n",
        &[(5, xobj)],
    )
}

/// Zero `/Columns`, `/Colors`, or `/BitsPerComponent` all drive `row_bytes`
/// to zero and reach `chunks(0)`. These panicked in release builds.
#[test]
fn zero_predictor_parameters_do_not_panic() {
    for parms in [
        &b"<</Predictor 2/Columns 0/Colors 1/BitsPerComponent 8>>"[..],
        &b"<</Predictor 2/Columns 8/Colors 0/BitsPerComponent 8>>"[..],
        &b"<</Predictor 2/Columns 8/Colors 1/BitsPerComponent 0>>"[..],
        &b"<</Predictor 12/Columns 0/Colors 1/BitsPerComponent 8>>"[..],
    ] {
        load_and_render(&predictor_doc(parms));
    }
}

/// Negative parameters become enormous under `as usize`. In release these
/// reached a 2.3-exabyte reservation and aborted the process.
#[test]
fn negative_predictor_parameters_do_not_allocate_wildly() {
    for parms in [
        &b"<</Predictor 2/Columns -1/Colors 1/BitsPerComponent 8>>"[..],
        &b"<</Predictor 2/Columns 8/Colors -1/BitsPerComponent 8>>"[..],
        &b"<</Predictor 2/Columns 8/Colors 1/BitsPerComponent -1>>"[..],
        &b"<</Predictor 15/Columns -1/Colors 1/BitsPerComponent 8>>"[..],
        &b"<</Predictor 15/Columns 8/Colors -1/BitsPerComponent 8>>"[..],
    ] {
        load_and_render(&predictor_doc(parms));
    }
}

/// Values large enough to overflow the row-size product.
#[test]
fn overflowing_predictor_parameters_do_not_panic() {
    for parms in [
        &b"<</Predictor 2/Columns 4294967296/Colors 1/BitsPerComponent 8>>"[..],
        &b"<</Predictor 15/Columns 9223372036854775807/Colors 1/BitsPerComponent 8>>"[..],
        &b"<</Predictor 2/Columns 9223372036854775807/Colors 9223372036854775807\
           /BitsPerComponent 8>>"[..],
    ] {
        load_and_render(&predictor_doc(parms));
    }
}

/// The ordinary parameters real files use must keep working — a regression
/// here means the bounds were tightened into legitimate territory. All 691
/// sample PDFs were checked against these bounds and none is rejected.
#[test]
fn ordinary_predictor_parameters_still_apply() {
    for parms in [
        &b"<</Predictor 12/Columns 16/Colors 1/BitsPerComponent 8>>"[..],
        &b"<</Predictor 2/Columns 16/Colors 3/BitsPerComponent 8>>"[..],
        &b"<</Predictor 15/Columns 16/Colors 4/BitsPerComponent 8>>"[..],
        &b"<</Predictor 2/Columns 16/Colors 1/BitsPerComponent 4>>"[..],
    ] {
        assert!(
            renders_an_image(&predictor_doc(parms)),
            "a well-formed /DecodeParms must still decode: {}",
            String::from_utf8_lossy(parms)
        );
    }
}

// === PS CIDFont header counts =============================================
//
// `/CIDCount`, `/SubrCount`, and the `/FDBytes` `/GDBytes` `/SDBytes` widths
// come from the embedded font program's own text header and drive every
// reservation in the CID-map parser. `/SubrCount` was reserved *before* the
// bounds check that would have rejected it, so a bogus count reached
// `Vec::with_capacity` and panicked with "capacity overflow" in release as
// well as debug. Separately, `FDBytes + GDBytes == 0` made the CID map size
// zero for any `/CIDCount`, so the length check passed and an 8 TB
// reservation followed from a 700-byte file.

fn ps_cidfont_doc(header: &[u8]) -> Vec<u8> {
    let mut font = b"%!PS-Adobe-3.0 Resource-CIDFont\n".to_vec();
    font.extend_from_slice(header);
    font.extend_from_slice(b"\n(Binary) 64 StartData\n");
    font.extend_from_slice(&[0u8; 64]);

    let mut fontfile =
        format!("<</Subtype/CIDFontType0C/Length {}>>\nstream\n", font.len()).into_bytes();
    fontfile.extend_from_slice(&font);
    fontfile.extend_from_slice(b"\nendstream");

    one_page_doc(
        b"/Resources<</Font<</F1 5 0 R>>>>",
        b"BT /F1 12 Tf 10 50 Td <0001> Tj ET\n",
        &[
            (
                5,
                b"<</Type/Font/Subtype/Type0/BaseFont/Test/Encoding/Identity-H\
                  /DescendantFonts[6 0 R]>>"
                    .to_vec(),
            ),
            (
                6,
                b"<</Type/Font/Subtype/CIDFontType0/BaseFont/Test/CIDSystemInfo\
                  <</Registry(Adobe)/Ordering(Identity)/Supplement 0>>\
                  /FontDescriptor 7 0 R/DW 1000>>"
                    .to_vec(),
            ),
            (
                7,
                b"<</Type/FontDescriptor/FontName/Test/Flags 4/ItalicAngle 0/Ascent 800\
                  /Descent -200/CapHeight 700/StemV 80/FontBBox[0 0 1000 1000]\
                  /FontFile3 8 0 R>>"
                    .to_vec(),
            ),
            (8, fontfile),
        ],
    )
}

/// `FDBytes + GDBytes == 0` leaves `/CIDCount` entirely unbounded: the CID
/// map size is zero whatever the count, so the "binary data too short" check
/// passes and the reservation that follows asks for terabytes.
#[test]
fn ps_cidfont_zero_entry_size_does_not_allocate_wildly() {
    load_and_render(&ps_cidfont_doc(
        b"/CIDCount 1000000000000 /FDBytes 0 /GDBytes 0 /SDBytes 4 /SubrCount 0 \
          /SubrMapOffset 0",
    ));
}

/// A `/SubrCount` that is reserved before it is validated.
#[test]
fn ps_cidfont_oversized_subr_count_does_not_allocate_wildly() {
    load_and_render(&ps_cidfont_doc(
        b"/CIDCount 1 /FDBytes 0 /GDBytes 1 /SDBytes 0 \
          /SubrCount 18446744073709551615 /SubrMapOffset 0",
    ));
    load_and_render(&ps_cidfont_doc(
        b"/CIDCount 1 /FDBytes 0 /GDBytes 1 /SDBytes 18446744073709551615 \
          /SubrCount 18446744073709551615 /SubrMapOffset 18446744073709551615",
    ));
}

/// Counts and widths large enough to overflow the size products.
#[test]
fn ps_cidfont_overflowing_counts_do_not_panic() {
    load_and_render(&ps_cidfont_doc(
        b"/CIDCount 18446744073709551615 /FDBytes 0 /GDBytes 0 /SDBytes 4 \
          /SubrCount 0 /SubrMapOffset 0",
    ));
    load_and_render(&ps_cidfont_doc(
        b"/CIDCount 2 /FDBytes 18446744073709551615 /GDBytes 18446744073709551615 \
          /SDBytes 4 /SubrCount 0 /SubrMapOffset 0",
    ));
    load_and_render(&ps_cidfont_doc(
        b"/CIDCount 9223372036854775807 /FDBytes 1 /GDBytes 1 /SDBytes 4 \
          /SubrCount 0 /SubrMapOffset 0",
    ));
}
