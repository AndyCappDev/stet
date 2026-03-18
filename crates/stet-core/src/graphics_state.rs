// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Graphics state: transforms, paths, colors, and rendering parameters.

use crate::display_list::DisplayList;
use crate::object::PsObject;
use std::sync::Arc;

// ── Re-exports from stet-fonts and stet-graphics ────────────────────────────
// These types used to be defined here. Re-export for backward compatibility.
pub use stet_fonts::geometry::{Matrix, PathSegment, PsPath, round10};
pub use stet_graphics::color::{
    CieAParams, CieAbcParams, CieDefParams, CieDefgParams, DashPattern, DeviceColor, FillRule,
    LineCap, LineJoin,
};

// ── Types that remain in stet-core (depend on PS VM types) ──────────────────

/// Color space identifier.
#[derive(Clone, Debug)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    /// Indexed color space: `[/Indexed base hival lookup]`.
    /// `lookup_proc` is `Some(proc_object)` when the lookup is a procedure that
    /// needs to be pre-evaluated via exec_sync during setcolorspace.
    Indexed {
        base: Box<ColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
        lookup_proc: Option<PsObject>,
    },
    /// CIE-based ABC color space (3 components): `[/CIEBasedABC dict]`.
    CIEBasedABC {
        params: Arc<CieAbcParams>,
        dict_entity: crate::object::EntityId,
    },
    /// CIE-based A color space (1 component): `[/CIEBasedA dict]`.
    CIEBasedA {
        params: Arc<CieAParams>,
        dict_entity: crate::object::EntityId,
    },
    /// CIE-based DEF color space (3 components → 3D table → ABC → sRGB).
    CIEBasedDEF {
        params: Arc<CieDefParams>,
        dict_entity: crate::object::EntityId,
    },
    /// CIE-based DEFG color space (4 components → 4D table → ABC → sRGB).
    CIEBasedDEFG {
        params: Arc<CieDefgParams>,
        dict_entity: crate::object::EntityId,
    },
    /// ICC-based color space: `[/ICCBased dict]` where dict has /N components.
    /// When `profile_hash` is Some, colors are converted through the ICC profile.
    /// Falls back to device space based on N (1=Gray, 3=RGB, 4=CMYK).
    ICCBased {
        dict_entity: crate::object::EntityId,
        n: u32,
        profile_hash: Option<crate::icc::ProfileHash>,
    },
    /// Separation color space: `[/Separation name alternativeSpace tintTransform]`.
    /// Single tint component mapped to alternative space via tint transform procedure.
    Separation {
        name: Vec<u8>,
        alt_space: Box<ColorSpace>,
        tint_transform: crate::object::PsObject,
        num_alt_components: u32,
    },
    /// DeviceN color space: `[/DeviceN names alternativeSpace tintTransform]`.
    /// N tint components mapped to alternative space via tint transform procedure.
    DeviceN {
        names: Vec<Vec<u8>>,
        num_colorants: u32,
        alt_space: Box<ColorSpace>,
        tint_transform: crate::object::PsObject,
        num_alt_components: u32,
    },
}

