// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript tokenizer.
//!
//! Converts a byte stream into a sequence of tokens following the PLRM
//! tokenization rules: numbers, names, strings, hex strings, procedures.

use crate::error::PsError;
use crate::file_store::FileStore;
use crate::object::EntityId;

/// A single PostScript token.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i32),
    Real(f64),
    Name(Vec<u8>, bool),    // (bytes, is_executable)
    LiteralName(Vec<u8>),   // /name
    ImmediateName(Vec<u8>), // //name
    String(Vec<u8>),        // (hello) or <hex>
    ProcBegin,              // {
    ProcEnd,                // }
    ArrayBegin,             // [
    ArrayEnd,               // ]
    DictBegin,              // <<
    DictEnd,                // >>
    /// Binary token byte (128-159) — caller must invoke the binary token parser.
    BinaryTokenByte(u8),
    Eof,
}

/// PostScript tokenizer.
pub struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    /// Current byte position in the input.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Return the remaining input bytes starting at the given position.
    pub fn remaining_from(&self, pos: usize) -> &[u8] {
        &self.input[pos..]
    }

    /// Advance the position by the given number of bytes.
    pub fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    /// Return the next token, or `None` at EOF.
    pub fn next_token(&mut self) -> Result<Option<Token>, PsError> {
        self.skip_whitespace_and_comments();

        if self.pos >= self.input.len() {
            return Ok(None);
        }

        let b = self.input[self.pos];
        match b {
            b'(' => self.scan_string().map(Some),
            b'<' => {
                if self.pos + 1 < self.input.len() && self.input[self.pos + 1] == b'<' {
                    self.pos += 2;
                    Ok(Some(Token::DictBegin))
                } else if self.pos + 1 < self.input.len() && self.input[self.pos + 1] == b'~' {
                    self.scan_ascii85_string().map(Some)
                } else {
                    self.scan_hex_string().map(Some)
                }
            }
            b'>' => {
                if self.pos + 1 < self.input.len() && self.input[self.pos + 1] == b'>' {
                    self.pos += 2;
                    Ok(Some(Token::DictEnd))
                } else {
                    Err(PsError::SyntaxError)
                }
            }
            b'{' => {
                self.pos += 1;
                Ok(Some(Token::ProcBegin))
            }
            b'}' => {
                self.pos += 1;
                Ok(Some(Token::ProcEnd))
            }
            b'[' => {
                self.pos += 1;
                Ok(Some(Token::ArrayBegin))
            }
            b']' => {
                self.pos += 1;
                Ok(Some(Token::ArrayEnd))
            }
            b'/' => {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'/' {
                    self.pos += 1;
                    Ok(Some(self.scan_immediate_name()))
                } else {
                    Ok(Some(self.scan_literal_name()))
                }
            }
            // Binary token bytes
            128..=159 => {
                self.pos += 1;
                Ok(Some(Token::BinaryTokenByte(b)))
            }
            _ => {
                // Try number first, fall back to name
                if let Some(tok) = self.try_scan_number() {
                    Ok(Some(tok))
                } else {
                    Ok(Some(self.scan_name()))
                }
            }
        }
    }

    fn skip_whitespace_and_comments(&mut self) {
        while self.pos < self.input.len() {
            let b = self.input[self.pos];
            if Self::is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                // Skip to end of line
                while self.pos < self.input.len()
                    && self.input[self.pos] != b'\n'
                    && self.input[self.pos] != b'\r'
                {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Try to scan a number. Returns `None` if the token at pos is not a valid number.
    fn try_scan_number(&mut self) -> Option<Token> {
        let start = self.pos;
        let bytes = self.input;
        let len = bytes.len();

        if start >= len {
            return None;
        }

        // Collect the token (up to whitespace, delimiter, or binary token byte)
        let mut end = start;
        while end < len
            && !Self::is_whitespace(bytes[end])
            && !Self::is_delimiter(bytes[end])
            && !is_binary_token_byte(bytes[end])
        {
            end += 1;
        }

        if end == start {
            return None;
        }

        let token_bytes = &bytes[start..end];

        if let Some(tok) = try_parse_number_token(token_bytes) {
            self.pos = end;
            Some(tok)
        } else {
            None
        }
    }

    fn scan_name(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.input.len()
            && !Self::is_whitespace(self.input[self.pos])
            && !Self::is_delimiter(self.input[self.pos])
            && !is_binary_token_byte(self.input[self.pos])
        {
            self.pos += 1;
        }
        let name = self.input[start..self.pos].to_vec();
        Token::Name(name, true) // executable name
    }

    fn scan_literal_name(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.input.len()
            && !Self::is_whitespace(self.input[self.pos])
            && !Self::is_delimiter(self.input[self.pos])
            && !is_binary_token_byte(self.input[self.pos])
        {
            self.pos += 1;
        }
        let name = self.input[start..self.pos].to_vec();
        Token::LiteralName(name)
    }

    fn scan_immediate_name(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.input.len()
            && !Self::is_whitespace(self.input[self.pos])
            && !Self::is_delimiter(self.input[self.pos])
            && !is_binary_token_byte(self.input[self.pos])
        {
            self.pos += 1;
        }
        let name = self.input[start..self.pos].to_vec();
        Token::ImmediateName(name)
    }

    /// Scan a parenthesized string: `(...)` with escape handling and balanced parens.
    fn scan_string(&mut self) -> Result<Token, PsError> {
        self.pos += 1; // skip opening '('
        let mut result = Vec::new();
        let mut depth = 1;

        while self.pos < self.input.len() && depth > 0 {
            let b = self.input[self.pos];
            match b {
                b'(' => {
                    depth += 1;
                    result.push(b'(');
                    self.pos += 1;
                }
                b')' => {
                    depth -= 1;
                    if depth > 0 {
                        result.push(b')');
                    }
                    self.pos += 1;
                }
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.input.len() {
                        return Err(PsError::SyntaxError);
                    }
                    let esc = self.input[self.pos];
                    match esc {
                        b'n' => {
                            result.push(b'\n');
                            self.pos += 1;
                        }
                        b'r' => {
                            result.push(b'\r');
                            self.pos += 1;
                        }
                        b't' => {
                            result.push(b'\t');
                            self.pos += 1;
                        }
                        b'b' => {
                            result.push(0x08);
                            self.pos += 1;
                        }
                        b'f' => {
                            result.push(0x0C);
                            self.pos += 1;
                        }
                        b'\\' => {
                            result.push(b'\\');
                            self.pos += 1;
                        }
                        b'(' => {
                            result.push(b'(');
                            self.pos += 1;
                        }
                        b')' => {
                            result.push(b')');
                            self.pos += 1;
                        }
                        b'\n' => {
                            // Line continuation — skip
                            self.pos += 1;
                        }
                        b'\r' => {
                            // Line continuation — skip (and skip \n if follows)
                            self.pos += 1;
                            if self.pos < self.input.len() && self.input[self.pos] == b'\n' {
                                self.pos += 1;
                            }
                        }
                        b'0'..=b'7' => {
                            // Octal escape: 1-3 digits
                            let mut val: u8 = esc - b'0';
                            self.pos += 1;
                            for _ in 0..2 {
                                if self.pos < self.input.len()
                                    && self.input[self.pos] >= b'0'
                                    && self.input[self.pos] <= b'7'
                                {
                                    val = (val << 3) | (self.input[self.pos] - b'0');
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            }
                            result.push(val);
                        }
                        _ => {
                            // Unrecognized escape: just the char itself
                            result.push(esc);
                            self.pos += 1;
                        }
                    }
                }
                _ => {
                    result.push(b);
                    self.pos += 1;
                }
            }
        }

        if depth != 0 {
            return Err(PsError::SyntaxError);
        }

        Ok(Token::String(result))
    }

    /// Scan a hex string: `<...>`.
    fn scan_hex_string(&mut self) -> Result<Token, PsError> {
        self.pos += 1; // skip '<'
        let mut result = Vec::new();
        let mut nibble: Option<u8> = None;

        while self.pos < self.input.len() {
            let b = self.input[self.pos];
            if b == b'>' {
                self.pos += 1;
                // If we have a pending nibble, pad with 0
                if let Some(high) = nibble {
                    result.push(high << 4);
                }
                return Ok(Token::String(result));
            }

            if Self::is_whitespace(b) {
                self.pos += 1;
                continue;
            }

            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(PsError::SyntaxError),
            };

            match nibble {
                None => nibble = Some(digit),
                Some(high) => {
                    result.push((high << 4) | digit);
                    nibble = None;
                }
            }
            self.pos += 1;
        }

        Err(PsError::SyntaxError) // unterminated hex string
    }

    /// Scan an ASCII85 string: `<~...~>`.
    fn scan_ascii85_string(&mut self) -> Result<Token, PsError> {
        self.pos += 2; // skip '<~'
        let mut encoded = Vec::new();

        while self.pos < self.input.len() {
            let b = self.input[self.pos];
            if b == b'~' {
                self.pos += 1;
                if self.pos < self.input.len() && self.input[self.pos] == b'>' {
                    self.pos += 1;
                    return Ok(Token::String(Self::decode_ascii85(&encoded)?));
                }
                return Err(PsError::SyntaxError);
            }
            if !Self::is_whitespace(b) {
                encoded.push(b);
            }
            self.pos += 1;
        }

        Err(PsError::SyntaxError)
    }

    fn decode_ascii85(data: &[u8]) -> Result<Vec<u8>, PsError> {
        decode_ascii85(data)
    }

    fn is_whitespace(b: u8) -> bool {
        is_whitespace(b)
    }

    fn is_delimiter(b: u8) -> bool {
        is_delimiter(b)
    }
}

// ─── Standalone helpers (shared by slice-based and streaming tokenizers) ─────

/// PostScript whitespace: all bytes ≤ 0x20 (matching PostForge).
fn is_whitespace(b: u8) -> bool {
    b <= b' '
}

/// Binary token byte (128-159) — terminates names and numbers.
fn is_binary_token_byte(b: u8) -> bool {
    (128..=159).contains(&b)
}

/// PostScript delimiter characters.
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Decode an ASCII85-encoded byte sequence.
fn decode_ascii85(data: &[u8]) -> Result<Vec<u8>, PsError> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < data.len() {
        if data[i] == b'z' {
            result.extend_from_slice(&[0, 0, 0, 0]);
            i += 1;
            continue;
        }

        let mut group = [0u8; 5];
        let mut count = 0;
        while count < 5 && i < data.len() && data[i] != b'z' {
            if data[i] < b'!' || data[i] > b'u' {
                return Err(PsError::SyntaxError);
            }
            group[count] = data[i] - b'!';
            count += 1;
            i += 1;
        }

        if count < 2 {
            if count == 1 {
                return Err(PsError::SyntaxError);
            }
            break;
        }

        // Pad remaining with 'u' (84)
        for g in group.iter_mut().skip(count) {
            *g = 84;
        }

        let mut value: u32 = 0;
        for &g in &group {
            value = value
                .checked_mul(85)
                .and_then(|v| v.checked_add(g as u32))
                .ok_or(PsError::SyntaxError)?;
        }

        let bytes = value.to_be_bytes();
        let output_count = count - 1;
        result.extend_from_slice(&bytes[..output_count]);
    }

    Ok(result)
}

