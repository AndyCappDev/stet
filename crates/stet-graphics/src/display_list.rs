// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Display list — records drawing operations for deferred replay to a device.

use std::sync::{Arc, Mutex};

use crate::device::{
    AxialShadingParams, ClipParams, FillParams, ImageParams, MeshShadingParams, PatchShadingParams,
    PatternFillParams, RadialShadingParams, StrokeParams, TextParams,
};
use stet_fonts::geometry::PsPath;

/// A pre-rasterized soft mask, cached on the display-list `SoftMasked`
/// element so the renderer can build it once at gs-time CTM and sample it
/// per content pixel without re-rasterizing for every band.
///
/// The renderer holds the mask raster in its own device-space pixel
/// coordinate system (anchored at `(origin_x, origin_y)`) rather than the
/// `SoftMasked.params.bbox` viewport, because the mask form's internal
/// `cm` operators may translate the actual paint elements outside the
/// form's `/BBox` after the gs-time CTM is applied. Sampling per content
/// pixel by device coordinates decouples the mask and content coordinate
/// systems entirely.
#[derive(Clone, Debug)]
pub struct MaskRaster {
    /// Single-channel mask values (luminosity or alpha), row-major.
    pub data: Vec<u8>,
    /// Width of the raster in pixels.
    pub width: u32,
    /// Height of the raster in pixels.
    pub height: u32,
    /// Top-left corner of the raster in device-space pixels at `scale`.
    pub origin_x: i32,
    /// Top-left corner of the raster in device-space pixels at `scale`.
    pub origin_y: i32,
    /// Horizontal scale at which the raster was built. The CLI egui
    /// viewer and the WASM viewport both re-render the same captured
    /// display list at varying zoom scales, so the cache must invalidate
    /// when this changes.
    pub scale_x: f32,
    /// Vertical scale at which the raster was built.
    pub scale_y: f32,
}

/// Subtype for soft mask extraction.
#[derive(Clone, Debug, PartialEq)]
pub enum SoftMaskSubtype {
    /// Use the alpha channel of the rendered mask directly.
    Alpha,
    /// Convert rendered mask to luminosity (grayscale).
    Luminosity,
}

/// Parameters for a soft mask compositing operation.
#[derive(Clone)]
pub struct SoftMaskParams {
    /// How to extract the mask from the rendered form.
    pub subtype: SoftMaskSubtype,
    /// Device-space bounding box [x_min, y_min, x_max, y_max].
    pub bbox: [f64; 4],
    /// Backdrop color for luminosity masks (RGB, 0.0–1.0). None = black.
    pub backdrop_color: Option<[f64; 3]>,
    /// Whether the mask values should be inverted (from /TR `{1 exch sub}`).
    pub transfer_invert: bool,
    /// Whether the mask form contained nested soft mask scopes (gs-set SMask).
    /// When true, the renderer composites semi-transparent pixels onto the
    /// backdrop before extracting luminosity.
    pub has_nested_mask_scope: bool,
    /// Bounding box of the parent gstate's clip path at the moment the
    /// SoftMasked element was emitted (in device space).
    ///
    /// Used by the renderer as a hard upper bound on the cached mask
    /// raster size: pixels outside the parent clip can't affect the
    /// final image, so the raster never needs to extend beyond it.
    /// Without this cap, a soft mask whose form contains an unbounded
    /// shading (no `/BBox`) inside a sentinel-sized internal clip would
    /// blow past the renderer's mask-raster size limit and rasterize
    /// to nothing, making the entire SoftMasked element invisible.
    ///
    /// `None` means the parent had no active clip path — the renderer
    /// then bounds the raster only by the mask's actual paint bounds.
    pub parent_clip_bbox: Option<[f64; 4]>,
}

/// Color space declared by a transparency group's `/CS` entry. Per PDF spec
/// §11.6.7, this is the color space in which the group's compositing
/// computations are performed; renderers that need spec-correct blend mode
/// math (especially for the inversion-sensitive separable modes and the HSL
/// non-separable modes) must operate in this space rather than the device's
/// display space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupColorSpace {
    /// No `/CS` entry — inherits from the enclosing group / page group.
    Inherited,
    /// `/DeviceGray` or `/CalGray` or `/ICCBased` with N=1.
    DeviceGray,
    /// `/DeviceRGB` or `/CalRGB` or `/ICCBased` with N=3.
    DeviceRGB,
    /// `/DeviceCMYK` or `/ICCBased` with N=4.
    DeviceCMYK,
}

