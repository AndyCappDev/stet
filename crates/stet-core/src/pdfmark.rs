// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `pdfmark` authoring records and accumulator.
//!
//! `pdfmark` is the PostScript-to-PDF authoring bridge. PostScript code
//! issues `pdfmark` calls during interpretation; the interpreter parks the
//! resulting [`PdfMarkRecord`]s on a [`PdfMarkBuffer`] hanging off
//! [`crate::context::Context`]. The PDF output device drains that buffer at
//! end-of-job (`finish_with_context`) and writes the records into the PDF
//! catalog, info dictionary, outline tree, page annotation arrays, and so
//! on. Non-PDF output devices simply ignore the buffer, so `pdfmark` is a
//! no-op for PNG / viewer output.
//!
//! See `docs/PLAN-PDFMARK-AUTHORING.md` for the staged plan and
//! `docs/PDFMARK-REFERENCE.md` (TBD) for the public reference once the
//! plan reaches its rollup.

/// One accumulated `pdfmark` record. Each variant corresponds to a
/// type-tag the interpreter recognises (`/DOCINFO`, `/OUT`, `/ANN`, …).
/// Only [`PdfMarkRecord::DocInfo`] is implemented in Phase 1; later phases
/// add variants without disturbing this enum's external API beyond the
/// new variant itself.
#[derive(Clone, Debug)]
pub enum PdfMarkRecord {
    /// `/DOCINFO` — entries to merge into the PDF Info dictionary.
    DocInfo(DocInfoRecord),
}

/// Buffered `pdfmark` records. Lives on `Context` for the entire job;
/// drained once by the PDF output device at end-of-job. The buffer is
/// document-global (not VM-level), so `save` / `restore` do **not** roll
/// it back — pdfmark records issued before a `restore` survive.
#[derive(Default, Clone, Debug)]
pub struct PdfMarkBuffer {
    records: Vec<PdfMarkRecord>,
    /// 1-based count of `showpage` calls so far. Annotations and other
    /// page-scoped records that omit an explicit `/Page` key default to
    /// `current_page + 1` — i.e. the page currently being assembled.
    /// Phase 1 doesn't consume this yet but the field lands now so
    /// `Context::on_showpage()` and downstream phases can rely on it.
    pub current_page: u32,
}

impl PdfMarkBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a record; ordering is preserved.
    pub fn push(&mut self, record: PdfMarkRecord) {
        self.records.push(record);
    }

    /// Read-only view of accumulated records.
    pub fn records(&self) -> &[PdfMarkRecord] {
        &self.records
    }

    /// Take ownership of the records, leaving the buffer empty. Used by
    /// the PDF output device once at end of job.
    pub fn drain(&mut self) -> Vec<PdfMarkRecord> {
        std::mem::take(&mut self.records)
    }

    /// True when no records have been pushed.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// `/DOCINFO` payload — `Option<String>` for every key so absent entries
/// don't overwrite values from another producer (or the device's
/// auto-generated defaults). `creation_date` and `mod_date` accept either
/// a parsed [`PdfDate`] or a passthrough string the writer emits verbatim.
#[derive(Clone, Debug, Default)]
pub struct DocInfoRecord {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<DocDate>,
    pub mod_date: Option<DocDate>,
    /// Trapped: PDF spec requires /True, /False, or /Unknown.
    pub trapped: Option<TrappedState>,
}

/// `/Trapped` value as written to the Info dict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrappedState {
    True,
    False,
    Unknown,
}

impl DocInfoRecord {
    /// Return the `/CreationDate` value formatted as a PDF date string,
    /// or `None` when no creation date is set.
    pub fn creation_date_string(&self) -> Option<String> {
        self.creation_date.as_ref().map(DocDate::to_pdf_string)
    }

    /// Return the `/ModDate` value formatted as a PDF date string, or
    /// `None` when no mod date is set.
    pub fn mod_date_string(&self) -> Option<String> {
        self.mod_date.as_ref().map(DocDate::to_pdf_string)
    }
}

impl DocDate {
    /// Render the date as a PDF date string. `Raw` round-trips the
    /// producer's bytes verbatim; `Parsed` reformats from the
    /// structural form.
    pub fn to_pdf_string(&self) -> String {
        match self {
            DocDate::Raw(s) => s.clone(),
            DocDate::Parsed(d) => {
                let mut out = format!(
                    "D:{:04}{:02}{:02}{:02}{:02}{:02}",
                    d.year, d.month, d.day, d.hour, d.minute, d.second
                );
                match d.tz_sign {
                    TzSign::Utc => out.push('Z'),
                    TzSign::East => out.push_str(&format!("+{:02}'{:02}'", d.tz_hour, d.tz_minute)),
                    TzSign::West => out.push_str(&format!("-{:02}'{:02}'", d.tz_hour, d.tz_minute)),
                    TzSign::Unknown => {}
                }
                out
            }
        }
    }
}