/// Try to parse a byte sequence as a PostScript number token.
fn try_parse_number_token(token_bytes: &[u8]) -> Option<Token> {
    if token_bytes.is_empty() {
        return None;
    }

    // Try radix: base#digits
    if let Some(result) = try_parse_radix(token_bytes) {
        return Some(result);
    }

    let s = std::str::from_utf8(token_bytes).ok()?;

    let first = token_bytes[0];
    let looks_numeric = first.is_ascii_digit()
        || ((first == b'+' || first == b'-')
            && token_bytes.len() > 1
            && (token_bytes[1].is_ascii_digit() || token_bytes[1] == b'.'))
        || (first == b'.' && token_bytes.len() > 1 && token_bytes[1].is_ascii_digit());

    if !looks_numeric {
        return None;
    }

    let is_real = s.contains('.') || s.contains('e') || s.contains('E');

    if is_real {
        return s.parse::<f64>().ok().map(Token::Real);
    }

    if let Ok(v) = s.parse::<i32>() {
        return Some(Token::Int(v));
    }
    if let Ok(v) = s.parse::<i64>() {
        return Some(Token::Real(v as f64));
    }
    s.parse::<f64>().ok().map(Token::Real)
}

/// Try to parse a radix number: `base#digits`.
fn try_parse_radix(token: &[u8]) -> Option<Token> {
    let s = std::str::from_utf8(token).ok()?;
    let hash_pos = s.find('#')?;
    let base_str = &s[..hash_pos];
    let digits_str = &s[hash_pos + 1..];
    if digits_str.is_empty() {
        return None;
    }
    let base: u32 = base_str.parse().ok()?;
    if !(2..=36).contains(&base) {
        return None;
    }
    let value = i64::from_str_radix(digits_str, base).ok()?;
    if value >= i32::MIN as i64 && value <= i32::MAX as i64 {
        Some(Token::Int(value as i32))
    } else {
        Some(Token::Real(value as f64))
    }
}

