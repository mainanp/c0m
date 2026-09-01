//! Increment 4 — The Direct Emitter (+ Register Oracle).
//!
//! Recursive walk: "emit code that computes this node value, leave
//! the result in the returned register." Also owns forward-jump patch
//! resolution and the --trace-emit traceability records (instruction
//! offset -> AST node span) — the concrete evidence for the study
//! auditability claim.

pub mod oracle;

use c0m_frontend::{Arena, NodeId};

pub struct EmitCtx {
    pub output: String,
    pub trace: Vec<(usize, u64)>,
}

impl EmitCtx {
    pub fn new() -> Self {
        Self { output: String::new(), trace: Vec::new() }
    }
}

impl Default for EmitCtx {
    fn default() -> Self { Self::new() }
}

pub fn emit(_arena: &Arena, _node: NodeId, _ctx: &mut EmitCtx) {
    todo!("increment 4: port emit() from the Chapter 3 pseudocode")
}
