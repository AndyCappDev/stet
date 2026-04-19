// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! ICC color profile support via moxcms.
//!
//! Parses embedded ICC profiles from `[/ICCBased stream]` color spaces and
//! converts colors to sRGB. Also searches for system CMYK profiles to improve
//! DeviceCMYK → RGB conversion beyond the naive PLRM formula.

pub mod bpc;

use bpc::{
    BpcParams, apply_bpc_f64, apply_bpc_rgb_u8, compute_bpc_params, detect_source_black_point,
};
use moxcms::{
    CmsError, ColorProfile, DataColorSpace, Layout, RenderingIntent, TransformExecutor,
    TransformOptions,
};
use std::collections::HashMap;
use std::sync::Arc;

/// SHA-256 hash used as profile key.
pub type ProfileHash = [u8; 32];

/// Black Point Compensation mode for CMYK→sRGB conversion.
///
/// Reference renderers (Ghostscript, Acrobat, Firefox via lcms2) apply BPC by
/// default for relative-colorimetric CMYK→sRGB. moxcms 0.8.1 ships BPC
/// commented out, so without it K-heavy colors render visibly lighter than
/// reference renderers. See `docs/PLAN-BPC.md` for the full design.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BpcMode {
    /// Skip BPC; matches stet's pre-fix behavior. Useful for proofing-style
    /// renders that should preserve actual densities, or for bit-for-bit
    /// reproduction of older baselines.
    Off,
    /// Always apply BPC during CMYK→sRGB conversion.
    On,
    /// Default — currently equivalent to `On`. Reserved for forward
    /// compatibility (eventually could honor PDF rendering-intent or
    /// output-intent hints).
    #[default]
    Auto,
}

impl BpcMode {
    /// True when BPC should be applied at conversion time.
    #[inline]
    pub fn is_enabled(self) -> bool {
        matches!(self, BpcMode::On | BpcMode::Auto)
    }
}

/// Construction-time options for [`IccCache`].
///
/// Bundles together the BPC mode and an optional pre-supplied source CMYK
/// profile (overriding the automatic system-profile search). Created via
/// [`IccCache::new_with_options`].
#[derive(Clone, Default)]
pub struct IccCacheOptions {
    /// BPC mode for CMYK→sRGB conversion.
    pub bpc_mode: BpcMode,
    /// Raw bytes of a source CMYK profile to register as the system profile.
    /// When `None`, the cache is created empty and the caller is responsible
    /// for invoking [`IccCache::search_system_cmyk_profile`] (or providing
    /// bytes some other way).
    pub source_cmyk_profile: Option<Vec<u8>>,
}

/// Identity Gray→RGB transform: maps each gray value to equal R=G=B.
/// Used as fallback when a Gray ICC profile can't produce a proper transform.
struct GrayToRgbIdentity;

impl TransformExecutor<u8> for GrayToRgbIdentity {
    fn transform(&self, src: &[u8], dst: &mut [u8]) -> Result<(), CmsError> {
        for (g, rgb) in src.iter().zip(dst.chunks_exact_mut(3)) {
            rgb[0] = *g;
            rgb[1] = *g;
            rgb[2] = *g;
        }
        Ok(())
    }
}

impl TransformExecutor<f64> for GrayToRgbIdentity {
    fn transform(&self, src: &[f64], dst: &mut [f64]) -> Result<(), CmsError> {
        for (g, rgb) in src.iter().zip(dst.chunks_exact_mut(3)) {
            rgb[0] = *g;
            rgb[1] = *g;
            rgb[2] = *g;
        }
        Ok(())
    }
}

/// Pre-baked 4D CLUT sampling a CMYK ICC transform on a regular grid.
///
/// At profile-registration time we sample moxcms at `grid_n^4` evenly-spaced
/// CMYK points and store the sRGB output. At image-conversion time we do
/// K-slice plus 3D tetrahedral interpolation inside each slice. This is ~30×
/// faster than direct moxcms for LUT-based CMYK profiles (e.g., SWOP) while
/// staying well inside imperceptible ΔE for typical print-workflow inputs.
#[derive(Clone)]
struct Clut4 {
    /// Grid points per axis (typical: 17).
    grid_n: u8,
    /// Flat LUT in order (k, y, m, c) with C fastest, K slowest.
    /// Length = grid_n^4 * 3 bytes (packed sRGB).
    data: Arc<Vec<u8>>,
}

/// Cached ICC transform to sRGB (specific to source layout).
#[derive(Clone)]
struct CachedTransform {
    /// 8-bit transform for image data.
    transform_8bit: Arc<dyn TransformExecutor<u8> + Send + Sync>,
    /// f64 transform for single-color conversions.
    transform_f64: Arc<dyn TransformExecutor<f64> + Send + Sync>,
    /// Number of source components.
    n: u32,
    /// Whether the source profile is Lab (needs value normalization).
    is_lab: bool,
    /// Pre-baked 4D CLUT for fast CMYK→sRGB image conversion.
    /// Only built for `n == 4` profiles; None otherwise.
    clut4: Option<Clut4>,
    /// Cached Black Point Compensation parameters for this profile. Computed
    /// when `n == 4` and `IccCache::bpc_mode` is enabled. Applied as a
    /// post-correction on the moxcms output (sRGB → XYZ-D50 → BPC shift →
    /// back to sRGB) so K-heavy CMYK colours map to true zero black.
    bpc_params: Option<BpcParams>,
}

/// ICC color profile cache and transform manager.
#[derive(Clone)]
pub struct IccCache {
    /// SHA-256 hash → parsed ColorProfile.
    profiles: HashMap<ProfileHash, Arc<ColorProfile>>,
    /// Cached transforms: hash → CachedTransform.
    transforms: HashMap<ProfileHash, CachedTransform>,
    /// Single-color conversion cache: (hash-prefix, quantized_components) → (r, g, b).
    /// Uses first 8 bytes of hash as u64 key for compactness.
    color_cache: HashMap<(u64, [u16; 4]), (f64, f64, f64)>,
    /// Default system CMYK profile hash (if found at startup).
    default_cmyk_hash: Option<ProfileHash>,
    /// Raw bytes of the system CMYK profile (for re-registration in render threads).
    system_cmyk_bytes: Option<Arc<Vec<u8>>>,
    /// Raw profile bytes for each registered profile (for PDF embedding).
    raw_bytes: HashMap<ProfileHash, Arc<Vec<u8>>>,
    /// sRGB output profile (created once).
    srgb_profile: ColorProfile,
    /// Cached sRGB→CMYK reverse transform (for RGB round-trip through CMYK page groups).
    reverse_cmyk_f64: Option<Arc<dyn TransformExecutor<f64> + Send + Sync>>,
    /// Black Point Compensation mode for CMYK→sRGB conversion. Set at
    /// construction time via [`IccCacheOptions`]; consulted by future BPC
    /// apply paths (commit 2 of `docs/PLAN-BPC.md`).
    bpc_mode: BpcMode,
}