// ─── Streaming tokenizer (byte-at-a-time from FileStore) ────────────────────

/// Read the next token from a file/filter stream, one byte at a time.
///
/// Returns the token and the number of newlines consumed in leading
/// whitespace/comments. Returns `None` on EOF.
pub fn stream_next_token(
    files: &mut FileStore,
    entity: EntityId,
) -> Result<Option<(Token, u32)>, PsError> {
    let mut newlines = 0u32;

    // Skip whitespace and comments to find the first significant byte.
    let first = loop {
        match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            None => return Ok(None),
            Some(b) if is_whitespace(b) => {
                if b == b'\n' || b == b'\r' {
                    newlines += 1;
                }
                continue;
            }
            Some(b'%') => {
                // Skip comment until end of line.
                loop {
                    match files.read_byte(entity).map_err(|_| PsError::IOError)? {
                        None => return Ok(None),
                        Some(b'\n') | Some(b'\r') => {
                            newlines += 1;
                            break;
                        }
                        Some(_) => {}
                    }
                }
                continue;
            }
            Some(b) => break b,
        }
    };

    let token = match first {
        b'(' => stream_scan_string(files, entity)?,
        b'<' => match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            Some(b'<') => Token::DictBegin,
            Some(b'~') => stream_scan_ascii85(files, entity)?,
            other => {
                if let Some(b) = other {
                    files.putback_bytes(entity, &[b]);
                }
                stream_scan_hex_string(files, entity)?
            }
        },
        b'>' => match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            Some(b'>') => Token::DictEnd,
            _ => return Err(PsError::SyntaxError),
        },
        b'{' => Token::ProcBegin,
        b'}' => Token::ProcEnd,
        b'[' => Token::ArrayBegin,
        b']' => Token::ArrayEnd,
        b'/' => match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            Some(b'/') => {
                let name = stream_read_name_bytes(files, entity)?;
                Token::ImmediateName(name)
            }
            Some(b) if !is_whitespace(b) && !is_delimiter(b) => {
                files.putback_bytes(entity, &[b]);
                let name = stream_read_name_bytes(files, entity)?;
                Token::LiteralName(name)
            }
            other => {
                if let Some(b) = other {
                    files.putback_bytes(entity, &[b]);
                }
                Token::LiteralName(Vec::new())
            }
        },
        // Binary token bytes
        128..=159 => Token::BinaryTokenByte(first),
        _ => {
            // Number or executable name — collect bytes until delimiter.
            let mut token_bytes = vec![first];
            loop {
                match files.read_byte(entity).map_err(|_| PsError::IOError)? {
                    None => break,
                    Some(b) if is_whitespace(b) => {
                        // PLRM: trailing whitespace consumed for numbers and
                        // executable names.  Critical for `RD` followed by
                        // binary charstring data.
                        if b == b'\n' || b == b'\r' {
                            newlines += 1;
                        }
                        break;
                    }
                    Some(b) if is_delimiter(b) => {
                        files.putback_bytes(entity, &[b]);
                        break;
                    }
                    // Binary token bytes terminate names/numbers
                    Some(b @ 128..=159) => {
                        files.putback_bytes(entity, &[b]);
                        break;
                    }
                    Some(b) => token_bytes.push(b),
                }
            }
            try_parse_number_token(&token_bytes).unwrap_or(Token::Name(token_bytes, true))
        }
    };

    Ok(Some((token, newlines)))
}

