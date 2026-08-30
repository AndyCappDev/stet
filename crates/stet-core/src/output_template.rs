// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Output-path templates for the `-o` / `--output` CLI flag.
//!
//! The model is Ghostscript's: the path the user supplies is a *literal
//! template*, and the decision about per-page naming is made from the string
//! alone rather than from the number of pages. A template containing a `%d`
//! conversion expands once per page; one without it names a single file.
//!
//! Deciding from the string matters because PostScript page counts are not
//! knowable in advance — pages arrive as `showpage` executes, so the name of
//! page 1 has to be committed before anyone knows whether a page 2 exists.
//! Ghostscript resolves this by opening the literal path once and streaming
//! every page into it, which silently produces a file holding several
//! concatenated images (measured against gs 10.05.1: three PNG signatures and
//! three `IEND` chunks in one `.png`, exit 0, no warning). stet accepts the
//! same templates but raises [`ExpandError::MultiPageNeedsToken`] on the
//! second page instead, leaving page 1 on disk intact.

/// A parsed `-o` / `--output` template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTemplate {
    /// The template exactly as the user wrote it.
    raw: String,
    /// Position and shape of the `%d` conversion, if the template has one.
    token: Option<Token>,
}

/// A `%d` / `%0Nd` conversion inside a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Token {
    /// Byte offset of the leading `%`.
    start: usize,
    /// Byte offset one past the trailing `d`.
    end: usize,
    /// Minimum field width; `0` when unspecified.
    width: usize,
    /// Whether the width is zero-padded (`%03d`) rather than space-padded.
    zero_pad: bool,
}

/// Why a template could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TemplateError {
    /// The template was the empty string.
    #[error("output path is empty")]
    Empty,
    /// More than one `%d` conversion. Which one numbers the page is
    /// ambiguous, so refuse rather than pick.
    #[error("output template contains more than one '%d' page-number token")]
    MultipleTokens,
    /// A `%` conversion stet does not implement. Only `%d` and `%0Nd` are
    /// supported; anything else is rejected rather than passed through to a
    /// formatter that might interpret it differently.
    #[error(
        "unsupported conversion '%{0}' in output template \
         (only '%d' and '%0Nd', e.g. '%03d', are supported; write '%%' for a literal '%')"
    )]
    UnsupportedConversion(String),
    /// A trailing `%` with nothing after it.
    #[error("output template ends with a lone '%' (write '%%' for a literal '%')")]
    TrailingPercent,
}

/// Why a template could not be expanded for a given page.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExpandError {
    /// A job produced a second page under a template with no `%d`, so every
    /// page would land on the same path.
    #[error(
        "output '{path}' has no '%d' page-number token, but the job produced more than one page\n\
         note: page 1 was written to '{path}'; pages 2 and beyond would overwrite it\n\
         help: use a template such as '{suggestion}'"
    )]
    MultiPageNeedsToken {
        /// The literal path the template names.
        path: String,
        /// A ready-to-paste template derived from `path`.
        suggestion: String,
    },
}

impl OutputTemplate {
    /// Parse a user-supplied `-o` value.
    ///
    /// Recognises `%d` and `%0Nd` as the page-number conversion and `%%` as an
    /// escaped literal `%`. Every other `%` sequence is an error, so a typo
    /// like `%s` is reported rather than silently written to disk.
    pub fn parse(raw: &str) -> Result<Self, TemplateError> {
        if raw.is_empty() {
            return Err(TemplateError::Empty);
        }

        let bytes = raw.as_bytes();
        let mut token: Option<Token> = None;
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] != b'%' {
                i += 1;
                continue;
            }
            let start = i;
            let mut j = i + 1;
            if j >= bytes.len() {
                return Err(TemplateError::TrailingPercent);
            }
            // `%%` — an escaped literal percent, not a conversion.
            if bytes[j] == b'%' {
                i = j + 1;
                continue;
            }
            // Optional zero-pad flag, then an optional decimal width.
            let zero_pad = bytes[j] == b'0';
            let digits_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let width: usize = if j > digits_start {
                raw[digits_start..j].parse().unwrap_or(0)
            } else {
                0
            };
            if j >= bytes.len() || bytes[j] != b'd' {
                // Quote the conversion itself — its width digits plus the one
                // character that broke it — rather than a fixed-length slice
                // of whatever follows, which would drag in unrelated path
                // text ("%s.p" for "out-%s.png"). Stepping by chars keeps a
                // multi-byte offender from splitting mid-character.
                let rest = &raw[start + 1..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                let offender: String = rest.chars().skip(digits.chars().count()).take(1).collect();
                return Err(TemplateError::UnsupportedConversion(format!(
                    "{}{}",
                    digits, offender
                )));
            }
            let end = j + 1;
            if token.is_some() {
                return Err(TemplateError::MultipleTokens);
            }
            token = Some(Token {
                start,
                end,
                width,
                zero_pad,
            });
            i = end;
        }

