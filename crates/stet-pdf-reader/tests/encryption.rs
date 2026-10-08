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

/// R2, R4 and R6 with the user password `æøå` and the owner password
/// `öwner`, given to qpdf as UTF-8. qpdf stores the first two in
/// PDFDocEncoding, as those handlers define, and the third in UTF-8.
const R2_LATIN: &[u8] = include_bytes!("data/encryption/r2-latin.pdf");
const R4_LATIN: &[u8] = include_bytes!("data/encryption/r4-latin.pdf");
const R6_LATIN: &[u8] = include_bytes!("data/encryption/r6-latin.pdf");
/// R4 with the user password `pass€word` and the owner password `own€r`:
/// the euro sign is 0xA0 in PDFDocEncoding, and not in Latin-1 at all.
const R4_EURO: &[u8] = include_bytes!("data/encryption/r4-euro.pdf");

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

/// The owner password opens a file as well as the user one does. Before
/// revision 5 it does so by decrypting `/O` to recover the user password;
/// from revision 5 it has its own hash and its own wrapped key, `/OE`.
#[test]
fn the_owner_password_opens_every_revision() {
    for (what, data) in EVERY_REVISION {
        opens(what, data, b"owner");
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

/// The owner password of an AES-256 file is prepared the same way.
#[cfg(feature = "saslprep")]
#[test]
fn an_aes256_owner_password_is_prepared_with_saslprep() {
    opens(
        "R6, owner password typed with U+00AA and U+00AD",
        R6_SASLPREP,
        "S\u{aa}SL\u{ad}prepOwner".as_bytes(),
    );
}

#[test]
fn an_aes256_password_already_in_prepared_form_opens() {
    opens("R6, prepared form", R6_SASLPREP, b"SaSLprep");
    opens("R6, owner, prepared form", R6_SASLPREP, b"SaSLprepOwner");
    is_refused("R6, wrong case", R6_SASLPREP, b"saslprep");
}

/// The R3 fixture with its `/Length 128` replaced by another three digits.
fn r3_with_key_length(bits: &str) -> Vec<u8> {
    let needle = b"/Length 128";
    let at = R3
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the R3 fixture declares /Length 128");
    let mut data = R3.to_vec();
    data[at + 8..at + 11].copy_from_slice(bits.as_bytes());
    data
}

/// The key length is the file's to state, and it used to be trusted: zero
/// keyed RC4 with nothing (a division by zero), and anything over 128 bits
/// sliced past the end of an MD5 digest. Neither may panic, and a length
/// the handler cannot have is no reason to refuse a file whose key still
/// works at the nearest length it can.
#[test]
fn a_key_length_out_of_range_does_not_panic() {
    for bits in ["000", "004", "-16"] {
        is_refused(bits, &r3_with_key_length(bits), b"user");
    }
    for bits in ["136", "256", "999"] {
        let data = r3_with_key_length(bits);
        opens(bits, &data, b"user");
        opens(bits, &data, b"owner");
    }
}

/// A caller passes the password as the user typed it, in UTF-8. An RC4 or
/// AES-128 file holds it in PDFDocEncoding, so beyond ASCII the two differ
/// and the reader has to bridge them.
#[test]
fn a_password_beyond_ascii_opens_an_rc4_or_aes128_file() {
    for (what, data) in [("R2", R2_LATIN), ("R4", R4_LATIN), ("R6", R6_LATIN)] {
        opens(what, data, "\u{e6}\u{f8}\u{e5}".as_bytes());
        opens(what, data, "\u{f6}wner".as_bytes());
        is_refused(what, data, "\u{e6}\u{f8}\u{e6}".as_bytes());
    }
    // A caller that encoded the password itself is still right.
    opens("R2, Latin-1 bytes", R2_LATIN, &[0xE6, 0xF8, 0xE5]);
    opens("R4, Latin-1 bytes", R4_LATIN, &[0xE6, 0xF8, 0xE5]);
    // Not Latin-1: the euro sign has its own place in PDFDocEncoding.
    opens("R4, euro", R4_EURO, "pass\u{20ac}word".as_bytes());
    opens("R4, euro, owner", R4_EURO, "own\u{20ac}r".as_bytes());
}
