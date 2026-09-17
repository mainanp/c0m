//! Lexical analysis: byte stream -> tokens.
//!
//! Per Chapter 3 (Trojan Source mitigation): Unicode bidirectional
//! control characters (U+202A, U+202B, U+202D, U+202E, U+2066-U+2069,
//! U+200F) outside string literal bytes are hard lexer errors, not
//! ignored. This is intentionally checked before any other tokenization.

use crate::span::SourceSpan;
use crate::ParseError;

/// How many continuation bytes follow this UTF-8 lead byte, i.e. how wide
/// (in bytes) the character starting here is, minus one. Needed because a
/// character literal must hold exactly one Unicode scalar value, which can
/// span 1-4 bytes -- unlike string scanning, which never needs to know a
/// character's byte-width since it just passes bytes through untouched.
fn utf8_extra_bytes(first_byte: u8) -> Option<usize> {
    match first_byte {
        0x00..=0x7F => Some(0),
        0xC0..=0xDF => Some(1),
        0xE0..=0xEF => Some(2),
        0xF0..=0xF7 => Some(3),
        _ => None,
    }
}

pub struct Lexer<'a> {
    source: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    file_hash: u64,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str, file_hash: u64) -> Self {
        Self {
            source: source.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            file_hash,
        }
    }

    //Current byte without consuming it.
    fn peek(&self) -> Option<u8> {
        self.source.get(self.pos).copied()
    }

    //Look one byte further ahead that 'peek', without consuming anything
    //Needed for two-character operators like '->' and '=='
    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.source.get(self.pos + offset).copied()
    }

    //Consume and return the current byte, advancing line/col bookkeeping
    fn advance(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    ///Skips spaces, tabs, newlines, and '//' line comments. Loops because
    ///a comment can be followed by more whitespaces, which can be followed
    ///by another comment, etc. - This has to fully settle before the next
    ///real token starts
    fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => {
                    self.advance();
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                _ => break,
            }
        }
    }

    ///Builds a SourceSpan from a token's starting position to the lexer's
    ///current position (i.e. one past the token's last byte).
    fn make_span(&self, start_line: u32, start_col: u32, start_offset: usize) -> SourceSpan {
        SourceSpan {
            file_hash: self.file_hash,
            line: start_line,
            col: start_col,
            byte_offset: start_offset as u32,
            length: (self.pos - start_offset) as u32,
        }
    }

    ///Scans an identifier or keyword. The first character was already
    ///consumed by 'advance()' in 'next_token'; this reads the rest
    fn scan_identifier_or_keyword(&mut self, start_offset: usize) -> Token {
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.advance();
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.source[start_offset..self.pos])
            .expect("ASCII alphanumeric and '_' are always valid UTF-8");
        match text {
            "fn" => Token::Fn,
            "let" => Token::Let,
            "mut" => Token::Mut,
            "if" => Token::If,
            "else" => Token::Else,
            "loop" => Token::Loop,
            "for" => Token::For,
            "parallel" => Token::Parallel,
            "match" => Token::Match,
            "return" => Token::Return,
            "true" => Token::BoolLit(true),
            "false" => Token::BoolLit(false),
            "_" => Token::Underscore,
            _ => Token::Ident(text.to_string()),
        }
    }

    ///Scans an integer or float literal. The first digit was already
    ///consumed by 'advance()' in 'next_token'
    fn scan_number(
        &mut self,
        start_line: u32,
        start_col: u32,
        start_offset: usize,
    ) -> Result<Token, ParseError> {
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() { self.advance(); } else { break; }
        }

        let mut is_float = false;
        if self.peek() == Some(b'.') && self.peek_at(1).map_or(false, |b| b.is_ascii_digit()) {
            is_float = true;
            self.advance();
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() { self.advance(); } else { break; }
            }
        }

        let text = std::str::from_utf8(&self.source[start_offset..self.pos])
            .expect("digits and '.' are always valid ASCII/UTF-8");

        if is_float {
            text.parse::<f64>().map(Token::FloatLit).map_err(|_| ParseError {
                span: self.make_span(start_line, start_col, start_offset),
                message: format!("invalid float literal '{}'", text),
            })
        } else {
            text.parse::<i64>().map(Token::IntLit).map_err(|_| ParseError {
                span: self.make_span(start_line, start_col, start_offset),
                message: format!("integer literal '{}' out of range", text),
            })
        }
    }

    /// Scans a string literal. The opening '"' was already consumed by
    /// `advance()` in `next_token`. Decodes escapes as it goes, rather
    /// than storing the raw source bytes, so `\n` in the literal becomes
    /// an actual newline byte in the resulting Token, not two characters.
    fn scan_string(
        &mut self,
        start_line: u32,
        start_col: u32,
        start_offset: usize,
    ) -> Result<Token, ParseError> {
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            match self.advance() {
                None => {
                    return Err(ParseError {
                        span: self.make_span(start_line, start_col, start_offset),
                        message: "unterminated string literal".to_string(),
                    });
                }
                Some(b'"') => break,
                Some(b'\n') => {
                    return Err(ParseError {
                        span: self.make_span(start_line, start_col, start_offset),
                        message: "unterminated string literal (raw newline — use \\n)".to_string(),
                    });
                }
                Some(b'\\') => {
                    let escaped = self.advance().ok_or_else(|| ParseError {
                        span: self.make_span(start_line, start_col, start_offset),
                        message: "unterminated escape sequence in string literal".to_string(),
                    })?;
                    bytes.push(match escaped {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'0' => 0u8,
                        b'\\' => b'\\',
                        b'"' => b'"',
                        other => {
                            return Err(ParseError {
                                span: self.make_span(start_line, start_col, start_offset),
                                message: format!("unknown escape sequence '\\{}'", other as char),
                            });
                        }
                    });
                }
                Some(b) => bytes.push(b),
            }
        }
        let value = String::from_utf8(bytes).map_err(|_| ParseError {
            span: self.make_span(start_line, start_col, start_offset),
            message: "string literal is not valid UTF-8".to_string(),
        })?;
        Ok(Token::StringLit(value))
    }

        /// Builds the "unterminated character literal" error. A method, not a
    /// stored closure — a closure captured by reference and bound to a
    /// `let` stays borrowed for its whole lifetime as a value (until its
    /// last use), which would overlap with the several `self.advance()`
    /// mutable borrows in between. A method call only borrows `self` for
    /// the instant it runs, so building a fresh one inline at each call
    /// site (as below) never overlaps with anything.
    fn unterminated_char(&self, start_line: u32, start_col: u32, start_offset: usize) -> ParseError {
        ParseError {
            span: self.make_span(start_line, start_col, start_offset),
            message: "unterminated character literal".to_string(),
        }
    }

    fn scan_char(
        &mut self,
        start_line: u32,
        start_col: u32,
        start_offset: usize,
    ) -> Result<Token, ParseError> {
        let ch = match self.advance() {
            None => return Err(self.unterminated_char(start_line, start_col, start_offset)),
            Some(b'\'') => {
                return Err(ParseError {
                    span: self.make_span(start_line, start_col, start_offset),
                    message: "empty character literal".to_string(),
                });
            }
            Some(b'\\') => {
                let escaped = self
                    .advance()
                    .ok_or_else(|| self.unterminated_char(start_line, start_col, start_offset))?;
                match escaped {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'0' => '\0',
                    b'\\' => '\\',
                    b'\'' => '\'',
                    other => {
                        return Err(ParseError {
                            span: self.make_span(start_line, start_col, start_offset),
                            message: format!("unknown escape sequence '\\{}'", other as char),
                        });
                    }
                }
            }
            Some(first_byte) => {
                let extra = utf8_extra_bytes(first_byte).ok_or_else(|| ParseError {
                    span: self.make_span(start_line, start_col, start_offset),
                    message: "invalid UTF-8 lead byte in character literal".to_string(),
                })?;
                let mut buf = vec![first_byte];
                for _ in 0..extra {
                    buf.push(
                        self.advance()
                            .ok_or_else(|| self.unterminated_char(start_line, start_col, start_offset))?,
                    );
                }
                std::str::from_utf8(&buf)
                    .ok()
                    .and_then(|s| s.chars().next())
                    .ok_or_else(|| ParseError {
                        span: self.make_span(start_line, start_col, start_offset),
                        message: "invalid UTF-8 in character literal".to_string(),
                    })?
            }
        };

        match self.advance() {
            Some(b'\'') => Ok(Token::CharLit(ch)),
            _ => Err(ParseError {
                span: self.make_span(start_line, start_col, start_offset),
                message: "character literal must contain exactly one character".to_string(),
            }),
        }
    }

    ///Produces the next token, or none at the end of input: One dispatch on
    ///the current byte, with one-byte lookahead for the two-character operators
    fn next_token(&mut self) -> Result<Option<(Token, SourceSpan)>, ParseError> {
        self.skip_whitespace_and_comments();

        let (start_line, start_col, start_offset) = (self.line, self.col, self.pos);
        let Some(b) = self.advance() else {
            return Ok(None);
        };

        let kind = match b {
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b'{' => Token::LBrace,
            b'}' => Token::RBrace,
            b'[' => Token::LBracket,
            b']' => Token::RBracket,
            b',' => Token::Comma,
            b';' => Token::Semicolon,
            b':' => Token::Colon,
            b'+' => Token::Plus,
            b'*' => Token::Star,
            b'/' => Token::Slash,
            b'%' => Token::Percent,
            b'@' => Token::At,

            b'-' => {
                if self.peek() == Some(b'>') {
                    self.advance();
                    Token::Arrow
                } else {
                    Token::Minus
                }
            }
            b'=' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    Token::EqEq
                } else if self.peek() == Some(b'>') {
                    self.advance();
                    Token::FatArrow
                } else {
                    Token::Eq
                }
            }
            b'!' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    Token::NotEq
                } else {
                    Token::Not
                }
            }
            b'<' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    Token::LtEq
                } else {
                    Token::Lt
                }
            }
            b'>' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    Token::GtEq
                } else {
                    Token::Gt
                }
            }
            b'&' => {
                if self.peek() == Some(b'&') {
                    self.advance();
                    Token::AndAnd
                } else {
                    Token::Amp
                }
            }
            b'|' if self.peek() == Some(b'|') => {
                self.advance();
                Token::OrOr
            }
            b'.' if self.peek() == Some(b'.') => {
                self.advance();
                Token::DotDot
            }

            b'"' => self.scan_string(start_line, start_col, start_offset)?,
            b'\'' => self.scan_char(start_line, start_col, start_offset)?,
            b'0'..=b'9' => self.scan_number(start_line, start_col, start_offset)?,
            c if c.is_ascii_alphabetic() || c == b'_' => {
                self.scan_identifier_or_keyword(start_offset)
            }

            other => {
                return Err(ParseError {
                    span: self.make_span(start_line, start_col, start_offset),
                    message: format!("unexpected character '{}'", other as char),
                });
            }
        };

        Ok(Some((
            kind,
            self.make_span(start_line, start_col, start_offset),
        )))
    }
}

