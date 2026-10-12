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
//!
//! Two neighbours are painted one after the other, and that leaves a way
//! for the later one to be smooth after all: over the colour the earlier
//! one painted, if that colour is first carried across the edge to where
//! the later one will cover only part of a pixel. So the table also walks
//! the list as it will be painted and notes, for each clip path, when it
//! is painted through and by what, and says of each piece whether it may
//! lie [`Over`](Meeting::Over) its neighbours.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::FillRule;
use stet_graphics::device::{ClipParams, ImageColorSpace};
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
/// How far to either side of a piece its path is asked "inside?". Close,
/// so that a sliver of a path far thinner than a pixel still has two sides.
const SIDE: f64 = 1.0 / 64.0;
/// How far from an axis, as a sine, an edge may lean and still be taken to
/// run along the pixel grid.
const UPRIGHT: f64 = 1e-6;
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
/// How far to either side of an edge, in pixels of the display list, the
/// pieces that meet along it are asked what is painted: the one that lies
/// over must be painted there by one thing and no more, and one of those
/// it lies over by something.
const INSIDE: f64 = 0.25;
/// How deep behind the edge, in output pixels, the colour carried across
/// may come from: it is taken from a pixel one step out, whose centre is
/// at most this far behind the edge. Nothing painted later may reach
/// that far, or the pixel would not yet hold the colour it is to have.
const DEEP: f64 = 1.5;
/// The depths behind an edge that are asked about, as multiples of
/// [`DEEP`]: enough to know the answer for an output as coarse as one
/// pixel to eight of the display list's.
const DEPTHS: [f64; 9] = [0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0];
/// The side of a cell of the grid that paths are found by, for asking
/// which of them a point is in.
const COVER_CELL: f64 = 32.0;
/// The most paints through one clip path that are looked into. A path
/// with more is taken to be painted nowhere that can be vouched for.
const PAINTS: usize = 64;
/// The most edges a filled path may have and still be asked where it
/// reaches.
const PAINT_EDGES: usize = 65_536;

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
    /// How far off the nearest neighbour is that keeps this piece hard:
    /// one painted later, or one this piece cannot lie over. Infinite if
    /// there is none.
    hard: f32,
    /// How far off the nearest neighbour is that this piece may lie over.
    over: f32,
    /// How far off the nearest clip is that has this edge too, on the
    /// same side, and may be smooth along it as well. Two such are two
    /// layers of paint along the edge.
    stack: f32,
    /// How deep behind the edge, in pixels of the display list, nothing
    /// painted after this piece's path is first painted through reaches.
    deep: f32,
    /// Unit vector across the piece, into the path.
    inward: [f32; 2],
}

/// What is to be done with a piece of edge that has a neighbour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Meeting {
    /// No neighbour near enough at this scale: smooth, as a free edge is.
    Free,
    /// Cut hard, as its neighbours are along it.
    Hard,
    /// Smooth, over the colour of the neighbours painted before it, once
    /// that colour is carried across the edge.
    Over,
}

impl MetPiece {
    /// What to do with the piece when a pixel of the display list comes
    /// out `scale` pixels across.
    pub(crate) fn meeting(&self, scale: f32) -> Meeting {
        let within = NEAR as f32 / scale;
        if self.hard <= within {
            Meeting::Hard
        } else if self.over <= within {
            // The depth is in pixels of the display list and is wanted
            // in pixels of the output.
            if self.stack <= within || self.deep * scale < DEEP as f32 {
                Meeting::Hard
            } else {
                Meeting::Over
            }
        } else {
            Meeting::Free
        }
    }

    /// Unit vector across the piece, into the path.
    pub(crate) fn inward(&self) -> [f32; 2] {
        self.inward
    }
}

/// How many pieces with a neighbour are found by rows together.
const RUN: usize = 32;

/// What is known of one clip path's edges.
#[derive(Debug, Default)]
struct ClipEdges {
    /// How many pieces its outline was cut into.
    pieces: u32,
    /// Those with a neighbour.
    met: Vec<MetPiece>,
    /// The rows each run of [`RUN`] of those lies between, least y and
    /// greatest: a band of a page looks only at the runs that reach it.
    rows: Vec<[f32; 2]>,
    /// How many of those are hard at the scale first asked about, and the
    /// scale. A page is most often rendered at one scale throughout.
    hard: OnceLock<(f32, usize)>,
    /// Some of those may lie over a neighbour.
    over: bool,
    /// Nothing but edges along the axes, which are cut hard along pixel edges
    /// whatever is beside it.
    rect: bool,
}

/// How a clip path's edges are to be drawn.
#[derive(Debug, PartialEq)]
pub(crate) enum ClipEdgeState<'a> {
    /// Every edge is to be cut hard, or the path is one this table does
    /// not know: hard throughout, as clips have always been.
    Hard,
    /// No edge is to be cut hard: none has a neighbour, or those that
    /// have lie over theirs ([`ClipNeighbours::lying_over`]).
    Smooth,
    /// Some edges are to be cut hard. These are the pieces with a
    /// neighbour; the hard ones are those whose
    /// [`meeting`](MetPiece::meeting) at `scale` is [`Meeting::Hard`].
    Mixed {
        /// Every piece with a neighbour in reach.
        met: Met<'a>,
        /// How many output pixels a pixel of the display list comes to.
        scale: f32,
    },
}

/// The pieces of one clip path's edge that have a neighbour, in the order
/// they come along the path.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Met<'a>(&'a ClipEdges);

