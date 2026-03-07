// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! ICC color profile support via moxcms.
//!
//! Parses embedded ICC profiles from `[/ICCBased stream]` color spaces and
//! converts colors to sRGB. Also searches for system CMYK profiles to improve
//! DeviceCMYK → RGB conversion beyond the naive PLRM formula.

use moxcms::{
    ColorProfile, DataColorSpace, Layout, TransformExecutor, TransformOptions,
};
use std::collections::HashMap;
use std::sync::Arc;

/// SHA-256 hash used as profile key.
pub type ProfileHash = [u8; 32];

/// Cached ICC transform to sRGB (specific to source layout).
struct CachedTransform {
    /// 8-bit transform for image data.
    transform_8bit: Arc<dyn TransformExecutor<u8> + Send + Sync>,
    /// f64 transform for single-color conversions.
    transform_f64: Arc<dyn TransformExecutor<f64> + Send + Sync>,
    /// Number of source components.
    n: u32,
}

/// ICC color profile cache and transform manager.
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
    /// sRGB output profile (created once).
    srgb_profile: ColorProfile,
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
            srgb_profile: ColorProfile::new_srgb(),
        }
    }

    /// Register an ICC profile from raw bytes. Returns the SHA-256 hash on success.
    pub fn register_profile(&mut self, bytes: &[u8]) -> Option<ProfileHash> {
        use sha2::{Digest, Sha256};
        let hash: ProfileHash = Sha256::digest(bytes).into();

        // Already registered?
        if self.transforms.contains_key(&hash) {
            return Some(hash);
        }

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

        let (src_layout_8, src_layout_f64) = match n {
            1 => (Layout::Gray, Layout::Gray),
            3 => (Layout::Rgb, Layout::Rgb),
            4 => (Layout::Rgba, Layout::Rgba),
            _ => return None,
        };

        let options = TransformOptions::default();

        let dst_layout_8 = Layout::Rgb;
        let dst_layout_f64 = Layout::Rgb;

        let transform_8bit = match profile.create_transform_8bit(
            src_layout_8,
            &self.srgb_profile,
            dst_layout_8,
            options,
        ) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[ICC] Failed to create 8-bit transform: {e}");
                return None;
            }
        };

        let transform_f64 = match profile.create_transform_f64(
            src_layout_f64,
            &self.srgb_profile,
            dst_layout_f64,
            options,
        ) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[ICC] Failed to create f64 transform: {e}");
                return None;
            }
        };

        self.profiles.insert(hash, Arc::new(profile));
        self.transforms.insert(
            hash,
            CachedTransform {
                transform_8bit,
                transform_f64,
                n,
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
        // Quantize to 16-bit for cache key
        let hash_prefix = u64::from_le_bytes(hash[..8].try_into().ok()?);
        let mut quantized = [0u16; 4];
        for (i, &c) in components.iter().take(4).enumerate() {
            quantized[i] = (c.clamp(0.0, 1.0) * 65535.0).round() as u16;
        }

        // Check cache
        let cache_key = (hash_prefix, quantized);
        if let Some(&cached) = self.color_cache.get(&cache_key) {
            return Some(cached);
        }

        let cached = self.transforms.get(hash)?;
        let n = cached.n as usize;

        let mut src = vec![0.0f64; n];
        for (i, s) in src.iter_mut().enumerate() {
            *s = components.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
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

    /// Convert CMYK to (r, g, b) using the default system CMYK profile.
    /// Returns None if no system CMYK profile is loaded.
    #[inline]
    pub fn convert_cmyk(&mut self, c: f64, m: f64, y: f64, k: f64) -> Option<(f64, f64, f64)> {
        let hash = *self.default_cmyk_hash.as_ref()?;
        self.convert_color(&hash, &[c, m, y, k])
    }

    /// Disable all ICC color management (equivalent to PostForge's `--no-icc`).
    /// Clears all profiles, transforms, and caches.
    pub fn disable(&mut self) {
        self.profiles.clear();
        self.transforms.clear();
        self.color_cache.clear();
        self.default_cmyk_hash = None;
    }
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
            let dir =
                std::path::PathBuf::from(sysroot).join("System32/spool/drivers/color");
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
