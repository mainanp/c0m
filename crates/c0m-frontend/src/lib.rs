//! Increment 1 — The Front End.
//!
//! Reads Suda source and produces the AST: the only representation
//! that exists anywhere in the C0m pipeline. No IR follows this stage.
//!
//! This crate owns:
//!   - Lexing (with hard-error Unicode bidi control character rejection,
//!     per the Trojan Source mitigation in the blueprint)
//!   - Recursive-descent parsing into an arena-allocated AST
//!   - SourceSpan byte-offset tracking (feedback loop identity)

pub mod arena;
pub mod ast;
pub mod dump;
pub mod lexer;
pub mod parser;
pub mod span;

pub use arena::Arena;
pub use ast::{AstNode, NodeId, NodeKind, Payload};
pub use span::SourceSpan;

/// Top-level entry point for increment 1: source text in, AST root out.
/// Everything downstream (increments 2-5) consumes only this AST.
pub fn parse_source(source: &str, file_hash: u64) -> Result<(Arena, NodeId), ParseError> {
    let tokens = lexer::tokenize(source, file_hash)?;
    let mut arena = Arena::new();
    let root = parser::parse(&tokens, &mut arena)?;
    Ok((arena, root))
}

#[derive(Debug)]
pub struct ParseError {
    pub span: SourceSpan,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_to_end_minimal_program() {
        let src = "fn main() -> () { let x: i64 = 1 + 2; }";
        let (arena, root) = parse_source(src, 0).expect("should parse end-to-end");
        let program = arena.get(root);
        assert_eq!(program.kind, NodeKind::Program);
        assert_eq!(program.children.len(), 1);
    }

    #[test]
    fn end_to_end_rejects_bidi_override() {
        let src = "fn main() -> () { let x: i64 = 1;\u{202E}// hidden }";
        assert!(parse_source(src, 0).is_err());
    }

    #[test]
    fn end_to_end_propagates_lexer_error() {
        // Unterminated string — must surface through the FULL pipeline,
        // not just in an isolated lexer::tokenize call.
        let src = r#"fn main() -> () { let x: string = "oops; }"#;
        assert!(parse_source(src, 0).is_err());
    }

    #[test]
    fn end_to_end_propagates_parser_error() {
        // Valid tokens, invalid grammar — same idea, one layer up.
        let src = "fn main() -> () { 1 + 1 = 5; }";
        assert!(parse_source(src, 0).is_err());
    }

    #[test]
    fn every_node_gets_a_unique_key() {
        // The old XOR hash failed this: `2 + 3` and its left `2` shared a
        // key, and Program/FuncDef covered identical bytes.
        let src = "fn f(x: i64) -> i64 { let y: i64 = 2 + 3 * x; return y; }";
        let (arena, _root) = parse_source(src, 0).expect("should parse");

        let mut seen = std::collections::HashSet::new();
        for i in 0..arena.len() {
            let key = arena.get(NodeId(i as u32)).key();
            // HashSet::insert returns false if the value was already there.
            assert!(seen.insert(key), "duplicate key at node index {i}");
        }
    }
}