        Ok(Self {
            raw: raw.to_string(),
            token,
        })
    }

    /// Whether this template carries a `%d` page-number conversion.
    pub fn has_page_token(&self) -> bool {
        self.token.is_some()
    }

    /// The template as the user wrote it.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Expand the template for a 1-based page number.
    ///
    /// `emitted_index` is the 1-based count of pages *actually written* so far
    /// including this one — not the logical page number, which `--pages 3`
    /// would make 3 for a job that emits a single file. A template without a
    /// `%d` is only valid while that count is 1.
    pub fn expand(&self, page_number: i32, emitted_index: u32) -> Result<String, ExpandError> {
        let Some(tok) = self.token else {
            if emitted_index > 1 {
                return Err(ExpandError::MultiPageNeedsToken {
                    path: self.raw.clone(),
                    suggestion: suggest_token_form(&self.raw),
                });
            }
            return Ok(unescape_percents(&self.raw));
        };

        let number = if tok.zero_pad {
            format!("{:0width$}", page_number, width = tok.width)
        } else {
            format!("{:width$}", page_number, width = tok.width)
        };

        let mut out = String::with_capacity(self.raw.len() + number.len());
        out.push_str(&unescape_percents(&self.raw[..tok.start]));
        out.push_str(&number);
        out.push_str(&unescape_percents(&self.raw[tok.end..]));
        Ok(out)
    }
}

/// Collapse `%%` to `%` in the literal segments of a template.
fn unescape_percents(segment: &str) -> String {
    segment.replace("%%", "%")
}

/// Build a `%03d` form of a path, for the "use a template like this" hint.
///
/// Inserts the token before the final extension so the suggestion keeps the
/// file type the user asked for: `out.png` becomes `out-%03d.png`.
fn suggest_token_form(path: &str) -> String {
    match split_extension(path) {
        Some((stem, ext)) => format!("{}-%03d{}", stem, ext),
        None => format!("{}-%03d", path),
    }
}

/// Split a path into (stem, extension-with-dot) at the final `.` of the last
/// path component. Returns `None` when that component has no extension.
pub fn split_extension(path: &str) -> Option<(&str, &str)> {
    let component_start = path.rfind(['/', '\\']).map(|pos| pos + 1).unwrap_or(0);
    let dot = path[component_start..].rfind('.')? + component_start;
    // A leading dot is a hidden file, not an extension.
    if dot == component_start {
        return None;
    }
    Some((&path[..dot], &path[dot..]))
}