impl Default for IccCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Apply BPC to an sRGB triple if `params` is `Some`; otherwise return the
/// triple unchanged. Centralised so every conversion entry point stays in
/// sync.
#[inline]
fn bpc_post_correct(rgb: [f64; 3], params: Option<&BpcParams>) -> [f64; 3] {
    match params {
        Some(p) => apply_bpc_f64(rgb, p),
        None => rgb,
    }
}

impl IccCache {
    /// Create an empty ICC cache with default options (BPC `Auto`, no
    /// pre-supplied source CMYK profile).
    pub fn new() -> Self {
        Self::new_with_options(IccCacheOptions::default())
    }

    /// Create an ICC cache with the given options.
    ///
    /// When `opts.source_cmyk_profile` is `Some`, the bytes are registered as
    /// the system CMYK profile (overriding any later
    /// [`Self::search_system_cmyk_profile`] call). Otherwise the cache starts
    /// empty and the caller is expected to supply a profile separately.
    pub fn new_with_options(opts: IccCacheOptions) -> Self {
        let mut cache = Self {
            profiles: HashMap::new(),
            transforms: HashMap::new(),
            color_cache: HashMap::new(),
            default_cmyk_hash: None,
            system_cmyk_bytes: None,
            raw_bytes: HashMap::new(),
            srgb_profile: ColorProfile::new_srgb(),
            reverse_cmyk_f64: None,
            bpc_mode: opts.bpc_mode,
        };
        if let Some(bytes) = opts.source_cmyk_profile {
            cache.load_cmyk_profile_bytes(&bytes);
        }
        cache
    }

    /// Current Black Point Compensation mode.
    #[inline]
    pub fn bpc_mode(&self) -> BpcMode {
        self.bpc_mode
    }

