// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A mesh shading (Types 4 to 7) may take its `DataSource` from a file as
//! well as from a string or an array (PLRM 3, 4.9.3). Illustrator writes a
//! gradient mesh that way, the data following the dictionary in the program
//! behind a filter on `currentfile`; `shfill` raised `typecheck` for it
//! (issue #6).

use stet::Interpreter;

const COMMON: &str = "/ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 \
                      /Decode [0 255 0 255 0 1 0 1 0 1]";

/// One shading of each mesh type, as `(dictionary entries, hex data)`.
fn meshes() -> Vec<(String, String)> {
    let rgb = ["ff0000", "00ff00", "0000ff", "ffff00"];
    // Type 4: three vertices, each flag x y r g b.
    let t4 = format!("001e1e{} 00c81e{} 0064c8{}", rgb[0], rgb[1], rgb[2]);
    // Type 5: a 2 x 2 lattice, each x y r g b.
    let t5 = format!(
        "1e1e{} c81e{} 1ec8{} c8c8{}",
        rgb[0], rgb[1], rgb[2], rgb[3]
    );
    // Type 6: one Coons patch, flag, twelve boundary points, four colours.
    let boundary = "1e1e 1e50 1e96 1ec8 50c8 96c8 c8c8 c896 c850 c81e 961e 501e";
    let t6 = format!("00 {boundary} {}", rgb.join(" "));
    // Type 7: the same patch with its four interior points.
    let t7 = format!("00 {boundary} 5050 5096 9696 9650 {}", rgb.join(" "));
    vec![
        (format!("/ShadingType 4 /BitsPerFlag 8 {COMMON}"), t4),
        (format!("/ShadingType 5 /VerticesPerRow 2 {COMMON}"), t5),
        (format!("/ShadingType 6 /BitsPerFlag 8 {COMMON}"), t6),
        (format!("/ShadingType 7 /BitsPerFlag 8 {COMMON}"), t7),
    ]
}

fn render(body: &str) -> Vec<u8> {
    let program = format!("%!PS\n<< /PageSize [220 220] >> setpagedevice\n{body}\nshowpage\n");
    let mut pages = Interpreter::new()
        .render(program.as_bytes(), 72.0)
        .expect("program runs");
    assert_eq!(pages.len(), 1);
    pages.remove(0).rgba
}

fn is_blank(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .all(|p| p[..3] == [255, 255, 255])
}

/// A square in the corner, drawn after the shading: the program went on
/// past the in-line data.
const MARKER: &str = "0 0 1 setrgbcolor 200 200 20 20 rectfill";

#[test]
fn a_filter_on_currentfile_paints_what_the_same_string_paints() {
    for (entries, hex) in meshes() {
        let from_string = render(&format!(
            "<< {entries} /DataSource <{hex}> >> shfill\n{MARKER}"
        ));
        assert!(!is_blank(&from_string), "{entries}");

        let from_file = render(&format!(
            "<< {entries} /DataSource currentfile /ASCIIHexDecode filter >> shfill\n{hex} >\n{MARKER}"
        ));
        assert!(from_file == from_string, "{entries}");
    }
}

/// Two filters deep, as Illustrator writes it.
#[test]
fn a_chain_of_filters_is_read_to_its_end() {
    let (entries, hex) = meshes().remove(3);
    let from_string = render(&format!("<< {entries} /DataSource <{hex}> >> shfill"));
    // The hex digits themselves, hex-encoded again.
    let twice: String = hex.bytes().map(|b| format!("{b:02x}")).collect();
    let from_file = render(&format!(
        "<< {entries} /DataSource currentfile /ASCIIHexDecode filter /ASCIIHexDecode filter \
         >> shfill\n{twice}3e>\n"
    ));
    assert!(from_file == from_string);
}

/// A `ReusableStreamDecode` filter is a file like any other here: read
/// from where it stands to its end. Painting it a second time finds it at
/// its end, as in Ghostscript, unless the program repositions it.
#[test]
fn a_reusable_stream_is_read_from_where_it_stands() {
    let (entries, hex) = meshes().remove(0);
    let from_string = render(&format!("<< {entries} /DataSource <{hex}> >> shfill"));
    let reusable = format!(
        "/R currentfile /ASCIIHexDecode filter /ReusableStreamDecode filter\n{hex} >\ndef\n\
         /D << {entries} /DataSource R >> def"
    );
    let once = render(&format!("{reusable}\nD shfill"));
    assert!(once == from_string);

    // The second, moved up the page, paints nothing more.
    let twice = render(&format!("{reusable}\nD shfill 0 20 translate D shfill"));
    assert!(twice == from_string);

    // Repositioned, it paints again. `setfileposition` raised `ioerror`
    // for a reusable stream, which is the one filter made to take it.
    let twice_from_string = render(&format!(
        "/D << {entries} /DataSource <{hex}> >> def\nD shfill 0 20 translate D shfill"
    ));
    let repositioned = render(&format!(
        "{reusable}\nD shfill R 0 setfileposition 0 20 translate D shfill"
    ));
    assert!(repositioned == twice_from_string);
    assert!(repositioned != from_string);
}

/// A closed file reads as one at its end, so the mesh is empty: nothing is
/// painted and nothing is raised.
#[test]
fn a_closed_file_is_an_empty_mesh() {
    let (entries, hex) = meshes().remove(0);
    let page = render(&format!(
        "/R currentfile /ASCIIHexDecode filter /ReusableStreamDecode filter\n{hex} >\ndef\n\
         R closefile\n<< {entries} /DataSource R >> shfill"
    ));
    assert!(is_blank(&page));
}

/// Anything else is still a `typecheck`.
#[test]
fn a_data_source_of_another_type_is_a_typecheck() {
    let (entries, _) = meshes().remove(0);
    let page = render(&format!(
        "{{ << {entries} /DataSource 42 >> shfill }} stopped\n\
         {{ $error /errorname get /typecheck eq {{ {MARKER} }} if }} if"
    ));
    assert!(!is_blank(&page), "typecheck was raised and caught");
}

/// The same gap one level down: a sampled (Type 0) function may take its
/// samples from "a string or positionable file", read from position 0
/// (PLRM 3, 3.10.1). In-line data becomes positionable through a
/// `ReusableStreamDecode` filter.
#[test]
fn a_sampled_function_reads_a_reusable_stream() {
    let shading = |source: &str| {
        format!(
            "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 200 0] \
             /Function << /FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [3] \
             /BitsPerSample 8 /DataSource {source} >> >> shfill"
        )
    };
    let from_string = render(&shading("<ff0000 00ff00 0000ff>"));
    assert!(!is_blank(&from_string));

    let reusable = "/R currentfile /ASCIIHexDecode filter /ReusableStreamDecode filter\nff0000 00ff00 0000ff >\ndef";
    let from_file = render(&format!("{reusable}\n{}", shading("R")));
    assert!(from_file == from_string);

    // Wherever the file was left: the samples begin at position 0.
    let read_first = render(&format!(
        "{reusable}\nR read pop pop R read pop pop\n{}",
        shading("R")
    ));
    assert!(read_first == from_string);

    // A file that is not positionable is not a legal source.
    let page = render(&format!(
        "{{ {} }} stopped\n{{ $error /errorname get /ioerror eq {{ {MARKER} }} if }} if",
        shading("(%stdin) (r) file")
    ));
    assert!(!is_blank(&page), "ioerror was raised and caught");
}
