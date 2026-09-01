//! Increment 3 — Deferred Optimization Store.
//!
//! Answers "given this node tier and kind, what strategy?" without
//! generating code itself. Strategies outside the current tier stay
//! latent. See Chapter 2.5 (DOS) and the tier table in Chapter 4.

use c0m_frontend::{Arena, NodeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitStrategy {
    Direct,
    LeaChain,
    ShiftAdd,
    Cmov,
    Unroll2,
    Unroll4,
    SublimateJumpTable,
    Imul,
    Inline,
}

pub struct DosStore;

impl DosStore {
    pub fn new() -> Self { Self }
}

impl Default for DosStore {
    fn default() -> Self { Self::new() }
}

pub fn dos_strategy(_arena: &Arena, _node: NodeId, _dos: &DosStore) -> EmitStrategy {
    todo!("increment 3: port dos_strategy() from the Chapter 3 pseudocode")
}