/// A document date entry. The writer can either round-trip a raw string
/// (already in PDF date syntax) or format a parsed [`PdfDate`].
#[derive(Clone, Debug)]
pub enum DocDate {
    /// Raw string the producer issued — passed through verbatim. Used
    /// when the input is already in PDF date format and round-tripping
    /// the bytes preserves precision and timezone offset exactly.
    Raw(String),
    /// Parsed structural form. Reserved for future phases that
    /// normalise dates; Phase 1 stores everything as `Raw`.
    Parsed(PdfDate),
}

/// Parsed PDF date string of the form `D:YYYYMMDDHHmmSSOHH'mm'`, where
/// `O` is one of `+`, `-`, or `Z` for the offset sign. All fields after
/// the year are optional in the PDF spec; missing components default to
/// the values shown in [`PdfDate::default`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdfDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub tz_sign: TzSign,
    pub tz_hour: u8,
    pub tz_minute: u8,
}

/// Sign of a PDF date timezone offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TzSign {
    /// `+` — east of UTC.
    East,
    /// `-` — west of UTC.
    West,
    /// `Z` — UTC.
    Utc,
    /// Offset omitted entirely — treat as local time per PDF spec.
    Unknown,
}

impl Default for PdfDate {
    fn default() -> Self {
        Self {
            year: 0,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            tz_sign: TzSign::Unknown,
            tz_hour: 0,
            tz_minute: 0,
        }
    }
}

impl PdfDate {
    /// Parse a PDF date string. Accepts the canonical `D:YYYY[MMDDHHmmSS[O[HH'[mm']]]]`
    /// shape. The `D:` prefix is required; everything after the year is
    /// optional and missing fields use [`PdfDate::default`] values.
    /// Returns `None` on malformed input.
    pub fn parse(s: &str) -> Option<Self> {
        let body = s.strip_prefix("D:")?;
        let bytes = body.as_bytes();
        if bytes.len() < 4 || !bytes[..4].iter().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let year: u16 = std::str::from_utf8(&bytes[..4]).ok()?.parse().ok()?;
        let mut date = PdfDate {
            year,
            ..PdfDate::default()
        };
        let mut i = 4;

        let take_pair = |idx: &mut usize, max: u8| -> Option<u8> {
            if *idx + 2 > bytes.len() {
                return None;
            }
            let pair = std::str::from_utf8(&bytes[*idx..*idx + 2]).ok()?;
            if !pair.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let v: u8 = pair.parse().ok()?;
            if v > max {
                return None;
            }
            *idx += 2;
            Some(v)
        };

        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.month = take_pair(&mut i, 12)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.day = take_pair(&mut i, 31)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.hour = take_pair(&mut i, 23)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.minute = take_pair(&mut i, 59)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.second = take_pair(&mut i, 59)?;
        }

        if i < bytes.len() {
            match bytes[i] {
                b'Z' => {
                    date.tz_sign = TzSign::Utc;
                }
                b'+' => {
                    date.tz_sign = TzSign::East;
                    i += 1;
                    date.tz_hour = take_pair(&mut i, 23)?;
                    if i < bytes.len() && bytes[i] == b'\'' {
                        i += 1;
                        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
                            date.tz_minute = take_pair(&mut i, 59)?;
                        }
                    }
                }
                b'-' => {
                    date.tz_sign = TzSign::West;
                    i += 1;
                    date.tz_hour = take_pair(&mut i, 23)?;
                    if i < bytes.len() && bytes[i] == b'\'' {
                        i += 1;
                        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
                            date.tz_minute = take_pair(&mut i, 59)?;
                        }
                    }
                }
                _ => return None,
            }
        }

        Some(date)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_parse_full() {
        let d = PdfDate::parse("D:20261231120000-05'00'").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 12);
        assert_eq!(d.day, 31);
        assert_eq!(d.hour, 12);
        assert_eq!(d.tz_sign, TzSign::West);
        assert_eq!(d.tz_hour, 5);
    }

    #[test]
    fn date_parse_utc() {
        let d = PdfDate::parse("D:20260101000000Z").unwrap();
        assert_eq!(d.tz_sign, TzSign::Utc);
        assert_eq!(d.year, 2026);
    }

    #[test]
    fn date_parse_year_only() {
        let d = PdfDate::parse("D:2026").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 1);
        assert_eq!(d.day, 1);
    }

    #[test]
    fn date_parse_no_prefix() {
        assert!(PdfDate::parse("20260101").is_none());
    }

    #[test]
    fn date_parse_garbage() {
        assert!(PdfDate::parse("D:abcd").is_none());
    }

    #[test]
    fn buffer_round_trip() {
        let mut buf = PdfMarkBuffer::new();
        assert!(buf.is_empty());
        buf.push(PdfMarkRecord::DocInfo(DocInfoRecord {
            title: Some("Hello".into()),
            ..DocInfoRecord::default()
        }));
        assert_eq!(buf.records().len(), 1);
        let drained = buf.drain();
        assert_eq!(drained.len(), 1);
        assert!(buf.is_empty());
    }
}
