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

    // ── Core cursor primitives ──────────────────────────────────────

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

    fn push_node_full(
        &mut self,
        kind: NodeKind,
        span: SourceSpan,
        payload: Option<Payload>,
        children: Vec<NodeId>,
    ) -> NodeId {
        let mut node = AstNode::new(kind, span);
        node.payload = payload;
        node.children = children;
        self.arena.alloc(node)
    }

    // ── Expressions, lowest to highest precedence ───────────────────

    /// Primary ::= IntLiteral | FloatLiteral | StringLiteral | CharLiteral
    ///           | BoolLiteral | Identifier | "(" Expression ")"
    ///           | IfExpr | MatchExpr ;
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
            Token::If => self.parse_if_expr(),
            Token::Match => self.parse_match(),
            other => Err(ParseError {
                span: start,
                message: format!("expected an expression, found {:?}", other),
            }),
        }
    }

    /// Call ::= Primary { "(" [ ArgList ] ")" } ;
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

    /// Unary ::= ( "-" | "!" | "&" [ "mut" ] | "*" ) Unary | Call ;
    fn parse_unary(&mut self) -> Result<NodeId, ParseError> {
        match self.peek().clone() {
            Token::Minus => {
                let (_, op_span) = self.advance();
                let operand = self.parse_unary()?;
                let span = self.cover(op_span, self.arena.get(operand).span);
                Ok(self.push_node_with_children(NodeKind::Neg, span, vec![operand]))
            }
            Token::Not => {
                let (_, op_span) = self.advance();
                let operand = self.parse_unary()?;
                let span = self.cover(op_span, self.arena.get(operand).span);
                Ok(self.push_node_with_children(NodeKind::Not, span, vec![operand]))
            }
            Token::Star => {
                let (_, op_span) = self.advance();
                let operand = self.parse_unary()?;
                let span = self.cover(op_span, self.arena.get(operand).span);
                Ok(self.push_node_with_children(NodeKind::Deref, span, vec![operand]))
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

    // ── @tier annotations ────────────────────────────────────────────

    /// TierAnnotation ::= "@" IntLiteral ;
    /// Only captures and validates the number (0-3, matching the
    /// existing internal tier scale) — what it *means* is increment 2's
    /// decision, not this one's.
    fn parse_optional_tier_annotation(&mut self) -> Result<Option<u8>, ParseError> {
        if *self.peek() != Token::At {
            return Ok(None);
        }
        let at_span = self.peek_span();
        self.advance();
        match self.peek().clone() {
            Token::IntLit(v) if (0..=3).contains(&v) => {
                self.advance();
                Ok(Some(v as u8))
            }
            Token::IntLit(v) => Err(ParseError {
                span: at_span,
                message: format!("@tier annotation must be 0-3, found {}", v),
            }),
            other => Err(ParseError {
                span: at_span,
                message: format!("expected an integer after '@', found {:?}", other),
            }),
        }
    }

    // ── Statements ───────────────────────────────────────────────────

    /// Statement ::= [ TierAnnotation ] StatementBody ;
    /// Handles only the unambiguous, keyword-led forms. The
    /// expression/assignment/tail-value case lives in `parse_block`,
    /// since only a block context has to decide "is this the tail?".
    fn parse_statement(&mut self) -> Result<NodeId, ParseError> {
        let tier = self.parse_optional_tier_annotation()?;
        let node_id = match self.peek() {
            Token::Let => self.parse_let_statement()?,
            Token::For => self.parse_for_statement()?,
            Token::Parallel => self.parse_parallel_for_statement()?,
            Token::Match => self.parse_match()?,
            Token::Return => self.parse_return_statement()?,
            Token::If => self.parse_if_expr()?,
            Token::LBrace => self.parse_block()?,
            other => {
                return Err(ParseError {
                    span: self.peek_span(),
                    message: format!("unexpected token at start of statement: {:?}", other),
                });
            }
        };
        if let Some(t) = tier {
            self.arena.get_mut(node_id).tier_override = Some(t);
        }
        Ok(node_id)
    }

    /// LetStatement ::= "let" [ "mut" ] Identifier [ ":" Type ] "=" Expression ";" ;
    /// children: [Type?, value] — length tells you whether a type was given.
    /// `mut` is consumed here but not recorded in the AST shape itself;
    /// mutability tracking belongs to the symbol table increment 2 builds,
    /// not to the tree's structure.
    fn parse_let_statement(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::Let)?;
        if *self.peek() == Token::Mut {
            self.advance();
        }
        let name = match self.peek().clone() {
            Token::Ident(n) => {
                self.advance();
                n
            }
            other => {
                return Err(ParseError {
                    span: self.peek_span(),
                    message: format!("expected variable name, found {:?}", other),
                });
            }
        };
        let mut children = Vec::new();
        if *self.peek() == Token::Colon {
            self.advance();
            children.push(self.parse_type()?);
        }
        self.expect(&Token::Eq)?;
        children.push(self.parse_expression()?);
        let close = self.expect(&Token::Semicolon)?;
        let span = self.cover(start, close);
        Ok(self.push_node_full(NodeKind::Let, span, Some(Payload::Ident(name)), children))
    }

    /// ForStatement ::= "for" Identifier "in" Expression ".." Expression Block ;
    fn parse_for_statement(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::For)?;
        self.parse_for_body(start, NodeKind::Loop)
    }

    /// ParallelForStatement ::= "parallel" "for" Identifier "in" Expression ".." Expression Block ;
    fn parse_parallel_for_statement(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::Parallel)?;
        self.expect(&Token::For)?;
        self.parse_for_body(start, NodeKind::ParallelLoop)
    }

    /// Shared body for `for` and `parallel for` — identical shape, only
    /// the produced NodeKind differs, since the DOS treats them very
    /// differently downstream.
    fn parse_for_body(&mut self, start: SourceSpan, kind: NodeKind) -> Result<NodeId, ParseError> {
        let var_name = match self.peek().clone() {
            Token::Ident(n) => {
                self.advance();
                n
            }
            other => {
                return Err(ParseError {
                    span: self.peek_span(),
                    message: format!("expected loop variable name, found {:?}", other),
                });
            }
        };
        self.expect(&Token::In)?;
        let lo = self.parse_expression()?;
        self.expect(&Token::DotDot)?;
        let hi = self.parse_expression()?;
        let body = self.parse_block()?;
        let span = self.cover(start, self.arena.get(body).span);

        let mut node = AstNode::new(kind, span);
        node.payload = Some(Payload::Ident(var_name));
        node.children = vec![lo, hi, body];

        // Stage 1C of the architecture blueprint: static loop-bound
        // extraction. Only possible here, at parse time, and only when
        // both bounds are literal integers -- a variable or function-call
        // bound isn't known until runtime, and loop_bound_hint stays None.
        if let (Some(Payload::Int(lo_v)), Some(Payload::Int(hi_v))) =
            (&self.arena.get(lo).payload, &self.arena.get(hi).payload)
        {
            if *hi_v > *lo_v {
                node.loop_bound_hint = Some((*hi_v - *lo_v) as u32);
            }
        }

        Ok(self.arena.alloc(node))
    }

    /// MatchStatement ::= "match" Expression "{" { MatchArm } "}" ;
    /// MatchExpr       ::= "match" Expression "{" { Pattern "=>" Expression "," } "}" ;
    /// One function for both: an arm's body can be a Block (which may or
    /// may not carry a tail value) or a bare Expression either way, so
    /// there's nothing left that actually differs between the statement
    /// and expression forms — same lesson as grouping parens needing no
    /// wrapper node, just applied one level up.
    fn parse_match(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::Match)?;
        let scrutinee = self.parse_expression()?;
        self.expect(&Token::LBrace)?;
        let mut arms = Vec::new();
        while *self.peek() != Token::RBrace {
            arms.push(self.parse_match_arm()?);
        }
        let close = self.expect(&Token::RBrace)?;
        let span = self.cover(start, close);
        let mut children = vec![scrutinee];
        children.extend(arms);
        Ok(self.push_node_with_children(NodeKind::Match, span, children))
    }

    /// MatchArm ::= Pattern "=>" ( Expression | Block ) "," ;
    fn parse_match_arm(&mut self) -> Result<NodeId, ParseError> {
        let pattern = self.parse_pattern()?;
        self.expect(&Token::FatArrow)?;
        let body = if *self.peek() == Token::LBrace {
            self.parse_block()?
        } else {
            self.parse_expression()?
        };
        let close = self.expect(&Token::Comma)?;
        let span = self.cover(self.arena.get(pattern).span, close);
        Ok(self.push_node_with_children(NodeKind::MatchArm, span, vec![pattern, body]))
    }

    /// Pattern ::= IntLiteral | StringLiteral | Identifier | "_" ;
    fn parse_pattern(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek_span();
        match self.peek().clone() {
            Token::IntLit(v) => {
                self.advance();
                Ok(self.push_node(NodeKind::IntLit, start, Some(Payload::Int(v))))
            }
            Token::StringLit(s) => {
                self.advance();
                Ok(self.push_node(NodeKind::StringLit, start, Some(Payload::Str(s))))
            }
            Token::Ident(n) => {
                self.advance();
                Ok(self.push_node(NodeKind::Ident, start, Some(Payload::Ident(n))))
            }
            Token::Underscore => {
                self.advance();
                Ok(self.push_node(NodeKind::Wildcard, start, None))
            }
            other => Err(ParseError {
                span: start,
                message: format!("expected a pattern, found {:?}", other),
            }),
        }
    }

    /// ReturnStatement ::= "return" [ ExpressionList ] ";" ;
    fn parse_return_statement(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::Return)?;
        let mut children = Vec::new();
        if *self.peek() != Token::Semicolon {
            children.push(self.parse_expression()?);
            while *self.peek() == Token::Comma {
                self.advance();
                children.push(self.parse_expression()?);
            }
        }
        let close = self.expect(&Token::Semicolon)?;
        let span = self.cover(start, close);
        Ok(self.push_node_with_children(NodeKind::Return, span, children))
    }

    /// IfExpr ::= "if" Expression Block [ "else" ( Block | IfExpr ) ] ;
    /// children: [cond, then] or [cond, then, else] — length tells you
    /// whether an else branch exists. `else if` needs no special case at
    /// all: it falls straight out of this function recursing on itself.
    fn parse_if_expr(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::If)?;
        let cond = self.parse_expression()?;
        let then_block = self.parse_block()?;
        let mut children = vec![cond, then_block];
        let mut end_span = self.arena.get(then_block).span;
        if *self.peek() == Token::Else {
            self.advance();
            let else_branch = if *self.peek() == Token::If {
                self.parse_if_expr()?
            } else {
                self.parse_block()?
            };
            end_span = self.arena.get(else_branch).span;
            children.push(else_branch);
        }
        let span = self.cover(start, end_span);
        Ok(self.push_node_with_children(NodeKind::If, span, children))
    }

    /// A parsed expression must be Ident- or Deref-shaped to be a legal
    /// assignment target. Checked structurally after parsing, since
    /// there's no separate LValue grammar path anymore.
    fn check_is_lvalue(&self, node: NodeId) -> Result<(), ParseError> {
        match self.arena.get(node).kind {
            NodeKind::Ident | NodeKind::Deref => Ok(()),
            _ => Err(ParseError {
                span: self.arena.get(node).span,
                message: "left side of '=' must be a variable or a dereference".to_string(),
            }),
        }
    }

    /// Block ::= "{" { Statement } [ Expression ] "}" ;
    /// The one place three different endings for a bare expression have
    /// to be told apart: `;` (discard, keep looping), `=` (it was really
    /// an assignment target), or `}` (it's the block's tail value).
    fn parse_block(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::LBrace)?;
        let mut children = Vec::new();
        let mut has_tail = false;

        while *self.peek() != Token::RBrace {
            let is_block_like = matches!(
                self.peek(),
                Token::Let
                    | Token::For
                    | Token::Parallel
                    | Token::Match
                    | Token::Return
                    | Token::If
                    | Token::LBrace
                    | Token::At
            );

            if is_block_like {
                children.push(self.parse_statement()?);
                continue;
            }

            let expr = self.parse_expression()?;
            if *self.peek() == Token::Eq {
                self.check_is_lvalue(expr)?;
                self.advance();
                let rhs = self.parse_expression()?;
                let close = self.expect(&Token::Semicolon)?;
                let span = self.cover(self.arena.get(expr).span, close);
                children.push(self.push_node_with_children(NodeKind::Assign, span, vec![expr, rhs]));
            } else if *self.peek() == Token::Semicolon {
                self.advance();
                children.push(expr); // bare expression-statement, value discarded
            } else if *self.peek() == Token::RBrace {
                children.push(expr);
                has_tail = true;
                break;
            } else {
                return Err(ParseError {
                    span: self.peek_span(),
                    message: format!("expected ';', '=', or '}}', found {:?}", self.peek()),
                });
            }
        }

        let close = self.expect(&Token::RBrace)?;
        let span = self.cover(start, close);
        let mut node = AstNode::new(NodeKind::Block, span);
        node.children = children;
        node.has_tail = has_tail;
        Ok(self.arena.alloc(node))
    }

    // ── Types, parameters, functions, program ───────────────────────

    /// Type ::= "&" [ "mut" ] Type | PrimitiveType ;
    /// PrimitiveType ::= Identifier ;  (i64/f64/bool/string aren't lexer
    /// keywords, so they arrive as plain Idents — validating that a name
    /// is actually a real type is a semantic check, not this parser's job)
    fn parse_type(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek_span();
        if *self.peek() == Token::Amp {
            self.advance();
            let is_mut = *self.peek() == Token::Mut;
            if is_mut {
                self.advance();
            }
            let inner = self.parse_type()?;
            let span = self.cover(start, self.arena.get(inner).span);
            let kind = if is_mut { NodeKind::TypeRefMut } else { NodeKind::TypeRef };
            return Ok(self.push_node_with_children(kind, span, vec![inner]));
        }
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(self.push_node(NodeKind::TypeName, start, Some(Payload::Ident(name))))
            }
            other => Err(ParseError {
                span: start,
                message: format!("expected a type, found {:?}", other),
            }),
        }
    }

    /// Param ::= Identifier ":" Type ;
    fn parse_param(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek_span();
        let name = match self.peek().clone() {
            Token::Ident(n) => {
                self.advance();
                n
            }
            other => {
                return Err(ParseError {
                    span: start,
                    message: format!("expected parameter name, found {:?}", other),
                });
            }
        };
        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;
        let span = self.cover(start, self.arena.get(ty).span);
        Ok(self.push_node_full(NodeKind::Param, span, Some(Payload::Ident(name)), vec![ty]))
    }

    /// ReturnType ::= Type | "(" Type { "," Type } ")" ;
    /// Always wrapped in a NodeKind::ReturnType node with 0+ Type
    /// children — `()` (zero children) is "returns nothing," a single
    /// child is the ordinary case, 2+ is the Go-style tuple form. Same
    /// child count either way downstream never has to special-case which.
    fn parse_return_type(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek_span();
        if *self.peek() == Token::LParen {
            self.advance();
            let mut types = Vec::new();
            if *self.peek() != Token::RParen {
                types.push(self.parse_type()?);
                while *self.peek() == Token::Comma {
                    self.advance();
                    types.push(self.parse_type()?);
                }
            }
            let close = self.expect(&Token::RParen)?;
            let span = self.cover(start, close);
            Ok(self.push_node_with_children(NodeKind::ReturnType, span, types))
        } else {
            let ty = self.parse_type()?;
            let span = self.arena.get(ty).span;
            Ok(self.push_node_with_children(NodeKind::ReturnType, span, vec![ty]))
        }
    }

    /// FunctionDecl ::= "fn" Identifier "(" [ ParamList ] ")" "->" ReturnType Block ;
    /// children: always exactly [ParamList, ReturnType, Block].
    fn parse_function_decl(&mut self) -> Result<NodeId, ParseError> {
        let start = self.expect(&Token::Fn)?;
        let name = match self.peek().clone() {
            Token::Ident(n) => {
                self.advance();
                n
            }
            other => {
                return Err(ParseError {
                    span: self.peek_span(),
                    message: format!("expected function name, found {:?}", other),
                });
            }
        };
        let params_open = self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        if *self.peek() != Token::RParen {
            params.push(self.parse_param()?);
            while *self.peek() == Token::Comma {
                self.advance();
                params.push(self.parse_param()?);
            }
        }
        let params_close = self.expect(&Token::RParen)?;
        let param_list_span = self.cover(params_open, params_close);
        let param_list = self.push_node_with_children(NodeKind::ParamList, param_list_span, params);

        self.expect(&Token::Arrow)?;
        let return_type = self.parse_return_type()?;
        let body = self.parse_block()?;

        let span = self.cover(start, self.arena.get(body).span);
        Ok(self.push_node_full(
            NodeKind::FuncDef,
            span,
            Some(Payload::Ident(name)),
            vec![param_list, return_type, body],
        ))
    }

    /// Program ::= { FunctionDecl } ;
    fn parse_program(&mut self) -> Result<NodeId, ParseError> {
        let start = self.peek_span();
        let mut funcs = Vec::new();
        while *self.peek() != Token::Eof {
            funcs.push(self.parse_function_decl()?);
        }
        let span = match funcs.last() {
            Some(&last) => self.cover(start, self.arena.get(last).span),
            None => start,
        };
        Ok(self.push_node_with_children(NodeKind::Program, span, funcs))
    }
}

