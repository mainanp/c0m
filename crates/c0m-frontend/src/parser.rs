//! Recursive-descent parser: tokens -> arena-allocated AST.
//! Grammar to be fixed per Chapter 3.5.1 (The Suda Language Definition).

use crate::arena::Arena;
use crate::ast::NodeId;
use crate::lexer::Token;
use crate::ParseError;

pub fn parse(_tokens: &[(Token, SourceSpan)], _arena: &mut Arena) -> Result<NodeId, ParseError> {
    todo!("increment 1: recursive-descent parse per the Suda grammar")
}
