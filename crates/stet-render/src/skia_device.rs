// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! tiny-skia implementation of the `OutputDevice` trait.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use rayon::prelude::*;
use tiny_skia::{
    Color, FillRule as SkiaFillRule, LineCap as SkiaLineCap, LineJoin as SkiaLineJoin, Mask, Paint,
    PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

use stet_core::device::{
    AxialShadingParams, ClipParams, FillParams, ImageParams, MeshShadingParams, OutputDevice,
    PageSinkFactory, PatchShadingParams, RadialShadingParams, StrokeParams,
};
use stet_core::graphics_state::{
    DeviceColor, FillRule, LineCap, LineJoin, Matrix, PathSegment, PsPath,
};

/// Axis-aligned rectangle in device pixel coordinates.
#[derive(Clone, Copy)]
struct ClipRect {
    x0: u32,
    y0: u32, // top-left (inclusive)
    x1: u32,
    y1: u32, // bottom-right (exclusive)
}

impl ClipRect {
    /// Intersect two rectangles. Result may be empty.
    fn intersect(&self, other: &ClipRect) -> ClipRect {
        ClipRect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// True if this rect covers the entire page.
    fn is_full_page(&self, w: u32, h: u32) -> bool {
        self.x0 == 0 && self.y0 == 0 && self.x1 == w && self.y1 == h
    }

    /// Create a mask with 255 inside the rect, 0 outside.
    fn make_mask(self, w: u32, h: u32) -> Option<Mask> {
        if self.is_empty() {
            return None;
        }
        let mut mask = Mask::new(w, h)?;
        let data = mask.data_mut();
        let stride = w as usize;
        for y in self.y0..self.y1 {
            let row_start = y as usize * stride + self.x0 as usize;
            let row_end = y as usize * stride + self.x1 as usize;
            data[row_start..row_end].fill(255);
        }
        Some(mask)
    }
}

/// Clip region: either a simple rectangle (fast) or a full rasterized mask.
enum ClipRegion {
    Rect(ClipRect),
    Mask(Mask),
}

/// tiny-skia based raster device.
pub struct SkiaDevice {
    pixmap: Pixmap,
    /// Page dimensions in device pixels. Stored separately so we can shrink
    /// the pixmap during banded rendering without losing page size info.
    page_w: u32,
    page_h: u32,
    /// Device resolution in DPI (for hairline width decisions).
    dpi: f64,
    clip_region: Option<ClipRegion>,
    /// Cache of rasterized clip masks keyed by path hash.
    /// Only paths seen more than once are cached (cache-on-second-sight).
    clip_mask_cache: HashMap<u64, Mask>,
    clip_mask_seen: HashSet<u64>,
    /// Recycled mask buffer to avoid repeated alloc/dealloc of large masks.
    spare_mask: Option<Mask>,
    /// Receiver for background render result (pipelined multi-page rendering).
    /// Uses rayon::spawn + oneshot channel to avoid OS thread spawn overhead.
    pending_render: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// Factory for creating page sinks (PNG, viewer, etc.).
    sink_factory: Box<dyn PageSinkFactory>,
}

impl SkiaDevice {
    /// Create a new device with the given page dimensions and default PNG output.
    ///
    /// Defers the full-page pixmap allocation — only a 1×1 placeholder is
    /// created here. The full pixmap is allocated lazily in `replay_and_show`
    /// only when the non-banded rendering path is needed.
    pub fn new(width: u32, height: u32) -> Self {
        Self::with_sink_factory(width, height, Box::new(crate::PngSinkFactory))
    }

    /// Create a new device with a custom page sink factory.
    pub fn with_sink_factory(
        width: u32,
        height: u32,
        sink_factory: Box<dyn PageSinkFactory>,
    ) -> Self {
        // Estimate DPI from page height (assumes ~792pt US Letter as reference).
        // Close enough for hairline width threshold decisions.
        let dpi = height as f64 * 72.0 / 792.0;

        // Start with a tiny placeholder. The full-page pixmap is allocated
        // lazily only when the non-banded path is used (small pages / low DPI).
        // For banded rendering, band-sized pixmaps are created in replay_and_show.
        let pixmap = Pixmap::new(1, 1).expect("Failed to create placeholder pixmap");
        Self {
            pixmap,
            page_w: width,
            page_h: height,
            dpi,
            clip_region: None,
            clip_mask_cache: HashMap::new(),
            clip_mask_seen: HashSet::new(),
            spare_mask: None,
            pending_render: None,
            sink_factory,
        }
    }

    /// Ensure `self.pixmap` is allocated at full page dimensions.
    /// Called before non-banded rendering which operates on the full pixmap.
    fn ensure_full_pixmap(&mut self) {
        if self.pixmap.width() != self.page_w || self.pixmap.height() != self.page_h {
            self.pixmap =
                Pixmap::new(self.page_w, self.page_h).expect("Failed to create page pixmap");
            self.pixmap.fill(Color::WHITE);
        }
    }

    /// Get the underlying pixmap (for testing).
    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }
}

/// Convert a PostScript `Matrix` to tiny-skia `Transform` (f32).
fn to_transform(m: &Matrix) -> Transform {
    Transform::from_row(
        m.a as f32,
        m.b as f32,
        m.c as f32,
        m.d as f32,
        m.tx as f32,
        m.ty as f32,
    )
}

/// Convert a `DeviceColor` to tiny-skia `Paint`.
fn to_paint(color: &DeviceColor) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        (color.r * 255.0).round().clamp(0.0, 255.0) as u8,
        (color.g * 255.0).round().clamp(0.0, 255.0) as u8,
        (color.b * 255.0).round().clamp(0.0, 255.0) as u8,
        255,
    );
    paint.anti_alias = true;
    paint
}

/// Convert a `PsPath` to tiny-skia `Path`.
fn build_skia_path(path: &PsPath) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();

    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                pb.move_to(*x as f32, *y as f32);
            }
            PathSegment::LineTo(x, y) => {
                pb.line_to(*x as f32, *y as f32);
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                pb.cubic_to(
                    *x1 as f32, *y1 as f32, *x2 as f32, *y2 as f32, *x3 as f32, *y3 as f32,
                );
            }
            PathSegment::ClosePath => {
                pb.close();
            }
        }
    }

    pb.finish()
}

/// Convert PostScript FillRule to tiny-skia FillRule.
fn to_fill_rule(rule: &FillRule) -> SkiaFillRule {
    match rule {
        FillRule::NonZeroWinding => SkiaFillRule::Winding,
        FillRule::EvenOdd => SkiaFillRule::EvenOdd,
    }
}

/// Convert PostScript LineCap to tiny-skia LineCap.
fn to_line_cap(cap: LineCap) -> SkiaLineCap {
    match cap {
        LineCap::Butt => SkiaLineCap::Butt,
        LineCap::Round => SkiaLineCap::Round,
        LineCap::Square => SkiaLineCap::Square,
    }
}

/// Convert PostScript LineJoin to tiny-skia LineJoin.
fn to_line_join(join: LineJoin) -> SkiaLineJoin {
    match join {
        LineJoin::Miter => SkiaLineJoin::Miter,
        LineJoin::Round => SkiaLineJoin::Round,
        LineJoin::Bevel => SkiaLineJoin::Bevel,
    }
}

/// Detect if a path is an axis-aligned rectangle. Returns pixel-coordinate ClipRect if so.
/// Handles both CW and CCW winding, with optional trailing ClosePath.
fn detect_rect(path: &PsPath, page_w: u32, page_h: u32) -> Option<ClipRect> {
    let segs = &path.segments;
    // Expect: MoveTo + 3 LineTo + ClosePath (5 segments)
    // or MoveTo + 3 LineTo + LineTo(back to start) + ClosePath (6 segments)
    // or MoveTo + 3 LineTo (4 segments, implicitly closed)
    let (move_to, lines, _has_close) = match segs.len() {
        5 => {
            // MoveTo + 3 LineTo + ClosePath
            if !matches!(segs[4], PathSegment::ClosePath) {
                return None;
            }
            (&segs[0], &segs[1..4], true)
        }
        6 => {
            // MoveTo + 4 LineTo + ClosePath (4th LineTo returns to start)
            if !matches!(segs[5], PathSegment::ClosePath) {
                return None;
            }
            (&segs[0], &segs[1..5], true)
        }
        4 => {
            // MoveTo + 3 LineTo (no explicit close)
            (&segs[0], &segs[1..4], false)
        }
        _ => return None,
    };

    let PathSegment::MoveTo(mx, my) = move_to else {
        return None;
    };

    // Collect all corner points
    let mut pts = vec![(*mx, *my)];
    for seg in lines {
        match seg {
            PathSegment::LineTo(x, y) => pts.push((*x, *y)),
            _ => return None,
        }
    }

    // If 5 points (4 LineTos), last must return to start
    if pts.len() == 5 {
        let (fx, fy) = pts[0];
        let (lx, ly) = pts[4];
        if (fx - lx).abs() > 0.01 || (fy - ly).abs() > 0.01 {
            return None;
        }
        pts.truncate(4);
    }

    // Check axis-aligned: each edge must be horizontal or vertical
    for i in 0..4 {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % 4];
        let dx = (x2 - x1).abs();
        let dy = (y2 - y1).abs();
        if dx > 0.01 && dy > 0.01 {
            return None; // diagonal edge
        }
    }

    // Compute bounding box
    let min_x = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let min_y = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let max_x = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let max_y = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);

    // Convert to pixel coords: floor for top-left, ceil for bottom-right, clamp to page
    let x0 = (min_x.floor().max(0.0) as u32).min(page_w);
    let y0 = (min_y.floor().max(0.0) as u32).min(page_h);
    let x1 = (max_x.ceil().max(0.0) as u32).min(page_w);
    let y1 = (max_y.ceil().max(0.0) as u32).min(page_h);

    Some(ClipRect { x0, y0, x1, y1 })
}

