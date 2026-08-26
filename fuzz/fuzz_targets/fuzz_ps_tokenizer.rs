// Tokenize arbitrary bytes as PostScript.
//
// The tokenizer is the first thing any PostScript input meets, and it handles
// the awkward cases on its own: nested `%` comments, `(...)` strings with
// escapes and unbalanced parens, `<~...~>` ASCII85, hex strings, radix
// numbers like `16#FFFF`, and the binary token encodings.
//
// This target stops at tokenization and does not evaluate. Running the
// interpreter over arbitrary bytes mostly measures how long a `{...} loop`
// takes to time out, which is a resource-governor question (see SECURITY.md
// Priority 1), not a parser-crash question.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stet_core::tokenizer::Tokenizer;

fuzz_target!(|data: &[u8]| {
    let mut tok = Tokenizer::new(data);
    // Bound the token count: a large input of single-character tokens is a
    // slow unit rather than an interesting one.
    for _ in 0..100_000 {
        match tok.next_token() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
});
