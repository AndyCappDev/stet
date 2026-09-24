// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Extracted text: how glyphs follow one another, and how runs assemble
//! into words and lines.
//!
//! One rule decides where a [`TextRunParams`] ends and where the gaps
//! between its words are, and every producer of text runs — the PostScript
//! interpreter and the PDF reader — applies it through
//! [`TextRunParams::step_to`], so their runs are cut the same way.
//!
//! For consumers, [`text_runs`] collects a page's runs and [`text_lines`]
//! assembles them into lines of words. The assembly is deliberately
//! simple: it keeps content order and does not detect columns, tables or
//! reading order, so a page drawn out of reading order reads out of order.
//! It uses only what every run carries, so it gives the same text at
//! [`TextExtraction::Runs`](crate::device::TextExtraction::Runs) as at
//! [`Glyphs`](crate::device::TextExtraction::Glyphs); only word boxes need
//! the glyphs.
//!
//! Distances are measured in a run's glyph space, as fractions of the
//! font's height (`ascent - descent`, about one em), so the rules hold at
//! any size, rotation or skew.

use std::ops::Range;

use crate::device::TextRunParams;
use crate::display_list::{DisplayElement, DisplayList};
use crate::layer_set::LayerSet;

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

/// Distance off a line's baseline, as a fraction of the font's height,
/// within which the next run joins the line in [`text_lines`]: a
/// superscript or subscript does, the next line does not.
pub const LINE_TOLERANCE: f64 = 0.5;

