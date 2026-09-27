//! Recursive-descent parser: tokens -> arena-allocated AST.
//! Grammar to be fixed per Chapter 3.5.1 (The Suda Language Definition).
//!
//! One function per grammar rule. Each function consumes tokens off the
//! front of the token slice and returns the NodeId of whatever it built
//! in the Arena — see /projects/.../overview for the full EBNF this
//! mirrors.

use crate::arena::Arena;
use crate::ast::{AstNode, NodeId, NodeKind, Payload};
use crate::lexer::Token;
use crate::span::SourceSpan;
use crate::ParseError;

pub struct Parser<'t, 'a> {
    tokens: &'t [(Token, SourceSpan)],
    pos: usize,
    arena: &'a mut Arena,
}

impl<'t, 'a> Parser<'t, 'a> {
    pub fn new(tokens: &'t [(Token, SourceSpan)], arena: &'a mut Arena) -> Self {
        Parser { tokens, pos: 0, arena }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].0
    }

    fn peek_span(&self) -> SourceSpan {
        self.tokens[self.pos].1
    }

    fn advance(&mut self) -> (Token, SourceSpan) {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, want: &Token) -> Result<SourceSpan, ParseError> {
        if self.peek() == want {
            let (_, span) = self.advance();
            Ok(span)
        } else {
            Err(ParseError {
                span: self.peek_span(),
                message: format!("expected {:?}, found {:?}", want, self.peek()),
            })
        }
    }

    /// Combines a starting span and an ending span into one span covering
    /// the whole range between them. Needed anywhere a node represents
    /// more than one token — `1 + 2` should carry a span from the start
    /// of `1` to the end of `2`, not just `1`'s single-token span.
    fn cover(&self, start: SourceSpan, end: SourceSpan) -> SourceSpan {
        SourceSpan {
            file_hash: start.file_hash,
            line: start.line,
            col: start.col,
            byte_offset: start.byte_offset,
            length: (end.byte_offset + end.length) - start.byte_offset,
        }
    }

    fn push_node(&mut self, kind: NodeKind, span: SourceSpan, payload: Option<Payload>) -> NodeId {
        let mut node = AstNode::new(kind, span);
        node.payload = payload;
        self.arena.alloc(node)
    }

    fn push_node_with_children(
        &mut self,
        kind: NodeKind,
        span: SourceSpan,
        children: Vec<NodeId>,
    ) -> NodeId {
        let mut node = AstNode::new(kind, span);
        node.children = children;
        self.arena.alloc(node)
    }

    // ── Expressions, bottom of the precedence chain first ──────────

    /// Primary ::= IntLiteral | FloatLiteral | StringLiteral | CharLiteral
    ///           | BoolLiteral | Identifier | "(" Expression ")"
    ///           | IfExpr | MatchExpr ;
    /// (IfExpr/MatchExpr join once Block exists, next round.)
    fn parse_primary(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek_span();
        match self.peek().clone() {
            Token::IntLit(v) => {
                self.advance();
                Ok(self.push_node(NodeKind::IntLit, start, Some(Payload::Int(v))))
            }
            Token::FloatLit(v) => {
                self.advance();
                Ok(self.push_node(NodeKind::FloatLit, start, Some(Payload::Float(v))))
            }
            Token::StringLit(s) => {
                self.advance();
                Ok(self.push_node(NodeKind::StringLit, start, Some(Payload::Str(s))))
            }
            Token::CharLit(c) => {
                self.advance();
                Ok(self.push_node(NodeKind::CharLit, start, Some(Payload::Char(c))))
            }
            Token::BoolLit(b) => {
                self.advance();
                Ok(self.push_node(NodeKind::BoolLit, start, Some(Payload::Bool(b))))
            }
            Token::Ident(name) => {
                self.advance();
                Ok(self.push_node(NodeKind::Ident, start, Some(Payload::Ident(name))))
            }
            Token::LParen => {
                self.advance();
                let inner = self.parse_expression()?;
                self.expect(&Token::RParen)?;
                Ok(inner) // grouping parens leave no trace in the AST
            }
            other => Err(ParseError {
                span: start,
                message: format!("expected an expression, found {:?}", other),
            }),
        }
    }

    /// Call ::= Primary { "(" [ ArgList ] ")" } ;
    /// ArgList ::= Expression { "," Expression } ;
    fn parse_call(&mut self) -> Result<NodeId, ParseError> {
        let callee = self.parse_primary()?;
        let start_span = self.arena.get(callee).span;

        if *self.peek() != Token::LParen {
            return Ok(callee);
        }
        self.advance();

        let mut children = vec![callee];
        if *self.peek() != Token::RParen {
            children.push(self.parse_expression()?);
            while *self.peek() == Token::Comma {
                self.advance();
                children.push(self.parse_expression()?);
            }
        }
        let close_span = self.expect(&Token::RParen)?;
        let span = self.cover(start_span, close_span);
        Ok(self.push_node_with_children(NodeKind::Call, span, children))
    }

    /// Unary ::= ( "-" | "!" | "&" [ "mut" ] ) Unary | Call ;
    fn parse_unary(&mut self) -> Result<NodeId, ParseError> {
        match self.peek().clone() {
            Token::Minus => {
                let (_, op_span) = self.advance();
                let operand = self.parse_unary()?; // recurse: handles --x too
                let span = self.cover(op_span, self.arena.get(operand).span);
                Ok(self.push_node_with_children(NodeKind::Neg, span, vec![operand]))
            }
            Token::Not => {
                let (_, op_span) = self.advance();
                let operand = self.parse_unary()?;
                let span = self.cover(op_span, self.arena.get(operand).span);
                Ok(self.push_node_with_children(NodeKind::Not, span, vec![operand]))
            }
            Token::Amp => {
                let (_, op_span) = self.advance();
                let is_mut = *self.peek() == Token::Mut;
                if is_mut {
                    self.advance();
                }
                let operand = self.parse_unary()?;
                let span = self.cover(op_span, self.arena.get(operand).span);
                let kind = if is_mut { NodeKind::RefMut } else { NodeKind::Ref };
                Ok(self.push_node_with_children(kind, span, vec![operand]))
            }
            _ => self.parse_call(),
        }
    }

    /// Multiplicative ::= Unary { ( "*" | "/" | "%" ) Unary } ;
    fn parse_multiplicative(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let kind = match self.peek() {
                Token::Star => NodeKind::BinaryMul,
                Token::Slash => NodeKind::BinaryDiv,
                Token::Percent => NodeKind::BinaryMod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            let span = self.cover(self.arena.get(left).span, self.arena.get(right).span);
            left = self.push_node_with_children(kind, span, vec![left, right]);
        }
        Ok(left)
    }

    /// Additive ::= Multiplicative { ( "+" | "-" ) Multiplicative } ;
    fn parse_additive(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let kind = match self.peek() {
                Token::Plus => NodeKind::BinaryAdd,
                Token::Minus => NodeKind::BinarySub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            let span = self.cover(self.arena.get(left).span, self.arena.get(right).span);
            left = self.push_node_with_children(kind, span, vec![left, right]);
        }
        Ok(left)
    }

    /// Comparison ::= Additive { ( "<" | "<=" | ">" | ">=" ) Additive } ;
    fn parse_comparison(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_additive()?;
        loop {
            let kind = match self.peek() {
                Token::Lt => NodeKind::CmpLt,
                Token::LtEq => NodeKind::CmpLtEq,
                Token::Gt => NodeKind::CmpGt,
                Token::GtEq => NodeKind::CmpGtEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_additive()?;
            let span = self.cover(self.arena.get(left).span, self.arena.get(right).span);
            left = self.push_node_with_children(kind, span, vec![left, right]);
        }
        Ok(left)
    }

    /// Equality ::= Comparison { ( "==" | "!=" ) Comparison } ;
    fn parse_equality(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_comparison()?;
        loop {
            let kind = match self.peek() {
                Token::EqEq => NodeKind::CmpEq,
                Token::NotEq => NodeKind::CmpNotEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_comparison()?;
            let span = self.cover(self.arena.get(left).span, self.arena.get(right).span);
            left = self.push_node_with_children(kind, span, vec![left, right]);
        }
        Ok(left)
    }

    /// LogicalAnd ::= Equality { "&&" Equality } ;
    fn parse_logical_and(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_equality()?;
        while *self.peek() == Token::AndAnd {
            self.advance();
            let right = self.parse_equality()?;
            let span = self.cover(self.arena.get(left).span, self.arena.get(right).span);
            left = self.push_node_with_children(NodeKind::LogicalAnd, span, vec![left, right]);
        }
        Ok(left)
    }

    /// LogicalOr ::= LogicalAnd { "||" LogicalAnd } ;
    fn parse_logical_or(&mut self) -> Result<NodeId, ParseError> {
        let mut left = self.parse_logical_and()?;
        while *self.peek() == Token::OrOr {
            self.advance();
            let right = self.parse_logical_and()?;
            let span = self.cover(self.arena.get(left).span, self.arena.get(right).span);
            left = self.push_node_with_children(NodeKind::LogicalOr, span, vec![left, right]);
        }
        Ok(left)
    }

    /// Expression ::= LogicalOr ;
    fn parse_expression(&mut self) -> Result<NodeId, ParseError> {
        self.parse_logical_or()
    }
}

/// TEMPORARY entry point — exercises only the expression grammar so far.
/// Once Statement/FunctionDecl/Program exist (next rounds), this becomes
/// `parser.parse_program()` instead.
pub fn parse(tokens: &[(Token, SourceSpan)], arena: &mut Arena) -> Result<NodeId, ParseError> {
    let mut parser = Parser::new(tokens, arena);
    parser.parse_expression()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn parse_expr(src: &str) -> (Arena, NodeId) {
        let tokens = tokenize(src, 0).expect("should tokenize cleanly");
        let mut arena = Arena::new();
        let root = parse(&tokens, &mut arena).expect("should parse cleanly");
        (arena, root)
    }

    #[test]
    fn parses_int_literal() {
        let (arena, root) = parse_expr("42");
        let node = arena.get(root);
        assert_eq!(node.kind, NodeKind::IntLit);
        assert_eq!(node.payload, Some(Payload::Int(42)));
    }

    #[test]
    fn parses_precedence_correctly() {
        // 2 + 3 * 4 must parse as 2 + (3 * 4)
        let (arena, root) = parse_expr("2 + 3 * 4");
        let root_node = arena.get(root);
        assert_eq!(root_node.kind, NodeKind::BinaryAdd);
        assert_eq!(root_node.children.len(), 2);
        assert_eq!(arena.get(root_node.children[0]).kind, NodeKind::IntLit);
        assert_eq!(arena.get(root_node.children[1]).kind, NodeKind::BinaryMul);
    }

    #[test]
    fn parses_parenthesized_expression() {
        // (2 + 3) * 4 must parse with BinaryMul at the root this time
        let (arena, root) = parse_expr("(2 + 3) * 4");
        let root_node = arena.get(root);
        assert_eq!(root_node.kind, NodeKind::BinaryMul);
        assert_eq!(arena.get(root_node.children[0]).kind, NodeKind::BinaryAdd);
    }

    #[test]
    fn parses_unary_negation() {
        let (arena, root) = parse_expr("-5");
        let node = arena.get(root);
        assert_eq!(node.kind, NodeKind::Neg);
        assert_eq!(arena.get(node.children[0]).payload, Some(Payload::Int(5)));
    }

    #[test]
    fn parses_function_call() {
        let (arena, root) = parse_expr("foo(1, 2)");
        let node = arena.get(root);
        assert_eq!(node.kind, NodeKind::Call);
        assert_eq!(node.children.len(), 3); // callee + 2 args
    }

    #[test]
    fn call_binds_tighter_than_addition() {
        let (arena, root) = parse_expr("foo() + 1");
        let root_node = arena.get(root);
        assert_eq!(root_node.kind, NodeKind::BinaryAdd);
        assert_eq!(arena.get(root_node.children[0]).kind, NodeKind::Call);
    }
}