/// Zero out mask pixels outside the given rectangle bounds.
fn intersect_mask_with_rect(mask: &mut Mask, rect: &ClipRect, w: u32, h: u32) {
    let data = mask.data_mut();
    let stride = w as usize;

    // Zero rows above rect
    if rect.y0 > 0 {
        let end = (rect.y0 as usize * stride).min(data.len());
        data[..end].fill(0);
    }

    // Zero rows below rect
    if rect.y1 < h {
        let start = (rect.y1 as usize * stride).min(data.len());
        data[start..].fill(0);
    }

    // Zero left and right margins within rect rows
    for y in rect.y0..rect.y1.min(h) {
        let row_start = y as usize * stride;
        // Left margin
        if rect.x0 > 0 {
            let end = row_start + rect.x0 as usize;
            data[row_start..end].fill(0);
        }
        // Right margin
        if rect.x1 < w {
            let start = row_start + rect.x1 as usize;
            let end = row_start + stride;
            data[start..end].fill(0);
        }
    }
}

/// Resolve a ClipRegion to an Option<&Mask> for paint operations.
/// Returns `None` if the clip is empty (caller should skip painting).
/// Returns `Some(None)` if no mask is needed (full page or no clip).
/// Returns `Some(Some(&Mask))` if a mask should be applied.
fn resolve_clip_mask<'a>(
    clip_region: &'a Option<ClipRegion>,
    temp_mask: &'a mut Option<Mask>,
    w: u32,
    h: u32,
) -> Option<Option<&'a Mask>> {
    match clip_region {
        None => Some(None),
        Some(ClipRegion::Mask(m)) => Some(Some(m)),
        Some(ClipRegion::Rect(rect)) => {
            if rect.is_empty() {
                return None; // empty clip → skip painting
            }
            if rect.is_full_page(w, h) {
                return Some(None); // full page → no mask needed
            }
            *temp_mask = rect.make_mask(w, h);
            Some(temp_mask.as_ref())
        }
    }
}

/// Hash a PsPath's segments for clip mask caching. Uses bit-exact f64 comparison
/// since paths are already in device space.
fn hash_clip_path(path: &PsPath, fill_rule: &FillRule) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(fill_rule).hash(&mut hasher);
    for seg in &path.segments {
        match seg {
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
                x1.to_bits().hash(&mut hasher);
                y1.to_bits().hash(&mut hasher);
                x2.to_bits().hash(&mut hasher);
                y2.to_bits().hash(&mut hasher);
                x3.to_bits().hash(&mut hasher);
                y3.to_bits().hash(&mut hasher);
            }
            PathSegment::ClosePath => {
                3u8.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Pixel-multiply two masks: dst[i] = dst[i] * src[i] / 255.
fn intersect_masks(dst: &mut Mask, src: &Mask) {
    let dst_data = dst.data_mut();
    let src_data = src.data();
    for (d, s) in dst_data.iter_mut().zip(src_data.iter()) {
        *d = ((*d as u16 * *s as u16 + 127) / 255) as u8;
    }
}

// ---- Banded rendering support ----

use stet_core::display_list::{DisplayElement, DisplayList};

/// Band-local clip state, rebuilt for each band.
struct BandState {
    clip_region: Option<ClipRegion>,
    spare_mask: Option<Mask>,
    /// Per-band cache (cleared each band since masks are band-sized).
    clip_mask_cache: HashMap<u64, Mask>,
    /// Persists across bands for cache-on-second-sight.
    clip_mask_seen: HashSet<u64>,
    /// Pool of recycled masks to avoid alloc/dealloc (mmap/munmap) per band.
    mask_pool: Vec<Mask>,
}

/// Maximum masks to keep in the recycling pool. Enough to avoid alloc churn
/// without accumulating unbounded memory across bands.
const MAX_POOL_MASKS: usize = 8;

impl BandState {
    /// Recycle all cached masks into the pool, clearing the cache for the next band.
    #[allow(dead_code)]
    fn recycle_cache(&mut self) {
        for (_, mask) in self.clip_mask_cache.drain() {
            if self.mask_pool.len() < MAX_POOL_MASKS {
                self.mask_pool.push(mask);
            }
            // else: drop mask, returning memory to OS
        }
    }

    /// Return a mask to the pool if under capacity, otherwise drop it.
    fn recycle_mask(&mut self, mask: Mask) {
        if self.mask_pool.len() < MAX_POOL_MASKS {
            self.mask_pool.push(mask);
        }
    }

    /// Get a recycled mask or allocate a new one.
    fn take_mask(&mut self, w: u32, h: u32) -> Mask {
        self.spare_mask
            .take()
            .or_else(|| self.mask_pool.pop())
            .unwrap_or_else(|| Mask::new(w, h).expect("Failed to create mask"))
    }
}

/// Y-axis bounding box in device pixels.
struct YBBox {
    y_min: f64,
    y_max: f64,
}

/// A group of display list elements between consecutive InitClip boundaries.
/// Each epoch starts with an InitClip (except possibly the first) and contains
/// all elements up to the next InitClip. Epochs whose paint elements don't
/// overlap a band can be skipped entirely.
struct ClipEpoch {
    /// Index of the first element in this epoch (the InitClip, or 0).
    start_idx: usize,
    /// One past the last element in this epoch.
    end_idx: usize,
    /// Y bounding box of all paint elements (Fill/Stroke/Image) in this epoch.
    /// None if the epoch has no paint elements (pure clip setup).
    paint_bbox: Option<YBBox>,
    /// True if this epoch contains an ErasePage element (must process for all bands).
    has_erase_page: bool,
}

/// Choose band height so that band pixmap + 2 clip masks fit in ~2 MB (L2 cache).
/// Returns `page_h` when banding is not worthwhile (≤2 bands).
fn select_band_height(w: u32, h: u32) -> u32 {
    if w == 0 || h == 0 {
        return h;
    }
    // Per-row cost: w*4 (RGBA) + w*1 (clip mask) + w*1 (spare mask) = w*6
    let per_row = w as u64 * 6;
    let budget = 2 * 1024 * 1024u64; // 2 MB (L2)
    let max_rows = budget / per_row;

    // Floor to power of 2, clamp to [16, h]
    let band = if max_rows >= h as u64 {
        h
    } else {
        let mut p = 1u32;
        while (p as u64) * 2 <= max_rows {
            p *= 2;
        }
        // Minimum 128 rows per band. At very high DPI the L2 budget yields
        // tiny bands (16 rows at 2400 DPI = 1650 bands) where display list
        // replay overhead dominates. 128-row minimum balances L3 cache fit
        // (~15 MB working set at 2400 DPI) against per-band overhead (207 bands).
        // Benchmarked: 16→31.3s, 64→22.5s, 128→21.8s, 256→22.1s.
        p.clamp(128, h)
    };

    // Skip banding if ≤2 bands
    if h.div_ceil(band) <= 2 {
        return h;
    }
    band
}

/// Compute conservative Y bounding boxes for display list elements.
/// Returns `None` for elements that must always be processed (Clip, InitClip, ErasePage).
///
/// All returned Y values are in **device space** (pixel coordinates) so they can be
/// compared directly against band boundaries.
fn precompute_bboxes(list: &DisplayList) -> Vec<Option<YBBox>> {
    list.elements()
        .iter()
        .map(|elem| match elem {
            DisplayElement::Fill { path, .. } => path_y_bbox(path),
            DisplayElement::Stroke { path, params } => stroke_device_y_bbox(path, params),
            DisplayElement::Image { params, .. } => image_y_bbox(params),
            DisplayElement::AxialShading { params } => {
                shading_y_bbox_from_bbox(&params.bbox, &params.ctm)
            }
            DisplayElement::RadialShading { params } => {
                shading_y_bbox_from_bbox(&params.bbox, &params.ctm)
            }
            DisplayElement::MeshShading { params } => {
                shading_y_bbox_from_bbox(&params.bbox, &params.ctm)
            }
            DisplayElement::PatchShading { params } => {
                shading_y_bbox_from_bbox(&params.bbox, &params.ctm)
            }
            _ => None, // Clip, InitClip, ErasePage: always process
        })
        .collect()
}

/// Compute device-space Y bounding box for a shading element.
/// Uses the BBox if present, otherwise returns a full-page sentinel
/// (y_min=0, y_max=very large) so the element is never culled.
fn shading_y_bbox_from_bbox(bbox: &Option<[f64; 4]>, ctm: &Matrix) -> Option<YBBox> {
    if let Some(bbox) = bbox {
        let corners = [
            (bbox[0], bbox[1]),
            (bbox[2], bbox[1]),
            (bbox[0], bbox[3]),
            (bbox[2], bbox[3]),
        ];
        let mut y_min = f64::INFINITY;
        let mut y_max = f64::NEG_INFINITY;
        for (x, y) in &corners {
            let (_, dy) = ctm.transform_point(*x, *y);
            y_min = y_min.min(dy);
            y_max = y_max.max(dy);
        }
        Some(YBBox { y_min, y_max })
    } else {
        // No BBox — shading covers unbounded area; return sentinel so it's
        // never culled by band processing.
        Some(YBBox {
            y_min: 0.0,
            y_max: 1e9,
        })
    }
}

/// Compute device-space Y bounding box for a stroke element.
///
/// Isotropic strokes have paths already in device space (Identity CTM), so
/// `path_y_bbox` gives device-space bounds directly. Anisotropic strokes have
/// paths in user space with the full CTM — we must transform the bounding box
/// through the CTM to get device-space bounds.
fn stroke_device_y_bbox(path: &PsPath, params: &StrokeParams) -> Option<YBBox> {
    let m = &params.ctm;
    let is_identity =
        m.a == 1.0 && m.b == 0.0 && m.c == 0.0 && m.d == 1.0 && m.tx == 0.0 && m.ty == 0.0;

    if is_identity {
        // Path in device space — just read Y coords and expand for stroke width.
        return path_y_bbox(path).map(|mut bbox| {
            let expand = params.line_width * params.miter_limit * 0.5;
            bbox.y_min -= expand;
            bbox.y_max += expand;
            bbox
        });
    }

    // Anisotropic: path in user space. Compute full XY bbox, transform corners
    // through CTM to get device-space Y range.
    let (mut x_min, mut x_max) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut y_min, mut y_max) = (f64::INFINITY, f64::NEG_INFINITY);
    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => {
                x_min = x_min.min(*x);
                x_max = x_max.max(*x);
                y_min = y_min.min(*y);
                y_max = y_max.max(*y);
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                x_min = x_min.min(*x1).min(*x2).min(*x3);
                x_max = x_max.max(*x1).max(*x2).max(*x3);
                y_min = y_min.min(*y1).min(*y2).min(*y3);
                y_max = y_max.max(*y1).max(*y2).max(*y3);
            }
            PathSegment::ClosePath => {}
        }
    }
    if x_min > x_max {
        return None;
    }

    // Transform all 4 corners of user-space bbox to device space
    let corners = [
        (x_min, y_min),
        (x_max, y_min),
        (x_min, y_max),
        (x_max, y_max),
    ];
    let mut dev_y_min = f64::INFINITY;
    let mut dev_y_max = f64::NEG_INFINITY;
    for (x, y) in &corners {
        let dy = m.b * x + m.d * y + m.ty;
        dev_y_min = dev_y_min.min(dy);
        dev_y_max = dev_y_max.max(dy);
    }

    // Expand for stroke width + miter in device-space units.
    // ||[c,d]|| converts user-space line_width to device-space Y expansion.
    let col_y_len = (m.c * m.c + m.d * m.d).sqrt().max(1.0);
    let expand = params.line_width * col_y_len * params.miter_limit * 0.5;
    dev_y_min -= expand;
    dev_y_max += expand;

    Some(YBBox {
        y_min: dev_y_min,
        y_max: dev_y_max,
    })
}

