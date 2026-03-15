// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Adobe CID → Unicode mapping tables for CJK character collections.
//!
//! When a CID font uses Identity-H encoding with an Adobe CID registry
//! (Japan1, CNS1, GB1, Korea1) and no /ToUnicode CMap is present in the
//! PDF, these tables provide the CID → Unicode mapping needed to render
//! text with a substitute font.
//!
//! Tables are derived from the Adobe UniXXX-UTF32-H CMap files and stored
//! as zlib-compressed arrays of (u16 CID, u16 Unicode) pairs.

use std::collections::HashMap;
use std::sync::OnceLock;

static JAPAN1: OnceLock<HashMap<u16, u32>> = OnceLock::new();
static CNS1: OnceLock<HashMap<u16, u32>> = OnceLock::new();
static GB1: OnceLock<HashMap<u16, u32>> = OnceLock::new();
static KOREA1: OnceLock<HashMap<u16, u32>> = OnceLock::new();

/// Look up CID → Unicode for a given Adobe CID registry/ordering.
pub fn cid_to_unicode(ordering: &[u8], cid: u16) -> Option<u32> {
    let table = match ordering {
        b"Japan1" => JAPAN1.get_or_init(|| load_table(include_bytes!("cid_japan1.bin"))),
        b"CNS1" => CNS1.get_or_init(|| load_table(include_bytes!("cid_cns1.bin"))),
        b"GB1" => GB1.get_or_init(|| load_table(include_bytes!("cid_gb1.bin"))),
        b"Korea1" => KOREA1.get_or_init(|| load_table(include_bytes!("cid_korea1.bin"))),
        _ => return None,
    };
    table.get(&cid).copied()
}

static JAPAN1_REV: OnceLock<HashMap<u32, u16>> = OnceLock::new();
static CNS1_REV: OnceLock<HashMap<u32, u16>> = OnceLock::new();
static GB1_REV: OnceLock<HashMap<u32, u16>> = OnceLock::new();
static KOREA1_REV: OnceLock<HashMap<u32, u16>> = OnceLock::new();

/// Look up Unicode → CID for a given Adobe CID registry/ordering.
/// Used for UCS2-based CMap encodings (e.g. UniJIS-UCS2-H) where
/// character codes are Unicode values that need mapping to CIDs.
pub fn unicode_to_cid(ordering: &[u8], unicode: u32) -> Option<u16> {
    let table = match ordering {
        b"Japan1" => JAPAN1_REV.get_or_init(|| invert_table(include_bytes!("cid_japan1.bin"))),
        b"CNS1" => CNS1_REV.get_or_init(|| invert_table(include_bytes!("cid_cns1.bin"))),
        b"GB1" => GB1_REV.get_or_init(|| invert_table(include_bytes!("cid_gb1.bin"))),
        b"Korea1" => KOREA1_REV.get_or_init(|| invert_table(include_bytes!("cid_korea1.bin"))),
        _ => return None,
    };
    table.get(&unicode).copied()
}

fn invert_table(compressed: &[u8]) -> HashMap<u32, u16> {
    let forward = load_table(compressed);
    let mut rev = HashMap::with_capacity(forward.len());
    for (&cid, &unicode) in &forward {
        // First CID wins (some Unicode values may have multiple CIDs)
        rev.entry(unicode).or_insert(cid);
    }
    rev
}

fn load_table(compressed: &[u8]) -> HashMap<u16, u32> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    let mut decoder = ZlibDecoder::new(compressed);
    let mut raw = Vec::new();
    if decoder.read_to_end(&mut raw).is_err() {
        return HashMap::new();
    }

    let mut map = HashMap::with_capacity(raw.len() / 4);
    for chunk in raw.chunks_exact(4) {
        let cid = u16::from_le_bytes([chunk[0], chunk[1]]);
        let unicode = u16::from_le_bytes([chunk[2], chunk[3]]);
        map.insert(cid, unicode as u32);
    }
    map
}
