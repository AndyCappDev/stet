// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! tiny-skia implementation of the `OutputDevice` trait.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

#[cfg(feature = "parallel")]
use rayon::prelude::*;
use stet_tiny_skia::{
    BlendMode, Color, FillRule as SkiaFillRule, LineCap as SkiaLineCap, LineJoin as SkiaLineJoin,
    Mask, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

use stet_core::device::OutputDevice;
use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DeviceColor, FillRule, LineCap, LineJoin};
use stet_graphics::device::{
    AxialShadingParams, ClipParams, FillParams, ImageColorSpace, ImageParams, MeshShadingParams,
    PageSinkFactory, PatchShadingParams, RadialShadingParams, ShadingColorSpace, ShadingVertex,
    StrokeParams, TintLookupTable,
};
use stet_graphics::icc::IccCache;

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
    /// Disable anti-aliasing for all fill/stroke operations (matches GhostScript).
    no_aa: bool,
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
            no_aa: false,
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

    /// Disable anti-aliasing for all fill/stroke operations.
    pub fn set_no_aa(&mut self, no_aa: bool) {
        self.no_aa = no_aa;
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
    to_paint_alpha(color, 1.0, 0, false)
}

/// Convert a `DeviceColor` to tiny-skia `Paint` with the given opacity and blend mode.
fn to_paint_alpha(color: &DeviceColor, alpha: f64, blend_mode: u8, no_aa: bool) -> Paint<'static> {
    let mut paint = Paint::default();
    let a = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    paint.set_color_rgba8(
        (color.r * 255.0).round().clamp(0.0, 255.0) as u8,
        (color.g * 255.0).round().clamp(0.0, 255.0) as u8,
        (color.b * 255.0).round().clamp(0.0, 255.0) as u8,
        a,
    );
    paint.anti_alias = !no_aa;
    paint.blend_mode = u8_to_blend_mode(blend_mode);
    paint
}

/// Map a blend mode byte (0–15) to the corresponding tiny-skia `BlendMode`.
fn u8_to_blend_mode(mode: u8) -> BlendMode {
    match mode {
        1 => BlendMode::Multiply,
        2 => BlendMode::Screen,
        3 => BlendMode::Overlay,
        4 => BlendMode::Darken,
        5 => BlendMode::Lighten,
        6 => BlendMode::ColorDodge,
        7 => BlendMode::ColorBurn,
        8 => BlendMode::HardLight,
        9 => BlendMode::SoftLight,
        10 => BlendMode::Difference,
        11 => BlendMode::Exclusion,
        12 => BlendMode::Hue,
        13 => BlendMode::Saturation,
        14 => BlendMode::Color,
        15 => BlendMode::Luminosity,
        _ => BlendMode::SourceOver,
    }
}