/// Compute Y bounds from path segments (conservative: uses control points for curves).
fn path_y_bbox(path: &PsPath) -> Option<YBBox> {
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(_, y) | PathSegment::LineTo(_, y) => {
                y_min = y_min.min(*y);
                y_max = y_max.max(*y);
            }
            PathSegment::CurveTo { y1, y2, y3, .. } => {
                y_min = y_min.min(*y1).min(*y2).min(*y3);
                y_max = y_max.max(*y1).max(*y2).max(*y3);
            }
            PathSegment::ClosePath => {}
        }
    }
    if y_min <= y_max {
        Some(YBBox { y_min, y_max })
    } else {
        None
    }
}

/// Compute Y bounds for an image element from its transform.
fn image_y_bbox(params: &ImageParams) -> Option<YBBox> {
    let image_inv = params.image_matrix.invert()?;
    let combined = params.ctm.concat(&image_inv);
    let corners = [
        (0.0, 0.0),
        (params.width as f64, 0.0),
        (params.width as f64, params.height as f64),
        (0.0, params.height as f64),
    ];
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for (x, y) in &corners {
        let (_, dy) = combined.transform_point(*x, *y);
        y_min = y_min.min(dy);
        y_max = y_max.max(dy);
    }
    Some(YBBox { y_min, y_max })
}

/// Pre-populate clip_mask_seen with hashes of clip paths that appear ≥2 times.
/// This lets the first band immediately cache repeated clip paths.
fn precompute_clip_seen(list: &DisplayList) -> HashSet<u64> {
    let mut counts: HashMap<u64, u32> = HashMap::new();
    for elem in list.elements() {
        if let DisplayElement::Clip { path, params } = elem {
            let hash = hash_clip_path(path, &params.fill_rule);
            *counts.entry(hash).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|(h, _)| h)
        .collect()
}

/// Build clip epochs — groups of elements between InitClip boundaries.
/// Each epoch's paint_bbox is the union of Y ranges for all paint elements in it.
fn build_clip_epochs(list: &DisplayList, bboxes: &[Option<YBBox>]) -> Vec<ClipEpoch> {
    let elements = list.elements();
    let mut epochs = Vec::new();
    let mut epoch_start = 0;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    let mut has_erase = false;

    for (i, element) in elements.iter().enumerate() {
        // InitClip starts a new epoch (close the previous one first)
        if matches!(element, DisplayElement::InitClip) && i > epoch_start {
            epochs.push(ClipEpoch {
                start_idx: epoch_start,
                end_idx: i,
                paint_bbox: if y_min <= y_max {
                    Some(YBBox { y_min, y_max })
                } else {
                    None
                },
                has_erase_page: has_erase,
            });
            epoch_start = i;
            y_min = f64::INFINITY;
            y_max = f64::NEG_INFINITY;
            has_erase = false;
        }
        if matches!(element, DisplayElement::ErasePage) {
            has_erase = true;
        }
        if let Some(ref bbox) = bboxes[i] {
            y_min = y_min.min(bbox.y_min);
            y_max = y_max.max(bbox.y_max);
        }
    }
    // Final epoch
    if epoch_start < elements.len() {
        epochs.push(ClipEpoch {
            start_idx: epoch_start,
            end_idx: elements.len(),
            paint_bbox: if y_min <= y_max {
                Some(YBBox { y_min, y_max })
            } else {
                None
            },
            has_erase_page: has_erase,
        });
    }
    epochs
}

/// Apply a device-space Y offset to a tiny-skia Transform.
/// The original transform maps from path space to full-page device space;
/// we subtract `y_offset` from `ty` so band rows [y_start, y_start+band_h)
/// map to pixmap rows [0, band_h).
fn offset_transform(t: Transform, y_offset: f32) -> Transform {
    Transform::from_row(t.sx, t.ky, t.kx, t.sy, t.tx, t.ty - y_offset)
}

/// Translate a device-space ClipRect into band-local coordinates.
fn translate_clip_rect(rect: &ClipRect, y_start: u32, band_h: u32) -> ClipRect {
    ClipRect {
        x0: rect.x0,
        y0: rect.y0.saturating_sub(y_start).min(band_h),
        x1: rect.x1,
        y1: rect.y1.saturating_sub(y_start).min(band_h),
    }
}

/// Build a stroke with minimum line-width enforcement (shared by trait impl and band rendering).
/// `dpi` is the device resolution, used to select the hairline minimum width:
/// at ≤150 DPI use 0.6 device pixels; above 150 DPI use 1.0 device pixel.
fn build_stroke(params: &StrokeParams, dpi: f64) -> Stroke {
    let min_lw = {
        let (a, b, c, d) = (params.ctm.a, params.ctm.b, params.ctm.c, params.ctm.d);
        let sum_sq = a * a + b * b + c * c + d * d;
        let diff = ((a * a + b * b - c * c - d * d).powi(2) + 4.0 * (a * c + b * d).powi(2)).sqrt();
        let s_max = (0.5 * (sum_sq + diff)).max(0.0).sqrt();
        let min_px = if dpi <= 150.0 { 0.5 } else { 1.0 };
        if s_max > 1e-10 { min_px / s_max } else { min_px }
    };
    let mut stroke = Stroke {
        width: (params.line_width as f32).max(min_lw as f32),
        line_cap: to_line_cap(params.line_cap),
        line_join: to_line_join(params.line_join),
        miter_limit: params.miter_limit as f32,
        ..Stroke::default()
    };
    if !params.dash_pattern.array.is_empty() {
        let dash_array: Vec<f32> = params
            .dash_pattern
            .array
            .iter()
            .map(|&v| v as f32)
            .collect();
        if let Some(dash) = StrokeDash::new(dash_array, params.dash_pattern.offset as f32) {
            stroke.dash = Some(dash);
        }
    }
    stroke
}

/// Process a single display list element into a band-sized pixmap.
fn render_element_to_band(
    pixmap: &mut Pixmap,
    band_state: &mut BandState,
    element: &DisplayElement,
    y_start: u32,
    band_w: u32,
    band_h: u32,
    dpi: f64,
) {
    let y_off = y_start as f32;
    match element {
        DisplayElement::Fill { path, params } => {
            let Some(skia_path) = build_skia_path(path) else {
                return;
            };
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, band_w, band_h)
            else {
                return;
            };
            let paint = to_paint(&params.color);
            let transform = offset_transform(to_transform(&params.ctm), y_off);
            let fill_rule = to_fill_rule(&params.fill_rule);
            pixmap.fill_path(&skia_path, &paint, fill_rule, transform, mask_ref);
        }
        DisplayElement::Stroke { path, params } => {
            let Some(skia_path) = build_skia_path(path) else {
                return;
            };
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, band_w, band_h)
            else {
                return;
            };
            let paint = to_paint(&params.color);
            let transform = offset_transform(to_transform(&params.ctm), y_off);
            let stroke = build_stroke(params, dpi);
            pixmap.stroke_path(&skia_path, &paint, &stroke, transform, mask_ref);
        }
        DisplayElement::Clip { path, params } => {
            clip_path_band(band_state, path, params, y_start, band_w, band_h);
        }
        DisplayElement::InitClip => {
            if let Some(ClipRegion::Mask(mask)) = band_state.clip_region.take() {
                band_state.recycle_mask(mask);
            }
            band_state.clip_region = None;
        }
        DisplayElement::ErasePage => {
            pixmap.fill(Color::WHITE);
            if let Some(ClipRegion::Mask(mask)) = band_state.clip_region.take() {
                band_state.recycle_mask(mask);
            }
            band_state.clip_region = None;
        }
        DisplayElement::Image { rgba_data, params } => {
            let iw = params.width;
            let ih = params.height;
            let expected = (iw * ih * 4) as usize;
            if rgba_data.len() < expected || iw == 0 || ih == 0 {
                return;
            }
            let Some(image_inv) = params.image_matrix.invert() else {
                return;
            };
            let combined = params.ctm.concat(&image_inv);
            let transform = offset_transform(to_transform(&combined), y_off);

            // Resolve clip for image draw
            let Some(img_pixmap) = tiny_skia::PixmapRef::from_bytes(rgba_data, iw, ih) else {
                return;
            };
            #[allow(unused_assignments)]
            let mut temp_mask = None;
            let mask_ref = match &band_state.clip_region {
                None => None,
                Some(ClipRegion::Mask(m)) => Some(m as &Mask),
                Some(ClipRegion::Rect(rect)) => {
                    if rect.is_empty() {
                        return;
                    } else if rect.is_full_page(band_w, band_h) {
                        None
                    } else {
                        temp_mask = rect.make_mask(band_w, band_h);
                        temp_mask.as_ref()
                    }
                }
            };
            pixmap.draw_pixmap(
                0,
                0,
                img_pixmap,
                &tiny_skia::PixmapPaint::default(),
                transform,
                mask_ref,
            );
        }
        DisplayElement::AxialShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, band_w, band_h)
            else {
                return;
            };
            render_axial_shading_to_pixmap(pixmap, params, y_start, mask_ref);
        }
        DisplayElement::RadialShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, band_w, band_h)
            else {
                return;
            };
            render_radial_shading_to_pixmap(pixmap, params, y_start, mask_ref);
        }
        DisplayElement::MeshShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, band_w, band_h)
            else {
                return;
            };
            render_mesh_shading_to_pixmap(pixmap, params, y_start, mask_ref);
        }
        DisplayElement::PatchShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, band_w, band_h)
            else {
                return;
            };
            render_patch_shading_to_pixmap(pixmap, params, y_start, mask_ref);
        }
    }
}