impl PartialEq for ColorSpace {
    fn eq(&self, other: &Self) -> bool {
        use ColorSpace::*;
        match (self, other) {
            (DeviceGray, DeviceGray) | (DeviceRGB, DeviceRGB) | (DeviceCMYK, DeviceCMYK) => true,
            (
                Indexed {
                    base: b1,
                    hival: h1,
                    lookup: l1,
                    ..
                },
                Indexed {
                    base: b2,
                    hival: h2,
                    lookup: l2,
                    ..
                },
            ) => b1 == b2 && h1 == h2 && l1 == l2,
            (
                CIEBasedABC {
                    dict_entity: d1, ..
                },
                CIEBasedABC {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                CIEBasedA {
                    dict_entity: d1, ..
                },
                CIEBasedA {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                CIEBasedDEF {
                    dict_entity: d1, ..
                },
                CIEBasedDEF {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                CIEBasedDEFG {
                    dict_entity: d1, ..
                },
                CIEBasedDEFG {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                ICCBased {
                    dict_entity: d1,
                    n: n1,
                    ..
                },
                ICCBased {
                    dict_entity: d2,
                    n: n2,
                    ..
                },
            ) => d1 == d2 && n1 == n2,
            (
                Separation {
                    name: name1,
                    alt_space: a1,
                    tint_transform: t1,
                    num_alt_components: n1,
                },
                Separation {
                    name: name2,
                    alt_space: a2,
                    tint_transform: t2,
                    num_alt_components: n2,
                },
            ) => name1 == name2 && a1 == a2 && t1 == t2 && n1 == n2,
            (
                DeviceN {
                    names: names1,
                    num_colorants: nc1,
                    alt_space: a1,
                    tint_transform: t1,
                    num_alt_components: n1,
                },
                DeviceN {
                    names: names2,
                    num_colorants: nc2,
                    alt_space: a2,
                    tint_transform: t2,
                    num_alt_components: n2,
                },
            ) => names1 == names2 && nc1 == nc2 && a1 == a2 && t1 == t2 && n1 == n2,
            _ => false,
        }
    }
}

/// Pattern instance data created by `makepattern`.
#[derive(Clone)]
pub struct PatternData {
    /// Pattern type: 1 = tiling, 2 = shading.
    pub pattern_type: i32,
    /// Paint type: 1 = colored, 2 = uncolored.
    pub paint_type: i32,
    /// Tiling type: 1 = constant spacing, 2 = no distortion, 3 = fast.
    pub tiling_type: i32,
    /// Bounding box [llx, lly, urx, ury] in pattern space.
    pub bbox: [f64; 4],
    /// X step between tile origins.
    pub xstep: f64,
    /// Y step between tile origins.
    pub ystep: f64,
    /// Combined matrix: matrix_arg × CTM at makepattern time.
    pub pattern_matrix: Matrix,
    /// Pre-rendered display list from executing PaintProc.
    pub cached_display_list: DisplayList,
}

/// Entry on the graphics state stack, tracking whether it was created by
/// `save` (implicit gsave) or `gsave`.
#[derive(Clone, Debug)]
pub struct GstateEntry {
    pub state: GraphicsState,
    /// True if created by `save`, false if by `gsave`.
    /// `grestore` skips save-created entries; `grestoreall` stops at them.
    pub saved_by_save: bool,
}

/// Complete graphics state (cloned for gsave/grestore).
#[derive(Clone, Debug)]
pub struct GraphicsState {
    pub ctm: Matrix,
    pub color: DeviceColor,
    pub color_space: ColorSpace,
    pub path: PsPath,
    pub current_point: Option<(f64, f64)>,
    pub clip_path: Option<PsPath>,
    pub clip_path_version: u32,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash_pattern: DashPattern,
    pub flatness: f64,
    pub stroke_adjust: bool,
    pub overprint: bool,
    pub smoothness: f64,
    pub default_ctm: Matrix,

    // Clip save/restore stack (per graphics state)
    pub clip_stack: Vec<Option<PsPath>>,

    // Current font (set by setfont, used by show operators)
    pub current_font: Option<crate::object::PsObject>,

    // Root font for composite font hierarchy (set during Type 0 rendering).
    // rootfont returns this if set, otherwise falls back to current_font.
    pub root_font: Option<crate::object::PsObject>,

    // Page device dict (EntityId into DictStore)
    pub page_device: Option<crate::object::EntityId>,

    // Halftone screen parameters (set by setscreen/setcolorscreen/sethalftone)
    pub screen_freq: f64,
    pub screen_angle: f64,
    pub screen_proc: Option<crate::object::PsObject>,
    /// Per-component color screen: [red, green, blue, gray] × (freq, angle, proc)
    pub color_screen: Option<[(f64, f64, crate::object::PsObject); 4]>,
    /// Halftone dictionary (set by sethalftone)
    pub halftone: Option<crate::object::PsObject>,

    // Transfer functions
    pub transfer_function: Option<crate::object::PsObject>,
    /// Per-component transfer: [red, green, blue, gray]
    pub color_transfer: Option<[crate::object::PsObject; 4]>,
    /// Pre-sampled transfer function (256 entries). None = identity.
    pub sampled_transfer: Option<Arc<Vec<f64>>>,
    /// Pre-sampled per-component transfer \[R, G, B, Gray\].
    pub sampled_color_transfer: Option<[Option<Arc<Vec<f64>>>; 4]>,
    /// Pre-computed halftone screen for PDF output. None = default (suppressed).
    pub precomputed_halftone: Option<Arc<crate::device::HalftoneScreen>>,
    /// Pre-computed per-component halftone \[R, G, B, Gray\] (from setcolorscreen).
    pub precomputed_color_halftone: Option<[Option<Arc<crate::device::HalftoneScreen>>; 4]>,

    // Black generation / undercolor removal
    pub black_generation: Option<crate::object::PsObject>,
    pub undercolor_removal: Option<crate::object::PsObject>,
    /// Pre-sampled black generation function (256 entries, domain [0,1] → range [0,1]).
    pub sampled_black_generation: Option<Arc<Vec<f64>>>,
    /// Pre-sampled undercolor removal function (256 entries, domain [0,1] → range [-1,1]).
    pub sampled_ucr: Option<Arc<Vec<f64>>>,

    // Color rendering dictionary
    pub color_rendering: Option<crate::object::PsObject>,

    /// Rendering intent: 0=RelativeColorimetric, 1=AbsoluteColorimetric,
    /// 2=Perceptual, 3=Saturation. Default is RelativeColorimetric.
    pub rendering_intent: u8,

    // Pattern state (set by setpattern, consumed by fill/eofill)
    /// Index into `Context.pattern_store` for the active tiling pattern.
    pub current_pattern: Option<u32>,
    /// Underlying color for uncolored (PaintType 2) patterns.
    pub pattern_underlying_color: Option<DeviceColor>,

    // Userpath bounding box (set by setbbox, cleared by newpath)
    pub bbox: Option<[f64; 4]>,

    /// Tint values from the most recent setcolor (for Separation/DeviceN).
    /// 1 value for Separation, N values for DeviceN. None for device color spaces.
    pub tint_values: Option<Vec<f64>>,

    /// Cached pre-sampled tint lookup table for the current Separation/DeviceN color space.
    /// Set when setcolorspace installs a Separation/DeviceN space.
    pub cached_tint_table: Option<Arc<crate::device::TintLookupTable>>,
}

impl GraphicsState {
    /// Create default graphics state (PostScript initial state).
    pub fn new() -> Self {
        Self {
            ctm: Matrix::identity(),
            color: DeviceColor::black(),
            color_space: ColorSpace::DeviceGray,
            path: PsPath::new(),
            current_point: None,
            clip_path: None,
            clip_path_version: 0,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern: DashPattern::solid(),
            flatness: 1.0,
            stroke_adjust: false,
            overprint: false,
            smoothness: 1.0,
            default_ctm: Matrix::identity(),
            clip_stack: Vec::new(),
            current_font: None,
            root_font: None,
            page_device: None,
            screen_freq: 60.0,
            screen_angle: 45.0,
            screen_proc: None,
            color_screen: None,
            halftone: None,
            transfer_function: None,
            color_transfer: None,
            sampled_transfer: None,
            sampled_color_transfer: None,
            precomputed_halftone: None,
            precomputed_color_halftone: None,
            black_generation: None,
            undercolor_removal: None,
            sampled_black_generation: None,
            sampled_ucr: None,
            color_rendering: None,
            rendering_intent: 0, // RelativeColorimetric
            current_pattern: None,
            pattern_underlying_color: None,
            bbox: None,
            tint_values: None,
            cached_tint_table: None,
        }
    }
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_graphics_state() {
        let gs = GraphicsState::new();
        assert_eq!(gs.line_width, 1.0);
        assert_eq!(gs.line_cap, LineCap::Butt);
        assert_eq!(gs.line_join, LineJoin::Miter);
        assert_eq!(gs.miter_limit, 10.0);
        assert!(gs.path.is_empty());
        assert!(gs.current_point.is_none());
        assert!(gs.clip_path.is_none());
        assert_eq!(gs.flatness, 1.0);
        assert!(!gs.stroke_adjust);
    }
}