/// Cosine of the widest angle between two baselines still taken as one
/// direction of writing (about 8°).
const PARALLEL: f64 = 0.99;

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
        let Some((along, across)) = self.offset_from_end(origin) else {
            return GlyphStep::Continues;
        };
        if across.abs() > BASELINE_TOLERANCE || along < -BACKWARD_LIMIT {
            GlyphStep::Leaves
        } else if along >= WORD_GAP {
            GlyphStep::WordBreak
        } else {
            GlyphStep::Continues
        }
    }

    /// The move from [`end`](Self::end) to device-space `point`, in font
    /// heights: along the direction of writing, and across it. `None` when
    /// the run's glyph space is degenerate.
    fn offset_from_end(&self, point: (f64, f64)) -> Option<(f64, f64)> {
        let m = &self.glyph_to_device;
        let det = m.a * m.d - m.b * m.c;
        let height = self.ascent - self.descent;
        if det == 0.0 || !det.is_finite() || height <= 0.0 || !height.is_finite() {
            return None;
        }
        // The move, in glyph space.
        let (dx, dy) = (point.0 - self.end.0, point.1 - self.end.1);
        let gx = (m.d * dx - m.c * dy) / det;
        let gy = (m.a * dy - m.b * dx) / det;
        // Vertical writing advances down the column: glyph-space -y.
        let (along, across) = if self.vertical { (-gy, gx) } else { (gx, gy) };
        Some((along / height, across / height))
    }

    /// The device-space unit vector of the direction of writing.
    fn direction(&self) -> Option<(f64, f64)> {
        let (x, y) = if self.vertical {
            self.glyph_to_device.transform_delta(0.0, -1.0)
        } else {
            self.glyph_to_device.transform_delta(1.0, 0.0)
        };
        let len = x.hypot(y);
        (len > 0.0 && len.is_finite()).then(|| (x / len, y / len))
    }

    /// The device-space vectors from a point on the baseline to the
    /// font's ascent and descent.
    fn extent_vectors(&self) -> ((f64, f64), (f64, f64)) {
        let at = |e: f64| {
            if self.vertical {
                self.glyph_to_device.transform_delta(e, 0.0)
            } else {
                self.glyph_to_device.transform_delta(0.0, e)
            }
        };
        (at(self.ascent), at(self.descent))
    }

    /// The device-space bounding box `[x0, y0, x1, y1]` of the stretch of
    /// baseline from `from` to `to`, between the font's ascent and descent.
    fn span_box(&self, from: (f64, f64), to: (f64, f64)) -> [f64; 4] {
        let (up, down) = self.extent_vectors();
        let mut bbox = EMPTY_BOX;
        for (x, y) in [from, to] {
            for (ex, ey) in [up, down] {
                grow(&mut bbox, (x + ex, y + ey));
            }
        }
        bbox
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

/// A box that grows to cover the first point added to it.
const EMPTY_BOX: [f64; 4] = [
    f64::INFINITY,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NEG_INFINITY,
];

fn grow(bbox: &mut [f64; 4], (x, y): (f64, f64)) {
    *bbox = [
        bbox[0].min(x),
        bbox[1].min(y),
        bbox[2].max(x),
        bbox[3].max(y),
    ];
}

fn union(bbox: &mut [f64; 4], other: [f64; 4]) {
    grow(bbox, (other[0], other[1]));
    grow(bbox, (other[2], other[3]));
}

/// Every [`TextRunParams`] in `list`, in content order, at every depth:
/// inside transparency groups, soft-masked content, and the layers that
/// `layers` shows (`LayerSet::new()` shows the document's defaults). Runs
/// in a soft mask are skipped: a mask is never seen as text.
///
/// Invisible runs ([`invisible`](TextRunParams::invisible), such as an OCR
/// layer's) are included; drop them to keep only visible text.
pub fn text_runs<'a>(list: &'a DisplayList, layers: &LayerSet) -> Vec<&'a TextRunParams> {
    fn walk<'a>(list: &'a DisplayList, layers: &LayerSet, out: &mut Vec<&'a TextRunParams>) {
        for element in list.elements() {
            match element {
                DisplayElement::TextRun { params } => out.push(params),
                DisplayElement::Group { elements, .. } => walk(elements, layers, out),
                DisplayElement::SoftMasked { content, .. } => walk(content, layers, out),
                DisplayElement::OcgGroup {
                    elements,
                    visibility,
                } if layers.evaluate(visibility) => walk(elements, layers, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(list, layers, &mut out);
    out
}

/// A line of text assembled by [`text_lines`].
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextLine {
    /// The line's words, separated by single spaces.
    pub text: String,
    /// The words, in order.
    pub words: Vec<TextWord>,
    /// Device-space bounding box `[x0, y0, x1, y1]` of the line's runs,
    /// between their fonts' ascents and descents.
    pub bbox: [f64; 4],
    /// Vertical writing.
    pub vertical: bool,
}

/// A word of a [`TextLine`].
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextWord {
    /// The word: a stretch of text with no space in it and none drawn as a
    /// gap.
    pub text: String,
    /// Byte range of the word in [`TextLine::text`].
    pub range: Range<usize>,
    /// Device-space bounding box `[x0, y0, x1, y1]` of the word's glyphs;
    /// `None` when its runs carry no glyphs
    /// ([`TextExtraction::Runs`](crate::device::TextExtraction::Runs)).
    pub bbox: Option<[f64; 4]>,
}

/// Assemble `runs`, in content order, into lines of words.
///
/// A run joins the line before it when it is written in the same direction
/// and starts within [`LINE_TOLERANCE`] of that line's last run's baseline,
/// no more than [`BACKWARD_LIMIT`] behind where that run ended; otherwise
/// it starts a new line. Words are separated by the spaces the document
/// shows, by the gaps runs record in
/// [`word_breaks`](TextRunParams::word_breaks), and by a gap of at least
/// [`WORD_GAP`] between runs; runs with no gap between them — a change of
/// font mid-word, a superscript — continue the word. Lines without a word
/// are left out.
///
/// Content order is kept throughout: nothing is sorted, and columns,
/// tables and reading order are not detected.
pub fn text_lines<'a>(runs: impl IntoIterator<Item = &'a TextRunParams>) -> Vec<TextLine> {
    let mut lines = Vec::new();
    let mut line: Option<LineBuilder<'a>> = None;
    let mut prev: Option<&'a TextRunParams> = None;
    for run in runs {
        match (line.as_mut(), prev.and_then(|prev| line_gap(prev, run))) {
            (Some(builder), Some(gap)) => builder.add(run, gap),
            _ => {
                lines.extend(line.take().and_then(LineBuilder::finish));
                let mut builder = LineBuilder::new(run.vertical);
                builder.add(run, false);
                line = Some(builder);
            }
        }
        prev = Some(run);
    }
    lines.extend(line.and_then(LineBuilder::finish));
    lines
}

/// Whether `next` continues the line `prev` ends, and if so whether a
/// word's gap separates them.
fn line_gap(prev: &TextRunParams, next: &TextRunParams) -> Option<bool> {
    if prev.vertical != next.vertical {
        return None;
    }
    let (Some(a), Some(b)) = (prev.direction(), next.direction()) else {
        return Some(false);
    };
    if a.0 * b.0 + a.1 * b.1 < PARALLEL {
        return None;
    }
    let Some((along, across)) = prev.offset_from_end(next.start) else {
        return Some(false);
    };
    (across.abs() <= LINE_TOLERANCE && along >= -BACKWARD_LIMIT).then_some(along >= WORD_GAP)
}

/// A line being assembled.
struct LineBuilder<'a> {
    words: Vec<WordBuilder<'a>>,
    /// The next piece of text starts a new word.
    new_word: bool,
    bbox: [f64; 4],
    vertical: bool,
}

