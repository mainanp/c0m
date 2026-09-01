//! Arena allocation for AST nodes.
//!
//! Chosen deliberately (see Chapter 3.6.1): no individual frees, no GC
//! pressure, cache-friendly bottom-up traversal for the Pattern Analyzer,
//! and teardown is a single reset between compiles.

use crate::ast::{AstNode, NodeId};

pub struct Arena {
    nodes: Vec<AstNode>,
}

impl Arena {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Allocates a node with a pre-zeroed WeightRec, per the blueprint's
    /// parser design (weights are filled later by c0m-weighting).
    pub fn alloc(&mut self, node: AstNode) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    pub fn get(&self, id: NodeId) -> &AstNode {
        &self.nodes[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: NodeId) -> &mut AstNode {
        &mut self.nodes[id.0 as usize]
    }

    /// Full reset between compiles — no destructor chains.
    pub fn reset(&mut self) {
        self.nodes.clear();
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}