    /// Compute the SHA-256 hash of an ICC profile without registering it.
    pub fn hash_profile(bytes: &[u8]) -> ProfileHash {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).into()
    }

    /// Register an ICC profile from raw bytes. Returns the SHA-256 hash on success.
    pub fn register_profile(&mut self, bytes: &[u8]) -> Option<ProfileHash> {
        self.register_profile_with_n(bytes, None)
    }

    /// Register an ICC profile, validating that its color space matches the
    /// expected component count `expected_n`. When the profile's actual color
    /// space has a different number of components (e.g. an RGB profile stored
    /// with PDF `/N 1`), the profile is rejected so the caller can fall back
    /// to the alternate color space.
    pub fn register_profile_with_n(
        &mut self,
        bytes: &[u8],
        expected_n: Option<u32>,
    ) -> Option<ProfileHash> {
        use sha2::{Digest, Sha256};
        let hash: ProfileHash = Sha256::digest(bytes).into();

        // Already registered?
        if self.transforms.contains_key(&hash) {
            return Some(hash);
        }

        // Store raw bytes for PDF embedding
        self.raw_bytes
            .entry(hash)
            .or_insert_with(|| Arc::new(bytes.to_vec()));

        let profile = match ColorProfile::new_from_slice(bytes) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[ICC] Failed to parse profile: {e}");
                return None;
            }
        };

        let n = match profile.color_space {
            DataColorSpace::Gray => 1u32,
            DataColorSpace::Rgb => 3,
            DataColorSpace::Cmyk => 4,
            DataColorSpace::Lab => 3,
            _ => {
                eprintln!(
                    "[ICC] Unsupported profile color space: {:?}",
                    profile.color_space
                );
                return None;
            }
        };

        // Reject profile when its actual component count doesn't match the
        // PDF's /N declaration — the input data won't match the profile's
        // expected input layout.
        if let Some(expected) = expected_n {
            if n != expected {
                return None;
            }
        }

        let (src_layout_8, src_layout_f64) = match n {
            1 => (Layout::Gray, Layout::Gray),
            3 => (Layout::Rgb, Layout::Rgb),
            4 => (Layout::Rgba, Layout::Rgba),
            _ => return None,
        };

        let dst_layout_8 = Layout::Rgb;
        let dst_layout_f64 = Layout::Rgb;

        // Try multiple rendering intents — ICC v4 profiles may only have A2B0 (Perceptual)
        let intents = [
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::Perceptual,
            RenderingIntent::AbsoluteColorimetric,
            RenderingIntent::Saturation,
        ];

        let mut transform_8bit = None;
        for &intent in &intents {
            let options = TransformOptions {
                rendering_intent: intent,
                ..TransformOptions::default()
            };
            match profile.create_transform_8bit(
                src_layout_8,
                &self.srgb_profile,
                dst_layout_8,
                options,
            ) {
                Ok(t) => {
                    transform_8bit = Some(t);
                    break;
                }
                Err(_) => continue,
            }
        }
        let transform_8bit = match transform_8bit {
            Some(t) => t,
            None if n == 1 => {
                // Gray profiles that can't produce Gray→sRGB transforms (e.g.
                // minimal Linotype profiles with only a TRC): fall back to the
                // sRGB gray curve, which is functionally correct for most Gray
                // profiles encountered in PDFs.
                return self.register_gray_identity(hash, profile);
            }
            None => {
                eprintln!(
                    "[ICC] Failed to create 8-bit transform (cs={:?})",
                    profile.color_space
                );
                return None;
            }
        };

        let mut transform_f64 = None;
        for &intent in &intents {
            let options = TransformOptions {
                rendering_intent: intent,
                ..TransformOptions::default()
            };
            match profile.create_transform_f64(
                src_layout_f64,
                &self.srgb_profile,
                dst_layout_f64,
                options,
            ) {
                Ok(t) => {
                    transform_f64 = Some(t);
                    break;
                }
                Err(_) => continue,
            }
        }
        let transform_f64 = match transform_f64 {
            Some(t) => t,
            None if n == 1 => {
                // Same Gray fallback for f64 path
                return self.register_gray_identity(hash, profile);
            }
            None => {
                eprintln!(
                    "[ICC] Failed to create f64 transform (cs={:?})",
                    profile.color_space
                );
                return None;
            }
        };

        let is_lab = profile.color_space == DataColorSpace::Lab;

        // BPC parameters for CMYK profiles. Detect the source profile's
        // "as-mapped" black point by sampling (1,1,1,1) through the 8-bit
        // transform, then derive per-axis shift coefficients targeting
        // sRGB's true zero black. Computed before the CLUT bake so the
        // bake can fold BPC in once and the runtime CLUT lookup stays at
        // zero per-pixel cost.
        let bpc_params = if n == 4 && self.bpc_mode.is_enabled() {
            detect_source_black_point(transform_8bit.as_ref())
                .map(|sbp| compute_bpc_params(sbp, [0.0; 3], bpc::WP_D50))
        } else {
            None
        };

        // For 4-channel (CMYK) profiles, pre-bake a 17^4 CLUT for fast image
        // conversion. The bake invokes the 8-bit transform once over a regular
        // grid; subsequent image conversions interpolate the grid at ~30× the
        // throughput of direct moxcms on LUT-based CMYK profiles. BPC is baked
        // in here when present, so per-pixel runtime cost stays at zero.
        let clut4 = if n == 4 {
            let c = bake_clut4(transform_8bit.as_ref(), 17, bpc_params.as_ref());
            if std::env::var_os("STET_ICC_VERIFY").is_some()
                && let Some(ref clut) = c
            {
                verify_clut4(clut, transform_8bit.as_ref(), bpc_params.as_ref());
            }
            c
        } else {
            None
        };

        self.profiles.insert(hash, Arc::new(profile));
        self.transforms.insert(
            hash,
            CachedTransform {
                transform_8bit,
                transform_f64,
                n,
                is_lab,
                clut4,
                bpc_params,
            },
        );

        Some(hash)
    }

    /// Register a Gray profile with an identity Gray→RGB fallback transform.
    /// Used when the ICC library can't create a proper transform from the profile
    /// (e.g. minimal profiles with only a TRC and no A2B/B2A tables).
    fn register_gray_identity(
        &mut self,
        hash: ProfileHash,
        profile: ColorProfile,
    ) -> Option<ProfileHash> {
        self.profiles.insert(hash, Arc::new(profile));
        self.transforms.insert(
            hash,
            CachedTransform {
                transform_8bit: Arc::new(GrayToRgbIdentity),
                transform_f64: Arc::new(GrayToRgbIdentity),
                n: 1,
                is_lab: false,
                clut4: None,
                bpc_params: None,
            },
        );
        Some(hash)
    }

    /// Convert a single color through an ICC profile to sRGB.
    /// Returns (r, g, b) in [0, 1] range.
    pub fn convert_color(
        &mut self,
        hash: &ProfileHash,
        components: &[f64],
    ) -> Option<(f64, f64, f64)> {
        let cached = self.transforms.get(hash)?;
        let n = cached.n as usize;
        let is_lab = cached.is_lab;

        // Normalize input values to [0,1] range.
        // Lab profiles need special mapping: L/100, (a+128)/255, (b+128)/255.
        let mut src = vec![0.0f64; n];
        for (i, s) in src.iter_mut().enumerate() {
            let v = components.get(i).copied().unwrap_or(0.0);
            *s = if is_lab {
                match i {
                    0 => (v / 100.0).clamp(0.0, 1.0),
                    _ => ((v + 128.0) / 255.0).clamp(0.0, 1.0),
                }
            } else {
                v.clamp(0.0, 1.0)
            };
        }

        // Quantize normalized values for cache key
        let hash_prefix = u64::from_le_bytes(hash[..8].try_into().ok()?);
        let mut quantized = [0u16; 4];
        for (i, &c) in src.iter().take(4).enumerate() {
            quantized[i] = (c * 65535.0).round() as u16;
        }

        // Check cache
        let cache_key = (hash_prefix, quantized);
        if let Some(&cached) = self.color_cache.get(&cache_key) {
            return Some(cached);
        }

        let mut dst = [0.0f64; 3];
        if cached.transform_f64.transform(&src, &mut dst).is_err() {
            return None;
        }

        let dst = bpc_post_correct(dst, cached.bpc_params.as_ref());
        let result = (
            dst[0].clamp(0.0, 1.0),
            dst[1].clamp(0.0, 1.0),
            dst[2].clamp(0.0, 1.0),
        );

        // Cache (limit size to avoid unbounded growth)
        if self.color_cache.len() < 65536 {
            self.color_cache.insert(cache_key, result);
        }

        Some(result)
    }

    /// Convert a single color through an ICC profile (read-only, no caching).
    ///
    /// Same as `convert_color` but takes `&self` instead of `&mut self`,
    /// suitable for use from immutable contexts like rendering.
    pub fn convert_color_readonly(
        &self,
        hash: &ProfileHash,
        components: &[f64],
    ) -> Option<(f64, f64, f64)> {
        let cached = self.transforms.get(hash)?;
        let n = cached.n as usize;
        let is_lab = cached.is_lab;

        let mut src = vec![0.0f64; n];
        for (i, s) in src.iter_mut().enumerate() {
            let v = components.get(i).copied().unwrap_or(0.0);
            *s = if is_lab {
                match i {
                    0 => (v / 100.0).clamp(0.0, 1.0),
                    _ => ((v + 128.0) / 255.0).clamp(0.0, 1.0),
                }
            } else {
                v.clamp(0.0, 1.0)
            };
        }

        let mut dst = [0.0f64; 3];
        if cached.transform_f64.transform(&src, &mut dst).is_err() {
            return None;
        }

        let dst = bpc_post_correct(dst, cached.bpc_params.as_ref());
        Some((
            dst[0].clamp(0.0, 1.0),
            dst[1].clamp(0.0, 1.0),
            dst[2].clamp(0.0, 1.0),
        ))
    }

    /// Bulk-convert 8-bit image samples through an ICC profile to RGB.
    /// Input: packed samples (Gray/RGB/CMYK depending on profile).
    /// Output: packed RGB bytes (3 bytes per pixel).
    pub fn convert_image_8bit(
        &self,
        hash: &ProfileHash,
        samples: &[u8],
        pixel_count: usize,
    ) -> Option<Vec<u8>> {
        let cached = self.transforms.get(hash)?;
        let n = cached.n as usize;
        let expected_len = pixel_count * n;
        if samples.len() < expected_len {
            return None;
        }

        // Fast path: pre-baked 4D CLUT for CMYK profiles. BPC is already
        // baked into the CLUT (when enabled), so no per-pixel correction
        // is needed here.
        if let Some(clut) = &cached.clut4 {
            return Some(apply_clut4_cmyk_to_rgb(
                clut,
                &samples[..expected_len],
                pixel_count,
            ));
        }

        let src = &samples[..expected_len];
        let mut dst = vec![0u8; pixel_count * 3];

        match cached.transform_8bit.transform(src, &mut dst) {
            Ok(()) => {
                // Apply BPC per pixel for non-CLUT bulk paths (CMYK profiles
                // whose CLUT bake failed; today no other layouts populate
                // bpc_params, so this is a no-op for RGB/Gray/Lab).
                if let Some(p) = cached.bpc_params.as_ref() {
                    for px in dst.chunks_exact_mut(3) {
                        let out = apply_bpc_rgb_u8([px[0], px[1], px[2]], p);
                        px[0] = out[0];
                        px[1] = out[1];
                        px[2] = out[2];
                    }
                }
                Some(dst)
            }
            Err(e) => {
                eprintln!("[ICC] Image transform failed: {e}");
                None
            }
        }
    }

    /// Search system paths for a CMYK ICC profile and register it.
    pub fn search_system_cmyk_profile(&mut self) {
        if let Some(bytes) = find_system_cmyk_profile()
            && let Some(hash) = self.register_profile(&bytes)
        {
            eprintln!("[ICC] Loaded system CMYK profile");
            self.system_cmyk_bytes = Some(Arc::new(bytes));
            self.default_cmyk_hash = Some(hash);
        }
    }

    /// Load a CMYK ICC profile from raw bytes (for environments without filesystem access).
    pub fn load_cmyk_profile_bytes(&mut self, bytes: &[u8]) {
        if let Some(hash) = self.register_profile(bytes) {
            self.system_cmyk_bytes = Some(Arc::new(bytes.to_vec()));
            self.default_cmyk_hash = Some(hash);
        }
    }

    /// Get the default CMYK profile hash, if a system CMYK profile was found.
    pub fn default_cmyk_hash(&self) -> Option<&ProfileHash> {
        self.default_cmyk_hash.as_ref()
    }

    /// Check if a profile hash has been registered.
    pub fn has_profile(&self, hash: &ProfileHash) -> bool {
        self.transforms.contains_key(hash)
    }

    /// Get the raw bytes of a registered ICC profile (for PDF embedding).
    pub fn get_profile_bytes(&self, hash: &ProfileHash) -> Option<Arc<Vec<u8>>> {
        self.raw_bytes.get(hash).cloned()
    }

    /// Get the raw bytes of the system CMYK profile (for re-registration in render threads).
    pub fn system_cmyk_bytes(&self) -> Option<&Arc<Vec<u8>>> {
        self.system_cmyk_bytes.as_ref()
    }

    /// Set the system CMYK profile from pre-loaded bytes and hash.
    ///
    /// Used by `--output-profile` to substitute the auto-detected system CMYK
    /// profile with a user-specified one.
    pub fn set_system_cmyk(&mut self, bytes: &[u8], hash: ProfileHash) {
        self.system_cmyk_bytes = Some(Arc::new(bytes.to_vec()));
        self.default_cmyk_hash = Some(hash);
    }

    /// Set the default CMYK profile hash (used when building render-thread caches).
    pub fn set_default_cmyk_hash(&mut self, hash: ProfileHash) {
        self.default_cmyk_hash = Some(hash);
    }

    /// Temporarily remove the default CMYK hash, returning the old value.
    /// Used to disable ICC CMYK conversion inside soft mask form rendering,
    /// where PLRM formulas produce correct luminosity values (ICC profiles
    /// map 100% K to non-zero RGB, breaking luminosity soft masks).
    pub fn suspend_default_cmyk(&mut self) -> Option<ProfileHash> {
        self.default_cmyk_hash.take()
    }

    /// Restore a previously suspended default CMYK hash.
    pub fn restore_default_cmyk(&mut self, hash: Option<ProfileHash>) {
        self.default_cmyk_hash = hash;
    }

    /// Convert CMYK to (r, g, b) using the default system CMYK profile.
    /// Returns None if no system CMYK profile is loaded.
    #[inline]
    pub fn convert_cmyk(&mut self, c: f64, m: f64, y: f64, k: f64) -> Option<(f64, f64, f64)> {
        let hash = *self.default_cmyk_hash.as_ref()?;
        self.convert_color(&hash, &[c, m, y, k])
    }

    /// Convert CMYK to (r, g, b) using the default system CMYK profile (read-only, no caching).
    /// Used by band renderers that only have `&self` access.
    pub fn convert_cmyk_readonly(&self, c: f64, m: f64, y: f64, k: f64) -> Option<(f64, f64, f64)> {
        let hash = self.default_cmyk_hash.as_ref()?;
        let cached = self.transforms.get(hash)?;
        let src = [
            c.clamp(0.0, 1.0),
            m.clamp(0.0, 1.0),
            y.clamp(0.0, 1.0),
            k.clamp(0.0, 1.0),
        ];
        let mut dst = [0.0f64; 3];
        if cached.transform_f64.transform(&src, &mut dst).is_err() {
            return None;
        }
        let dst = bpc_post_correct(dst, cached.bpc_params.as_ref());
        Some((
            dst[0].clamp(0.0, 1.0),
            dst[1].clamp(0.0, 1.0),
            dst[2].clamp(0.0, 1.0),
        ))
    }

    /// Build the lazy sRGB→CMYK reverse transform from the system CMYK profile.
    /// Returns `Some(())` if the transform is now present (built or already
    /// cached). Returns `None` if no system CMYK profile is registered or no
    /// rendering intent could create a transform.
    fn ensure_reverse_cmyk_transform(&mut self) -> Option<()> {
        if self.reverse_cmyk_f64.is_some() {
            return Some(());
        }
        let hash = *self.default_cmyk_hash.as_ref()?;
        let cmyk_profile = self.profiles.get(&hash)?.clone();
        let intents = [
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::Perceptual,
            RenderingIntent::AbsoluteColorimetric,
            RenderingIntent::Saturation,
        ];
        for &intent in &intents {
            let options = TransformOptions {
                rendering_intent: intent,
                ..TransformOptions::default()
            };
            if let Ok(t) = self.srgb_profile.create_transform_f64(
                Layout::Rgb,
                &cmyk_profile,
                Layout::Rgba,
                options,
            ) {
                self.reverse_cmyk_f64 = Some(t);
                return Some(());
            }
        }
        None
    }

    /// Pre-warm the lazy sRGB→CMYK reverse transform. Should be called once on
    /// the build thread that owns `&mut IccCache`, after the system CMYK
    /// profile has been registered, so that band renderers (which only hold
    /// `&IccCache`) can use [`Self::convert_rgb_to_cmyk_readonly`] without
    /// having to mutate state.
    pub fn prepare_reverse_cmyk(&mut self) {
        let _ = self.ensure_reverse_cmyk_transform();
    }

    /// Convert an sRGB color to CMYK using the system CMYK profile, without
    /// mutating any state. Returns `None` when the reverse transform has not
    /// been pre-built (call [`Self::prepare_reverse_cmyk`] first) or when no
    /// system CMYK profile is registered.
    ///
    /// The returned components are clamped to `[0, 1]`.
    pub fn convert_rgb_to_cmyk_readonly(&self, r: f64, g: f64, b: f64) -> Option<[f64; 4]> {
        let reverse = self.reverse_cmyk_f64.as_ref()?;
        let src_rgb = [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)];
        let mut cmyk = [0.0f64; 4];
        reverse.transform(&src_rgb, &mut cmyk).ok()?;
        Some([
            cmyk[0].clamp(0.0, 1.0),
            cmyk[1].clamp(0.0, 1.0),
            cmyk[2].clamp(0.0, 1.0),
            cmyk[3].clamp(0.0, 1.0),
        ])
    }

    /// Round-trip an RGB color through the system CMYK profile: sRGB→CMYK→sRGB.
    /// Used when compositing in a DeviceCMYK page group — saturated RGB colors
    /// become more muted after passing through the CMYK gamut.
    /// Returns None if no CMYK profile is loaded.
    pub fn round_trip_rgb_via_cmyk(&mut self, r: f64, g: f64, b: f64) -> Option<(f64, f64, f64)> {
        self.ensure_reverse_cmyk_transform()?;
        let hash = *self.default_cmyk_hash.as_ref()?;
        let reverse = self.reverse_cmyk_f64.as_ref()?;

        // sRGB → CMYK
        let src_rgb = [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)];
        let mut cmyk = [0.0f64; 4];
        reverse.transform(&src_rgb, &mut cmyk).ok()?;

        // CMYK → sRGB (via existing forward transform)
        let forward = self.transforms.get(&hash)?;
        let mut dst = [0.0f64; 3];
        forward.transform_f64.transform(&cmyk, &mut dst).ok()?;

        let dst = bpc_post_correct(dst, forward.bpc_params.as_ref());
        Some((
            dst[0].clamp(0.0, 1.0),
            dst[1].clamp(0.0, 1.0),
            dst[2].clamp(0.0, 1.0),
        ))
    }

    /// Disable all ICC color management — clears all profiles, transforms,
    /// and caches. Equivalent to the CLI's `--no-icc` flag.
    pub fn disable(&mut self) {
        self.profiles.clear();
        self.transforms.clear();
        self.color_cache.clear();
        self.raw_bytes.clear();
        self.default_cmyk_hash = None;
        self.system_cmyk_bytes = None;
        self.reverse_cmyk_f64 = None;
    }
}