impl PartialEq for Met<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl<'a> Met<'a> {
    /// All of them.
    #[cfg(test)]
    pub(crate) fn iter(&self) -> std::slice::Iter<'a, MetPiece> {
        self.0.met.iter()
    }

    /// Those that may reach the rows from `rows[0]` to `rows[1]` of the
    /// display list's device space, and perhaps some that do not.
    pub(crate) fn between(&self, rows: [f32; 2]) -> impl Iterator<Item = &'a MetPiece> + use<'a> {
        self.0
            .met
            .chunks(RUN)
            .zip(&self.0.rows)
            .filter(move |(_, run)| run[1] >= rows[0] && run[0] <= rows[1])
            .flat_map(|(pieces, _)| pieces)
    }
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
        let mut walk = Walk::default();
        walk.list(list, &mut Stack::default(), 0);
        let Walk {
            shapes,
            paints,
            stacks,
            ..
        } = walk;
        // A page whose clips are all rectangles has nothing to smooth.
        if shapes.iter().all(|s| s.rect) {
            return Self::default();
        }
        // Only what lies near a path that might be smoothed matters.
        let area = Area::around(shapes.iter().filter(|s| !s.rect));
        if !area.within_budget(&shapes) {
            return Self::default();
        }

        fn cut_one<'a>(id: usize, shape: &'a Shape, area: &Area) -> (Sides<'a>, Vec<Piece>) {
            let sides = Sides::new(Cow::Borrowed(&shape.edges[..]), shape.even_odd, area);
            let mut pieces = Vec::new();
            cut(shape, id as u32, &sides, area, &mut pieces);
            (sides, pieces)
        }
        #[cfg(feature = "parallel")]
        let (sides, cuts): (Vec<Sides<'_>>, Vec<Vec<Piece>>) = shapes
            .par_iter()
            .enumerate()
            .map(|(id, shape)| cut_one(id, shape, &area))
            .unzip();
        #[cfg(not(feature = "parallel"))]
        let (sides, cuts): (Vec<Sides<'_>>, Vec<Vec<Piece>>) = shapes
            .iter()
            .enumerate()
            .map(|(id, shape)| cut_one(id, shape, &area))
            .unzip();

        let mut clips: HashMap<u64, ClipEdges> = shapes
            .iter()
            .zip(&cuts)
            .map(|(shape, pieces)| {
                (
                    shape.key,
                    ClipEdges {
                        pieces: pieces.len() as u32,
                        met: Vec::new(),
                        rows: Vec::new(),
                        hard: OnceLock::new(),
                        over: false,
                        rect: shape.rect,
                    },
                )
            })
            .collect();
        let pieces: Vec<Piece> = cuts.into_iter().flatten().collect();
        let grid = Grid::new(&pieces, &area);
        let page = Page {
            shapes: &shapes,
            sides: &sides,
            paints: &paints,
            stacks: &stacks,
            area: &area,
            pieces: &pieces,
            cover: Cover::new(&shapes, &area),
        };

        let meet = |room: &mut Room, index: usize| {
            let piece = &pieces[index];
            if shapes[piece.shape as usize].rect {
                return None;
            }
            Some((piece.shape, page.meet(index, &grid, room)?))
        };
        #[cfg(feature = "parallel")]
        let met: Vec<(u32, MetPiece)> = (0..pieces.len())
            .into_par_iter()
            .map_init(Room::default, meet)
            .flatten_iter()
            .collect();
        #[cfg(not(feature = "parallel"))]
        let met: Vec<(u32, MetPiece)> = {
            let mut room = Room::default();
            (0..pieces.len())
                .filter_map(|index| meet(&mut room, index))
                .collect()
        };
        for (shape, piece) in met {
            let edges = clips
                .get_mut(&shapes[shape as usize].key)
                .expect("every shape has an entry");
            edges.over |= piece.over.is_finite();
            edges.met.push(piece);
        }
        for edges in clips.values_mut() {
            edges.rows = edges
                .met
                .chunks(RUN)
                .map(|run| {
                    let ys = || run.iter().flat_map(|p| [p.a[1], p.b[1]]);
                    [
                        ys().fold(f32::INFINITY, f32::min),
                        ys().fold(f32::NEG_INFINITY, f32::max),
                    ]
                })
                .collect();
        }
        Self { clips }
    }

    /// The pieces of the clip path known by `key` that have a neighbour.
    #[cfg(test)]
    pub(crate) fn met(&self, key: u64) -> &[MetPiece] {
        self.clips.get(&key).map_or(&[], |edges| &edges.met)
    }

    /// How the edges of the clip path known by `key` are to be drawn when
    /// a pixel of the display list comes out `scale` pixels across.
    pub(crate) fn edges(&self, key: u64, scale: f32) -> ClipEdgeState<'_> {
        let Some(edges) = self.clips.get(&key) else {
            return ClipEdgeState::Hard;
        };
        // A scale that is not a positive number leaves the clip as it was.
        if edges.rect || scale.is_nan() || scale <= 0.0 {
            return ClipEdgeState::Hard;
        }
        let count = || {
            edges
                .met
                .iter()
                .filter(|p| p.meeting(scale) == Meeting::Hard)
                .count()
        };
        let hard = match *edges.hard.get_or_init(|| (scale, count())) {
            (at, hard) if at == scale => hard,
            _ => count(),
        };
        if hard == 0 {
            ClipEdgeState::Smooth
        } else if hard == edges.pieces as usize {
            ClipEdgeState::Hard
        } else {
            ClipEdgeState::Mixed {
                met: Met(edges),
                scale,
            }
        }
    }

    /// The pieces of the clip path known by `key` that lie over their
    /// neighbours at this scale: those the colour across the edge is to
    /// be carried over before anything is painted through the path. Only
    /// those that may reach the rows from `rows[0]` to `rows[1]` of the
    /// display list's device space are looked for.
    pub(crate) fn lying_over(
        &self,
        key: u64,
        scale: f32,
        rows: [f32; 2],
    ) -> impl Iterator<Item = &MetPiece> {
        self.clips
            .get(&key)
            .filter(|edges| edges.over && !edges.rect && scale > 0.0)
            .into_iter()
            .flat_map(move |edges| {
                Met(edges)
                    .between(rows)
                    .filter(move |p| p.meeting(scale) == Meeting::Over)
            })
    }
}

/// A clip path flattened to straight edges in device space.
struct Shape {
    key: u64,
    edges: Vec<(Point, Point)>,
    even_odd: bool,
    rect: bool,
    /// The box of its points: least x and y, then greatest.
    bounds: [f64; 4],
    painted: Painted,
}

/// When and by what a clip path is painted through.
struct Painted {
    /// The place in the order of painting of the first paint through it,
    /// or `u32::MAX` if there is none.
    first: u32,
    /// And of the last.
    last: u32,
    /// The list those paints are in.
    list: u32,
    /// They are in more than one list, so on more than one pixmap.
    lists: bool,
    /// Some paint through it does not wholly hide what is under it.
    veiled: bool,
    /// The paints, as indices; no more than [`PAINTS`] of them.
    paints: Vec<u32>,
    /// There were more.
    many: bool,
}

impl Painted {
    fn never() -> Self {
        Painted {
            first: u32::MAX,
            last: 0,
            list: 0,
            lists: false,
            veiled: false,
            paints: Vec::new(),
            many: false,
        }
    }

    fn at_all(&self) -> bool {
        self.first != u32::MAX
    }
}

/// One thing painted through a clip path.
struct Paint<'a> {
    elem: &'a DisplayElement,
    /// Its place in the order of painting.
    place: u32,
    /// The clip paths it was painted through: where they start in
    /// [`Walk::stacks`], and how many.
    clips: (u32, u32),
    /// Where it paints, worked out when first asked.
    reach: OnceLock<Reach>,
}

/// Where a paint puts colour, clips aside.
enum Reach {
    /// An image: the matrix from device space to its samples, and how
    /// many samples across and down.
    Image(Matrix, f64, f64),
    /// A filled path.
    Path(Sides<'static>),
    /// Not known, and taken to be nowhere.
    Unknown,
}

/// In a stack of clips, one set by stroking: not a shape of the table.
const STROKED: u32 = u32::MAX;

/// The clip paths in force at some point of a list.
#[derive(Default)]
struct Stack {
    clips: Vec<u32>,
    /// Where these clips are in [`Walk::stacks`], once a paint has needed
    /// them there.
    run: Option<(u32, u32)>,
}

impl Stack {
    fn push(&mut self, clip: u32) {
        self.clips.push(clip);
        self.run = None;
    }

    fn clear(&mut self) {
        self.clips.clear();
        self.run = None;
    }
}

/// A walk of a display list in the order it is painted, gathering its clip
/// paths and what is painted through each.
#[derive(Default)]
struct Walk<'a> {
    ids: HashMap<u64, u32>,
    shapes: Vec<Shape>,
    paints: Vec<Paint<'a>>,
    stacks: Vec<u32>,
    /// The place in the order of the next paint.
    place: u32,
    /// How many lists have been met.
    lists: u32,
}