/// Read name bytes from a stream until whitespace or delimiter.
/// Always puts back the terminating byte (literal/immediate names
/// do NOT consume trailing whitespace per PLRM).
fn stream_read_name_bytes(files: &mut FileStore, entity: EntityId) -> Result<Vec<u8>, PsError> {
    let mut name = Vec::new();
    loop {
        match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            None => break,
            Some(b) if is_whitespace(b) || is_delimiter(b) || is_binary_token_byte(b) => {
                files.putback_bytes(entity, &[b]);
                break;
            }
            Some(b) => name.push(b),
        }
    }
    Ok(name)
}

/// Scan a parenthesised string from a stream: `(...)`.
fn stream_scan_string(files: &mut FileStore, entity: EntityId) -> Result<Token, PsError> {
    let mut result = Vec::new();
    let mut depth: u32 = 1;

    while depth > 0 {
        match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            None => return Err(PsError::SyntaxError),
            Some(b'(') => {
                depth += 1;
                result.push(b'(');
            }
            Some(b')') => {
                depth -= 1;
                if depth > 0 {
                    result.push(b')');
                }
            }
            Some(b'\\') => {
                let esc = files
                    .read_byte(entity)
                    .map_err(|_| PsError::IOError)?
                    .ok_or(PsError::SyntaxError)?;
                match esc {
                    b'n' => result.push(b'\n'),
                    b'r' => result.push(b'\r'),
                    b't' => result.push(b'\t'),
                    b'b' => result.push(0x08),
                    b'f' => result.push(0x0C),
                    b'\\' => result.push(b'\\'),
                    b'(' => result.push(b'('),
                    b')' => result.push(b')'),
                    b'\n' => {} // line continuation
                    b'\r' => {
                        // \r\n is a single line continuation.
                        if let Some(next) = files.read_byte(entity).map_err(|_| PsError::IOError)?
                            && next != b'\n'
                        {
                            files.putback_bytes(entity, &[next]);
                        }
                    }
                    b'0'..=b'7' => {
                        let mut val = esc - b'0';
                        for _ in 0..2 {
                            match files.read_byte(entity).map_err(|_| PsError::IOError)? {
                                Some(b @ b'0'..=b'7') => val = (val << 3) | (b - b'0'),
                                Some(other) => {
                                    files.putback_bytes(entity, &[other]);
                                    break;
                                }
                                None => break,
                            }
                        }
                        result.push(val);
                    }
                    _ => result.push(esc),
                }
            }
            Some(b) => result.push(b),
        }
    }

    Ok(Token::String(result))
}

