// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A number or executable name ended by white space takes that one
//! character with it, and a carriage return followed at once by a line
//! feed is one newline (PLRM 3.2.2), so it takes both.
//!
//! The scanner took the carriage return and left the line feed, and a
//! program that then read from `currentfile` got an empty first line.
//! Illustrator's EPS relies on this: it writes CR LF line endings and keeps
//! gradient-mesh data in comment lines read by a procedure data source, to
//! which an empty line is the end of the data — so every mesh painted
//! nothing, and the job reported success (issue #7).
//!
//! The programs are built here with their line endings written out, so
//! that no checkout setting can change what is tested.

use stet::Interpreter;

/// Run `source` and report whether it left `true` on the operand stack.
fn probe(source: &[u8]) -> bool {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    if stet_engine::eval::parse_and_exec(ctx, source).is_err() {
        return false;
    }
    matches!(
        ctx.o_stack.peek(0).map(|o| o.value),
        Ok(stet_core::object::PsValue::Bool(true))
    )
}

/// `lines` joined with, and ended by, `ending`.
fn with_endings(lines: &[&str], ending: &str) -> Vec<u8> {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str(ending);
    }
    out.into_bytes()
}

const ENDINGS: [(&str, &str); 3] = [("LF", "\n"), ("CR", "\r"), ("CR LF", "\r\n")];

/// The line after an executable name is the next thing `readline` reads.
#[test]
fn readline_after_a_name_reads_the_next_line() {
    for (name, ending) in ENDINGS {
        let program = with_endings(
            &[
                "%!PS",
                "true { currentfile 80 string readline pop (HELLO) eq } if",
                "HELLO",
            ],
            ending,
        );
        assert!(probe(&program), "{name}");
    }
}

/// The same after a number, which ends the same way.
#[test]
fn readline_after_a_number_reads_the_next_line() {
    for (name, ending) in ENDINGS {
        let program = with_endings(
            &[
                "currentfile 80 string 1",
                "pop readline",
                "HELLO",
                "pop (HELLO) eq",
            ],
            ending,
        );
        assert!(probe(&program), "{name}");
    }
}

/// A file read through a filter is scanned a byte at a time, by a scanner
/// of its own.
#[test]
fn readline_inside_a_filtered_file_reads_the_next_line() {
    for (name, ending) in ENDINGS {
        let program = with_endings(
            &[
                "currentfile 0 (%%EOD) /SubFileDecode filter cvx exec",
                "true { currentfile 80 string readline pop (HELLO) eq } if",
                "HELLO",
                "%%EOD",
            ],
            ending,
        );
        assert!(probe(&program), "{name}");
    }
}

/// `token` on a string leaves what follows the newline.
#[test]
fn token_on_a_string_takes_the_whole_newline() {
    for (source, rest) in [
        ("(12\\r\\nX)", "(X)"),
        ("(ab\\r\\nX)", "(X)"),
        ("(12\\rX)", "(X)"),
        ("(12\\nX)", "(X)"),
        // A line feed and then a carriage return are two newlines.
        ("(12\\n\\rX)", "(\\rX)"),
        ("(12\\r\\n)", "()"),
        ("(12\\r)", "()"),
    ] {
        let program = format!("{source} token pop pop {rest} eq");
        assert!(probe(program.as_bytes()), "{source}");
    }
}

fn render(program: &[u8]) -> Vec<u8> {
    let mut pages = Interpreter::new()
        .render(program, 72.0)
        .expect("program runs");
    assert_eq!(pages.len(), 1);
    pages.remove(0).rgba
}

/// Illustrator's gradient mesh, as a program: the data in comment lines
/// after the shading, read a line at a time by a procedure (`rdcmntline`,
/// from its `Adobe_AGM_Utils` procset) behind `filter`, with `data` as
/// those lines. A blue square is drawn after the data.
fn mesh_program(filter: &str, data: &[&str], ending: &str) -> Vec<u8> {
    let source = format!("   /DataSource /rdcmntline load /{filter} filter");
    let mut lines = vec![
        "%!PS-Adobe-3.0 EPSF-3.0",
        "%%BoundingBox: 0 0 200 200",
        "<< /PageSize [200 200] >> setpagedevice",
        "/buf 256 string def",
        "/rdcmntline { currentfile buf readline pop (%) anchorsearch { pop } if } bind def",
        "true {",
        "<< /ShadingType 4 /ColorSpace /DeviceRGB",
        "   /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8",
        "   /Decode [0 200 0 200 0 1 0 1 0 1]",
        &source,
        ">> shfill",
        "} if",
    ];
    lines.extend_from_slice(data);
    lines.extend_from_slice(&["0 0 1 setrgbcolor 150 150 40 40 rectfill", "showpage"]);
    with_endings(&lines, ending)
}