impl<'a> Walk<'a> {
    /// Go through `list`, which is list number `id`, with `stack` in force.
    fn list(&mut self, list: &'a DisplayList, stack: &mut Stack, id: u32) {
        // Whether paint with these settings hides what is under it.
        let plain =
            |alpha: f64, blend: u8, overprint: bool| alpha >= 1.0 && blend == 0 && !overprint;
        for elem in list.elements() {
            match elem {
                DisplayElement::Clip { path, params } => {
                    if params.stroke_params.is_some() {
                        stack.push(STROKED);
                        continue;
                    }
                    let key = clip_key(path, params);
                    let next = self.shapes.len() as u32;
                    let shape = *self.ids.entry(key).or_insert(next);
                    if shape == next {
                        let edges = flatten(path, &params.ctm);
                        self.shapes.push(Shape {
                            key,
                            rect: is_rect(&edges),
                            bounds: bounds(&edges),
                            edges,
                            even_odd: matches!(params.fill_rule, FillRule::EvenOdd),
                            painted: Painted::never(),
                        });
                    }
                    stack.push(shape);
                }
                DisplayElement::InitClip | DisplayElement::ErasePage => stack.clear(),
                DisplayElement::Fill { params, .. } => self.paint(
                    stack,
                    id,
                    elem,
                    plain(params.alpha, params.blend_mode, params.overprint),
                ),
                DisplayElement::Stroke { params, .. } => self.paint(
                    stack,
                    id,
                    elem,
                    plain(params.alpha, params.blend_mode, params.overprint),
                ),
                DisplayElement::Image { params, .. } => {
                    // A stencil, samples with an alpha of their own or a
                    // colour key: not every sample is painted.
                    let whole = !matches!(
                        params.color_space,
                        ImageColorSpace::Mask { .. } | ImageColorSpace::PreconvertedRGBA
                    ) && params.mask_color.is_none();
                    let opaque = whole && plain(params.alpha, params.blend_mode, params.overprint);
                    self.paint(stack, id, elem, opaque);
                }
                DisplayElement::AxialShading { params } => self.paint(
                    stack,
                    id,
                    elem,
                    plain(params.alpha, params.blend_mode, params.overprint),
                ),
                DisplayElement::RadialShading { params } => self.paint(
                    stack,
                    id,
                    elem,
                    plain(params.alpha, params.blend_mode, params.overprint),
                ),
                DisplayElement::MeshShading { params } => self.paint(
                    stack,
                    id,
                    elem,
                    plain(params.alpha, params.blend_mode, params.overprint),
                ),
                DisplayElement::PatchShading { params } => self.paint(
                    stack,
                    id,
                    elem,
                    plain(params.alpha, params.blend_mode, params.overprint),
                ),
                DisplayElement::PatternFill { .. } => self.paint(stack, id, elem, false),
                // A group is painted on a pixmap of its own, with no clip
                // to begin with, and then put down through the clips in
                // force here as one paint that may be clear in places.
                DisplayElement::Group { elements, .. } => {
                    self.paint(stack, id, elem, false);
                    self.lists += 1;
                    self.list(elements, &mut Stack::default(), self.lists);
                }
                DisplayElement::SoftMasked { mask, content, .. } => {
                    self.paint(stack, id, elem, false);
                    self.lists += 1;
                    self.list(mask, &mut Stack::default(), self.lists);
                    self.lists += 1;
                    self.list(content, &mut Stack::default(), self.lists);
                }
                // A layer is painted in place, and its clips stay in force
                // after it.
                DisplayElement::OcgGroup { elements, .. } => self.list(elements, stack, id),
                // Text for extraction and anything not known: no paint.
                _ => {}
            }
        }
    }

    /// Note a paint through the clips of `stack`.
    fn paint(&mut self, stack: &mut Stack, list: u32, elem: &'a DisplayElement, opaque: bool) {
        let place = self.place;
        self.place += 1;
        if stack.clips.is_empty() {
            return;
        }
        let (start, count) = *stack.run.get_or_insert_with(|| {
            let start = self.stacks.len();
            let mut clips = stack.clips.clone();
            clips.sort_unstable();
            clips.dedup();
            self.stacks.extend(&clips);
            (start as u32, clips.len() as u32)
        });
        let index = self.paints.len() as u32;
        self.paints.push(Paint {
            elem,
            place,
            clips: (start, count),
            reach: OnceLock::new(),
        });
        for at in start..start + count {
            let clip = self.stacks[at as usize];
            if clip == STROKED {
                continue;
            }
            let painted = &mut self.shapes[clip as usize].painted;
            if painted.at_all() && painted.list != list {
                painted.lists = true;
            }
            painted.first = painted.first.min(place);
            painted.last = place;
            painted.list = list;
            painted.veiled |= !opaque;
            if painted.paints.len() < PAINTS {
                painted.paints.push(index);
            } else {
                painted.many = true;
            }
        }
    }
}

/// Room to work in while asking about one piece after another, so that
/// each does not ask for memory of its own.
#[derive(Default)]
struct Room {
    /// The paths the piece might lie over.
    under: Vec<u32>,
    /// Paths with the piece's edge too, on the same side: how far off, the
    /// place of the last paint through each, and which.
    beside: Vec<(f64, u32, u32)>,
    /// The paints that may come after the piece's path.
    later: Later,
}

/// The paints that may come after some path is first painted through,
/// through paths whose boxes reach some cell of the [`Cover`], each with
/// the path it is through; and which path and cell they are for.
type Later = (Option<(u32, usize)>, Vec<(u32, u32)>);

/// Everything known of a page's clips, for asking how a piece of edge
/// stands to its neighbours.
struct Page<'a> {
    shapes: &'a [Shape],
    /// "Inside?" for each shape.
    sides: &'a [Sides<'a>],
    paints: &'a [Paint<'a>],
    stacks: &'a [u32],
    area: &'a Area,
    pieces: &'a [Piece],
    cover: Cover,
}

/// Which paths' boxes reach each cell of a coarse grid over the area, for
/// finding the paths a point is in.
struct Cover {
    /// The paths of each cell, one cell after another.
    shapes: Vec<u32>,
    /// Where each cell's start in those, and one past the last.
    starts: Vec<u32>,
    cols: usize,
    rows: usize,
    x0: f64,
    y0: f64,
}

impl Cover {
    /// The cells the box of a shape's points reaches, if it is in the area.
    fn cells(&self, shape: &Shape, area: &Area) -> Option<(usize, usize, usize, usize)> {
        let [x0, y0, x1, y1] = shape.bounds;
        if !(x0 <= area.x1 && x1 >= area.x0 && y0 <= area.y1 && y1 >= area.y0) {
            return None;
        }
        let col = |x: f64| (((x - self.x0) / COVER_CELL).max(0.0) as usize).min(self.cols - 1);
        let row = |y: f64| (((y - self.y0) / COVER_CELL).max(0.0) as usize).min(self.rows - 1);
        Some((col(x0), row(y0), col(x1), row(y1)))
    }

    /// The grid for the painted paths among `shapes`, or one with nothing
    /// in it if it would be past [`BUDGET`].
    fn new(shapes: &[Shape], area: &Area) -> Self {
        let cols = (((area.x1 - area.x0) / COVER_CELL).ceil().max(1.0)) as usize;
        let rows = (((area.y1 - area.y0) / COVER_CELL).ceil().max(1.0)) as usize;
        let mut cover = Cover {
            shapes: Vec::new(),
            starts: vec![0; cols * rows + 1],
            cols,
            rows,
            x0: area.x0,
            y0: area.y0,
        };
        let reach: Vec<Option<(usize, usize, usize, usize)>> = shapes
            .iter()
            .map(|s| cover.cells(s, area).filter(|_| s.painted.at_all()))
            .collect();
        let mut total = 0usize;
        for &(c0, r0, c1, r1) in reach.iter().flatten() {
            total += (c1 - c0 + 1) * (r1 - r0 + 1);
            for r in r0..=r1 {
                for n in &mut cover.starts[r * cols + c0 + 1..=r * cols + c1 + 1] {
                    *n += 1;
                }
            }
            if total > BUDGET {
                cover.starts.fill(0);
                return cover;
            }
        }
        for cell in 1..cover.starts.len() {
            cover.starts[cell] += cover.starts[cell - 1];
        }
        let mut next = cover.starts.clone();
        cover.shapes = vec![0; total];
        for (shape, &(c0, r0, c1, r1)) in reach
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.as_ref().map(|r| (i, r)))
        {
            for r in r0..=r1 {
                for at in &mut next[r * cols + c0..=r * cols + c1] {
                    cover.shapes[*at as usize] = shape as u32;
                    *at += 1;
                }
            }
        }
        cover
    }

    /// The cell `p` is in, if it is in the area.
    fn cell(&self, p: Point) -> Option<usize> {
        let (col, row) = ((p.0 - self.x0) / COVER_CELL, (p.1 - self.y0) / COVER_CELL);
        (col >= 0.0 && row >= 0.0 && (col as usize) < self.cols && (row as usize) < self.rows)
            .then(|| row as usize * self.cols + col as usize)
    }

    /// The paths whose boxes reach `cell`.
    fn reaching(&self, cell: usize) -> &[u32] {
        &self.shapes[self.starts[cell] as usize..self.starts[cell + 1] as usize]
    }
}

