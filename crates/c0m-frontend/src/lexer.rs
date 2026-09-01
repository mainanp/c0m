//! Lexical analysis: byte stream -> tokens.
//!
//! Per Chapter 3 (Trojan Source mitigation): Unicode bidirectional
//! control characters (U+202A, U+202B, U+202D, U+202E, U+2066-U+2069,
//! U+200F) outside string literal bytes are hard lexer errors, not
//! ignored. This is intentionally checked before any other tokenization.

const BIDI_CONTROL_CHARS: &[char] = &[
    '\u{202A}', '\u{202B}', '\u{202D}', '\u{202E}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{200F}',
];

pub fn contains_bidi_control(source: &str) -> Option<usize> {
    source.char_indices()
        .find(|(_, c)| BIDI_CONTROL_CHARS.contains(c))
        .map(|(i, _)| i)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    IntLit(i64),
    FloatLit(f64),
    Plus, Minus, Star, Slash,
    Eq, EqEq, Lt, Gt,
    LParen, RParen, LBrace, RBrace,
    If, Else, Loop, Fn, Let,
    Eof,
}

pub fn tokenize(_source: &str) -> Result<Vec<Token>, crate::ParseError> {
    todo!("increment 1: implement the finite state machine from the Ch4 FSM diagram")
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
}
