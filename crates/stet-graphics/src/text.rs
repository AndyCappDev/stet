// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How the glyphs of extracted text follow one another.
//!
//! One rule decides where a [`TextRunParams`] ends and where the gaps
//! between its words are, and every producer of text runs — the PostScript
//! interpreter and the PDF reader — applies it through
//! [`TextRunParams::step_to`], so their runs are cut the same way.
//!
//! Distances are measured in the run's glyph space, as fractions of the
//! font's height (`ascent - descent`, about one em), so the rule holds at
//! any size, rotation or skew.

use crate::device::TextRunParams;

/// A forward gap between two glyphs, as a fraction of the font's height,
/// from which on they are separate words: about a tenth of an em. Kerning
/// and tracking stay below it; the space TeX leaves between words
/// (a third of an em) does not.
pub const WORD_GAP: f64 = 0.1;

/// Distance off the run's baseline, as a fraction of the font's height,
/// beyond which a glyph starts a new run: a new line, or a superscript.
pub const BASELINE_TOLERANCE: f64 = 0.1;

/// A move back along the baseline, as a fraction of the font's height,
/// beyond which a glyph starts a new run: text restarting elsewhere. A
/// shorter move back — kerning, or an accent drawn over the letter before
/// it — stays in the run.
pub const BACKWARD_LIMIT: f64 = 1.0;

/// How a glyph follows on from the end of a run: see
/// [`TextRunParams::step_to`].
///
/// Marked `#[non_exhaustive]`: finer distinctions may be added, so `match`
/// on it with a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GlyphStep {
    /// The glyph continues the run's word.
    Continues,
    /// The glyph continues the run after a word's gap.
    WordBreak,
    /// The glyph is off the run's baseline, or well behind its end: it
    /// starts a new run.
    Leaves,
}

impl TextRunParams {
    /// How a glyph whose origin is `origin`, in device space, follows on
    /// from [`end`](Self::end), where the run's last glyph finished — in
    /// the run's direction of writing, horizontal or
    /// [`vertical`](Self::vertical).
    ///
    /// A glyph off the baseline by more than [`BASELINE_TOLERANCE`], or
    /// more than [`BACKWARD_LIMIT`] behind the end, [`Leaves`]; one at
    /// least [`WORD_GAP`] ahead is a [`WordBreak`]; any other
    /// [`Continues`]. A run whose glyph space is degenerate (zero size)
    /// cannot measure distance, and every glyph continues it.
    ///
    /// [`Leaves`]: GlyphStep::Leaves
    /// [`WordBreak`]: GlyphStep::WordBreak
    /// [`Continues`]: GlyphStep::Continues
    pub fn step_to(&self, origin: (f64, f64)) -> GlyphStep {
        let m = &self.glyph_to_device;
        let det = m.a * m.d - m.b * m.c;
        let height = self.ascent - self.descent;
        if det == 0.0 || !det.is_finite() || height <= 0.0 || !height.is_finite() {
            return GlyphStep::Continues;
        }
        // The move from the end to the glyph, in glyph space.
        let (dx, dy) = (origin.0 - self.end.0, origin.1 - self.end.1);
        let gx = (m.d * dx - m.c * dy) / det;
        let gy = (m.a * dy - m.b * dx) / det;
        // Vertical writing advances down the column: glyph-space -y.
        let (along, across) = if self.vertical { (-gy, gx) } else { (gx, gy) };
        let (along, across) = (along / height, across / height);
        if across.abs() > BASELINE_TOLERANCE || along < -BACKWARD_LIMIT {
            GlyphStep::Leaves
        } else if along >= WORD_GAP {
            GlyphStep::WordBreak
        } else {
            GlyphStep::Continues
        }
    }

    /// For producers: mark a word break at byte `at` of
    /// [`text`](Self::text), where the text of a glyph that followed a
    /// word's gap ([`GlyphStep::WordBreak`]) begins — unless a space shown
    /// on either side already separates the words, no text precedes it, or
    /// the break is marked already.
    pub fn push_word_break(&mut self, at: usize) {
        let Some((before, after)) = self.text.split_at_checked(at) else {
            return;
        };
        if before.is_empty()
            || before.ends_with(char::is_whitespace)
            || after.starts_with(char::is_whitespace)
            || self.word_breaks.last() == Some(&(at as u32))
        {
            return;
        }
        self.word_breaks.push(at as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_fonts::geometry::Matrix;

    /// A 10-point run in 1000-unit glyph space, ending at (100, 50).
    fn run(glyph_to_device: Matrix, vertical: bool) -> TextRunParams {
        TextRunParams {
            glyph_to_device,
            ascent: 800.0,
            descent: -200.0,
            end: (100.0, 50.0),
            vertical,
            ..TextRunParams::default()
        }
    }

    #[test]
    fn horizontal_steps() {
        let r = run(Matrix::new(0.01, 0.0, 0.0, 0.01, 0.0, 0.0), false);
        assert_eq!(r.step_to((100.0, 50.0)), GlyphStep::Continues);
        assert_eq!(r.step_to((99.5, 50.0)), GlyphStep::Continues); // kern
        assert_eq!(r.step_to((100.5, 50.0)), GlyphStep::Continues);
        assert_eq!(r.step_to((101.0, 50.0)), GlyphStep::WordBreak);
        assert_eq!(r.step_to((300.0, 50.0)), GlyphStep::WordBreak);
        assert_eq!(r.step_to((80.0, 50.0)), GlyphStep::Leaves);
        assert_eq!(r.step_to((100.0, 53.0)), GlyphStep::Leaves); // superscript
        assert_eq!(r.step_to((20.0, 38.0)), GlyphStep::Leaves); // next line
    }

    #[test]
    fn steps_follow_rotation_and_flipped_device_space() {
        // Rotated 90° and y-flipped, as a raster's device space is.
        let r = run(Matrix::new(0.0, -0.01, -0.01, 0.0, 0.0, 0.0), false);
        assert_eq!(r.step_to((100.0, 48.0)), GlyphStep::WordBreak);
        assert_eq!(r.step_to((100.0, 50.5)), GlyphStep::Continues);
        assert_eq!(r.step_to((102.0, 50.0)), GlyphStep::Leaves);
    }

    #[test]
    fn vertical_steps_run_down_the_column() {
        let r = run(Matrix::new(0.01, 0.0, 0.0, 0.01, 0.0, 0.0), true);
        assert_eq!(r.step_to((100.0, 49.5)), GlyphStep::Continues);
        assert_eq!(r.step_to((100.0, 48.0)), GlyphStep::WordBreak);
        assert_eq!(r.step_to((102.0, 50.0)), GlyphStep::Leaves);
    }

    #[test]
    fn word_breaks_are_not_marked_beside_shown_spaces() {
        let mut r = TextRunParams {
            text: "Hi there".into(),
            ..TextRunParams::default()
        };
        r.push_word_break(0); // nothing before
        r.push_word_break(3); // after the space
        r.push_word_break(2); // before the space
        assert!(r.word_breaks.is_empty());
        r.text = "PaperTitle".into();
        r.push_word_break(5);
        r.push_word_break(5);
        assert_eq!(r.word_breaks, [5]);
    }

    #[test]
    fn a_degenerate_run_is_always_continued() {
        let r = run(Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0), false);
        assert_eq!(r.step_to((500.0, 500.0)), GlyphStep::Continues);
    }
}
