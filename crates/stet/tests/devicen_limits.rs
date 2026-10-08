// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

// `setcolorspace` samples a DeviceN tint transform into a lookup table of
// `samples ^ colorants` entries, running the transform procedure once per
// entry. The colorant count is the program's to choose: nine colorants at
// nine samples a side is 387 million runs of the procedure, which did not
// finish, and eight was 43 million. The table now has a ceiling, and a
// space too wide for one is painted from the tint transform alone.
//
// A regression here does not fail an assertion: the test never returns.

use stet::Interpreter;

/// A DeviceN space of `n` colorants whose tint transform keeps the first as
/// cyan, selected and painted with.
fn paint_in_devicen(n: usize) -> Interpreter {
    let names: String = (0..n).map(|i| format!("/c{i} ")).collect();
    let source = format!(
        "[/DeviceN [{names}] /DeviceCMYK {{ {}0 0 0 }}] setcolorspace\n\
         1 {}setcolor 100 100 50 50 rectfill\n",
        "pop ".repeat(n - 1),
        "0 ".repeat(n - 1),
    );
    let mut interp = Interpreter::builder().suppress_output().build();
    stet_engine::eval::parse_and_exec(interp.context(), source.as_bytes())
        .unwrap_or_else(|e| panic!("{n} colorants: {e:?}"));
    interp
}

#[test]
fn eight_colorants_get_a_smaller_table() {
    let mut interp = paint_in_devicen(8);
    let table = interp
        .context()
        .gstate
        .cached_tint_table
        .clone()
        .expect("eight colorants fit a table");
    assert_eq!(table.samples_per_dim, 6);
    assert_eq!(table.data.len(), 6usize.pow(8) * 4);
}

#[test]
fn seven_colorants_keep_the_table_they_had() {
    let mut interp = paint_in_devicen(7);
    let table = interp.context().gstate.cached_tint_table.clone().unwrap();
    assert_eq!(table.samples_per_dim, 9);
}

#[test]
fn more_colorants_than_a_table_spans_still_paint() {
    for n in [9, 32] {
        let mut interp = paint_in_devicen(n);
        assert!(
            interp.context().gstate.cached_tint_table.is_none(),
            "{n} colorants"
        );
    }
}