/// Parameters for a transparency group compositing operation.
#[derive(Clone)]
pub struct GroupParams {
    /// Device-space bounding box [x_min, y_min, x_max, y_max].
    pub bbox: [f64; 4],
    /// Whether the group is isolated (renders against transparent backdrop).
    pub isolated: bool,
    /// Whether the group uses knockout semantics (elements composite against
    /// the initial backdrop, not against accumulated siblings).
    pub knockout: bool,
    /// Blend mode for compositing the group result onto the parent.
    pub blend_mode: u8,
    /// Opacity for compositing the group result (0.0–1.0).
    pub alpha: f64,
    /// Group's transparency color space (`/CS` entry on the `/Group` dict).
    /// Inherited from the enclosing group when not explicitly declared.
    pub color_space: GroupColorSpace,
}

/// Visibility predicate for an [`DisplayElement::OcgGroup`].
///
/// Three forms exist in PDF:
///
/// - `Single` — a `/OC BDC` block whose property is a direct OCG ref,
///   or a single-OCG OCMD. Most common case.
/// - `Membership` — an OCMD with a `/P` policy (AllOn / AnyOn /
///   AllOff / AnyOff) over multiple OCGs. PDF default policy is
///   `AnyOn`.
/// - `Expression` — an OCMD with a `/VE` boolean expression
///   (PDF 1.6+). Most expressive form.
///
/// The renderer evaluates the predicate against the active
/// `LayerSet` (in `stet-pdf-reader`); each variant's
/// `default_visible` is the fallback when the LayerSet has no
/// opinion.
#[derive(Clone, Debug)]
pub enum OcgVisibility {
    /// Visibility tied to a single OCG.
    Single { ocg_id: u32, default_visible: bool },
    /// OCMD with a `/P` membership policy over a set of OCGs.
    Membership {
        ocg_ids: Vec<u32>,
        policy: MembershipPolicy,
        default_visible: bool,
    },
    /// OCMD with a `/VE` visibility expression.
    Expression {
        expr: VisibilityExpr,
        default_visible: bool,
    },
}

/// `/P` policy on an OCMD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MembershipPolicy {
    /// `/AllOn` — visible iff every OCG is on.
    AllOn,
    /// `/AnyOn` — visible iff at least one OCG is on. PDF default.
    AnyOn,
    /// `/AllOff` — visible iff every OCG is off.
    AllOff,
    /// `/AnyOff` — visible iff at least one OCG is off.
    AnyOff,
}

/// Boolean visibility expression from an OCMD `/VE` array.
///
/// The parser always emits the canonical form (operands as
/// `Vec<VisibilityExpr>`); leaves are layer references.
#[derive(Clone, Debug)]
pub enum VisibilityExpr {
    /// Conjunction — visible iff every operand is visible.
    And(Vec<VisibilityExpr>),
    /// Disjunction — visible iff any operand is visible.
    Or(Vec<VisibilityExpr>),
    /// Negation — exactly one operand.
    Not(Box<VisibilityExpr>),
    /// Leaf: refer to a single OCG by object number.
    Layer(u32),
}

impl OcgVisibility {
    /// Convenience constructor for the most common case.
    pub fn single(ocg_id: u32, default_visible: bool) -> Self {
        OcgVisibility::Single {
            ocg_id,
            default_visible,
        }
    }

    /// The fallback visibility used when no `LayerSet` has an
    /// opinion. Renderers that do not consult a LayerSet read this.
    pub fn default_visible(&self) -> bool {
        match self {
            OcgVisibility::Single {
                default_visible, ..
            }
            | OcgVisibility::Membership {
                default_visible, ..
            }
            | OcgVisibility::Expression {
                default_visible, ..
            } => *default_visible,
        }
    }
}