/// Clip path handling for band rendering. Same logic as SkiaDevice::clip_path
/// but operates on band-local coordinates and band-sized masks.
fn clip_path_band(
    band_state: &mut BandState,
    path: &PsPath,
    params: &ClipParams,
    y_start: u32,
    band_w: u32,
    band_h: u32,
) {
    // Early out: if clip path's Y extent doesn't overlap this band, the clip
    // contributes zero coverage → clip region becomes empty. This avoids
    // expensive rasterization for clips on distant bands.
    if let Some(bbox) = path_y_bbox(path)
        && (bbox.y_max <= y_start as f64 || bbox.y_min >= (y_start + band_h) as f64)
    {
        if let Some(ClipRegion::Mask(mask)) = band_state.clip_region.take() {
            band_state.recycle_mask(mask);
        }
        band_state.clip_region = Some(ClipRegion::Rect(ClipRect {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        }));
        return;
    }

    // Fast path: detect axis-aligned rectangle (in full device space), translate to band-local
    // Pass large page_h to avoid clamping Y to band height during detection
    if let Some(dev_rect) = detect_rect(path, band_w, u32::MAX) {
        let new_rect = translate_clip_rect(&dev_rect, y_start, band_h);
        match band_state.clip_region.take() {
            None => {
                band_state.clip_region = Some(ClipRegion::Rect(new_rect));
            }
            Some(ClipRegion::Rect(existing)) => {
                band_state.clip_region = Some(ClipRegion::Rect(existing.intersect(&new_rect)));
            }
            Some(ClipRegion::Mask(mut mask)) => {
                intersect_mask_with_rect(&mut mask, &new_rect, band_w, band_h);
                band_state.clip_region = Some(ClipRegion::Mask(mask));
            }
        }
        return;
    }

    // Slow path: non-rectangular clip
    let fill_rule = to_fill_rule(&params.fill_rule);
    let path_hash = hash_clip_path(path, &params.fill_rule);
    let prev_region = band_state.clip_region.take();

    // Take a reusable mask first to avoid borrow conflict with cache lookup
    let mut mask = band_state.take_mask(band_w, band_h);

    let path_mask = if let Some(cached) = band_state.clip_mask_cache.get(&path_hash) {
        // Cache hit: memcpy cached data into recycled mask
        mask.data_mut().copy_from_slice(cached.data());
        mask
    } else {
        let Some(skia_path) = build_skia_path(path) else {
            band_state.recycle_mask(mask);
            band_state.clip_region = prev_region;
            return;
        };
        // Rasterize with Y-offset into band-sized mask
        let transform = offset_transform(to_transform(&params.ctm), y_start as f32);
        mask.data_mut().fill(0);
        mask.fill_path(&skia_path, fill_rule, true, transform);
        // Cache on second sight
        if !band_state.clip_mask_seen.insert(path_hash) {
            band_state.clip_mask_cache.insert(path_hash, mask.clone());
        }
        mask
    };

    match prev_region {
        None => {
            band_state.clip_region = Some(ClipRegion::Mask(path_mask));
        }
        Some(ClipRegion::Rect(rect)) => {
            if rect.is_empty() {
                band_state.recycle_mask(path_mask);
            } else {
                let mut mask = path_mask;
                intersect_mask_with_rect(&mut mask, &rect, band_w, band_h);
                band_state.clip_region = Some(ClipRegion::Mask(mask));
            }
        }
        Some(ClipRegion::Mask(mut existing)) => {
            intersect_masks(&mut existing, &path_mask);
            band_state.recycle_mask(path_mask);
            band_state.clip_region = Some(ClipRegion::Mask(existing));
        }
    }
}

impl OutputDevice for SkiaDevice {
    fn fill_path(&mut self, path: &PsPath, params: &FillParams) {
        self.ensure_full_pixmap();
        let Some(skia_path) = build_skia_path(path) else {
            return;
        };
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return; // empty clip
        };

        let paint = to_paint(&params.color);
        let transform = to_transform(&params.ctm);
        let fill_rule = to_fill_rule(&params.fill_rule);