const BIDI_CONTROL_CHARS: &[char] = &[
    '\u{202A}', '\u{202B}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
    '\u{200F}',
];

pub fn contains_bidi_control(source: &str) -> Option<usize> {
    source
        .char_indices()
        .find(|(_, c)| BIDI_CONTROL_CHARS.contains(c))
        .map(|(i, _)| i)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    //Literals
    Ident(String),
    IntLit(i64),
    FloatLit(f64),
    StringLit(String),
    CharLit(char),
    BoolLit(bool),

    //Keywords
    Fn,
    Let,
    Mut,
    If,
    Else,
    Loop,
    For,
    Parallel,
    Match,
    Return,

    //Punctuation/Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semicolon,
    Colon,
    Arrow,
    FatArrow,
    At,
    Underscore,

    //Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    AndAnd,
    OrOr,
    Not,
    Amp,
    DotDot,

    Eof,
}

pub fn tokenize(
    source: &str,
    file_hash: u64,
) -> Result<Vec<(Token, SourceSpan)>, crate::ParseError> {
    let mut lexer = Lexer::new(source, file_hash);
    let mut tokens = Vec::new();
    while let Some(tok) = lexer.next_token()? {
        tokens.push(tok);
    }
    let eof_span = lexer.make_span(lexer.line, lexer.col, lexer.pos);
    tokens.push((Token::Eof, eof_span));
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bidi_override() {
        let src = "let x = 1;\u{202E}// hidden";
        assert!(contains_bidi_control(src).is_some());
    }

    #[test]
    fn accepts_clean_source() {
        let src = "let x = 1;";
        assert!(contains_bidi_control(src).is_none());
    }

    #[test]
    fn tokenize_basic_punctuation() {
        let src = "( ) { } -> => == != && || ..";
        let tokens: Vec<Token> = tokenize(src, 0)
            .expect("Should tokenize cleanly")
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::RParen,
                Token::LBrace,
                Token::RBrace,
                Token::Arrow,
                Token::FatArrow,
                Token::EqEq,
                Token::NotEq,
                Token::AndAnd,
                Token::OrOr,
                Token::DotDot,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn tokenizes_identifiers_keywords_and_numbers() {
        let src = "fn main mut count 42 3.14 0..10 _";
        let tokens: Vec<Token> = tokenize(src, 0)
            .expect("should tokenize cleanly")
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(tokens, vec![
            Token::Fn,
            Token::Ident("main".to_string()),
            Token::Mut,
            Token::Ident("count".to_string()),
            Token::IntLit(42),
            Token::FloatLit(3.14),
            Token::IntLit(0),
            Token::DotDot,
            Token::IntLit(10),
            Token::Underscore,
            Token::Eof,
        ]);
    }

    #[test]
    fn tokenizes_strings_and_bools() {
        let src = r#"let s = "hello\nworld"; let b = true; let c = false;"#;
        let tokens: Vec<Token> = tokenize(src, 0)
            .expect("should tokenize cleanly")
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(tokens, vec![
            Token::Let, Token::Ident("s".to_string()), Token::Eq,
            Token::StringLit("hello\nworld".to_string()), Token::Semicolon,
            Token::Let, Token::Ident("b".to_string()), Token::Eq,
            Token::BoolLit(true), Token::Semicolon,
            Token::Let, Token::Ident("c".to_string()), Token::Eq,
            Token::BoolLit(false), Token::Semicolon,
            Token::Eof,
        ]);
    }

    #[test]
    fn unterminated_string_is_an_error() {
        let src = r#"let s = "oops"#;
        assert!(tokenize(src, 0).is_err());
    }

    #[test]
    fn tokenizes_char_literals() {
        let src = "'a' '\\n' '\\'' 'é'";
        let tokens: Vec<Token> = tokenize(src, 0)
            .expect("should tokenize cleanly")
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(tokens, vec![
            Token::CharLit('a'),
            Token::CharLit('\n'),
            Token::CharLit('\''),
            Token::CharLit('é'),
            Token::Eof,
        ]);
    }

    #[test]
    fn rejects_malformed_char_literals() {
        assert!(tokenize("''", 0).is_err());   // empty
        assert!(tokenize("'ab'", 0).is_err()); // more than one character
        assert!(tokenize("'a", 0).is_err());   // unterminated
    }
}
