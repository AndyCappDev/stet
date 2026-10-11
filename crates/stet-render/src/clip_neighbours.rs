// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which edges of a page's clip paths have another clip against them.
//!
//! Two clipped regions that share an edge must both cut that edge hard: if
//! each covered the pixels along it in part, painting one and then the
//! other would leave the background showing between them. An edge with
//! nothing on its far side has no such neighbour and may be smooth.
//!
//! [`ClipNeighbours`] is worked out once for a display list. It cuts every
//! clip path into short straight pieces and, for each piece, looks for a
//! piece of a *different* path that runs along it with its interior on the
//! opposite side. A path lying inside another and sharing its edge is on
//! the same side, and is not a neighbour: nothing shows between the two.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::FillRule;
use stet_graphics::device::ClipParams;
use stet_graphics::display_list::{DisplayElement, DisplayList};

type Point = (f64, f64);

/// The longest piece an edge is cut into, in pixels of the display list.
const PIECE: f64 = 2.0;
/// The farthest apart two edges may be and still be recorded as running
/// together. How near they must be to count is decided when rendering, by
/// [`NEAR`] at the scale of the output.
const REACH: f64 = 1.5;
/// How near, in output pixels, a neighbour's edge must be for an edge to
/// stay hard. Two regions further apart than this do not tile: there is a
/// sliver of background between them whichever way they are drawn.
const NEAR: f64 = 0.35;
/// The sine of the widest angle between two pieces that run together.
const PARALLEL: f64 = 0.2;
/// How far to either side of a piece its path is asked "inside?".
const SIDE: f64 = 0.25;
/// The side of a cell of the grid pieces are found by. A piece's
/// neighbours are within [`REACH`] of it and at most a piece along, so
/// they are in its cell or one touching it.
const CELL: f64 = 3.0;
/// The height of the bands of rows a path's edges are sorted into. A curve
/// is flattened to chords about this long, so a band holds little more
/// than the edges that cross one row.
const BAND: f64 = 1.0;
/// Past this many pieces, or entries in the bands of rows, the page is
/// left with every clip hard: the table would cost more than the page.
const BUDGET: usize = 4_000_000;

/// A piece of a clip path's edge that another clip runs along from the
/// other side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MetPiece {
    /// Where the piece starts, in the display list's device space.
    pub a: [f32; 2],
    /// Where it ends.
    pub b: [f32; 2],
    /// How far off the nearest neighbour's edge is, in the same units.
    pub gap: f32,
}

/// What is known of one clip path's edges.
#[derive(Debug, Default)]
struct ClipEdges {
    /// How many pieces its outline was cut into.
    pieces: u32,
    /// Those with a neighbour.
    met: Vec<MetPiece>,
    /// An axis-aligned rectangle, which is cut hard along pixel edges
    /// whatever is beside it.
    rect: bool,
}

/// How a clip path's edges are to be drawn.
#[derive(Debug, PartialEq)]
#[cfg_attr(not(test), expect(dead_code, reason = "no clip asks yet"))]
pub(crate) enum ClipEdgeState<'a> {
    /// Every edge has a neighbour, or the path is one this table does not
    /// know: hard throughout, as clips have always been.
    Hard,
    /// No edge has a neighbour.
    Smooth,
    /// Some edges have one. These are the pieces that do; a piece counts
    /// where its `gap` is no more than `within`.
    Mixed {
        /// Every piece with a neighbour in reach.
        met: &'a [MetPiece],
        /// The widest gap that still makes a piece hard at this scale.
        within: f32,
    },
}

/// For each clip path of a display list, the pieces of its edge that have
/// a neighbouring clip on the other side.
#[derive(Debug, Default)]
pub(crate) struct ClipNeighbours {
    clips: HashMap<u64, ClipEdges>,
}

/// What a clip path is known by in a [`ClipNeighbours`]: its segments, its
/// fill rule and the matrix that places it.
pub(crate) fn clip_key(path: &PsPath, params: &ClipParams) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(&params.fill_rule).hash(&mut hasher);
    let m = &params.ctm;
    for v in [m.a, m.b, m.c, m.d, m.tx, m.ty] {
        v.to_bits().hash(&mut hasher);
    }
    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => {
                0u8.hash(&mut hasher);
                x.to_bits().hash(&mut hasher);
                y.to_bits().hash(&mut hasher);
            }
            PathSegment::LineTo(x, y) => {
                1u8.hash(&mut hasher);
                x.to_bits().hash(&mut hasher);
                y.to_bits().hash(&mut hasher);
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                2u8.hash(&mut hasher);
                for v in [x1, y1, x2, y2, x3, y3] {
                    v.to_bits().hash(&mut hasher);
                }
            }
            PathSegment::ClosePath => 3u8.hash(&mut hasher),
        }
    }
    hasher.finish()
}

