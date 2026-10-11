// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A name followed by white space takes that one character with it,
//! whichever kind of name it is.
//!
//! PLRM says so twice, of `token` ("If the token is a name or a number
//! followed by a white-space character, token consumes the white-space
//! character") and of `currentfile` ("If that token was a number or a name
//! immediately followed by a white-space character, the file is positioned
//! after the white-space character"). stet did it for executable names and
//! numbers and not for literal (`/name`) or immediately evaluated
//! (`//name`) ones, so what `token` left behind began with the white space,
//! and a read after such a name started one character early.

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

/// What `token` leaves of a string, by the kind of token and what ends it.
#[test]
fn token_on_a_string_leaves_what_follows_the_white_space() {
    for (source, rest) in [
        // One white-space character goes with the name.
        ("(/ab X)", "(X)"),
        ("(//add X)", "(X)"),
        ("(ab X)", "(X)"),
        ("(12 X)", "(X)"),
        // Only the first, if there are several.
        ("(/ab  X)", "( X)"),
        // CR LF is one.
        ("(/ab\\r\\nX)", "(X)"),
        ("(//add\\r\\nX)", "(X)"),
        // The name with no characters is a name.
        ("(/ X)", "(X)"),
        // A delimiter belongs to the next token and stays.
        ("(/ab(X))", "((X))"),
        ("(/ab/X)", "(/X)"),
        ("(/ab[X)", "([X)"),
        // Nothing follows.
        ("(/ab)", "()"),
        ("(/ab )", "()"),
    ] {
        let program = format!("{source} token pop pop {rest} eq");
        assert!(probe(program.as_bytes()), "{source}");
    }
}

/// `token` on the program's own file: the rest of the line, or the next
/// line, is what a read finds afterwards.
#[test]
fn a_read_after_token_reads_a_literal_name_starts_past_the_white_space() {
    for (name, ending) in ENDINGS {
        let same_line = with_endings(
            &[
                "/t { currentfile token pop pop currentfile 80 string readline pop } def",
                "t",
                "/abc REST",
                "(REST) eq",
            ],
            ending,
        );
        assert!(probe(&same_line), "{name}: the rest of the name's line");

        let next_line = with_endings(
            &[
                "/t { currentfile token pop pop currentfile 80 string readline pop } def",
                "t",
                "/abc",
                "REST",
                "(REST) eq",
            ],
            ending,
        );
        assert!(probe(&next_line), "{name}: the line after the name's");

        let one_character = with_endings(
            &[
                "/t { currentfile token pop pop currentfile read pop } def",
                "t",
                "//add x",
                "120 eq",
            ],
            ending,
        );
        assert!(
            probe(&one_character),
            "{name}: an immediately evaluated name"
        );
    }
}

/// A file read through a filter has a scanner of its own.
#[test]
fn the_same_inside_a_filtered_file() {
    for (name, ending) in ENDINGS {
        let program = with_endings(
            &[
                "/t { currentfile token pop pop currentfile 80 string readline pop } def",
                "currentfile 0 (%%EOD) /SubFileDecode filter cvx exec",
                "t",
                "/abc",
                "REST",
                "(REST) eq",
                "t",
                "/ REST",
                "(REST) eq and",
                "%%EOD",
            ],
            ending,
        );
        assert!(probe(&program), "{name}");
    }
}