/// A single recorded drawing operation.
///
/// Marked `#[non_exhaustive]` so additional element kinds can land
/// without breaking third-party renderers; consumers must include a
/// wildcard arm in their `match` expressions. See
/// `docs/DISPLAY-LIST.md` ("Stability") for the policy.
#[derive(Clone)]
#[non_exhaustive]
pub enum DisplayElement {
    /// Fill a path.
    Fill { path: PsPath, params: FillParams },
    /// Stroke a path.
    Stroke { path: PsPath, params: StrokeParams },
    /// Intersect the clip region with a path.
    Clip { path: PsPath, params: ClipParams },
    /// Reset clipping to the full page.
    InitClip,
    /// Draw an image (raw sample data in native color space).
    Image {
        sample_data: Arc<Vec<u8>>,
        params: ImageParams,
    },
    /// Erase the page (fill with white).
    ErasePage,
    /// Axial (linear) gradient shading.
    AxialShading { params: AxialShadingParams },
    /// Radial gradient shading.
    RadialShading { params: RadialShadingParams },
    /// Gouraud-shaded triangle mesh.
    MeshShading { params: MeshShadingParams },
    /// Coons/tensor-product patch mesh.
    PatchShading { params: PatchShadingParams },
    /// Tiled pattern fill.
    PatternFill { params: PatternFillParams },
    /// Text element from show operators (used by PDF device, ignored by rasterizer).
    Text { params: TextParams },
    /// Transparency group: render children offscreen, composite with blend mode + alpha.
    Group {
        elements: DisplayList,
        params: GroupParams,
    },
    /// PDF Optional Content Group (layer). Children are rendered only
    /// when [`OcgVisibility`] evaluates to `true` under the active
    /// `LayerSet` (consult `stet-pdf-reader`'s `LayerSet::evaluate`).
    /// `default_visible` on each variant is the fallback used when the
    /// renderer has no `LayerSet` opinion for the relevant OCGs.
    OcgGroup {
        elements: DisplayList,
        /// Visibility predicate: a single OCG, an OCMD membership
        /// policy, or a /VE expression.
        visibility: OcgVisibility,
    },
    /// Soft-masked content: render mask form to grayscale, multiply with content alpha.
    SoftMasked {
        mask: DisplayList,
        content: DisplayList,
        params: SoftMaskParams,
        /// Render-time cache of the rasterized mask. `None` means "not
        /// yet rasterized". `Some(None)` means "rasterized and produced
        /// no visible mask" — memoized so subsequent bands skip the
        /// rasterization work. `Some(Some(raster))` is the populated
        /// raster; the renderer compares `raster.scale_x/scale_y`
        /// against the current render scale and re-rasterizes if they
        /// differ.
        ///
        /// Wrapped in `Arc<Mutex<...>>` so cloned display lists (e.g. by
        /// the egui viewer or the WASM viewport during zoom) share the
        /// same cache cell, and so the cache can be replaced when the
        /// scale changes.
        mask_cache: Arc<Mutex<Option<Option<MaskRaster>>>>,
    },
}

/// An ordered list of drawing operations for a single page.
#[derive(Clone)]
pub struct DisplayList {
    elements: Vec<DisplayElement>,
    /// Color space of the page-level transparency group, when one is declared.
    /// Per PDF spec §11.6.7 the page group's color space is the one in which
    /// any contained transparency compositing must be performed; renderers
    /// use this to decide whether to track CMYK alongside sRGB.
    page_group_color_space: GroupColorSpace,
}

impl DisplayList {
    /// Create an empty display list.
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            page_group_color_space: GroupColorSpace::Inherited,
        }
    }

    /// Returns the page-level transparency group color space.
    pub fn page_group_color_space(&self) -> GroupColorSpace {
        self.page_group_color_space
    }

    /// Set the page-level transparency group color space (called by the PDF
    /// reader when the page dictionary declares a `/Group /CS`).
    pub fn set_page_group_color_space(&mut self, cs: GroupColorSpace) {
        self.page_group_color_space = cs;
    }

    /// Append a drawing operation.
    pub fn push(&mut self, element: DisplayElement) {
        self.elements.push(element);
    }

    /// Access recorded elements.
    pub fn elements(&self) -> &[DisplayElement] {
        &self.elements
    }

    /// Discard all recorded operations.
    pub fn clear(&mut self) {
        self.elements.clear();
    }

    /// Returns true if the display list has no elements.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Returns the number of elements.
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Returns a slice of elements starting from the given index.
    pub fn elements_from(&self, start: usize) -> &[DisplayElement] {
        &self.elements[start..]
    }

    /// Drain elements from `start..` into a new DisplayList, truncating self.
    pub fn split_off(&mut self, start: usize) -> DisplayList {
        let drained: Vec<DisplayElement> = self.elements.drain(start..).collect();
        DisplayList {
            elements: drained,
            page_group_color_space: GroupColorSpace::Inherited,
        }
    }

    /// Consume the display list and return the elements.
    pub fn into_elements(self) -> Vec<DisplayElement> {
        self.elements
    }
}

impl Default for DisplayList {
    fn default() -> Self {
        Self::new()
    }
}
