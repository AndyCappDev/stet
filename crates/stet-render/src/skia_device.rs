// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! tiny-skia implementation of the `OutputDevice` trait.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

#[cfg(feature = "parallel")]
use rayon::prelude::*;
use tiny_skia::{
    Color, FillRule as SkiaFillRule, LineCap as SkiaLineCap, LineJoin as SkiaLineJoin, Mask, Paint,
    PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

use stet_core::device::{
    AxialShadingParams, ClipParams, FillParams, ImageColorSpace, ImageParams, MeshShadingParams,
    OutputDevice, PageSinkFactory, PatchShadingParams, RadialShadingParams, StrokeParams,
};
use stet_core::graphics_state::{
    DeviceColor, FillRule, LineCap, LineJoin, Matrix, PathSegment, PsPath,
};
use stet_core::icc::IccCache;

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
    /// Raw bytes of the system CMYK ICC profile (for building render-thread IccCaches).
    system_cmyk_bytes: Option<std::sync::Arc<Vec<u8>>>,
    /// Transient IccCache used during non-banded replay_to_device rendering.
    render_icc_cache: Option<IccCache>,
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
            system_cmyk_bytes: None,
            render_icc_cache: None,
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

    /// Set the system CMYK ICC profile bytes for ICC-aware rendering.
    pub fn set_system_cmyk_bytes(&mut self, bytes: std::sync::Arc<Vec<u8>>) {
        self.system_cmyk_bytes = Some(bytes);
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
fn precompute_bboxes(list: &DisplayList, dpi: f64) -> Vec<Option<YBBox>> {
    list.elements()
        .iter()
        .map(|elem| match elem {
            DisplayElement::Fill { path, .. } => path_y_bbox(path),
            DisplayElement::Stroke { path, params } => stroke_device_y_bbox(path, params, dpi),
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
            DisplayElement::PatternFill { params } => path_y_bbox(&params.path),
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
fn stroke_device_y_bbox(path: &PsPath, params: &StrokeParams, dpi: f64) -> Option<YBBox> {
    let m = &params.ctm;
    let is_identity =
        m.a == 1.0 && m.b == 0.0 && m.c == 0.0 && m.d == 1.0 && m.tx == 0.0 && m.ty == 0.0;

    // Use effective line width: actual width or hairline minimum, whichever is larger
    let effective_lw = params.line_width.max(hairline_min_width(&params.ctm, dpi));

    if is_identity {
        // Path in device space — just read Y coords and expand for stroke width.
        return path_y_bbox(path).map(|mut bbox| {
            let expand = effective_lw * params.miter_limit * 0.5;
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
    let expand = effective_lw * col_y_len * params.miter_limit * 0.5;
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

/// Apply a combined offset + scale to a tiny-skia Transform for viewport rendering.
/// Maps device-space coordinates into viewport-local pixel coordinates:
///   output_x = (device_x - vp_x) * scale_x
///   output_y = (device_y - vp_y) * scale_y
fn viewport_transform(t: Transform, vp_x: f32, vp_y: f32, scale_x: f32, scale_y: f32) -> Transform {
    // Post-compose: first apply `t` (path→device), then translate(-vp_x,-vp_y), then scale
    Transform::from_row(
        t.sx * scale_x,
        t.ky * scale_y,
        t.kx * scale_x,
        t.sy * scale_y,
        (t.tx - vp_x) * scale_x,
        (t.ty - vp_y) * scale_y,
    )
}

/// Lanczos3 windowed-sinc kernel.
fn lanczos3(x: f64) -> f64 {
    if x.abs() < 1e-8 {
        1.0
    } else if x.abs() >= 3.0 {
        0.0
    } else {
        let px = std::f64::consts::PI * x;
        (px.sin() / px) * ((px / 3.0).sin() / (px / 3.0))
    }
}

/// Lanczos3 separable resample (two-pass: horizontal then vertical).
fn lanczos3_resample(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    // Pass 1: horizontal (sw → dw for each of sh rows), into f64 intermediate.
    let ratio_x = sw as f64 / dw as f64;
    let radius_x = 3.0 * ratio_x.max(1.0);
    let inv_ratio_x = ratio_x.max(1.0);
    let mut tmp = vec![0.0f64; dw as usize * sh as usize * 4];

    for y in 0..sh as usize {
        let src_row = y * sw as usize * 4;
        let dst_row = y * dw as usize * 4;
        for ox in 0..dw as usize {
            let center = (ox as f64 + 0.5) * ratio_x - 0.5;
            let left = (center - radius_x).ceil() as i64;
            let right = (center + radius_x).floor() as i64;
            let left = left.max(0) as usize;
            let right = right.min(sw as i64 - 1) as usize;

            let (mut sr, mut sg, mut sb, mut sa, mut sw_sum) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for ix in left..=right {
                let w = lanczos3((ix as f64 - center) / inv_ratio_x);
                let si = src_row + ix * 4;
                sr += src[si] as f64 * w;
                sg += src[si + 1] as f64 * w;
                sb += src[si + 2] as f64 * w;
                sa += src[si + 3] as f64 * w;
                sw_sum += w;
            }
            if sw_sum > 0.0 {
                let inv = 1.0 / sw_sum;
                let di = dst_row + ox * 4;
                tmp[di] = sr * inv;
                tmp[di + 1] = sg * inv;
                tmp[di + 2] = sb * inv;
                tmp[di + 3] = sa * inv;
            }
        }
    }

    // Pass 2: vertical (sh → dh for each of dw columns), f64 → u8.
    let ratio_y = sh as f64 / dh as f64;
    let radius_y = 3.0 * ratio_y.max(1.0);
    let inv_ratio_y = ratio_y.max(1.0);
    let tmp_stride = dw as usize * 4;
    let mut out = vec![0u8; dw as usize * dh as usize * 4];

    for x in 0..dw as usize {
        for oy in 0..dh as usize {
            let center = (oy as f64 + 0.5) * ratio_y - 0.5;
            let top = (center - radius_y).ceil() as i64;
            let bottom = (center + radius_y).floor() as i64;
            let top = top.max(0) as usize;
            let bottom = bottom.min(sh as i64 - 1) as usize;

            let (mut sr, mut sg, mut sb, mut sa, mut sw_sum) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for iy in top..=bottom {
                let w = lanczos3((iy as f64 - center) / inv_ratio_y);
                let si = iy * tmp_stride + x * 4;
                sr += tmp[si] * w;
                sg += tmp[si + 1] * w;
                sb += tmp[si + 2] * w;
                sa += tmp[si + 3] * w;
                sw_sum += w;
            }
            if sw_sum > 0.0 {
                let inv = 1.0 / sw_sum;
                let di = (oy * dw as usize + x) * 4;
                out[di] = (sr * inv).round().clamp(0.0, 255.0) as u8;
                out[di + 1] = (sg * inv).round().clamp(0.0, 255.0) as u8;
                out[di + 2] = (sb * inv).round().clamp(0.0, 255.0) as u8;
                out[di + 3] = (sa * inv).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Pre-downsample an image when the transform indicates significant downscaling.
///
/// tiny-skia's bilinear filter only samples a 2×2 neighborhood — it has no mipmap
/// support, so large downscale ratios cause severe aliasing (e.g., 300 DPI bitmap
/// fonts rendered at screen resolution).
///
/// For axis-aligned transforms: Lanczos3 resample to the exact target dimensions
/// (matching Cairo's FILTER_BEST quality).
///
/// Build an `IccCache` from ICC profiles found in a display list.
///
/// Registers all unique ICCBased profiles and optionally the system CMYK profile.
pub fn build_icc_cache_for_list(
    list: &DisplayList,
    system_cmyk_bytes: Option<&std::sync::Arc<Vec<u8>>>,
) -> IccCache {
    let mut cache = IccCache::new();
    let mut seen = HashSet::new();

    // Register system CMYK profile first
    if let Some(cmyk_bytes) = system_cmyk_bytes {
        if let Some(hash) = cache.register_profile(cmyk_bytes) {
            seen.insert(hash);
            // Set the default CMYK hash so convert_image_8bit works for DeviceCMYK
            cache.set_default_cmyk_hash(hash);
        }
    }

    // Scan display list for ICCBased images
    for element in list.elements() {
        let cs = match element {
            DisplayElement::Image { params, .. } => Some(&params.color_space),
            _ => None,
        };
        if let Some(ImageColorSpace::ICCBased {
            profile_hash,
            profile_data,
            ..
        }) = cs
        {
            if seen.insert(*profile_hash) {
                cache.register_profile(profile_data);
            }
        }
        // Also check Indexed with ICCBased base
        if let Some(ImageColorSpace::Indexed { base, .. }) = cs {
            if let ImageColorSpace::ICCBased {
                profile_hash,
                profile_data,
                ..
            } = base.as_ref()
            {
                if seen.insert(*profile_hash) {
                    cache.register_profile(profile_data);
                }
            }
        }
    }

    cache
}

/// Convert raw image samples to RGBA for rasterization.
///
/// Handles all `ImageColorSpace` variants, producing width×height×4 RGBA bytes.
fn samples_to_rgba(data: &[u8], params: &ImageParams, icc: Option<&IccCache>) -> Vec<u8> {
    let w = params.width as usize;
    let h = params.height as usize;
    let npixels = w * h;
    match &params.color_space {
        ImageColorSpace::PreconvertedRGBA => {
            // Already RGBA — just return as-is
            data.to_vec()
        }
        ImageColorSpace::DeviceGray => {
            let mut rgba = vec![255u8; npixels * 4];
            for i in 0..npixels {
                let g = data.get(i).copied().unwrap_or(0);
                let pi = i * 4;
                rgba[pi] = g;
                rgba[pi + 1] = g;
                rgba[pi + 2] = g;
            }
            rgba
        }
        ImageColorSpace::DeviceRGB => {
            let mut rgba = vec![255u8; npixels * 4];
            for i in 0..npixels {
                let si = i * 3;
                let pi = i * 4;
                rgba[pi] = data.get(si).copied().unwrap_or(0);
                rgba[pi + 1] = data.get(si + 1).copied().unwrap_or(0);
                rgba[pi + 2] = data.get(si + 2).copied().unwrap_or(0);
            }
            rgba
        }
        ImageColorSpace::DeviceCMYK => {
            // Try ICC-based CMYK→RGB conversion via system CMYK profile.
            // Convert as many complete pixels as the data allows; PLRM-fallback
            // for any remaining pixels with insufficient data.
            if let Some(cache) = icc {
                if let Some(cmyk_hash) = cache.default_cmyk_hash() {
                    let avail_pixels = data.len() / 4;
                    let icc_pixels = avail_pixels.min(npixels);
                    if icc_pixels > 0 {
                        if let Some(rgb) =
                            cache.convert_image_8bit(cmyk_hash, data, icc_pixels)
                        {
                            let mut rgba = vec![255u8; npixels * 4];
                            for i in 0..icc_pixels {
                                rgba[i * 4] = rgb[i * 3];
                                rgba[i * 4 + 1] = rgb[i * 3 + 1];
                                rgba[i * 4 + 2] = rgb[i * 3 + 2];
                            }
                            // Remaining pixels (if data was short) stay white (0xFF)
                            return rgba;
                        }
                    }
                }
            }
            // Fallback: PLRM CMYK→RGB formula
            let mut rgba = vec![255u8; npixels * 4];
            for i in 0..npixels {
                let si = i * 4;
                let c = data.get(si).copied().unwrap_or(0) as f64 / 255.0;
                let m = data.get(si + 1).copied().unwrap_or(0) as f64 / 255.0;
                let y = data.get(si + 2).copied().unwrap_or(0) as f64 / 255.0;
                let k = data.get(si + 3).copied().unwrap_or(0) as f64 / 255.0;
                let r = (1.0 - c.min(1.0)) * (1.0 - k.min(1.0));
                let g = (1.0 - m.min(1.0)) * (1.0 - k.min(1.0));
                let b = (1.0 - y.min(1.0)) * (1.0 - k.min(1.0));
                let pi = i * 4;
                rgba[pi] = (r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            rgba
        }
        ImageColorSpace::ICCBased {
            n,
            profile_hash,
            profile_data,
        } => {
            // Try ICC-based conversion if cache is available
            if let Some(cache) = icc {
                if cache.has_profile(profile_hash) {
                    if let Some(rgb) = cache.convert_image_8bit(profile_hash, data, npixels) {
                        let mut rgba = vec![255u8; npixels * 4];
                        for i in 0..npixels {
                            rgba[i * 4] = rgb[i * 3];
                            rgba[i * 4 + 1] = rgb[i * 3 + 1];
                            rgba[i * 4 + 2] = rgb[i * 3 + 2];
                        }
                        return rgba;
                    }
                }
            }
            // Fallback to device equivalent based on component count
            let _ = (profile_hash, profile_data);
            let fallback = match n {
                1 => ImageColorSpace::DeviceGray,
                4 => ImageColorSpace::DeviceCMYK,
                _ => ImageColorSpace::DeviceRGB,
            };
            let p = ImageParams {
                color_space: fallback,
                ..params.clone()
            };
            samples_to_rgba(data, &p, icc)
        }
        ImageColorSpace::Indexed {
            base,
            hival,
            lookup,
        } => {
            let base_ncomp = base.num_components() as usize;
            // Expand indexed samples to base color space, then convert
            let mut expanded = Vec::with_capacity(npixels * base_ncomp);
            for i in 0..npixels {
                let idx = data.get(i).copied().unwrap_or(0) as usize;
                let idx = idx.min(*hival as usize);
                let offset = idx * base_ncomp;
                for c in 0..base_ncomp {
                    expanded.push(lookup.get(offset + c).copied().unwrap_or(0));
                }
            }
            let p = ImageParams {
                color_space: *base.clone(),
                ..params.clone()
            };
            samples_to_rgba(&expanded, &p, icc)
        }
        ImageColorSpace::CIEBasedABC { params: cie_params } => {
            let mut rgba = vec![255u8; npixels * 4];
            for i in 0..npixels {
                let si = i * 3;
                let a = data.get(si).copied().unwrap_or(0) as f64 / 255.0;
                let b = data.get(si + 1).copied().unwrap_or(0) as f64 / 255.0;
                let c = data.get(si + 2).copied().unwrap_or(0) as f64 / 255.0;
                let color = DeviceColor::from_cie_abc(a, b, c, cie_params);
                let pi = i * 4;
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            rgba
        }
        ImageColorSpace::CIEBasedA { params: cie_params } => {
            let mut rgba = vec![255u8; npixels * 4];
            for i in 0..npixels {
                let val = data.get(i).copied().unwrap_or(0) as f64 / 255.0;
                let color = DeviceColor::from_cie_a(val, cie_params);
                let pi = i * 4;
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            rgba
        }
        ImageColorSpace::Separation {
            alt_space,
            tint_table,
            ..
        } => {
            // 1 byte per pixel → lookup in tint table → convert alt space to RGB
            let mut rgba = vec![255u8; npixels * 4];
            let no = tint_table.num_outputs as usize;
            let mut alt_comps = vec![0.0f32; no];
            for i in 0..npixels {
                let tint = data.get(i).copied().unwrap_or(0) as f32 / 255.0;
                tint_table.lookup_1d(tint, &mut alt_comps);
                let (r, g, b) = alt_comps_to_rgb(&alt_comps, alt_space);
                let pi = i * 4;
                rgba[pi] = r;
                rgba[pi + 1] = g;
                rgba[pi + 2] = b;
            }
            rgba
        }
        ImageColorSpace::DeviceN {
            alt_space,
            tint_table,
            ..
        } => {
            let ni = tint_table.num_inputs as usize;
            let no = tint_table.num_outputs as usize;
            let mut rgba = vec![255u8; npixels * 4];
            let mut inputs = vec![0.0f32; ni];
            let mut alt_comps = vec![0.0f32; no];
            for i in 0..npixels {
                let si = i * ni;
                for (c, inp) in inputs.iter_mut().enumerate() {
                    *inp = data.get(si + c).copied().unwrap_or(0) as f32 / 255.0;
                }
                tint_table.lookup_nd(&inputs, &mut alt_comps);
                let (r, g, b) = alt_comps_to_rgb(&alt_comps, alt_space);
                let pi = i * 4;
                rgba[pi] = r;
                rgba[pi + 1] = g;
                rgba[pi + 2] = b;
            }
            rgba
        }
        ImageColorSpace::Mask { color, polarity } => {
            let mut rgba = vec![0u8; npixels * 4];
            let r = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
            let g = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
            let b = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            let bytes_per_row = (w).div_ceil(8);
            for row in 0..h {
                for col in 0..w {
                    let byte_idx = row * bytes_per_row + col / 8;
                    let bit_offset = 7 - (col % 8);
                    let bit = if byte_idx < data.len() {
                        (data[byte_idx] >> bit_offset) & 1
                    } else {
                        0
                    };
                    let paint = if *polarity { bit == 1 } else { bit == 0 };
                    if paint {
                        let pi = (row * w + col) * 4;
                        rgba[pi] = r;
                        rgba[pi + 1] = g;
                        rgba[pi + 2] = b;
                        rgba[pi + 3] = 255;
                    }
                }
            }
            rgba
        }
    }
}

/// Convert alt-space f32 component values to RGB bytes.
fn alt_comps_to_rgb(comps: &[f32], alt_space: &ImageColorSpace) -> (u8, u8, u8) {
    match alt_space {
        ImageColorSpace::DeviceGray => {
            let g = (comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            (g, g, g)
        }
        ImageColorSpace::DeviceRGB => {
            let r = (comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let g = (comps.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let b = (comps.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            (r, g, b)
        }
        ImageColorSpace::DeviceCMYK => {
            let c = comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let m = comps.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let y = comps.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let k = comps.get(3).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let r = ((1.0 - (c + k).min(1.0)) * 255.0).round() as u8;
            let g = ((1.0 - (m + k).min(1.0)) * 255.0).round() as u8;
            let b = ((1.0 - (y + k).min(1.0)) * 255.0).round() as u8;
            (r, g, b)
        }
        _ => (0, 0, 0),
    }
}

/// Apply ImageType 4 mask color transparency to RGBA data.
fn apply_mask_color_rgba(rgba: &mut [u8], sample_data: &[u8], params: &ImageParams) {
    let mask_color = match &params.mask_color {
        Some(mc) => mc,
        None => return,
    };
    let ncomp = params.color_space.num_components() as usize;
    let npixels = params.width as usize * params.height as usize;
    let is_range = mask_color.len() == 2 * ncomp;

    for i in 0..npixels {
        let si = i * ncomp;
        let matched = if is_range {
            (0..ncomp).all(|c| {
                let sample = sample_data.get(si + c).copied().unwrap_or(0);
                let min_val = mask_color.get(c * 2).copied().unwrap_or(0);
                let max_val = mask_color.get(c * 2 + 1).copied().unwrap_or(0);
                sample >= min_val && sample <= max_val
            })
        } else {
            (0..ncomp).all(|c| {
                let sample = sample_data.get(si + c).copied().unwrap_or(0);
                let target = mask_color.get(c).copied().unwrap_or(0);
                sample == target
            })
        };
        if matched {
            let pi = i * 4;
            if pi + 3 < rgba.len() {
                rgba[pi] = 0;
                rgba[pi + 1] = 0;
                rgba[pi + 2] = 0;
                rgba[pi + 3] = 0;
            }
        }
    }
}

/// For rotated/sheared transforms: integer box-filter pre-downsample, leaving
/// the fractional remainder to tiny-skia's bilinear.
///
/// Returns `None` if no pre-scaling is needed.
fn prescale_image(
    rgba_data: &[u8],
    w: u32,
    h: u32,
    transform: Transform,
) -> Option<(Vec<u8>, u32, u32, Transform)> {
    // Compute effective scale factors from the 2×2 part of the transform.
    let scale_x = (transform.sx * transform.sx + transform.ky * transform.ky).sqrt();
    let scale_y = (transform.kx * transform.kx + transform.sy * transform.sy).sqrt();
    let min_scale = scale_x.min(scale_y);

    // Only pre-scale if downscaling by more than 2.5× (e.g., 300 DPI bitmap
    // fonts at screen resolution). Lower ratios stay crisp with bilinear alone.
    if min_scale >= 0.4 {
        return None;
    }

    // Axis-aligned: use exact Lanczos3 resample to target dimensions.
    let is_axis_aligned = transform.kx.abs() < 1e-4 && transform.ky.abs() < 1e-4;
    if is_axis_aligned && w >= 2 && h >= 2 {
        let dw = (w as f32 * transform.sx.abs()).ceil().max(1.0) as u32;
        let dh = (h as f32 * transform.sy.abs()).ceil().max(1.0) as u32;
        if dw < w || dh < h {
            let resampled = lanczos3_resample(rgba_data, w, h, dw, dh);
            // Adjust transform so scale ≈ ±1 (sign preserved), same translation.
            let new_sx = transform.sx * w as f32 / dw as f32;
            let new_sy = transform.sy * h as f32 / dh as f32;
            let adjusted = Transform::from_row(
                new_sx,
                transform.ky,
                transform.kx,
                new_sy,
                transform.tx,
                transform.ty,
            );
            return Some((resampled, dw, dh, adjusted));
        }
    }

    // Fallback for rotated/sheared: integer box filter.
    let factor = (1.0 / min_scale) as u32;
    if factor < 2 || w < factor || h < factor {
        return None;
    }
    let nw = w / factor;
    let nh = h / factor;
    if nw == 0 || nh == 0 {
        return None;
    }
    let area = (factor * factor) as u32;
    let half = area / 2;
    let stride = w as usize * 4;
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for dy in 0..nh {
        for dx in 0..nw {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            let sy0 = (dy * factor) as usize;
            let sx0 = (dx * factor) as usize;
            for iy in 0..factor as usize {
                let row = (sy0 + iy) * stride + sx0 * 4;
                for ix in 0..factor as usize {
                    let i = row + ix * 4;
                    r += rgba_data[i] as u32;
                    g += rgba_data[i + 1] as u32;
                    b += rgba_data[i + 2] as u32;
                    a += rgba_data[i + 3] as u32;
                }
            }
            let di = (dy * nw + dx) as usize * 4;
            out[di] = ((r + half) / area) as u8;
            out[di + 1] = ((g + half) / area) as u8;
            out[di + 2] = ((b + half) / area) as u8;
            out[di + 3] = ((a + half) / area) as u8;
        }
    }
    let f = factor as f32;
    let adjusted = Transform::from_row(
        transform.sx * f,
        transform.ky * f,
        transform.kx * f,
        transform.sy * f,
        transform.tx,
        transform.ty,
    );
    Some((out, nw, nh, adjusted))
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

/// Compute minimum line width for hairline strokes at a given DPI and CTM.
/// Returns the minimum width in user-space units that ensures at least
/// 0.5 device pixels at ≤150 DPI or 1.0 device pixel above 150 DPI.
fn hairline_min_width(ctm: &Matrix, dpi: f64) -> f64 {
    let (a, b, c, d) = (ctm.a, ctm.b, ctm.c, ctm.d);
    let sum_sq = a * a + b * b + c * c + d * d;
    let diff = ((a * a + b * b - c * c - d * d).powi(2) + 4.0 * (a * c + b * d).powi(2)).sqrt();
    let s_max = (0.5 * (sum_sq + diff)).max(0.0).sqrt();
    let min_px = if dpi <= 150.0 { 0.5 } else { 1.0 };
    if s_max > 1e-10 {
        min_px / s_max
    } else {
        min_px
    }
}

/// Build a stroke with minimum line-width enforcement (shared by trait impl and band rendering).
/// `dpi` is the device resolution, used to select the hairline minimum width:
/// at ≤150 DPI use 0.6 device pixels; above 150 DPI use 1.0 device pixel.
fn build_stroke(params: &StrokeParams, dpi: f64) -> Stroke {
    let min_lw = hairline_min_width(&params.ctm, dpi);
    let mut stroke = Stroke {
        width: (params.line_width as f32).max(min_lw as f32),
        line_cap: to_line_cap(params.line_cap),
        line_join: to_line_join(params.line_join),
        miter_limit: params.miter_limit as f32,
        ..Stroke::default()
    };
    if !params.dash_pattern.array.is_empty() {
        let mut dash_array: Vec<f32> = params
            .dash_pattern
            .array
            .iter()
            .map(|&v| v as f32)
            .collect();
        // PostScript allows odd-length dash arrays (implicitly doubled),
        // but tiny-skia requires even length. Double odd arrays to match PS semantics.
        if dash_array.len() % 2 == 1 {
            let clone = dash_array.clone();
            dash_array.extend_from_slice(&clone);
        }
        if let Some(dash) = StrokeDash::new(dash_array, params.dash_pattern.offset as f32) {
            stroke.dash = Some(dash);
        }
    }
    stroke
}

/// Apply stroke adjustment: snap axis-aligned path segments to device pixel
/// centers so thin strokes render with consistent weight.
///
/// For a stroke of width W in device pixels:
/// - Odd-integer width (1, 3, ...): snap to half-pixel (floor(x) + 0.5)
/// - Even-integer width or non-integer: snap to pixel edge (round(x))
/// - For hairlines (device width < 1.5): always snap to half-pixel
///
/// Only axis-aligned segments (horizontal/vertical lines) are snapped.
/// Diagonal/curved segments are left as-is since snapping would distort them.
fn stroke_adjust_path(path: &PsPath, device_width: f64) -> PsPath {
    // Determine snap mode: half-pixel for hairlines/odd widths, whole-pixel for even
    let use_half_pixel = device_width < 1.5 || (device_width.round() as i32) % 2 == 1;

    let snap = |v: f64| -> f64 {
        if use_half_pixel {
            v.floor() + 0.5
        } else {
            v.round()
        }
    };

    let mut result = PsPath::new();
    let mut prev_x = 0.0_f64;
    let mut prev_y = 0.0_f64;

    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => {
                prev_x = x;
                prev_y = y;
                result.segments.push(PathSegment::MoveTo(x, y));
            }
            PathSegment::LineTo(x, y) => {
                let is_horizontal = (y - prev_y).abs() < 1e-6;
                let is_vertical = (x - prev_x).abs() < 1e-6;

                if is_horizontal {
                    // Snap Y coordinate to pixel center, adjust previous MoveTo/LineTo too
                    let snapped_y = snap(y);
                    // Also fix the previous segment's Y if it was the start of this H-line
                    if let Some(last) = result.segments.last_mut() {
                        match last {
                            PathSegment::MoveTo(_, ly) | PathSegment::LineTo(_, ly) => {
                                *ly = snapped_y;
                            }
                            _ => {}
                        }
                    }
                    result.segments.push(PathSegment::LineTo(x, snapped_y));
                    prev_x = x;
                    prev_y = snapped_y;
                } else if is_vertical {
                    // Snap X coordinate to pixel center
                    let snapped_x = snap(x);
                    if let Some(last) = result.segments.last_mut() {
                        match last {
                            PathSegment::MoveTo(lx, _) | PathSegment::LineTo(lx, _) => {
                                *lx = snapped_x;
                            }
                            _ => {}
                        }
                    }
                    result.segments.push(PathSegment::LineTo(snapped_x, y));
                    prev_x = snapped_x;
                    prev_y = y;
                } else {
                    // Diagonal — leave as-is
                    result.segments.push(PathSegment::LineTo(x, y));
                    prev_x = x;
                    prev_y = y;
                }
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                // Curves: leave as-is
                result.segments.push(PathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                });
                prev_x = x3;
                prev_y = y3;
            }
            PathSegment::ClosePath => {
                result.segments.push(PathSegment::ClosePath);
            }
        }
    }
    result
}

/// Apply stroke adjustment for viewport rendering.
///
/// Path coordinates are in reference-DPI device space. The viewport transform
/// maps them to output pixels: out = (ref - vp_origin) * scale.
/// We snap in output pixel space then map back to reference space.
fn stroke_adjust_path_viewport(
    path: &PsPath,
    device_width: f64,
    scale_x: f64,
    scale_y: f64,
    vp_x: f64,
    vp_y: f64,
) -> PsPath {
    let use_half_pixel = device_width < 1.5 || (device_width.round() as i32) % 2 == 1;

    // Snap a reference-space coordinate to the output pixel grid, then map back
    let snap_x = |v: f64| -> f64 {
        let out = (v - vp_x) * scale_x;
        let snapped = if use_half_pixel {
            out.floor() + 0.5
        } else {
            out.round()
        };
        snapped / scale_x + vp_x
    };
    let snap_y = |v: f64| -> f64 {
        let out = (v - vp_y) * scale_y;
        let snapped = if use_half_pixel {
            out.floor() + 0.5
        } else {
            out.round()
        };
        snapped / scale_y + vp_y
    };

    let mut result = PsPath::new();
    let mut prev_x = 0.0_f64;
    let mut prev_y = 0.0_f64;

    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => {
                prev_x = x;
                prev_y = y;
                result.segments.push(PathSegment::MoveTo(x, y));
            }
            PathSegment::LineTo(x, y) => {
                let is_horizontal = (y - prev_y).abs() < 1e-6;
                let is_vertical = (x - prev_x).abs() < 1e-6;

                if is_horizontal {
                    let snapped_y = snap_y(y);
                    if let Some(last) = result.segments.last_mut() {
                        match last {
                            PathSegment::MoveTo(_, ly) | PathSegment::LineTo(_, ly) => {
                                *ly = snapped_y;
                            }
                            _ => {}
                        }
                    }
                    result.segments.push(PathSegment::LineTo(x, snapped_y));
                    prev_x = x;
                    prev_y = snapped_y;
                } else if is_vertical {
                    let snapped_x = snap_x(x);
                    if let Some(last) = result.segments.last_mut() {
                        match last {
                            PathSegment::MoveTo(lx, _) | PathSegment::LineTo(lx, _) => {
                                *lx = snapped_x;
                            }
                            _ => {}
                        }
                    }
                    result.segments.push(PathSegment::LineTo(snapped_x, y));
                    prev_x = snapped_x;
                    prev_y = y;
                } else {
                    result.segments.push(PathSegment::LineTo(x, y));
                    prev_x = x;
                    prev_y = y;
                }
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                result.segments.push(PathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                });
                prev_x = x3;
                prev_y = y3;
            }
            PathSegment::ClosePath => {
                result.segments.push(PathSegment::ClosePath);
            }
        }
    }
    result
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
    icc: Option<&IccCache>,
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
            let stroke = build_stroke(params, dpi);
            // Apply stroke adjustment for thin strokes when enabled
            let adjusted;
            let draw_path = if params.stroke_adjust && stroke.width <= 2.0 {
                adjusted = stroke_adjust_path(path, stroke.width as f64);
                &adjusted
            } else {
                path
            };
            let Some(skia_path) = build_skia_path(draw_path) else {
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
        DisplayElement::Image {
            sample_data,
            params,
        } => {
            let iw = params.width;
            let ih = params.height;
            if iw == 0 || ih == 0 {
                return;
            }
            let mut rgba_data = samples_to_rgba(sample_data, params, icc);
            if params.mask_color.is_some() {
                apply_mask_color_rgba(&mut rgba_data, sample_data, params);
            }
            let expected = (iw * ih * 4) as usize;
            if rgba_data.len() < expected {
                return;
            }
            let Some(image_inv) = params.image_matrix.invert() else {
                return;
            };
            let combined = params.ctm.concat(&image_inv);
            let raw_transform = offset_transform(to_transform(&combined), y_off);

            let prescaled = prescale_image(&rgba_data, iw, ih, raw_transform);
            let (img_data, img_w, img_h, transform) = match &prescaled {
                Some((data, w, h, t)) => (data.as_slice(), *w, *h, *t),
                None => (rgba_data.as_slice(), iw, ih, raw_transform),
            };

            let Some(img_pixmap) = tiny_skia::PixmapRef::from_bytes(img_data, img_w, img_h) else {
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
            let eff_sx = (transform.sx * transform.sx + transform.ky * transform.ky).sqrt();
            let eff_sy = (transform.kx * transform.kx + transform.sy * transform.sy).sqrt();
            let quality = if eff_sx >= 0.9 && eff_sy >= 0.9 {
                tiny_skia::FilterQuality::Nearest
            } else {
                tiny_skia::FilterQuality::Bilinear
            };
            let img_paint = tiny_skia::PixmapPaint {
                quality,
                ..tiny_skia::PixmapPaint::default()
            };
            pixmap.draw_pixmap(0, 0, img_pixmap, &img_paint, transform, mask_ref);
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
        DisplayElement::PatternFill { params } => {
            render_pattern_fill_to_band(pixmap, band_state, params, y_start, band_w, band_h, dpi);
        }
        DisplayElement::Text { .. } => {} // PDF-only, ignored by rasterizer
    }
}

/// Render a tiled pattern fill into a band.
fn render_pattern_fill_to_band(
    pixmap: &mut Pixmap,
    band_state: &mut BandState,
    params: &stet_core::device::PatternFillParams,
    y_start: u32,
    band_w: u32,
    band_h: u32,
    dpi: f64,
) {
    let mut temp_mask = None;
    let Some(mask_ref) = resolve_clip_mask(&band_state.clip_region, &mut temp_mask, band_w, band_h)
    else {
        return;
    };

    let pm = &params.pattern_matrix;
    let y_off = y_start as f64;

    // Tile step vectors in device space (handles rotation/shear)
    let (step_ux, step_uy) = pm.transform_delta(params.xstep, 0.0);
    let (step_vx, step_vy) = pm.transform_delta(0.0, params.ystep);

    // Check for degenerate steps
    let step_u_len = (step_ux * step_ux + step_uy * step_uy).sqrt();
    let step_v_len = (step_vx * step_vx + step_vy * step_vy).sqrt();
    if step_u_len < 0.01 || step_v_len < 0.01 {
        return;
    }

    // Pattern origin in device space
    let origin_x = pm.tx;
    let origin_y = pm.ty;

    // Compute the fill path bounding box to determine tile range
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for seg in &params.path.segments {
        let (x, y) = match seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => (*x, *y),
            PathSegment::CurveTo { x3, y3, .. } => (*x3, *y3),
            PathSegment::ClosePath => continue,
        };
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    // Clamp to band Y range
    let band_min_y = y_off;
    let band_max_y = y_off + band_h as f64;
    min_y = min_y.max(band_min_y);
    max_y = max_y.min(band_max_y);
    if min_y >= max_y {
        return;
    }

    // Compute tile index range by projecting fill bbox into tile coordinates.
    // For rotated/sheared patterns, we need to find the range of (tu, tv) indices
    // such that origin + tu*step_u + tv*step_v covers the fill bbox.
    // Project each corner of the fill bbox onto the step vectors.
    let det = step_ux * step_vy - step_uy * step_vx;
    if det.abs() < 1e-10 {
        return; // degenerate (parallel step vectors)
    }
    let inv_det = 1.0 / det;

    let mut tu_min = f64::MAX;
    let mut tu_max = f64::MIN;
    let mut tv_min = f64::MAX;
    let mut tv_max = f64::MIN;
    for &(cx, cy) in &[
        (min_x, min_y),
        (max_x, min_y),
        (min_x, max_y),
        (max_x, max_y),
    ] {
        let dx = cx - origin_x;
        let dy = cy - origin_y;
        let tu = (dx * step_vy - dy * step_vx) * inv_det;
        let tv = (-dx * step_uy + dy * step_ux) * inv_det;
        tu_min = tu_min.min(tu);
        tu_max = tu_max.max(tu);
        tv_min = tv_min.min(tv);
        tv_max = tv_max.max(tv);
    }

    let tile_x_start = tu_min.floor() as i32 - 1;
    let tile_x_end = tu_max.ceil() as i32 + 1;
    let tile_y_start = tv_min.floor() as i32 - 1;
    let tile_y_end = tv_max.ceil() as i32 + 1;

    // Safety limit on tile count
    let tile_count = (tile_x_end - tile_x_start) as i64 * (tile_y_end - tile_y_start) as i64;
    if tile_count > 10000 {
        return;
    }

    // Render tiled pattern into a temporary pixmap, then composite through
    // the fill path onto the main pixmap. This gives exact fractional tile
    // positioning with no rounding artifacts from Pattern shader repeat.
    let Some(mut tile_buf) = Pixmap::new(band_w, band_h) else {
        return;
    };

    for tv in tile_y_start..tile_y_end {
        for tu in tile_x_start..tile_x_end {
            // Quick Y cull using tile center
            let tile_dev_y = origin_y + tu as f64 * step_uy + tv as f64 * step_vy;
            let tile_extent = step_u_len + step_v_len; // conservative radius
            if tile_dev_y + tile_extent < band_min_y || tile_dev_y - tile_extent > band_max_y {
                continue;
            }

            // Tile transform: translate by (tu*xstep, tv*ystep) in pattern space,
            // then apply the full pattern matrix. This is equivalent to
            // base_transform pre-translated by the pattern-space offset.
            let pat_offset_x = tu as f64 * params.xstep;
            let pat_offset_y = tv as f64 * params.ystep;
            let tile_transform = Transform::from_row(
                pm.a as f32,
                pm.b as f32,
                pm.c as f32,
                pm.d as f32,
                (pm.a * pat_offset_x + pm.c * pat_offset_y + pm.tx) as f32,
                (pm.b * pat_offset_x + pm.d * pat_offset_y + pm.ty - y_off) as f32,
            );

            for elem in params.tile.elements() {
                match elem {
                    DisplayElement::Fill { path, params: fp } => {
                        if let Some(sp) = build_skia_path(path) {
                            let mut paint = if params.paint_type == 1 {
                                to_paint(&fp.color)
                            } else {
                                to_paint(
                                    params
                                        .underlying_color
                                        .as_ref()
                                        .unwrap_or(&DeviceColor::black()),
                                )
                            };
                            // Disable anti-aliasing for tile fills so adjacent
                            // tiles abut perfectly with no seam artifacts.
                            paint.anti_alias = false;
                            let t = to_transform(&fp.ctm);
                            let combined = tile_transform.post_concat(t);
                            let fr = to_fill_rule(&fp.fill_rule);
                            tile_buf.fill_path(&sp, &paint, fr, combined, None);
                        }
                    }
                    DisplayElement::Stroke { path, params: sp } => {
                        if let Some(skp) = build_skia_path(path) {
                            let stroke = build_stroke(sp, dpi);
                            let paint = to_paint(&sp.color);
                            let t = to_transform(&sp.ctm);
                            let combined = tile_transform.post_concat(t);
                            tile_buf.stroke_path(&skp, &paint, &stroke, combined, None);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Composite tile_buf onto main pixmap through the fill path.
    // Create a mask from the fill path, intersect with clip mask, then draw.
    let Some(fill_skia_path) = build_skia_path(&params.path) else {
        return;
    };
    let fill_rule = to_fill_rule(&params.fill_rule);
    let mut fill_mask = Mask::new(band_w, band_h).expect("mask");
    fill_mask.fill_path(
        &fill_skia_path,
        fill_rule,
        true, // anti-alias
        Transform::from_translate(0.0, -(y_off as f32)),
    );

    // Intersect fill mask with clip mask
    if let Some(clip_mask) = mask_ref {
        intersect_masks(&mut fill_mask, clip_mask);
    }

    let img_paint = tiny_skia::PixmapPaint::default();
    pixmap.draw_pixmap(
        0,
        0,
        tile_buf.as_ref(),
        &img_paint,
        Transform::identity(),
        Some(&fill_mask),
    );
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
        // Rasterize with Y-offset into band-sized mask.
        // Anti-alias disabled: AA clip edges cause visible seams between
        // adjacent clipped regions (e.g. AGM EPS tiled fills).
        let transform = offset_transform(to_transform(&params.ctm), y_start as f32);
        mask.data_mut().fill(0);
        mask.fill_path(&skia_path, fill_rule, false, transform);
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
        let stroke = build_stroke(params, self.dpi);
        let adjusted;
        let draw_path = if params.stroke_adjust && stroke.width <= 2.0 {
            adjusted = stroke_adjust_path(path, stroke.width as f64);
            &adjusted
        } else {
            path
        };
        let Some(skia_path) = build_skia_path(draw_path) else {
            return;
        };
        let paint = to_paint(&params.color);
        let transform = to_transform(&params.ctm);

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
            mask.fill_path(&skia_path, fill_rule, false, transform);
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

    fn draw_image(&mut self, sample_data: &[u8], params: &ImageParams) {
        self.ensure_full_pixmap();
        let w = params.width;
        let h = params.height;
        if w == 0 || h == 0 {
            return;
        }
        let mut rgba_data = samples_to_rgba(sample_data, params, self.render_icc_cache.as_ref());
        if params.mask_color.is_some() {
            apply_mask_color_rgba(&mut rgba_data, sample_data, params);
        }
        let expected = (w * h * 4) as usize;
        if rgba_data.len() < expected {
            return;
        }

        let Some(image_inv) = params.image_matrix.invert() else {
            return;
        };
        let combined = params.ctm.concat(&image_inv);
        let raw_transform = to_transform(&combined);

        let prescaled = prescale_image(&rgba_data, w, h, raw_transform);
        let (img_data, img_w, img_h, transform) = match &prescaled {
            Some((data, pw, ph, t)) => (data.as_slice(), *pw, *ph, *t),
            None => (rgba_data.as_slice(), w, h, raw_transform),
        };

        let Some(img_pixmap) = tiny_skia::PixmapRef::from_bytes(img_data, img_w, img_h) else {
            return;
        };

        let (pw, ph) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, pw, ph) else {
            return;
        };

        let eff_sx = (transform.sx * transform.sx + transform.ky * transform.ky).sqrt();
        let eff_sy = (transform.kx * transform.kx + transform.sy * transform.sy).sqrt();
        let quality = if eff_sx >= 0.9 && eff_sy >= 0.9 {
            tiny_skia::FilterQuality::Nearest
        } else {
            tiny_skia::FilterQuality::Bilinear
        };
        let paint = tiny_skia::PixmapPaint {
            quality,
            ..tiny_skia::PixmapPaint::default()
        };
        self.pixmap
            .draw_pixmap(0, 0, img_pixmap, &paint, transform, mask_ref);
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

    fn paint_pattern_fill(&mut self, params: &stet_core::device::PatternFillParams) {
        self.ensure_full_pixmap();
        let w = self.pixmap.width();
        let h = self.pixmap.height();
        let mut band_state = BandState {
            clip_region: self.clip_region.take(),
            spare_mask: self.spare_mask.take(),
            clip_mask_cache: HashMap::new(),
            clip_mask_seen: HashSet::new(),
            mask_pool: Vec::new(),
        };
        render_pattern_fill_to_band(&mut self.pixmap, &mut band_state, params, 0, w, h, self.dpi);
        self.clip_region = band_state.clip_region.take();
        if let Some(mask) = band_state.spare_mask.take() {
            self.spare_mask = Some(mask);
        }
    }

    fn page_size(&self) -> (u32, u32) {
        (self.page_w, self.page_h)
    }

    fn replay_and_show(&mut self, list: DisplayList, output_path: &str) -> Result<(), String> {
        // Wait for any previous background render to complete
        self.join_pending()?;

        let (page_w, page_h) = self.page_size();
        let band_h = select_band_height(page_w, page_h);

        // Build ICC cache for this page's display list
        let icc_cache = build_icc_cache_for_list(&list, self.system_cmyk_bytes.as_ref());

        // If banding not worthwhile, fall back to normal replay + show_page.
        // Lazily allocate the full-page pixmap since it's needed for this path.
        if band_h >= page_h {
            self.ensure_full_pixmap();
            self.render_icc_cache = Some(icc_cache);
            stet_core::display_list::replay_to_device(&list, self);
            self.render_icc_cache = None;
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

        #[cfg(feature = "parallel")]
        {
            // Spawn banded rendering on rayon's thread pool, overlapping with
            // interpretation of the next page. Using rayon::spawn avoids OS thread
            // creation overhead and keeps work on the warmed-up pool.
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            rayon::spawn(move || {
                let result = render_banded_to_sink(
                    page_w, page_h, band_h, dpi, &list, &mut *sink, &icc_cache,
                );
                let _ = tx.send(result);
            });
            self.pending_render = Some(rx);
        }
        #[cfg(not(feature = "parallel"))]
        {
            render_banded_to_sink(page_w, page_h, band_h, dpi, &list, &mut *sink, &icc_cache)?;
        }

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
    icc_cache: &IccCache,
) -> Result<(), String> {
    // Precompute Y bounding boxes for culling
    let bboxes = precompute_bboxes(list, dpi);

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
    let icc_ref = Some(icc_cache);

    // Closure that renders a single band and returns its RGBA pixels.
    let render_band = |band_idx: u32| -> Vec<u8> {
        let y_start = band_idx * band_h;
        let actual_h = (page_h - y_start).min(band_h);

        let render_y_start = y_start.saturating_sub(BAND_OVERLAP);
        let render_y_end_f = ((y_start + actual_h + BAND_OVERLAP).min(page_h)) as f64;
        let band_offset = y_start - render_y_start;

        let mut band_pixmap = Pixmap::new(page_w, render_h).expect("Failed to create band pixmap");
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
                        if pb.y_max <= render_y_start as f64 || pb.y_min >= render_y_end_f =>
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
                    icc_ref,
                );
            }
        }

        // Extract only the actual band rows (skip overlap)
        let start_byte = band_offset as usize * row_bytes;
        let total_bytes = actual_h as usize * row_bytes;
        band_pixmap.data()[start_byte..start_byte + total_bytes].to_vec()
    };

    // Render bands in parallel (when available), write to sink in order.
    #[cfg(feature = "parallel")]
    {
        // Process in chunks of `chunk_size` bands to limit peak memory
        // (each rendered band is ~band_h * page_w * 4 bytes).
        // Cap at 8 threads — sequential sink writing bottleneck means
        // additional cores yield no speedup (benchmarked: 8→7.8s plateau).
        let chunk_size = rayon::current_num_threads().clamp(1, 8);

        for chunk_start in (0..num_bands).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size as u32).min(num_bands);

            let rendered: Vec<Vec<u8>> = (chunk_start..chunk_end)
                .into_par_iter()
                .map(&render_band)
                .collect();

            for (i, band_data) in rendered.iter().enumerate() {
                let band_idx = chunk_start + i as u32;
                let y_start = band_idx * band_h;
                let actual_h = (page_h - y_start).min(band_h);
                sink.write_rows(band_data, actual_h)?;
            }
        }
    }
    #[cfg(not(feature = "parallel"))]
    {
        // Sequential single-threaded rendering
        for band_idx in 0..num_bands {
            let band_data = render_band(band_idx);
            let y_start = band_idx * band_h;
            let actual_h = (page_h - y_start).min(band_h);
            sink.write_rows(&band_data, actual_h)?;
        }
    }

    sink.end_page()
}

/// 2D bounding box in device pixels.
struct BBox2D {
    x_min: f64,
    y_min: f64,
    x_max: f64,
    y_max: f64,
}

/// Compute full 2D bounding boxes for display list elements (for viewport culling).
fn precompute_full_bboxes(list: &DisplayList, dpi: f64) -> Vec<Option<BBox2D>> {
    list.elements()
        .iter()
        .map(|elem| match elem {
            DisplayElement::Fill { path, .. } => path_full_bbox(path),
            DisplayElement::Stroke { path, params } => {
                path_full_bbox(path).map(|mut bbox| {
                    // Use effective line width: actual width or hairline minimum
                    let effective_lw = params.line_width.max(hairline_min_width(&params.ctm, dpi));
                    let expand = effective_lw * params.miter_limit * 0.5;
                    let m = &params.ctm;
                    let is_identity = m.a == 1.0
                        && m.b == 0.0
                        && m.c == 0.0
                        && m.d == 1.0
                        && m.tx == 0.0
                        && m.ty == 0.0;
                    if is_identity {
                        bbox.x_min -= expand;
                        bbox.x_max += expand;
                        bbox.y_min -= expand;
                        bbox.y_max += expand;
                    } else {
                        // Path is in user space — expand for stroke, then
                        // transform bbox corners through CTM to device space.
                        let col_x_len = (m.a * m.a + m.b * m.b).sqrt().max(1.0);
                        let col_y_len = (m.c * m.c + m.d * m.d).sqrt().max(1.0);
                        let expand_x = effective_lw * col_x_len * params.miter_limit * 0.5;
                        let expand_y = effective_lw * col_y_len * params.miter_limit * 0.5;
                        bbox.x_min -= expand_x;
                        bbox.x_max += expand_x;
                        bbox.y_min -= expand_y;
                        bbox.y_max += expand_y;
                        // Transform all 4 corners to device space
                        let corners = [
                            (
                                m.a * bbox.x_min + m.c * bbox.y_min + m.tx,
                                m.b * bbox.x_min + m.d * bbox.y_min + m.ty,
                            ),
                            (
                                m.a * bbox.x_max + m.c * bbox.y_min + m.tx,
                                m.b * bbox.x_max + m.d * bbox.y_min + m.ty,
                            ),
                            (
                                m.a * bbox.x_min + m.c * bbox.y_max + m.tx,
                                m.b * bbox.x_min + m.d * bbox.y_max + m.ty,
                            ),
                            (
                                m.a * bbox.x_max + m.c * bbox.y_max + m.tx,
                                m.b * bbox.x_max + m.d * bbox.y_max + m.ty,
                            ),
                        ];
                        bbox.x_min = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
                        bbox.x_max = corners
                            .iter()
                            .map(|c| c.0)
                            .fold(f64::NEG_INFINITY, f64::max);
                        bbox.y_min = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
                        bbox.y_max = corners
                            .iter()
                            .map(|c| c.1)
                            .fold(f64::NEG_INFINITY, f64::max);
                    }
                    bbox
                })
            }
            DisplayElement::Image { params, .. } => image_full_bbox(params),
            DisplayElement::AxialShading { params } => shading_full_bbox(&params.bbox, &params.ctm),
            DisplayElement::RadialShading { params } => {
                shading_full_bbox(&params.bbox, &params.ctm)
            }
            DisplayElement::MeshShading { params } => shading_full_bbox(&params.bbox, &params.ctm),
            DisplayElement::PatchShading { params } => shading_full_bbox(&params.bbox, &params.ctm),
            DisplayElement::PatternFill { params } => path_full_bbox(&params.path),
            _ => None, // Clip, InitClip, ErasePage: always process
        })
        .collect()
}

/// Compute full 2D bounds from path segments.
fn path_full_bbox(path: &PsPath) -> Option<BBox2D> {
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
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
    if x_min <= x_max {
        Some(BBox2D {
            x_min,
            y_min,
            x_max,
            y_max,
        })
    } else {
        None
    }
}

/// Compute full 2D bounds for an image from its transform.
fn image_full_bbox(params: &ImageParams) -> Option<BBox2D> {
    let m = &params.ctm;
    let im = &params.image_matrix;
    let Some(im_inv) = im.invert() else {
        return None;
    };
    let combined = m.concat(&im_inv);
    // Image occupies [0, width] × [0, height] in image space
    let w = params.width as f64;
    let h = params.height as f64;
    let corners = [
        combined.transform_point(0.0, 0.0),
        combined.transform_point(w, 0.0),
        combined.transform_point(0.0, h),
        combined.transform_point(w, h),
    ];
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for (x, y) in &corners {
        x_min = x_min.min(*x);
        x_max = x_max.max(*x);
        y_min = y_min.min(*y);
        y_max = y_max.max(*y);
    }
    Some(BBox2D {
        x_min,
        y_min,
        x_max,
        y_max,
    })
}

/// Compute full 2D bounds for a shading element from its BBox.
fn shading_full_bbox(bbox: &Option<[f64; 4]>, ctm: &Matrix) -> Option<BBox2D> {
    if let Some(bbox) = bbox {
        let corners = [
            ctm.transform_point(bbox[0], bbox[1]),
            ctm.transform_point(bbox[2], bbox[1]),
            ctm.transform_point(bbox[0], bbox[3]),
            ctm.transform_point(bbox[2], bbox[3]),
        ];
        let mut x_min = f64::INFINITY;
        let mut x_max = f64::NEG_INFINITY;
        let mut y_min = f64::INFINITY;
        let mut y_max = f64::NEG_INFINITY;
        for (x, y) in &corners {
            x_min = x_min.min(*x);
            x_max = x_max.max(*x);
            y_min = y_min.min(*y);
            y_max = y_max.max(*y);
        }
        Some(BBox2D {
            x_min,
            y_min,
            x_max,
            y_max,
        })
    } else {
        Some(BBox2D {
            x_min: 0.0,
            y_min: 0.0,
            x_max: 1e9,
            y_max: 1e9,
        })
    }
}

/// Build 2D clip epochs for viewport culling.
fn build_viewport_epochs(list: &DisplayList, bboxes: &[Option<BBox2D>]) -> Vec<ViewportEpoch> {
    let elements = list.elements();
    let mut epochs = Vec::new();
    let mut epoch_start = 0;
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    let mut has_erase = false;

    for (i, element) in elements.iter().enumerate() {
        if matches!(element, DisplayElement::InitClip) && i > epoch_start {
            epochs.push(ViewportEpoch {
                start_idx: epoch_start,
                end_idx: i,
                paint_bbox: if x_min <= x_max {
                    Some(BBox2D {
                        x_min,
                        y_min,
                        x_max,
                        y_max,
                    })
                } else {
                    None
                },
                has_erase_page: has_erase,
            });
            epoch_start = i;
            x_min = f64::INFINITY;
            x_max = f64::NEG_INFINITY;
            y_min = f64::INFINITY;
            y_max = f64::NEG_INFINITY;
            has_erase = false;
        }
        if matches!(element, DisplayElement::ErasePage) {
            has_erase = true;
        }
        if let Some(ref bbox) = bboxes[i] {
            x_min = x_min.min(bbox.x_min);
            x_max = x_max.max(bbox.x_max);
            y_min = y_min.min(bbox.y_min);
            y_max = y_max.max(bbox.y_max);
        }
    }
    if epoch_start < elements.len() {
        epochs.push(ViewportEpoch {
            start_idx: epoch_start,
            end_idx: elements.len(),
            paint_bbox: if x_min <= x_max {
                Some(BBox2D {
                    x_min,
                    y_min,
                    x_max,
                    y_max,
                })
            } else {
                None
            },
            has_erase_page: has_erase,
        });
    }
    epochs
}

/// Clip epoch with full 2D bounding box for viewport culling.
struct ViewportEpoch {
    start_idx: usize,
    end_idx: usize,
    paint_bbox: Option<BBox2D>,
    has_erase_page: bool,
}

/// Pre-computed metadata for fast viewport rendering.
///
/// Compute once per display list via [`prepare_display_list()`],
/// reuse across all [`render_region_prepared()`] calls. This avoids
/// three expensive traversals (bboxes, epochs, clip_seen) on every pan.
pub struct PreparedDisplayList {
    bboxes: Vec<Option<BBox2D>>,
    epochs: Vec<ViewportEpoch>,
    clip_seen: HashSet<u64>,
}

/// Precompute display list metadata for fast viewport rendering.
///
/// Uses a conservative DPI (72.0) for hairline expansion in bounding boxes,
/// producing safe overestimates that work at any zoom level without recomputation.
pub fn prepare_display_list(list: &DisplayList) -> PreparedDisplayList {
    let bboxes = precompute_full_bboxes(list, 72.0);
    let epochs = build_viewport_epochs(list, &bboxes);
    let clip_seen = precompute_clip_seen(list);
    PreparedDisplayList {
        bboxes,
        epochs,
        clip_seen,
    }
}

/// Render a rectangular viewport region using precomputed metadata.
///
/// Like [`render_region()`] but skips the three precomputation passes,
/// using the [`PreparedDisplayList`] instead. Significantly faster for
/// repeated renders of the same display list (e.g., panning at a fixed zoom).
#[allow(clippy::too_many_arguments)]
pub fn render_region_prepared(
    list: &DisplayList,
    prepared: &PreparedDisplayList,
    vp_x: f64,
    vp_y: f64,
    vp_w: f64,
    vp_h: f64,
    pixel_w: u32,
    pixel_h: u32,
    dpi: f64,
    icc: Option<&IccCache>,
) -> Vec<u8> {
    if pixel_w == 0 || pixel_h == 0 || vp_w <= 0.0 || vp_h <= 0.0 {
        return vec![0xFF; pixel_w as usize * pixel_h as usize * 4];
    }

    let scale_x = pixel_w as f64 / vp_w;
    let scale_y = pixel_h as f64 / vp_h;
    let effective_dpi = dpi * scale_x;

    let mut pixmap = Pixmap::new(pixel_w, pixel_h).expect("Failed to create viewport pixmap");
    pixmap.fill(Color::WHITE);

    let mut state = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: prepared.clip_seen.clone(),
        mask_pool: Vec::new(),
    };

    let elements = list.elements();
    let vp_x_f = vp_x as f32;
    let vp_y_f = vp_y as f32;
    let sx = scale_x as f32;
    let sy = scale_y as f32;
    let vp_x_max = vp_x + vp_w;
    let vp_y_max = vp_y + vp_h;

    for epoch in &prepared.epochs {
        if !epoch.has_erase_page {
            match epoch.paint_bbox {
                Some(ref pb)
                    if pb.x_max <= vp_x
                        || pb.x_min >= vp_x_max
                        || pb.y_max <= vp_y
                        || pb.y_min >= vp_y_max =>
                {
                    continue;
                }
                None => continue,
                _ => {}
            }
        }

        for i in epoch.start_idx..epoch.end_idx {
            if let Some(ref bbox) = prepared.bboxes[i] {
                if bbox.x_max <= vp_x
                    || bbox.x_min >= vp_x_max
                    || bbox.y_max <= vp_y
                    || bbox.y_min >= vp_y_max
                {
                    continue;
                }
            }
            render_element_to_viewport(
                &mut pixmap,
                &mut state,
                &elements[i],
                vp_x_f,
                vp_y_f,
                sx,
                sy,
                pixel_w,
                pixel_h,
                effective_dpi,
                icc,
            );
        }
    }

    pixmap.data().to_vec()
}

/// Render a rectangular viewport region of a display list to RGBA pixels.
///
/// - `list`: The display list to render (in device-space coordinates at the reference DPI)
/// - `vp_x, vp_y, vp_w, vp_h`: Viewport rectangle in device-space pixels
/// - `pixel_w, pixel_h`: Output pixel dimensions
/// - `dpi`: Reference DPI (for hairline width decisions)
///
/// Returns RGBA pixel data of size `pixel_w × pixel_h × 4`.
#[allow(clippy::too_many_arguments)]
pub fn render_region(
    list: &DisplayList,
    vp_x: f64,
    vp_y: f64,
    vp_w: f64,
    vp_h: f64,
    pixel_w: u32,
    pixel_h: u32,
    dpi: f64,
    icc: Option<&IccCache>,
) -> Vec<u8> {
    if pixel_w == 0 || pixel_h == 0 || vp_w <= 0.0 || vp_h <= 0.0 {
        return vec![0xFF; pixel_w as usize * pixel_h as usize * 4];
    }

    let scale_x = pixel_w as f64 / vp_w;
    let scale_y = pixel_h as f64 / vp_h;
    // Effective DPI for hairline decisions — reference DPI scaled by zoom
    let effective_dpi = dpi * scale_x;

    let bboxes = precompute_full_bboxes(list, effective_dpi);
    let epochs = build_viewport_epochs(list, &bboxes);
    let clip_seen = precompute_clip_seen(list);

    let mut pixmap = Pixmap::new(pixel_w, pixel_h).expect("Failed to create viewport pixmap");
    pixmap.fill(Color::WHITE);

    let mut state = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: clip_seen,
        mask_pool: Vec::new(),
    };

    let elements = list.elements();
    let vp_x_f = vp_x as f32;
    let vp_y_f = vp_y as f32;
    let sx = scale_x as f32;
    let sy = scale_y as f32;
    let vp_x_max = vp_x + vp_w;
    let vp_y_max = vp_y + vp_h;

    for epoch in &epochs {
        // Epoch-level culling
        if !epoch.has_erase_page {
            match epoch.paint_bbox {
                Some(ref pb)
                    if pb.x_max <= vp_x
                        || pb.x_min >= vp_x_max
                        || pb.y_max <= vp_y
                        || pb.y_min >= vp_y_max =>
                {
                    continue;
                }
                None => continue,
                _ => {}
            }
        }

        for i in epoch.start_idx..epoch.end_idx {
            // Element-level culling
            if let Some(ref bbox) = bboxes[i] {
                if bbox.x_max <= vp_x
                    || bbox.x_min >= vp_x_max
                    || bbox.y_max <= vp_y
                    || bbox.y_min >= vp_y_max
                {
                    continue;
                }
            }
            render_element_to_viewport(
                &mut pixmap,
                &mut state,
                &elements[i],
                vp_x_f,
                vp_y_f,
                sx,
                sy,
                pixel_w,
                pixel_h,
                effective_dpi,
                icc,
            );
        }
    }

    pixmap.data().to_vec()
}

/// Render a single display element into a viewport-local pixmap.
#[allow(clippy::too_many_arguments)]
fn render_element_to_viewport(
    pixmap: &mut Pixmap,
    state: &mut BandState,
    element: &DisplayElement,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    out_w: u32,
    out_h: u32,
    effective_dpi: f64,
    icc: Option<&IccCache>,
) {
    match element {
        DisplayElement::Fill { path, params } => {
            let Some(skia_path) = build_skia_path(path) else {
                return;
            };
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&state.clip_region, &mut temp_mask, out_w, out_h)
            else {
                return;
            };
            let paint = to_paint(&params.color);
            let transform =
                viewport_transform(to_transform(&params.ctm), vp_x, vp_y, scale_x, scale_y);
            let fill_rule = to_fill_rule(&params.fill_rule);
            pixmap.fill_path(&skia_path, &paint, fill_rule, transform, mask_ref);
        }
        DisplayElement::Stroke { path, params } => {
            let transform =
                viewport_transform(to_transform(&params.ctm), vp_x, vp_y, scale_x, scale_y);
            // For viewport rendering, build the stroke using the composited
            // transform (original CTM × viewport transform) so hairline width
            // calculations account for the actual output resolution.
            let vp_ctm = Matrix {
                a: transform.sx as f64,
                b: transform.ky as f64,
                c: transform.kx as f64,
                d: transform.sy as f64,
                tx: 0.0,
                ty: 0.0,
            };
            let vp_params = StrokeParams {
                ctm: vp_ctm,
                ..params.clone()
            };
            let stroke = build_stroke(&vp_params, effective_dpi);
            // Apply stroke adjustment — snap in output device space.
            // Path coords are in reference-DPI device space, so we scale
            // the snap grid to match the viewport transform.
            let adjusted;
            let draw_path = if params.stroke_adjust && stroke.width <= 2.0 {
                // Snap in output pixel space: transform coords, snap, inverse-transform.
                // For identity CTM (isotropic strokes in device space), we can snap
                // directly using the viewport scale factors.
                adjusted = stroke_adjust_path_viewport(
                    path,
                    stroke.width as f64,
                    scale_x as f64,
                    scale_y as f64,
                    vp_x as f64,
                    vp_y as f64,
                );
                &adjusted
            } else {
                path
            };
            let Some(skia_path) = build_skia_path(draw_path) else {
                return;
            };
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&state.clip_region, &mut temp_mask, out_w, out_h)
            else {
                return;
            };
            let paint = to_paint(&params.color);
            pixmap.stroke_path(&skia_path, &paint, &stroke, transform, mask_ref);
        }
        DisplayElement::Clip { path, params } => {
            clip_path_viewport(
                state, path, params, vp_x, vp_y, scale_x, scale_y, out_w, out_h,
            );
        }
        DisplayElement::InitClip => {
            if let Some(ClipRegion::Mask(mask)) = state.clip_region.take() {
                state.recycle_mask(mask);
            }
            state.clip_region = None;
        }
        DisplayElement::ErasePage => {
            pixmap.fill(Color::WHITE);
            if let Some(ClipRegion::Mask(mask)) = state.clip_region.take() {
                state.recycle_mask(mask);
            }
            state.clip_region = None;
        }
        DisplayElement::Image {
            sample_data,
            params,
        } => {
            let iw = params.width;
            let ih = params.height;
            if iw == 0 || ih == 0 {
                return;
            }
            let mut rgba_data = samples_to_rgba(sample_data, params, icc);
            if params.mask_color.is_some() {
                apply_mask_color_rgba(&mut rgba_data, sample_data, params);
            }
            let expected = (iw * ih * 4) as usize;
            if rgba_data.len() < expected {
                return;
            }
            let Some(image_inv) = params.image_matrix.invert() else {
                return;
            };
            let combined = params.ctm.concat(&image_inv);
            let raw_transform =
                viewport_transform(to_transform(&combined), vp_x, vp_y, scale_x, scale_y);

            let prescaled = prescale_image(&rgba_data, iw, ih, raw_transform);
            let (img_data, img_w, img_h, transform) = match &prescaled {
                Some((data, w, h, t)) => (data.as_slice(), *w, *h, *t),
                None => (rgba_data.as_slice(), iw, ih, raw_transform),
            };

            let Some(img_pixmap) = tiny_skia::PixmapRef::from_bytes(img_data, img_w, img_h) else {
                return;
            };
            #[allow(unused_assignments)]
            let mut temp_mask = None;
            let mask_ref = match &state.clip_region {
                None => None,
                Some(ClipRegion::Mask(m)) => Some(m as &Mask),
                Some(ClipRegion::Rect(rect)) => {
                    if rect.is_empty() {
                        return;
                    } else if rect.is_full_page(out_w, out_h) {
                        None
                    } else {
                        temp_mask = rect.make_mask(out_w, out_h);
                        temp_mask.as_ref()
                    }
                }
            };
            let eff_sx = (transform.sx * transform.sx + transform.ky * transform.ky).sqrt();
            let eff_sy = (transform.kx * transform.kx + transform.sy * transform.sy).sqrt();
            let quality = if eff_sx >= 0.9 && eff_sy >= 0.9 {
                tiny_skia::FilterQuality::Nearest
            } else {
                tiny_skia::FilterQuality::Bilinear
            };
            let img_paint = tiny_skia::PixmapPaint {
                quality,
                ..tiny_skia::PixmapPaint::default()
            };
            pixmap.draw_pixmap(0, 0, img_pixmap, &img_paint, transform, mask_ref);
        }
        DisplayElement::AxialShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&state.clip_region, &mut temp_mask, out_w, out_h)
            else {
                return;
            };
            render_axial_shading_viewport(pixmap, params, vp_x, vp_y, scale_x, scale_y, mask_ref);
        }
        DisplayElement::RadialShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&state.clip_region, &mut temp_mask, out_w, out_h)
            else {
                return;
            };
            render_radial_shading_viewport(pixmap, params, vp_x, vp_y, scale_x, scale_y, mask_ref);
        }
        DisplayElement::MeshShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&state.clip_region, &mut temp_mask, out_w, out_h)
            else {
                return;
            };
            render_mesh_shading_viewport(pixmap, params, vp_x, vp_y, scale_x, scale_y, mask_ref);
        }
        DisplayElement::PatchShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&state.clip_region, &mut temp_mask, out_w, out_h)
            else {
                return;
            };
            render_patch_shading_viewport(pixmap, params, vp_x, vp_y, scale_x, scale_y, mask_ref);
        }
        DisplayElement::PatternFill { params } => {
            render_pattern_fill_viewport(
                pixmap,
                state,
                params,
                vp_x,
                vp_y,
                scale_x,
                scale_y,
                out_w,
                out_h,
                effective_dpi,
            );
        }
        DisplayElement::Text { .. } => {} // PDF-only, ignored by rasterizer
    }
}

/// Pattern fill for viewport rendering. Applies viewport transform to both
/// the fill path and the pattern shader.
#[allow(clippy::too_many_arguments)]
fn render_pattern_fill_viewport(
    pixmap: &mut Pixmap,
    state: &mut BandState,
    params: &stet_core::device::PatternFillParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    out_w: u32,
    out_h: u32,
    effective_dpi: f64,
) {
    let mut temp_mask = None;
    let Some(mask_ref) = resolve_clip_mask(&state.clip_region, &mut temp_mask, out_w, out_h) else {
        return;
    };

    let pm = &params.pattern_matrix;

    // Tile step vectors in device space (handles rotation/shear)
    let (step_ux, step_uy) = pm.transform_delta(params.xstep, 0.0);
    let (step_vx, step_vy) = pm.transform_delta(0.0, params.ystep);

    // Check for degenerate steps
    let step_u_len = (step_ux * step_ux + step_uy * step_uy).sqrt();
    let step_v_len = (step_vx * step_vx + step_vy * step_vy).sqrt();
    if step_u_len < 0.01 || step_v_len < 0.01 {
        return;
    }

    // Pattern origin in device space
    let origin_x = pm.tx;
    let origin_y = pm.ty;

    // Viewport bounds in device space
    let dev_vp_x = vp_x as f64;
    let dev_vp_y = vp_y as f64;
    let dev_vp_w = out_w as f64 / scale_x as f64;
    let dev_vp_h = out_h as f64 / scale_y as f64;

    // Compute the fill path bounding box to determine tile range
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for seg in &params.path.segments {
        let (x, y) = match seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => (*x, *y),
            PathSegment::CurveTo { x3, y3, .. } => (*x3, *y3),
            PathSegment::ClosePath => continue,
        };
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    // Clamp to viewport bounds in device space
    min_x = min_x.max(dev_vp_x);
    min_y = min_y.max(dev_vp_y);
    max_x = max_x.min(dev_vp_x + dev_vp_w);
    max_y = max_y.min(dev_vp_y + dev_vp_h);
    if min_x >= max_x || min_y >= max_y {
        return;
    }

    // Project fill bbox corners onto tile coordinate system
    let det = step_ux * step_vy - step_uy * step_vx;
    if det.abs() < 1e-10 {
        return;
    }
    let inv_det = 1.0 / det;

    let mut tu_min = f64::MAX;
    let mut tu_max = f64::MIN;
    let mut tv_min = f64::MAX;
    let mut tv_max = f64::MIN;
    for &(cx, cy) in &[
        (min_x, min_y),
        (max_x, min_y),
        (min_x, max_y),
        (max_x, max_y),
    ] {
        let dx = cx - origin_x;
        let dy = cy - origin_y;
        let tu = (dx * step_vy - dy * step_vx) * inv_det;
        let tv = (-dx * step_uy + dy * step_ux) * inv_det;
        tu_min = tu_min.min(tu);
        tu_max = tu_max.max(tu);
        tv_min = tv_min.min(tv);
        tv_max = tv_max.max(tv);
    }

    let tile_x_start = tu_min.floor() as i32 - 1;
    let tile_x_end = tu_max.ceil() as i32 + 1;
    let tile_y_start = tv_min.floor() as i32 - 1;
    let tile_y_end = tv_max.ceil() as i32 + 1;

    // Safety limit on tile count
    let tile_count = (tile_x_end - tile_x_start) as i64 * (tile_y_end - tile_y_start) as i64;
    if tile_count > 10000 {
        return;
    }

    // Render tiled pattern into a temporary pixmap, then composite through
    // the fill path onto the main pixmap.
    let Some(mut tile_buf) = Pixmap::new(out_w, out_h) else {
        return;
    };

    let sx_f = scale_x as f64;
    let sy_f = scale_y as f64;

    for tv in tile_y_start..tile_y_end {
        for tu in tile_x_start..tile_x_end {
            // Tile offset in pattern space
            let pat_offset_x = tu as f64 * params.xstep;
            let pat_offset_y = tv as f64 * params.ystep;

            // Apply full pattern matrix then viewport transform
            let tile_transform = Transform::from_row(
                (pm.a * sx_f) as f32,
                (pm.b * sy_f) as f32,
                (pm.c * sx_f) as f32,
                (pm.d * sy_f) as f32,
                ((pm.a * pat_offset_x + pm.c * pat_offset_y + pm.tx - dev_vp_x) * sx_f) as f32,
                ((pm.b * pat_offset_x + pm.d * pat_offset_y + pm.ty - dev_vp_y) * sy_f) as f32,
            );

            for elem in params.tile.elements() {
                match elem {
                    DisplayElement::Fill { path, params: fp } => {
                        if let Some(sp) = build_skia_path(path) {
                            let mut paint = if params.paint_type == 1 {
                                to_paint(&fp.color)
                            } else {
                                to_paint(
                                    params
                                        .underlying_color
                                        .as_ref()
                                        .unwrap_or(&DeviceColor::black()),
                                )
                            };
                            // Disable anti-aliasing for tile fills so adjacent
                            // tiles abut perfectly with no seam artifacts.
                            paint.anti_alias = false;
                            let t = to_transform(&fp.ctm);
                            let combined = tile_transform.post_concat(t);
                            let fr = to_fill_rule(&fp.fill_rule);
                            tile_buf.fill_path(&sp, &paint, fr, combined, None);
                        }
                    }
                    DisplayElement::Stroke { path, params: sp } => {
                        if let Some(skp) = build_skia_path(path) {
                            let stroke = build_stroke(sp, effective_dpi);
                            let paint = to_paint(&sp.color);
                            let t = to_transform(&sp.ctm);
                            let combined = tile_transform.post_concat(t);
                            tile_buf.stroke_path(&skp, &paint, &stroke, combined, None);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Composite tile_buf onto main pixmap through the fill path.
    let Some(fill_skia_path) = build_skia_path(&params.path) else {
        return;
    };
    let fill_rule = to_fill_rule(&params.fill_rule);
    let mut fill_mask = Mask::new(out_w, out_h).expect("mask");
    let path_transform = viewport_transform(Transform::identity(), vp_x, vp_y, scale_x, scale_y);
    fill_mask.fill_path(&fill_skia_path, fill_rule, true, path_transform);

    // Intersect fill mask with clip mask
    if let Some(clip_mask) = mask_ref {
        intersect_masks(&mut fill_mask, clip_mask);
    }

    let img_paint = tiny_skia::PixmapPaint::default();
    pixmap.draw_pixmap(
        0,
        0,
        tile_buf.as_ref(),
        &img_paint,
        Transform::identity(),
        Some(&fill_mask),
    );
}

/// Clip path handling for viewport rendering.
#[allow(clippy::too_many_arguments)]
fn clip_path_viewport(
    state: &mut BandState,
    path: &PsPath,
    params: &ClipParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    out_w: u32,
    out_h: u32,
) {
    let fill_rule = to_fill_rule(&params.fill_rule);
    let path_hash = hash_clip_path(path, &params.fill_rule);
    let prev_region = state.clip_region.take();

    let mut mask = state.take_mask(out_w, out_h);

    let path_mask = if let Some(cached) = state.clip_mask_cache.get(&path_hash) {
        mask.data_mut().copy_from_slice(cached.data());
        mask
    } else {
        let Some(skia_path) = build_skia_path(path) else {
            state.recycle_mask(mask);
            state.clip_region = prev_region;
            return;
        };
        let transform = viewport_transform(to_transform(&params.ctm), vp_x, vp_y, scale_x, scale_y);
        mask.data_mut().fill(0);
        mask.fill_path(&skia_path, fill_rule, false, transform);
        if !state.clip_mask_seen.insert(path_hash) {
            state.clip_mask_cache.insert(path_hash, mask.clone());
        }
        mask
    };

    match prev_region {
        None => {
            state.clip_region = Some(ClipRegion::Mask(path_mask));
        }
        Some(ClipRegion::Rect(rect)) => {
            if rect.is_empty() {
                state.recycle_mask(path_mask);
            } else {
                let mut mask = path_mask;
                intersect_mask_with_rect(&mut mask, &rect, out_w, out_h);
                state.clip_region = Some(ClipRegion::Mask(mask));
            }
        }
        Some(ClipRegion::Mask(mut existing)) => {
            intersect_masks(&mut existing, &path_mask);
            state.recycle_mask(path_mask);
            state.clip_region = Some(ClipRegion::Mask(existing));
        }
    }
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
        // Transform all 4 BBox corners for correct bounds under rotation/shear
        let corners = [
            params.ctm.transform_point(bbox[0], bbox[1]),
            params.ctm.transform_point(bbox[2], bbox[1]),
            params.ctm.transform_point(bbox[0], bbox[3]),
            params.ctm.transform_point(bbox[2], bbox[3]),
        ];
        let x_min_f = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let y_min_f = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let x_max_f = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let y_max_f = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);
        (
            x_min_f.max(0.0),
            (y_min_f - y_start as f64).max(0.0),
            x_max_f.min(pw as f64),
            (y_max_f - y_start as f64).min(ph as f64),
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
///
/// Solves the two-circle radial gradient equation in **user space** for each
/// device pixel by inverse-transforming through the CTM. This correctly handles
/// arbitrary CTMs (rotation, shear, non-uniform scaling, X-flip) where circles
/// in user space become ellipses in device space.
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

    // Inverse CTM: device space → user space (where circles are circular)
    let Some(inv_ctm) = params.ctm.invert() else {
        return; // Degenerate CTM
    };

    // Compute pixel bounds (default: full pixmap, clipped by BBox if present)
    let (px_min, py_min, px_max, py_max) = if let Some(bbox) = &params.bbox {
        // Transform all 4 BBox corners to get correct device-space bounds
        // (2 corners is wrong when CTM has rotation/shear)
        let corners = [
            params.ctm.transform_point(bbox[0], bbox[1]),
            params.ctm.transform_point(bbox[2], bbox[1]),
            params.ctm.transform_point(bbox[0], bbox[3]),
            params.ctm.transform_point(bbox[2], bbox[3]),
        ];
        let x_min_f = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let y_min_f = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let x_max_f = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let y_max_f = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);
        let x_min = x_min_f.max(0.0) as u32;
        let y_min_dev = y_min_f.max(0.0);
        let x_max = (x_max_f.ceil() as u32).min(pw);
        let y_max_dev = y_max_f.ceil();
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

            // Inverse-transform device pixel to user space
            let (ux, uy) = inv_ctm.transform_point(dev_x, dev_y);

            // Solve for t in user space where circles are circular
            let t = solve_radial_t(
                ux, uy, params.x0, params.y0, params.r0, params.x1, params.y1, params.r1,
            );
            if let Some(t) = t {
                // Check extend
                if t < 0.0 && !params.extend_start {
                    continue;
                }
                if t > 1.0 && !params.extend_end {
                    continue;
                }
                let clamped = t.clamp(0.0, 1.0);
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
            color_space: params.color_space.clone(),
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
                    raw_components: vec![],
                },
                v1: ShadingVertex {
                    x: *x10,
                    y: *y10,
                    color: c10.clone(),
                    raw_components: vec![],
                },
                v2: ShadingVertex {
                    x: *x01,
                    y: *y01,
                    color: c01.clone(),
                    raw_components: vec![],
                },
            });
            triangles.push(stet_core::device::ShadingTriangle {
                v0: ShadingVertex {
                    x: *x10,
                    y: *y10,
                    color: c10.clone(),
                    raw_components: vec![],
                },
                v1: ShadingVertex {
                    x: *x11,
                    y: *y11,
                    color: c11.clone(),
                    raw_components: vec![],
                },
                v2: ShadingVertex {
                    x: *x01,
                    y: *y01,
                    color: c01.clone(),
                    raw_components: vec![],
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

// ---- Viewport shading renderers ----
// These map device-space coordinates into viewport-local pixel coordinates
// using the viewport transform: output = (device - vp) * scale

/// Render an axial shading into viewport-local coordinates.
fn render_axial_shading_viewport(
    pixmap: &mut Pixmap,
    params: &AxialShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    clip_mask: Option<&Mask>,
) {
    let pw = pixmap.width();
    let ph = pixmap.height();
    if params.color_stops.is_empty() || pw == 0 || ph == 0 {
        return;
    }

    let (dx0, dy0) = params.ctm.transform_point(params.x0, params.y0);
    let (dx1, dy1) = params.ctm.transform_point(params.x1, params.y1);

    let stops = build_gradient_stops(&params.color_stops);
    if stops.is_empty() {
        return;
    }

    let start =
        tiny_skia::Point::from_xy((dx0 as f32 - vp_x) * scale_x, (dy0 as f32 - vp_y) * scale_y);
    let end =
        tiny_skia::Point::from_xy((dx1 as f32 - vp_x) * scale_x, (dy1 as f32 - vp_y) * scale_y);

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

    // Build fill rect from BBox (transformed to viewport coords) or full pixmap
    let (rx_min, ry_min, rx_max, ry_max) = if let Some(bbox) = &params.bbox {
        let corners = [
            params.ctm.transform_point(bbox[0], bbox[1]),
            params.ctm.transform_point(bbox[2], bbox[1]),
            params.ctm.transform_point(bbox[0], bbox[3]),
            params.ctm.transform_point(bbox[2], bbox[3]),
        ];
        let x_min = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let y_min = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let x_max = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let y_max = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);
        (
            ((x_min as f32 - vp_x) * scale_x).max(0.0),
            ((y_min as f32 - vp_y) * scale_y).max(0.0),
            ((x_max as f32 - vp_x) * scale_x).min(pw as f32),
            ((y_max as f32 - vp_y) * scale_y).min(ph as f32),
        )
    } else {
        (0.0, 0.0, pw as f32, ph as f32)
    };

    if rx_max <= rx_min || ry_max <= ry_min {
        return;
    }

    let rect = tiny_skia::Rect::from_ltrb(rx_min, ry_min, rx_max, ry_max)
        .unwrap_or(tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap());
    pixmap.fill_rect(rect, &paint, Transform::identity(), clip_mask);
}

/// Render a radial shading into viewport-local coordinates.
fn render_radial_shading_viewport(
    pixmap: &mut Pixmap,
    params: &RadialShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    clip_mask: Option<&Mask>,
) {
    let pw = pixmap.width();
    let ph = pixmap.height();
    if params.color_stops.is_empty() || pw == 0 || ph == 0 {
        return;
    }

    let Some(inv_ctm) = params.ctm.invert() else {
        return;
    };

    let (px_min, py_min, px_max, py_max) = if let Some(bbox) = &params.bbox {
        let corners = [
            params.ctm.transform_point(bbox[0], bbox[1]),
            params.ctm.transform_point(bbox[2], bbox[1]),
            params.ctm.transform_point(bbox[0], bbox[3]),
            params.ctm.transform_point(bbox[2], bbox[3]),
        ];
        let x_min = corners
            .iter()
            .map(|c| c.0 as f32)
            .fold(f32::INFINITY, f32::min);
        let y_min = corners
            .iter()
            .map(|c| c.1 as f32)
            .fold(f32::INFINITY, f32::min);
        let x_max = corners
            .iter()
            .map(|c| c.0 as f32)
            .fold(f32::NEG_INFINITY, f32::max);
        let y_max = corners
            .iter()
            .map(|c| c.1 as f32)
            .fold(f32::NEG_INFINITY, f32::max);
        (
            ((x_min - vp_x) * scale_x).max(0.0) as u32,
            ((y_min - vp_y) * scale_y).max(0.0) as u32,
            (((x_max - vp_x) * scale_x).ceil() as u32).min(pw),
            (((y_max - vp_y) * scale_y).ceil() as u32).min(ph),
        )
    } else {
        (0, 0, pw, ph)
    };

    let inv_sx = 1.0 / scale_x as f64;
    let inv_sy = 1.0 / scale_y as f64;

    let data = pixmap.data_mut();
    let stride = pw as usize * 4;

    for py in py_min..py_max {
        // Map viewport pixel back to device space
        let dev_y = py as f64 * inv_sy + vp_y as f64;
        for px in px_min..px_max {
            let dev_x = px as f64 * inv_sx + vp_x as f64;

            let (ux, uy) = inv_ctm.transform_point(dev_x, dev_y);

            let t = solve_radial_t(
                ux, uy, params.x0, params.y0, params.r0, params.x1, params.y1, params.r1,
            );
            if let Some(t) = t {
                if t < 0.0 && !params.extend_start {
                    continue;
                }
                if t > 1.0 && !params.extend_end {
                    continue;
                }
                let clamped = t.clamp(0.0, 1.0);
                let color = interpolate_color_stops(&params.color_stops, clamped);

                if let Some(mask) = clip_mask {
                    let mask_val = mask.data()[py as usize * pw as usize + px as usize];
                    if mask_val == 0 {
                        continue;
                    }
                }

                let offset = py as usize * stride + px as usize * 4;
                data[offset] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 3] = 255;
            }
        }
    }
}

/// Render a Gouraud-shaded triangle mesh into viewport-local coordinates.
fn render_mesh_shading_viewport(
    pixmap: &mut Pixmap,
    params: &MeshShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
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
        let (dx0, dy0) = params.ctm.transform_point(tri.v0.x, tri.v0.y);
        let (dx1, dy1) = params.ctm.transform_point(tri.v1.x, tri.v1.y);
        let (dx2, dy2) = params.ctm.transform_point(tri.v2.x, tri.v2.y);

        // Map to viewport pixels
        let x0 = (dx0 as f32 - vp_x) * scale_x;
        let y0 = (dy0 as f32 - vp_y) * scale_y;
        let x1 = (dx1 as f32 - vp_x) * scale_x;
        let y1 = (dy1 as f32 - vp_y) * scale_y;
        let x2 = (dx2 as f32 - vp_x) * scale_x;
        let y2 = (dy2 as f32 - vp_y) * scale_y;

        let min_x = (x0.min(x1).min(x2).floor().max(0.0)) as usize;
        let max_x = (x0.max(x1).max(x2).ceil() as usize).min(pw);
        let min_y = (y0.min(y1).min(y2).floor().max(0.0)) as usize;
        let max_y = (y0.max(y1).max(y2).ceil() as usize).min(ph);

        if min_x >= max_x || min_y >= max_y {
            continue;
        }

        let x0 = x0 as f64;
        let y0 = y0 as f64;
        let x1 = x1 as f64;
        let y1 = y1 as f64;
        let x2 = x2 as f64;
        let y2 = y2 as f64;
        let denom = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2);
        if denom.abs() < 1e-10 {
            continue;
        }
        let inv_denom = 1.0 / denom;

        for py in min_y..max_y {
            for px in min_x..max_x {
                let pxf = px as f64 + 0.5;
                let pyf = py as f64 + 0.5;

                let w0 = ((y1 - y2) * (pxf - x2) + (x2 - x1) * (pyf - y2)) * inv_denom;
                let w1 = ((y2 - y0) * (pxf - x2) + (x0 - x2) * (pyf - y2)) * inv_denom;
                let w2 = 1.0 - w0 - w1;

                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }

                if let Some(mask) = clip_mask {
                    let mask_val = mask.data()[py * pw + px];
                    if mask_val == 0 {
                        continue;
                    }
                }

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

/// Render a patch mesh into viewport-local coordinates.
fn render_patch_shading_viewport(
    pixmap: &mut Pixmap,
    params: &PatchShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    clip_mask: Option<&Mask>,
) {
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
            color_space: params.color_space.clone(),
        };
        render_mesh_shading_viewport(
            pixmap,
            &mesh_params,
            vp_x,
            vp_y,
            scale_x,
            scale_y,
            clip_mask,
        );
    }
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
            is_text_glyph: false,
            overprint: false,
            spot_color: None,
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
            stroke_adjust: false,
            is_text_glyph: false,
            overprint: false,
            spot_color: None,
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
            is_text_glyph: false,
            overprint: false,
            spot_color: None,
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
            is_text_glyph: false,
            overprint: false,
            spot_color: None,
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
            is_text_glyph: false,
            overprint: false,
            spot_color: None,
        };
        dev.fill_path(&path, &params);

        // Pixel at translated location should be green
        let pixel = dev.pixmap().pixel(105, 105).unwrap();
        assert_eq!(pixel.green(), 255);
        assert_eq!(pixel.red(), 0);
    }
}
