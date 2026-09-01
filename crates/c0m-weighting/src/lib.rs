//! Increment 2 — The Weighting Engine (Instinctive Automata).
//!
//! Single bottom-up pass over the arena AST. Computes W_raw from loop
//! depth, branch density, and call depth; merges with W_learned from
//! pattern memory via EMA (alpha = 0.6) into W_combined; assigns dos_tier.
//! See Chapter 2.5 and the formal proof in the blueprint Section 9.

use c0m_frontend::{Arena, NodeId};

pub struct AnalysisCtx {
    pub loop_depth: u8,
    pub call_depth: u8,
    pub branch_density: f32,
}

impl Default for AnalysisCtx {
    fn default() -> Self {
        Self { loop_depth: 0, call_depth: 0, branch_density: 0.0 }
    }
}

/// tier(n) per the blueprint thresholds: 4.0, 12.0, 30.0.
pub fn tier_of(w_combined: f32) -> u8 {
    if w_combined >= 30.0 { 3 }
    else if w_combined >= 12.0 { 2 }
    else if w_combined >= 4.0 { 1 }
    else { 0 }
}

/// EMA merge: W_combined = 0.6 * W_raw + 0.4 * W_learned.
pub fn combine(w_raw: f32, w_learned: f32) -> f32 {
    0.6 * w_raw + 0.4 * w_learned
}

pub fn analyze(_arena: &mut Arena, _root: NodeId, _ctx: &mut AnalysisCtx) -> f32 {
    todo!("increment 2: bottom-up recursion per the Chapter 3 analyze() pseudocode")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_thresholds_match_blueprint() {
        assert_eq!(tier_of(3.9), 0);
        assert_eq!(tier_of(4.0), 1);
        assert_eq!(tier_of(11.9), 1);
        assert_eq!(tier_of(30.0), 3);
    }
}
