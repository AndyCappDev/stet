// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Opening encrypted PDFs with the Standard security handler, at every
//! revision.
//!
//! The fixtures in `data/encryption/` are one hand-written page — a red
//! square, and a `/Title` in the Info dictionary — encrypted by qpdf 12
//! (`qpdf --static-id --encrypt <user> <owner> <bits> …`). They are another
//! implementation's output, so a pass here is agreement with qpdf and not
//! with ourselves. Each file is opened, then one string and one stream are
//! read back: a wrong key gets past neither.

use stet_graphics::icc::IccCache;
use stet_pdf_reader::{PdfDocument, PdfError};

const R2: &[u8] = include_bytes!("data/encryption/r2.pdf");
const R3: &[u8] = include_bytes!("data/encryption/r3.pdf");
const R4: &[u8] = include_bytes!("data/encryption/r4.pdf");
const R5: &[u8] = include_bytes!("data/encryption/r5.pdf");
const R6: &[u8] = include_bytes!("data/encryption/r6.pdf");
/// R6 with the user password `SaSLprep`.
const R6_SASLPREP: &[u8] = include_bytes!("data/encryption/r6-saslprep.pdf");

const EVERY_REVISION: [(&str, &[u8]); 5] = [
    ("R2, RC4 40-bit", R2),
    ("R3, RC4 128-bit", R3),
    ("R4, AES-128", R4),
    ("R5, AES-256", R5),
    ("R6, AES-256", R6),
];

/// Opens `data` with `password` and checks that a string and a stream both
/// decrypt.
fn opens(what: &str, data: &[u8], password: &[u8]) {
    let doc = PdfDocument::from_bytes_with_password(data, IccCache::new(), password)
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    assert_eq!(doc.page_count(), 1, "{what}");
    assert_eq!(
        doc.metadata().title.as_deref(),
        Some("stet encryption fixture"),
        "{what}: a string decrypted wrongly"
    );
    let list = doc.render_page(0, 72.0).unwrap();
    assert!(
        !list.elements().is_empty(),
        "{what}: the content stream decrypted wrongly"
    );
}

fn is_refused(what: &str, data: &[u8], password: &[u8]) {
    match PdfDocument::from_bytes_with_password(data, IccCache::new(), password) {
        Err(PdfError::PasswordRequired) => {}
        Err(e) => panic!("{what}: expected PasswordRequired, got {e}"),
        Ok(_) => panic!("{what}: opened with a wrong password"),
    }
}

#[test]
fn the_user_password_opens_every_revision() {
    for (what, data) in EVERY_REVISION {
        opens(what, data, b"user");
    }
}

#[test]
fn a_wrong_password_is_refused_at_every_revision() {
    for (what, data) in EVERY_REVISION {
        is_refused(what, data, b"wrong");
        is_refused(what, data, b"");
    }
}

/// ISO 32000-2 has an AES-256 password prepared with SASLprep before it is
/// hashed. U+00AA is "a" after NFKC and U+00AD (soft hyphen) maps to
/// nothing, so this is `SaSLprep` — RFC 4013's own example, and the case in
/// pdf.js's `saslprep-r6.pdf`.
#[cfg(feature = "saslprep")]
#[test]
fn an_aes256_password_is_prepared_with_saslprep() {
    opens(
        "R6, typed with U+00AA and U+00AD",
        R6_SASLPREP,
        "S\u{aa}SL\u{ad}prep".as_bytes(),
    );
    // Full-width letters fold to ASCII the same way.
    opens(
        "R6, typed full-width",
        R6_SASLPREP,
        "\u{ff33}\u{ff41}\u{ff33}\u{ff2c}prep".as_bytes(),
    );
}

#[test]
fn an_aes256_password_already_in_prepared_form_opens() {
    opens("R6, prepared form", R6_SASLPREP, b"SaSLprep");
    is_refused("R6, wrong case", R6_SASLPREP, b"saslprep");
}
