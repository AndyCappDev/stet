// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The font copies a job keeps for its output device go with the job.
//!
//! A long-lived [`Interpreter`] runs job after job; the copies of one job's
//! fonts are read by its device at the end of the job and are no use to the
//! next. They are dropped before the job's own `restore`, which would
//! otherwise copy every font still live for nothing.

use stet::Interpreter;
use stet_core::device::{ClipParams, FillParams, ImageParams, OutputDevice, StrokeParams};
use stet_core::geometry::PsPath;

/// A device that asks for font copies and draws nothing.
struct KeepsFonts;

impl OutputDevice for KeepsFonts {
    fn fill_path(&mut self, _: &PsPath, _: &FillParams) {}
    fn stroke_path(&mut self, _: &PsPath, _: &StrokeParams) {}
    fn clip_path(&mut self, _: &PsPath, _: &ClipParams) {}
    fn init_clip(&mut self) {}
    fn erase_page(&mut self) {}
    fn show_page(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn draw_image(&mut self, _: &[u8], _: &ImageParams) {}
    fn page_size(&self) -> (u32, u32) {
        (612, 792)
    }
    fn keeps_text_fonts(&self) -> bool {
        true
    }
}

#[test]
fn a_job_leaves_no_font_copies_behind() {
    let mut interp = Interpreter::new();
    interp.context().device = Some(Box::new(KeepsFonts));
    interp
        .exec(b"/Times-Roman findfont 12 scalefont setfont 72 72 moveto (A) show")
        .unwrap();
    assert!(interp.context().font_snapshots.is_empty());
}