        self.pixmap
            .fill_path(&skia_path, &paint, fill_rule, transform, mask_ref);
    }

    fn stroke_path(&mut self, path: &PsPath, params: &StrokeParams) {
        self.ensure_full_pixmap();
        let Some(skia_path) = build_skia_path(path) else {
            return;
        };
        let paint = to_paint(&params.color);
        let transform = to_transform(&params.ctm);
        let stroke = build_stroke(params, self.dpi);

        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return; // empty clip
        };

        self.pixmap
            .stroke_path(&skia_path, &paint, &stroke, transform, mask_ref);
    }

    fn clip_path(&mut self, path: &PsPath, params: &ClipParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());

        // Fast path: detect axis-aligned rectangle
        if let Some(new_rect) = detect_rect(path, w, h) {
            match self.clip_region.take() {
                None => {
                    self.clip_region = Some(ClipRegion::Rect(new_rect));
                }
                Some(ClipRegion::Rect(existing)) => {
                    // O(1) rect-rect intersection
                    self.clip_region = Some(ClipRegion::Rect(existing.intersect(&new_rect)));
                }
                Some(ClipRegion::Mask(mut mask)) => {
                    // Zero mask pixels outside rect
                    intersect_mask_with_rect(&mut mask, &new_rect, w, h);
                    self.clip_region = Some(ClipRegion::Mask(mask));
                }
            }
            return;
        }

        // Slow path: non-rectangular clip with mask caching + allocation reuse.
        let fill_rule = to_fill_rule(&params.fill_rule);
        let path_hash = hash_clip_path(path, &params.fill_rule);
        let prev_region = self.clip_region.take();

        // Reuse a spare mask buffer if available (avoids alloc/dealloc per tile).
        macro_rules! take_spare {
            ($self:expr, $w:expr, $h:expr) => {
                $self
                    .spare_mask
                    .take()
                    .unwrap_or_else(|| Mask::new($w, $h).expect("Failed to create mask"))
            };
        }

        // Try cache first; rasterize only on miss
        let path_mask = if let Some(cached) = self.clip_mask_cache.get(&path_hash) {
            // Cache hit: copy cached data into reused buffer (memcpy, no alloc)
            let mut mask = take_spare!(self, w, h);
            mask.data_mut().copy_from_slice(cached.data());
            mask
        } else {
            let Some(skia_path) = build_skia_path(path) else {
                self.clip_region = prev_region;
                return;
            };
            let transform = to_transform(&params.ctm);
            let mut mask = take_spare!(self, w, h);
            mask.data_mut().fill(0); // zero before rasterizing (spare may have old data)
            mask.fill_path(&skia_path, fill_rule, true, transform);
            // Cache on second sight: first time just record, second time store
            if !self.clip_mask_seen.insert(path_hash) {
                // Seen before — cache it (this clone only happens once per unique path)
                self.clip_mask_cache.insert(path_hash, mask.clone());
            }
            mask
        };

        match prev_region {
            None => {
                self.clip_region = Some(ClipRegion::Mask(path_mask));
            }
            Some(ClipRegion::Rect(rect)) => {
                if rect.is_empty() {
                    self.spare_mask = Some(path_mask); // recycle
                } else {
                    let mut mask = path_mask;
                    intersect_mask_with_rect(&mut mask, &rect, w, h);
                    self.clip_region = Some(ClipRegion::Mask(mask));
                }
            }
            Some(ClipRegion::Mask(mut existing)) => {
                intersect_masks(&mut existing, &path_mask);
                self.spare_mask = Some(path_mask); // recycle the copy
                self.clip_region = Some(ClipRegion::Mask(existing));
            }
        }
    }

    fn init_clip(&mut self) {
        if let Some(ClipRegion::Mask(mask)) = self.clip_region.take() {
            self.spare_mask = Some(mask);
        }
        self.clip_region = None;
    }

    fn erase_page(&mut self) {
        // Only fill the full pixmap when it's actually allocated (non-banded path).
        // During banding, self.pixmap is a 1×1 placeholder — filling it is harmless.
        self.pixmap.fill(Color::WHITE);
        if let Some(ClipRegion::Mask(mask)) = self.clip_region.take() {
            self.spare_mask = Some(mask);
        }
        self.clip_region = None;
    }

    fn show_page(&mut self, output_path: &str) -> Result<(), String> {
        let w = self.pixmap.width();
        let h = self.pixmap.height();
        let mut sink = self.sink_factory.create_sink(output_path)?;
        sink.begin_page(w, h)?;
        sink.write_rows(self.pixmap.data(), h)?;
        sink.end_page()
    }

    fn draw_image(&mut self, rgba_data: &[u8], params: &ImageParams) {
        self.ensure_full_pixmap();
        let w = params.width;
        let h = params.height;
        let expected = (w * h * 4) as usize;
        if rgba_data.len() < expected || w == 0 || h == 0 {
            return;
        }

        // Create a pixmap from RGBA data
        let Some(img_pixmap) = tiny_skia::PixmapRef::from_bytes(rgba_data, w, h) else {
            return;
        };

        // PostScript image_matrix maps image space → user space.
        // We need: image space → device space = CTM × inv(image_matrix)
        let Some(image_inv) = params.image_matrix.invert() else {
            return;
        };
        let combined = params.ctm.concat(&image_inv);
        let transform = to_transform(&combined);

        let (pw, ph) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, pw, ph) else {
            return; // empty clip
        };

        self.pixmap.draw_pixmap(
            0,
            0,
            img_pixmap,
            &tiny_skia::PixmapPaint::default(),
            transform,
            mask_ref,
        );
    }

    fn paint_axial_shading(&mut self, params: &AxialShadingParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return;
        };
        render_axial_shading_to_pixmap(&mut self.pixmap, params, 0, mask_ref);
    }

    fn paint_radial_shading(&mut self, params: &RadialShadingParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return;
        };
        render_radial_shading_to_pixmap(&mut self.pixmap, params, 0, mask_ref);
    }

    fn paint_mesh_shading(&mut self, params: &MeshShadingParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return;
        };
        render_mesh_shading_to_pixmap(&mut self.pixmap, params, 0, mask_ref);
    }

    fn paint_patch_shading(&mut self, params: &PatchShadingParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return;
        };
        render_patch_shading_to_pixmap(&mut self.pixmap, params, 0, mask_ref);
    }

    fn page_size(&self) -> (u32, u32) {
        (self.page_w, self.page_h)
    }

    fn replay_and_show(&mut self, list: DisplayList, output_path: &str) -> Result<(), String> {
        // Wait for any previous background render to complete
        self.join_pending()?;

        let (page_w, page_h) = self.page_size();
        let band_h = select_band_height(page_w, page_h);

        // If banding not worthwhile, fall back to normal replay + show_page.
        // Lazily allocate the full-page pixmap since it's needed for this path.
        if band_h >= page_h {
            self.ensure_full_pixmap();
            stet_core::display_list::replay_to_device(&list, self);
            return self.show_page(output_path);
        }

        // Banded path: shrink self.pixmap to free memory — we use a
        // band-sized pixmap instead. This avoids holding a multi-GB
        // full-page buffer during rendering.
        if self.pixmap.width() > 1 {
            self.pixmap = Pixmap::new(1, 1).expect("Failed to create placeholder pixmap");
        }

        // Create the sink for this page before spawning background work
        let mut sink = self.sink_factory.create_sink(output_path)?;
        let dpi = self.dpi;

        // Spawn banded rendering on rayon's thread pool, overlapping with
        // interpretation of the next page. Using rayon::spawn avoids OS thread
        // creation overhead and keeps work on the warmed-up pool.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        rayon::spawn(move || {
            let result = render_banded_to_sink(page_w, page_h, band_h, dpi, &list, &mut *sink);
            let _ = tx.send(result);
        });
        self.pending_render = Some(rx);

        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        self.join_pending()
    }
}

impl Drop for SkiaDevice {
    fn drop(&mut self) {
        // Safety net: ensure background render completes before device is destroyed.
        if let Some(rx) = self.pending_render.take() {
            let _ = rx.recv();
        }
    }
}

impl SkiaDevice {
    /// Wait for the pending background render to complete, if any.
    fn join_pending(&mut self) -> Result<(), String> {
        if let Some(rx) = self.pending_render.take() {
            match rx.recv() {
                Ok(result) => result?,
                Err(_) => return Err("Background render task failed".to_string()),
            }
        }
        Ok(())
    }
}

/// Banded rendering as a free function — runs on a background thread.
///
/// Renders the display list in horizontal bands and streams the output
/// to a `PageSink`. This function is self-contained: it creates its own
/// band pixmaps, clip state, and streams rows to the sink.
fn render_banded_to_sink(
    page_w: u32,
    page_h: u32,
    band_h: u32,
    dpi: f64,
    list: &DisplayList,
    sink: &mut dyn stet_core::device::PageSink,
) -> Result<(), String> {
    // Precompute Y bounding boxes for culling
    let bboxes = precompute_bboxes(list);

    // Build clip epochs — groups of elements between InitClip boundaries.
    // Epochs whose paint elements don't overlap a band can be skipped entirely,
    // avoiding both the per-element iteration AND clip mask rasterization.
    let epochs = build_clip_epochs(list, &bboxes);

    // Pre-populate clip_mask_seen so repeated clip paths get cached from first band
    let clip_seen = precompute_clip_seen(list);

    // Extra rows rendered above and below each band to provide anti-aliasing
    // context at band seams. Without this, tiny-skia clips geometry at the
    // pixmap edge, producing visible discontinuities in thin diagonal strokes.
    const BAND_OVERLAP: u32 = 6;

    let render_h = band_h + 2 * BAND_OVERLAP;

    // Initialize the sink for this page
    sink.begin_page(page_w, page_h)?;

    let num_bands = page_h.div_ceil(band_h);
    let elements = list.elements();
    let row_bytes = page_w as usize * 4;

    // Render bands in parallel, write to sink in order.
    // Process in chunks of `chunk_size` bands to limit peak memory
    // (each rendered band is ~band_h * page_w * 4 bytes).
    // Cap at 8 threads — sequential sink writing bottleneck means
    // additional cores yield no speedup (benchmarked: 8→7.8s plateau).
    let chunk_size = rayon::current_num_threads().clamp(1, 8);

    for chunk_start in (0..num_bands).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size as u32).min(num_bands);

        // Render this chunk of bands in parallel
        let rendered: Vec<Vec<u8>> = (chunk_start..chunk_end)
            .into_par_iter()
            .map(|band_idx| {
                let y_start = band_idx * band_h;
                let actual_h = (page_h - y_start).min(band_h);

                let render_y_start = y_start.saturating_sub(BAND_OVERLAP);
                let render_y_end_f = ((y_start + actual_h + BAND_OVERLAP).min(page_h)) as f64;
                let band_offset = y_start - render_y_start;

                // Each thread gets its own pixmap and clip state
                let mut band_pixmap =
                    Pixmap::new(page_w, render_h).expect("Failed to create band pixmap");
                band_pixmap.as_mut().data_mut().fill(0xFF);

                let mut band_state = BandState {
                    clip_region: None,
                    spare_mask: None,
                    clip_mask_cache: HashMap::new(),
                    clip_mask_seen: clip_seen.clone(),
                    mask_pool: Vec::new(),
                };

                // Epoch-based replay
                for epoch in &epochs {
                    if !epoch.has_erase_page {
                        match epoch.paint_bbox {
                            Some(ref pb)
                                if pb.y_max <= render_y_start as f64
                                    || pb.y_min >= render_y_end_f =>
                            {
                                continue;
                            }
                            None => continue,
                            _ => {}
                        }
                    }

                    for i in epoch.start_idx..epoch.end_idx {
                        if let Some(ref bbox) = bboxes[i]
                            && (bbox.y_max <= render_y_start as f64 || bbox.y_min >= render_y_end_f)
                        {
                            continue;
                        }
                        render_element_to_band(
                            &mut band_pixmap,
                            &mut band_state,
                            &elements[i],
                            render_y_start,
                            page_w,
                            render_h,
                            dpi,
                        );
                    }
                }

                // Extract only the actual band rows (skip overlap)
                let start_byte = band_offset as usize * row_bytes;
                let total_bytes = actual_h as usize * row_bytes;
                band_pixmap.data()[start_byte..start_byte + total_bytes].to_vec()
            })
            .collect();

        // Write completed bands to sink in order
        for (i, band_data) in rendered.iter().enumerate() {
            let band_idx = chunk_start + i as u32;
            let y_start = band_idx * band_h;
            let actual_h = (page_h - y_start).min(band_h);
            sink.write_rows(band_data, actual_h)?;
        }
    }

    sink.end_page()
}

