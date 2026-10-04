// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output keeps the function a mesh or patch shading takes its colours
//! from.
//!
//! For such a shading the reader carries each vertex's function input,
//! sampling the function into a colour table for the renderer. The writer
//! wrote that input as the vertex's colour and dropped the function, so a
//! gray or spot shading came out as a ramp of its parameter — a flat 0.8
//! gray as black to white — and a shading in any other space lost the
//! function's curve between vertices.

use std::path::{Path, PathBuf};
use std::process::Command;

use stet_graphics::device::ShadingColorSpace;
use stet_graphics::display_list::DisplayElement;

/// Path to the `stet` binary built alongside this test.
fn stet_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join(if cfg!(windows) { "stet.exe" } else { "stet" })
}

/// A scratch directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "stet-pdf-shading-functions-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A one-page PDF whose content is `/Sh0 sh`, the shading being `shading`
/// (its dictionary entries) over `data`.
fn shading_pdf(shading: &str, data: &[u8]) -> Vec<u8> {
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 500 500] \
           /Resources << /Shading << /Sh0 5 0 R >> >> /Contents 4 0 R >>"
            .to_vec(),
        b"<< /Length 7 >>\nstream\n/Sh0 sh\nendstream".to_vec(),
        [
            format!("<< {shading} /Length {} >>\nstream\n", data.len()).into_bytes(),
            data.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A coordinate in `[0, 500]` as 16 bits, for `/Decode [0 500 …]`.
fn coord(v: f64) -> [u8; 2] {
    ((v / 500.0 * 65535.0).round() as u16).to_be_bytes()
}

/// A Type 4 mesh of two triangles over a square, its function input 0 on
/// the left and 1 on the right.
fn mesh_data() -> Vec<u8> {
    let vertex = |x: f64, y: f64, t: u8| [&[0u8][..], &coord(x), &coord(y), &[t]].concat();
    [
        vertex(50.0, 50.0, 0),
        vertex(450.0, 50.0, 255),
        vertex(50.0, 450.0, 0),
        vertex(450.0, 450.0, 255),
        vertex(450.0, 50.0, 255),
        vertex(50.0, 450.0, 0),
    ]
    .concat()
}

/// A Type 6 Coons patch over the same square, input 0 at its left corners
/// and 1 at its right.
fn patch_data() -> Vec<u8> {
    // Boundary from (50,50) up the left, along the top, down the right and
    // back along the bottom; corners (50,50) (50,450) (450,450) (450,50).
    let points = [
        (50.0, 50.0),
        (50.0, 183.0),
        (50.0, 316.0),
        (50.0, 450.0),
        (183.0, 450.0),
        (316.0, 450.0),
        (450.0, 450.0),
        (450.0, 316.0),
        (450.0, 183.0),
        (450.0, 50.0),
        (316.0, 50.0),
        (183.0, 50.0),
    ];
    let mut data = vec![0u8];
    for (x, y) in points {
        data.extend(coord(x));
        data.extend(coord(y));
    }
    data.extend([0u8, 0, 255, 255]);
    data
}

/// What the reader makes of a page's first mesh or patch shading.
struct Shading {
    /// The colour table, converted.
    lut: Vec<[f64; 3]>,
    /// The table's samples in the shading's colour space.
    components: Option<Vec<Vec<f64>>>,
    color_space: ShadingColorSpace,
}

/// The first mesh or patch shading on the page.
fn read_shading(bytes: &[u8]) -> Shading {
    let doc = stet_pdf_reader::PdfDocument::from_bytes(bytes).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    for e in list.elements() {
        let (lut, comps, cs) = match e {
            DisplayElement::MeshShading { params } => (
                &params.color_lut,
                &params.color_lut_components,
                &params.color_space,
            ),
            DisplayElement::PatchShading { params } => (
                &params.color_lut,
                &params.color_lut_components,
                &params.color_space,
            ),
            _ => continue,
        };
        let lut = lut.as_ref().expect("the shading is read with its function");
        return Shading {
            lut: lut.iter().map(|c| [c.r, c.g, c.b]).collect(),
            components: comps.as_ref().map(|c| c.to_vec()),
            color_space: cs.clone(),
        };
    }
    panic!("no mesh or patch shading on the page");
}

/// Run the shading through `stet --device pdf` and check the output
/// shades as the input does: the same colour table, the same samples in
/// the same colour space.
fn round_trips(tag: &str, shading: &str, data: &[u8]) -> Vec<Vec<f64>> {
    let dir = TempDir::new(tag);
    let input = dir.path().join("in.pdf");
    let output = dir.path().join("out.pdf");
    std::fs::write(&input, shading_pdf(shading, data)).unwrap();
    let out = Command::new(stet_bin())
        .args(["--device", "pdf", "-o"])
        .arg(&output)
        .arg(&input)
        .output()
        .expect("run stet");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let want = read_shading(&std::fs::read(&input).unwrap());
    let got = read_shading(&std::fs::read(&output).unwrap());
    assert_eq!(
        std::mem::discriminant(&got.color_space),
        std::mem::discriminant(&want.color_space),
        "colour space"
    );
    assert_eq!(got.lut.len(), want.lut.len());
    for (i, (g, w)) in got.lut.iter().zip(&want.lut).enumerate() {
        assert!(
            g.iter().zip(w).all(|(a, b)| (a - b).abs() < 1.0 / 255.0),
            "entry {i}: got {g:?}, want {w:?}"
        );
    }
    let (got_comps, want_comps) = (got.components.unwrap(), want.components.unwrap());
    for (g, w) in got_comps.iter().zip(&want_comps) {
        assert!(g.iter().zip(w).all(|(a, b)| (a - b).abs() < 1e-3));
    }
    got_comps
}

const MESH: &str = "/ShadingType 4 /BitsPerCoordinate 16 /BitsPerComponent 8 \
                    /BitsPerFlag 8 /Decode [0 500 0 500 0 1]";

/// The case found in the corpus (4468_5.pdf's soft masks): a gray mesh
/// whose function is constant. Written as its input, it was a ramp.
#[test]
fn a_gray_mesh_keeps_its_function() {
    let comps = round_trips(
        "gray",
        &format!(
            "{MESH} /ColorSpace /DeviceGray \
             /Function << /FunctionType 2 /Domain [0 1] /C0 [0.8] /C1 [0.8] /N 1 >>"
        ),
        &mesh_data(),
    );
    assert!(comps.iter().all(|c| (c[0] - 0.8).abs() < 1e-3));
}

/// A spot shading stays a spot shading, with its tints.
#[test]
fn a_spot_mesh_keeps_its_tints() {
    let comps = round_trips(
        "spot",
        &format!(
            "{MESH} /ColorSpace [/Separation /Gold /DeviceCMYK \
             << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] \
             /Function << /FunctionType 2 /Domain [0 1] /C0 [0.2] /C1 [0.9] /N 1 >>"
        ),
        &mesh_data(),
    );
    assert!((comps[0][0] - 0.2).abs() < 1e-3 && (comps[255][0] - 0.9).abs() < 1e-3);
}

/// A curved function keeps its curve between the vertices: the middle of
/// the table is 0.5³, not the 0.5 interpolating vertex colours would give.
#[test]
fn an_rgb_mesh_keeps_its_curve() {
    let comps = round_trips(
        "rgb",
        &format!(
            "{MESH} /ColorSpace /DeviceRGB \
             /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 3 >>"
        ),
        &mesh_data(),
    );
    assert!((comps[128][0] - (128.0f64 / 255.0).powi(3)).abs() < 1e-3);
}

#[test]
fn a_gray_patch_keeps_its_function() {
    let comps = round_trips(
        "patch",
        "/ShadingType 6 /BitsPerCoordinate 16 /BitsPerComponent 8 /BitsPerFlag 8 \
         /Decode [0 500 0 500 0 1] /ColorSpace /DeviceGray \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [0.3] /C1 [0.3] /N 1 >>",
        &patch_data(),
    );
    assert!(comps.iter().all(|c| (c[0] - 0.3).abs() < 1e-3));
}