/// Bake a 4D CLUT by sampling an 8-bit CMYK→sRGB transform on a regular grid.
///
/// Generates `grid_n^4` CMYK sample points (each channel stepping `0..=255` in
/// `grid_n` steps), invokes moxcms once on the full batch, and stores the
/// packed sRGB output. Storage order is K outermost, then Y, M, C innermost,
/// matching the interpolation access pattern in `apply_clut4_cmyk_to_rgb`.
///
/// Returns `None` if the transform invocation fails — callers fall back to
/// direct moxcms calls per image.
fn bake_clut4(
    transform: &(dyn TransformExecutor<u8> + Send + Sync),
    grid_n: u8,
    bpc_params: Option<&BpcParams>,
) -> Option<Clut4> {
    let n = grid_n as usize;
    if !(2..=33).contains(&n) {
        return None;
    }
    let total = n * n * n * n;
    // Sample grid: for each (k, y, m, c) grid index, emit bytes (c, m, y, k).
    // moxcms consumes this as packed 4-channel input.
    let mut src = Vec::with_capacity(total * 4);
    let step = |i: usize| -> u8 {
        // Spread grid indices evenly across 0..=255 (endpoints inclusive).
        ((i as u32 * 255) / (n as u32 - 1)) as u8
    };
    for k in 0..n {
        let kv = step(k);
        for y in 0..n {
            let yv = step(y);
            for m in 0..n {
                let mv = step(m);
                for c in 0..n {
                    let cv = step(c);
                    src.extend_from_slice(&[cv, mv, yv, kv]);
                }
            }
        }
    }
    let mut dst = vec![0u8; total * 3];
    transform.transform(&src, &mut dst).ok()?;

    // Bake BPC into every grid point so runtime CLUT lookup stays at zero
    // per-pixel cost.
    if let Some(p) = bpc_params {
        for px in dst.chunks_exact_mut(3) {
            let out = apply_bpc_rgb_u8([px[0], px[1], px[2]], p);
            px[0] = out[0];
            px[1] = out[1];
            px[2] = out[2];
        }
    }

    Some(Clut4 {
        grid_n,
        data: Arc::new(dst),
    })
}