// ---- Shading rendering ----

/// Render an axial (linear) gradient shading to a pixmap.
fn render_axial_shading_to_pixmap(
    pixmap: &mut Pixmap,
    params: &AxialShadingParams,
    y_start: u32,
    clip_mask: Option<&Mask>,
) {
    let pw = pixmap.width();
    let ph = pixmap.height();
    if params.color_stops.is_empty() || pw == 0 || ph == 0 {
        return;
    }

    // Transform gradient endpoints from user space to device space via CTM
    let (dx0, dy0) = params.ctm.transform_point(params.x0, params.y0);
    let (dx1, dy1) = params.ctm.transform_point(params.x1, params.y1);

    // Build gradient stops for tiny-skia
    let stops = build_gradient_stops(&params.color_stops);
    if stops.is_empty() {
        return;
    }

    let start = tiny_skia::Point::from_xy(dx0 as f32, (dy0 - y_start as f64) as f32);
    let end = tiny_skia::Point::from_xy(dx1 as f32, (dy1 - y_start as f64) as f32);

    let Some(gradient) = tiny_skia::LinearGradient::new(
        start,
        end,
        stops,
        tiny_skia::SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };

    let paint = Paint {
        shader: gradient,
        anti_alias: true,
        ..Paint::default()
    };

    // Build the fill rect: start with BBox (or full page), then clip based on extend flags.
    let (mut rx_min, mut ry_min, mut rx_max, mut ry_max) = if let Some(bbox) = &params.bbox {
        let (bx0, by0) = params.ctm.transform_point(bbox[0], bbox[1]);
        let (bx1, by1) = params.ctm.transform_point(bbox[2], bbox[3]);
        (
            bx0.min(bx1).max(0.0),
            (by0.min(by1) - y_start as f64).max(0.0),
            bx0.max(bx1).min(pw as f64),
            (by0.max(by1) - y_start as f64).min(ph as f64),
        )
    } else {
        (0.0, 0.0, pw as f64, ph as f64)
    };

    // When extend is false on one side, clip the fill rect so the gradient
    // doesn't paint beyond the endpoint in the gradient axis direction.
    if !params.extend_start || !params.extend_end {
        let axis_x = dx1 - dx0;
        let axis_y = dy1 - dy0;
        let gy0 = dy0 - y_start as f64;
        let gy1 = dy1 - y_start as f64;

        if axis_x.abs() >= axis_y.abs() {
            // Primarily horizontal gradient — clip x bounds
            if !params.extend_start {
                if axis_x >= 0.0 {
                    rx_min = rx_min.max(dx0);
                } else {
                    rx_max = rx_max.min(dx0);
                }
            }
            if !params.extend_end {
                if axis_x >= 0.0 {
                    rx_max = rx_max.min(dx1);
                } else {
                    rx_min = rx_min.max(dx1);
                }
            }
        } else {
            // Primarily vertical gradient — clip y bounds
            if !params.extend_start {
                if axis_y >= 0.0 {
                    ry_min = ry_min.max(gy0);
                } else {
                    ry_max = ry_max.min(gy0);
                }
            }
            if !params.extend_end {
                if axis_y >= 0.0 {
                    ry_max = ry_max.min(gy1);
                } else {
                    ry_min = ry_min.max(gy1);
                }
            }
        }
    }

    let rw = (rx_max - rx_min) as f32;
    let rh = (ry_max - ry_min) as f32;
    let (rx, ry) = (rx_min as f32, ry_min as f32);

    let Some(rect) = tiny_skia::Rect::from_xywh(rx, ry, rw.max(1.0), rh.max(1.0)) else {
        return;
    };
    let mut pb = PathBuilder::new();
    pb.push_rect(rect);
    let Some(path) = pb.finish() else {
        return;
    };

    pixmap.fill_path(
        &path,
        &paint,
        SkiaFillRule::Winding,
        Transform::identity(),
        clip_mask,
    );
}

/// Render a radial gradient shading to a pixmap.
fn render_radial_shading_to_pixmap(
    pixmap: &mut Pixmap,
    params: &RadialShadingParams,
    y_start: u32,
    clip_mask: Option<&Mask>,
) {
    let pw = pixmap.width();
    let ph = pixmap.height();
    if params.color_stops.is_empty() || pw == 0 || ph == 0 {
        return;
    }

    // Transform endpoints to device space
    let (dx0, dy0) = params.ctm.transform_point(params.x0, params.y0);
    let (dx1, dy1) = params.ctm.transform_point(params.x1, params.y1);

    // Scale radii by CTM (approximate: use average scale factor)
    let ctm_scale = ((params.ctm.a * params.ctm.a + params.ctm.b * params.ctm.b).sqrt()
        + (params.ctm.c * params.ctm.c + params.ctm.d * params.ctm.d).sqrt())
        / 2.0;
    let dr0 = params.r0 * ctm_scale;
    let dr1 = params.r1 * ctm_scale;

    // Compute pixel bounds (default: full pixmap, clipped by BBox if present)
    let (px_min, py_min, px_max, py_max) = if let Some(bbox) = &params.bbox {
        let (bx0, by0) = params.ctm.transform_point(bbox[0], bbox[1]);
        let (bx1, by1) = params.ctm.transform_point(bbox[2], bbox[3]);
        let x_min = bx0.min(bx1).max(0.0) as u32;
        let y_min_dev = by0.min(by1).max(0.0);
        let x_max = (bx0.max(bx1).ceil() as u32).min(pw);
        let y_max_dev = by0.max(by1).ceil();
        // Adjust for band offset
        let y_min = if y_min_dev > y_start as f64 {
            (y_min_dev - y_start as f64) as u32
        } else {
            0
        };
        let y_max = ((y_max_dev - y_start as f64).ceil() as u32).min(ph);
        (x_min, y_min, x_max, y_max)
    } else {
        (0, 0, pw, ph)
    };

    // Software rasterize the radial gradient
    let data = pixmap.data_mut();
    let stride = pw as usize * 4;

    for py in py_min..py_max {
        let dev_y = py as f64 + y_start as f64;
        for px in px_min..px_max {
            let dev_x = px as f64;

            // Solve for t: point is on circle(center(t), radius(t))
            // center(t) = (1-t)*c0 + t*c1, radius(t) = (1-t)*r0 + t*r1
            // |P - center(t)|^2 = radius(t)^2
            let t = solve_radial_t(dev_x, dev_y, dx0, dy0, dr0, dx1, dy1, dr1);
            if let Some(t) = t {
                let clamped = t.clamp(0.0, 1.0);
                // Check extend
                if t < 0.0 && !params.extend_start {
                    continue;
                }
                if t > 1.0 && !params.extend_end {
                    continue;
                }
                let color = interpolate_color_stops(&params.color_stops, clamped);
                let offset = py as usize * stride + px as usize * 4;
                let r = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                let g = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                let b = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;

                // Apply clip mask
                if let Some(mask) = clip_mask {
                    let mask_val = mask.data()[py as usize * pw as usize + px as usize];
                    if mask_val == 0 {
                        continue;
                    }
                }

                data[offset] = r;
                data[offset + 1] = g;
                data[offset + 2] = b;
                data[offset + 3] = 255;
            }
        }
    }
}