impl ClipNeighbours {
    /// Work out the neighbours of every clip path in `list` and the lists
    /// nested in it.
    ///
    /// Clips set by stroking are left out, and so are clips inside pattern
    /// tiles, which are lists of their own in a space of their own. Both
    /// stay hard.
    pub(crate) fn build(list: &DisplayList) -> Self {
        let mut shapes = Vec::new();
        let mut seen = HashSet::new();
        collect(list, &mut seen, &mut shapes);
        // A page whose clips are all rectangles has nothing to smooth.
        if shapes.iter().all(|s| s.rect) {
            return Self::default();
        }
        // Only what lies near a path that might be smoothed matters.
        let area = Area::around(shapes.iter().filter(|s| !s.rect));
        if !area.within_budget(&shapes) {
            return Self::default();
        }

        let cut_one = |(id, shape): (usize, &Shape)| {
            let sides = Sides::new(&shape.edges, shape.even_odd, &area);
            let mut pieces = Vec::new();
            cut(shape, id as u32, &sides, &area, &mut pieces);
            pieces
        };
        #[cfg(feature = "parallel")]
        let cuts: Vec<Vec<Piece>> = shapes.par_iter().enumerate().map(cut_one).collect();
        #[cfg(not(feature = "parallel"))]
        let cuts: Vec<Vec<Piece>> = shapes.iter().enumerate().map(cut_one).collect();

        let mut clips: HashMap<u64, ClipEdges> = shapes
            .iter()
            .zip(&cuts)
            .map(|(shape, pieces)| {
                (
                    shape.key,
                    ClipEdges {
                        pieces: pieces.len() as u32,
                        met: Vec::new(),
                        rect: shape.rect,
                    },
                )
            })
            .collect();
        let pieces: Vec<Piece> = cuts.into_iter().flatten().collect();
        let grid = Grid::new(&pieces, &area);

        let meet = |piece: &Piece| {
            if shapes[piece.shape as usize].rect {
                return None;
            }
            let gap = nearest_neighbour(piece, &pieces, &grid)?;
            Some((
                piece.shape,
                MetPiece {
                    a: [piece.a.0 as f32, piece.a.1 as f32],
                    b: [piece.b.0 as f32, piece.b.1 as f32],
                    gap: gap as f32,
                },
            ))
        };
        #[cfg(feature = "parallel")]
        let met: Vec<(u32, MetPiece)> = pieces.par_iter().filter_map(meet).collect();
        #[cfg(not(feature = "parallel"))]
        let met: Vec<(u32, MetPiece)> = pieces.iter().filter_map(meet).collect();
        for (shape, piece) in met {
            clips
                .get_mut(&shapes[shape as usize].key)
                .expect("every shape has an entry")
                .met
                .push(piece);
        }
        Self { clips }
    }

    /// How the edges of the clip path known by `key` are to be drawn when
    /// a pixel of the display list comes out `scale` pixels across.
    #[cfg_attr(not(test), expect(dead_code, reason = "no clip asks yet"))]
    pub(crate) fn edges(&self, key: u64, scale: f32) -> ClipEdgeState<'_> {
        let Some(edges) = self.clips.get(&key) else {
            return ClipEdgeState::Hard;
        };
        // A scale that is not a positive number leaves the clip as it was.
        if edges.rect || scale.is_nan() || scale <= 0.0 {
            return ClipEdgeState::Hard;
        }
        let within = NEAR as f32 / scale;
        let hard = edges.met.iter().filter(|p| p.gap <= within).count();
        if hard == 0 {
            ClipEdgeState::Smooth
        } else if hard == edges.pieces as usize {
            ClipEdgeState::Hard
        } else {
            ClipEdgeState::Mixed {
                met: &edges.met,
                within,
            }
        }
    }
}