/// Convert an 8-bit packed CMYK buffer to 8-bit packed sRGB using the baked
/// 4D CLUT. For each pixel: bracket the K axis into two slices, run 3D
/// tetrahedral (Kasson) interpolation on (C,M,Y) in each slice, then linearly
/// blend the two results by the K fraction.
///
/// This preserves the profile's behavior across the K axis (UCR/black-point
/// transitions) while giving image-rate throughput.
fn apply_clut4_cmyk_to_rgb(clut: &Clut4, src: &[u8], pixel_count: usize) -> Vec<u8> {
    let n = clut.grid_n as usize;
    let nm1 = (n - 1) as u32;
    let lut = clut.data.as_slice();

    // Strides in bytes within the flat LUT (K outermost, then Y, M; C innermost).
    let stride_c: usize = 3;
    let stride_m: usize = n * stride_c;
    let stride_y: usize = n * stride_m;
    let stride_k: usize = n * stride_y;

    let mut out = vec![0u8; pixel_count * 3];

    // Per-axis: quantize byte → (lo_idx, hi_idx, frac_in_0_255).
    #[inline(always)]
    fn axis(v: u8, nm1: u32) -> (usize, usize, u32) {
        let scaled = v as u32 * nm1;
        let lo = scaled / 255;
        let frac = scaled - lo * 255;
        let hi = if lo < nm1 { lo + 1 } else { lo };
        (lo as usize, hi as usize, frac)
    }

    for i in 0..pixel_count {
        let o = i * 4;
        let c = src[o];
        let m = src[o + 1];
        let y = src[o + 2];
        let k = src[o + 3];

        let (ci, ci1, fc) = axis(c, nm1);
        let (mi, mi1, fm) = axis(m, nm1);
        let (yi, yi1, fy) = axis(y, nm1);
        let (ki, ki1, fk) = axis(k, nm1);

        // Pick tetrahedron vertices and sorted weights ONCE per pixel
        // (previously done per channel — 3× waste). Kasson '94:
        //   out = V000 + (Va - V000)*w1 + (Vb - Va)*w2 + (V111 - Vb)*w3
        // with w1 >= w2 >= w3 and Va, Vb the two intermediate corners.
        let (a_dxmy, b_dxmy, w1, w2, w3) = if fc >= fm {
            if fm >= fy {
                // C,M,Y
                ((1, 0, 0), (1, 1, 0), fc, fm, fy)
            } else if fc >= fy {
                // C,Y,M
                ((1, 0, 0), (1, 0, 1), fc, fy, fm)
            } else {
                // Y,C,M
                ((0, 0, 1), (1, 0, 1), fy, fc, fm)
            }
        } else if fc >= fy {
            // M,C,Y
            ((0, 1, 0), (1, 1, 0), fm, fc, fy)
        } else if fm >= fy {
            // M,Y,C
            ((0, 1, 0), (0, 1, 1), fm, fy, fc)
        } else {
            // Y,M,C
            ((0, 0, 1), (0, 1, 1), fy, fm, fc)
        };

        // Map tetrahedron corner selector (dc, dm, dy) → LUT offset within a K slice.
        let corner = |d: (u8, u8, u8)| -> usize {
            let (dc, dm, dy) = d;
            let cx = if dc == 0 { ci } else { ci1 };
            let mx = if dm == 0 { mi } else { mi1 };
            let yx = if dy == 0 { yi } else { yi1 };
            yx * stride_y + mx * stride_m + cx * stride_c
        };

        let o000 = corner((0, 0, 0));
        let o111 = corner((1, 1, 1));
        let oa = corner(a_dxmy);
        let ob = corner(b_dxmy);

        // Two K slices, 3 channels. Compute inline (no closures, no per-channel branching).
        let base_lo = ki * stride_k;
        let base_hi = ki1 * stride_k;

        // Per-channel tetrahedral formula in integer:
        //   accum = v000*255 + (va - v000)*w1 + (vb - va)*w2 + (v111 - vb)*w3
        // accum is in units of (value * 255), in range [0, 255*255].
        let tetra_channel = |base: usize, ch: usize| -> i32 {
            let v000 = lut[base + o000 + ch] as i32;
            let va = lut[base + oa + ch] as i32;
            let vb = lut[base + ob + ch] as i32;
            let v111 = lut[base + o111 + ch] as i32;
            v000 * 255 + (va - v000) * w1 as i32 + (vb - va) * w2 as i32 + (v111 - vb) * w3 as i32
        };

        let r_lo = tetra_channel(base_lo, 0);
        let g_lo = tetra_channel(base_lo, 1);
        let b_lo = tetra_channel(base_lo, 2);
        let (r_hi, g_hi, b_hi) = if ki == ki1 {
            (r_lo, g_lo, b_lo)
        } else {
            (
                tetra_channel(base_hi, 0),
                tetra_channel(base_hi, 1),
                tetra_channel(base_hi, 2),
            )
        };

        // Linear blend across K slices and rescale to u8.
        let inv_fk = (255 - fk) as i32;
        let fk_i = fk as i32;
        let round = 255 * 255 / 2;
        let finish = |lo: i32, hi: i32| -> u8 {
            let combined = lo * inv_fk + hi * fk_i + round;
            let v = combined / (255 * 255);
            v.clamp(0, 255) as u8
        };

        let di = i * 3;
        out[di] = finish(r_lo, r_hi);
        out[di + 1] = finish(g_lo, g_hi);
        out[di + 2] = finish(b_lo, b_hi);
    }

    out
}

