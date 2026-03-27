// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! ICC color profile support via moxcms.
//!
//! Parses embedded ICC profiles from `[/ICCBased stream]` color spaces and
//! converts colors to sRGB. Also searches for system CMYK profiles to improve
//! DeviceCMYK → RGB conversion beyond the naive PLRM formula.

use moxcms::{
    CmsError, ColorProfile, DataColorSpace, Layout, RenderingIntent, TransformExecutor,
    TransformOptions,
};
use std::collections::HashMap;
use std::sync::Arc;

/// SHA-256 hash used as profile key.
pub type ProfileHash = [u8; 32];

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
}

impl Default for IccCache {
    fn default() -> Self {
        Self::new()
    }
}

impl IccCache {
    /// Create an empty ICC cache.
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
            transforms: HashMap::new(),
            color_cache: HashMap::new(),
            default_cmyk_hash: None,
            system_cmyk_bytes: None,
            raw_bytes: HashMap::new(),
            srgb_profile: ColorProfile::new_srgb(),
            reverse_cmyk_f64: None,
        }
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
        self.profiles.insert(hash, Arc::new(profile));
        self.transforms.insert(
            hash,
            CachedTransform {
                transform_8bit,
                transform_f64,
                n,
                is_lab,
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

        let src = &samples[..expected_len];
        let mut dst = vec![0u8; pixel_count * 3];

        match cached.transform_8bit.transform(src, &mut dst) {
            Ok(()) => Some(dst),
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
        Some((
            dst[0].clamp(0.0, 1.0),
            dst[1].clamp(0.0, 1.0),
            dst[2].clamp(0.0, 1.0),
        ))
    }

    /// Round-trip an RGB color through the system CMYK profile: sRGB→CMYK→sRGB.
    /// Used when compositing in a DeviceCMYK page group — saturated RGB colors
    /// become more muted after passing through the CMYK gamut.
    /// Returns None if no CMYK profile is loaded.
    pub fn round_trip_rgb_via_cmyk(&mut self, r: f64, g: f64, b: f64) -> Option<(f64, f64, f64)> {
        let hash = *self.default_cmyk_hash.as_ref()?;

        // Build reverse (sRGB→CMYK) transform lazily
        if self.reverse_cmyk_f64.is_none() {
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
                    break;
                }
            }
        }

        let reverse = self.reverse_cmyk_f64.as_ref()?;

        // sRGB → CMYK
        let src_rgb = [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)];
        let mut cmyk = [0.0f64; 4];
        reverse.transform(&src_rgb, &mut cmyk).ok()?;

        // CMYK → sRGB (via existing forward transform)
        let forward = self.transforms.get(&hash)?;
        let mut dst = [0.0f64; 3];
        forward.transform_f64.transform(&cmyk, &mut dst).ok()?;

        Some((
            dst[0].clamp(0.0, 1.0),
            dst[1].clamp(0.0, 1.0),
            dst[2].clamp(0.0, 1.0),
        ))
    }

    /// Disable all ICC color management (equivalent to PostForge's `--no-icc`).
    /// Clears all profiles, transforms, and caches.
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
}