/// A clip path flattened to straight edges in device space.
struct Shape {
    key: u64,
    edges: Vec<(Point, Point)>,
    even_odd: bool,
    rect: bool,
}

fn collect(list: &DisplayList, seen: &mut HashSet<u64>, out: &mut Vec<Shape>) {
    for elem in list.elements() {
        match elem {
            DisplayElement::Clip { path, params } if params.stroke_params.is_none() => {
                let key = clip_key(path, params);
                if seen.insert(key) {
                    let edges = flatten(path, &params.ctm);
                    out.push(Shape {
                        key,
                        rect: is_rect(&edges),
                        edges,
                        even_odd: matches!(params.fill_rule, FillRule::EvenOdd),
                    });
                }
            }
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                collect(elements, seen, out)
            }
            DisplayElement::SoftMasked { mask, content, .. } => {
                collect(mask, seen, out);
                collect(content, seen, out);
            }
            _ => {}
        }
    }
}

/// The edges of `path` under `m`, curves as chords about a pixel long and
/// every subpath closed, as filling closes it.
fn flatten(path: &PsPath, m: &Matrix) -> Vec<(Point, Point)> {
    let at = |x: f64, y: f64| (m.a * x + m.c * y + m.tx, m.b * x + m.d * y + m.ty);
    let mut edges = Vec::new();
    let mut start = (0.0, 0.0);
    let mut cur = (0.0, 0.0);
    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => {
                edges.push((cur, start));
                start = at(x, y);
                cur = start;
            }
            PathSegment::LineTo(x, y) => {
                let p = at(x, y);
                edges.push((cur, p));
                cur = p;
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                let (p0, p1, p2, p3) = (cur, at(x1, y1), at(x2, y2), at(x3, y3));
                let length = distance(p0, p1) + distance(p1, p2) + distance(p2, p3);
                // Not finite, or absurd: one chord, which the checks below
                // then drop or bound.
                let steps = if length.is_finite() {
                    (length.ceil() as usize).clamp(1, 4096)
                } else {
                    1
                };
                let mut prev = p0;
                for k in 1..=steps {
                    let t = k as f64 / steps as f64;
                    let u = 1.0 - t;
                    let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    let q = (
                        b0 * p0.0 + b1 * p1.0 + b2 * p2.0 + b3 * p3.0,
                        b0 * p0.1 + b1 * p1.1 + b2 * p2.1 + b3 * p3.1,
                    );
                    edges.push((prev, q));
                    prev = q;
                }
                cur = p3;
            }
            PathSegment::ClosePath => {
                edges.push((cur, start));
                cur = start;
            }
        }
    }
    edges.push((cur, start));
    edges.retain(|(a, b)| {
        a != b && a.0.is_finite() && a.1.is_finite() && b.0.is_finite() && b.1.is_finite()
    });
    edges
}

fn distance(a: Point, b: Point) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Four edges, each along an axis.
fn is_rect(edges: &[(Point, Point)]) -> bool {
    edges.len() == 4 && edges.iter().all(|(a, b)| a.0 == b.0 || a.1 == b.1)
}

/// The part of device space the table is concerned with.
struct Area {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Area {
    /// The box of the given shapes' points, and a little more.
    fn around<'a>(shapes: impl Iterator<Item = &'a Shape>) -> Self {
        let mut area = Area {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
        };
        for (a, b) in shapes.flat_map(|s| &s.edges) {
            for p in [a, b] {
                area.x0 = area.x0.min(p.0);
                area.y0 = area.y0.min(p.1);
                area.x1 = area.x1.max(p.0);
                area.y1 = area.y1.max(p.1);
            }
        }
        let margin = CELL + REACH;
        area.x0 -= margin;
        area.y0 -= margin;
        area.x1 += margin;
        area.y1 += margin;
        area
    }

    /// Whether cutting these shapes up within the area stays inside
    /// [`BUDGET`]: in pieces, in rows of the area, and in the entries the
    /// rows' lists of edges would hold.
    fn within_budget(&self, shapes: &[Shape]) -> bool {
        let rows = (self.y1 - self.y0) / BAND.min(CELL);
        let cols = (self.x1 - self.x0) / CELL;
        // Not finite, or beyond counting.
        if !(rows < BUDGET as f64 && cols < BUDGET as f64) {
            return false;
        }
        let mut spent = 0.0;
        for (a, b) in shapes.iter().flat_map(|s| &s.edges) {
            if let Some((t0, t1)) = self.clip(*a, *b) {
                let part = t1 - t0;
                spent += (distance(*a, *b) * part / PIECE).ceil();
                spent += ((a.1 - b.1).abs() * part / BAND).ceil() + 1.0;
                if spent > BUDGET as f64 {
                    return false;
                }
            }
        }
        true
    }

