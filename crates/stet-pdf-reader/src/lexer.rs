// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF tokenizer.

use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};

/// PDF token types.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Bool(bool),
    Int(i64),
    Real(f64),
    /// Name without leading `/`.
    Name(Vec<u8>),
    /// Literal string `(...)`, decoded.
    LitString(Vec<u8>),
    /// Hex string `<...>`, decoded.
    HexString(Vec<u8>),
    /// `[`
    ArrayBegin,
    /// `]`
    ArrayEnd,
    /// `<<`
    DictBegin,
    /// `>>`
    DictEnd,
    /// Keywords: `obj`, `endobj`, `stream`, `endstream`, `R`, `null`, `xref`, `trailer`, etc.
    Keyword(Vec<u8>),
    Eof,
}

/// PDF lexer operating on a byte slice with a cursor.
pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Create a lexer starting at a given offset.
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    /// Current byte offset.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Set the cursor position.
    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Underlying data slice.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Read the next token, advancing the cursor.
    pub fn next_token(&mut self) -> Result<Token, PdfError> {
        self.skip_whitespace_and_comments();

        if self.pos >= self.data.len() {
            return Ok(Token::Eof);
        }

        let b = self.data[self.pos];
        match b {
            b'/' => self.read_name(),
            b'(' => self.read_literal_string(),
            b'<' => {
                if self.pos + 1 < self.data.len() && self.data[self.pos + 1] == b'<' {
                    self.pos += 2;
                    Ok(Token::DictBegin)
                } else {
                    self.read_hex_string()
                }
            }
            b'>' => {
                if self.pos + 1 < self.data.len() && self.data[self.pos + 1] == b'>' {
                    self.pos += 2;
                    Ok(Token::DictEnd)
                } else {
                    self.pos += 1;
                    Err(PdfError::UnexpectedToken {
                        expected: ">>".into(),
                        got: ">".into(),
                    })
                }
            }
            b'[' => {
                self.pos += 1;
                Ok(Token::ArrayBegin)
            }
            b']' => {
                self.pos += 1;
                Ok(Token::ArrayEnd)
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.read_number(),
            _ if b.is_ascii_alphabetic() => self.read_keyword(),
            _ => {
                let ch = b as char;
                self.pos += 1;
                Err(PdfError::UnexpectedToken {
                    expected: "token".into(),
                    got: format!("byte 0x{b:02x} '{ch}'"),
                })
            }
        }
    }

    /// Peek at the next token without advancing.
    pub fn peek_token(&mut self) -> Result<Token, PdfError> {
        let saved = self.pos;
        let tok = self.next_token();
        self.pos = saved;
        tok
    }

    /// Skip whitespace (space, tab, CR, LF, FF, NUL) and comments (% to EOL).
    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while self.pos < self.data.len() && is_whitespace(self.data[self.pos]) {
                self.pos += 1;
            }
            // Skip comments
            if self.pos < self.data.len() && self.data[self.pos] == b'%' {
                while self.pos < self.data.len()
                    && self.data[self.pos] != b'\n'
                    && self.data[self.pos] != b'\r'
                {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Read a number token (integer or real).
    fn read_number(&mut self) -> Result<Token, PdfError> {
        let start = self.pos;
        let mut has_dot = false;

        // Optional sign
        if self.pos < self.data.len() && (self.data[self.pos] == b'+' || self.data[self.pos] == b'-')
        {
            self.pos += 1;
        }

        // Digits and optional decimal point
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if b == b'.' && !has_dot {
                has_dot = true;
                self.pos += 1;
            } else if b.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }

        let s = &self.data[start..self.pos];
        if s == b"+" || s == b"-" || s == b"." || s == b"+." || s == b"-." {
            // Not a valid number — treat as keyword
            return Ok(Token::Keyword(s.to_vec()));
        }

        if has_dot {
            let s_str = std::str::from_utf8(s).map_err(|_| PdfError::Other("invalid number".into()))?;
            let f: f64 = s_str
                .parse()
                .map_err(|_| PdfError::Other(format!("invalid real: {s_str}")))?;
            Ok(Token::Real(f))
        } else {
            let s_str = std::str::from_utf8(s).map_err(|_| PdfError::Other("invalid number".into()))?;
            let n: i64 = s_str
                .parse()
                .map_err(|_| PdfError::Other(format!("invalid integer: {s_str}")))?;
            Ok(Token::Int(n))
        }
    }

    /// Read a name token (after consuming `/`).
    fn read_name(&mut self) -> Result<Token, PdfError> {
        self.pos += 1; // skip '/'
        let mut name = Vec::new();

        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if is_whitespace(b) || is_delimiter(b) {
                break;
            }
            if b == b'#' && self.pos + 2 < self.data.len() {
                // Hex escape
                let hi = hex_digit(self.data[self.pos + 1]);
                let lo = hex_digit(self.data[self.pos + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    name.push(h << 4 | l);
                    self.pos += 3;
                    continue;
                }
            }
            name.push(b);
            self.pos += 1;
        }

        Ok(Token::Name(name))
    }

    /// Read a literal string `(...)` with escapes and nested parens.
    fn read_literal_string(&mut self) -> Result<Token, PdfError> {
        self.pos += 1; // skip '('
        let mut result = Vec::new();
        let mut depth = 1u32;

        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            match b {
                b'(' => {
                    depth += 1;
                    result.push(b);
                    self.pos += 1;
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return Ok(Token::LitString(result));
                    }
                    result.push(b);
                    self.pos += 1;
                }
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.data.len() {
                        break;
                    }
                    let esc = self.data[self.pos];
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
                        b'(' | b')' | b'\\' => {
                            result.push(esc);
                            self.pos += 1;
                        }
                        b'\r' => {
                            // Line continuation
                            self.pos += 1;
                            if self.pos < self.data.len() && self.data[self.pos] == b'\n' {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {
                            // Line continuation
                            self.pos += 1;
                        }
                        b'0'..=b'7' => {
                            // Octal escape (1-3 digits)
                            let mut val = esc - b'0';
                            self.pos += 1;
                            if self.pos < self.data.len()
                                && self.data[self.pos] >= b'0'
                                && self.data[self.pos] <= b'7'
                            {
                                val = val * 8 + (self.data[self.pos] - b'0');
                                self.pos += 1;
                                if self.pos < self.data.len()
                                    && self.data[self.pos] >= b'0'
                                    && self.data[self.pos] <= b'7'
                                {
                                    val = val * 8 + (self.data[self.pos] - b'0');
                                    self.pos += 1;
                                }
                            }
                            result.push(val);
                        }
                        _ => {
                            // Unknown escape — just include the character
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

        Err(PdfError::Unterminated("string"))
    }

    /// Read a hex string `<...>`.
    fn read_hex_string(&mut self) -> Result<Token, PdfError> {
        self.pos += 1; // skip '<'
        let mut result = Vec::new();
        let mut high_nibble: Option<u8> = None;

        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if b == b'>' {
                self.pos += 1;
                // Odd number of hex digits: implicit trailing 0
                if let Some(h) = high_nibble {
                    result.push(h << 4);
                }
                return Ok(Token::HexString(result));
            }
            if is_whitespace(b) {
                self.pos += 1;
                continue;
            }
            if let Some(nibble) = hex_digit(b) {
                match high_nibble {
                    None => high_nibble = Some(nibble),
                    Some(h) => {
                        result.push(h << 4 | nibble);
                        high_nibble = None;
                    }
                }
                self.pos += 1;
            } else {
                self.pos += 1;
                return Err(PdfError::UnexpectedToken {
                    expected: "hex digit".into(),
                    got: format!("byte 0x{b:02x}"),
                });
            }
        }

        Err(PdfError::Unterminated("hex string"))
    }

    /// Read a keyword (alphabetic sequence).
    fn read_keyword(&mut self) -> Result<Token, PdfError> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let word = &self.data[start..self.pos];
        match word {
            b"true" => Ok(Token::Bool(true)),
            b"false" => Ok(Token::Bool(false)),
            _ => Ok(Token::Keyword(word.to_vec())),
        }
    }
}

/// Parse a PDF object from the lexer (recursive descent).
///
/// This handles arrays, dicts, and indirect references (`N G R`).
pub fn parse_object(lexer: &mut Lexer) -> Result<PdfObj, PdfError> {
    let tok = lexer.next_token()?;
    parse_object_from_token(lexer, tok)
}

/// Parse a PDF object given an already-consumed first token.
pub fn parse_object_from_token(lexer: &mut Lexer, tok: Token) -> Result<PdfObj, PdfError> {
    match tok {
        Token::Bool(b) => Ok(PdfObj::Bool(b)),
        Token::Real(f) => Ok(PdfObj::Real(f)),
        Token::Int(n) => {
            // Could be start of indirect reference: N G R
            let saved = lexer.pos();
            match lexer.next_token() {
                Ok(Token::Int(g)) => match lexer.next_token() {
                    Ok(Token::Keyword(ref kw)) if kw == b"R" => {
                        Ok(PdfObj::Ref(n as u32, g as u16))
                    }
                    _ => {
                        lexer.set_pos(saved);
                        Ok(PdfObj::Int(n))
                    }
                },
                _ => {
                    lexer.set_pos(saved);
                    Ok(PdfObj::Int(n))
                }
            }
        }
        Token::Name(n) => Ok(PdfObj::Name(n)),
        Token::LitString(s) => Ok(PdfObj::Str(s)),
        Token::HexString(s) => Ok(PdfObj::Str(s)),
        Token::Keyword(ref kw) if kw == b"null" => Ok(PdfObj::Null),
        Token::ArrayBegin => {
            let mut elems = Vec::new();
            loop {
                let t = lexer.next_token()?;
                if t == Token::ArrayEnd || t == Token::Eof {
                    break;
                }
                elems.push(parse_object_from_token(lexer, t)?);
            }
            Ok(PdfObj::Array(elems))
        }
        Token::DictBegin => {
            let dict = parse_dict_body(lexer)?;
            Ok(PdfObj::Dict(dict))
        }
        _ => Err(PdfError::UnexpectedToken {
            expected: "object".into(),
            got: format!("{tok:?}"),
        }),
    }
}

/// Parse dictionary entries until `>>`, returning a PdfDict.
pub fn parse_dict_body(lexer: &mut Lexer) -> Result<PdfDict, PdfError> {
    let mut dict = PdfDict::new();
    loop {
        let t = lexer.next_token()?;
        match t {
            Token::DictEnd | Token::Eof => break,
            Token::Name(key) => {
                let val = parse_object(lexer)?;
                dict.insert(key, val);
            }
            _ => {
                // Tolerate unexpected tokens in dict (skip and continue)
                continue;
            }
        }
    }
    Ok(dict)
}

/// PDF whitespace characters (PDF spec 7.2.2).
fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0x00)
}