/// A word being assembled: its text, and the stretches of runs' text it
/// comes from.
struct WordBuilder<'a> {
    text: String,
    pieces: Vec<(&'a TextRunParams, Range<usize>)>,
}

impl<'a> LineBuilder<'a> {
    fn new(vertical: bool) -> Self {
        Self {
            words: Vec::new(),
            new_word: true,
            bbox: EMPTY_BOX,
            vertical,
        }
    }

    /// Add `run`'s text, after a word's gap when `gap`.
    fn add(&mut self, run: &'a TextRunParams, gap: bool) {
        self.new_word |= gap;
        union(&mut self.bbox, run.span_box(run.start, run.end));
        let mut breaks = run.word_breaks.iter().map(|&b| b as usize).peekable();
        let mut piece: Option<(usize, bool)> = None;
        for (i, ch) in run.text.char_indices() {
            let at_break = breaks.next_if(|&b| b <= i).is_some_and(|b| b == i);
            if at_break || ch.is_whitespace() {
                if let Some((start, starts_word)) = piece.take() {
                    self.push_piece(run, start..i, starts_word);
                }
                self.new_word = true;
            }
            if !ch.is_whitespace() && piece.is_none() {
                piece = Some((i, std::mem::take(&mut self.new_word)));
            }
        }
        if let Some((start, starts_word)) = piece {
            self.push_piece(run, start..run.text.len(), starts_word);
        }
    }

    fn push_piece(&mut self, run: &'a TextRunParams, range: Range<usize>, starts_word: bool) {
        let text = &run.text[range.clone()];
        match self.words.last_mut() {
            Some(word) if !starts_word => {
                word.text.push_str(text);
                word.pieces.push((run, range));
            }
            _ => self.words.push(WordBuilder {
                text: text.to_string(),
                pieces: vec![(run, range)],
            }),
        }
    }

    fn finish(self) -> Option<TextLine> {
        if self.words.is_empty() {
            return None;
        }
        let mut text = String::new();
        let mut words = Vec::with_capacity(self.words.len());
        for word in self.words {
            if !text.is_empty() {
                text.push(' ');
            }
            let start = text.len();
            text.push_str(&word.text);
            words.push(TextWord {
                range: start..text.len(),
                bbox: word_box(&word.pieces),
                text: word.text,
            });
        }
        Some(TextLine {
            text,
            words,
            bbox: self.bbox,
            vertical: self.vertical,
        })
    }
}

