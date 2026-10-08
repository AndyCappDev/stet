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
    /// Pattern color space: `/Pattern`, `[/Pattern]` or `[/Pattern base]`.
    ///
    /// PLRM 4.9.6. `base` is `None` for a space that can only hold colored
    /// (PaintType 1) patterns, and `Some(space)` for one that also accepts
    /// uncolored (PaintType 2) patterns, whose color comes from `base`.
    /// A Pattern space may not itself be used as `base`.
    Pattern {
        base: Option<Box<ColorSpace>>,
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
            (Pattern { base: b1 }, Pattern { base: b2 }) => b1 == b2,
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
    /// Pre-rendered display list: what PaintProc drew (tiling), or the
    /// shading placed by `pattern_matrix` (shading).
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
    /// Overprint mode, 0 or 1, set by `setoverprintmode` (an Adobe
    /// version-3015 extension; PDF `/OPM`). Under 1, a DeviceCMYK paint with
    /// overprint on leaves the colorants whose component is 0 untouched.
    pub overprint_mode: i32,
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
    /// Pre-sampled black generation function (256 entries, domain `[0,1]` → range `[0,1]`).
    pub sampled_black_generation: Option<Arc<Vec<f64>>>,
    /// Pre-sampled undercolor removal function (256 entries, domain `[0,1]` → range `[-1,1]`).
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
    /// The pattern dictionary installed by `setpattern` (or by `setcolor` in a
    /// Pattern color space). PLRM 4.9.6 makes the pattern part of the current
    /// color, so `currentcolor` has to hand the same object back.
    pub current_pattern_dict: Option<crate::object::EntityId>,
    /// Components of the underlying color for an uncolored (PaintType 2)
    /// pattern, in the Pattern space's base color space. Empty for colored
    /// patterns; `currentcolor` pushes these ahead of the pattern dictionary.
    pub pattern_components: Vec<f64>,

    // Userpath bounding box (set by setbbox, cleared by newpath)
    pub bbox: Option<[f64; 4]>,

    /// Tint values from the most recent setcolor (for Separation/DeviceN).
    /// 1 value for Separation, N values for DeviceN. None for device color spaces.
    pub tint_values: Option<Vec<f64>>,

    /// Components of the most recent colour set in an ICCBased space with a
    /// profile, as given to `setcolor`. Meaningful only while `color_space`
    /// is that space. The colour is converted when it is set, so
    /// `setrenderingintent` converts these again under the new intent.
    pub icc_components: Option<Vec<f64>>,

    /// Cached pre-sampled tint lookup table for the current Separation/DeviceN color space.
    /// Set when setcolorspace installs a Separation/DeviceN space.
    pub cached_tint_table: Option<Arc<crate::device::TintLookupTable>>,

    /// Constant fill opacity (PDF `ca`). Range \[0,1\]. Default 1.0.
    pub fill_opacity: f64,
    /// Constant stroke opacity (PDF `CA`). Range \[0,1\]. Default 1.0.
    pub stroke_opacity: f64,
    /// Blend mode index. 0=Normal, 1=Multiply, …, 15=Luminosity. Default 0.
    pub blend_mode: u8,
    /// Alpha-is-shape flag (PDF `AIS`). Default false.
    pub alpha_is_shape: bool,
    /// Text knockout flag (PDF `TK`). Default true.
    pub text_knockout: bool,
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
            overprint_mode: 0,
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
            rendering_intent: stet_graphics::rendering_intent::RELATIVE_COLORIMETRIC,
            current_pattern: None,
            pattern_underlying_color: None,
            current_pattern_dict: None,
            pattern_components: Vec::new(),
            bbox: None,
            tint_values: None,
            icc_components: None,
            cached_tint_table: None,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
            text_knockout: true,
        }
    }

    /// Reset the parameters `initgraphics` resets, leaving the rest alone.
    ///
    /// PLRM 3e (`initgraphics`) lists them: position, path, clipping path,
    /// color space, color, line width, cap, join, miter limit and dash
    /// pattern, plus the CTM, which the caller sets because the default
    /// matrix depends on the page device. "All other graphics state
    /// parameters are left unchanged. These include the current output
    /// device, font parameter, stroke adjustment, clipping path stack, and
    /// all device-dependent parameters": overprint, flatness, smoothness,
    /// halftone, transfer, black generation and undercolor removal.
    /// `showpage` performs the equivalent of `initgraphics`, so those
    /// survive from page to page, as in Ghostscript.
    ///
    /// The PDF-imaging extension parameters (opacity, blend mode,
    /// alpha-is-shape, text knockout) are outside the PLRM; they reset here
    /// because Ghostscript's `gs_initgraphics` resets them.
    ///
    /// Every field is named below, so a new one fails to compile until it
    /// is sorted into one group or the other.
    pub fn init_graphics(&mut self) {
        let GraphicsState {
            // Reset.
            color,
            color_space,
            path,
            current_point,
            clip_path,
            line_width,
            line_cap,
            line_join,
            miter_limit,
            dash_pattern,
            current_pattern,
            pattern_underlying_color,
            current_pattern_dict,
            pattern_components,
            bbox,
            tint_values,
            icc_components,
            cached_tint_table,
            fill_opacity,
            stroke_opacity,
            blend_mode,
            alpha_is_shape,
            text_knockout,
            // Set by the caller.
            ctm: _,
            // Advanced, not reset: `grestore` compares versions to decide
            // whether the device clip needs re-emitting.
            clip_path_version: _,
            // Left unchanged.
            default_ctm: _,
            flatness: _,
            stroke_adjust: _,
            overprint: _,
            overprint_mode: _,
            smoothness: _,
            clip_stack: _,
            current_font: _,
            root_font: _,
            page_device: _,
            screen_freq: _,
            screen_angle: _,
            screen_proc: _,
            color_screen: _,
            halftone: _,
            transfer_function: _,
            color_transfer: _,
            sampled_transfer: _,
            sampled_color_transfer: _,
            precomputed_halftone: _,
            precomputed_color_halftone: _,
            black_generation: _,
            undercolor_removal: _,
            sampled_black_generation: _,
            sampled_ucr: _,
            color_rendering: _,
            rendering_intent: _,
        } = GraphicsState::new();
        self.color = color;
        self.color_space = color_space;
        self.path = path;
        self.current_point = current_point;
        self.clip_path = clip_path;
        self.clip_path_version += 1;
        self.line_width = line_width;
        self.line_cap = line_cap;
        self.line_join = line_join;
        self.miter_limit = miter_limit;
        self.dash_pattern = dash_pattern;
        self.current_pattern = current_pattern;
        self.pattern_underlying_color = pattern_underlying_color;
        self.current_pattern_dict = current_pattern_dict;
        self.pattern_components = pattern_components;
        self.bbox = bbox;
        self.tint_values = tint_values;
        self.icc_components = icc_components;
        self.cached_tint_table = cached_tint_table;
        self.fill_opacity = fill_opacity;
        self.stroke_opacity = stroke_opacity;
        self.blend_mode = blend_mode;
        self.alpha_is_shape = alpha_is_shape;
        self.text_knockout = text_knockout;
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

    #[test]
    fn init_graphics_resets_only_what_the_plrm_lists() {
        let mut gs = GraphicsState::new();
        gs.line_width = 5.0;
        gs.line_cap = LineCap::Round;
        gs.color_space = ColorSpace::DeviceCMYK;
        gs.current_point = Some((1.0, 2.0));
        gs.fill_opacity = 0.5;
        gs.flatness = 7.0;
        gs.stroke_adjust = true;
        gs.overprint = true;
        gs.overprint_mode = 1;
        gs.smoothness = 0.4;
        gs.clip_path_version = 3;
        gs.init_graphics();

        assert_eq!(gs.line_width, 1.0);
        assert_eq!(gs.line_cap, LineCap::Butt);
        assert!(matches!(gs.color_space, ColorSpace::DeviceGray));
        assert!(gs.current_point.is_none());
        assert_eq!(gs.fill_opacity, 1.0);

        assert_eq!(gs.flatness, 7.0);
        assert!(gs.stroke_adjust);
        assert!(gs.overprint);
        assert_eq!(gs.overprint_mode, 1);
        assert_eq!(gs.smoothness, 0.4);
        assert_eq!(gs.clip_path_version, 4, "clip version must advance");
    }
}