    /// The stretch of the edge from `a` to `b` inside the area, as the
    /// parameters it runs between, or `None` if none of it is.
    fn clip(&self, a: Point, b: Point) -> Option<(f64, f64)> {
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        for (p, q) in [
            (-dx, a.0 - self.x0),
            (dx, self.x1 - a.0),
            (-dy, a.1 - self.y0),
            (dy, self.y1 - a.1),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return None;
                }
            } else if p < 0.0 {
                t0 = t0.max(q / p);
            } else {
                t1 = t1.min(q / p);
            }
        }
        (t0 < t1).then_some((t0, t1))
    }
}

/// Answers "is this point inside the path?" for points within an [`Area`].
struct Sides<'a> {
    edges: &'a [(Point, Point)],
    /// For each band of rows of the area, the edges that cross it.
    bands: Vec<Vec<u32>>,
    top: f64,
    even_odd: bool,
}

impl<'a> Sides<'a> {
    fn new(edges: &'a [(Point, Point)], even_odd: bool, area: &Area) -> Self {
        let count = (((area.y1 - area.y0) / BAND).ceil().max(1.0)) as usize;
        let mut bands = vec![Vec::new(); count];
        for (i, (a, b)) in edges.iter().enumerate() {
            let (lo, hi) = (a.1.min(b.1), a.1.max(b.1));
            if hi < area.y0 || lo > area.y1 {
                continue;
            }
            let first = (((lo - area.y0) / BAND).floor().max(0.0)) as usize;
            let last = ((((hi - area.y0) / BAND).floor()).max(0.0) as usize).min(count - 1);
            for band in &mut bands[first.min(last)..=last] {
                band.push(i as u32);
            }
        }
        Self {
            edges,
            bands,
            top: area.y0,
            even_odd,
        }
    }

    fn inside(&self, p: Point) -> bool {
        let band = ((p.1 - self.top) / BAND).floor();
        if !(band >= 0.0 && (band as usize) < self.bands.len()) {
            return false;
        }
        let mut winding = 0i32;
        for &i in &self.bands[band as usize] {
            let (a, b) = self.edges[i as usize];
            if (a.1 <= p.1) != (b.1 <= p.1) {
                let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
                if x > p.0 {
                    winding += if b.1 > a.1 { 1 } else { -1 };
                }
            }
        }
        if self.even_odd {
            winding & 1 != 0
        } else {
            winding != 0
        }
    }
}

/// A short straight stretch of a shape's outline.
struct Piece {
    a: Point,
    b: Point,
    mid: Point,
    /// Unit vector along it.
    along: Point,
    /// Unit vector across it, pointing into the shape.
    inward: Point,
    shape: u32,
}

/// Cut the outline of `shape` into pieces. An edge with the shape on both
/// sides of it, or on neither, bounds nothing and yields none.
fn cut(shape: &Shape, id: u32, sides: &Sides<'_>, area: &Area, out: &mut Vec<Piece>) {
    // A path of very many edges is one shape, so sharing the shapes out
    // among threads does nothing for it: share out its edges.
    #[cfg(feature = "parallel")]
    if shape.edges.len() >= 4096 {
        let runs: Vec<Vec<Piece>> = shape
            .edges
            .par_chunks(1024)
            .map(|edges| {
                let mut pieces = Vec::new();
                cut_edges(edges, id, sides, area, &mut pieces);
                pieces
            })
            .collect();
        out.extend(runs.into_iter().flatten());
        return;
    }
    cut_edges(&shape.edges, id, sides, area, out);
}

