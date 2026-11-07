// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Regression tests for hostile or malformed PostScript.
//!
//! The bar is the one `stet-pdf-reader`'s `malformed_input_guards.rs` sets:
//! the job must *return*, with or without an error. Nothing here asserts what
//! is drawn. A panic is the failure these guard against: it takes down the
//! process of any program embedding the interpreter, not just the job.

use stet::Interpreter;

/// Run `body` as a one-page job; an error is allowed, a panic is not.
fn run(body: &str) {
    let job = format!("%!PS\n<< /PageSize [20 20] >> setpagedevice\n{body}\nshowpage\n");
    let _ = Interpreter::new().render(job.as_bytes(), 72.0);
}

/// A CIE-based colour space's range read from the file, reversed. Colour
/// conversion clamped to it with `f64::clamp`, which panics on a reversed
/// pair; Ghostscript raises `rangecheck`.
#[test]
fn reversed_cie_ranges_do_not_panic() {
    let wp = "/WhitePoint [0.9505 1 1.089]";
    for space in [
        format!("[/CIEBasedABC << {wp} /RangeABC [1 0 0 1 0 1] >>]"),
        format!("[/CIEBasedABC << {wp} /RangeLMN [0 1 1 0 0 1] >>]"),
        format!("[/CIEBasedA << {wp} /RangeA [1 0] >>]"),
        format!("[/CIEBasedA << {wp} /RangeLMN [1 0 0 1 0 1] >>]"),
    ] {
        let comps = if space.contains("CIEBasedABC") {
            "0.5 0.5 0.5"
        } else {
            "0.5"
        };
        run(&format!(
            "{space} setcolorspace {comps} setcolor 0 0 20 20 rectfill"
        ));
    }
}

/// A shading function's `/Domain` or `/Range`, reversed: the same clamp,
/// in the function evaluator.
#[test]
fn reversed_function_domain_or_range_does_not_panic() {
    for bounds in ["/Domain [1 0]", "/Domain [0 1] /Range [1 0]"] {
        run(&format!(
            "<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 20 0]\n\
             /Function << /FunctionType 2 {bounds} /C0 [0] /C1 [1] /N 1 >> >> shfill"
        ));
    }
}
