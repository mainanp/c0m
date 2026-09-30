use crate::span::SourceSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    // literals & names
    IntLit,
    FloatLit,
    StringLit,
    CharLit,
    BoolLit,
    Ident,

    // binary ops — one kind per concrete operator, matching the existing
    // BinaryAdd/BinarySub/BinaryMul/BinaryDiv convention rather than a
    // single "BinaryOp" kind with a tag
    BinaryAdd,
    BinarySub,
    BinaryMul,
    BinaryDiv,
    BinaryMod,
    CmpLt,
    CmpLtEq,
    CmpGt,
    CmpGtEq,
    CmpEq,
    CmpNotEq,
    LogicalAnd,
    LogicalOr,

    // unary ops
    Neg,
    Not,
    Ref,
    RefMut,
    Deref,

    // control flow
    If,
    Loop,         // bounded `for` (and `parallel for`, see ParallelLoop)
    ParallelLoop,
    Match,
    MatchArm,
    Wildcard,     // the `_` pattern

    // calls & statements
    Call,
    FuncDef,
    Param,
    ParamList,
    ReturnType,
    Let,
    Assign,
    Return,
    Block,
    Program,

    // types
    TypeName,     // primitive/named type, e.g. i64
    TypeRef,      // &T
    TypeRefMut,   // &mut T
    // extend as the Suda grammar (Chapter 3.5.1) is formalized
}

/// Zeroed at alloc time; populated by c0m-weighting (increment 2) and
/// consulted by c0m-dos (increment 3). Mirrors W_raw / W_learned /
/// W_combined / dos_tier from the formal weight formula.
#[derive(Debug, Clone, Copy, Default)]
pub struct WeightRec {
    pub raw: f32,
    pub learned: f32,
    pub combined: f32,
    pub dos_tier: u8,
}

/// The Rust equivalent of the blueprint's C `union { i64 int_val; f64
/// flt_val; StrId sym; ... } payload`. An enum instead of a union because
/// it self-tags — there's no way to read the wrong variant by accident,
/// unlike a C union where nothing tracks which field was last written.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Bool(bool),
    Ident(String),
}

pub struct AstNode {
    pub kind: NodeKind,
    pub span: SourceSpan,
    pub weight: WeightRec,
    pub children: Vec<NodeId>,
    pub loop_bound_hint: Option<u32>,
    pub payload: Option<Payload>,

    ///Set only when the parser saw an explicit '@N' annotation directly
    ///on this node's statement. None means no override - let the pattern Analyzer
    ///compute the normal way
    pub tier_override: Option<u8>,

    ///Only menaingful when 'kind == NodeKind::Block'
    pub has_tail: bool,
}

impl AstNode {
    pub fn new(kind: NodeKind, span: SourceSpan) -> Self {
        Self {
            kind,
            span,
            weight: WeightRec::default(),
            children: Vec::new(),
            loop_bound_hint: None,
            payload: None,
            tier_override: None,
            has_tail: false,
        }
    }
}
