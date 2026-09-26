// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The glyph data of a CIDFontType 0 font with Type 1 charstrings.
//!
//! Such a font keeps its charstrings and subroutines in one binary block —
//! the `(Hex)` or `(Binary)` data after `StartData`, stored as the CIDFont's
//! `GlyphData` — indexed by offset tables inside the block (Adobe TN#5014,
//! "Adobe CMap and CIDFont Files Specification", §5.11).
//!
//! These functions read only that block. The numbers that locate the
//! tables (`CIDMapOffset`, `FDBytes`, `GDBytes`, and the Private dict's
//! `SubrMapOffset`, `SDBytes`, `SubrCount`) come from PostScript
//! dictionaries, which the caller reads.

/// Largest `FDBytes` / `GDBytes` / `SDBytes` accepted. They are byte widths
/// of offsets into the glyph data, so four already addresses 4 GB;
/// Ghostscript has the same limit (`MAX_FDBytes`, `MAX_GDBytes` in
/// `gxfcid.h`).
pub const MAX_OFFSET_BYTES: usize = 4;

/// Where a font's CID map is and how its entries are laid out.
///
/// The map starts at `CIDMapOffset` with one entry per CID from 0 to
/// `CIDCount`: an FD index (`FDBytes` wide) and the offset of the CID's
/// charstring (`GDBytes` wide), both big-endian. A charstring runs to the
/// next entry's offset, so the map has one entry more than `CIDCount`, and
/// a CID whose charstring is empty has no glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CidMap {
    cid_count: usize,
    offset: usize,
    fd_bytes: usize,
    gd_bytes: usize,
}

impl CidMap {
    /// A map for `CIDCount` CIDs at `CIDMapOffset` with the given entry
    /// widths. `FDBytes` may be 0 (every glyph uses FD 0); `GDBytes` may
    /// not. Neither may exceed [`MAX_OFFSET_BYTES`].
    pub fn new(
        cid_count: usize,
        cid_map_offset: usize,
        fd_bytes: usize,
        gd_bytes: usize,
    ) -> Result<Self, String> {
        if fd_bytes > MAX_OFFSET_BYTES || gd_bytes == 0 || gd_bytes > MAX_OFFSET_BYTES {
            return Err(format!(
                "CIDFont: FDBytes {fd_bytes} / GDBytes {gd_bytes} out of range"
            ));
        }
        Ok(Self {
            cid_count,
            offset: cid_map_offset,
            fd_bytes,
            gd_bytes,
        })
    }

    /// `CIDCount`: CIDs from 0 up to, not including, this have entries.
    pub fn cid_count(&self) -> usize {
        self.cid_count
    }

    /// The FD index and charstring of `cid` in `data`, or `None` when the
    /// font has no glyph for it: the CID is out of range, its charstring is
    /// empty, or the map or charstring lies outside `data` (a truncated
    /// job).
    pub fn glyph<'d>(&self, data: &'d [u8], cid: usize) -> Option<(usize, &'d [u8])> {
        if cid >= self.cid_count {
            return None;
        }
        let entry = self.fd_bytes + self.gd_bytes;
        let base = cid.checked_mul(entry)?.checked_add(self.offset)?;
        // This entry and the next: the next one's offset ends the glyph.
        let map = data.get(base..base.checked_add(2 * entry)?)?;
        let fd = read_be(&map[..self.fd_bytes]);
        let start = read_be(&map[self.fd_bytes..entry]);
        let end = read_be(&map[entry + self.fd_bytes..]);
        if end <= start {
            return None;
        }
        Some((fd, data.get(start..end)?))
    }
}

/// A Private dict's subroutines, sliced out of `data`: `SubrCount + 1`
/// offsets of `SDBytes` each from `SubrMapOffset`, subroutine *i* running
/// from offset *i* to offset *i + 1*.
///
/// The whole offset table must lie inside `data`. A subroutine whose own
/// bytes do not is returned empty, so the numbering of the others holds.
pub fn subrs(
    data: &[u8],
    subr_map_offset: usize,
    sd_bytes: usize,
    subr_count: usize,
) -> Result<Vec<Vec<u8>>, String> {
    if sd_bytes == 0 || sd_bytes > MAX_OFFSET_BYTES {
        return Err(format!("CIDFont: SDBytes {sd_bytes} out of range"));
    }
    // The table must be present before anything is sized from
    // `subr_count`, which comes from the file.
    let map = subr_count
        .checked_add(1)
        .and_then(|n| n.checked_mul(sd_bytes))
        .and_then(|n| n.checked_add(subr_map_offset))
        .and_then(|end| data.get(subr_map_offset..end))
        .ok_or("CIDFont: subroutine map outside the glyph data")?;
    let offsets: Vec<usize> = map.chunks_exact(sd_bytes).map(read_be).collect();
    Ok(offsets
        .windows(2)
        .map(|w| data.get(w[0]..w[1]).unwrap_or_default().to_vec())
        .collect())
}

/// A big-endian unsigned integer of up to [`MAX_OFFSET_BYTES`] bytes.
fn read_be(bytes: &[u8]) -> usize {
    bytes.iter().fold(0, |v, &b| (v << 8) | usize::from(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two bytes of padding, then a map for CIDs 0–2 (FDBytes 1, GDBytes 2)
    /// whose CID 1 is empty, then the charstrings.
    fn data() -> Vec<u8> {
        let mut d = vec![0xEE, 0xEE];
        for (fd, off) in [(0u8, 14u16), (1, 17), (1, 17), (0, 19)] {
            d.push(fd);
            d.extend(off.to_be_bytes());
        }
        d.extend(b"abcde");
        d
    }

    #[test]
    fn glyphs_run_to_the_next_entry() {
        let map = CidMap::new(3, 2, 1, 2).unwrap();
        let d = data();
        assert_eq!(map.glyph(&d, 0), Some((0, &b"abc"[..])));
        assert_eq!(map.glyph(&d, 1), None);
        assert_eq!(map.glyph(&d, 2), Some((1, &b"de"[..])));
        assert_eq!(map.glyph(&d, 3), None);
        // A truncated job: the map is cut short.
        assert_eq!(map.glyph(&d[..10], 2), None);
    }

    #[test]
    fn fd_bytes_zero_means_fd_0() {
        let d = [0u8, 4, 0, 6, b'x', b'y'];
        let map = CidMap::new(1, 0, 0, 2).unwrap();
        assert_eq!(map.glyph(&d, 0), Some((0, &b"xy"[..])));
    }

    #[test]
    fn entry_widths_are_bounded() {
        assert!(CidMap::new(1, 0, 5, 2).is_err());
        assert!(CidMap::new(1, 0, 1, 0).is_err());
        assert!(CidMap::new(1, 0, 1, 5).is_err());
    }

    #[test]
    fn subrs_are_sliced_by_their_map() {
        let d = [4u8, 5, 7, 7, b'a', b'b', b'c'];
        assert_eq!(
            subrs(&d, 0, 1, 3).unwrap(),
            [b"a".to_vec(), b"bc".to_vec(), Vec::new()]
        );
        // The table itself must fit.
        assert!(subrs(&d, 0, 1, 100).is_err());
        assert!(subrs(&d, 0, 1, usize::MAX).is_err());
        assert!(subrs(&d, 0, 0, 1).is_err());
    }
}
