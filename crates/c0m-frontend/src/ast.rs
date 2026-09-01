use crate::span::SourceSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    IntLit,
    FloatLit,
    BinaryAdd,
    BinarySub,
    BinaryMul,
    BinaryDiv,
    Compare,
    If,
    Loop,
    Call,
    FuncDef,
    Assign,
    Block,
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

pub struct AstNode {
    pub kind: NodeKind,
    pub span: SourceSpan,
    pub weight: WeightRec,
    pub children: Vec<NodeId>,
    pub loop_bound_hint: Option<u32>,
}

impl AstNode {
    pub fn new(kind: NodeKind, span: SourceSpan) -> Self {
        Self {
            kind,
            span,
            weight: WeightRec::default(),
            children: Vec::new(),
            loop_bound_hint: None,
        }
    }
}