/// Solve for the parameter t of a two-circle radial gradient at point (px, py).
/// Returns the largest valid t, or None if no solution exists.
#[allow(clippy::too_many_arguments)]
fn solve_radial_t(
    px: f64,
    py: f64,
    x0: f64,
    y0: f64,
    r0: f64,
    x1: f64,
    y1: f64,
    r1: f64,
) -> Option<f64> {
    // Parametric: C(t) = (1-t)*C0 + t*C1, R(t) = (1-t)*r0 + t*r1
    // Solve: (px - Cx(t))^2 + (py - Cy(t))^2 = R(t)^2
    let cdx = x1 - x0;
    let cdy = y1 - y0;
    let dr = r1 - r0;

    let a = cdx * cdx + cdy * cdy - dr * dr;
    let dpx = px - x0;
    let dpy = py - y0;
    let b = 2.0 * (dpx * cdx + dpy * cdy - r0 * dr);
    let c = dpx * dpx + dpy * dpy - r0 * r0;

    if a.abs() < 1e-10 {
        // Linear case
        if b.abs() < 1e-10 {
            return None;
        }
        let t = -c / b;
        let radius = r0 + t * dr;
        if radius >= 0.0 {
            return Some(t);
        }
        return None;
    }

    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 {
        return None;
    }
    let sqrt_d = discriminant.sqrt();
    let t1 = (-b + sqrt_d) / (2.0 * a);
    let t2 = (-b - sqrt_d) / (2.0 * a);

    // Pick the largest t where radius >= 0
    let mut best: Option<f64> = None;
    for t in [t1, t2] {
        let radius = r0 + t * dr;
        if radius >= 0.0 {
            best = Some(match best {
                Some(prev) => prev.max(t),
                None => t,
            });
        }
    }
    best
}

/// Render a Gouraud-shaded triangle mesh to a pixmap.
fn render_mesh_shading_to_pixmap(
    pixmap: &mut Pixmap,
    params: &MeshShadingParams,
    y_start: u32,
    clip_mask: Option<&Mask>,
) {
    let pw = pixmap.width() as usize;
    let ph = pixmap.height() as usize;
    if pw == 0 || ph == 0 {
        return;
    }
    let data = pixmap.data_mut();
    let stride = pw * 4;

    for tri in &params.triangles {
        // Transform vertices to device space
        let (x0, y0) = params.ctm.transform_point(tri.v0.x, tri.v0.y);
        let (x1, y1) = params.ctm.transform_point(tri.v1.x, tri.v1.y);
        let (x2, y2) = params.ctm.transform_point(tri.v2.x, tri.v2.y);

        // Offset for banding
        let y0b = y0 - y_start as f64;
        let y1b = y1 - y_start as f64;
        let y2b = y2 - y_start as f64;

        // Bounding box (clamp to pixmap)
        let min_x = x0.min(x1).min(x2).floor().max(0.0) as usize;
        let max_x = (x0.max(x1).max(x2).ceil() as usize).min(pw);
        let min_y = y0b.min(y1b).min(y2b).floor().max(0.0) as usize;
        let max_y = (y0b.max(y1b).max(y2b).ceil() as usize).min(ph);

        if min_x >= max_x || min_y >= max_y {
            continue;
        }

        // Precompute barycentric denominator
        let denom = (y1b - y2b) * (x0 - x2) + (x2 - x1) * (y0b - y2b);
        if denom.abs() < 1e-10 {
            continue; // degenerate triangle
        }
        let inv_denom = 1.0 / denom;

        for py in min_y..max_y {
            for px in min_x..max_x {
                let pxf = px as f64 + 0.5;
                let pyf = py as f64 + 0.5;

                // Barycentric coordinates
                let w0 = ((y1b - y2b) * (pxf - x2) + (x2 - x1) * (pyf - y2b)) * inv_denom;
                let w1 = ((y2b - y0b) * (pxf - x2) + (x0 - x2) * (pyf - y2b)) * inv_denom;
                let w2 = 1.0 - w0 - w1;

                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }

                // Apply clip mask
                if let Some(mask) = clip_mask {
                    let mask_val = mask.data()[py * pw + px];
                    if mask_val == 0 {
                        continue;
                    }
                }

                // Interpolate color
                let r = w0 * tri.v0.color.r + w1 * tri.v1.color.r + w2 * tri.v2.color.r;
                let g = w0 * tri.v0.color.g + w1 * tri.v1.color.g + w2 * tri.v2.color.g;
                let b = w0 * tri.v0.color.b + w1 * tri.v1.color.b + w2 * tri.v2.color.b;

                let offset = py * stride + px * 4;
                data[offset] = (r * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 1] = (g * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 2] = (b * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 3] = 255;
            }
        }
    }
}

/// Render a Coons/tensor-product patch mesh by subdividing into triangles.
fn render_patch_shading_to_pixmap(
    pixmap: &mut Pixmap,
    params: &PatchShadingParams,
    y_start: u32,
    clip_mask: Option<&Mask>,
) {
    // Subdivide each patch into triangles, then render as mesh
    let mut triangles = Vec::new();

    for patch in &params.patches {
        if patch.points.len() >= 12 {
            subdivide_patch_to_triangles(patch, &mut triangles);
        }
    }

    if !triangles.is_empty() {
        let mesh_params = MeshShadingParams {
            triangles,
            ctm: params.ctm,
            bbox: params.bbox,
        };
        render_mesh_shading_to_pixmap(pixmap, &mesh_params, y_start, clip_mask);
    }
}

/// Subdivide a Coons/tensor patch into triangles via recursive de Casteljau.
/// Uses a simple grid subdivision approach: evaluate the patch at NxN points
/// and triangulate the resulting grid.
fn subdivide_patch_to_triangles(
    patch: &stet_core::device::ShadingPatch,
    triangles: &mut Vec<stet_core::device::ShadingTriangle>,
) {
    let n = 8; // Subdivision level (8x8 grid = 128 triangles per patch)

    // Evaluate patch at grid points
    let mut grid: Vec<(f64, f64, DeviceColor)> = Vec::with_capacity((n + 1) * (n + 1));

    for row in 0..=n {
        let v = row as f64 / n as f64;
        for col in 0..=n {
            let u = col as f64 / n as f64;
            let (x, y) = eval_coons_patch(patch, u, v);
            let color = bilinear_color(&patch.colors, u, v);
            grid.push((x, y, color));
        }
    }

    // Triangulate grid
    let cols = n + 1;
    for row in 0..n {
        for col in 0..n {
            let i00 = row * cols + col;
            let i10 = i00 + 1;
            let i01 = i00 + cols;
            let i11 = i01 + 1;

            let (x00, y00, c00) = &grid[i00];
            let (x10, y10, c10) = &grid[i10];
            let (x01, y01, c01) = &grid[i01];
            let (x11, y11, c11) = &grid[i11];

            use stet_core::device::ShadingVertex;
            triangles.push(stet_core::device::ShadingTriangle {
                v0: ShadingVertex {
                    x: *x00,
                    y: *y00,
                    color: c00.clone(),
                },
                v1: ShadingVertex {
                    x: *x10,
                    y: *y10,
                    color: c10.clone(),
                },
                v2: ShadingVertex {
                    x: *x01,
                    y: *y01,
                    color: c01.clone(),
                },
            });
            triangles.push(stet_core::device::ShadingTriangle {
                v0: ShadingVertex {
                    x: *x10,
                    y: *y10,
                    color: c10.clone(),
                },
                v1: ShadingVertex {
                    x: *x11,
                    y: *y11,
                    color: c11.clone(),
                },
                v2: ShadingVertex {
                    x: *x01,
                    y: *y01,
                    color: c01.clone(),
                },
            });
        }
    }
}

/// Evaluate a Coons patch at parameter (u, v).
/// The 12 control points define 4 cubic Bezier boundary curves.
fn eval_coons_patch(patch: &stet_core::device::ShadingPatch, u: f64, v: f64) -> (f64, f64) {
    let pts = &patch.points;
    if pts.len() < 12 {
        return (0.0, 0.0);
    }

    // Side 0 (bottom): pts[0..4], u goes 0→1
    // Side 1 (right): pts[3..7], v goes 0→1
    // Side 2 (top): pts[6..10], u goes 1→0 (reversed)
    // Side 3 (left): pts[9..12] + pts[0], v goes 1→0 (reversed)
    let c0 = eval_cubic_bezier(pts[0], pts[1], pts[2], pts[3], u);
    let c2 = eval_cubic_bezier(pts[6], pts[7], pts[8], pts[9], 1.0 - u);
    let d0 = eval_cubic_bezier(pts[0], pts[11], pts[10], pts[9], v);
    let d1 = eval_cubic_bezier(pts[3], pts[4], pts[5], pts[6], v);

    // Bilinear blending of corners
    let p00 = pts[0];
    let p10 = pts[3];
    let p01 = pts[9];
    let p11 = pts[6];
    let bx = (1.0 - u) * (1.0 - v) * p00.0
        + u * (1.0 - v) * p10.0
        + (1.0 - u) * v * p01.0
        + u * v * p11.0;
    let by = (1.0 - u) * (1.0 - v) * p00.1
        + u * (1.0 - v) * p10.1
        + (1.0 - u) * v * p01.1
        + u * v * p11.1;

    // Coons blending: S(u,v) = c(u,v) + d(u,v) - B(u,v)
    let x = (1.0 - v) * c0.0 + v * c2.0 + (1.0 - u) * d0.0 + u * d1.0 - bx;
    let y = (1.0 - v) * c0.1 + v * c2.1 + (1.0 - u) * d0.1 + u * d1.1 - by;

    (x, y)
}