/// Convert a `PsPath` to tiny-skia `Path`.
fn build_skia_path(path: &PsPath) -> Option<stet_tiny_skia::Path> {
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

/// Convert a tiny-skia Path back to a PsPath.
/// Used for overprint stroke handling where we convert a stroked outline to a fill.
fn skia_path_to_ps_path(path: &stet_tiny_skia::Path) -> PsPath {
    let mut segments = Vec::new();
    for seg in path.segments() {
        match seg {
            stet_tiny_skia::PathSegment::MoveTo(p) => {
                segments.push(PathSegment::MoveTo(p.x as f64, p.y as f64));
            }
            stet_tiny_skia::PathSegment::LineTo(p) => {
                segments.push(PathSegment::LineTo(p.x as f64, p.y as f64));
            }
            stet_tiny_skia::PathSegment::QuadTo(p1, p2) => {
                // Convert quadratic to cubic: control points at 2/3 along quad controls
                let last = segments.last().map(|s| match s {
                    PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => (*x, *y),
                    PathSegment::CurveTo { x3, y3, .. } => (*x3, *y3),
                    PathSegment::ClosePath => (0.0, 0.0),
                }).unwrap_or((0.0, 0.0));
                let (x0, y0) = last;
                let cx1 = x0 + 2.0 / 3.0 * (p1.x as f64 - x0);
                let cy1 = y0 + 2.0 / 3.0 * (p1.y as f64 - y0);
                let cx2 = p2.x as f64 + 2.0 / 3.0 * (p1.x as f64 - p2.x as f64);
                let cy2 = p2.y as f64 + 2.0 / 3.0 * (p1.y as f64 - p2.y as f64);
                segments.push(PathSegment::CurveTo {
                    x1: cx1, y1: cy1, x2: cx2, y2: cy2,
                    x3: p2.x as f64, y3: p2.y as f64,
                });
            }
            stet_tiny_skia::PathSegment::CubicTo(p1, p2, p3) => {
                segments.push(PathSegment::CurveTo {
                    x1: p1.x as f64, y1: p1.y as f64,
                    x2: p2.x as f64, y2: p2.y as f64,
                    x3: p3.x as f64, y3: p3.y as f64,
                });
            }
            stet_tiny_skia::PathSegment::Close => {
                segments.push(PathSegment::ClosePath);
            }
        }
    }
    PsPath { segments }
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

use stet_graphics::display_list::{DisplayElement, DisplayList};

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
    /// Per-pixel CMYK tracking buffer for overprint simulation.
    /// Only allocated when the display list contains overprint elements.
    /// Layout: [C, M, Y, K] as f32 per pixel, band_w * band_h * 4 entries.
    cmyk_buffer: Option<Vec<f32>>,
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

/// Unified rendering context that parameterizes both band and viewport rendering.
///
/// Band rendering is viewport rendering with `scale_x = scale_y = 1.0`.
/// `viewport_transform(t, vp_x, vp_y, 1.0, 1.0)` == `offset_transform_xy(t, vp_x, vp_y)`.
struct RenderContext<'a> {
    /// Viewport/band origin X in device space.
    vp_x: f32,
    /// Viewport/band origin Y in device space.
    vp_y: f32,
    /// Horizontal scale (1.0 for band rendering, zoom for viewport).
    scale_x: f32,
    /// Vertical scale (1.0 for band rendering, zoom for viewport).
    scale_y: f32,
    /// Output pixmap width in pixels.
    out_w: u32,
    /// Output pixmap height in pixels.
    out_h: u32,
    /// Effective DPI at output scale.
    effective_dpi: f64,
    /// ICC color profile cache (for CMYK conversions).
    icc: Option<&'a IccCache>,
    /// Pre-converted image data cache (for viewport rendering).
    image_cache: Option<&'a ImageCache>,
    /// Element index in parent display list (for image cache lookup).
    elem_idx: usize,
    /// Disable anti-aliasing for all fill/stroke operations.
    no_aa: bool,
}

impl RenderContext<'_> {
    /// Apply viewport transform to a PostScript matrix.
    fn transform(&self, m: &Matrix) -> Transform {
        viewport_transform(to_transform(m), self.vp_x, self.vp_y, self.scale_x, self.scale_y)
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
            DisplayElement::Group { params, .. } => Some(YBBox {
                y_min: params.bbox[1],
                y_max: params.bbox[3],
            }),
            DisplayElement::SoftMasked { params, .. } => Some(YBBox {
                y_min: params.bbox[1],
                y_max: params.bbox[3],
            }),
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
/// Composite premultiplied-alpha RGBA pixels onto a white background.
/// After this, all pixels are fully opaque (alpha=255).
fn composite_onto_white(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        let a = pixel[3] as u16;
        if a == 255 {
            continue; // fully opaque — no compositing needed
        }
        let inv_a = 255 - a;
        pixel[0] = (pixel[0] as u16 + inv_a).min(255) as u8;
        pixel[1] = (pixel[1] as u16 + inv_a).min(255) as u8;
        pixel[2] = (pixel[2] as u16 + inv_a).min(255) as u8;
        pixel[3] = 255;
    }
}


/// Extract the contribution of a non-isolated transparency group and composite
/// it onto the parent using the group's blend mode and alpha.
///
/// Composite a (possibly cropped) non-isolated group offscreen onto the parent pixmap.
///
/// Like `extract_and_composite_contribution`, but the offscreen and backdrop
/// are crop-sized (only covering the group's bounding box region), positioned
/// at `(crop_x, crop_y)` in the parent's coordinate system.
fn composite_non_isolated_group_cropped(
    target: &mut Pixmap,
    source: &Pixmap,
    backdrop: &[u8],
    params: &stet_graphics::display_list::GroupParams,
    clip_mask: Option<&stet_tiny_skia::Mask>,
    crop_x: i32,
    crop_y: i32,
) {
    let cw = source.width();
    let ch = source.height();

    // Build a contribution pixmap: pixels that changed vs backdrop
    let Some(mut contribution) = Pixmap::new(cw, ch) else {
        return;
    };
    let src_data = source.data();
    let contrib_data = contribution.data_mut();

    for (i, chunk) in contrib_data.chunks_exact_mut(4).enumerate() {
        let off = i * 4;
        if src_data[off] != backdrop[off]
            || src_data[off + 1] != backdrop[off + 1]
            || src_data[off + 2] != backdrop[off + 2]
            || src_data[off + 3] != backdrop[off + 3]
        {
            chunk.copy_from_slice(&src_data[off..off + 4]);
        }
    }

    let paint = stet_tiny_skia::PixmapPaint {
        opacity: params.alpha as f32,
        blend_mode: u8_to_blend_mode(params.blend_mode),
        quality: stet_tiny_skia::FilterQuality::Nearest,
    };
    target.draw_pixmap(
        crop_x,
        crop_y,
        contribution.as_ref(),
        &paint,
        Transform::identity(),
        clip_mask,
    );
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

/// Fast area-average box filter resample for downscaling.
///
/// Each output pixel averages all source pixels that fall within its footprint.
/// Two-pass separable (horizontal then vertical) for O(src) total work regardless
/// of scale ratio. Produces quality equivalent to Lanczos3 for downscaling at a
/// fraction of the cost.
fn box_resample(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if dw == 0 || dh == 0 {
        return Vec::new();
    }
    // Pass 1: horizontal (sw → dw) using column prefix sums per row.
    let ratio_x = sw as f64 / dw as f64;
    let mut tmp = vec![0u32; dw as usize * sh as usize * 4];
    let tmp_stride = dw as usize * 4;

    for y in 0..sh as usize {
        let row_off = y * sw as usize * 4;
        // Build prefix sums for this row (one extra element at index 0 = 0).
        // To avoid a separate allocation per row, compute on the fly.
        for dx in 0..dw as usize {
            let left_f = dx as f64 * ratio_x;
            let right_f = (dx + 1) as f64 * ratio_x;
            let left = left_f as usize;
            let right = (right_f as usize).min(sw as usize);
            let count = (right - left).max(1) as u32;
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sx in left..right {
                let i = row_off + sx * 4;
                r += src[i] as u32;
                g += src[i + 1] as u32;
                b += src[i + 2] as u32;
                a += src[i + 3] as u32;
            }
            let di = y * tmp_stride + dx * 4;
            tmp[di] = (r + count / 2) / count;
            tmp[di + 1] = (g + count / 2) / count;
            tmp[di + 2] = (b + count / 2) / count;
            tmp[di + 3] = (a + count / 2) / count;
        }
    }

    // Pass 2: vertical (sh → dh) on the horizontally-resampled tmp.
    let ratio_y = sh as f64 / dh as f64;
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    let out_stride = dw as usize * 4;

    for x in 0..dw as usize {
        for dy in 0..dh as usize {
            let top_f = dy as f64 * ratio_y;
            let bottom_f = (dy + 1) as f64 * ratio_y;
            let top = top_f as usize;
            let bottom = (bottom_f as usize).min(sh as usize);
            let count = (bottom - top).max(1) as u32;
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sy in top..bottom {
                let i = sy * tmp_stride + x * 4;
                r += tmp[i];
                g += tmp[i + 1];
                b += tmp[i + 2];
                a += tmp[i + 3];
            }
            let di = dy * out_stride + x * 4;
            out[di] = ((r + count / 2) / count) as u8;
            out[di + 1] = ((g + count / 2) / count) as u8;
            out[di + 2] = ((b + count / 2) / count) as u8;
            out[di + 3] = ((a + count / 2) / count) as u8;
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
/// For axis-aligned transforms: box-filter resample to the exact target dimensions.
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
    if let Some(cmyk_bytes) = system_cmyk_bytes
        && let Some(hash) = cache.register_profile(cmyk_bytes) {
            seen.insert(hash);
            // Set the default CMYK hash so convert_image_8bit works for DeviceCMYK
            cache.set_default_cmyk_hash(hash);
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
            && seen.insert(*profile_hash) {
                cache.register_profile(profile_data);
            }
        // Also check Indexed with ICCBased base
        if let Some(ImageColorSpace::Indexed { base, .. }) = cs
            && let ImageColorSpace::ICCBased {
                profile_hash,
                profile_data,
                ..
            } = base.as_ref()
                && seen.insert(*profile_hash) {
                    cache.register_profile(profile_data);
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
            if let Some(cache) = icc
                && let Some(cmyk_hash) = cache.default_cmyk_hash() {
                    let avail_pixels = data.len() / 4;
                    let icc_pixels = avail_pixels.min(npixels);
                    if icc_pixels > 0
                        && let Some(rgb) = cache.convert_image_8bit(cmyk_hash, data, icc_pixels) {
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
            if let Some(cache) = icc
                && cache.has_profile(profile_hash)
                    && let Some(rgb) = cache.convert_image_8bit(profile_hash, data, npixels) {
                        let mut rgba = vec![255u8; npixels * 4];
                        for i in 0..npixels {
                            rgba[i * 4] = rgb[i * 3];
                            rgba[i * 4 + 1] = rgb[i * 3 + 1];
                            rgba[i * 4 + 2] = rgb[i * 3 + 2];
                        }
                        return rgba;
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
            // For CMYK alt space with ICC, build bulk CMYK data and convert via ICC
            if matches!(alt_space.as_ref(), ImageColorSpace::DeviceCMYK)
                && let Some(rgba) =
                    tint_separation_via_icc(data, npixels, tint_table, icc)
                {
                    return rgba;
                }
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
            // For CMYK alt space with ICC, build bulk CMYK data and convert via ICC
            if matches!(alt_space.as_ref(), ImageColorSpace::DeviceCMYK)
                && let Some(rgba) =
                    tint_devicen_via_icc(data, npixels, ni, tint_table, icc)
                {
                    return rgba;
                }
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

/// Convert Separation (1-input) tint table output through ICC CMYK profile.
/// Builds 4-byte CMYK data from tint table, then bulk-converts via ICC 8-bit transform.
fn tint_separation_via_icc(
    data: &[u8],
    npixels: usize,
    tint_table: &TintLookupTable,
    icc: Option<&IccCache>,
) -> Option<Vec<u8>> {
    let cache = icc?;
    let cmyk_hash = cache.default_cmyk_hash()?;
    // Build CMYK byte buffer from tint table
    let mut cmyk_data = vec![0u8; npixels * 4];
    let mut alt_comps = [0.0f32; 4];
    for i in 0..npixels {
        let tint = data.get(i).copied().unwrap_or(0) as f32 / 255.0;
        tint_table.lookup_1d(tint, &mut alt_comps);
        let si = i * 4;
        cmyk_data[si] = (alt_comps[0].clamp(0.0, 1.0) * 255.0).round() as u8;
        cmyk_data[si + 1] = (alt_comps[1].clamp(0.0, 1.0) * 255.0).round() as u8;
        cmyk_data[si + 2] = (alt_comps[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        cmyk_data[si + 3] = (alt_comps[3].clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    let rgb = cache.convert_image_8bit(cmyk_hash, &cmyk_data, npixels)?;
    let mut rgba = vec![255u8; npixels * 4];
    for i in 0..npixels {
        rgba[i * 4] = rgb[i * 3];
        rgba[i * 4 + 1] = rgb[i * 3 + 1];
        rgba[i * 4 + 2] = rgb[i * 3 + 2];
    }
    Some(rgba)
}

/// Convert DeviceN (N-input) tint table output through ICC CMYK profile.
fn tint_devicen_via_icc(
    data: &[u8],
    npixels: usize,
    ni: usize,
    tint_table: &TintLookupTable,
    icc: Option<&IccCache>,
) -> Option<Vec<u8>> {
    let cache = icc?;
    let cmyk_hash = cache.default_cmyk_hash()?;
    let mut cmyk_data = vec![0u8; npixels * 4];
    let mut inputs = vec![0.0f32; ni];
    let mut alt_comps = [0.0f32; 4];
    for i in 0..npixels {
        let si = i * ni;
        for (c, inp) in inputs.iter_mut().enumerate() {
            *inp = data.get(si + c).copied().unwrap_or(0) as f32 / 255.0;
        }
        tint_table.lookup_nd(&inputs, &mut alt_comps);
        let di = i * 4;
        cmyk_data[di] = (alt_comps[0].clamp(0.0, 1.0) * 255.0).round() as u8;
        cmyk_data[di + 1] = (alt_comps[1].clamp(0.0, 1.0) * 255.0).round() as u8;
        cmyk_data[di + 2] = (alt_comps[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        cmyk_data[di + 3] = (alt_comps[3].clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    let rgb = cache.convert_image_8bit(cmyk_hash, &cmyk_data, npixels)?;
    let mut rgba = vec![255u8; npixels * 4];
    for i in 0..npixels {
        rgba[i * 4] = rgb[i * 3];
        rgba[i * 4 + 1] = rgb[i * 3 + 1];
        rgba[i * 4 + 2] = rgb[i * 3 + 2];
    }
    Some(rgba)
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

    // Pre-scale any image that's being downscaled. Lanczos3 produces much
    // better results than tiny-skia's bilinear for text, bitmap fonts, and
    // detailed images — especially at fit-to-screen zoom levels.
    if min_scale >= 0.95 {
        return None;
    }

    // Axis-aligned: use area-average box filter to target dimensions.
    // Much faster than Lanczos3 and produces equally good results for downscaling.
    let is_axis_aligned = transform.kx.abs() < 1e-4 && transform.ky.abs() < 1e-4;
    if is_axis_aligned && w >= 2 && h >= 2 {
        let dw = (w as f32 * transform.sx.abs()).ceil().max(1.0) as u32;
        let dh = (h as f32 * transform.sy.abs()).ceil().max(1.0) as u32;
        if dw < w || dh < h {
            let resampled = box_resample(rgba_data, w, h, dw, dh);
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
    let area = factor * factor;
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
///
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
                    if let Some(PathSegment::MoveTo(_, ly) | PathSegment::LineTo(_, ly)) = result.segments.last_mut() {
                        *ly = snapped_y;
                    }
                    result.segments.push(PathSegment::LineTo(x, snapped_y));
                    prev_x = x;
                    prev_y = snapped_y;
                } else if is_vertical {
                    let snapped_x = snap_x(x);
                    if let Some(PathSegment::MoveTo(lx, _) | PathSegment::LineTo(lx, _)) = result.segments.last_mut() {
                        *lx = snapped_x;
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

/// Process a single display list element into a pixmap using the given render context.
///
/// This unified function handles both band rendering (scale=1.0) and viewport
/// rendering (arbitrary scale). Band rendering is viewport rendering with
/// `scale_x = scale_y = 1.0`.
fn render_element(
    pixmap: &mut Pixmap,
    band_state: &mut BandState,
    element: &DisplayElement,
    ctx: &RenderContext<'_>,
) {
    match element {
        DisplayElement::Fill { path, params } => {
            // Only use overprint path for CMYK fills with partial channels.
            // When all channels are painted (CMYK_ALL) or for non-CMYK fills,
            // overprint has no visual effect — use the normal rendering path.
            let needs_overprint = params.overprint
                && params.is_device_cmyk
                && params.painted_channels != stet_graphics::device::CMYK_ALL
                && band_state.cmyk_buffer.is_some();

            if needs_overprint {
                let mut cmyk_buf = band_state.cmyk_buffer.take().unwrap();
                render_overprint_fill(
                    pixmap,
                    &mut cmyk_buf,
                    band_state,
                    path,
                    params,
                    ctx.vp_x,
                    ctx.vp_y,
                    ctx.scale_x,
                    ctx.scale_y,
                    ctx.out_w,
                    ctx.out_h,
                    ctx.icc,
                    ctx.no_aa,
                );
                band_state.cmyk_buffer = Some(cmyk_buf);
            } else {
                let Some(skia_path) = build_skia_path(path) else {
                    return;
                };
                let mut temp_mask = None;
                let Some(mask_ref) =
                    resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h)
                else {
                    return;
                };
                let paint = to_paint_alpha(&params.color, params.alpha, params.blend_mode, ctx.no_aa);
                let transform = ctx.transform(&params.ctm);
                let fill_rule = to_fill_rule(&params.fill_rule);
                pixmap.fill_path(&skia_path, &paint, fill_rule, transform, mask_ref);

                // Update CMYK tracking buffer for non-overprint fills
                if let Some(ref mut cmyk_buf) = band_state.cmyk_buffer {
                    update_cmyk_buffer_for_fill(
                        cmyk_buf,
                        path,
                        params,
                        ctx.vp_x,
                        ctx.vp_y,
                        ctx.scale_x,
                        ctx.scale_y,
                        ctx.out_w,
                        ctx.out_h,
                        &band_state.clip_region,
                        ctx.no_aa,
                    );
                }
            }
        }
        DisplayElement::Stroke { path, params } => {
            let transform = ctx.transform(&params.ctm);
            // Build stroke using the composited transform so hairline width
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
            let stroke = build_stroke(&vp_params, ctx.effective_dpi);

            // Apply stroke adjustment — snap in output device space
            let adjusted;
            let draw_path = if params.stroke_adjust && stroke.width <= 2.0 {
                adjusted = stroke_adjust_path_viewport(
                    path,
                    stroke.width as f64,
                    ctx.scale_x as f64,
                    ctx.scale_y as f64,
                    ctx.vp_x as f64,
                    ctx.vp_y as f64,
                );
                &adjusted
            } else {
                path
            };

            let needs_overprint = params.overprint
                && params.painted_channels != 0
                && band_state.cmyk_buffer.is_some();

            // For overprint strokes, try converting stroke outline to a fill
            // for per-channel CMYK simulation. If the stroke-to-fill conversion
            // fails (e.g., hairline strokes too thin for outline conversion),
            // fall through to the normal stroke path so the stroke still renders.
            let mut overprint_handled = false;
            if needs_overprint {
                let skia_path_op = build_skia_path(draw_path);
                if let Some(skia_path) = skia_path_op {
                    let resolution_scale = (transform.sx * transform.sx + transform.sy * transform.sy).sqrt().max(1.0);
                    if let Some(transformed) = skia_path.clone().transform(transform)
                        && let Some(stroked) = transformed.stroke(&stroke, resolution_scale) {
                            overprint_handled = true;
                            let fill_params = FillParams {
                                color: params.color.clone(),
                                fill_rule: FillRule::NonZeroWinding,
                                ctm: Matrix::identity(),
                                is_text_glyph: false,
                                overprint: params.overprint,
                                overprint_mode: params.overprint_mode,
                                painted_channels: params.painted_channels,
                                is_device_cmyk: false,
                                spot_color: params.spot_color.clone(),
                                rendering_intent: params.rendering_intent,
                                transfer: params.transfer.clone(),
                                halftone: params.halftone.clone(),
                                bg_ucr: params.bg_ucr.clone(),
                                alpha: params.alpha,
                                blend_mode: params.blend_mode,
                            };
                            let stroked_path = skia_path_to_ps_path(&stroked);
                            let mut cmyk_buf = band_state.cmyk_buffer.take().unwrap();
                            render_overprint_fill(
                                pixmap,
                                &mut cmyk_buf,
                                band_state,
                                &stroked_path,
                                &fill_params,
                                0.0, 0.0, 1.0, 1.0,
                                ctx.out_w,
                                ctx.out_h,
                                ctx.icc,
                                ctx.no_aa,
                            );
                            band_state.cmyk_buffer = Some(cmyk_buf);
                        }
                }
            }
            if !overprint_handled {
                let Some(skia_path) = build_skia_path(draw_path) else {
                    return;
                };
                let mut temp_mask = None;
                let Some(mask_ref) =
                    resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h)
                else {
                    return;
                };
                let paint = to_paint_alpha(&params.color, params.alpha, params.blend_mode, ctx.no_aa);
                pixmap.stroke_path(&skia_path, &paint, &stroke, transform, mask_ref);
            }
        }
        DisplayElement::Clip { path, params } => {
            clip_path_unified(band_state, path, params, ctx);
        }
        DisplayElement::InitClip => {
            if let Some(ClipRegion::Mask(mask)) = band_state.clip_region.take() {
                band_state.recycle_mask(mask);
            }
            band_state.clip_region = None;
        }
        DisplayElement::ErasePage => {
            pixmap.fill(Color::TRANSPARENT);
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

            let needs_overprint = params.overprint
                && band_state.cmyk_buffer.is_some()
                && image_supports_overprint(&params.color_space);

            if needs_overprint {
                let mut cmyk_buf = band_state.cmyk_buffer.take().unwrap();
                render_overprint_image(
                    pixmap,
                    &mut cmyk_buf,
                    band_state,
                    sample_data,
                    params,
                    ctx.vp_x,
                    ctx.vp_y,
                    ctx.scale_x,
                    ctx.scale_y,
                    ctx.out_w,
                    ctx.out_h,
                    ctx.icc,
                );
                band_state.cmyk_buffer = Some(cmyk_buf);
            } else {
                // Use pre-converted RGBA from image cache when available
                let owned_rgba;
                let rgba_data: &[u8] = if let Some(cached) = ctx.image_cache.and_then(|c| c.get(ctx.elem_idx)) {
                    cached
                } else {
                    owned_rgba = {
                        let mut rgba = samples_to_rgba(sample_data, params, ctx.icc);
                        if params.mask_color.is_some() {
                            apply_mask_color_rgba(&mut rgba, sample_data, params);
                        }
                        rgba
                    };
                    &owned_rgba
                };
                let expected = (iw * ih * 4) as usize;
                if rgba_data.len() < expected {
                    return;
                }
                let Some(image_inv) = params.image_matrix.invert() else {
                    return;
                };
                let combined = params.ctm.concat(&image_inv);
                let raw_transform = ctx.transform(&combined);

                // Pre-scale images that are being downscaled. Even non-interpolated
                // images need proper area averaging when shrinking — "no interpolation"
                // means don't smooth when *upscaling*, but downscaling without averaging
                // produces aliased garbage.
                let prescaled = prescale_image(rgba_data, iw, ih, raw_transform);
                let (img_data, img_w, img_h, transform) = match &prescaled {
                    Some((data, w, h, t)) => (data.as_slice(), *w, *h, *t),
                    None => (rgba_data, iw, ih, raw_transform),
                };

                let Some(img_pixmap) = stet_tiny_skia::PixmapRef::from_bytes(img_data, img_w, img_h)
                else {
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
                        } else if rect.is_full_page(ctx.out_w, ctx.out_h) {
                            None
                        } else {
                            temp_mask = rect.make_mask(ctx.out_w, ctx.out_h);
                            temp_mask.as_ref()
                        }
                    }
                };
                let eff_sx =
                    (transform.sx * transform.sx + transform.ky * transform.ky).sqrt();
                let eff_sy =
                    (transform.kx * transform.kx + transform.sy * transform.sy).sqrt();
                let quality = if eff_sx >= 0.9 && eff_sy >= 0.9 {
                    stet_tiny_skia::FilterQuality::Nearest
                } else {
                    stet_tiny_skia::FilterQuality::Bilinear
                };
                let img_paint = stet_tiny_skia::PixmapPaint {
                    quality,
                    opacity: params.alpha as f32,
                    blend_mode: u8_to_blend_mode(params.blend_mode),
                };
                pixmap.draw_pixmap(0, 0, img_pixmap, &img_paint, transform, mask_ref);

                // Update CMYK tracking buffer for non-overprint images
                if let Some(ref mut cmyk_buf) = band_state.cmyk_buffer {
                    update_cmyk_buffer_for_image(
                        cmyk_buf,
                        sample_data,
                        params,
                        ctx.vp_x,
                        ctx.vp_y,
                        ctx.scale_x,
                        ctx.scale_y,
                        ctx.out_w,
                        ctx.out_h,
                        &band_state.clip_region,
                    );
                }
            }
        }
        DisplayElement::AxialShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h)
            else {
                return;
            };
            render_axial_shading(pixmap, params, ctx.vp_x, ctx.vp_y, ctx.scale_x, ctx.scale_y, mask_ref, ctx.no_aa,
                band_state.cmyk_buffer.as_deref_mut(), ctx.icc);
        }
        DisplayElement::RadialShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h)
            else {
                return;
            };
            render_radial_shading(pixmap, params, ctx.vp_x, ctx.vp_y, ctx.scale_x, ctx.scale_y, mask_ref, ctx.no_aa,
                band_state.cmyk_buffer.as_deref_mut(), ctx.icc);
        }
        DisplayElement::MeshShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h)
            else {
                return;
            };
            render_mesh_shading(pixmap, params, ctx.vp_x, ctx.vp_y, ctx.scale_x, ctx.scale_y, mask_ref,
                band_state.cmyk_buffer.as_deref_mut(), ctx.icc);
        }
        DisplayElement::PatchShading { params } => {
            let mut temp_mask = None;
            let Some(mask_ref) =
                resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h)
            else {
                return;
            };
            render_patch_shading(pixmap, params, ctx.vp_x, ctx.vp_y, ctx.scale_x, ctx.scale_y, mask_ref,
                band_state.cmyk_buffer.as_deref_mut(), ctx.icc);
        }
        DisplayElement::PatternFill { params } => {
            render_pattern_fill(pixmap, band_state, params, ctx);
        }
        DisplayElement::Group { elements, params } => {
            render_group(pixmap, band_state, elements, params, ctx);
        }
        DisplayElement::SoftMasked {
            mask,
            content,
            params,
        } => {
            render_soft_masked(pixmap, band_state, mask, content, params, ctx);
        }
        DisplayElement::Text { .. } => {} // PDF-only, ignored by rasterizer
    }
}

/// Compute the cropped output-pixel region for a group's device-space bounding box.
///
/// Returns `(crop_x, crop_y, crop_w, crop_h)` in output pixels, or `None` if
/// the group is entirely outside the viewport or cropping isn't worthwhile.
fn compute_group_crop(
    bbox: &[f64; 4],
    ctx: &RenderContext<'_>,
) -> Option<(i32, i32, u32, u32)> {
    // Transform device-space bbox to output pixel coords
    let px_min = ((bbox[0] as f32 - ctx.vp_x) * ctx.scale_x).floor() as i32;
    let py_min = ((bbox[1] as f32 - ctx.vp_y) * ctx.scale_y).floor() as i32;
    let px_max = ((bbox[2] as f32 - ctx.vp_x) * ctx.scale_x).ceil() as i32;
    let py_max = ((bbox[3] as f32 - ctx.vp_y) * ctx.scale_y).ceil() as i32;

    // Clip to output bounds
    let x0 = px_min.max(0);
    let y0 = py_min.max(0);
    let x1 = px_max.min(ctx.out_w as i32);
    let y1 = py_max.min(ctx.out_h as i32);

    if x0 >= x1 || y0 >= y1 {
        return None;
    }

    let crop_w = (x1 - x0) as u32;
    let crop_h = (y1 - y0) as u32;

    // Only crop if it saves at least 25% of pixels
    let crop_pixels = crop_w as u64 * crop_h as u64;
    let full_pixels = ctx.out_w as u64 * ctx.out_h as u64;
    if crop_pixels * 4 >= full_pixels * 3 {
        return None;
    }

    Some((x0, y0, crop_w, crop_h))
}

/// Render a transparency group into a pixmap.
///
/// Creates an offscreen pixmap, renders the group's child elements into it,
/// then composites back onto the parent with the group's blend mode and alpha.
fn render_group(
    pixmap: &mut Pixmap,
    band_state: &mut BandState,
    elements: &DisplayList,
    params: &stet_graphics::display_list::GroupParams,
    ctx: &RenderContext<'_>,
) {
    if params.knockout {
        render_knockout_group(pixmap, band_state, elements, params, ctx);
        return;
    }

    let crop = compute_group_crop(&params.bbox, ctx);

    let (eff_w, eff_h, crop_x, crop_y, eff_vp_x, eff_vp_y) = match crop {
        Some((cx, cy, cw, ch)) => (
            cw,
            ch,
            cx,
            cy,
            ctx.vp_x + cx as f32 / ctx.scale_x,
            ctx.vp_y + cy as f32 / ctx.scale_y,
        ),
        None => (ctx.out_w, ctx.out_h, 0, 0, ctx.vp_x, ctx.vp_y),
    };

    let Some(mut offscreen) = Pixmap::new(eff_w, eff_h) else {
        return;
    };

    // For non-isolated groups, copy the parent backdrop into the offscreen buffer.
    // Exception: for non-Normal blend modes, render as isolated to avoid
    // anti-aliased clip edges at BBox boundaries creating visible artifacts.
    // The contribution-extraction approach (comparing source vs backdrop byte-by-byte)
    // mishandles partially-blended edge pixels, causing them to be Screen/Multiply/etc.
    // blended incorrectly. Rendering as isolated uses proper alpha coverage instead.
    let treat_as_isolated = params.blend_mode != 0;
    let backdrop = if !params.isolated && !treat_as_isolated {
        let data = if crop.is_some() {
            copy_backdrop_crop(pixmap, crop_x, crop_y, eff_w, eff_h)
        } else {
            pixmap.data().to_vec()
        };
        offscreen.data_mut().copy_from_slice(&data);
        Some(data)
    } else {
        None
    };

    // Allocate CMYK buffer for the group if overprint tracking is needed
    let group_cmyk = if has_overprint_elements(elements) || band_state.cmyk_buffer.is_some() {
        let buf_size = eff_w as usize * eff_h as usize * 4;
        let mut buf = vec![0.0f32; buf_size];
        if let Some(ref parent_cmyk) = band_state.cmyk_buffer {
            let parent_stride = ctx.out_w as usize * 4;
            let group_stride = eff_w as usize * 4;
            for gy in 0..eff_h as usize {
                let py = crop_y as usize + gy;
                if py < ctx.out_h as usize {
                    let p_start = py * parent_stride + crop_x as usize * 4;
                    let g_start = gy * group_stride;
                    let copy_len = group_stride.min(parent_stride - crop_x as usize * 4);
                    buf[g_start..g_start + copy_len]
                        .copy_from_slice(&parent_cmyk[p_start..p_start + copy_len]);
                }
            }
        }
        Some(buf)
    } else {
        None
    };

    let mut group_band = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: HashSet::new(),
        mask_pool: Vec::new(),
        cmyk_buffer: group_cmyk,
    };

    let group_ctx = RenderContext {
        vp_x: eff_vp_x,
        vp_y: eff_vp_y,
        scale_x: ctx.scale_x,
        scale_y: ctx.scale_y,
        out_w: eff_w,
        out_h: eff_h,
        effective_dpi: ctx.effective_dpi,
        icc: ctx.icc,
        image_cache: None, // Group elements don't use parent image cache
        elem_idx: 0,
        no_aa: ctx.no_aa,
    };

    for (idx, elem) in elements.elements().iter().enumerate() {
        let elem_ctx = RenderContext {
            elem_idx: idx,
            ..group_ctx
        };
        render_element(&mut offscreen, &mut group_band, elem, &elem_ctx);
    }

    let mut temp_mask = None;
    let mask_ref = resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h);
    let mask_ref = match mask_ref {
        None => return,
        Some(m) => m,
    };

    if let Some(backdrop) = &backdrop {
        composite_non_isolated_group_cropped(
            pixmap, &offscreen, backdrop, params, mask_ref, crop_x, crop_y,
        );
    } else {
        let paint = stet_tiny_skia::PixmapPaint {
            opacity: params.alpha as f32,
            blend_mode: u8_to_blend_mode(params.blend_mode),
            quality: stet_tiny_skia::FilterQuality::Nearest,
        };
        pixmap.draw_pixmap(
            crop_x,
            crop_y,
            offscreen.as_ref(),
            &paint,
            Transform::identity(),
            mask_ref,
        );
    }

    // Write group CMYK buffer back to parent
    if let (Some(group_cmyk), Some(parent_cmyk)) =
        (&group_band.cmyk_buffer, &mut band_state.cmyk_buffer)
    {
        copy_cmyk_buffer_to_parent(
            parent_cmyk,
            group_cmyk,
            offscreen.data(),
            crop_x as usize,
            crop_y as usize,
            eff_w as usize,
            eff_h as usize,
            ctx.out_w as usize,
            ctx.out_h as usize,
        );
    }
}

/// Render a knockout transparency group into a pixmap.
///
/// In a knockout group, each element composites against the group's initial
/// backdrop (not the accumulated result of previous elements).
fn render_knockout_group(
    pixmap: &mut Pixmap,
    band_state: &mut BandState,
    elements: &DisplayList,
    params: &stet_graphics::display_list::GroupParams,
    ctx: &RenderContext<'_>,
) {
    let crop = compute_group_crop(&params.bbox, ctx);

    let (eff_w, eff_h, crop_x, crop_y, eff_vp_x, eff_vp_y) = match crop {
        Some((cx, cy, cw, ch)) => (
            cw,
            ch,
            cx,
            cy,
            ctx.vp_x + cx as f32 / ctx.scale_x,
            ctx.vp_y + cy as f32 / ctx.scale_y,
        ),
        None => (ctx.out_w, ctx.out_h, 0, 0, ctx.vp_x, ctx.vp_y),
    };

    let Some(mut offscreen) = Pixmap::new(eff_w, eff_h) else {
        return;
    };

    let initial_backdrop = if !params.isolated {
        if crop.is_some() {
            copy_backdrop_crop(pixmap, crop_x, crop_y, eff_w, eff_h)
        } else {
            pixmap.data().to_vec()
        }
    } else {
        vec![0u8; (eff_w * eff_h * 4) as usize]
    };

    let Some(mut accumulated) = Pixmap::new(eff_w, eff_h) else {
        return;
    };
    accumulated.data_mut().copy_from_slice(&initial_backdrop);

    // Initial CMYK values for the knockout group
    let needs_cmyk = has_overprint_elements(elements) || band_state.cmyk_buffer.is_some();
    let initial_cmyk = if needs_cmyk {
        let buf_size = eff_w as usize * eff_h as usize * 4;
        let mut buf = vec![0.0f32; buf_size];
        if let Some(ref parent_cmyk) = band_state.cmyk_buffer {
            let parent_stride = ctx.out_w as usize * 4;
            let group_stride = eff_w as usize * 4;
            for gy in 0..eff_h as usize {
                let py = crop_y as usize + gy;
                if py < ctx.out_h as usize {
                    let p_start = py * parent_stride + crop_x as usize * 4;
                    let g_start = gy * group_stride;
                    let copy_len = group_stride.min(parent_stride - crop_x as usize * 4);
                    buf[g_start..g_start + copy_len]
                        .copy_from_slice(&parent_cmyk[p_start..p_start + copy_len]);
                }
            }
        }
        Some(buf)
    } else {
        None
    };

    let mut accumulated_cmyk = initial_cmyk.clone();

    let group_ctx = RenderContext {
        vp_x: eff_vp_x,
        vp_y: eff_vp_y,
        scale_x: ctx.scale_x,
        scale_y: ctx.scale_y,
        out_w: eff_w,
        out_h: eff_h,
        effective_dpi: ctx.effective_dpi,
        icc: ctx.icc,
        image_cache: None,
        elem_idx: 0,
        no_aa: ctx.no_aa,
    };

    for elem in elements.elements() {
        offscreen.data_mut().copy_from_slice(&initial_backdrop);

        let ko_cmyk = initial_cmyk.clone();
        let mut elem_band = BandState {
            clip_region: None,
            spare_mask: None,
            clip_mask_cache: HashMap::new(),
            clip_mask_seen: HashSet::new(),
            mask_pool: Vec::new(),
            cmyk_buffer: ko_cmyk,
        };

        render_element(&mut offscreen, &mut elem_band, elem, &group_ctx);

        if let (Some(elem_cmyk), Some(acc_cmyk)) =
            (&elem_band.cmyk_buffer, &mut accumulated_cmyk)
        {
            replace_changed_cmyk(acc_cmyk, elem_cmyk, offscreen.data(), &initial_backdrop);
        }

        replace_changed_pixels(accumulated.data_mut(), offscreen.data(), &initial_backdrop);
    }

    let mut temp_mask = None;
    let mask_ref = resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h);
    let mask_ref = match mask_ref {
        None => return,
        Some(m) => m,
    };

    composite_non_isolated_group_cropped(
        pixmap, &accumulated, &initial_backdrop, params, mask_ref, crop_x, crop_y,
    );

    if let (Some(acc_cmyk), Some(parent_cmyk)) =
        (&accumulated_cmyk, &mut band_state.cmyk_buffer)
    {
        copy_cmyk_buffer_to_parent(
            parent_cmyk,
            acc_cmyk,
            accumulated.data(),
            crop_x as usize,
            crop_y as usize,
            eff_w as usize,
            eff_h as usize,
            ctx.out_w as usize,
            ctx.out_h as usize,
        );
    }
}
/// Replace pixels in `target` with pixels from `source` wherever `source`
/// differs from `backdrop`. Used for knockout group per-element compositing
/// where each element replaces (not blends with) previous elements.
fn replace_changed_pixels(target: &mut [u8], source: &[u8], backdrop: &[u8]) {
    for i in (0..target.len()).step_by(4) {
        if source[i] != backdrop[i]
            || source[i + 1] != backdrop[i + 1]
            || source[i + 2] != backdrop[i + 2]
            || source[i + 3] != backdrop[i + 3]
        {
            target[i..i + 4].copy_from_slice(&source[i..i + 4]);
        }
    }
}

/// Copy a group's CMYK buffer back to the parent's CMYK buffer after compositing.
/// Only copies values for pixels where the group offscreen has non-zero alpha,
/// indicating the group actually painted something at that position.
#[allow(clippy::too_many_arguments)]
fn copy_cmyk_buffer_to_parent(
    parent_cmyk: &mut [f32],
    group_cmyk: &[f32],
    group_pixels: &[u8],
    crop_x: usize,
    crop_y: usize,
    group_w: usize,
    group_h: usize,
    parent_w: usize,
    parent_h: usize,
) {
    let parent_stride = parent_w * 4;
    let group_stride = group_w * 4;
    for gy in 0..group_h {
        let py = crop_y + gy;
        if py >= parent_h {
            break;
        }
        for gx in 0..group_w {
            let px = crop_x + gx;
            if px >= parent_w {
                break;
            }
            // Only copy if the group pixel has non-zero alpha AND
            // the group's cmyk at that pixel is non-zero.
            // Zero cmyk means "not tracked by a CMYK fill in this group"
            // — writing it back would erase the parent's tracked values.
            let g_pixel_idx = (gy * group_w + gx) * 4;
            let g_cmyk_idx = gy * group_stride + gx * 4;
            if group_pixels[g_pixel_idx + 3] > 0
                && (group_cmyk[g_cmyk_idx] != 0.0
                    || group_cmyk[g_cmyk_idx + 1] != 0.0
                    || group_cmyk[g_cmyk_idx + 2] != 0.0
                    || group_cmyk[g_cmyk_idx + 3] != 0.0)
            {
                let p_cmyk_idx = py * parent_stride + px * 4;
                parent_cmyk[p_cmyk_idx..p_cmyk_idx + 4]
                    .copy_from_slice(&group_cmyk[g_cmyk_idx..g_cmyk_idx + 4]);
            }
        }
    }
}

/// Copy CMYK values for pixels that changed in a knockout element.
/// Used alongside replace_changed_pixels to keep CMYK in sync with RGB.
fn replace_changed_cmyk(
    target_cmyk: &mut [f32],
    source_cmyk: &[f32],
    source_pixels: &[u8],
    backdrop_pixels: &[u8],
) {
    let pixel_count = target_cmyk.len() / 4;
    for i in 0..pixel_count {
        let pi = i * 4;
        if source_pixels[pi] != backdrop_pixels[pi]
            || source_pixels[pi + 1] != backdrop_pixels[pi + 1]
            || source_pixels[pi + 2] != backdrop_pixels[pi + 2]
            || source_pixels[pi + 3] != backdrop_pixels[pi + 3]
        {
            target_cmyk[pi..pi + 4].copy_from_slice(&source_cmyk[pi..pi + 4]);
        }
    }
}

/// Render soft-masked content.
///
/// 1. Renders the mask display list to an offscreen pixmap.
/// 2. Extracts a grayscale mask (luminosity or alpha).
/// 3. Renders content into another offscreen pixmap.
/// 4. Multiplies content alpha by the mask values.
/// 5. Composites the masked content onto the parent.
fn render_soft_masked(
    pixmap: &mut Pixmap,
    band_state: &mut BandState,
    mask_list: &DisplayList,
    content_list: &DisplayList,
    params: &stet_graphics::display_list::SoftMaskParams,
    ctx: &RenderContext<'_>,
) {
    // The SoftMask's display list elements are in absolute device space (page coords).
    // When rendered inside a Group, the parent's viewport may be offset from the
    // SoftMask's bbox. We use the SoftMask's own bbox as the viewport for its
    // offscreen buffers, then composite back at the correct offset in the parent.
    let bbox = &params.bbox;
    let smask_px_x0 = ((bbox[0] as f32 - ctx.vp_x) * ctx.scale_x).floor() as i32;
    let smask_px_y0 = ((bbox[1] as f32 - ctx.vp_y) * ctx.scale_y).floor() as i32;
    let smask_px_x1 = ((bbox[2] as f32 - ctx.vp_x) * ctx.scale_x).ceil() as i32;
    let smask_px_y1 = ((bbox[3] as f32 - ctx.vp_y) * ctx.scale_y).ceil() as i32;

    // Clip to parent output bounds
    let crop_x = smask_px_x0.max(0);
    let crop_y = smask_px_y0.max(0);
    let crop_x1 = smask_px_x1.min(ctx.out_w as i32);
    let crop_y1 = smask_px_y1.min(ctx.out_h as i32);
    if crop_x >= crop_x1 || crop_y >= crop_y1 {
        return;
    }
    let eff_w = (crop_x1 - crop_x) as u32;
    let eff_h = (crop_y1 - crop_y) as u32;

    // Viewport for the offscreen buffers: derived from the SoftMask's bbox position
    // relative to the parent's viewport. This ensures the mask/content display list
    // elements (which are in page device space) render at the correct positions.
    let eff_vp_x = ctx.vp_x + crop_x as f32 / ctx.scale_x;
    let eff_vp_y = ctx.vp_y + crop_y as f32 / ctx.scale_y;

    let sub_ctx = RenderContext {
        vp_x: eff_vp_x,
        vp_y: eff_vp_y,
        scale_x: ctx.scale_x,
        scale_y: ctx.scale_y,
        out_w: eff_w,
        out_h: eff_h,
        effective_dpi: ctx.effective_dpi,
        icc: ctx.icc,
        image_cache: None,
        elem_idx: 0,
        no_aa: ctx.no_aa,
    };

    // 1. Render mask form into offscreen
    let Some(mut mask_pixmap) = Pixmap::new(eff_w, eff_h) else {
        return;
    };
    let mut mask_band = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: HashSet::new(),
        mask_pool: Vec::new(),
        cmyk_buffer: None,
    };
    for (idx, elem) in mask_list.elements().iter().enumerate() {
        let elem_ctx = RenderContext { elem_idx: idx, ..sub_ctx };
        render_element(&mut mask_pixmap, &mut mask_band, elem, &elem_ctx);
    }

    // 2. Extract grayscale mask values
    let pixel_count = (eff_w * eff_h) as usize;
    let mut mask_values = vec![0u8; pixel_count];
    extract_soft_mask_values(mask_pixmap.data(), &mut mask_values, params);

    // 3. Render content into another offscreen
    let Some(mut content_pixmap) = Pixmap::new(eff_w, eff_h) else {
        return;
    };
    let content_cmyk = if has_overprint_elements(content_list) || band_state.cmyk_buffer.is_some() {
        let buf_size = eff_w as usize * eff_h as usize * 4;
        let mut buf = vec![0.0f32; buf_size];
        if let Some(ref parent_cmyk) = band_state.cmyk_buffer {
            let parent_stride = ctx.out_w as usize * 4;
            let group_stride = eff_w as usize * 4;
            for gy in 0..eff_h as usize {
                let py = crop_y as usize + gy;
                if py < ctx.out_h as usize {
                    let p_start = py * parent_stride + crop_x as usize * 4;
                    let g_start = gy * group_stride;
                    let copy_len = group_stride.min(parent_stride - crop_x as usize * 4);
                    buf[g_start..g_start + copy_len]
                        .copy_from_slice(&parent_cmyk[p_start..p_start + copy_len]);
                }
            }
        }
        Some(buf)
    } else {
        None
    };
    let mut content_band = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: HashSet::new(),
        mask_pool: Vec::new(),
        cmyk_buffer: content_cmyk,
    };
    for (idx, elem) in content_list.elements().iter().enumerate() {
        let elem_ctx = RenderContext { elem_idx: idx, ..sub_ctx };
        render_element(&mut content_pixmap, &mut content_band, elem, &elem_ctx);
    }


    // 4. Multiply content RGBA by mask values
    let content_data = content_pixmap.data_mut();
    #[allow(clippy::needless_range_loop)]
    for i in 0..pixel_count {
        let m = mask_values[i] as u16;
        let off = i * 4;
        content_data[off] = ((content_data[off] as u16 * m + 128) / 255) as u8;
        content_data[off + 1] = ((content_data[off + 1] as u16 * m + 128) / 255) as u8;
        content_data[off + 2] = ((content_data[off + 2] as u16 * m + 128) / 255) as u8;
        content_data[off + 3] = ((content_data[off + 3] as u16 * m + 128) / 255) as u8;
    }

    // 5. Composite masked content onto parent with clip
    let mut temp_mask = None;
    let mask_ref = resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h);
    let mask_ref = match mask_ref {
        None => return,
        Some(m) => m,
    };

    let paint = stet_tiny_skia::PixmapPaint {
        opacity: 1.0,
        blend_mode: stet_tiny_skia::BlendMode::SourceOver,
        quality: stet_tiny_skia::FilterQuality::Nearest,
    };
    pixmap.draw_pixmap(
        crop_x,
        crop_y,
        content_pixmap.as_ref(),
        &paint,
        Transform::identity(),
        mask_ref,
    );

    // Write content CMYK buffer back to parent
    if let (Some(content_cmyk), Some(parent_cmyk)) =
        (&content_band.cmyk_buffer, &mut band_state.cmyk_buffer)
    {
        copy_cmyk_buffer_to_parent(
            parent_cmyk,
            content_cmyk,
            content_pixmap.data(),
            crop_x as usize,
            crop_y as usize,
            eff_w as usize,
            eff_h as usize,
            ctx.out_w as usize,
            ctx.out_h as usize,
        );
    }
}
/// Extract grayscale mask values from rendered RGBA pixels.
fn extract_soft_mask_values(
    rgba: &[u8],
    out: &mut [u8],
    params: &stet_graphics::display_list::SoftMaskParams,
) {
    use stet_graphics::display_list::SoftMaskSubtype;
    let pixel_count = out.len();

    match params.subtype {
        SoftMaskSubtype::Alpha => {
            for i in 0..pixel_count {
                out[i] = rgba[i * 4 + 3]; // alpha channel
            }
        }
        SoftMaskSubtype::Luminosity => {
            // Backdrop luminosity for transparent pixels
            let backdrop_lum = if let Some(bc) = &params.backdrop_color {
                (0.2126 * bc[0] + 0.7152 * bc[1] + 0.0722 * bc[2]).clamp(0.0, 1.0)
            } else {
                0.0 // black backdrop
            };
            let backdrop_byte = (backdrop_lum * 255.0 + 0.5) as u8;

            #[allow(clippy::needless_range_loop)]
            for i in 0..pixel_count {
                let off = i * 4;
                let a = rgba[off + 3];
                let lum_byte = if a == 0 {
                    backdrop_byte
                } else {
                    // Un-premultiply to get linear RGB, then compute luminosity
                    let inv_a = 255.0 / a as f64;
                    let r = (rgba[off] as f64 * inv_a).min(255.0);
                    let g = (rgba[off + 1] as f64 * inv_a).min(255.0);
                    let b = (rgba[off + 2] as f64 * inv_a).min(255.0);
                    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    (lum + 0.5).clamp(0.0, 255.0) as u8
                };
                // Apply transfer function inversion: {1 exch sub} → 255 - value
                out[i] = if params.transfer_invert { 255 - lum_byte } else { lum_byte };
            }
        }
    }
}

/// Render a tiled pattern fill.
fn render_pattern_fill(
    pixmap: &mut Pixmap,
    band_state: &mut BandState,
    params: &stet_graphics::device::PatternFillParams,
    ctx: &RenderContext<'_>,
) {
    let mut temp_mask = None;
    let Some(mask_ref) = resolve_clip_mask(&band_state.clip_region, &mut temp_mask, ctx.out_w, ctx.out_h)
    else {
        return;
    };

    let pm = &params.pattern_matrix;

    // Tile step vectors in device space (handles rotation/shear)
    let (step_ux, step_uy) = pm.transform_delta(params.xstep, 0.0);
    let (step_vx, step_vy) = pm.transform_delta(0.0, params.ystep);

    let step_u_len = (step_ux * step_ux + step_uy * step_uy).sqrt();
    let step_v_len = (step_vx * step_vx + step_vy * step_vy).sqrt();
    if step_u_len < 0.01 || step_v_len < 0.01 {
        return;
    }

    let origin_x = pm.tx;
    let origin_y = pm.ty;

    // Viewport bounds in device space
    let dev_vp_x = ctx.vp_x as f64;
    let dev_vp_y = ctx.vp_y as f64;
    let dev_vp_w = ctx.out_w as f64 / ctx.scale_x as f64;
    let dev_vp_h = ctx.out_h as f64 / ctx.scale_y as f64;

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

    let tile_count = (tile_x_end - tile_x_start) as i64 * (tile_y_end - tile_y_start) as i64;
    if tile_count > 10000 {
        return;
    }

    let Some(mut tile_buf) = Pixmap::new(ctx.out_w, ctx.out_h) else {
        return;
    };

    let sx_f = ctx.scale_x as f64;
    let sy_f = ctx.scale_y as f64;

    for tv in tile_y_start..tile_y_end {
        for tu in tile_x_start..tile_x_end {
            let pat_offset_x = tu as f64 * params.xstep;
            let pat_offset_y = tv as f64 * params.ystep;

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
                            paint.anti_alias = false;
                            let t = to_transform(&fp.ctm);
                            let combined = tile_transform.post_concat(t);
                            let fr = to_fill_rule(&fp.fill_rule);
                            tile_buf.fill_path(&sp, &paint, fr, combined, None);
                        }
                    }
                    DisplayElement::Stroke { path, params: sp } => {
                        if let Some(skp) = build_skia_path(path) {
                            let stroke = build_stroke(sp, ctx.effective_dpi);
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

    // Composite tile_buf onto main pixmap through the fill path
    let Some(fill_skia_path) = build_skia_path(&params.path) else {
        return;
    };
    let fill_rule = to_fill_rule(&params.fill_rule);
    let mut fill_mask = Mask::new(ctx.out_w, ctx.out_h).expect("mask");
    let path_transform = viewport_transform(
        Transform::identity(),
        ctx.vp_x,
        ctx.vp_y,
        ctx.scale_x,
        ctx.scale_y,
    );
    fill_mask.fill_path(&fill_skia_path, fill_rule, !ctx.no_aa, path_transform);

    if let Some(clip_mask) = mask_ref {
        intersect_masks(&mut fill_mask, clip_mask);
    }

    let img_paint = stet_tiny_skia::PixmapPaint::default();
    pixmap.draw_pixmap(
        0,
        0,
        tile_buf.as_ref(),
        &img_paint,
        Transform::identity(),
        Some(&fill_mask),
    );
}

/// Unified clip path handling for both band and viewport rendering.
///
/// For band rendering (scale=1.0), includes rect fast-path and Y-bbox early exit.
/// For viewport rendering (scale!=1.0), uses the general mask path.
fn clip_path_unified(
    band_state: &mut BandState,
    path: &PsPath,
    params: &ClipParams,
    ctx: &RenderContext<'_>,
) {
    let is_unit_scale = ctx.scale_x == 1.0 && ctx.scale_y == 1.0;

    // Band-mode optimizations (scale=1.0): Y-bbox early exit and rect fast-path
    if is_unit_scale {
        let y_start = ctx.vp_y as u32;
        let x_start = ctx.vp_x as u32;

        // Y-bbox early exit: if clip path doesn't overlap this band, set empty clip
        if x_start == 0
            && let Some(bbox) = path_y_bbox(path)
                && (bbox.y_max <= y_start as f64 || bbox.y_min >= (y_start + ctx.out_h) as f64)
            {
                if let Some(ClipRegion::Mask(mask)) = band_state.clip_region.take() {
                    band_state.recycle_mask(mask);
                }
                band_state.clip_region = Some(ClipRegion::Rect(ClipRect {
                    x0: 0, y0: 0, x1: 0, y1: 0,
                }));
                return;
            }

        // Rect fast-path (only when x_start==0 for simplicity)
        if x_start == 0
            && let Some(dev_rect) = detect_rect(path, ctx.out_w, u32::MAX) {
                let new_rect = translate_clip_rect(&dev_rect, y_start, ctx.out_h);
                match band_state.clip_region.take() {
                    None => {
                        band_state.clip_region = Some(ClipRegion::Rect(new_rect));
                    }
                    Some(ClipRegion::Rect(existing)) => {
                        band_state.clip_region = Some(ClipRegion::Rect(existing.intersect(&new_rect)));
                    }
                    Some(ClipRegion::Mask(mut mask)) => {
                        intersect_mask_with_rect(&mut mask, &new_rect, ctx.out_w, ctx.out_h);
                        band_state.clip_region = Some(ClipRegion::Mask(mask));
                    }
                }
                return;
            }
    }

    // General path: non-rectangular clip with cache + mask reuse
    let fill_rule = to_fill_rule(&params.fill_rule);
    let path_hash = hash_clip_path(path, &params.fill_rule);
    let prev_region = band_state.clip_region.take();

    let mut mask = band_state.take_mask(ctx.out_w, ctx.out_h);

    let path_mask = if let Some(cached) = band_state.clip_mask_cache.get(&path_hash) {
        mask.data_mut().copy_from_slice(cached.data());
        mask
    } else {
        let Some(skia_path) = build_skia_path(path) else {
            band_state.recycle_mask(mask);
            band_state.clip_region = prev_region;
            return;
        };
        let transform = ctx.transform(&params.ctm);
        mask.data_mut().fill(0);
        mask.fill_path(&skia_path, fill_rule, false, transform);
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
                intersect_mask_with_rect(&mut mask, &rect, ctx.out_w, ctx.out_h);
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

        let paint = to_paint_alpha(&params.color, params.alpha, params.blend_mode, self.no_aa);
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
            adjusted = stroke_adjust_path_viewport(path, stroke.width as f64, 1.0, 1.0, 0.0, 0.0);
            &adjusted
        } else {
            path
        };
        let Some(skia_path) = build_skia_path(draw_path) else {
            return;
        };
        let paint = to_paint_alpha(&params.color, params.alpha, params.blend_mode, self.no_aa);
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
        // Composite onto white background before output
        composite_onto_white(self.pixmap.data_mut());
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

        let Some(img_pixmap) = stet_tiny_skia::PixmapRef::from_bytes(img_data, img_w, img_h) else {
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
            stet_tiny_skia::FilterQuality::Nearest
        } else {
            stet_tiny_skia::FilterQuality::Bilinear
        };
        let paint = stet_tiny_skia::PixmapPaint {
            quality,
            opacity: params.alpha as f32,
            blend_mode: u8_to_blend_mode(params.blend_mode),
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
        render_axial_shading(&mut self.pixmap, params, 0.0, 0.0, 1.0, 1.0, mask_ref, self.no_aa, None, None);
    }

    fn paint_radial_shading(&mut self, params: &RadialShadingParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return;
        };
        render_radial_shading(&mut self.pixmap, params, 0.0, 0.0, 1.0, 1.0, mask_ref, self.no_aa, None, None);
    }

    fn paint_mesh_shading(&mut self, params: &MeshShadingParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return;
        };
        render_mesh_shading(&mut self.pixmap, params, 0.0, 0.0, 1.0, 1.0, mask_ref, None, None);
    }

    fn paint_patch_shading(&mut self, params: &PatchShadingParams) {
        self.ensure_full_pixmap();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut temp_mask = None;
        let Some(mask_ref) = resolve_clip_mask(&self.clip_region, &mut temp_mask, w, h) else {
            return;
        };
        render_patch_shading(&mut self.pixmap, params, 0.0, 0.0, 1.0, 1.0, mask_ref, None, None);
    }

    fn paint_pattern_fill(&mut self, params: &stet_graphics::device::PatternFillParams) {
        self.ensure_full_pixmap();
        let w = self.pixmap.width();
        let h = self.pixmap.height();
        let mut band_state = BandState {
            clip_region: self.clip_region.take(),
            spare_mask: self.spare_mask.take(),
            clip_mask_cache: HashMap::new(),
            clip_mask_seen: HashSet::new(),
            mask_pool: Vec::new(),
            cmyk_buffer: None,
        };
        {
            let ctx = RenderContext {
                vp_x: 0.0,
                vp_y: 0.0,
                scale_x: 1.0,
                scale_y: 1.0,
                out_w: w,
                out_h: h,
                effective_dpi: self.dpi,
                icc: None,
                image_cache: None,
                elem_idx: 0,
                no_aa: self.no_aa,
            };
            render_pattern_fill(&mut self.pixmap, &mut band_state, params, &ctx);
        }
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

        // If banding not worthwhile, render the full page as a single band.
        // This still uses render_element (same as banded path) so that Group
        // and SoftMasked elements get proper offscreen compositing.
        if band_h >= page_h {
            self.ensure_full_pixmap();
            let ctx = RenderContext {
                vp_x: 0.0,
                vp_y: 0.0,
                scale_x: 1.0,
                scale_y: 1.0,
                out_w: page_w,
                out_h: page_h,
                effective_dpi: self.dpi,
                icc: Some(&icc_cache),
                image_cache: None,
                elem_idx: 0,
                no_aa: self.no_aa,
            };
            let mut band_state = BandState {
                clip_region: None,
                spare_mask: None,
                clip_mask_cache: HashMap::new(),
                clip_mask_seen: HashSet::new(),
                mask_pool: Vec::new(),
                cmyk_buffer: None,
            };
            for (idx, elem) in list.elements().iter().enumerate() {
                let elem_ctx = RenderContext { elem_idx: idx, ..ctx };
                render_element(&mut self.pixmap, &mut band_state, elem, &elem_ctx);
            }
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
            let no_aa = self.no_aa;
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            rayon::spawn(move || {
                let result = render_banded_to_sink(
                    page_w, page_h, band_h, dpi, &list, &mut *sink, &icc_cache, no_aa,
                );
                let _ = tx.send(result);
            });
            self.pending_render = Some(rx);
        }
        #[cfg(not(feature = "parallel"))]
        {
            render_banded_to_sink(page_w, page_h, band_h, dpi, &list, &mut *sink, &icc_cache, self.no_aa)?;
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

/// Scan a display list for any overprint fill/stroke elements that need CMYK simulation.
fn has_overprint_elements(list: &DisplayList) -> bool {
    for elem in list.elements() {
        match elem {
            DisplayElement::Fill { params, .. } => {
                if params.overprint {
                    return true;
                }
            }
            DisplayElement::Stroke { params, .. } => {
                if params.overprint {
                    return true;
                }
            }
            DisplayElement::Image { params, .. } => {
                if params.overprint {
                    return true;
                }
            }
            DisplayElement::AxialShading { params } => {
                if params.overprint {
                    return true;
                }
            }
            DisplayElement::RadialShading { params } => {
                if params.overprint {
                    return true;
                }
            }
            DisplayElement::MeshShading { params } => {
                if params.overprint {
                    return true;
                }
            }
            DisplayElement::PatchShading { params } => {
                if params.overprint {
                    return true;
                }
            }
            DisplayElement::Group { elements, .. } => {
                if has_overprint_elements(elements) {
                    return true;
                }
            }
            DisplayElement::SoftMasked {
                content, mask, ..
            } => {
                if has_overprint_elements(content) || has_overprint_elements(mask) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}


/// Render an overprint fill: rasterize path to coverage mask, then composite
/// at the CMYK level, converting the result to RGB for the pixmap.
#[allow(clippy::too_many_arguments)]
fn render_overprint_fill(
    pixmap: &mut Pixmap,
    cmyk_buf: &mut [f32],
    band_state: &mut BandState,
    path: &PsPath,
    params: &FillParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    out_w: u32,
    out_h: u32,
    icc: Option<&IccCache>,
    no_aa: bool,
) {
    let Some(skia_path) = build_skia_path(path) else {
        return;
    };
    let fill_rule = to_fill_rule(&params.fill_rule);

    let mut coverage_mask = match Mask::new(out_w, out_h) {
        Some(m) => m,
        None => return,
    };
    let transform = viewport_transform(to_transform(&params.ctm), vp_x, vp_y, scale_x, scale_y);
    coverage_mask.fill_path(&skia_path, fill_rule, !no_aa, transform);

    // Intersect with clip mask
    let clip_coverage: Option<&[u8]> = match &band_state.clip_region {
        None => None,
        Some(ClipRegion::Rect(r)) => {
            let data = coverage_mask.data_mut();
            let stride = out_w as usize;
            for y in 0..out_h {
                let row_start = y as usize * stride;
                for x in 0..out_w {
                    if y < r.y0 || y >= r.y1 || x < r.x0 || x >= r.x1 {
                        data[row_start + x as usize] = 0;
                    }
                }
            }
            None
        }
        Some(ClipRegion::Mask(clip_mask)) => Some(clip_mask.data()),
    };

    let (src_c, src_m, src_y, src_k) = params.color.native_cmyk.unwrap_or_else(|| {
        let r = params.color.r;
        let g = params.color.g;
        let b = params.color.b;
        (1.0 - r, 1.0 - g, 1.0 - b, 0.0)
    });

    let mut channels = params.painted_channels;
    // Non-CMYK fills (painted_channels=0, e.g. Separation spot colors, RGB, Gray)
    // replace all color at each pixel — update all CMYK channels to keep buffer in sync.
    if channels == 0 {
        channels = stet_graphics::device::CMYK_ALL;
    }
    // OPM 1 per-pixel zero filtering only applies to DeviceCMYK, not DeviceN/Separation
    if params.overprint_mode == 1
        && channels == stet_graphics::device::CMYK_ALL
        && params.is_device_cmyk
    {
        channels = 0;
        if src_c != 0.0 { channels |= stet_graphics::device::CMYK_C; }
        if src_m != 0.0 { channels |= stet_graphics::device::CMYK_M; }
        if src_y != 0.0 { channels |= stet_graphics::device::CMYK_Y; }
        if src_k != 0.0 { channels |= stet_graphics::device::CMYK_K; }
    }


    if channels == stet_graphics::device::CMYK_ALL {
        let cov_data = coverage_mask.data();
        let stride = out_w as usize;
        for y in 0..out_h as usize {
            for x in 0..out_w as usize {
                let mi = y * stride + x;
                let mut cov = cov_data[mi] as f32 / 255.0;
                if let Some(clip) = clip_coverage {
                    cov *= clip[mi] as f32 / 255.0;
                }
                if cov > 0.0 {
                    let ci = (y * stride + x) * 4;
                    cmyk_buf[ci] = src_c as f32;
                    cmyk_buf[ci + 1] = src_m as f32;
                    cmyk_buf[ci + 2] = src_y as f32;
                    cmyk_buf[ci + 3] = src_k as f32;
                }
            }
        }
        let mut temp_mask = None;
        let Some(mask_ref) =
            resolve_clip_mask(&band_state.clip_region, &mut temp_mask, out_w, out_h)
        else {
            return;
        };
        let paint = to_paint_alpha(&params.color, params.alpha, params.blend_mode, no_aa);
        pixmap.fill_path(&skia_path, &paint, fill_rule, transform, mask_ref);
        return;
    }

    let cov_data = coverage_mask.data();
    let stride = out_w as usize;
    let px_data = pixmap.data_mut();
    let px_stride = out_w as usize * 4;

    for y in 0..out_h as usize {
        for x in 0..out_w as usize {
            let mi = y * stride + x;
            let mut cov = cov_data[mi] as f32 / 255.0;
            if let Some(clip) = clip_coverage {
                cov *= clip[mi] as f32 / 255.0;
            }
            if cov <= 0.0 {
                continue;
            }

            let ci = mi * 4;
            let pi = y * px_stride + x * 4;
            let (cur_c, cur_m, cur_y, cur_k) = {
                let c = cmyk_buf[ci] as f64;
                let m = cmyk_buf[ci + 1] as f64;
                let y_val = cmyk_buf[ci + 2] as f64;
                let k = cmyk_buf[ci + 3] as f64;
                if c == 0.0 && m == 0.0 && y_val == 0.0 && k == 0.0 && px_data[pi + 3] > 0 {
                    let r = px_data[pi] as f64 / 255.0;
                    let g = px_data[pi + 1] as f64 / 255.0;
                    let b = px_data[pi + 2] as f64 / 255.0;
                    let rk = 1.0 - r.max(g).max(b);
                    if rk >= 1.0 {
                        (0.0, 0.0, 0.0, 1.0)
                    } else {
                        let inv = 1.0 / (1.0 - rk);
                        ((1.0 - r - rk) * inv, (1.0 - g - rk) * inv, (1.0 - b - rk) * inv, rk)
                    }
                } else {
                    (c, m, y_val, k)
                }
            };

            let new_c = if channels & stet_graphics::device::CMYK_C != 0 { src_c } else { cur_c };
            let new_m = if channels & stet_graphics::device::CMYK_M != 0 { src_m } else { cur_m };
            let new_y = if channels & stet_graphics::device::CMYK_Y != 0 { src_y } else { cur_y };
            let new_k = if channels & stet_graphics::device::CMYK_K != 0 { src_k } else { cur_k };

            if (new_c as f32 - cmyk_buf[ci]).abs() < 1e-6
                && (new_m as f32 - cmyk_buf[ci + 1]).abs() < 1e-6
                && (new_y as f32 - cmyk_buf[ci + 2]).abs() < 1e-6
                && (new_k as f32 - cmyk_buf[ci + 3]).abs() < 1e-6
            {
                continue;
            }

            cmyk_buf[ci] = new_c as f32;
            cmyk_buf[ci + 1] = new_m as f32;
            cmyk_buf[ci + 2] = new_y as f32;
            cmyk_buf[ci + 3] = new_k as f32;

            let (r, g, b) = if let Some(icc_cache) = icc {
                icc_cache
                    .convert_cmyk_readonly(new_c, new_m, new_y, new_k)
                    .unwrap_or_else(|| cmyk_to_rgb_plrm(new_c, new_m, new_y, new_k))
            } else {
                cmyk_to_rgb_plrm(new_c, new_m, new_y, new_k)
            };

            let a = (cov * params.alpha as f32).min(1.0);
            let dst_a = px_data[pi + 3] as f32 / 255.0;
            let out_a = a + dst_a * (1.0 - a);
            if out_a > 0.0 {
                let inv = 1.0 / out_a;
                px_data[pi] = ((r as f32 * a + px_data[pi] as f32 / 255.0 * dst_a * (1.0 - a)) * inv * 255.0).round() as u8;
                px_data[pi + 1] = ((g as f32 * a + px_data[pi + 1] as f32 / 255.0 * dst_a * (1.0 - a)) * inv * 255.0).round() as u8;
                px_data[pi + 2] = ((b as f32 * a + px_data[pi + 2] as f32 / 255.0 * dst_a * (1.0 - a)) * inv * 255.0).round() as u8;
                px_data[pi + 3] = (out_a * 255.0).round() as u8;
            }
        }
    }
}
/// PLRM CMYK-to-RGB formula fallback.
fn cmyk_to_rgb_plrm(c: f64, m: f64, y: f64, k: f64) -> (f64, f64, f64) {
    (
        1.0 - (c + k).min(1.0),
        1.0 - (m + k).min(1.0),
        1.0 - (y + k).min(1.0),
    )
}

/// Update the CMYK buffer for a non-overprint fill (to track backdrop for future overprints).
#[allow(clippy::too_many_arguments)]
fn update_cmyk_buffer_for_fill(
    cmyk_buf: &mut [f32],
    path: &PsPath,
    params: &FillParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    out_w: u32,
    out_h: u32,
    clip_region: &Option<ClipRegion>,
    no_aa: bool,
) {
    let Some((src_c, src_m, src_y, src_k)) = params.color.native_cmyk else {
        return;
    };
    let Some(skia_path) = build_skia_path(path) else {
        return;
    };

    let mut coverage_mask = match Mask::new(out_w, out_h) {
        Some(m) => m,
        None => return,
    };
    let transform = viewport_transform(to_transform(&params.ctm), vp_x, vp_y, scale_x, scale_y);
    let fill_rule = to_fill_rule(&params.fill_rule);
    coverage_mask.fill_path(&skia_path, fill_rule, !no_aa, transform);

    let cov_data = coverage_mask.data();
    let clip_data: Option<&[u8]> = match clip_region {
        Some(ClipRegion::Mask(m)) => Some(m.data()),
        _ => None,
    };
    let clip_rect = match clip_region {
        Some(ClipRegion::Rect(r)) => Some(*r),
        _ => None,
    };

    let stride = out_w as usize;
    for y in 0..out_h as usize {
        for x in 0..out_w as usize {
            let mi = y * stride + x;
            let mut cov = cov_data[mi] as f32 / 255.0;
            if let Some(clip) = clip_data {
                cov *= clip[mi] as f32 / 255.0;
            }
            if let Some(r) = clip_rect
                && ((y as u32) < r.y0 || (y as u32) >= r.y1 || (x as u32) < r.x0 || (x as u32) >= r.x1) {
                    cov = 0.0;
                }
            if cov > 0.0 {
                let ci = mi * 4;
                cmyk_buf[ci] = src_c as f32;
                cmyk_buf[ci + 1] = src_m as f32;
                cmyk_buf[ci + 2] = src_y as f32;
                cmyk_buf[ci + 3] = src_k as f32;
            }
        }
    }
}

/// Render an overprint image with viewport params.
#[allow(clippy::too_many_arguments)]
fn render_overprint_image(
    pixmap: &mut Pixmap,
    cmyk_buf: &mut [f32],
    band_state: &mut BandState,
    sample_data: &[u8],
    params: &ImageParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    out_w: u32,
    out_h: u32,
    icc: Option<&IccCache>,
) {
    let iw = params.width as usize;
    let ih = params.height as usize;
    let Some(image_inv) = params.image_matrix.invert() else {
        return;
    };
    let combined = params.ctm.concat(&image_inv);
    let Some(inv_combined) = combined.invert() else {
        return;
    };

    let px_data = pixmap.data_mut();
    let stride = out_w as usize;
    let inv_sx = 1.0 / scale_x as f64;
    let inv_sy = 1.0 / scale_y as f64;

    let clip_data: Option<&[u8]> = match &band_state.clip_region {
        Some(ClipRegion::Mask(m)) => Some(m.data()),
        _ => None,
    };
    let clip_rect = match &band_state.clip_region {
        Some(ClipRegion::Rect(r)) => Some(*r),
        _ => None,
    };

    let mask_info = if let ImageColorSpace::Mask { color, polarity } = &params.color_space {
        let (src_c, src_m, src_y, src_k) = color.native_cmyk.unwrap_or_else(|| {
            let r = color.r;
            let g = color.g;
            let b = color.b;
            (1.0 - r, 1.0 - g, 1.0 - b, 0.0)
        });
        Some((src_c, src_m, src_y, src_k, *polarity, iw.div_ceil(8)))
    } else {
        None
    };

    for by in 0..out_h as usize {
        for bx in 0..out_w as usize {
            if let Some(ref r) = clip_rect
                && ((by as u32) < r.y0 || (by as u32) >= r.y1
                    || (bx as u32) < r.x0 || (bx as u32) >= r.x1)
                {
                    continue;
                }
            if let Some(clip) = clip_data {
                let ci_clip = by * stride + bx;
                if clip[ci_clip] == 0 {
                    let bh = out_h as usize;
                    let has_neighbor = (bx > 0 && clip[ci_clip - 1] != 0)
                        || (bx + 1 < stride && clip[ci_clip + 1] != 0)
                        || (by > 0 && clip[ci_clip - stride] != 0)
                        || (by + 1 < bh && clip[ci_clip + stride] != 0)
                        || (bx > 0 && by > 0 && clip[ci_clip - stride - 1] != 0)
                        || (bx + 1 < stride && by > 0 && clip[ci_clip - stride + 1] != 0)
                        || (bx > 0 && by + 1 < bh && clip[ci_clip + stride - 1] != 0)
                        || (bx + 1 < stride && by + 1 < bh && clip[ci_clip + stride + 1] != 0);
                    if !has_neighbor {
                        continue;
                    }
                }
            }

            // Map output pixel to device space, then to image space
            let dx = (bx as f64 + 0.5) * inv_sx + vp_x as f64;
            let dy = (by as f64 + 0.5) * inv_sy + vp_y as f64;
            let ix = inv_combined.a * dx + inv_combined.c * dy + inv_combined.tx;
            let iy = inv_combined.b * dx + inv_combined.d * dy + inv_combined.ty;

            let col = ix.floor() as i64;
            let row = iy.floor() as i64;
            if col < 0 || col >= iw as i64 || row < 0 || row >= ih as i64 {
                continue;
            }
            let col = col as usize;
            let row = row as usize;

            let (src_c, src_m, src_y, src_k) =
                if let Some((mc, mm, my, mk, polarity, bytes_per_row)) = mask_info {
                    let byte_idx = row * bytes_per_row + col / 8;
                    let bit_offset = 7 - (col % 8);
                    let bit = if byte_idx < sample_data.len() {
                        (sample_data[byte_idx] >> bit_offset) & 1
                    } else {
                        0
                    };
                    let paint = if polarity { bit == 1 } else { bit == 0 };
                    if !paint { continue; }
                    (mc, mm, my, mk)
                } else if let Some(cmyk) =
                    sample_pixel_cmyk(sample_data, &params.color_space, iw, row, col)
                {
                    cmyk
                } else {
                    continue;
                };

            let mut channels = params.painted_channels;
            // Non-CMYK images (painted_channels=0, e.g. Separation/DeviceN spot colors)
            // replace all CMYK channels with the tinted equivalent.
            if channels == 0 {
                channels = stet_graphics::device::CMYK_ALL;
            }
            let is_direct_cmyk = matches!(
                &params.color_space,
                ImageColorSpace::DeviceCMYK
                    | ImageColorSpace::ICCBased { n: 4, .. }
                    | ImageColorSpace::Mask { .. }
            );
            if params.overprint_mode == 1
                && channels == stet_graphics::device::CMYK_ALL
                && is_direct_cmyk
            {
                channels = 0;
                if src_c != 0.0 { channels |= stet_graphics::device::CMYK_C; }
                if src_m != 0.0 { channels |= stet_graphics::device::CMYK_M; }
                if src_y != 0.0 { channels |= stet_graphics::device::CMYK_Y; }
                if src_k != 0.0 { channels |= stet_graphics::device::CMYK_K; }
            }

            let mi = by * stride + bx;
            let ci = mi * 4;
            let pi = mi * 4;

            let (cur_c, cur_m, cur_y, cur_k) = {
                let c = cmyk_buf[ci] as f64;
                let m = cmyk_buf[ci + 1] as f64;
                let y_val = cmyk_buf[ci + 2] as f64;
                let k = cmyk_buf[ci + 3] as f64;
                if c == 0.0 && m == 0.0 && y_val == 0.0 && k == 0.0 && px_data[pi + 3] > 0 {
                    let r = px_data[pi] as f64 / 255.0;
                    let g = px_data[pi + 1] as f64 / 255.0;
                    let b = px_data[pi + 2] as f64 / 255.0;
                    let rk = 1.0 - r.max(g).max(b);
                    if rk >= 1.0 { (0.0, 0.0, 0.0, 1.0) }
                    else {
                        let inv = 1.0 / (1.0 - rk);
                        ((1.0 - r - rk) * inv, (1.0 - g - rk) * inv, (1.0 - b - rk) * inv, rk)
                    }
                } else { (c, m, y_val, k) }
            };

            let new_c = if channels & stet_graphics::device::CMYK_C != 0 { src_c } else { cur_c };
            let new_m = if channels & stet_graphics::device::CMYK_M != 0 { src_m } else { cur_m };
            let new_y = if channels & stet_graphics::device::CMYK_Y != 0 { src_y } else { cur_y };
            let new_k = if channels & stet_graphics::device::CMYK_K != 0 { src_k } else { cur_k };

            cmyk_buf[ci] = new_c as f32;
            cmyk_buf[ci + 1] = new_m as f32;
            cmyk_buf[ci + 2] = new_y as f32;
            cmyk_buf[ci + 3] = new_k as f32;

            let (r, g, b) = if let Some(icc_cache) = icc {
                icc_cache
                    .convert_cmyk_readonly(new_c, new_m, new_y, new_k)
                    .unwrap_or_else(|| cmyk_to_rgb_plrm(new_c, new_m, new_y, new_k))
            } else {
                cmyk_to_rgb_plrm(new_c, new_m, new_y, new_k)
            };

            px_data[pi] = (r * 255.0).round() as u8;
            px_data[pi + 1] = (g * 255.0).round() as u8;
            px_data[pi + 2] = (b * 255.0).round() as u8;
            px_data[pi + 3] = 255;
        }
    }
}

/// Update CMYK buffer for a non-overprint image.
#[allow(clippy::too_many_arguments)]
fn update_cmyk_buffer_for_image(
    cmyk_buf: &mut [f32],
    sample_data: &[u8],
    params: &ImageParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    out_w: u32,
    out_h: u32,
    clip_region: &Option<ClipRegion>,
) {
    let iw = params.width as usize;
    let ih = params.height as usize;
    let Some(image_inv) = params.image_matrix.invert() else { return; };
    let combined = params.ctm.concat(&image_inv);
    let Some(inv_combined) = combined.invert() else { return; };
    let stride = out_w as usize;
    let inv_sx = 1.0 / scale_x as f64;
    let inv_sy = 1.0 / scale_y as f64;

    let mask_info = if let ImageColorSpace::Mask { color, polarity } = &params.color_space {
        let Some((c, m, y, k)) = color.native_cmyk else { return; };
        Some((c as f32, m as f32, y as f32, k as f32, *polarity, iw.div_ceil(8)))
    } else if !is_cmyk_color_space(&params.color_space) {
        return;
    } else {
        None
    };

    let clip_data: Option<&[u8]> = match clip_region {
        Some(ClipRegion::Mask(m)) => Some(m.data()),
        _ => None,
    };
    let clip_rect = match clip_region {
        Some(ClipRegion::Rect(r)) => Some(*r),
        _ => None,
    };

    for by in 0..out_h as usize {
        for bx in 0..out_w as usize {
            if let Some(ref r) = clip_rect
                && ((by as u32) < r.y0 || (by as u32) >= r.y1
                    || (bx as u32) < r.x0 || (bx as u32) >= r.x1) { continue; }
            if let Some(clip) = clip_data
                && clip[by * stride + bx] == 0 { continue; }

            let dx = (bx as f64 + 0.5) * inv_sx + vp_x as f64;
            let dy = (by as f64 + 0.5) * inv_sy + vp_y as f64;
            let ix = inv_combined.a * dx + inv_combined.c * dy + inv_combined.tx;
            let iy = inv_combined.b * dx + inv_combined.d * dy + inv_combined.ty;

            let col = ix.floor() as i64;
            let row = iy.floor() as i64;
            if col < 0 || col >= iw as i64 || row < 0 || row >= ih as i64 { continue; }
            let col = col as usize;
            let row = row as usize;

            let ci = (by * stride + bx) * 4;
            if let Some((sc, sm, sy, sk, polarity, bytes_per_row)) = mask_info {
                let byte_idx = row * bytes_per_row + col / 8;
                let bit_offset = 7 - (col % 8);
                let bit = if byte_idx < sample_data.len() {
                    (sample_data[byte_idx] >> bit_offset) & 1
                } else { 0 };
                let paint = if polarity { bit == 1 } else { bit == 0 };
                if paint {
                    cmyk_buf[ci] = sc;
                    cmyk_buf[ci + 1] = sm;
                    cmyk_buf[ci + 2] = sy;
                    cmyk_buf[ci + 3] = sk;
                }
            } else if let Some((sc, sm, sy, sk)) =
                sample_pixel_cmyk(sample_data, &params.color_space, iw, row, col)
            {
                cmyk_buf[ci] = sc as f32;
                cmyk_buf[ci + 1] = sm as f32;
                cmyk_buf[ci + 2] = sy as f32;
                cmyk_buf[ci + 3] = sk as f32;
            }
        }
    }
}
/// Check if an image color space can be rendered through the overprint path.
/// Image masks always work (they use the fill color's native CMYK).
/// Other color spaces must be CMYK-resolvable via `sample_pixel_cmyk`.
fn image_supports_overprint(cs: &ImageColorSpace) -> bool {
    match cs {
        ImageColorSpace::Mask { .. } => true,
        ImageColorSpace::DeviceCMYK | ImageColorSpace::ICCBased { n: 4, .. } => true,
        ImageColorSpace::Separation { alt_space, .. } | ImageColorSpace::DeviceN { alt_space, .. } => {
            matches!(alt_space.as_ref(), ImageColorSpace::DeviceCMYK | ImageColorSpace::ICCBased { n: 4, .. })
        }
        ImageColorSpace::Indexed { base, .. } => image_supports_overprint(base),
        _ => false,
    }
}

/// Check if an image color space is CMYK-based (DeviceCMYK, ICCBased 4-component, or Indexed over CMYK).
fn is_cmyk_color_space(cs: &ImageColorSpace) -> bool {
    match cs {
        ImageColorSpace::DeviceCMYK => true,
        ImageColorSpace::ICCBased { n: 4, .. } => true,
        ImageColorSpace::Indexed { base, .. } => is_cmyk_color_space(base),
        _ => false,
    }
}

/// Sample a single pixel's CMYK values from image data, handling DeviceCMYK,
/// ICCBased(4), Separation/DeviceN with CMYK alt, and Indexed color spaces.
/// Returns None for non-CMYK images.
fn sample_pixel_cmyk(
    sample_data: &[u8],
    cs: &ImageColorSpace,
    iw: usize,
    row: usize,
    col: usize,
) -> Option<(f64, f64, f64, f64)> {
    match cs {
        ImageColorSpace::DeviceCMYK | ImageColorSpace::ICCBased { n: 4, .. } => {
            let si = (row * iw + col) * 4;
            if si + 3 < sample_data.len() {
                Some((
                    sample_data[si] as f64 / 255.0,
                    sample_data[si + 1] as f64 / 255.0,
                    sample_data[si + 2] as f64 / 255.0,
                    sample_data[si + 3] as f64 / 255.0,
                ))
            } else {
                None
            }
        }
        ImageColorSpace::Separation { alt_space, tint_table, .. } => {
            if !matches!(alt_space.as_ref(), ImageColorSpace::DeviceCMYK | ImageColorSpace::ICCBased { n: 4, .. }) { return None; }
            let si = row * iw + col;
            if si >= sample_data.len() { return None; }
            let tint = sample_data[si] as f32 / 255.0;
            let mut alt = [0.0f32; 4];
            tint_table.lookup_1d(tint, &mut alt);
            Some((alt[0] as f64, alt[1] as f64, alt[2] as f64, alt[3] as f64))
        }
        ImageColorSpace::DeviceN { alt_space, tint_table, .. } => {
            if !matches!(alt_space.as_ref(), ImageColorSpace::DeviceCMYK | ImageColorSpace::ICCBased { n: 4, .. }) { return None; }
            let ni = tint_table.num_inputs as usize;
            let si = (row * iw + col) * ni;
            if si + ni > sample_data.len() { return None; }
            let mut inputs = vec![0.0f32; ni];
            for (c, inp) in inputs.iter_mut().enumerate() {
                *inp = sample_data[si + c] as f32 / 255.0;
            }
            let mut alt = [0.0f32; 4];
            tint_table.lookup_nd(&inputs, &mut alt);
            Some((alt[0] as f64, alt[1] as f64, alt[2] as f64, alt[3] as f64))
        }
        ImageColorSpace::Indexed { base, hival, lookup } => {
            let pi = row * iw + col;
            if pi >= sample_data.len() {
                return None;
            }
            let idx = sample_data[pi] as usize;
            let idx = idx.min(*hival as usize);
            let base_ncomp = base.num_components() as usize;
            let li = idx * base_ncomp;
            // For direct CMYK base (4 components): read CMYK from lookup table
            if is_cmyk_color_space(base) && base_ncomp == 4 && li + 3 < lookup.len() {
                return Some((
                    lookup[li] as f64 / 255.0,
                    lookup[li + 1] as f64 / 255.0,
                    lookup[li + 2] as f64 / 255.0,
                    lookup[li + 3] as f64 / 255.0,
                ));
            }
            // For Separation/DeviceN base: extract base components from lookup, then tint
            if li + base_ncomp <= lookup.len() {
                match base.as_ref() {
                    ImageColorSpace::Separation { alt_space, tint_table, .. }
                        if matches!(alt_space.as_ref(), ImageColorSpace::DeviceCMYK | ImageColorSpace::ICCBased { n: 4, .. }) =>
                    {
                        let tint = lookup[li] as f32 / 255.0;
                        let mut alt = [0.0f32; 4];
                        tint_table.lookup_1d(tint, &mut alt);
                        return Some((alt[0] as f64, alt[1] as f64, alt[2] as f64, alt[3] as f64));
                    }
                    ImageColorSpace::DeviceN { alt_space, tint_table, .. }
                        if matches!(alt_space.as_ref(), ImageColorSpace::DeviceCMYK | ImageColorSpace::ICCBased { n: 4, .. }) =>
                    {
                        let ni = tint_table.num_inputs as usize;
                        let mut inputs = vec![0.0f32; ni];
                        for (c, inp) in inputs.iter_mut().enumerate() {
                            if c < base_ncomp {
                                *inp = lookup[li + c] as f32 / 255.0;
                            }
                        }
                        let mut alt = [0.0f32; 4];
                        tint_table.lookup_nd(&inputs, &mut alt);
                        return Some((alt[0] as f64, alt[1] as f64, alt[2] as f64, alt[3] as f64));
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}
/// Banded rendering as a free function — runs on a background thread.
///
/// Renders the display list in horizontal bands and streams the output
/// to a `PageSink`. This function is self-contained: it creates its own
/// band pixmaps, clip state, and streams rows to the sink.
#[allow(clippy::too_many_arguments)]
fn render_banded_to_sink(
    page_w: u32,
    page_h: u32,
    band_h: u32,
    dpi: f64,
    list: &DisplayList,
    sink: &mut dyn stet_graphics::device::PageSink,
    icc_cache: &IccCache,
    no_aa: bool,
) -> Result<(), String> {
    // Precompute Y bounding boxes for culling
    let bboxes = precompute_bboxes(list, dpi);

    // Build clip epochs — groups of elements between InitClip boundaries.
    // Epochs whose paint elements don't overlap a band can be skipped entirely,
    // avoiding both the per-element iteration AND clip mask rasterization.
    let epochs = build_clip_epochs(list, &bboxes);

    // Pre-populate clip_mask_seen so repeated clip paths get cached from first band
    let clip_seen = precompute_clip_seen(list);

    // Check if CMYK overprint simulation is needed
    let needs_cmyk_buffer = has_overprint_elements(list);

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
        // Start transparent — white background composited after content rendering
        band_pixmap.as_mut().data_mut().fill(0x00);

        let cmyk_buf = if needs_cmyk_buffer {
            // CMYK buffer for the render region (including overlap)
            Some(vec![0.0f32; page_w as usize * render_h as usize * 4])
        } else {
            None
        };

        let mut band_state = BandState {
            clip_region: None,
            spare_mask: None,
            clip_mask_cache: HashMap::new(),
            clip_mask_seen: clip_seen.clone(),
            mask_pool: Vec::new(),
            cmyk_buffer: cmyk_buf,
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
                let ctx = RenderContext {
                    vp_x: 0.0,
                    vp_y: render_y_start as f32,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    out_w: page_w,
                    out_h: render_h,
                    effective_dpi: dpi,
                    icc: icc_ref,
                    image_cache: None,
                    elem_idx: i,
                    no_aa,
                };
                render_element(
                    &mut band_pixmap,
                    &mut band_state,
                    &elements[i],
                    &ctx,
                );
            }
        }

        // Composite content onto white background (premultiplied alpha)
        composite_onto_white(band_pixmap.data_mut());

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
            DisplayElement::Group { params, .. } => Some(BBox2D {
                x_min: params.bbox[0],
                y_min: params.bbox[1],
                x_max: params.bbox[2],
                y_max: params.bbox[3],
            }),
            DisplayElement::SoftMasked { params, .. } => Some(BBox2D {
                x_min: params.bbox[0],
                y_min: params.bbox[1],
                x_max: params.bbox[2],
                y_max: params.bbox[3],
            }),
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
    let im_inv = im.invert()?;
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

/// Pre-converted RGBA image data cache, indexed by display list element index.
///
/// Built once per page after display list capture. Reused across all viewport
/// renders so that ICC color conversion (especially CMYK→sRGB) is not repeated
/// on every pan/zoom.
pub struct ImageCache {
    /// RGBA data per element index. `None` for non-image elements.
    entries: Vec<Option<Vec<u8>>>,
}

impl ImageCache {
    /// Build cache by pre-converting all images in the display list.
    pub fn build(list: &DisplayList, icc: Option<&IccCache>) -> Self {
        let entries = list
            .elements()
            .iter()
            .map(|elem| {
                if let DisplayElement::Image {
                    sample_data,
                    params,
                } = elem
                {
                    if params.width == 0 || params.height == 0 {
                        return None;
                    }
                    let mut rgba = samples_to_rgba(sample_data, params, icc);
                    if params.mask_color.is_some() {
                        apply_mask_color_rgba(&mut rgba, sample_data, params);
                    }
                    Some(rgba)
                } else {
                    None
                }
            })
            .collect();
        Self { entries }
    }

    /// Get pre-converted RGBA for the element at the given index.
    pub fn get(&self, index: usize) -> Option<&[u8]> {
        self.entries.get(index).and_then(|e| e.as_deref())
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
    image_cache: Option<&ImageCache>,
    no_aa: bool,
) -> Vec<u8> {
    if pixel_w == 0 || pixel_h == 0 || vp_w <= 0.0 || vp_h <= 0.0 {
        return vec![0xFF; pixel_w as usize * pixel_h as usize * 4];
    }

    let scale_x = pixel_w as f64 / vp_w;
    let scale_y = pixel_h as f64 / vp_h;
    let effective_dpi = dpi * scale_x;

    let mut pixmap = Pixmap::new(pixel_w, pixel_h).expect("Failed to create viewport pixmap");
    // Start transparent — white background composited after content rendering
    pixmap.fill(Color::TRANSPARENT);

    let cmyk_buf = if has_overprint_elements(list) {
        Some(vec![0.0f32; pixel_w as usize * pixel_h as usize * 4])
    } else {
        None
    };

    let mut state = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: prepared.clip_seen.clone(),
        mask_pool: Vec::new(),
        cmyk_buffer: cmyk_buf,
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

        #[allow(clippy::needless_range_loop)]
        for i in epoch.start_idx..epoch.end_idx {
            if let Some(ref bbox) = prepared.bboxes[i]
                && (bbox.x_max <= vp_x
                    || bbox.x_min >= vp_x_max
                    || bbox.y_max <= vp_y
                    || bbox.y_min >= vp_y_max)
                {
                    continue;
                }
            let ctx = RenderContext {
                vp_x: vp_x_f,
                vp_y: vp_y_f,
                scale_x: sx,
                scale_y: sy,
                out_w: pixel_w,
                out_h: pixel_h,
                effective_dpi,
                icc,
                image_cache,
                elem_idx: i,
                no_aa,
            };
            render_element(&mut pixmap, &mut state, &elements[i], &ctx);
        }
    }

    // Composite onto white background
    composite_onto_white(pixmap.data_mut());
    pixmap.data().to_vec()
}

/// Compute the number of bands and band height for viewport banding.
///
/// Returns `(num_bands, band_height)` using the same L2-cache-budget logic
/// as the full-page banded renderer.
pub fn viewport_band_count(pixel_w: u32, pixel_h: u32) -> (u32, u32) {
    let band_h = select_band_height(pixel_w, pixel_h);
    let num_bands = if band_h >= pixel_h {
        1
    } else {
        pixel_h.div_ceil(band_h)
    };
    (num_bands, band_h)
}

/// Render a single horizontal band of a viewport region.
///
/// This is the per-band counterpart to [`render_region_prepared()`]. The caller
/// loops over `band_idx` in `0..num_bands`, collecting RGBA strips that tile
/// vertically to form the full viewport image.
///
/// Returns RGBA pixel data for `actual_h` rows (may be less than `band_h` for
/// the last band).
#[allow(clippy::too_many_arguments)]
pub fn render_region_single_band(
    list: &DisplayList,
    prepared: &PreparedDisplayList,
    vp_x: f64,
    vp_y: f64,
    vp_w: f64,
    vp_h: f64,
    pixel_w: u32,
    pixel_h: u32,
    band_idx: u32,
    band_h: u32,
    num_bands: u32,
    dpi: f64,
    icc: Option<&IccCache>,
    image_cache: Option<&ImageCache>,
    no_aa: bool,
) -> Vec<u8> {
    if pixel_w == 0 || pixel_h == 0 || vp_w <= 0.0 || vp_h <= 0.0 {
        let actual_h = if band_idx < num_bands - 1 {
            band_h
        } else {
            pixel_h - band_idx * band_h
        };
        return vec![0xFF; pixel_w as usize * actual_h as usize * 4];
    }

    let scale_x = pixel_w as f64 / vp_w;
    let scale_y = pixel_h as f64 / vp_h;
    let effective_dpi = dpi * scale_x;

    // Output Y range for this band
    let out_y_start = band_idx * band_h;
    let actual_h = if band_idx < num_bands - 1 {
        band_h
    } else {
        pixel_h - out_y_start
    };

    // Add overlap above/below for anti-aliasing at seams
    const OVERLAP: u32 = 6;
    let render_y_start = out_y_start.saturating_sub(OVERLAP);
    let render_y_end = (out_y_start + actual_h + OVERLAP).min(pixel_h);
    let render_h = render_y_end - render_y_start;
    let overlap_top = out_y_start - render_y_start;

    // Source-space Y range for culling
    let src_y_min = vp_y + render_y_start as f64 / scale_y;
    let src_y_max = vp_y + render_y_end as f64 / scale_y;

    // Adjusted viewport offset for this band's pixmap
    let band_vp_y = vp_y + render_y_start as f64 / scale_y;

    let mut pixmap =
        Pixmap::new(pixel_w, render_h).expect("Failed to create band pixmap");
    pixmap.fill(Color::TRANSPARENT);

    let cmyk_buf = if has_overprint_elements(list) {
        Some(vec![0.0f32; pixel_w as usize * render_h as usize * 4])
    } else {
        None
    };

    let mut state = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: prepared.clip_seen.clone(),
        mask_pool: Vec::new(),
        cmyk_buffer: cmyk_buf,
    };

    let elements = list.elements();
    let vp_x_f = vp_x as f32;
    let band_vp_y_f = band_vp_y as f32;
    let sx = scale_x as f32;
    let sy = scale_y as f32;
    let vp_x_max = vp_x + vp_w;

    for epoch in &prepared.epochs {
        if !epoch.has_erase_page {
            match epoch.paint_bbox {
                Some(ref pb)
                    if pb.x_max <= vp_x
                        || pb.x_min >= vp_x_max
                        || pb.y_max <= src_y_min
                        || pb.y_min >= src_y_max =>
                {
                    continue;
                }
                None => continue,
                _ => {}
            }
        }

        #[allow(clippy::needless_range_loop)]
        for i in epoch.start_idx..epoch.end_idx {
            if let Some(ref bbox) = prepared.bboxes[i]
                && (bbox.x_max <= vp_x
                    || bbox.x_min >= vp_x_max
                    || bbox.y_max <= src_y_min
                    || bbox.y_min >= src_y_max)
                {
                    continue;
                }
            let ctx = RenderContext {
                vp_x: vp_x_f,
                vp_y: band_vp_y_f,
                scale_x: sx,
                scale_y: sy,
                out_w: pixel_w,
                out_h: render_h,
                effective_dpi,
                icc,
                image_cache,
                elem_idx: i,
                no_aa,
            };
            render_element(&mut pixmap, &mut state, &elements[i], &ctx);
        }
    }

    // Composite onto white background
    composite_onto_white(pixmap.data_mut());

    // Extract only the non-overlap rows
    let row_bytes = pixel_w as usize * 4;
    let start = overlap_top as usize * row_bytes;
    let end = start + actual_h as usize * row_bytes;
    pixmap.data()[start..end].to_vec()
}

/// Render a viewport region using parallel banded rendering via rayon.
///
/// This is the WASM counterpart to the parallel path in `render_banded_to_sink`.
/// All bands are rendered in parallel using `par_iter`, then assembled into the
/// final RGBA buffer in order.
///
/// Requires the `parallel` feature (rayon). Falls back to sequential rendering
/// if `parallel` is not enabled.
#[allow(clippy::too_many_arguments)]
pub fn render_region_prepared_parallel(
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
    image_cache: Option<&ImageCache>,
    no_aa: bool,
) -> Vec<u8> {
    let (num_bands, band_h) = viewport_band_count(pixel_w, pixel_h);

    if num_bands <= 1 {
        // Single band — no parallelism needed
        return render_region_prepared(list, prepared, vp_x, vp_y, vp_w, vp_h, pixel_w, pixel_h, dpi, icc, image_cache, no_aa);
    }

    let render_band = |band_idx: u32| -> Vec<u8> {
        render_region_single_band(
            list, prepared,
            vp_x, vp_y, vp_w, vp_h,
            pixel_w, pixel_h,
            band_idx, band_h, num_bands,
            dpi, icc, image_cache, no_aa,
        )
    };

    let row_bytes = pixel_w as usize * 4;
    let mut result = vec![0u8; pixel_w as usize * pixel_h as usize * 4];

    #[cfg(feature = "parallel")]
    {
        let chunk_size = rayon::current_num_threads().clamp(1, 8);

        for chunk_start in (0..num_bands).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size as u32).min(num_bands);

            let rendered: Vec<Vec<u8>> = (chunk_start..chunk_end)
                .into_par_iter()
                .map(&render_band)
                .collect();

            for (i, band_data) in rendered.iter().enumerate() {
                let band_idx = chunk_start + i as u32;
                let y_start = (band_idx * band_h) as usize;
                let dest_start = y_start * row_bytes;
                let len = band_data.len();
                result[dest_start..dest_start + len].copy_from_slice(band_data);
            }
        }
    }
    #[cfg(not(feature = "parallel"))]
    {
        for band_idx in 0..num_bands {
            let band_data = render_band(band_idx);
            let y_start = (band_idx * band_h) as usize;
            let dest_start = y_start * row_bytes;
            let len = band_data.len();
            result[dest_start..dest_start + len].copy_from_slice(&band_data);
        }
    }

    result
}

/// Render a full-page display list to RGBA pixels using the banded parallel renderer.
///
/// This is the preferred way to render a complete page — it uses rayon parallelism
/// (when the `parallel` feature is enabled) and L2-cache-friendly band sizing.
/// For sub-region / zoomed viewport rendering, use `render_region` instead.
///
/// Returns RGBA pixel data of size `pixel_w × pixel_h × 4`, composited onto white.
pub fn render_to_rgba(
    list: &DisplayList,
    pixel_w: u32,
    pixel_h: u32,
    dpi: f64,
    icc: Option<&IccCache>,
    no_aa: bool,
) -> Vec<u8> {
    if pixel_w == 0 || pixel_h == 0 {
        return vec![0xFF; pixel_w as usize * pixel_h as usize * 4];
    }

    let icc_cache = match icc {
        Some(c) => c.clone(),
        None => IccCache::new(),
    };

    let mut sink = MemorySink {
        data: Vec::new(),
        width: 0,
    };

    let band_h = select_band_height(pixel_w, pixel_h);
    if let Err(e) = render_banded_to_sink(pixel_w, pixel_h, band_h, dpi, list, &mut sink, &icc_cache, no_aa) {
        eprintln!("render_to_rgba: banded render failed: {e}");
        return vec![0xFF; pixel_w as usize * pixel_h as usize * 4];
    }

    sink.data
}

/// In-memory page sink that collects RGBA rows into a Vec.
struct MemorySink {
    data: Vec<u8>,
    width: u32,
}

impl stet_graphics::device::PageSink for MemorySink {
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.width = width;
        self.data.reserve(width as usize * height as usize * 4);
        Ok(())
    }

    fn write_rows(&mut self, rgba_rows: &[u8], _num_rows: u32) -> Result<(), String> {
        self.data.extend_from_slice(rgba_rows);
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), String> {
        Ok(())
    }
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
    image_cache: Option<&ImageCache>,
    no_aa: bool,
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
    pixmap.fill(Color::TRANSPARENT);

    let cmyk_buf = if has_overprint_elements(list) {
        Some(vec![0.0f32; pixel_w as usize * pixel_h as usize * 4])
    } else {
        None
    };

    let mut state = BandState {
        clip_region: None,
        spare_mask: None,
        clip_mask_cache: HashMap::new(),
        clip_mask_seen: clip_seen,
        mask_pool: Vec::new(),
        cmyk_buffer: cmyk_buf,
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
            if let Some(ref bbox) = bboxes[i]
                && (bbox.x_max <= vp_x
                    || bbox.x_min >= vp_x_max
                    || bbox.y_max <= vp_y
                    || bbox.y_min >= vp_y_max)
                {
                    continue;
                }
            let ctx = RenderContext {
                vp_x: vp_x_f,
                vp_y: vp_y_f,
                scale_x: sx,
                scale_y: sy,
                out_w: pixel_w,
                out_h: pixel_h,
                effective_dpi,
                icc,
                image_cache,
                elem_idx: i,
                no_aa,
            };
            render_element(&mut pixmap, &mut state, &elements[i], &ctx);
        }
    }

    composite_onto_white(pixmap.data_mut());
    pixmap.data().to_vec()
}
/// Copy a rectangular region from parent pixmap into a smaller crop pixmap.
fn copy_backdrop_crop(
    parent: &Pixmap,
    crop_x: i32,
    crop_y: i32,
    crop_w: u32,
    crop_h: u32,
) -> Vec<u8> {
    let pw = parent.width() as usize;
    let src = parent.data();
    let cw = crop_w as usize;
    let ch = crop_h as usize;
    let cx = crop_x as usize;
    let cy = crop_y as usize;
    let mut backdrop = vec![0u8; cw * ch * 4];
    for row in 0..ch {
        let src_off = ((cy + row) * pw + cx) * 4;
        let dst_off = row * cw * 4;
        backdrop[dst_off..dst_off + cw * 4].copy_from_slice(&src[src_off..src_off + cw * 4]);
    }
    backdrop
}
// ---- Shading rendering ----

/// Sutherland-Hodgman polygon clipping against a half-plane.
/// Keeps the side where `nx*(x-px) + ny*(y-py) >= 0`.
fn clip_polygon_halfplane(
    poly: &[(f32, f32)],
    nx: f32,
    ny: f32,
    px: f32,
    py: f32,
) -> Vec<(f32, f32)> {
    if poly.is_empty() {
        return vec![];
    }
    let dot = |x: f32, y: f32| nx * (x - px) + ny * (y - py);
    let mut out = Vec::with_capacity(poly.len() + 1);
    let n = poly.len();
    for i in 0..n {
        let (ax, ay) = poly[i];
        let (bx, by) = poly[(i + 1) % n];
        let da = dot(ax, ay);
        let db = dot(bx, by);
        if da >= 0.0 {
            out.push((ax, ay));
        }
        if (da >= 0.0) != (db >= 0.0) {
            // Edge crosses the clipping line — compute intersection
            let t = da / (da - db);
            out.push((ax + t * (bx - ax), ay + t * (by - ay)));
        }
    }
    out
}

/// Render an axial (linear) gradient shading.
#[allow(clippy::too_many_arguments)]
fn render_axial_shading(
    pixmap: &mut Pixmap,
    params: &AxialShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    clip_mask: Option<&Mask>,
    no_aa: bool,
    cmyk_buf: Option<&mut [f32]>,
    icc: Option<&IccCache>,
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

    let start = stet_tiny_skia::Point::from_xy(
        (dx0 as f32 - vp_x) * scale_x,
        (dy0 as f32 - vp_y) * scale_y,
    );
    let end = stet_tiny_skia::Point::from_xy(
        (dx1 as f32 - vp_x) * scale_x,
        (dy1 as f32 - vp_y) * scale_y,
    );

    let Some(gradient) = stet_tiny_skia::LinearGradient::new(
        start,
        end,
        stops,
        stet_tiny_skia::SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };

    let paint = Paint {
        shader: gradient,
        anti_alias: !no_aa,
        ..Paint::default()
    };

    let (mut rx_min, mut ry_min, mut rx_max, mut ry_max) = if let Some(bbox) = &params.bbox {
        let corners = [
            params.ctm.transform_point(bbox[0], bbox[1]),
            params.ctm.transform_point(bbox[2], bbox[1]),
            params.ctm.transform_point(bbox[0], bbox[3]),
            params.ctm.transform_point(bbox[2], bbox[3]),
        ];
        let x_min = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let y_min = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let x_max = corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max);
        let y_max = corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max);
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

    // When extend is false on a side, clip the fill area along a line
    // perpendicular to the gradient axis through that endpoint. For diagonal
    // gradients this produces a diagonal cutoff (not axis-aligned).
    let needs_perpendicular_clip = (!params.extend_start || !params.extend_end) && {
        let axis_x = dx1 - dx0;
        let axis_y = dy1 - dy0;
        // Only needed for diagonal gradients — axis-aligned ones get correct
        // results from simple rect clipping
        axis_x.abs() > 1e-6 && axis_y.abs() > 1e-6
    };

    if needs_perpendicular_clip {
        // Clip the bbox rectangle against perpendicular half-planes using
        // Sutherland-Hodgman polygon clipping.
        let mut poly: Vec<(f32, f32)> = vec![
            (rx_min, ry_min),
            (rx_max, ry_min),
            (rx_max, ry_max),
            (rx_min, ry_max),
        ];

        let ax = (dx1 - dx0) as f32 * scale_x;
        let ay = (dy1 - dy0) as f32 * scale_y;

        // Clip against perpendicular at start (keep side toward end)
        if !params.extend_start {
            let px = (dx0 as f32 - vp_x) * scale_x;
            let py = (dy0 as f32 - vp_y) * scale_y;
            // Normal pointing toward end: (ax, ay)
            // Keep points where ax*(x-px) + ay*(y-py) >= 0
            poly = clip_polygon_halfplane(&poly, ax, ay, px, py);
        }

        // Clip against perpendicular at end (keep side toward start)
        if !params.extend_end {
            let px = (dx1 as f32 - vp_x) * scale_x;
            let py = (dy1 as f32 - vp_y) * scale_y;
            // Normal pointing toward start: (-ax, -ay)
            // Keep points where -ax*(x-px) - ay*(y-py) >= 0
            poly = clip_polygon_halfplane(&poly, -ax, -ay, px, py);
        }

        if poly.len() >= 3 {
            let mut pb = PathBuilder::new();
            pb.move_to(poly[0].0, poly[0].1);
            for &(x, y) in &poly[1..] {
                pb.line_to(x, y);
            }
            pb.close();
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &paint, SkiaFillRule::Winding, Transform::identity(), clip_mask);
            }
        }
    } else {
        // Axis-aligned gradient or both sides extended: simple rect fill
        if !params.extend_start || !params.extend_end {
            let axis_x = dx1 - dx0;
            let axis_y = dy1 - dy0;
            let gx0 = (dx0 as f32 - vp_x) * scale_x;
            let gy0 = (dy0 as f32 - vp_y) * scale_y;
            let gx1 = (dx1 as f32 - vp_x) * scale_x;
            let gy1 = (dy1 as f32 - vp_y) * scale_y;

            if axis_x.abs() >= axis_y.abs() {
                if !params.extend_start {
                    if axis_x >= 0.0 { rx_min = rx_min.max(gx0); }
                    else { rx_max = rx_max.min(gx0); }
                }
                if !params.extend_end {
                    if axis_x >= 0.0 { rx_max = rx_max.min(gx1); }
                    else { rx_min = rx_min.max(gx1); }
                }
            } else {
                if !params.extend_start {
                    if axis_y >= 0.0 { ry_min = ry_min.max(gy0); }
                    else { ry_max = ry_max.min(gy0); }
                }
                if !params.extend_end {
                    if axis_y >= 0.0 { ry_max = ry_max.min(gy1); }
                    else { ry_min = ry_min.max(gy1); }
                }
            }
            if rx_max <= rx_min || ry_max <= ry_min {
                return;
            }
        }
        let rect = stet_tiny_skia::Rect::from_ltrb(rx_min, ry_min, rx_max, ry_max)
            .unwrap_or(stet_tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap());
        pixmap.fill_rect(rect, &paint, Transform::identity(), clip_mask);
    }

    // Update CMYK tracking buffer for axial shading
    if let Some(buf) = cmyk_buf {
        let pw = pixmap.width();
        let inv_sx = 1.0 / scale_x as f64;
        let inv_sy = 1.0 / scale_y as f64;
        let axis_x = params.x1 - params.x0;
        let axis_y = params.y1 - params.y0;
        let axis_len_sq = axis_x * axis_x + axis_y * axis_y;
        let Some(inv_ctm) = params.ctm.invert() else {
            return;
        };

        let iy_min = ry_min.floor() as u32;
        let iy_max = ry_max.ceil().min(pixmap.height() as f32) as u32;
        let ix_min = rx_min.floor() as u32;
        let ix_max = rx_max.ceil().min(pw as f32) as u32;

        for py in iy_min..iy_max {
            let dev_y = py as f64 * inv_sy + vp_y as f64;
            for px in ix_min..ix_max {
                let dev_x = px as f64 * inv_sx + vp_x as f64;
                let (ux, uy) = inv_ctm.transform_point(dev_x, dev_y);
                let t = if axis_len_sq > 1e-10 {
                    ((ux - params.x0) * axis_x + (uy - params.y0) * axis_y) / axis_len_sq
                } else {
                    0.0
                };
                if t < 0.0 && !params.extend_start {
                    continue;
                }
                if t > 1.0 && !params.extend_end {
                    continue;
                }
                let clamped = t.clamp(0.0, 1.0);

                if let Some(mask) = clip_mask {
                    let mi = py as usize * pw as usize + px as usize;
                    if mask.data()[mi] == 0 { continue; }
                }

                let color = interpolate_color_stops(&params.color_stops, clamped);
                let cmyk = interpolate_cmyk_from_stops(
                    &params.color_stops,
                    &params.color_space,
                    clamped,
                    &color,
                );
                let ci = (py as usize * pw as usize + px as usize) * 4;
                if ci + 3 < buf.len() {
                    if params.overprint
                        && params.painted_channels != stet_graphics::device::CMYK_ALL
                    {
                        if params.painted_channels & stet_graphics::device::CMYK_C != 0 {
                            buf[ci] = cmyk.0 as f32;
                        }
                        if params.painted_channels & stet_graphics::device::CMYK_M != 0 {
                            buf[ci + 1] = cmyk.1 as f32;
                        }
                        if params.painted_channels & stet_graphics::device::CMYK_Y != 0 {
                            buf[ci + 2] = cmyk.2 as f32;
                        }
                        if params.painted_channels & stet_graphics::device::CMYK_K != 0 {
                            buf[ci + 3] = cmyk.3 as f32;
                        }
                        // Recomposite RGB from merged CMYK via ICC
                        let c = buf[ci] as f64;
                        let m = buf[ci + 1] as f64;
                        let y = buf[ci + 2] as f64;
                        let k = buf[ci + 3] as f64;
                        let (rv, gv, bv) = if let Some(icc_cache) = icc {
                            icc_cache
                                .convert_cmyk_readonly(c, m, y, k)
                                .unwrap_or_else(|| cmyk_to_rgb_plrm(c, m, y, k))
                        } else {
                            cmyk_to_rgb_plrm(c, m, y, k)
                        };
                        let stride = pixmap.data().len() / pixmap.height() as usize;
                        let offset = py as usize * stride + px as usize * 4;
                        let data = pixmap.data_mut();
                        data[offset] = (rv * 255.0).round().clamp(0.0, 255.0) as u8;
                        data[offset + 1] = (gv * 255.0).round().clamp(0.0, 255.0) as u8;
                        data[offset + 2] = (bv * 255.0).round().clamp(0.0, 255.0) as u8;
                    } else {
                        buf[ci] = cmyk.0 as f32;
                        buf[ci + 1] = cmyk.1 as f32;
                        buf[ci + 2] = cmyk.2 as f32;
                        buf[ci + 3] = cmyk.3 as f32;
                        // Recomposite RGB from CMYK via ICC
                        if let Some(icc_cache) = icc {
                            let c = buf[ci] as f64;
                            let m = buf[ci + 1] as f64;
                            let y = buf[ci + 2] as f64;
                            let k = buf[ci + 3] as f64;
                            if let Some((rv, gv, bv)) =
                                icc_cache.convert_cmyk_readonly(c, m, y, k)
                            {
                                let stride =
                                    pixmap.data().len() / pixmap.height() as usize;
                                let offset = py as usize * stride + px as usize * 4;
                                let data = pixmap.data_mut();
                                data[offset] =
                                    (rv * 255.0).round().clamp(0.0, 255.0) as u8;
                                data[offset + 1] =
                                    (gv * 255.0).round().clamp(0.0, 255.0) as u8;
                                data[offset + 2] =
                                    (bv * 255.0).round().clamp(0.0, 255.0) as u8;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Render a radial gradient shading.
#[allow(clippy::too_many_arguments)]
fn render_radial_shading(
    pixmap: &mut Pixmap,
    params: &RadialShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    clip_mask: Option<&Mask>,
    _no_aa: bool,
    mut cmyk_buf: Option<&mut [f32]>,
    icc: Option<&IccCache>,
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
        let x_min = corners.iter().map(|c| c.0 as f32).fold(f32::INFINITY, f32::min);
        let y_min = corners.iter().map(|c| c.1 as f32).fold(f32::INFINITY, f32::min);
        let x_max = corners.iter().map(|c| c.0 as f32).fold(f32::NEG_INFINITY, f32::max);
        let y_max = corners.iter().map(|c| c.1 as f32).fold(f32::NEG_INFINITY, f32::max);
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
        let dev_y = py as f64 * inv_sy + vp_y as f64;
        for px in px_min..px_max {
            let dev_x = px as f64 * inv_sx + vp_x as f64;
            let (ux, uy) = inv_ctm.transform_point(dev_x, dev_y);
            let t = solve_radial_t(
                ux, uy, params.x0, params.y0, params.r0, params.x1, params.y1, params.r1,
                params.extend_start, params.extend_end,
            );
            if let Some(t) = t {
                let clamped = t.clamp(0.0, 1.0);
                let color = interpolate_color_stops(&params.color_stops, clamped);

                let clipped = clip_mask.is_some_and(|mask| {
                    mask.data()[py as usize * pw as usize + px as usize] == 0
                });

                if clipped {
                    continue;
                }

                // Write CMYK buffer at non-clipped pixels
                if let Some(ref mut buf) = cmyk_buf {
                    let ci = (py as usize * pw as usize + px as usize) * 4;
                    if ci + 3 < buf.len() {
                        let cmyk = interpolate_cmyk_from_stops(
                            &params.color_stops,
                            &params.color_space,
                            clamped,
                            &color,
                        );
                        if params.overprint
                            && params.painted_channels != stet_graphics::device::CMYK_ALL
                        {
                            if params.painted_channels & stet_graphics::device::CMYK_C != 0 {
                                buf[ci] = cmyk.0 as f32;
                            }
                            if params.painted_channels & stet_graphics::device::CMYK_M != 0 {
                                buf[ci + 1] = cmyk.1 as f32;
                            }
                            if params.painted_channels & stet_graphics::device::CMYK_Y != 0 {
                                buf[ci + 2] = cmyk.2 as f32;
                            }
                            if params.painted_channels & stet_graphics::device::CMYK_K != 0 {
                                buf[ci + 3] = cmyk.3 as f32;
                            }
                        } else {
                            buf[ci] = cmyk.0 as f32;
                            buf[ci + 1] = cmyk.1 as f32;
                            buf[ci + 2] = cmyk.2 as f32;
                            buf[ci + 3] = cmyk.3 as f32;
                        }
                    }
                }

                let offset = py as usize * stride + px as usize * 4;
                data[offset] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 3] = 255;

                // Recomposite RGB from CMYK buffer via ICC (only for CMYK shadings)
                if matches!(params.color_space, ShadingColorSpace::DeviceCMYK)
                    && let Some(ref mut buf) = cmyk_buf {
                        let ci = (py as usize * pw as usize + px as usize) * 4;
                        if ci + 3 < buf.len()
                            && let Some(icc_cache) = icc {
                                let c = buf[ci] as f64;
                                let m = buf[ci + 1] as f64;
                                let y = buf[ci + 2] as f64;
                                let k = buf[ci + 3] as f64;
                                if let Some((r, g, b)) =
                                    icc_cache.convert_cmyk_readonly(c, m, y, k)
                                {
                                    data[offset] =
                                        (r * 255.0).round().clamp(0.0, 255.0) as u8;
                                    data[offset + 1] =
                                        (g * 255.0).round().clamp(0.0, 255.0) as u8;
                                    data[offset + 2] =
                                        (b * 255.0).round().clamp(0.0, 255.0) as u8;
                                }
                            }
                    }
            }
        }
    }
}
/// Solve for the parameter t of a two-circle radial gradient at point (px, py).
///
/// Returns the largest root of the circle equation that falls within the valid
/// domain and has R(t) >= 0. The valid domain is [0,1], extended by extend flags.
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
    extend_start: bool,
    extend_end: bool,
) -> Option<f64> {
    // Parametric: C(t) = (1-t)*C0 + t*C1, R(t) = (1-t)*r0 + t*r1
    // Solve: (px - Cx(t))^2 + (py - Cy(t))^2 = R(t)^2
    let cdx = x1 - x0;
    let cdy = y1 - y0;
    let dr = r1 - r0;

    let a = cdx * cdx + cdy * cdy - dr * dr;
    let dpx = px - x0;
    let dpy = py - y0;
    let b = -2.0 * (dpx * cdx + dpy * cdy + r0 * dr);
    let c = dpx * dpx + dpy * dpy - r0 * r0;

    // Helper: check if a root is in the valid domain
    let in_domain = |t: f64| -> bool {
        (0.0..=1.0).contains(&t)
            || (t < 0.0 && extend_start)
            || (t > 1.0 && extend_end)
    };

    if a.abs() < 1e-10 {
        // Linear case
        if b.abs() < 1e-10 {
            return None;
        }
        let t = -c / b;
        let radius = r0 + t * dr;
        if radius >= 0.0 && in_domain(t) {
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

    // Pick the largest root that is in the valid domain and has R(t) >= 0
    let mut best: Option<f64> = None;
    for t in [t1, t2] {
        let radius = r0 + t * dr;
        if radius >= 0.0 && in_domain(t) {
            best = Some(match best {
                Some(prev) => prev.max(t),
                None => t,
            });
        }
    }
    best
}

/// Render a Gouraud-shaded triangle mesh.
#[allow(clippy::too_many_arguments)]
fn render_mesh_shading(
    pixmap: &mut Pixmap,
    params: &MeshShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    clip_mask: Option<&Mask>,
    mut cmyk_buf: Option<&mut [f32]>,
    icc: Option<&IccCache>,
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

                let clipped = clip_mask.is_some_and(|mask| {
                    mask.data()[py * pw + px] == 0
                });

                let w0c = w0.max(0.0);
                let w1c = w1.max(0.0);
                let w2c = w2.max(0.0);
                let wsum = w0c + w1c + w2c;
                let w0n = w0c / wsum;
                let w1n = w1c / wsum;
                let w2n = w2c / wsum;
                let r = w0n * tri.v0.color.r + w1n * tri.v1.color.r + w2n * tri.v2.color.r;
                let g = w0n * tri.v0.color.g + w1n * tri.v1.color.g + w2n * tri.v2.color.g;
                let b = w0n * tri.v0.color.b + w1n * tri.v1.color.b + w2n * tri.v2.color.b;

                // Write CMYK buffer
                if let Some(ref mut buf) = cmyk_buf {
                    let ci = (py * pw + px) * 4;
                    if ci + 3 < buf.len() {
                        let cmyk = interpolate_cmyk_from_vertices(
                            &tri.v0, &tri.v1, &tri.v2, w0n, w1n, w2n,
                            &params.color_space, r, g, b,
                        );
                        if params.overprint
                            && params.painted_channels != stet_graphics::device::CMYK_ALL
                        {
                            if !clipped {
                                if params.painted_channels & stet_graphics::device::CMYK_C != 0 {
                                    buf[ci] = cmyk.0 as f32;
                                }
                                if params.painted_channels & stet_graphics::device::CMYK_M != 0 {
                                    buf[ci + 1] = cmyk.1 as f32;
                                }
                                if params.painted_channels & stet_graphics::device::CMYK_Y != 0 {
                                    buf[ci + 2] = cmyk.2 as f32;
                                }
                                if params.painted_channels & stet_graphics::device::CMYK_K != 0 {
                                    buf[ci + 3] = cmyk.3 as f32;
                                }
                            }
                        } else {
                            buf[ci] = cmyk.0 as f32;
                            buf[ci + 1] = cmyk.1 as f32;
                            buf[ci + 2] = cmyk.2 as f32;
                            buf[ci + 3] = cmyk.3 as f32;
                        }
                    }
                }

                if clipped {
                    continue;
                }

                let offset = py * stride + px * 4;
                data[offset] = (r * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 1] = (g * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 2] = (b * 255.0).round().clamp(0.0, 255.0) as u8;
                data[offset + 3] = 255;

                // Recomposite RGB from CMYK buffer via ICC
                if let Some(ref mut buf) = cmyk_buf {
                    let ci = (py * pw + px) * 4;
                    if ci + 3 < buf.len()
                        && let Some(icc_cache) = icc {
                            let c = buf[ci] as f64;
                            let m = buf[ci + 1] as f64;
                            let y = buf[ci + 2] as f64;
                            let k = buf[ci + 3] as f64;
                            if let Some((rv, gv, bv)) =
                                icc_cache.convert_cmyk_readonly(c, m, y, k)
                            {
                                data[offset] =
                                    (rv * 255.0).round().clamp(0.0, 255.0) as u8;
                                data[offset + 1] =
                                    (gv * 255.0).round().clamp(0.0, 255.0) as u8;
                                data[offset + 2] =
                                    (bv * 255.0).round().clamp(0.0, 255.0) as u8;
                            }
                        }
                }
            }
        }
    }
}

/// Render a Coons/tensor-product patch mesh by subdividing into triangles.
#[allow(clippy::too_many_arguments)]
fn render_patch_shading(
    pixmap: &mut Pixmap,
    params: &PatchShadingParams,
    vp_x: f32,
    vp_y: f32,
    scale_x: f32,
    scale_y: f32,
    clip_mask: Option<&Mask>,
    cmyk_buf: Option<&mut [f32]>,
    icc: Option<&IccCache>,
) {
    let mut triangles = Vec::new();
    let scale = scale_x.max(scale_y) as f64;
    for patch in &params.patches {
        if patch.points.len() >= 12 {
            // Compute device-space extent to choose subdivision level
            let mut x_min = f64::INFINITY;
            let mut y_min = f64::INFINITY;
            let mut x_max = f64::NEG_INFINITY;
            let mut y_max = f64::NEG_INFINITY;
            for &(px, py) in &patch.points {
                let (dx, dy) = params.ctm.transform_point(px, py);
                x_min = x_min.min(dx);
                y_min = y_min.min(dy);
                x_max = x_max.max(dx);
                y_max = y_max.max(dy);
            }
            let extent = (x_max - x_min).max(y_max - y_min).abs() * scale;
            // Target ~2 device pixels per boundary segment
            let n = (extent / 2.0).ceil().clamp(8.0, 64.0) as usize;
            subdivide_patch_to_triangles(patch, &mut triangles, n);
        }
    }
    if !triangles.is_empty() {
        let mesh_params = MeshShadingParams {
            triangles,
            ctm: params.ctm,
            bbox: params.bbox,
            color_space: params.color_space.clone(),
            overprint: params.overprint,
            painted_channels: params.painted_channels,
        };
        render_mesh_shading(
            pixmap,
            &mesh_params,
            vp_x,
            vp_y,
            scale_x,
            scale_y,
            clip_mask,
            cmyk_buf,
            icc,
        );
    }
}
/// Subdivide a Coons/tensor patch into triangles via recursive de Casteljau.
/// Uses a simple grid subdivision approach: evaluate the patch at NxN points
/// and triangulate the resulting grid.
fn subdivide_patch_to_triangles(
    patch: &stet_graphics::device::ShadingPatch,
    triangles: &mut Vec<stet_graphics::device::ShadingTriangle>,
    n: usize,
) {

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

            use stet_graphics::device::ShadingVertex;
            triangles.push(stet_graphics::device::ShadingTriangle {
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
            triangles.push(stet_graphics::device::ShadingTriangle {
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
fn eval_coons_patch(patch: &stet_graphics::device::ShadingPatch, u: f64, v: f64) -> (f64, f64) {
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
fn build_gradient_stops(stops: &[stet_graphics::device::ColorStop]) -> Vec<stet_tiny_skia::GradientStop> {
    let mut result = Vec::with_capacity(stops.len());
    for stop in stops {
        let r = (stop.color.r * 255.0).round().clamp(0.0, 255.0) as u8;
        let g = (stop.color.g * 255.0).round().clamp(0.0, 255.0) as u8;
        let b = (stop.color.b * 255.0).round().clamp(0.0, 255.0) as u8;
        result.push(stet_tiny_skia::GradientStop::new(
            stop.position as f32,
            Color::from_rgba8(r, g, b, 255),
        ));
    }
    result
}

/// Interpolate between color stops at a given position (0.0..=1.0).
fn interpolate_color_stops(stops: &[stet_graphics::device::ColorStop], position: f64) -> DeviceColor {
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

/// Derive CMYK values from color stops at parameter t.
/// Uses raw_components when the shading is in DeviceCMYK, otherwise
/// reverse-engineers from the interpolated RGB.
fn interpolate_cmyk_from_stops(
    stops: &[stet_graphics::device::ColorStop],
    cs: &ShadingColorSpace,
    t: f64,
    color: &DeviceColor,
) -> (f64, f64, f64, f64) {
    match cs {
        ShadingColorSpace::DeviceCMYK => {
            // Interpolate raw CMYK components from stops
            if stops.len() == 1 {
                let rc = &stops[0].raw_components;
                if rc.len() >= 4 {
                    return (rc[0], rc[1], rc[2], rc[3]);
                }
            }
            // Find surrounding stops and interpolate
            let mut lo = &stops[0];
            let mut hi = stops.last().unwrap();
            for i in 0..stops.len() - 1 {
                if stops[i + 1].position >= t {
                    lo = &stops[i];
                    hi = &stops[i + 1];
                    break;
                }
            }
            let span = hi.position - lo.position;
            let frac = if span > 1e-10 {
                (t - lo.position) / span
            } else {
                0.0
            };
            let frac = frac.clamp(0.0, 1.0);
            if lo.raw_components.len() >= 4 && hi.raw_components.len() >= 4 {
                (
                    lo.raw_components[0] + frac * (hi.raw_components[0] - lo.raw_components[0]),
                    lo.raw_components[1] + frac * (hi.raw_components[1] - lo.raw_components[1]),
                    lo.raw_components[2] + frac * (hi.raw_components[2] - lo.raw_components[2]),
                    lo.raw_components[3] + frac * (hi.raw_components[3] - lo.raw_components[3]),
                )
            } else {
                // Fallback: reverse from RGB
                let r = color.r;
                let g = color.g;
                let b = color.b;
                (1.0 - r, 1.0 - g, 1.0 - b, 0.0)
            }
        }
        _ => {
            // Non-CMYK color space: reverse-engineer from RGB
            let r = color.r;
            let g = color.g;
            let b = color.b;
            (1.0 - r, 1.0 - g, 1.0 - b, 0.0)
        }
    }
}

/// Derive CMYK values from triangle mesh vertices using barycentric weights.
#[allow(clippy::too_many_arguments)]
fn interpolate_cmyk_from_vertices(
    v0: &ShadingVertex,
    v1: &ShadingVertex,
    v2: &ShadingVertex,
    w0: f64,
    w1: f64,
    w2: f64,
    cs: &ShadingColorSpace,
    r: f64,
    g: f64,
    b: f64,
) -> (f64, f64, f64, f64) {
    match cs {
        ShadingColorSpace::DeviceCMYK => {
            if v0.raw_components.len() >= 4
                && v1.raw_components.len() >= 4
                && v2.raw_components.len() >= 4
            {
                (
                    w0 * v0.raw_components[0]
                        + w1 * v1.raw_components[0]
                        + w2 * v2.raw_components[0],
                    w0 * v0.raw_components[1]
                        + w1 * v1.raw_components[1]
                        + w2 * v2.raw_components[1],
                    w0 * v0.raw_components[2]
                        + w1 * v1.raw_components[2]
                        + w2 * v2.raw_components[2],
                    w0 * v0.raw_components[3]
                        + w1 * v1.raw_components[3]
                        + w2 * v2.raw_components[3],
                )
            } else {
                (1.0 - r, 1.0 - g, 1.0 - b, 0.0)
            }
        }
        _ => (1.0 - r, 1.0 - g, 1.0 - b, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_graphics::device::{BgUcrState, HalftoneState, TransferState};
    use stet_graphics::color::DashPattern;

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
            overprint_mode: 0,
            painted_channels: 0,
            is_device_cmyk: false,
            spot_color: None,
            rendering_intent: 0,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
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
            overprint_mode: 0,
            painted_channels: 0,
            spot_color: None,
            rendering_intent: 0,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
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
            overprint_mode: 0,
            painted_channels: 0,
            is_device_cmyk: false,
            spot_color: None,
            rendering_intent: 0,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
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
            overprint_mode: 0,
            painted_channels: 0,
            is_device_cmyk: false,
            spot_color: None,
            rendering_intent: 0,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
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
            overprint_mode: 0,
            painted_channels: 0,
            is_device_cmyk: false,
            spot_color: None,
            rendering_intent: 0,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
        };
        dev.fill_path(&path, &params);

        // Pixel at translated location should be green
        let pixel = dev.pixmap().pixel(105, 105).unwrap();
        assert_eq!(pixel.green(), 255);
        assert_eq!(pixel.red(), 0);
    }
}
