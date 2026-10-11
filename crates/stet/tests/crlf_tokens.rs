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