/// Validate a baked CLUT against the direct moxcms transform over a
/// pseudorandom sample of CMYK inputs. Reports median and max per-channel
/// deviation (in u8 units) to stderr. Invoked only when `STET_ICC_VERIFY` is
/// set in the environment.
fn verify_clut4(
    clut: &Clut4,
    transform: &(dyn TransformExecutor<u8> + Send + Sync),
    bpc_params: Option<&BpcParams>,
) {
    const N_SAMPLES: usize = 4096;
    let mut rng: u64 = 0xa8b3c4d5e6f70819;
    let mut next = || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        rng
    };
    let mut cmyk = Vec::with_capacity(N_SAMPLES * 4);
    for _ in 0..N_SAMPLES {
        let r = next();
        cmyk.extend_from_slice(&[
            (r & 0xff) as u8,
            ((r >> 8) & 0xff) as u8,
            ((r >> 16) & 0xff) as u8,
            ((r >> 24) & 0xff) as u8,
        ]);
    }
    let mut reference = vec![0u8; N_SAMPLES * 3];
    if transform.transform(&cmyk, &mut reference).is_err() {
        eprintln!("[ICC VERIFY] reference transform failed");
        return;
    }
    // Mirror the CLUT bake's BPC step in the reference path so the
    // comparison measures interpolation error, not whether BPC was applied.
    if let Some(p) = bpc_params {
        for px in reference.chunks_exact_mut(3) {
            let out = apply_bpc_rgb_u8([px[0], px[1], px[2]], p);
            px[0] = out[0];
            px[1] = out[1];
            px[2] = out[2];
        }
    }
    let interp = apply_clut4_cmyk_to_rgb(clut, &cmyk, N_SAMPLES);
    // Per-pixel Euclidean distance in 8-bit sRGB (crude ΔE proxy).
    let mut dists: Vec<f64> = Vec::with_capacity(N_SAMPLES);
    let mut max_ch: u8 = 0;
    for i in 0..N_SAMPLES {
        let dr = interp[i * 3] as i32 - reference[i * 3] as i32;
        let dg = interp[i * 3 + 1] as i32 - reference[i * 3 + 1] as i32;
        let db = interp[i * 3 + 2] as i32 - reference[i * 3 + 2] as i32;
        let d = ((dr * dr + dg * dg + db * db) as f64).sqrt();
        dists.push(d);
        max_ch = max_ch
            .max(dr.unsigned_abs() as u8)
            .max(dg.unsigned_abs() as u8)
            .max(db.unsigned_abs() as u8);
    }
    dists.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = dists[N_SAMPLES / 2];
    let p99 = dists[(N_SAMPLES * 99) / 100];
    let max = dists[N_SAMPLES - 1];
    eprintln!(
        "[ICC VERIFY] N=17 CLUT vs direct moxcms (sRGB u8): median={:.2}, p99={:.2}, max={:.2}, max_per_channel={}",
        median, p99, max, max_ch
    );
}

