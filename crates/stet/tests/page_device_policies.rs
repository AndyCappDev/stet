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

/// A request for a parameter the device does not have is ignored under the
/// default policy, and the page device has no entry for it afterwards
/// (PLRM 6.2.7: "Absent if the parameter is unknown to the device"). The
/// `PolicyReport` gets each such key with its policy, after `BeginPage`,
/// with the new page device installed, as in Ghostscript.
#[test]
fn an_unknown_parameter_is_ignored_and_reported() {
    passes(
        "/log [] def /note { log length 1 add array dup dup length 1 sub 4 -1 roll put\n\
             dup 0 log putinterval /log exch def } def\n\
         /logged { dup length log length eq {\n\
             true 0 1 log length 1 sub { dup log exch get exch 3 index exch get ne\n\
             { pop false } if } for exch pop } { pop false } ifelse } def\n\
         << /BeginPage { pop /begin note }\n\
            /Policies << /PolicyReport {\n\
               dup length 2 ne { 0 0 div } if\n\
               dup /Duplex get 1 ne { 0 0 div } if /Foo get 1 ne { 0 0 div } if\n\
               currentpagedevice /PageSize get 0 get 300 ne { 0 0 div } if\n\
               /report note } >>\n\
            /Duplex true /Foo 3 /PageSize [300 400] >> setpagedevice\n\
         [/begin /report] logged not { 0 0 div } if\n\
         currentpagedevice dup /Duplex known { 0 0 div } if /Foo known { 0 0 div } if\n\
         /log [] def << /PageSize [300 400] >> setpagedevice\n\
         [/begin] logged not { 0 0 div } if",
    );
}

/// Under a policy other than 1, a request for an unknown parameter is a
/// `configurationerror`, with `[key value]` in `$error /errorinfo`, and the
/// page device keeps everything it had, the rest of the request included.
/// A per-key policy decides before `PolicyNotFound`, and a request's own
/// `Policies` govern its other keys (PLRM 6.2.7).
#[test]
fn an_unknown_parameter_under_policy_0_is_a_configurationerror() {
    for policies in [
        "/PolicyNotFound 0",
        "/PolicyNotFound 2",
        "/PolicyNotFound 7",
        "/Duplex 0",
        "/Duplex 2",
        "/Duplex 3",
        "/Duplex (x)",
        "/PolicyNotFound 1 /Duplex 0",
    ] {
        for request in [
            format!(
                "<< /Policies << {policies} >> >> setpagedevice << /Duplex null /PageSize [300 400] >>"
            ),
            format!("<< /Duplex null /PageSize [300 400] /Policies << {policies} >> >>"),
        ] {
            passes(&format!(
                "{{ {request} setpagedevice }} stopped not {{ 0 0 div }} if\n\
                 $error /errorname get /configurationerror ne {{ 0 0 div }} if\n\
                 $error /errorinfo get aload pop null ne {{ 0 0 div }} if /Duplex ne {{ 0 0 div }} if\n\
                 type /dicttype ne {{ 0 0 div }} if\n\
                 currentpagedevice dup /Duplex known {{ 0 0 div }} if\n\
                 /PageSize get 0 get 300 eq {{ 0 0 div }} if"
            ));
        }
    }
    passes(
        "<< /Policies << /PolicyNotFound 0 /Duplex 1 >> /Duplex true >> setpagedevice\n\
         currentpagedevice /Duplex known { 0 0 div } if",
    );
}

/// The parameters a device has but cannot change keep the device's values,
/// silently, even under a policy of 0, as in Ghostscript: the request is
/// for a known feature, and the report is for unknown ones.
#[test]
fn a_read_only_parameter_keeps_the_devices_value() {
    passes(
        "<< /Policies << /PolicyNotFound 0 /PolicyReport { 0 0 div } >>\n\
            /BitsPerPixel 8 /Colors 1 /ProcessColorModel /DeviceGray /ColorModel /DeviceGray\n\
            /Separations true /PageCount 9 /MediaSize [1 1] >> setpagedevice\n\
         currentpagedevice dup /BitsPerPixel get 24 ne { 0 0 div } if\n\
         dup /Colors get 3 ne { 0 0 div } if\n\
         dup /ProcessColorModel get /DeviceRGB ne { 0 0 div } if\n\
         dup /Separations get { 0 0 div } if dup /PageCount get 0 ne { 0 0 div } if\n\
         /MediaSize get 0 get 1 eq { 0 0 div } if",
    );
}

/// The media selection requests, `SeparationColorNames` and `ImagingBBox`
/// are taken, under any policy, as in Ghostscript, although the device
/// selects no medium and images the whole page.
#[test]
fn media_requests_are_taken_under_any_policy() {
    passes(
        "<< /Policies << /PolicyNotFound 0 >>\n\
            /MediaColor (white) /MediaWeight 75 /MediaType (plain) /InsertSheet false\n\
            /ManualFeed true /LeadingEdge 1 /MediaClass (c) /MediaPosition 2 /Orientation 1\n\
            /OutputType (o) /SeparationColorNames [] /ImagingBBox [0 0 100 100] >> setpagedevice\n\
         currentpagedevice dup /MediaPosition get 2 ne { 0 0 div } if\n\
         dup /Orientation get 1 ne { 0 0 div } if /ImagingBBox get 2 get 100 ne { 0 0 div } if\n\
         currentpagedevice /ImagingBBox get pop",
    );
}

/// `errorinfo` is null until a `configurationerror` sets it, stays through
/// later errors, and is null again after `handleerror` (PLRM 3.11), which
/// prints it, or after a `restore` to before it was set, as in Ghostscript.
#[test]
fn errorinfo_lasts_until_handleerror() {
    let src = "%!PS\n\
        $error /errorinfo get null ne { 0 0 div } if\n\
        << /Policies << /PolicyNotFound 0 >> >> setpagedevice\n\
        save { << /Duplex true >> setpagedevice } stopped pop pop restore\n\
        $error /errorinfo get null ne { 0 0 div } if\n\
        { << /Duplex true >> setpagedevice } stopped pop pop\n\
        { 1 0 idiv } stopped pop\n\
        $error /errorinfo get 1 get true ne { 0 0 div } if\n\
        (MARK\\n) print handleerror (MARK\\n) print\n\
        $error /errorinfo get null ne { 0 0 div } if\n\
        { 1 0 idiv } stopped pop handleerror\n\
        0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n";
    let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut interp = Interpreter::builder().build();
    interp.context().stdout = Box::new(Shared(out.clone()));
    let pages = interp.render(src.as_bytes(), 72.0).expect("renders");
    assert_eq!(pages.len(), 1, "a check failed");
    let out = String::from_utf8_lossy(&out.borrow()).into_owned();
    let errors: Vec<&str> = out.lines().filter(|l| l.contains("%%[ Error")).collect();
    assert_eq!(errors.len(), 2, "{out}");
    assert!(
        errors[0]
            .contains("Error: undefinedresult; OffendingCommand: idiv; ErrorInfo: Duplex true"),
        "{out}"
    );
    assert!(!errors[1].contains("ErrorInfo"), "{out}");
}

/// A writer that keeps what the interpreter prints.
struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