/// Evaluate a cubic Bezier curve at parameter t.
fn eval_cubic_bezier(
    p0: (f64, f64),
    p1: (f64, f64),
    p2: (f64, f64),
    p3: (f64, f64),
    t: f64,
) -> (f64, f64) {
    let s = 1.0 - t;
    let s2 = s * s;
    let t2 = t * t;
    let b0 = s2 * s;
    let b1 = 3.0 * s2 * t;
    let b2 = 3.0 * s * t2;
    let b3 = t2 * t;
    (
        b0 * p0.0 + b1 * p1.0 + b2 * p2.0 + b3 * p3.0,
        b0 * p0.1 + b1 * p1.1 + b2 * p2.1 + b3 * p3.1,
    )
}

/// Bilinear color interpolation across patch corners.
fn bilinear_color(colors: &[DeviceColor; 4], u: f64, v: f64) -> DeviceColor {
    let r = (1.0 - u) * (1.0 - v) * colors[0].r
        + u * (1.0 - v) * colors[1].r
        + (1.0 - u) * v * colors[3].r
        + u * v * colors[2].r;
    let g = (1.0 - u) * (1.0 - v) * colors[0].g
        + u * (1.0 - v) * colors[1].g
        + (1.0 - u) * v * colors[3].g
        + u * v * colors[2].g;
    let b = (1.0 - u) * (1.0 - v) * colors[0].b
        + u * (1.0 - v) * colors[1].b
        + (1.0 - u) * v * colors[3].b
        + u * v * colors[2].b;
    DeviceColor::from_rgb(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
}

/// Build tiny-skia gradient stops from color stops.
fn build_gradient_stops(stops: &[stet_core::device::ColorStop]) -> Vec<tiny_skia::GradientStop> {
    let mut result = Vec::with_capacity(stops.len());
    for stop in stops {
        let r = (stop.color.r * 255.0).round().clamp(0.0, 255.0) as u8;
        let g = (stop.color.g * 255.0).round().clamp(0.0, 255.0) as u8;
        let b = (stop.color.b * 255.0).round().clamp(0.0, 255.0) as u8;
        result.push(tiny_skia::GradientStop::new(
            stop.position as f32,
            Color::from_rgba8(r, g, b, 255),
        ));
    }
    result
}

/// Interpolate between color stops at a given position (0.0..=1.0).
fn interpolate_color_stops(stops: &[stet_core::device::ColorStop], position: f64) -> DeviceColor {
    if stops.is_empty() {
        return DeviceColor::from_gray(0.0);
    }
    if stops.len() == 1 || position <= stops[0].position {
        return stops[0].color.clone();
    }
    if position >= stops.last().unwrap().position {
        return stops.last().unwrap().color.clone();
    }

    // Find the two stops bracketing this position
    for i in 1..stops.len() {
        if position <= stops[i].position {
            let t0 = stops[i - 1].position;
            let t1 = stops[i].position;
            let frac = if (t1 - t0).abs() < 1e-10 {
                0.0
            } else {
                (position - t0) / (t1 - t0)
            };
            let c0 = &stops[i - 1].color;
            let c1 = &stops[i].color;
            return DeviceColor::from_rgb(
                (c0.r + frac * (c1.r - c0.r)).clamp(0.0, 1.0),
                (c0.g + frac * (c1.g - c0.g)).clamp(0.0, 1.0),
                (c0.b + frac * (c1.b - c0.b)).clamp(0.0, 1.0),
            );
        }
    }

    stops.last().unwrap().color.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::graphics_state::DashPattern;

    #[test]
    fn test_create_device() {
        let dev = SkiaDevice::new(100, 100);
        assert_eq!(dev.page_size(), (100, 100));
    }

    #[test]
    fn test_fill_rect() {
        let mut dev = SkiaDevice::new(100, 100);
        let mut path = PsPath::new();
        path.segments.push(PathSegment::MoveTo(10.0, 10.0));
        path.segments.push(PathSegment::LineTo(90.0, 10.0));
        path.segments.push(PathSegment::LineTo(90.0, 90.0));
        path.segments.push(PathSegment::LineTo(10.0, 90.0));
        path.segments.push(PathSegment::ClosePath);

        let params = FillParams {
            color: DeviceColor::from_rgb(1.0, 0.0, 0.0),
            fill_rule: FillRule::NonZeroWinding,
            ctm: Matrix::identity(),
        };
        dev.fill_path(&path, &params);

        // Check that pixel at center is red
        let pixel = dev.pixmap().pixel(50, 50).unwrap();
        assert_eq!(pixel.red(), 255);
        assert_eq!(pixel.green(), 0);
        assert_eq!(pixel.blue(), 0);
    }

    #[test]
    fn test_stroke_line() {
        let mut dev = SkiaDevice::new(100, 100);
        let mut path = PsPath::new();
        path.segments.push(PathSegment::MoveTo(10.0, 50.0));
        path.segments.push(PathSegment::LineTo(90.0, 50.0));

        let params = StrokeParams {
            color: DeviceColor::from_rgb(0.0, 0.0, 1.0),
            line_width: 4.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern: DashPattern::solid(),
            ctm: Matrix::identity(),
        };
        dev.stroke_path(&path, &params);

        // Check that pixel on the line is blue
        let pixel = dev.pixmap().pixel(50, 50).unwrap();
        assert_eq!(pixel.blue(), 255);
    }

    #[test]
    fn test_clip() {
        let mut dev = SkiaDevice::new(100, 100);

        // Set clip to left half
        let mut clip_path = PsPath::new();
        clip_path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        clip_path.segments.push(PathSegment::LineTo(50.0, 0.0));
        clip_path.segments.push(PathSegment::LineTo(50.0, 100.0));
        clip_path.segments.push(PathSegment::LineTo(0.0, 100.0));
        clip_path.segments.push(PathSegment::ClosePath);

        let clip_params = ClipParams {
            fill_rule: FillRule::NonZeroWinding,
            ctm: Matrix::identity(),
        };
        dev.clip_path(&clip_path, &clip_params);

        // Fill entire page with red
        let mut fill_path = PsPath::new();
        fill_path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        fill_path.segments.push(PathSegment::LineTo(100.0, 0.0));
        fill_path.segments.push(PathSegment::LineTo(100.0, 100.0));
        fill_path.segments.push(PathSegment::LineTo(0.0, 100.0));
        fill_path.segments.push(PathSegment::ClosePath);

        let fill_params = FillParams {
            color: DeviceColor::from_rgb(1.0, 0.0, 0.0),
            fill_rule: FillRule::NonZeroWinding,
            ctm: Matrix::identity(),
        };
        dev.fill_path(&fill_path, &fill_params);

        // Left half should be red
        let left_pixel = dev.pixmap().pixel(25, 50).unwrap();
        assert_eq!(left_pixel.red(), 255);

        // Right half should still be white
        let right_pixel = dev.pixmap().pixel(75, 50).unwrap();
        assert_eq!(right_pixel.red(), 255);
        assert_eq!(right_pixel.green(), 255); // white
    }

    #[test]
    fn test_erase_page() {
        let mut dev = SkiaDevice::new(100, 100);
        // Fill with red
        let mut path = PsPath::new();
        path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        path.segments.push(PathSegment::LineTo(100.0, 0.0));
        path.segments.push(PathSegment::LineTo(100.0, 100.0));
        path.segments.push(PathSegment::LineTo(0.0, 100.0));
        path.segments.push(PathSegment::ClosePath);
        let params = FillParams {
            color: DeviceColor::from_rgb(1.0, 0.0, 0.0),
            fill_rule: FillRule::NonZeroWinding,
            ctm: Matrix::identity(),
        };
        dev.fill_path(&path, &params);

        dev.erase_page();

        // Should be white again
        let pixel = dev.pixmap().pixel(50, 50).unwrap();
        assert_eq!(pixel.red(), 255);
        assert_eq!(pixel.green(), 255);
        assert_eq!(pixel.blue(), 255);
    }

    #[test]
    fn test_show_page() {
        let mut dev = SkiaDevice::new(10, 10);
        let result = dev.show_page("/tmp/stet_test_output.png");
        assert!(result.is_ok());
        assert!(std::path::Path::new("/tmp/stet_test_output.png").exists());
        std::fs::remove_file("/tmp/stet_test_output.png").ok();
    }

    #[test]
    fn test_transform() {
        let mut dev = SkiaDevice::new(200, 200);
        // Draw at origin with a translate transform
        let mut path = PsPath::new();
        path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        path.segments.push(PathSegment::LineTo(10.0, 0.0));
        path.segments.push(PathSegment::LineTo(10.0, 10.0));
        path.segments.push(PathSegment::LineTo(0.0, 10.0));
        path.segments.push(PathSegment::ClosePath);

        let params = FillParams {
            color: DeviceColor::from_rgb(0.0, 1.0, 0.0),
            fill_rule: FillRule::NonZeroWinding,
            ctm: Matrix::translate(100.0, 100.0),
        };
        dev.fill_path(&path, &params);

        // Pixel at translated location should be green
        let pixel = dev.pixmap().pixel(105, 105).unwrap();
        assert_eq!(pixel.green(), 255);
        assert_eq!(pixel.red(), 0);
    }
}