/// PDF delimiter characters.
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Convert a hex digit to its value (0-15).
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenize(input: &[u8]) -> Vec<Token> {
        let mut lexer = Lexer::new(input);
        let mut tokens = Vec::new();
        loop {
            let tok = lexer.next_token().unwrap();
            if tok == Token::Eof {
                break;
            }
            tokens.push(tok);
        }
        tokens
    }

    #[test]
    fn integers() {
        assert_eq!(tokenize(b"42"), vec![Token::Int(42)]);
        assert_eq!(tokenize(b"-7"), vec![Token::Int(-7)]);
        assert_eq!(tokenize(b"+5"), vec![Token::Int(5)]);
        assert_eq!(tokenize(b"0"), vec![Token::Int(0)]);
    }

    #[test]
    fn reals() {
        assert_eq!(tokenize(b"3.14"), vec![Token::Real(3.14)]);
        assert_eq!(tokenize(b".5"), vec![Token::Real(0.5)]);
        assert_eq!(tokenize(b"-2.0"), vec![Token::Real(-2.0)]);
    }

    #[test]
    fn names() {
        assert_eq!(tokenize(b"/Type"), vec![Token::Name(b"Type".to_vec())]);
        assert_eq!(tokenize(b"/"), vec![Token::Name(b"".to_vec())]); // empty name
        assert_eq!(
            tokenize(b"/A#20B"),
            vec![Token::Name(b"A B".to_vec())]
        ); // hex escape
    }

    #[test]
    fn strings() {
        assert_eq!(
            tokenize(b"(hello)"),
            vec![Token::LitString(b"hello".to_vec())]
        );
        assert_eq!(
            tokenize(b"(nested (parens))"),
            vec![Token::LitString(b"nested (parens)".to_vec())]
        );
        assert_eq!(
            tokenize(b"(line\\nfeed)"),
            vec![Token::LitString(b"line\nfeed".to_vec())]
        );
        assert_eq!(
            tokenize(b"(octal\\101)"),
            vec![Token::LitString(b"octalA".to_vec())]
        );
    }

    #[test]
    fn hex_strings() {
        assert_eq!(
            tokenize(b"<48656C6C6F>"),
            vec![Token::HexString(b"Hello".to_vec())]
        );
        // Odd digits: trailing 0
        assert_eq!(tokenize(b"<ABC>"), vec![Token::HexString(vec![0xAB, 0xC0])]);
        // Whitespace inside
        assert_eq!(
            tokenize(b"<48 65 6C>"),
            vec![Token::HexString(b"Hel".to_vec())]
        );
    }

    #[test]
    fn booleans_and_null() {
        assert_eq!(tokenize(b"true"), vec![Token::Bool(true)]);
        assert_eq!(tokenize(b"false"), vec![Token::Bool(false)]);
        let obj = parse_object(&mut Lexer::new(b"null")).unwrap();
        assert_eq!(obj, PdfObj::Null);
    }

    #[test]
    fn delimiters() {
        let toks = tokenize(b"<< >> [ ]");
        assert_eq!(
            toks,
            vec![
                Token::DictBegin,
                Token::DictEnd,
                Token::ArrayBegin,
                Token::ArrayEnd,
            ]
        );
    }

    #[test]
    fn comments_skipped() {
        assert_eq!(tokenize(b"% comment\n42"), vec![Token::Int(42)]);
    }

    #[test]
    fn keywords() {
        assert_eq!(
            tokenize(b"obj endobj stream"),
            vec![
                Token::Keyword(b"obj".to_vec()),
                Token::Keyword(b"endobj".to_vec()),
                Token::Keyword(b"stream".to_vec()),
            ]
        );
    }

    #[test]
    fn parse_array() {
        let obj = parse_object(&mut Lexer::new(b"[1 2 /Name]")).unwrap();
        assert_eq!(
            obj,
            PdfObj::Array(vec![
                PdfObj::Int(1),
                PdfObj::Int(2),
                PdfObj::Name(b"Name".to_vec()),
            ])
        );
    }

    #[test]
    fn parse_dict() {
        let obj = parse_object(&mut Lexer::new(b"<< /Type /Page /Count 5 >>")).unwrap();
        let dict = obj.as_dict().unwrap();
        assert_eq!(dict.get_name(b"Type"), Some(b"Page".as_slice()));
        assert_eq!(dict.get_int(b"Count"), Some(5));
    }

    #[test]
    fn parse_indirect_ref() {
        let obj = parse_object(&mut Lexer::new(b"10 0 R")).unwrap();
        assert_eq!(obj, PdfObj::Ref(10, 0));
    }

    #[test]
    fn parse_nested_dict() {
        let obj = parse_object(&mut Lexer::new(
            b"<< /Resources << /Font << /F1 5 0 R >> >> >>",
        ))
        .unwrap();
        let dict = obj.as_dict().unwrap();
        let res = dict.get_dict(b"Resources").unwrap();
        let font = res.get_dict(b"Font").unwrap();
        assert_eq!(font.get(b"F1"), Some(&PdfObj::Ref(5, 0)));
    }

    #[test]
    fn int_not_ref_at_eof() {
        // A lone integer should not be confused with a ref
        let obj = parse_object(&mut Lexer::new(b"42")).unwrap();
        assert_eq!(obj, PdfObj::Int(42));
    }
}
