// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The page device's `Policies` (PLRM 6.2.7), and the parameters whose
//! requests merge into the current value, as in Ghostscript.

use stet::Interpreter;

/// Run `body`, then paint and show one page. Each check in `body` divides by
/// zero when it fails, so a passing program renders exactly one page.
fn passes(body: &str) -> Vec<stet::RenderedPage> {
    let src =
        format!("%!PS-Adobe-3.0\n{body}\n0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src.as_bytes(), 72.0).expect("renders");
    assert_eq!(pages.len(), 1, "a check failed in:\n{body}");
    pages
}

/// The policies in force, as `pol`, and a check that one is `value`.
const POLICIES: &str = "/pol { currentpagedevice /Policies get } def\n\
    /is { pol exch get ne { 0 0 div } if } def\n";

/// Each device starts with Ghostscript's policies: ignore a parameter the
/// device does not know, refuse a page size there is no medium for, and
/// report nothing.
#[test]
fn a_device_starts_with_the_default_policies() {
    passes(&format!(
        "{POLICIES}\
         1 /PolicyNotFound is 0 /PageSize is\n\
         pol /PolicyReport get dup xcheck not {{ 0 0 div }} if type /arraytype ne {{ 0 0 div }} if"
    ));
}

/// A request's `Policies` merges into the current ones (PLRM: "Changes to
/// the contents of the Policies dictionary are cumulative"), so pdftops'
/// `<< /PageSize 6 >>` leaves `PolicyNotFound` in place.
#[test]
fn a_requests_policies_merge_into_the_current_ones() {
    passes(&format!(
        "{POLICIES}\
         << /Policies << /PageSize 6 >> /Duplex true >> setpagedevice\n\
         6 /PageSize is 1 /PolicyNotFound is\n\
         << /Policies << /Duplex 0 >> >> setpagedevice\n\
         0 /Duplex is 6 /PageSize is 1 /PolicyNotFound is"
    ));
}

/// `InputAttributes` and `OutputAttributes` merge too, as in Ghostscript.
#[test]
fn media_attributes_merge_into_the_current_ones() {
    for key in ["InputAttributes", "OutputAttributes"] {
        passes(&format!(
            "<< /{key} << 1 << >> >> >> setpagedevice\n\
             << /{key} << 2 << >> >> >> setpagedevice\n\
             currentpagedevice /{key} get dup 1 known not {{ 0 0 div }} if\n\
             2 known not {{ 0 0 div }} if"
        ));
    }
}

/// The errors Ghostscript raises for a merged parameter that is not a
/// dictionary and for the two policies it checks on every request:
/// `PolicyNotFound`, an integer from 0 to 7, and `PageSize`, any integer.
/// A policy for another key is not checked until a request needs it.
#[test]
fn bad_policies_are_refused() {
    for (request, error) in [
        ("/Policies 5", "typecheck"),
        ("/Policies null", "typecheck"),
        ("/InputAttributes null", "typecheck"),
        ("/OutputAttributes 5", "typecheck"),
        ("/Policies << /PolicyNotFound (1) >>", "typecheck"),
        ("/Policies << /PolicyNotFound 8 >>", "rangecheck"),
        ("/Policies << /PolicyNotFound -1 >>", "rangecheck"),
        ("/Policies << /PageSize 1.0 >>", "typecheck"),
    ] {
        passes(&format!(
            "{{ << {request} >> setpagedevice }} stopped not {{ 0 0 div }} if\n\
             $error /errorname get /{error} ne {{ 0 0 div }} if\n\
             type /dicttype ne {{ 0 0 div }} if"
        ));
    }
    for request in [
        "/Policies << /PolicyNotFound 7 >>",
        "/Policies << /PageSize 99 >>",
        "/Policies << /Duplex (x) >>",
    ] {
        passes(&format!("<< {request} >> setpagedevice"));
    }
}

/// A request that fails leaves the page device as it was: its parameters,
/// the page and the graphics state. Its EndPage has run, with reason 2, as
/// in Ghostscript.
#[test]
fn a_failed_request_changes_nothing() {
    let src = "%!PS-Adobe-3.0\n\
        /deactivations 0 def\n\
        << /PageSize [200 100] /EndPage { exch pop dup 2 eq\n\
           { /deactivations deactivations 1 add def } if 2 ne } >> setpagedevice\n\
        1 0 0 setrgbcolor 0 0 200 100 rectfill\n\
        0 1 0 setrgbcolor\n\
        { << /PageSize [50 50] /Policies << /PolicyNotFound 8 >> >> setpagedevice } stopped\n\
        not { 0 0 div } if pop\n\
        deactivations 1 ne { 0 0 div } if\n\
        currentpagedevice /PageSize get aload pop 100 ne exch 200 ne or { 0 0 div } if\n\
        currentrgbcolor 0 ne exch 1 ne or exch 0 ne or { 0 0 div } if\n\
        showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src.as_bytes(), 72.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    assert_eq!((page.width, page.height), (200, 100));
    let i = ((50 * page.width + 100) * 4) as usize;
    assert_eq!(&page.rgba[i..i + 3], &[255, 0, 0], "the page was erased");
}
