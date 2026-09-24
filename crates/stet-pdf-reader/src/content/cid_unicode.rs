// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Adobe CID → Unicode mapping tables for CJK character collections.
//!
//! The tables moved to [`stet_fonts::cid_unicode`] so the PostScript
//! interpreter can share them for text extraction. These forwarding
//! functions keep the path this module was published under working.

/// Look up CID → Unicode for a given Adobe CID registry/ordering.
#[deprecated(
    since = "0.8.2",
    note = "moved to `stet_fonts::cid_unicode::cid_to_unicode`"
)]
pub fn cid_to_unicode(ordering: &[u8], cid: u16) -> Option<u32> {
    stet_fonts::cid_unicode::cid_to_unicode(ordering, cid)
}

/// Look up Unicode → CID for a given Adobe CID registry/ordering.
/// Used for UCS2-based CMap encodings (e.g. UniJIS-UCS2-H) where
/// character codes are Unicode values that need mapping to CIDs.
#[deprecated(
    since = "0.8.2",
    note = "moved to `stet_fonts::cid_unicode::unicode_to_cid`"
)]
pub fn unicode_to_cid(ordering: &[u8], unicode: u32) -> Option<u16> {
    stet_fonts::cid_unicode::unicode_to_cid(ordering, unicode)
}
