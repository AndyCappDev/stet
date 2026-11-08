//! A pattern's `TilingType` reaches the display list and the PDF output. TilingType 2 asks for the exact step,
//! which a device may not round to whole pixels as it may for 1; a vector writer needs to know which it has.

use stet::Interpreter;
use stet_graphics::display_list::DisplayElement;

fn job(tiling_type: i32) -> String {
    format!(
        "%!PS\n<< /PatternType 1 /PaintType 1 /TilingType {tiling_type} /BBox [0 0 51.25 51.25] \
         /XStep 51.25 /YStep 51.25 /PaintProc {{ pop 0 0 10 10 rectfill }} >> matrix makepattern setpattern\n\
         0 0 300 300 rectfill showpage\n"
    )
}

#[test]
fn the_tiling_type_reaches_the_display_list() {
    for tiling_type in 1..=3 {
        let pages = Interpreter::new()
            .render_to_display_list(job(tiling_type).as_bytes(), 72.0)
            .expect("renders");
        let found: Vec<i32> = pages[0]
            .display_list
            .elements()
            .iter()
            .filter_map(|e| match e {
                DisplayElement::PatternFill { params } => Some(params.tiling_type),
                _ => None,
            })
            .collect();
        assert_eq!(found, vec![tiling_type]);
    }
}

#[test]
fn the_tiling_type_reaches_the_pdf() {
    let pdf = Interpreter::new()
        .render_to_pdf(job(2).as_bytes(), 72.0)
        .expect("writes");
    let text = String::from_utf8_lossy(&pdf);
    assert!(
        text.contains("/TilingType 2"),
        "the pattern keeps TilingType 2"
    );
}