/// Search system paths for CMYK ICC profile bytes without parsing or logging.
///
/// Returns the raw bytes suitable for passing to the viewer for ICC-aware rendering.
pub fn find_system_cmyk_profile_bytes() -> Option<Arc<Vec<u8>>> {
    find_system_cmyk_profile().map(Arc::new)
}

/// Search common system paths for a CMYK ICC profile.
fn find_system_cmyk_profile() -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        let paths = [
            "/usr/share/color/icc/ghostscript/default_cmyk.icc",
            "/usr/share/color/icc/ghostscript/ps_cmyk.icc",
            "/usr/share/color/icc/colord/FOGRA39L_coated.icc",
        ];
        for path in &paths {
            if let Ok(bytes) = std::fs::read(path) {
                return Some(bytes);
            }
        }
        // Glob for SWOP profiles
        if let Ok(entries) = glob::glob("/usr/share/color/icc/colord/SWOP*.icc") {
            for entry in entries.flatten() {
                if let Ok(bytes) = std::fs::read(&entry) {
                    return Some(bytes);
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let dirs = [
            "/Library/ColorSync/Profiles",
            "/System/Library/ColorSync/Profiles",
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home_dir = std::path::PathBuf::from(home).join("Library/ColorSync/Profiles");
            if let Some(bytes) = scan_dir_for_cmyk_icc(&home_dir) {
                return Some(bytes);
            }
        }
        for dir in &dirs {
            if let Some(bytes) = scan_dir_for_cmyk_icc(std::path::Path::new(dir)) {
                return Some(bytes);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(sysroot) = std::env::var_os("SYSTEMROOT") {
            let dir = std::path::PathBuf::from(sysroot).join("System32/spool/drivers/color");
            if let Some(bytes) = scan_dir_for_cmyk_icc(&dir) {
                return Some(bytes);
            }
        }
    }

    None
}

/// Scan a directory for ICC files with CMYK color space.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn scan_dir_for_cmyk_icc(dir: &std::path::Path) -> Option<Vec<u8>> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("icc") || ext.eq_ignore_ascii_case("icm") {
            if let Ok(bytes) = std::fs::read(&path) {
                // Check ICC header: color space at offset 16, 'CMYK' = 0x434D594B
                if bytes.len() >= 20 && &bytes[16..20] == b"CMYK" {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_icc_cache_new() {
        let cache = IccCache::new();
        assert!(cache.default_cmyk_hash.is_none());
        assert!(cache.profiles.is_empty());
        assert_eq!(cache.bpc_mode(), BpcMode::Auto);
    }

    #[test]
    fn test_icc_cache_options_default_matches_new() {
        let cache = IccCache::new_with_options(IccCacheOptions::default());
        assert_eq!(cache.bpc_mode(), BpcMode::Auto);
        assert!(cache.default_cmyk_hash.is_none());
    }

    #[test]
    fn test_icc_cache_options_bpc_off() {
        let cache = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::Off,
            source_cmyk_profile: None,
        });
        assert_eq!(cache.bpc_mode(), BpcMode::Off);
        assert!(!cache.bpc_mode().is_enabled());
    }

    #[test]
    fn test_icc_cache_options_preloads_cmyk_profile() {
        // Skip when no system CMYK profile is available.
        let Some(cmyk_bytes) = find_system_cmyk_profile() else {
            return;
        };
        let cache = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::On,
            source_cmyk_profile: Some(cmyk_bytes.clone()),
        });
        assert!(cache.default_cmyk_hash().is_some());
        assert_eq!(cache.bpc_mode(), BpcMode::On);
    }

    #[test]
    fn test_bpc_darkens_pure_k_per_color() {
        // Skip when no system CMYK profile is available.
        let Some(cmyk_bytes) = find_system_cmyk_profile() else {
            return;
        };

        // Without BPC: K=1 through default_cmyk.icc lands near RGB(55, 53, 53)
        // — the profile's as-mapped black projected through moxcms's sRGB B2A.
        let mut off = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::Off,
            source_cmyk_profile: Some(cmyk_bytes.clone()),
        });
        let off_rgb = off.convert_cmyk(0.0, 0.0, 0.0, 1.0).unwrap();

        // With BPC: K=1 must land significantly darker. Adobe Acrobat (lcms2)
        // produces RGB(35, 31, 32). moxcms's sRGB B2A handles very-dark XYZ
        // slightly differently from lcms2, so our post-correct lands a few
        // levels brighter than Acrobat — the meaningful invariant is "K=1 is
        // visibly darker than the no-BPC baseline by a substantial margin."
        let mut on = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::On,
            source_cmyk_profile: Some(cmyk_bytes),
        });
        let on_rgb = on.convert_cmyk(0.0, 0.0, 0.0, 1.0).unwrap();

        // Precondition: this test only exercises BPC's darkening effect, which
        // requires a profile whose black point has non-zero luminance. Some
        // system-supplied CMYK profiles (e.g. macOS's default ColorSync CMYK)
        // already map K=1 to (near-)zero XYZ, so BPC has nothing to correct
        // and off == on. Skip in that case — there's no regression to anchor
        // here, just a profile that doesn't benefit from BPC.
        if (on_rgb.1 - off_rgb.1).abs() < 0.005 {
            eprintln!(
                "Skipping: system CMYK profile's black point is already ~zero; \
                 BPC is a no-op here. off={off_rgb:?} on={on_rgb:?}"
            );
            return;
        }

        assert!(
            on_rgb.1 + 0.03 < off_rgb.1,
            "BPC should darken K=1 by ≥0.03 (~8 RGB levels): off={off_rgb:?} on={on_rgb:?}"
        );
        // And the resulting RGB should be in the "deep gray" range — well
        // under 0.25 (RGB ≤ ~64) on every channel.
        assert!(
            on_rgb.0 < 0.25 && on_rgb.1 < 0.25 && on_rgb.2 < 0.25,
            "Expected deep gray after BPC, got {on_rgb:?}"
        );
    }

    #[test]
    fn test_bpc_white_anchored_per_color() {
        // Skip when no system CMYK profile is available.
        let Some(cmyk_bytes) = find_system_cmyk_profile() else {
            return;
        };
        let mut cache = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::On,
            source_cmyk_profile: Some(cmyk_bytes),
        });
        // CMYK white (no ink) must still render as sRGB white under BPC.
        let (r, g, b) = cache.convert_cmyk(0.0, 0.0, 0.0, 0.0).unwrap();
        assert!(r > 0.99 && g > 0.99 && b > 0.99, "({r}, {g}, {b})");
    }

    #[test]
    fn test_bpc_image_clut_path_darkens_pure_k() {
        // Skip when no system CMYK profile is available.
        let Some(cmyk_bytes) = find_system_cmyk_profile() else {
            return;
        };

        // Build a 1-pixel CMYK image at K=1 and route it through the CLUT.
        // Without BPC vs with BPC, the K=1 pixel must shift darker, mirroring
        // the per-color path behaviour.
        let off = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::Off,
            source_cmyk_profile: Some(cmyk_bytes.clone()),
        });
        let on = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::On,
            source_cmyk_profile: Some(cmyk_bytes),
        });
        let off_hash = *off.default_cmyk_hash().unwrap();
        let on_hash = *on.default_cmyk_hash().unwrap();

        let pixel = [0u8, 0, 0, 255]; // C=0 M=0 Y=0 K=255
        let off_rgb = off.convert_image_8bit(&off_hash, &pixel, 1).unwrap();
        let on_rgb = on.convert_image_8bit(&on_hash, &pixel, 1).unwrap();

        // Precondition, same as `test_bpc_darkens_pure_k_per_color`: BPC only
        // shifts pixels when the profile's black point has non-zero luminance.
        // Skip when the system profile already maps K=1 to near-zero XYZ.
        if (on_rgb[1] as i32 - off_rgb[1] as i32).abs() <= 1 {
            eprintln!(
                "Skipping: system CMYK profile's black point is already ~zero; \
                 BPC is a no-op here. off={off_rgb:?} on={on_rgb:?}"
            );
            return;
        }

        // BPC must darken the green channel by ≥8 RGB levels (mirrors the
        // per-color path's anchor in test_bpc_darkens_pure_k_per_color).
        assert!(
            (on_rgb[1] as i32) + 8 < (off_rgb[1] as i32),
            "CLUT BPC should darken K=1 image green by ≥8 levels: off={off_rgb:?} on={on_rgb:?}"
        );
        // And land in the deep-gray range.
        assert!(
            on_rgb[0] < 64 && on_rgb[1] < 64 && on_rgb[2] < 64,
            "Expected deep gray after CLUT BPC, got {on_rgb:?}"
        );
    }

    #[test]
    fn test_bpc_off_image_matches_per_color_off() {
        // With --bpc off, the bulk image path's K=1 output must match the
        // per-color path's K=1 output (within u8 quantization). Anchors that
        // disabling BPC reproduces stet's pre-fix behaviour bit-for-bit on
        // the dominant CMYK image path.
        let Some(cmyk_bytes) = find_system_cmyk_profile() else {
            return;
        };
        let mut cache = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::Off,
            source_cmyk_profile: Some(cmyk_bytes),
        });
        let hash = *cache.default_cmyk_hash().unwrap();

        let pixel = [0u8, 0, 0, 255];
        let img = cache.convert_image_8bit(&hash, &pixel, 1).unwrap();
        let (r, g, b) = cache.convert_cmyk(0.0, 0.0, 0.0, 1.0).unwrap();
        let pc = [
            (r * 255.0).round() as i32,
            (g * 255.0).round() as i32,
            (b * 255.0).round() as i32,
        ];
        // CLUT interpolation drift can introduce ±1 vs the direct f64 path.
        assert!((img[0] as i32 - pc[0]).abs() <= 2, "img={img:?} pc={pc:?}");
        assert!((img[1] as i32 - pc[1]).abs() <= 2, "img={img:?} pc={pc:?}");
        assert!((img[2] as i32 - pc[2]).abs() <= 2, "img={img:?} pc={pc:?}");
    }

    #[test]
    fn test_register_invalid_profile() {
        let mut cache = IccCache::new();
        assert!(cache.register_profile(b"not a valid ICC profile").is_none());
    }

    #[test]
    fn test_srgb_identity_transform() {
        // Create an sRGB profile, register it, and verify identity-ish conversion
        let srgb = ColorProfile::new_srgb();
        let bytes = srgb.encode().unwrap();
        let mut cache = IccCache::new();
        let hash = cache.register_profile(&bytes).unwrap();

        // Red should stay approximately red
        let (r, g, b) = cache.convert_color(&hash, &[1.0, 0.0, 0.0]).unwrap();
        assert!((r - 1.0).abs() < 0.02, "r={r}");
        assert!(g < 0.02, "g={g}");
        assert!(b < 0.02, "b={b}");

        // White
        let (r, g, b) = cache.convert_color(&hash, &[1.0, 1.0, 1.0]).unwrap();
        assert!((r - 1.0).abs() < 0.02);
        assert!((g - 1.0).abs() < 0.02);
        assert!((b - 1.0).abs() < 0.02);
    }

    #[test]
    fn test_srgb_image_transform() {
        let srgb = ColorProfile::new_srgb();
        let bytes = srgb.encode().unwrap();
        let mut cache = IccCache::new();
        let hash = cache.register_profile(&bytes).unwrap();

        // 2 pixels: red, green
        let src = [255u8, 0, 0, 0, 255, 0];
        let result = cache.convert_image_8bit(&hash, &src, 2).unwrap();
        assert_eq!(result.len(), 6);
        // Red pixel should be approximately (255, 0, 0)
        assert!(result[0] > 240);
        assert!(result[1] < 15);
        assert!(result[2] < 15);
    }

    #[test]
    fn test_convert_rgb_to_cmyk_readonly() {
        // Skip when no system CMYK profile is available (CI without ICC packs).
        let Some(cmyk_bytes) = find_system_cmyk_profile() else {
            return;
        };
        let mut cache = IccCache::new();
        let hash = cache.register_profile(&cmyk_bytes).unwrap();
        cache.set_default_cmyk_hash(hash);

        // Before pre-warming the reverse transform must be unavailable.
        assert!(cache.convert_rgb_to_cmyk_readonly(0.0, 0.0, 0.0).is_none());

        cache.prepare_reverse_cmyk();

        // Pure black sRGB should land deep in K (any reasonable CMYK profile
        // produces a high K component).
        let cmyk = cache
            .convert_rgb_to_cmyk_readonly(0.0, 0.0, 0.0)
            .expect("reverse transform should be available after prepare");
        assert!(
            cmyk[3] > 0.5,
            "expected K>0.5 for sRGB black, got cmyk={cmyk:?}"
        );

        // Pure white sRGB should land near (0,0,0,0) — minimal ink.
        let cmyk = cache
            .convert_rgb_to_cmyk_readonly(1.0, 1.0, 1.0)
            .expect("reverse transform should be available");
        for (i, v) in cmyk.iter().enumerate() {
            assert!(*v < 0.05, "expected near-zero ink at chan {i}, got {v}");
        }
    }
}