impl Page<'_> {
    /// Where `paint` puts colour.
    fn reach<'p>(&self, paint: &'p Paint<'_>) -> &'p Reach {
        paint.reach.get_or_init(|| match paint.elem {
            DisplayElement::Image { params, .. } => params
                .image_matrix
                .invert()
                .and_then(|inv| params.ctm.concat(&inv).invert())
                .map_or(Reach::Unknown, |m| {
                    Reach::Image(m, f64::from(params.width), f64::from(params.height))
                }),
            DisplayElement::Fill { path, params } => {
                let edges = flatten(path, &params.ctm);
                if edges.len() > PAINT_EDGES {
                    return Reach::Unknown;
                }
                Reach::Path(Sides::new(
                    Cow::Owned(edges),
                    matches!(params.fill_rule, FillRule::EvenOdd),
                    self.area,
                ))
            }
            _ => Reach::Unknown,
        })
    }

    /// Whether `paint` puts colour at `p`, clips aside. `None` if where it
    /// paints is not known.
    fn paints_there(&self, paint: &Paint<'_>, p: Point) -> Option<bool> {
        match self.reach(paint) {
            Reach::Image(m, width, height) => {
                let (u, v) = m.transform_point(p.0, p.1);
                Some((0.0..=*width).contains(&u) && (0.0..=*height).contains(&v))
            }
            Reach::Path(sides) => Some(sides.inside(p)),
            Reach::Unknown => None,
        }
    }

    /// Whether every clip path `paint` is painted through lets it reach
    /// `p`, `shape` aside.
    fn let_through(&self, paint: &Paint<'_>, p: Point, shape: u32) -> bool {
        let (start, count) = paint.clips;
        self.stacks[start as usize..(start + count) as usize]
            .iter()
            .all(|&clip| clip == shape || (clip != STROKED && self.sides[clip as usize].inside(p)))
    }

    /// Whether `paint`, which is painted through `shape`, reaches `p`:
    /// it paints there, and every other clip it is painted through lets
    /// it.
    fn reaches(&self, paint: &Paint<'_>, p: Point, shape: u32) -> bool {
        self.paints_there(paint, p) == Some(true) && self.let_through(paint, p, shape)
    }

    /// How many paints through `shape` reach `p`, if that can be said.
    fn paints_at(&self, shape: u32, p: Point) -> Option<usize> {
        let painted = &self.shapes[shape as usize].painted;
        if painted.many {
            return None;
        }
        Some(
            painted
                .paints
                .iter()
                .filter(|&&i| self.reaches(&self.paints[i as usize], p, shape))
                .count(),
        )
    }

    /// Whether some paint through `shape` is known to reach `p`.
    fn painted_at(&self, shape: u32, p: Point) -> bool {
        let painted = &self.shapes[shape as usize].painted;
        !painted.many
            && painted
                .paints
                .iter()
                .any(|&i| self.reaches(&self.paints[i as usize], p, shape))
    }

    /// The point `by` pixels inside the edge at the middle of a piece.
    fn inside(piece: &Piece, by: f64) -> Point {
        (
            piece.mid.0 + by * piece.inward.0,
            piece.mid.1 + by * piece.inward.1,
        )
    }

    /// Whether exactly one thing is painted along the inside of a piece's
    /// edge. With none, the colour carried across would be left showing;
    /// with more, each smooth layer would let the one under it show.
    fn painted_once(&self, piece: &Piece) -> bool {
        self.paints_at(piece.shape, Self::inside(piece, INSIDE)) == Some(1)
    }

    /// The paints through `shape` that might be a piece of the page
    /// painted after the path of `piece` is first painted through, going
    /// by when and through what they are painted and not yet by where.
    /// Each comes with `shape`.
    ///
    /// A path painted through more times than are kept is passed over. It
    /// is a clip round a whole page or group, and what is painted through
    /// it and matters here is painted through a smaller path as well, and
    /// is found by that.
    fn painted_after<'s>(
        &'s self,
        piece: &Piece,
        shape: u32,
    ) -> impl Iterator<Item = (u32, u32)> + 's {
        let own = &self.shapes[piece.shape as usize].painted;
        let (own_shape, first, list) = (piece.shape, own.first, own.list);
        let painted = &self.shapes[shape as usize].painted;
        let may = shape != own_shape
            && painted.last > first
            && !painted.many
            && !painted.lists
            && painted.list == list;
        let paints: &[u32] = if may { &painted.paints } else { &[] };
        paints
            .iter()
            .filter(move |&&i| {
                let paint = &self.paints[i as usize];
                let (start, count) = paint.clips;
                paint.place > first
                    && !self.stacks[start as usize..(start + count) as usize].contains(&own_shape)
            })
            .map(move |&i| (shape, i))
    }

    /// Whether one of `later`, paints that may come
    /// [after](Self::painted_after) the path of `piece` is first painted
    /// through, reaches `p`, a point behind the piece's edge. The pixel
    /// there may not yet hold the colour it is to have.
    ///
    /// Such a piece of the page is a paint that reaches `p` and not the
    /// piece's own side of the edge, by its clips or by where it paints.
    /// A paint that reaches both sides is no piece of the page that meets
    /// this one: it goes over both alike, and what is under it does not
    /// show.
    fn painted_later(&self, piece: &Piece, p: Point, later: &[(u32, u32)]) -> bool {
        let near = Self::inside(piece, INSIDE);
        // The paints of one path are together.
        later.chunk_by(|a, b| a.0 == b.0).any(|paints| {
            let shape = paints[0].0;
            let b = &self.shapes[shape as usize].bounds;
            b[0] <= p.0
                && p.0 <= b[2]
                && b[1] <= p.1
                && p.1 <= b[3]
                && self.sides[shape as usize].inside(p)
                && paints.iter().any(|&(_, i)| {
                    let paint = &self.paints[i as usize];
                    self.let_through(paint, p, shape)
                        && (!self.let_through(paint, near, STROKED)
                            || (self.paints_there(paint, p) == Some(true)
                                && self.paints_there(paint, near) == Some(false)))
                })
        })
    }

    /// How deep behind the edge of `piece`, in pixels, the colour carried
    /// across may come from: the deepest of [`DEPTHS`] with every one
    /// before it at which nothing is painted later. Nothing at all unless
    /// one of the paths `under` is known to be painted along the edge:
    /// what is carried across is to be a neighbour's paint.
    ///
    /// Further back it need not be one of theirs. A thin neighbour with
    /// another piece behind it, or bare paper, has behind it what a pixel
    /// there should hold, so long as it is all painted already.
    ///
    /// `later` is room to work in.
    fn deep_under(&self, piece: &Piece, under: &[u32], later: &mut Later) -> f64 {
        let beside = Self::inside(piece, -INSIDE);
        let painted = under.iter().any(|&shape| {
            self.sides[shape as usize].inside(beside) && self.painted_at(shape, beside)
        });
        if !painted {
            return 0.0;
        }
        // The depths are a few pixels apart and a cell is many across, so
        // one after another is most often in the same cell, and so is the
        // next piece of the same path: the paths to ask about are kept
        // from one to the next.
        let mut deep = 0.0;
        for by in DEPTHS.map(|times| times * DEEP) {
            let p = Self::inside(piece, -by);
            let here = self.cover.cell(p).map(|cell| (piece.shape, cell));
            if here != later.0 || here.is_none() {
                later.0 = here;
                later.1.clear();
                if let Some((_, cell)) = here {
                    later.1.extend(
                        self.cover
                            .reaching(cell)
                            .iter()
                            .flat_map(|&shape| self.painted_after(piece, shape)),
                    );
                }
            }
            let later = &later.1[..];
            if self.painted_later(piece, p, later) {
                break;
            }
            deep = by;
        }
        deep
    }

    /// Whether a piece of `shape` might lie over a piece of another path
    /// across the edge from it, going by how the two are painted.
    fn may_follow(&self, shape: &Shape, other: &Near) -> bool {
        let (here, there) = (&shape.painted, &self.shapes[other.shape as usize]);
        let before = &there.painted;
        // A neighbour along the pixel grid is cut there whatever happens.
        if there.rect || other.along.0.abs() < UPRIGHT || other.along.1.abs() < UPRIGHT {
            return false;
        }
        // The neighbour is all painted first, on the same pixmap, and
        // every paint through this path hides it.
        here.at_all()
            && before.at_all()
            && before.last < here.first
            && !here.lists
            && !before.lists
            && here.list == before.list
            && !here.veiled
    }

    /// What is known of piece `index` and its neighbours, if it has any
    /// within reach.
    ///
    /// The tests read the same from either piece, so two neighbours always
    /// agree that they are neighbours, on how far apart, and on which of
    /// them may lie over the other.
    fn meet(&self, index: usize, grid: &Grid, room: &mut Room) -> Option<MetPiece> {
        let piece = &self.pieces[index];
        let shape = &self.shapes[piece.shape as usize];
        let (mut hard, mut over) = (f64::INFINITY, f64::INFINITY);
        let Room {
            under,
            later,
            beside,
        } = room;
        under.clear();
        beside.clear();
        // The place of the first paint through the neighbour that starts
        // to be painted last.
        let mut newest = 0;
        if piece.is_upright() {
            hard = 0.0;
        } else {
            for other in grid.around(piece.mid) {
                if other.shape == piece.shape {
                    continue;
                }
                let facing = piece.inward.0 * other.inward.0 + piece.inward.1 * other.inward.1;
                if facing == 0.0 {
                    continue;
                }
                let Some(gap) = piece.gap_to(other, facing < 0.0) else {
                    continue;
                };
                let painted = &self.shapes[other.shape as usize].painted;
                if facing > 0.0 {
                    if painted.at_all() {
                        let last = if painted.lists || painted.list != shape.painted.list {
                            u32::MAX
                        } else {
                            painted.last
                        };
                        beside.push((gap, last, other.shape));
                    }
                    continue;
                }
                if painted.at_all() {
                    newest = newest.max(painted.first);
                }
                if self.may_follow(shape, other) {
                    over = over.min(gap);
                    if !under.contains(&other.shape) {
                        under.push(other.shape);
                    }
                } else {
                    hard = hard.min(gap);
                }
            }
        }
        // It lies over them only if it is painted once along the edge and
        // they are deep enough behind it at the display list's own scale.
        let mut deep = 0.0;
        if !under.is_empty() {
            if self.painted_once(piece) {
                deep = self.deep_under(piece, under, later);
            }
            if deep < DEEP {
                hard = hard.min(over);
                over = f64::INFINITY;
            }
        }
        let stack = beside
            .iter()
            .filter(|&&(_, last, shape)| last >= newest && !under.contains(&shape))
            .map(|&(gap, _, _)| gap)
            .fold(f64::INFINITY, f64::min);
        let gap = hard.min(over);
        gap.is_finite().then_some(MetPiece {
            a: [piece.a.0 as f32, piece.a.1 as f32],
            b: [piece.b.0 as f32, piece.b.1 as f32],
            gap: gap as f32,
            hard: hard as f32,
            over: over as f32,
            stack: stack as f32,
            deep: deep as f32,
            inward: [piece.inward.0 as f32, piece.inward.1 as f32],
        })
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

/// The box of the points of `edges`: least x and y, then greatest.
fn bounds(edges: &[(Point, Point)]) -> [f64; 4] {
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for p in edges.iter().flat_map(|(a, b)| [a, b]) {
        bounds[0] = bounds[0].min(p.0);
        bounds[1] = bounds[1].min(p.1);
        bounds[2] = bounds[2].max(p.0);
        bounds[3] = bounds[3].max(p.1);
    }
    bounds
}

/// Every edge along an axis: a rectangle, or any outline of upright and
/// level edges.
fn is_rect(edges: &[(Point, Point)]) -> bool {
    edges.iter().all(|(a, b)| a.0 == b.0 || a.1 == b.1)
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
    edges: Cow<'a, [(Point, Point)]>,
    /// The edges that cross each band of rows the path reaches, one band
    /// after another.
    crossing: Vec<u32>,
    /// Where each band's edges start in `crossing`, and one past the last.
    starts: Vec<u32>,
    /// The band of the area the first of those is.
    first: usize,
    /// How many bands the area has.
    count: usize,
    top: f64,
    even_odd: bool,
}

impl<'a> Sides<'a> {
    fn new(edges: Cow<'a, [(Point, Point)]>, even_odd: bool, area: &Area) -> Self {
        let count = (((area.y1 - area.y0) / BAND).ceil().max(1.0)) as usize;
        // The bands an edge crosses, if it is in the area at all.
        let reach = |(a, b): &(Point, Point)| {
            let (lo, hi) = (a.1.min(b.1), a.1.max(b.1));
            if hi < area.y0 || lo > area.y1 {
                return None;
            }
            let first = (((lo - area.y0) / BAND).floor().max(0.0)) as usize;
            let last = ((((hi - area.y0) / BAND).floor()).max(0.0) as usize).min(count - 1);
            Some((first.min(last), last))
        };
        let (mut first, mut last) = (usize::MAX, 0);
        for (from, to) in edges.iter().filter_map(reach) {
            first = first.min(from);
            last = last.max(to);
        }
        if first == usize::MAX {
            (first, last) = (0, 0);
        }
        // Count each band's edges, then put each edge in its place.
        let mut starts = vec![0u32; last - first + 2];
        for (from, to) in edges.iter().filter_map(reach) {
            for n in &mut starts[from - first + 1..=to - first + 1] {
                *n += 1;
            }
        }
        for band in 1..starts.len() {
            starts[band] += starts[band - 1];
        }
        let mut next = starts.clone();
        let mut crossing = vec![0u32; *starts.last().expect("at least two") as usize];
        for (i, edge) in edges.iter().enumerate() {
            if let Some((from, to)) = reach(edge) {
                for at in &mut next[from - first..=to - first] {
                    crossing[*at as usize] = i as u32;
                    *at += 1;
                }
            }
        }
        // Furthest right first: an edge wholly left of a point adds nothing
        // to the winding there, and neither does any after it.
        let right = |&i: &u32| {
            let (a, b) = edges[i as usize];
            a.0.max(b.0)
        };
        for band in starts.windows(2) {
            let band = &mut crossing[band[0] as usize..band[1] as usize];
            if band.len() > 8 {
                band.sort_unstable_by(|i, j| right(j).total_cmp(&right(i)));
            }
        }
        Self {
            edges,
            crossing,
            starts,
            first,
            count,
            top: area.y0,
            even_odd,
        }
    }

    /// The edges that might cross the row `y` is in, or `None` if `y` is
    /// outside the area.
    fn band(&self, y: f64) -> Option<&[u32]> {
        let band = ((y - self.top) / BAND).floor();
        if !(band >= 0.0 && (band as usize) < self.count) {
            return None;
        }
        // A band the path does not reach has no edge of it to cross.
        let reached = (band as usize)
            .checked_sub(self.first)
            .filter(|band| band + 1 < self.starts.len());
        Some(match reached {
            Some(band) => {
                &self.crossing[self.starts[band] as usize..self.starts[band + 1] as usize]
            }
            None => &[],
        })
    }

    /// What the edge from `a` to `b` adds to the winding number at `p`.
    #[inline]
    fn turn((a, b): (Point, Point), p: Point) -> i32 {
        if (a.1 <= p.1) != (b.1 <= p.1) {
            let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
            if x > p.0 {
                return if b.1 > a.1 { 1 } else { -1 };
            }
        }
        0
    }

    /// The winding numbers at `p` and at `q`, from the edges of one band.
    fn windings(&self, band: &[u32], p: Point, q: Point) -> (i32, i32) {
        // Bands of more than a few edges are in order of their reach to
        // the right, and can be left as soon as one falls short of both.
        let sorted = band.len() > 8;
        let left = p.0.min(q.0);
        let (mut wp, mut wq) = (0i32, 0i32);
        for &i in band {
            let edge = self.edges[i as usize];
            if sorted && edge.0.0.max(edge.1.0) <= left {
                break;
            }
            wp += Self::turn(edge, p);
            wq += Self::turn(edge, q);
        }
        (wp, wq)
    }

    fn is_inside(&self, winding: i32) -> bool {
        if self.even_odd {
            winding & 1 != 0
        } else {
            winding != 0
        }
    }

    fn inside(&self, p: Point) -> bool {
        let Some(band) = self.band(p.1) else {
            return false;
        };
        self.is_inside(self.windings(band, p, p).0)
    }

    /// Whether each of two points is inside, going over the edges once
    /// where the points are in the same band of rows, as two points either
    /// side of an edge most often are.
    fn inside_both(&self, p: Point, q: Point) -> (bool, bool) {
        match (self.band(p.1), self.band(q.1)) {
            (Some(at_p), Some(at_q)) if std::ptr::eq(at_p, at_q) => {
                let (wp, wq) = self.windings(at_p, p, q);
                (self.is_inside(wp), self.is_inside(wq))
            }
            _ => (self.inside(p), self.inside(q)),
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

impl Piece {
    /// Whether the piece runs along the pixel grid. Such an edge is kept
    /// hard, as if it had a neighbour: it has no staircase to smooth, only
    /// a position, and a hard edge puts it on the nearest pixel boundary
    /// as a rectangle's is put.
    ///
    /// It matters most where it is least noticed. A table's rules are very
    /// often drawn as a stroke clipped to its own outline, and a thin line
    /// covered in part by the clip *and* in part by its own anti-aliasing
    /// comes out at half its weight.
    fn is_upright(&self) -> bool {
        self.along.0.abs() < UPRIGHT || self.along.1.abs() < UPRIGHT
    }

    /// How far off `other`, a piece of another path, runs along this one,
    /// if it does within [`REACH`]: from the other side if `across`, as a
    /// neighbour does, or else on the same side.
    #[inline(always)]
    fn gap_to(&self, other: &Near, across: bool) -> Option<f64> {
        if other.shape == self.shape {
            return None;
        }
        let facing = self.inward.0 * other.inward.0 + self.inward.1 * other.inward.1;
        if facing == 0.0 || (facing < 0.0) != across {
            return None;
        }
        let d = (other.mid.0 - self.mid.0, other.mid.1 - self.mid.1);
        let cross = self.along.0 * other.along.1 - self.along.1 * other.along.0;
        if cross.abs() > PARALLEL {
            return None;
        }
        let along_here = d.0 * self.along.0 + d.1 * self.along.1;
        let along_there = d.0 * other.along.0 + d.1 * other.along.1;
        if along_here.abs() > REACH || along_there.abs() > REACH {
            return None;
        }
        let off_here = (d.0 * self.along.1 - d.1 * self.along.0).abs();
        let off_there = (d.0 * other.along.1 - d.1 * other.along.0).abs();
        let gap = off_here.max(off_there);
        (gap <= REACH).then_some(gap)
    }
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
            let (left, right) = sides.inside_both(
                (mid.0 + SIDE * across.0, mid.1 + SIDE * across.1),
                (mid.0 - SIDE * across.0, mid.1 - SIDE * across.1),
            );
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

/// What of a piece decides whether it runs beside another.
#[derive(Clone, Copy)]
struct Near {
    mid: Point,
    along: Point,
    inward: Point,
    shape: u32,
}

/// Pieces sorted by the cell of the area their middle falls in, a row of
/// cells at a time, so that the pieces of a run of cells in one row are a
/// run of this list, and looking through them is reading memory in order.
struct Grid {
    /// The column of each piece, by row and then by column.
    cols: Vec<u32>,
    /// The pieces, in the same order.
    near: Vec<Near>,
    /// Where each row starts in those, and one past the last.
    rows: Vec<u32>,
    x0: f64,
    y0: f64,
}

impl Grid {
    fn new(pieces: &[Piece], area: &Area) -> Self {
        let mut grid = Grid {
            cols: Vec::new(),
            near: Vec::new(),
            rows: Vec::new(),
            x0: area.x0,
            y0: area.y0,
        };
        let count = (((area.y1 - area.y0) / CELL).floor().max(0.0)) as usize + 1;
        let cell = |p: &Piece| {
            let (col, row) = grid.cell(p.mid);
            (col, (row as usize).min(count - 1))
        };
        // Sorted by counting: how many pieces each row has, and so where
        // each row starts; then each piece into the next place of its row.
        let mut rows = vec![0u32; count + 1];
        for p in pieces {
            rows[cell(p).1 + 1] += 1;
        }
        for r in 0..count {
            rows[r + 1] += rows[r];
        }
        let mut next = rows.clone();
        let mut order = vec![(0u32, 0u32); pieces.len()];
        for (i, p) in pieces.iter().enumerate() {
            let (col, row) = cell(p);
            order[next[row] as usize] = (col, i as u32);
            next[row] += 1;
        }
        // And within a row by column. Pieces of one column keep the order
        // they were cut in.
        for row in rows.windows(2) {
            order[row[0] as usize..row[1] as usize].sort_unstable();
        }
        grid.cols = order.iter().map(|&(col, _)| col).collect();
        grid.near = order
            .iter()
            .map(|&(_, i)| {
                let p = &pieces[i as usize];
                Near {
                    mid: p.mid,
                    along: p.along,
                    inward: p.inward,
                    shape: p.shape,
                }
            })
            .collect();
        grid.rows = rows;
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

    /// Every piece whose middle may be within `WINDOW` of `p` along both
    /// axes: those of the cells such a square touches.
    fn around(&self, p: Point) -> impl Iterator<Item = &Near> + '_ {
        let (col0, row0) = self.cell((p.0 - WINDOW, p.1 - WINDOW));
        let (col1, row1) = self.cell((p.0 + WINDOW, p.1 + WINDOW));
        let last = self.rows.len().saturating_sub(2) as u32;
        (row0..=row1.min(last)).flat_map(move |r| {
            let (start, end) = (
                self.rows[r as usize] as usize,
                self.rows[r as usize + 1] as usize,
            );
            let cols = &self.cols[start..end];
            let from = cols.partition_point(|&c| c < col0);
            let to = from + cols[from..].partition_point(|&c| c <= col1);
            self.near[start + from..start + to].iter()
        })
    }
}

/// How far apart along either axis the middles of two pieces that run
/// together can be: [`REACH`] along the one and [`REACH`] off it, so the
/// diagonal of a square of that side, and a little for rounding.
const WINDOW: f64 = REACH * std::f64::consts::SQRT_2 + 1e-6;

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

    const EVERY_ROW: [f32; 2] = [f32::NEG_INFINITY, f32::INFINITY];

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
            ClipEdgeState::Mixed { met, scale } => met
                .iter()
                .filter(|p| p.meeting(*scale) == Meeting::Hard)
                .copied()
                .collect(),
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
        let long = polygon(&[(10.0, 10.0), (50.0, 11.0), (52.0, 91.0), (8.0, 90.0)]);
        let upper = polygon(&[(50.0, 11.0), (80.0, 10.0), (81.0, 50.0), (51.0, 51.0)]);
        let lower = polygon(&[(51.0, 51.0), (81.0, 50.0), (82.0, 90.0), (52.0, 91.0)]);
        let t = table(&[long.clone(), upper, lower]);
        let length: f32 = hard(&t.edges(key(&long), 1.0))
            .iter()
            .map(|p| ((p.b[0] - p.a[0]).powi(2) + (p.b[1] - p.a[1]).powi(2)).sqrt())
            .sum();
        assert!((length - 80.025).abs() < 0.01, "{length}");
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
            PathSegment::LineTo(39.0, 161.0),
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

    /// An edge along the pixel grid is hard with nothing beside it; the
    /// slanted edges of the same path are free.
    #[test]
    fn an_edge_along_the_grid_stays_hard() {
        // A table rule with mitred ends, as a word processor clips it.
        let rule = polygon(&[
            (20.8, 40.7),
            (20.0, 39.9),
            (200.0, 39.9),
            (200.8, 40.7),
            (200.0, 41.5),
            (20.0, 41.5),
        ]);
        let t = table(std::slice::from_ref(&rule));
        let pieces = hard(&t.edges(key(&rule), 1.0));
        assert!(!pieces.is_empty());
        assert!(
            pieces.iter().all(|p| p.a[1] == p.b[1]),
            "only the level edges"
        );
        let length: f32 = pieces.iter().map(|p| (p.b[0] - p.a[0]).abs()).sum();
        assert!((length - 360.0).abs() < 0.01, "{length}");
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

    // ---- Which of two neighbours may lie over the other ----

    fn fill_with(points: &[Point], alpha: f64) -> DisplayElement {
        let DisplayElement::Clip { path, .. } = polygon(points) else {
            unreachable!()
        };
        DisplayElement::Fill {
            path,
            params: stet_graphics::device::FillParams {
                alpha,
                ..Default::default()
            },
        }
    }

    /// Paint over everything.
    fn flood() -> DisplayElement {
        fill_with(
            &[(0.0, 0.0), (200.0, 0.0), (200.0, 200.0), (0.0, 200.0)],
            1.0,
        )
    }

    /// Each clip set afresh and painted through by what follows it.
    fn painted(parts: &[(&DisplayElement, &[DisplayElement])]) -> ClipNeighbours {
        let mut elems = Vec::new();
        for (clip, paints) in parts {
            elems.push(DisplayElement::InitClip);
            elems.push((*clip).clone());
            elems.extend(paints.iter().cloned());
        }
        table(&elems)
    }

    /// What becomes of the pieces of `clip` that have a neighbour and do
    /// not run along the pixel grid, which must all come to the same.
    fn meeting(table: &ClipNeighbours, clip: &DisplayElement, scale: f32) -> Meeting {
        let met: Vec<Meeting> = table
            .met(key(clip))
            .iter()
            .filter(|p| p.gap > 0.0 || p.a[0] != p.b[0] && p.a[1] != p.b[1])
            .map(|p| p.meeting(scale))
            .collect();
        assert!(!met.is_empty(), "no piece has a neighbour");
        assert!(met.iter().all(|m| *m == met[0]), "{met:?}");
        met[0]
    }

    #[test]
    fn the_later_of_two_tiles_lies_over_the_earlier() {
        let (left, right) = (left_tile(), right_tile());
        let t = painted(&[(&left, &[flood()]), (&right, &[flood()])]);
        assert_eq!(meeting(&t, &left, 1.0), Meeting::Hard);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Over);
        let t = painted(&[(&right, &[flood()]), (&left, &[flood()])]);
        assert_eq!(meeting(&t, &left, 1.0), Meeting::Over);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
        // The one that lies over is filled smooth, and the other is hard
        // along the edge they share and smooth elsewhere.
        assert_eq!(t.edges(key(&left), 1.0), ClipEdgeState::Smooth);
        assert!(matches!(
            t.edges(key(&right), 1.0),
            ClipEdgeState::Mixed { .. }
        ));
        assert_eq!(
            t.lying_over(key(&left), 1.0, EVERY_ROW).count(),
            hard(&t.edges(key(&right), 1.0)).len()
        );
        assert_eq!(t.lying_over(key(&right), 1.0, EVERY_ROW).count(), 0);
    }

    #[test]
    fn a_piece_lying_over_points_into_its_own_path() {
        let (left, right) = (left_tile(), right_tile());
        let t = painted(&[(&left, &[flood()]), (&right, &[flood()])]);
        for piece in t.met(key(&right)) {
            assert!(piece.inward()[0] > 0.9, "{piece:?}");
        }
    }

    #[test]
    fn only_the_last_of_a_stack_lies_over() {
        // A second path with the left tile's edge, on the same side.
        let again = polygon(&[(30.0, 11.0), (50.0, 12.0), (52.0, 60.0), (30.0, 59.0)]);
        let (left, right) = (left_tile(), right_tile());
        let t = painted(&[
            (&left, &[flood()]),
            (&right, &[flood()]),
            (&again, &[flood()]),
        ]);
        assert_eq!(meeting(&t, &left, 1.0), Meeting::Hard);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
        assert_eq!(meeting(&t, &again, 1.0), Meeting::Over);
    }

    #[test]
    fn two_paths_with_one_edge_painted_after_their_neighbour_are_two_layers() {
        let again = polygon(&[(30.0, 11.0), (50.0, 12.0), (52.0, 60.0), (30.0, 59.0)]);
        let (left, right) = (left_tile(), right_tile());
        let t = painted(&[
            (&right, &[flood()]),
            (&left, &[flood()]),
            (&again, &[flood()]),
        ]);
        for clip in [&left, &right, &again] {
            assert_eq!(meeting(&t, clip, 1.0), Meeting::Hard);
        }
    }

    #[test]
    fn paint_that_does_not_hide_what_is_under_it_stays_hard() {
        let (left, right) = (left_tile(), right_tile());
        let veil = fill_with(
            &[(0.0, 0.0), (200.0, 0.0), (200.0, 200.0), (0.0, 200.0)],
            0.5,
        );
        let t = painted(&[(&left, &[flood()]), (&right, &[veil])]);
        assert_eq!(meeting(&t, &left, 1.0), Meeting::Hard);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
    }

    #[test]
    fn two_paints_through_the_later_path_stay_hard() {
        let (left, right) = (left_tile(), right_tile());
        let t = painted(&[(&left, &[flood()]), (&right, &[flood(), flood()])]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
        // Two paints through the earlier one are nothing to the later.
        let t = painted(&[(&left, &[flood(), flood()]), (&right, &[flood()])]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Over);
    }

    #[test]
    fn a_later_path_not_painted_up_to_its_edge_stays_hard() {
        let (left, right) = (left_tile(), right_tile());
        // Paint that starts three pixels in from the shared edge.
        let short = fill_with(
            &[(55.0, 0.0), (200.0, 0.0), (200.0, 200.0), (55.0, 200.0)],
            1.0,
        );
        let t = painted(&[(&left, &[flood()]), (&right, &[short])]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
    }

    #[test]
    fn a_thin_earlier_path_with_a_later_one_behind_it_stays_hard() {
        // A sliver a pixel wide along the right tile's left edge, and
        // behind it a tile painted last: the next pixel out is that
        // tile's, and does not hold its colour yet.
        let sliver = polygon(&[(48.5, 0.0), (49.5, 0.0), (52.5, 72.0), (51.5, 72.0)]);
        let beyond = polygon(&[(10.0, 0.0), (48.5, 0.0), (51.5, 72.0), (10.0, 72.0)]);
        let right = right_tile();
        let t = painted(&[
            (&sliver, &[flood()]),
            (&right, &[flood()]),
            (&beyond, &[flood()]),
        ]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
        // With bare paper behind the sliver, the next pixel out holds
        // what it should.
        let t = painted(&[(&sliver, &[flood()]), (&right, &[flood()])]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Over);
    }

    #[test]
    fn an_earlier_path_painted_by_who_knows_what_stays_hard() {
        let (left, right) = (left_tile(), right_tile());
        let DisplayElement::Clip { path, .. } = left_tile() else {
            unreachable!()
        };
        let stroke = DisplayElement::Stroke {
            path,
            params: Default::default(),
        };
        let t = painted(&[(&left, &[stroke]), (&right, &[flood()])]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
    }

    #[test]
    fn a_path_painted_before_and_after_its_neighbour_stays_hard() {
        let (left, right) = (left_tile(), right_tile());
        let t = painted(&[
            (&left, &[flood()]),
            (&right, &[flood()]),
            (&left, &[flood()]),
        ]);
        assert_eq!(meeting(&t, &left, 1.0), Meeting::Hard);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
    }

    #[test]
    fn neighbours_on_different_pixmaps_stay_hard() {
        let (left, right) = (left_tile(), right_tile());
        let mut inner = DisplayList::new();
        inner.push(right.clone());
        inner.push(flood());
        let mut list = DisplayList::new();
        list.push(left.clone());
        list.push(flood());
        list.push(DisplayElement::InitClip);
        list.push(DisplayElement::Group {
            elements: inner,
            params: stet_graphics::display_list::GroupParams {
                bbox: [0.0, 0.0, 200.0, 200.0],
                isolated: true,
                knockout: false,
                blend_mode: 0,
                alpha: 1.0,
                color_space: stet_graphics::display_list::GroupColorSpace::Inherited,
            },
        });
        let t = ClipNeighbours::build(&list);
        assert_eq!(meeting(&t, &left, 1.0), Meeting::Hard);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
    }

    #[test]
    fn a_clip_nothing_is_painted_through_is_still_a_neighbour() {
        let (left, right) = (left_tile(), right_tile());
        let t = painted(&[(&left, &[]), (&right, &[flood()])]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
    }

    #[test]
    fn how_deep_is_deep_enough_goes_by_the_scale_of_the_output() {
        // What the pieces in the middle of the shared edge come to; near
        // its ends a point some pixels in is past the corner.
        let middle = |t: &ClipNeighbours, clip: &DisplayElement, scale: f32| {
            let met: Vec<Meeting> = t
                .met(key(clip))
                .iter()
                .filter(|p| p.gap == 0.0 && p.a[1] > 25.0 && p.b[1] < 45.0)
                .filter(|p| p.a[0] > 40.0 && p.a[0] < 60.0)
                .map(|p| p.meeting(scale))
                .collect();
            assert!(!met.is_empty());
            assert!(met.iter().all(|m| *m == met[0]), "{met:?}");
            met[0]
        };
        // A strip three pixels deep behind the shared edge, and behind the
        // strip a tile that is painted last of all. A pixel taken from
        // further back than the strip would not yet hold that tile's
        // colour, so the strip is deep enough only down to half scale.
        let strip = polygon(&[(46.5, 0.0), (49.5, 0.0), (52.5, 72.0), (49.5, 72.0)]);
        let beyond = polygon(&[(10.0, 0.0), (46.5, 0.0), (49.5, 72.0), (10.0, 72.0)]);
        let right = right_tile();
        let t = painted(&[
            (&strip, &[flood()]),
            (&right, &[flood()]),
            (&beyond, &[flood()]),
        ]);
        assert_eq!(middle(&t, &right, 1.0), Meeting::Over);
        assert_eq!(middle(&t, &right, 4.0), Meeting::Over);
        assert_eq!(middle(&t, &right, 0.7), Meeting::Over);
        assert_eq!(middle(&t, &right, 0.4), Meeting::Hard);
        // Painted first, the tile behind is no bar: its colour is there.
        let t = painted(&[
            (&beyond, &[flood()]),
            (&strip, &[flood()]),
            (&right, &[flood()]),
        ]);
        assert_eq!(middle(&t, &right, 0.4), Meeting::Over);
        // Nor is bare paper behind the strip: it is what is there.
        let t = painted(&[(&strip, &[flood()]), (&right, &[flood()])]);
        assert_eq!(middle(&t, &right, 0.4), Meeting::Over);
        // Nothing is vouched for past one pixel to eight.
        assert_eq!(middle(&t, &right, 0.125), Meeting::Over);
        assert_eq!(middle(&t, &right, 0.1), Meeting::Hard);
    }

    #[test]
    fn a_thin_path_is_no_bar_with_another_behind_it() {
        // A sliver along the right tile's edge, and the left tile behind
        // the sliver: between them they are deep.
        let sliver = polygon(&[(49.0, 12.0), (50.0, 12.0), (52.0, 60.0), (51.0, 60.0)]);
        let behind = polygon(&[(10.0, 10.0), (50.0, 12.0), (52.0, 60.0), (8.0, 58.0)]);
        let right = right_tile();
        let t = painted(&[
            (&behind, &[flood()]),
            (&sliver, &[flood()]),
            (&right, &[flood()]),
        ]);
        let over = t.lying_over(key(&right), 0.25, EVERY_ROW).count();
        assert!(over > 15, "{over} pieces lie over");
    }

    #[test]
    fn an_image_is_asked_where_it_reaches() {
        let (left, right) = (left_tile(), right_tile());
        let image = |x: f64, width: f64| DisplayElement::Image {
            sample_data: std::sync::Arc::new(vec![0; 3]),
            params: stet_graphics::device::ImageParams {
                width: 1,
                height: 1,
                color_space: ImageColorSpace::DeviceRGB,
                bits_per_component: 8,
                ctm: Matrix::new(width, 0.0, 0.0, 200.0, x, 0.0),
                image_matrix: Matrix::default(),
                interpolate: false,
                mask_color: None,
                alpha: 1.0,
                blend_mode: 0,
                overprint: false,
                overprint_mode: 0,
                opm_paired: false,
                painted_channels: 0,
                alpha_is_shape: false,
                rendering_intent: 0,
                transfer: Default::default(),
            },
        };
        let t = painted(&[
            (&left, &[image(0.0, 200.0)]),
            (&right, &[image(0.0, 200.0)]),
        ]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Over);
        // An image that ends three pixels short of the edge.
        let t = painted(&[
            (&left, &[image(0.0, 200.0)]),
            (&right, &[image(55.0, 100.0)]),
        ]);
        assert_eq!(meeting(&t, &right, 1.0), Meeting::Hard);
    }
}
