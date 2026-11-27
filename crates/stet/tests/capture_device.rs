// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A pattern cell or form is drawn from what its PaintProc paints, wherever
//! it was first painted.
//!
//! `makepattern` runs PaintProc on "a special [device] provided by the
//! PostScript implementation" (PLRM `makepattern`), and `execform` caches its
//! first PaintProc run for every later use. Both captured by swapping out the
//! page's display list, but paint went to the innermost open transparency
//! group, or nowhere under the null device: a pattern made, or a form first
//! used, in either place came out empty for the rest of the job, and inside a
//! group the cell's marks landed in the group at device-space coordinates.
//! Checked against Ghostscript 10.05, which draws all of these.
#![cfg(feature = "render")]

use stet::Interpreter;
use stet_core::object::PsValue;

/// A 100 × 100 page, a pattern whose cell is a red 10 × 10 square, and a form
/// that paints the same square.
const SETUP: &str = "%!PS
<< /PageSize [100 100] >> setpagedevice
/cell << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10]
  /XStep 10 /YStep 10 /PaintProc { pop 1 0 0 setrgbcolor 0 0 10 10 rectfill } >> def
/form << /FormType 1 /BBox [0 0 10 10] /Matrix [1 0 0 1 0 0]
  /PaintProc { pop 1 0 0 setrgbcolor 0 0 10 10 rectfill } >> def
/grp { << /BBox [0 0 100 100] >> begintransparencygroup } def
";

/// Whether each user-space point of the page `body` draws is red.
fn red_at(body: &str, points: &[(u32, u32)]) -> Vec<bool> {
    let job = format!("{SETUP}{body}\nshowpage\n");
    let pages = Interpreter::new().render(job.as_bytes(), 72.0).unwrap();
    let page = &pages[0];
    assert_eq!((page.width, page.height), (100, 100));
    points
        .iter()
        .map(|&(x, y)| {
            let i = (((page.height - 1 - y) * page.width + x) * 4) as usize;
            page.rgba[i] > 200 && page.rgba[i + 1] < 60
        })
        .collect()
}

/// Run `SETUP` and `body`, and return the name left on top of the operand
/// stack.
fn outcome(body: &str) -> String {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    let source = format!(
        "/null /OutputDevice findresource setpagedevice\n{}\n{body}\n",
        &SETUP[SETUP.find('\n').unwrap() + 1..]
    );
    stet_engine::eval::parse_and_exec(ctx, source.as_bytes())
        .unwrap_or_else(|e| panic!("job failed: {e:?}\nsource:\n{source}"));
    let top = ctx.o_stack.peek(0).expect("a result on the operand stack");
    match top.value {
        PsValue::Name(id) => String::from_utf8_lossy(ctx.names.get_bytes(id)).into_owned(),
        other => panic!("expected a name, got {other:?}"),
    }
}

#[test]
fn pattern_made_in_a_group_is_drawn_where_used() {
    assert_eq!(
        red_at(
            "grp /P cell matrix makepattern def endtransparencygroup\n\
             P setpattern 40 40 20 20 rectfill",
            // Two cells of the fill, and the device-space origin (the page's
            // top left), where the cell's square used to land in the group.
            &[(45, 45), (55, 55), (5, 95)]
        ),
        [true, true, false]
    );
}

#[test]
fn form_first_used_in_a_group_is_drawn_there_and_after() {
    assert_eq!(
        red_at(
            "grp gsave 50 50 translate form execform grestore endtransparencygroup\n\
             gsave 20 20 translate form execform grestore",
            &[(55, 55), (25, 25)]
        ),
        [true, true]
    );
}

#[test]
fn pattern_made_under_the_null_device_is_drawn_where_used() {
    assert_eq!(
        red_at(
            "gsave nulldevice /P cell matrix makepattern def grestore\n\
             P setpattern 40 40 20 20 rectfill",
            &[(45, 45), (55, 55)]
        ),
        [true, true]
    );
}

#[test]
fn form_first_used_under_the_null_device_is_drawn_after() {
    assert_eq!(
        red_at(
            "gsave nulldevice form execform grestore\n\
             gsave 20 20 translate form execform grestore",
            &[(25, 25)]
        ),
        [true]
    );
}

#[test]
fn form_still_paints_nothing_under_the_null_device() {
    // The capture is the cache's; this use is still on the null device.
    assert_eq!(
        red_at(
            "gsave 20 20 translate form execform grestore\n\
             gsave nulldevice 60 60 translate form execform grestore",
            &[(25, 25), (65, 65), (5, 5)]
        ),
        [true, false, false]
    );
}

#[test]
fn null_device_a_paint_proc_installs_is_honoured() {
    assert_eq!(
        red_at(
            "/P << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10]\n\
               /XStep 10 /YStep 10\n\
               /PaintProc { pop nulldevice 1 0 0 setrgbcolor 0 0 10 10 rectfill } >>\n\
             matrix makepattern def\n\
             P setpattern 40 40 20 20 rectfill",
            &[(45, 45), (55, 55)]
        ),
        [false, false]
    );
}

/// A PaintProc runs on a graphics-state stack of its own: a `grestore` too
/// many does not pop the program's `gsave`, as in Ghostscript.
#[test]
fn paint_proc_cannot_pop_the_programs_gsave() {
    assert_eq!(
        outcome(
            "1 setlinewidth gsave 7 setlinewidth\n\
             << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10]\n\
                /XStep 10 /YStep 10 /PaintProc { pop grestore grestore } >>\n\
             matrix makepattern pop\n\
             currentlinewidth 7 eq grestore currentlinewidth 1 eq and\n\
             { /ok } { /popped } ifelse"
        ),
        "ok"
    );
}

/// A PaintProc cannot end the group the pattern is made in: no group is open
/// as far as it can see, and the program's group stays open for the program
/// to end.
#[test]
fn paint_proc_cannot_end_the_programs_group() {
    assert_eq!(
        outcome(
            "grp\n\
             { << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10]\n\
                  /XStep 10 /YStep 10 /PaintProc { pop endtransparencygroup } >>\n\
               matrix makepattern } stopped\n\
             { $error /errorname get /rangecheck eq } { pop false } ifelse\n\
             { endtransparencygroup } stopped not and\n\
             { /ok } { /wrong } ifelse"
        ),
        "ok"
    );
}

/// A group a PaintProc leaves open does not stay open for the program.
#[test]
fn group_a_paint_proc_leaves_open_is_not_the_programs() {
    assert_eq!(
        outcome(
            "<< /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10]\n\
                /XStep 10 /YStep 10 /PaintProc { pop grp 0 0 10 10 rectfill } >>\n\
             matrix makepattern pop\n\
             { endtransparencygroup } stopped { /ok } { /leaked } ifelse"
        ),
        "ok"
    );
}

/// A `save` and `restore` on either side of a pattern made in a group still
/// agree on the group nesting.
#[test]
fn save_and_restore_around_a_capture_in_a_group() {
    assert_eq!(
        outcome(
            "grp /sv save def /P cell matrix makepattern def\n\
             { sv restore } stopped { /refused } { endtransparencygroup /ok } ifelse"
        ),
        "ok"
    );
}