/// Scan a hex string from a stream: `<...>`.
fn stream_scan_hex_string(files: &mut FileStore, entity: EntityId) -> Result<Token, PsError> {
    let mut result = Vec::new();
    let mut nibble: Option<u8> = None;

    loop {
        match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            None => return Err(PsError::SyntaxError),
            Some(b'>') => {
                if let Some(high) = nibble {
                    result.push(high << 4);
                }
                return Ok(Token::String(result));
            }
            Some(b) if is_whitespace(b) => continue,
            Some(b) => {
                let digit = match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    b'A'..=b'F' => b - b'A' + 10,
                    _ => return Err(PsError::SyntaxError),
                };
                match nibble {
                    None => nibble = Some(digit),
                    Some(high) => {
                        result.push((high << 4) | digit);
                        nibble = None;
                    }
                }
            }
        }
    }
}

/// Scan an ASCII85 string from a stream: `<~...~>`.
fn stream_scan_ascii85(files: &mut FileStore, entity: EntityId) -> Result<Token, PsError> {
    let mut encoded = Vec::new();

    loop {
        match files.read_byte(entity).map_err(|_| PsError::IOError)? {
            None => return Err(PsError::SyntaxError),
            Some(b'~') => match files.read_byte(entity).map_err(|_| PsError::IOError)? {
                Some(b'>') => return Ok(Token::String(decode_ascii85(&encoded)?)),
                _ => return Err(PsError::SyntaxError),
            },
            Some(b) if is_whitespace(b) => continue,
            Some(b) => encoded.push(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenize_all(input: &[u8]) -> Vec<Token> {
        let mut t = Tokenizer::new(input);
        let mut tokens = Vec::new();
        while let Ok(Some(tok)) = t.next_token() {
            tokens.push(tok);
        }
        tokens
    }

    #[test]
    fn test_integers() {
        assert_eq!(tokenize_all(b"42"), vec![Token::Int(42)]);
        assert_eq!(tokenize_all(b"-7"), vec![Token::Int(-7)]);
        assert_eq!(tokenize_all(b"+3"), vec![Token::Int(3)]);
        assert_eq!(tokenize_all(b"0"), vec![Token::Int(0)]);
    }

    #[test]
    fn test_reals() {
        assert_eq!(tokenize_all(b"2.5"), vec![Token::Real(2.5)]);
        assert_eq!(tokenize_all(b"-0.5"), vec![Token::Real(-0.5)]);
        assert_eq!(tokenize_all(b"1e10"), vec![Token::Real(1e10)]);
        assert_eq!(tokenize_all(b"1.5E-3"), vec![Token::Real(1.5e-3)]);
    }

    #[test]
    fn test_radix() {
        assert_eq!(tokenize_all(b"16#FF"), vec![Token::Int(255)]);
        assert_eq!(tokenize_all(b"2#1010"), vec![Token::Int(10)]);
        assert_eq!(tokenize_all(b"8#77"), vec![Token::Int(63)]);
    }

    #[test]
    fn test_names() {
        assert_eq!(
            tokenize_all(b"add"),
            vec![Token::Name(b"add".to_vec(), true)]
        );
        assert_eq!(
            tokenize_all(b"/foo"),
            vec![Token::LiteralName(b"foo".to_vec())]
        );
        assert_eq!(
            tokenize_all(b"//bar"),
            vec![Token::ImmediateName(b"bar".to_vec())]
        );
    }

    #[test]
    fn test_string_basic() {
        assert_eq!(
            tokenize_all(b"(hello)"),
            vec![Token::String(b"hello".to_vec())]
        );
    }

    #[test]
    fn test_string_escapes() {
        assert_eq!(
            tokenize_all(b"(a\\nb)"),
            vec![Token::String(b"a\nb".to_vec())]
        );
        assert_eq!(
            tokenize_all(b"(a\\\\b)"),
            vec![Token::String(b"a\\b".to_vec())]
        );
        assert_eq!(
            tokenize_all(b"(\\110\\145\\154\\154\\157)"),
            vec![Token::String(b"Hello".to_vec())]
        );
    }

    #[test]
    fn test_string_balanced_parens() {
        assert_eq!(
            tokenize_all(b"(a(b)c)"),
            vec![Token::String(b"a(b)c".to_vec())]
        );
    }

    #[test]
    fn test_hex_string() {
        assert_eq!(
            tokenize_all(b"<48656C6C6F>"),
            vec![Token::String(b"Hello".to_vec())]
        );
        // Odd nibble padded
        assert_eq!(tokenize_all(b"<0>"), vec![Token::String(vec![0x00])]);
    }

    #[test]
    fn test_procedures() {
        let tokens = tokenize_all(b"{ add }");
        assert_eq!(
            tokens,
            vec![
                Token::ProcBegin,
                Token::Name(b"add".to_vec(), true),
                Token::ProcEnd,
            ]
        );
    }

    #[test]
    fn test_comments() {
        let tokens = tokenize_all(b"3 % comment\n4 add");
        assert_eq!(
            tokens,
            vec![
                Token::Int(3),
                Token::Int(4),
                Token::Name(b"add".to_vec(), true),
            ]
        );
    }

    #[test]
    fn test_full_program() {
        let tokens = tokenize_all(b"3 4 add 7 eq { (YES\\n) print } { (NO\\n) print } ifelse");
        assert_eq!(tokens.len(), 14);
        assert_eq!(tokens[0], Token::Int(3));
        assert_eq!(tokens[1], Token::Int(4));
        assert_eq!(tokens[2], Token::Name(b"add".to_vec(), true));
        assert_eq!(tokens[3], Token::Int(7));
        assert_eq!(tokens[4], Token::Name(b"eq".to_vec(), true));
        assert_eq!(tokens[5], Token::ProcBegin);
        assert_eq!(tokens[6], Token::String(b"YES\n".to_vec()));
        assert_eq!(tokens[7], Token::Name(b"print".to_vec(), true));
        assert_eq!(tokens[8], Token::ProcEnd);
        assert_eq!(tokens[9], Token::ProcBegin);
        assert_eq!(tokens[10], Token::String(b"NO\n".to_vec()));
        assert_eq!(tokens[11], Token::Name(b"print".to_vec(), true));
        assert_eq!(tokens[12], Token::ProcEnd);
        assert_eq!(tokens[13], Token::Name(b"ifelse".to_vec(), true));
    }

    #[test]
    fn test_dict_delimiters() {
        let tokens = tokenize_all(b"<< /foo 42 >>");
        assert_eq!(
            tokens,
            vec![
                Token::DictBegin,
                Token::LiteralName(b"foo".to_vec()),
                Token::Int(42),
                Token::DictEnd,
            ]
        );
    }
}