const HEX_DATA: [&str; 3] = [
    "%00 20 20 FF 00 00",
    "%00 E0 20 00 FF 00",
    "%00 80 E0 00 00 FF >",
];

/// The pixels of the mesh's triangle, and of the square drawn after it.
fn triangle_and_square(rgba: &[u8]) -> (usize, usize) {
    let pixels = rgba.as_chunks::<4>().0;
    let square = pixels.iter().filter(|p| p[..3] == [0, 0, 255]).count();
    let painted = pixels.iter().filter(|p| p[..3] != [255, 255, 255]).count();
    (painted - square, square)
}

#[test]
fn a_mesh_read_from_comment_lines_paints_whatever_the_line_endings() {
    let with_lf = render(&mesh_program("ASCIIHexDecode", &HEX_DATA, "\n"));
    let (triangle, _) = triangle_and_square(&with_lf);
    assert!(triangle > 5000, "the triangle is drawn: {triangle} pixels");
    for (name, ending) in ENDINGS {
        let page = render(&mesh_program("ASCIIHexDecode", &HEX_DATA, ending));
        assert!(page == with_lf, "{name}");
    }
}

/// The procedure reads the program's own file and has no end of its own
/// before the file's: asked for data until it returned an empty string, it
/// took the rest of the program, and nothing after the first mesh ran.
/// The filter stops at its end-of-data mark, and so must the asking.
#[test]
fn the_program_goes_on_after_a_mesh_read_from_comment_lines() {
    for (name, ending) in ENDINGS {
        let page = render(&mesh_program("ASCIIHexDecode", &HEX_DATA, ending));
        let (_, square) = triangle_and_square(&page);
        assert!(
            square > 1000,
            "{name}: the square after the mesh, {square} pixels"
        );
    }
}

/// The same data as Illustrator encodes it, in ASCII85 — and once with the
/// two characters of the end-of-data mark on separate lines, which the
/// procedure hands over in separate strings.
#[test]
fn the_program_goes_on_after_ascii85_mesh_data() {
    let hex = render(&mesh_program("ASCIIHexDecode", &HEX_DATA, "\n"));
    let (triangle, square) = triangle_and_square(&hex);
    assert!(triangle > 5000 && square > 1000, "{triangle}, {square}");
    let bytes: [u8; 18] = [
        0x00, 0x20, 0x20, 0xFF, 0x00, 0x00, 0x00, 0xE0, 0x20, 0x00, 0xFF, 0x00, 0x00, 0x80, 0xE0,
        0x00, 0x00, 0xFF,
    ];
    let encoded = ascii85(&bytes);
    let (head, tail) = encoded.split_at(encoded.len() / 2);
    let whole = [format!("%{head}"), format!("%{tail}~>")];
    let split = [format!("%{head}"), format!("%{tail}~"), "%>".to_string()];
    for data in [&whole[..], &split[..]] {
        let data: Vec<&str> = data.iter().map(String::as_str).collect();
        for (name, ending) in ENDINGS {
            let page = render(&mesh_program("ASCII85Decode", &data, ending));
            assert!(page == hex, "{name}, {} lines of data", data.len());
        }
    }
}

/// `bytes` in ASCII85, without the end-of-data mark.
fn ascii85(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(4) {
        let mut group = [0u8; 4];
        group[..chunk.len()].copy_from_slice(chunk);
        let mut value = u32::from_be_bytes(group);
        let mut digits = [0u8; 5];
        for digit in digits.iter_mut().rev() {
            *digit = (value % 85) as u8 + b'!';
            value /= 85;
        }
        if chunk.len() == 4 && digits == [b'!'; 5] {
            out.push('z');
        } else {
            out.extend(digits[..chunk.len() + 1].iter().map(|d| *d as char));
        }
    }
    out
}
