// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The page-device protocol: when `BeginPage` and `EndPage` run, what they
//! are passed, and what `EndPage`'s answer does (PLRM 3e §6.2.6).
//!
//! Each test logs the calls a job's own procedures receive and checks the
//! sequence against Ghostscript 10's for the same job.

use std::io::Write;
use std::sync::{Arc, Mutex};

use stet::{Interpreter, RenderedPage};

/// What PostScript `print`s, kept for inspection.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Log {
    /// The logged lines, trimmed.
    fn lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }
}

/// A 100 × 100 page device whose `BeginPage` and `EndPage` log their
/// operands; `EndPage` answers with `decide`, a procedure from the reason
/// code to a boolean.
fn device(decide: &str) -> String {
    format!(
        "<< /PageSize [100 100] \
            /BeginPage {{ (BeginPage ) print = }} \
            /EndPage {{ (EndPage ) print 1 index =only ( ) print dup = exch pop {decide} }} \
         >> setpagedevice\n"
    )
}

/// The standard answer: produce the page unless the device is going away.
const STANDARD: &str = "2 ne";

fn run(body: &str) -> (Vec<RenderedPage>, Vec<String>) {
    let log = Log::default();
    let mut interp = Interpreter::new();
    interp.context().stdout = Box::new(log.clone());
    let pages = interp.render(body.as_bytes(), 72.0).unwrap();
    (pages, log.lines())
}

/// RGB at a point given in user space (origin bottom-left).
fn at(page: &RenderedPage, x: u32, y: u32) -> [u8; 3] {
    let i = (((page.height - 1 - y) * page.width + x) * 4) as usize;
    [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
}

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const PAPER: [u8; 3] = [255; 3];

/// In LanguageLevel 3, `copypage` is `showpage` without `initgraphics`:
/// reason 0, counted, `BeginPage` after it, and the page erased once sent.
#[test]
fn copypage_is_showpage_without_initgraphics() {
    let (pages, log) = run(&format!(
        "{}1 0 0 setrgbcolor 0 0 50 100 rectfill copypage \
         (gray after copypage ) print currentrgbcolor pop pop = \
         0 0 1 setrgbcolor 50 0 50 100 rectfill showpage",
        device(STANDARD)
    ));
    assert_eq!(
        log,
        [
            "BeginPage 0",
            "EndPage 0 0",
            "BeginPage 1",
            "gray after copypage 1.0", // the colour survives: no initgraphics
            "EndPage 1 0",
            "BeginPage 2",
            "EndPage 2 2", // end of job
        ]
    );
    assert_eq!(pages.len(), 2);
    assert_eq!([at(&pages[0], 25, 50), at(&pages[0], 75, 50)], [RED, PAPER]);
    assert_eq!(
        [at(&pages[1], 25, 50), at(&pages[1], 75, 50)],
        [PAPER, BLUE]
    );
}

/// An `EndPage` that answers false neither sends nor erases the page: the
/// next page is drawn on top of it, which is how n-up imposition gathers
/// pages. `PageCount` counts pages produced; the count `EndPage` receives
/// counts every `showpage`.
#[test]
fn a_declined_page_carries_over() {
    let (pages, log) = run(&format!(
        "/n 0 def {}1 0 0 setrgbcolor 0 0 50 100 rectfill showpage \
         0 0 1 setrgbcolor 50 0 50 100 rectfill showpage \
         (PageCount ) print currentpagedevice /PageCount get =",
        device("2 eq { false } { /n n 1 add def n 2 mod 0 eq } ifelse")
    ));
    assert_eq!(
        log,
        [
            "BeginPage 0",
            "EndPage 0 0",
            "BeginPage 1",
            "EndPage 1 0",
            "BeginPage 2",
            "PageCount 1",
            "EndPage 2 2",
        ]
    );
    assert_eq!(pages.len(), 1);
    assert_eq!([at(&pages[0], 25, 50), at(&pages[0], 75, 50)], [RED, BLUE]);
}

/// At the end of a job the device is deactivated: `EndPage` runs with
/// reason 2, and a true answer sends the page no `showpage` ended — the
/// last, partly filled sheet of an n-up job.
#[test]
fn end_of_job_can_send_the_last_page() {
    let (pages, log) = run(&format!(
        "{}1 0 0 setrgbcolor 0 0 50 100 rectfill showpage \
         0 0 1 setrgbcolor 50 0 50 100 rectfill",
        device("2 eq") // gather everything, send it at the end
    ));
    assert_eq!(
        log,
        ["BeginPage 0", "EndPage 0 0", "BeginPage 1", "EndPage 1 2"]
    );
    assert_eq!(pages.len(), 1);
    assert_eq!([at(&pages[0], 25, 50), at(&pages[0], 75, 50)], [RED, BLUE]);
}

/// `setpagedevice` deactivates the current device first, even to change
/// one parameter, and erases the page: marks made before it do not reach
/// the next page. The count carries on.
#[test]
fn setpagedevice_ends_the_old_device_and_erases() {
    let (pages, log) = run(&format!(
        "{}1 0 0 setrgbcolor 0 0 50 100 rectfill \
         << /PageSize [100 100] >> setpagedevice \
         0 0 1 setrgbcolor 50 0 50 100 rectfill showpage",
        device(STANDARD)
    ));
    assert_eq!(
        log,
        [
            "BeginPage 0",
            "EndPage 0 2",
            "BeginPage 0",
            "EndPage 0 0",
            "BeginPage 1",
            "EndPage 1 2",
        ]
    );
    assert_eq!(pages.len(), 1);
    assert_eq!(
        [at(&pages[0], 25, 50), at(&pages[0], 75, 50)],
        [PAPER, BLUE]
    );
}

/// Each output device's own `EndPage` consumes both its operands and
/// answers true for a page, false for deactivation. `pdf.ps` used to leave
/// the count behind at every page.
#[test]
fn device_procedures_consume_their_operands() {
    for device in ["png", "pdf", "viewer", "null"] {
        let log = Log::default();
        let mut interp = Interpreter::new();
        interp.context().stdout = Box::new(log.clone());
        let check = format!(
            "/{device} /OutputDevice findresource /EndPage get /ep exch def \
             [0 1 2] {{ 3 exch ep count = = }} forall"
        );
        interp.exec(check.as_bytes()).unwrap();
        let lines: Vec<String> = log
            .lines()
            .into_iter()
            .filter(|l| !l.starts_with("Creating"))
            .collect();
        let produce = device != "null";
        assert_eq!(
            lines,
            [
                "1".to_string(),
                produce.to_string(),
                "1".to_string(),
                produce.to_string(),
                "1".to_string(),
                "false".to_string(),
            ],
            "{device}"
        );
    }
}

/// With `--device pdf`, a page leaves nothing on the operand stack.
#[test]
fn pdf_output_leaves_the_operand_stack_clean() {
    let log = Log::default();
    let mut interp = Interpreter::new();
    interp.context().stdout = Box::new(log.clone());
    interp
        .render_to_pdf(
            b"%!PS\nshowpage count = copypage count = 0 0 10 10 rectfill showpage count =",
            72.0,
        )
        .unwrap();
    assert_eq!(log.lines(), ["0", "0", "0"]);
}