/// The bounding box of the glyphs whose text makes up `pieces`, when their
/// runs carry glyphs.
fn word_box(pieces: &[(&TextRunParams, Range<usize>)]) -> Option<[f64; 4]> {
    let mut bbox = EMPTY_BOX;
    for (run, range) in pieces {
        if run.glyphs.is_empty() {
            return None;
        }
        let (start, end) = (range.start as u32, range.end as u32);
        for glyph in &run.glyphs {
            let r = &glyph.text_range;
            // A glyph with no text of its own (the rest of an ActualText
            // span, an unmapped glyph) belongs where it sits.
            let inside = if r.is_empty() {
                start <= r.start && r.start <= end
            } else {
                r.start < end && r.end > start
            };
            if inside {
                let (ox, oy) = glyph.origin;
                let to = (ox + glyph.advance.0, oy + glyph.advance.1);
                union(&mut bbox, run.span_box(glyph.origin, to));
            }
        }
    }
    (bbox[0] <= bbox[2]).then_some(bbox)
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

    /// A 10-point horizontal run (font height 10 device units) with no
    /// glyphs, as `TextExtraction::Runs` records it.
    fn text_run(text: &str, start: (f64, f64), end: (f64, f64), breaks: &[u32]) -> TextRunParams {
        TextRunParams {
            text: text.into(),
            glyph_to_device: Matrix::new(0.01, 0.0, 0.0, 0.01, start.0, start.1),
            ascent: 800.0,
            descent: -200.0,
            start,
            end,
            word_breaks: breaks.to_vec(),
            ..TextRunParams::default()
        }
    }

    fn line_texts(runs: &[TextRunParams]) -> Vec<String> {
        text_lines(runs).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn lines_join_runs_along_a_baseline() {
        let runs = [
            // Words set apart by distance inside a run, and a shown space.
            text_run("PaperTitle", (0.0, 0.0), (50.0, 0.0), &[5]),
            // No gap: a change of font mid-word continues the word.
            text_run("s", (50.2, 0.0), (54.0, 0.0), &[]),
            // A superscript, just after: same line, same word.
            text_run("2", (54.0, 3.0), (57.0, 3.0), &[]),
            // A word's gap from the superscript's end.
            text_run("and more", (62.0, 0.0), (100.0, 0.0), &[]),
            // The next line down.
            text_run("Next", (0.0, -12.0), (20.0, -12.0), &[]),
            // Far back along the same baseline: a new line.
            text_run("back", (0.0, -12.5), (20.0, -12.5), &[]),
        ];
        assert_eq!(
            line_texts(&runs),
            ["Paper Titles2 and more", "Next", "back"]
        );
        let lines = text_lines(&runs);
        let words: Vec<(&str, Range<usize>)> = lines[0]
            .words
            .iter()
            .map(|w| (w.text.as_str(), w.range.clone()))
            .collect();
        assert_eq!(
            words,
            [
                ("Paper", 0..5),
                ("Titles2", 6..13),
                ("and", 14..17),
                ("more", 18..22)
            ]
        );
        // No glyphs, so no word boxes; the line box spans its runs from
        // descent (-2) to ascent (+8), the superscript 3 higher.
        assert!(lines[0].words.iter().all(|w| w.bbox.is_none()));
        assert_eq!(lines[0].bbox, [0.0, -2.0, 100.0, 11.0]);
    }

    #[test]
    fn lines_split_on_direction_and_skip_empty_text() {
        let mut vertical = text_run("縦", (50.0, 0.0), (50.0, -10.0), &[]);
        vertical.vertical = true;
        let runs = [
            text_run("a", (0.0, 0.0), (5.0, 0.0), &[]),
            vertical,
            text_run("", (0.0, -40.0), (5.0, -40.0), &[]),
            text_run(" ", (0.0, -60.0), (5.0, -60.0), &[]),
        ];
        let lines = text_lines(&runs);
        let found: Vec<(&str, bool)> = lines
            .iter()
            .map(|l| (l.text.as_str(), l.vertical))
            .collect();
        assert_eq!(found, [("a", false), ("縦", true)]);
    }

    #[test]
    fn word_boxes_cover_their_glyphs() {
        use crate::device::ShownGlyph;
        let glyph = |range: Range<u32>, x: f64| ShownGlyph {
            text_range: range,
            origin: (x, 0.0),
            advance: (5.0, 0.0),
            ..ShownGlyph::default()
        };
        let mut run = text_run("abcd", (0.0, 0.0), (25.0, 0.0), &[2]);
        run.glyphs = vec![
            glyph(0..1, 0.0),
            glyph(1..2, 5.0),
            glyph(2..3, 15.0),
            glyph(3..4, 20.0),
        ];
        let lines = text_lines([&run]);
        let boxes: Vec<Option<[f64; 4]>> = lines[0].words.iter().map(|w| w.bbox).collect();
        assert_eq!(
            boxes,
            [Some([0.0, -2.0, 10.0, 8.0]), Some([15.0, -2.0, 25.0, 8.0])]
        );
    }

    #[test]
    fn a_degenerate_run_is_always_continued() {
        let r = run(Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0), false);
        assert_eq!(r.step_to((500.0, 500.0)), GlyphStep::Continues);
    }
}
