use stet_graphics::icc::{BpcMode, IccCache, IccCacheOptions};

fn dump_profile_shape(bytes: &[u8]) {
    use moxcms::{ColorProfile, LutWarehouse};
    let profile = match ColorProfile::new_from_slice(bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parse error: {e:?}");
            return;
        }
    };
    eprintln!(
        "profile: cs={:?} pcs={:?} version={:?}",
        profile.color_space,
        profile.pcs,
        profile.version()
    );
    let names = [
        (
            "a_to_b_perceptual (A2B0)",
            profile.lut_a_to_b_perceptual.as_ref(),
        ),
        (
            "a_to_b_colorimetric (A2B1)",
            profile.lut_a_to_b_colorimetric.as_ref(),
        ),
        (
            "a_to_b_saturation (A2B2)",
            profile.lut_a_to_b_saturation.as_ref(),
        ),
        (
            "b_to_a_perceptual (B2A0)",
            profile.lut_b_to_a_perceptual.as_ref(),
        ),
    ];
    for (name, slot) in names {
        match slot {
            None => eprintln!("  {name}: missing"),
            Some(LutWarehouse::Lut(lut)) => {
                eprintln!(
                    "  {name}: Lut(in={} out={} grid={} type={:?})",
                    lut.num_input_channels,
                    lut.num_output_channels,
                    lut.num_clut_grid_points,
                    lut.lut_type
                );
            }
            Some(LutWarehouse::Multidimensional(m)) => {
                eprintln!(
                    "  {name}: mAB(in={} out={} grids={:?})",
                    m.num_input_channels, m.num_output_channels, m.grid_points
                );
            }
        }
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: icc_probe <profile.icc>");
    let bytes = std::fs::read(&path).expect("read profile");
    dump_profile_shape(&bytes);
    let cases: &[(f64, f64, f64, f64)] = &[
        (0.15, 1.0, 1.0, 0.0),
        (0.0, 0.6, 1.0, 0.0),
        (0.0, 0.5, 0.9, 0.0),
        (0.0, 0.7, 1.0, 0.0),
        (0.15, 0.7, 1.0, 0.0),
        (0.0, 1.0, 1.0, 0.0),
        (0.0, 0.0, 0.0, 1.0),
        (0.0, 0.0, 0.0, 0.0),
        // Medium-light green range — covers the 0001056.pdf glove regression.
        (0.15, 0.05, 0.30, 0.0),
        (0.25, 0.10, 0.40, 0.0),
        (0.40, 0.15, 0.50, 0.0),
        (0.40, 0.0, 1.0, 0.0),
        (0.60, 0.0, 0.80, 0.0),
        (0.20, 0.20, 0.20, 0.0),
        // Process primaries — likely out of sRGB gamut.
        (1.0, 0.0, 0.0, 0.0),
        (0.0, 1.0, 0.0, 0.0),
        (0.0, 0.0, 1.0, 0.0),
        (1.0, 1.0, 0.0, 0.0),
    ];
    for mode in [BpcMode::Off, BpcMode::On] {
        println!("\n== BPC {:?} ==", mode);
        let opts = IccCacheOptions {
            bpc_mode: mode,
            source_cmyk_profile: Some(bytes.clone()),
        };
        let cache = IccCache::new_with_options(opts);
        for &(c, m, y, k) in cases {
            let rgb = cache.convert_cmyk_readonly(c, m, y, k).expect("convert");
            println!(
                "  CMYK({:.2},{:.2},{:.2},{:.2}) -> ({}, {}, {})",
                c,
                m,
                y,
                k,
                (rgb.0 * 255.0).round() as u8,
                (rgb.1 * 255.0).round() as u8,
                (rgb.2 * 255.0).round() as u8,
            );
        }
    }
}