fn cut_edges(
    edges: &[(Point, Point)],
    id: u32,
    sides: &Sides<'_>,
    area: &Area,
    out: &mut Vec<Piece>,
) {
    for &(a, b) in edges {
        let Some((t0, t1)) = area.clip(a, b) else {
            continue;
        };
        let length = distance(a, b);
        let along = ((b.0 - a.0) / length, (b.1 - a.1) / length);
        let across = (-along.1, along.0);
        let count = ((length * (t1 - t0) / PIECE).ceil() as usize).clamp(1, BUDGET);
        let at = |t: f64| (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
        for k in 0..count {
            let from = t0 + (t1 - t0) * k as f64 / count as f64;
            let to = t0 + (t1 - t0) * (k + 1) as f64 / count as f64;
            let mid = at((from + to) / 2.0);
            let left = sides.inside((mid.0 + SIDE * across.0, mid.1 + SIDE * across.1));
            let right = sides.inside((mid.0 - SIDE * across.0, mid.1 - SIDE * across.1));
            let inward = match (left, right) {
                (true, false) => across,
                (false, true) => (-across.0, -across.1),
                _ => continue,
            };
            out.push(Piece {
                a: at(from),
                b: at(to),
                mid,
                along,
                inward,
                shape: id,
            });
        }
    }
}

/// Pieces sorted by the cell of the area their middle falls in, a row of
/// cells at a time, so that the pieces of a run of cells in one row are a
/// run of this list.
struct Grid {
    /// `(column, piece)`, by row and then by column.
    cells: Vec<(u32, u32)>,
    /// Where each row starts in `cells`, and one past the last.
    rows: Vec<u32>,
    x0: f64,
    y0: f64,
}

impl Grid {
    fn new(pieces: &[Piece], area: &Area) -> Self {
        let mut grid = Grid {
            cells: Vec::new(),
            rows: Vec::new(),
            x0: area.x0,
            y0: area.y0,
        };
        let count = (((area.y1 - area.y0) / CELL).floor().max(0.0)) as usize + 1;
        let mut keyed: Vec<(u32, u32, u32)> = pieces
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (col, row) = grid.cell(p.mid);
                (row.min(count as u32 - 1), col, i as u32)
            })
            .collect();
        keyed.sort_unstable();
        grid.rows = vec![0; count + 1];
        for &(row, _, _) in &keyed {
            grid.rows[row as usize + 1] += 1;
        }
        for r in 0..count {
            grid.rows[r + 1] += grid.rows[r];
        }
        grid.cells = keyed.into_iter().map(|(_, col, i)| (col, i)).collect();
        grid
    }

    /// The column and row of the cell `p` is in. Pieces lie inside the
    /// area, so neither is negative.
    fn cell(&self, p: Point) -> (u32, u32) {
        (
            ((p.0 - self.x0) / CELL).max(0.0) as u32,
            ((p.1 - self.y0) / CELL).max(0.0) as u32,
        )
    }

    /// Every piece in the cell `p` is in and the eight around it.
    fn around(&self, p: Point) -> impl Iterator<Item = u32> + '_ {
        let (col, row) = self.cell(p);
        let last = self.rows.len().saturating_sub(2) as u32;
        (row.saturating_sub(1)..=(row + 1).min(last)).flat_map(move |r| {
            let run =
                &self.cells[self.rows[r as usize] as usize..self.rows[r as usize + 1] as usize];
            let from = run.partition_point(|&(c, _)| c + 1 < col);
            run[from..]
                .iter()
                .take_while(move |&&(c, _)| c <= col + 1)
                .map(|&(_, i)| i)
        })
    }
}

