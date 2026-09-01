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
pub mod lexer;
pub mod parser;
pub mod span;

pub use arena::Arena;
pub use ast::{AstNode, NodeId, NodeKind};
pub use span::SourceSpan;

/// Top-level entry point for increment 1: source text in, AST root out.
/// Everything downstream (increments 2-5) consumes only this AST.
pub fn parse_source(_source: &str, _file_hash: u64) -> Result<(Arena, NodeId), ParseError> {
    todo!("wire lexer::tokenize -> parser::parse once the grammar is defined")
}

#[derive(Debug)]
pub struct ParseError {
    pub span: SourceSpan,
    pub message: String,
}