/// Insert a `-NNN` page number before a path's extension.
///
/// This is the default multi-page naming used when no `-o` template is given.
pub fn insert_page_number(path: &str, page_number: i32, digits: usize) -> String {
    match split_extension(path) {
        Some((stem, ext)) => format!("{}-{:0width$}{}", stem, page_number, ext, width = digits),
        None => format!("{}-{:0width$}", path, page_number, width = digits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_token_single_page_is_literal() {
        let t = OutputTemplate::parse("out.png").unwrap();
        assert!(!t.has_page_token());
        assert_eq!(t.expand(1, 1).unwrap(), "out.png");
    }

    #[test]
    fn no_token_uses_logical_page_number_without_renaming() {
        // `--pages 7` emits one file; the logical number is 7 but it is still
        // the first emitted page, so the literal path stands.
        let t = OutputTemplate::parse("out.png").unwrap();
        assert_eq!(t.expand(7, 1).unwrap(), "out.png");
    }

    #[test]
    fn no_token_second_emitted_page_errors() {
        let t = OutputTemplate::parse("out.png").unwrap();
        let err = t.expand(2, 2).unwrap_err();
        match err {
            ExpandError::MultiPageNeedsToken { ref suggestion, .. } => {
                assert_eq!(suggestion, "out-%03d.png");
            }
        }
        // The message names the flag value and a usable replacement.
        let text = err.to_string();
        assert!(text.contains("out.png"), "{}", text);
        assert!(text.contains("out-%03d.png"), "{}", text);
    }

    #[test]
    fn plain_token_expands_unpadded() {
        let t = OutputTemplate::parse("p-%d.png").unwrap();
        assert!(t.has_page_token());
        assert_eq!(t.expand(7, 1).unwrap(), "p-7.png");
        assert_eq!(t.expand(1234, 4).unwrap(), "p-1234.png");
    }

    #[test]
    fn zero_padded_token_expands_to_width() {
        let t = OutputTemplate::parse("p-%03d.png").unwrap();
        assert_eq!(t.expand(7, 1).unwrap(), "p-007.png");
        // Numbers wider than the field are not truncated.
        assert_eq!(t.expand(12345, 1).unwrap(), "p-12345.png");
    }

    #[test]
    fn token_expands_for_every_page_including_the_first() {
        // gs expands the token even when only one page is produced.
        let t = OutputTemplate::parse("s-%03d.png").unwrap();
        assert_eq!(t.expand(1, 1).unwrap(), "s-001.png");
    }

    #[test]
    fn token_may_appear_anywhere_including_a_directory() {
        let t = OutputTemplate::parse("/tmp/page%03d/img.png").unwrap();
        assert_eq!(t.expand(2, 2).unwrap(), "/tmp/page002/img.png");
    }

    #[test]
    fn escaped_percent_is_literal_and_not_a_token() {
        let t = OutputTemplate::parse("100%%-scale.png").unwrap();
        assert!(!t.has_page_token());
        assert_eq!(t.expand(1, 1).unwrap(), "100%-scale.png");
    }

    #[test]
    fn escaped_percent_alongside_a_real_token() {
        let t = OutputTemplate::parse("100%%-%02d.png").unwrap();
        assert!(t.has_page_token());
        assert_eq!(t.expand(3, 3).unwrap(), "100%-03.png");
    }

    #[test]
    fn rejects_multiple_tokens() {
        assert_eq!(
            OutputTemplate::parse("%d-%d.png").unwrap_err(),
            TemplateError::MultipleTokens
        );
    }

    #[test]
    fn rejects_unsupported_conversion() {
        // A typo must not reach the filesystem as a literal name.
        let err = OutputTemplate::parse("out-%s.png").unwrap_err();
        // The message quotes the conversion alone, not the path text after it.
        assert_eq!(
            err,
            TemplateError::UnsupportedConversion("s".to_string()),
            "{}",
            err
        );
        assert!(err.to_string().contains("%03d"), "{}", err);

        // A width with the wrong terminator reports the width too.
        assert_eq!(
            OutputTemplate::parse("out-%03x.png").unwrap_err(),
            TemplateError::UnsupportedConversion("03x".to_string())
        );
        // A conversion cut off by end of string is still a conversion error,
        // not a "lone %".
        assert_eq!(
            OutputTemplate::parse("out-%03").unwrap_err(),
            TemplateError::UnsupportedConversion("03".to_string())
        );
        // A multi-byte offender must not panic on a byte-boundary slice.
        assert_eq!(
            OutputTemplate::parse("out-%é.png").unwrap_err(),
            TemplateError::UnsupportedConversion("é".to_string())
        );
    }

    #[test]
    fn rejects_trailing_percent() {
        assert_eq!(
            OutputTemplate::parse("out.png%").unwrap_err(),
            TemplateError::TrailingPercent
        );
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(OutputTemplate::parse("").unwrap_err(), TemplateError::Empty);
    }

    #[test]
    fn extension_splitting_handles_paths_and_dotfiles() {
        assert_eq!(split_extension("out.png"), Some(("out", ".png")));
        assert_eq!(split_extension("/a.b/out.png"), Some(("/a.b/out", ".png")));
        // No extension on the final component, despite a dot in a parent dir.
        assert_eq!(split_extension("/a.b/out"), None);
        assert_eq!(split_extension(".hidden"), None);
    }

    #[test]
    fn suggestion_keeps_the_requested_extension() {
        assert_eq!(suggest_token_form("out.png"), "out-%03d.png");
        assert_eq!(suggest_token_form("/tmp/o.pdf"), "/tmp/o-%03d.pdf");
        assert_eq!(suggest_token_form("out"), "out-%03d");
    }

    #[test]
    fn default_numbering_matches_existing_conventions() {
        // PDF raster default: three digits.
        assert_eq!(insert_page_number("f.png", 1, 3), "f-001.png");
        // PostScript default: four digits.
        assert_eq!(insert_page_number("f.png", 12, 4), "f-0012.png");
    }
}