/// Top-level entry point: tokens in, a Program NodeId out.
pub fn parse(tokens: &[(Token, SourceSpan)], arena: &mut Arena) -> Result<NodeId, ParseError> {
    let mut parser = Parser::new(tokens, arena);
    parser.parse_program()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn parse_expr(src: &str) -> (Arena, NodeId) {
        let tokens = tokenize(src, 0).expect("should tokenize cleanly");
        let mut arena = Arena::new();
        let root = {
            let mut parser = Parser::new(&tokens, &mut arena);
            parser.parse_expression().expect("should parse cleanly")
        };
        (arena, root)
    }

    fn parse_program(src: &str) -> (Arena, NodeId) {
        let tokens = tokenize(src, 0).expect("should tokenize cleanly");
        let mut arena = Arena::new();
        let root = parse(&tokens, &mut arena).expect("should parse cleanly");
        (arena, root)
    }

    // ── expression-level tests (unchanged from last round) ──────────

    #[test]
    fn parses_int_literal() {
        let (arena, root) = parse_expr("42");
        let node = arena.get(root);
        assert_eq!(node.kind, NodeKind::IntLit);
        assert_eq!(node.payload, Some(Payload::Int(42)));
    }

    #[test]
    fn parses_precedence_correctly() {
        let (arena, root) = parse_expr("2 + 3 * 4");
        let root_node = arena.get(root);
        assert_eq!(root_node.kind, NodeKind::BinaryAdd);
        assert_eq!(arena.get(root_node.children[1]).kind, NodeKind::BinaryMul);
    }

    #[test]
    fn parses_dereference() {
        let (arena, root) = parse_expr("*p + 1");
        let root_node = arena.get(root);
        assert_eq!(root_node.kind, NodeKind::BinaryAdd);
        assert_eq!(arena.get(root_node.children[0]).kind, NodeKind::Deref);
    }

    // ── new: statements, functions, whole programs ──────────────────

    #[test]
    fn parses_minimal_function() {
        let (arena, root) = parse_program("fn main() -> () { let x: i64 = 1; }");
        let program = arena.get(root);
        assert_eq!(program.kind, NodeKind::Program);
        let func = arena.get(program.children[0]);
        assert_eq!(func.kind, NodeKind::FuncDef);
        assert_eq!(func.payload, Some(Payload::Ident("main".to_string())));
        assert_eq!(func.children.len(), 3); // ParamList, ReturnType, Block
    }

    #[test]
    fn for_loop_gets_static_bound_hint() {
        let (arena, root) = parse_program("fn f() -> () { for i in 0..8 { let x: i64 = i; } }");
        let func = arena.get(arena.get(root).children[0]);
        let block = arena.get(func.children[2]);
        let for_node = arena.get(block.children[0]);
        assert_eq!(for_node.kind, NodeKind::Loop);
        assert_eq!(for_node.loop_bound_hint, Some(8));
    }

    #[test]
    fn tier_annotation_sets_override() {
        let (arena, root) = parse_program("fn f() -> () { @2 for i in 0..4 { } }");
        let func = arena.get(arena.get(root).children[0]);
        let block = arena.get(func.children[2]);
        let for_node = arena.get(block.children[0]);
        assert_eq!(for_node.tier_override, Some(2));
    }

    #[test]
    fn if_expression_feeds_a_let_with_tail_values() {
        let src = "fn f(x: i64) -> i64 { let y: i64 = if x > 0 { 1 } else { -1 }; return y; }";
        let (arena, root) = parse_program(src);
        let func = arena.get(arena.get(root).children[0]);
        let block = arena.get(func.children[2]);
        let let_node = arena.get(block.children[0]);
        assert_eq!(let_node.kind, NodeKind::Let);
        let if_node = arena.get(let_node.children[1]); // [type, value]
        assert_eq!(if_node.kind, NodeKind::If);
        assert_eq!(if_node.children.len(), 3); // cond, then, else
        let then_block = arena.get(if_node.children[1]);
        assert!(then_block.has_tail);
    }

    #[test]
    fn general_match_with_go_style_return() {
        let src = r#"fn f(code: i64) -> (i64, i64) {
            match code {
                1 => { return 1, 0; },
                _ => { return -1, 1; },
            }
        }"#;
        let (arena, root) = parse_program(src);
        let func = arena.get(arena.get(root).children[0]);
        let block = arena.get(func.children[2]);
        let match_node = arena.get(block.children[0]);
        assert_eq!(match_node.kind, NodeKind::Match);
        // children[0] = scrutinee, then one MatchArm per arm
        assert_eq!(match_node.children.len(), 3);
        let arm0 = arena.get(match_node.children[1]);
        assert_eq!(arena.get(arm0.children[0]).kind, NodeKind::IntLit);
        let arm1 = arena.get(match_node.children[2]);
        assert_eq!(arena.get(arm1.children[0]).kind, NodeKind::Wildcard);
    }

    #[test]
    fn assignment_through_deref() {
        let (arena, root) = parse_program("fn f(p: &mut i64) -> () { *p = 5; }");
        let func = arena.get(arena.get(root).children[0]);
        let block = arena.get(func.children[2]);
        let assign = arena.get(block.children[0]);
        assert_eq!(assign.kind, NodeKind::Assign);
        assert_eq!(arena.get(assign.children[0]).kind, NodeKind::Deref);
    }

    #[test]
    fn rejects_bad_assignment_target() {
        let tokens = tokenize("fn f() -> () { 1 + 1 = 5; }", 0).unwrap();
        let mut arena = Arena::new();
        assert!(parse(&tokens, &mut arena).is_err());
    }
}