/// How far off the nearest piece of another shape is that runs along
/// `piece` with its interior on the other side, if one is within reach.
///
/// The test reads the same from either piece, so two neighbours always
/// agree that they are neighbours, and on how far apart.
fn nearest_neighbour(piece: &Piece, pieces: &[Piece], grid: &Grid) -> Option<f64> {
    let mut nearest: Option<f64> = None;
    for j in grid.around(piece.mid) {
        let other = &pieces[j as usize];
        if other.shape == piece.shape {
            continue;
        }
        let facing = piece.inward.0 * other.inward.0 + piece.inward.1 * other.inward.1;
        if facing >= 0.0 {
            continue;
        }
        let d = (other.mid.0 - piece.mid.0, other.mid.1 - piece.mid.1);
        let cross = piece.along.0 * other.along.1 - piece.along.1 * other.along.0;
        if cross.abs() > PARALLEL {
            continue;
        }
        let along_here = d.0 * piece.along.0 + d.1 * piece.along.1;
        let along_there = d.0 * other.along.0 + d.1 * other.along.1;
        if along_here.abs() > REACH || along_there.abs() > REACH {
            continue;
        }
        let off_here = (d.0 * piece.along.1 - d.1 * piece.along.0).abs();
        let off_there = (d.0 * other.along.1 - d.1 * other.along.0).abs();
        let gap = off_here.max(off_there);
        if gap <= REACH && nearest.is_none_or(|n| gap < n) {
            nearest = Some(gap);
        }
    }
    nearest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(segments: Vec<PathSegment>) -> DisplayElement {
        DisplayElement::Clip {
            path: PsPath { segments },
            params: ClipParams {
                fill_rule: FillRule::NonZeroWinding,
                ctm: Matrix::default(),
                stroke_params: None,
            },
        }
    }

    fn polygon(points: &[Point]) -> DisplayElement {
        let mut segments = vec![PathSegment::MoveTo(points[0].0, points[0].1)];
        segments.extend(points[1..].iter().map(|p| PathSegment::LineTo(p.0, p.1)));
        segments.push(PathSegment::ClosePath);
        clip(segments)
    }

    fn key(elem: &DisplayElement) -> u64 {
        let DisplayElement::Clip { path, params } = elem else {
            unreachable!()
        };
        clip_key(path, params)
    }

    fn table(elems: &[DisplayElement]) -> ClipNeighbours {
        let mut list = DisplayList::new();
        for e in elems {
            list.push(e.clone());
        }
        ClipNeighbours::build(&list)
    }

    /// The hard pieces of a mixed path, at the scale the list was made for.
    fn hard(state: &ClipEdgeState<'_>) -> Vec<MetPiece> {
        match state {
            ClipEdgeState::Mixed { met, within } => {
                met.iter().filter(|p| p.gap <= *within).copied().collect()
            }
            other => panic!("not mixed: {other:?}"),
        }
    }

    // Slanted, so that none of these is a rectangle.
    fn left_tile() -> DisplayElement {
        polygon(&[(10.0, 10.0), (50.0, 12.0), (52.0, 60.0), (8.0, 58.0)])
    }
    fn right_tile() -> DisplayElement {
        polygon(&[(50.0, 12.0), (90.0, 10.0), (92.0, 58.0), (52.0, 60.0)])
    }

    #[test]
    fn a_clip_alone_is_smooth() {
        let a = left_tile();
        let t = table(std::slice::from_ref(&a));
        assert_eq!(t.edges(key(&a), 1.0), ClipEdgeState::Smooth);
    }

    #[test]
    fn a_clip_the_table_has_not_seen_is_hard() {
        let t = table(&[left_tile()]);
        assert_eq!(t.edges(key(&right_tile()), 1.0), ClipEdgeState::Hard);
        assert_eq!(
            ClipNeighbours::default().edges(key(&left_tile()), 1.0),
            ClipEdgeState::Hard
        );
    }

    /// Two tiles side by side: the edge between them is hard on both, and
    /// nothing else is.
    #[test]
    fn tiles_are_hard_where_they_meet_and_only_there() {
        let (a, b) = (left_tile(), right_tile());
        let t = table(&[a.clone(), b.clone()]);
        for tile in [&a, &b] {
            let state = t.edges(key(tile), 1.0);
            let pieces = hard(&state);
            assert!(!pieces.is_empty());
            for p in &pieces {
                // On the line from (50, 12) to (52, 60).
                for end in [p.a, p.b] {
                    let x = 50.0 + (end[1] - 12.0) / 24.0;
                    assert!((end[0] - x).abs() < 0.01, "{end:?}");
                }
                assert!(p.gap < 0.01);
            }
            // The whole of the shared edge, 48 and a bit long.
            let length: f32 = pieces
                .iter()
                .map(|p| ((p.b[0] - p.a[0]).powi(2) + (p.b[1] - p.a[1]).powi(2)).sqrt())
                .sum();
            assert!((length - 48.04).abs() < 0.1, "{length}");
        }
    }

    /// A row of strips: the middle one has a neighbour on both long sides
    /// and its short ends free.
    #[test]
    fn a_strip_between_strips_keeps_its_free_ends() {
        let strip = |x: f64| polygon(&[(x, 10.0), (x + 4.0, 10.5), (x + 4.0, 80.5), (x, 80.0)]);
        let strips = [strip(20.0), strip(24.0), strip(28.0)];
        let t = table(&strips);
        let state = t.edges(key(&strips[1]), 1.0);
        let pieces = hard(&state);
        assert!(
            pieces.iter().all(|p| p.a[0] == p.b[0]),
            "only the long sides"
        );
        assert!(pieces.iter().any(|p| p.a[0] == 24.0));
        assert!(pieces.iter().any(|p| p.a[0] == 28.0));
    }

    /// A long edge against two short ones is hard along both.
    #[test]
    fn a_long_edge_meets_two_short_ones() {
        let long = polygon(&[(10.0, 10.0), (50.0, 11.0), (50.0, 91.0), (10.0, 90.0)]);
        let upper = polygon(&[(50.0, 11.0), (80.0, 10.0), (80.0, 50.0), (50.0, 51.0)]);
        let lower = polygon(&[(50.0, 51.0), (80.0, 50.0), (80.0, 90.0), (50.0, 91.0)]);
        let t = table(&[long.clone(), upper, lower]);
        let length: f32 = hard(&t.edges(key(&long), 1.0))
            .iter()
            .map(|p| (p.b[1] - p.a[1]).abs())
            .sum();
        assert!((length - 80.0).abs() < 0.01, "{length}");
    }

    /// One clip inside another, sharing an edge: both are on the same side
    /// of it, so nothing shows between them and the edge is free.
    #[test]
    fn a_clip_inside_another_is_not_its_neighbour() {
        let outer = left_tile();
        let inner = polygon(&[(10.0, 10.0), (50.0, 12.0), (30.0, 40.0)]);
        let t = table(&[outer.clone(), inner.clone()]);
        assert_eq!(t.edges(key(&outer), 1.0), ClipEdgeState::Smooth);
        assert_eq!(t.edges(key(&inner), 1.0), ClipEdgeState::Smooth);
    }

    /// The same path set twice is one path.
    #[test]
    fn a_clip_set_twice_is_not_its_own_neighbour() {
        let a = left_tile();
        let t = table(&[a.clone(), a.clone()]);
        assert_eq!(t.edges(key(&a), 1.0), ClipEdgeState::Smooth);
    }

    /// A disc, and a ring around it whose hole is the same circle drawn as
    /// two half-turns cut at other points: control points differ, the
    /// geometry does not.
    #[test]
    fn one_curve_cut_two_ways_is_still_a_neighbour() {
        const K: f64 = 0.552_284_749_830_793_6;
        let (cx, cy, r) = (100.0, 100.0, 40.0);
        let arc = |to: Point, c1: Point, c2: Point| PathSegment::CurveTo {
            x1: c1.0,
            y1: c1.1,
            x2: c2.0,
            y2: c2.1,
            x3: to.0,
            y3: to.1,
        };
        // Four quarter arcs from the east point.
        let quarter = |i: usize| {
            let dir = |j: usize| match j % 4 {
                0 => (1.0, 0.0),
                1 => (0.0, 1.0),
                2 => (-1.0, 0.0),
                _ => (0.0, -1.0),
            };
            let (a, b) = (dir(i), dir(i + 1));
            let p = |d: Point| (cx + r * d.0, cy + r * d.1);
            arc(
                p(b),
                (cx + r * (a.0 + K * b.0), cy + r * (a.1 + K * b.1)),
                (cx + r * (b.0 + K * a.0), cy + r * (b.1 + K * a.1)),
            )
        };
        let mut disc = vec![PathSegment::MoveTo(cx + r, cy)];
        disc.extend((0..4).map(quarter));
        disc.push(PathSegment::ClosePath);
        let disc = clip(disc);

        // The same circle from eight arcs, starting half a quarter round,
        // wound the other way as the hole of a square.
        let n = 8;
        let k = 4.0 / 3.0 * (std::f64::consts::PI / (2.0 * n as f64)).tan();
        let angle = |i: usize| -(std::f64::consts::TAU * (i as f64 + 0.5) / n as f64);
        let on = |t: f64| (cx + r * t.cos(), cy + r * t.sin());
        let mut ring = vec![
            PathSegment::MoveTo(40.0, 41.0),
            PathSegment::LineTo(160.0, 40.0),
            PathSegment::LineTo(161.0, 160.0),
            PathSegment::LineTo(40.0, 160.0),
            PathSegment::ClosePath,
            PathSegment::MoveTo(on(angle(0)).0, on(angle(0)).1),
        ];
        for i in 0..n {
            let (t0, t1) = (angle(i), angle(i + 1));
            let (p0, p1) = (on(t0), on(t1));
            // Tangents, for a turn in the direction of decreasing angle.
            let c1 = (p0.0 + k * r * t0.sin(), p0.1 - k * r * t0.cos());
            let c2 = (p1.0 - k * r * t1.sin(), p1.1 + k * r * t1.cos());
            ring.push(arc(p1, c1, c2));
        }
        ring.push(PathSegment::ClosePath);
        let ring = DisplayElement::Clip {
            path: PsPath { segments: ring },
            params: ClipParams {
                fill_rule: FillRule::EvenOdd,
                ctm: Matrix::default(),
                stroke_params: None,
            },
        };

        let t = table(&[disc.clone(), ring.clone()]);
        // Every piece of the disc has the ring's hole against it.
        assert_eq!(t.edges(key(&disc), 1.0), ClipEdgeState::Hard);
        // The ring is hard round its hole and free round its outside.
        let pieces = hard(&t.edges(key(&ring), 1.0));
        assert!(!pieces.is_empty());
        for p in &pieces {
            let d = ((f64::from(p.a[0]) - cx).powi(2) + (f64::from(p.a[1]) - cy).powi(2)).sqrt();
            assert!((d - r).abs() < 0.1, "{d}");
        }
    }

    /// Edges half a pixel apart are neighbours when the page is drawn at
    /// half size, where that is a quarter of a pixel, and not at full size.
    #[test]
    fn how_near_is_near_goes_by_the_scale_of_the_output() {
        let a = left_tile();
        let b = polygon(&[(50.5, 12.0), (90.0, 10.0), (92.0, 58.0), (52.5, 60.0)]);
        let t = table(&[a.clone(), b]);
        assert_eq!(t.edges(key(&a), 1.0), ClipEdgeState::Smooth);
        assert!(matches!(t.edges(key(&a), 0.5), ClipEdgeState::Mixed { .. }));
    }

    /// A rectangle is cut hard along pixel edges whatever is beside it, and
    /// still counts as a neighbour of what is.
    #[test]
    fn a_rectangle_is_hard_and_is_a_neighbour() {
        let rect = polygon(&[(52.0, 0.0), (120.0, 0.0), (120.0, 100.0), (52.0, 100.0)]);
        let beside = polygon(&[(10.0, 10.0), (52.0, 12.0), (52.0, 60.0), (8.0, 58.0)]);
        let t = table(&[rect.clone(), beside.clone()]);
        assert_eq!(t.edges(key(&rect), 1.0), ClipEdgeState::Hard);
        let pieces = hard(&t.edges(key(&beside), 1.0));
        assert!(pieces.iter().all(|p| p.a[0] == 52.0 && p.b[0] == 52.0));
        // Rectangles only: nothing to work out.
        let only = table(std::slice::from_ref(&rect));
        assert!(only.clips.is_empty());
    }

    /// Clips in nested lists are found, and are neighbours of clips
    /// outside them.
    #[test]
    fn clips_in_nested_lists_are_neighbours_too() {
        let (a, b) = (left_tile(), right_tile());
        let mut inner = DisplayList::new();
        inner.push(b.clone());
        let mut list = DisplayList::new();
        list.push(a.clone());
        list.push(DisplayElement::OcgGroup {
            elements: inner,
            visibility: stet_graphics::display_list::OcgVisibility::single(1, true),
        });
        let t = ClipNeighbours::build(&list);
        assert!(matches!(t.edges(key(&a), 1.0), ClipEdgeState::Mixed { .. }));
        assert!(matches!(t.edges(key(&b), 1.0), ClipEdgeState::Mixed { .. }));
    }

    /// The same segments placed by another matrix are another clip.
    #[test]
    fn the_matrix_is_part_of_what_a_clip_is() {
        let a = left_tile();
        let DisplayElement::Clip { path, params } = &a else {
            unreachable!()
        };
        let mut moved = params.clone();
        moved.ctm.tx = 100.0;
        assert_ne!(clip_key(path, params), clip_key(path, &moved));
    }
}
