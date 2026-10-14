//! Increment 3 — Deferred Optimization Store.
//!
//! Answers "given this node, its tier and its shape, which strategy?"
//! without generating any code. The emitter (increment 4) cooks; the
//! DOS only names the dish.
//!
//! Three questions per node, always in this order, cheapest first:
//!   1. Is the tier high enough?          (one integer comparison)
//!   2. Is the strategy legal right here? (read-only look at the tree)
//!   3. Of the legal ones, which is best? (a ladder, best first)
//!
//! Two safety rules:
//!   - `Direct` is always legal: the answer whenever anything is in doubt.
//!   - Every other strategy computes the same result as `Direct`.

use std::collections::HashMap;
use c0m_frontend::{Arena, AstNode, NodeId, NodeKind, Payload};

//The menu and tier gates
/// Every way the emitter may translate a node. No variant carries data,
/// so a value is one byte and `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitStrategy {
    Direct,
    ///Constant multiply as a single 'lea'
    LeaChain,
    /// Constant multiply with at most two set bits, as shifts and an add.
    ShiftAdd,
    /// Branchless `if`/`else` via a conditional move.
    Cmov,
    /// Innermost loop body repeated twice per iteration.
    Unroll2,
    /// Innermost loop body repeated four times per iteration.
    Unroll4,
    /// Dense integer `match` compiled to a jump table.
    SublimateJumpTable,
    /// Small leaf callee expanded at the call site.
    Inline,

}

impl EmitStrategy {
    /// Every strategy, for tests and for printing the menu.
    pub const ALL: [EmitStrategy; 8] = [
        Self::Direct,
        Self::LeaChain,
        Self::ShiftAdd,
        Self::Cmov,
        Self::Unroll2,
        Self::Unroll4,
        Self::SublimateJumpTable,
        Self::Inline,
    ];

    /// The tier gate: the lowest tier allowed to use this strategy.
    /// A `match`, not an array indexed by `self as usize`, so the compiler
    /// forces every new variant to declare its gate.
    pub const fn min_tier(self) -> u8 {
        match self {
            Self::Direct => 0,
            Self::LeaChain
            | Self::ShiftAdd
            | Self::Cmov
            | Self::Unroll2
            | Self::Inline => 1,
            Self::Unroll4 | Self::SublimateJumpTable => 2,
        }
    }

      /// is this strategy within the node's budget?
    pub const fn unlocked_at(self, tier: u8) -> bool {
        tier >= self.min_tier()
    }
}

/// Question 1 for a whole rule at once: if none of a rule's strategies is
/// unlocked, the rule stops here and no tree is inspected at all.
fn any_unlocked(tier: u8, candidates: &[EmitStrategy]) -> bool {
    candidates.iter().any(|s| s.unlocked_at(tier))
}

// Tunable priors (like the 4/12/30 tier thresholds: reasoned starting
// values, to be justified or adjusted by the test suite)
/// Largest loop body (in AST nodes) that may be duplicated by unrolling.
const UNROLL_BODY_MAX_NODES: usize = 32;
/// Largest right-hand side (in AST nodes) a `cmov` arm may speculate.
const CMOV_ARM_MAX_NODES: usize = 12;
/// Largest callee expression (in AST nodes) that may be inlined.
const INLINE_BODY_MAX_NODES: usize = 16;
/// A jump table needs at least this many integer arms...
const JUMP_TABLE_MIN_ARMS: usize = 4;
/// ...and its value range may be at most this many times the arm count.
const JUMP_TABLE_MAX_SPARSITY: usize = 3;

/// The store: a small index, not a code cache
/// Holds the function-name index that the `Inline` check needs to find a
/// callee's body. Built once per program. It is a lookup table, not an IR.
pub struct DosStore {
    functions: HashMap<String, NodeId>,
}

impl DosStore {
    /// An empty store knows no functions, so `Inline` is never legal.
    /// That is the safe default: when in doubt, `Direct`.
    pub fn new() -> Self {
        Self { functions: HashMap::new() }
    }

    /// Indexes every `FuncDef` under the `Program` root by name.
    pub fn build(arena: &Arena, root: NodeId) -> Self {
        let mut functions = HashMap::new();
        for &f in &arena.get(root).children {
            if let Some(Payload::Ident(name)) = &arena.get(f).payload {
                functions.insert(name.clone(), f);
            }
        }
        Self { functions }
    }

    pub fn function(&self, name: &str) -> Option<NodeId> {
        self.functions.get(name).copied()
    }
}

impl Default for DosStore {
    fn default() -> Self {
        Self::new()
    }
}


// Structural checks: read-only questions about the tree
/// How many nodes are in the subtree rooted at `id` (itself included).
fn subtree_size(arena: &Arena, id: NodeId) -> usize {
    1 + arena
        .get(id)
        .children
        .iter()
        .map(|&c| subtree_size(arena, c))
        .sum::<usize>()
}

/// Does any node in the subtree (including `id`) satisfy `pred`?
fn any_node(arena: &Arena, id: NodeId, pred: &dyn Fn(&AstNode) -> bool) -> bool {
    let n = arena.get(id);
    pred(n) || n.children.iter().any(|&c| any_node(arena, c, pred))
}

/// The value of an integer literal node, if it is one.
fn int_value(arena: &Arena, id: NodeId) -> Option<i64> {
    let n = arena.get(id);
    if n.kind != NodeKind::IntLit {
        return None;
    }
    match &n.payload {
        Some(Payload::Int(v)) => Some(*v),
        _ => None,
    }
}

/// `cmov` evaluates BOTH arms. So an arm must be cheap and must not be able
/// to fault or have effects: no calls, no dereferences, no division or
/// remainder (divide by zero), and no nested control flow.
fn is_cheap_and_safe(arena: &Arena, id: NodeId) -> bool {
    !any_node(arena, id, &|n| {
        matches!(
            n.kind,
            NodeKind::Call
                | NodeKind::Deref
                | NodeKind::BinaryDiv
                | NodeKind::BinaryMod
                | NodeKind::If
                | NodeKind::Match
                | NodeKind::Block
        )
    })
}

/// If `block` is exactly `{ name = <cheap expression>; }`, returns `name`.
fn simple_assign_arm<'a>(arena: &'a Arena, block: NodeId) -> Option<&'a str> {
    let b = arena.get(block);
    if b.kind != NodeKind::Block || b.has_tail || b.children.len() != 1 {
        return None;
    }
    let assign = arena.get(b.children[0]);
    if assign.kind != NodeKind::Assign {
        return None;
    }
    let target = arena.get(assign.children[0]);
    let name = match (&target.kind, &target.payload) {
        (NodeKind::Ident, Some(Payload::Ident(n))) => n.as_str(),
        _ => return None, // storing through a pointer cannot be a cmov
    };
    let rhs = assign.children[1];
    if subtree_size(arena, rhs) <= CMOV_ARM_MAX_NODES && is_cheap_and_safe(arena, rhs) {
        Some(name)
    } else {
        None
    }
}

/// `if c { r = a; } else { r = b; }` (or with no else) is a conditional move.
/// An `else if` chain is deliberately left alone in this version.
fn cmov_eligible(arena: &Arena, node: NodeId) -> bool {
    let kids = &arena.get(node).children; // [cond, then, else?]
    let Some(target) = simple_assign_arm(arena, kids[1]) else {
        return false;
    };
    match kids.get(2) {
        None => true, // missing else: the variable simply keeps its old value
        Some(&e) => matches!(simple_assign_arm(arena, e), Some(t) if t == target),
    }
}

// Returns the loop's iteration count if the loop may be unrolled at all.
/// Structure decides WHETHER; the tier decides HOW MUCH (see `decide_loop`).
fn unroll_bound(arena: &Arena, node: NodeId) -> Option<u32> {
    let n = arena.get(node);
    let bound = n.loop_bound_hint?; // no known bound => no unrolling
    let body = n.children[2]; // [lo, hi, body]
    let var = match &n.payload {
        Some(Payload::Ident(v)) => v.as_str(),
        _ => return None,
    };

    if subtree_size(arena, body) > UNROLL_BODY_MAX_NODES {
        return None;
    }
    let has_inner_loop = any_node(arena, body, &|b| {
        matches!(b.kind, NodeKind::Loop | NodeKind::ParallelLoop)
    });
    if has_inner_loop {
        return None; // only the innermost loop is worth unrolling
    }
    // Unrolling assumes the counter advances by exactly 1 per iteration,
    // so a body that writes to the counter breaks that assumption.
    let writes_counter = any_node(arena, body, &|b| {
        b.kind == NodeKind::Assign
            && matches!(&arena.get(b.children[0]).payload,
                        Some(Payload::Ident(n)) if n == var)
    });
    if writes_counter {
        return None;
    }
    Some(bound)
}

/// A dense integer `match`: at least a few arms, all integer patterns with
/// no duplicates (an optional catch-all may come last), values close together.
fn jump_table_eligible(arena: &Arena, node: NodeId) -> bool {
    let arms = &arena.get(node).children[1..]; // [scrutinee, arms...]
    let last = arms.len().saturating_sub(1);
    let mut values: Vec<i64> = Vec::new();

    for (i, &arm) in arms.iter().enumerate() {
        let pattern = arena.get(arena.get(arm).children[0]);
        match pattern.kind {
            NodeKind::IntLit => match &pattern.payload {
                Some(Payload::Int(v)) => values.push(*v),
                _ => return false,
            },
            // a catch-all is fine, but only as the final arm
            NodeKind::Wildcard | NodeKind::Ident if i == last => {}
            _ => return false,
        }
    }

    if values.len() < JUMP_TABLE_MIN_ARMS {
        return false;
    }
    values.sort_unstable();
    if values.windows(2).any(|w| w[0] == w[1]) {
        return false; // duplicates: first-match-wins needs care, so skip
    }
    let (min, max) = (values[0], values[values.len() - 1]);
    let range = i128::from(max) - i128::from(min) + 1;
    range <= (JUMP_TABLE_MAX_SPARSITY * values.len()) as i128
}

/// A callee is inlinable if it is a known function whose whole body is a
/// single `return <small expression>;` with no calls inside it (a "leaf").
/// Leaf-only guarantees termination: an inlined body can never trigger
/// further expansion, so recursion cannot loop the compiler.
fn inline_eligible(arena: &Arena, node: NodeId, dos: &DosStore) -> bool {
    let call = arena.get(node); // [callee, args...]
    let name = match &arena.get(call.children[0]).payload {
        Some(Payload::Ident(n)) => n.as_str(),
        _ => return false,
    };
    let Some(func) = dos.function(name) else {
        return false; // unknown callee: cannot see its body
    };
    let f = arena.get(func); // [ParamList, ReturnType, Block]
    if arena.get(f.children[0]).children.len() != call.children.len() - 1 {
        return false; // argument count mismatch
    }
    if arena.get(f.children[1]).children.len() != 1 {
        return false; // exactly one return value
    }
    let body = arena.get(f.children[2]);
    if body.kind != NodeKind::Block || body.has_tail || body.children.len() != 1 {
        return false;
    }
    let ret = arena.get(body.children[0]);
    if ret.kind != NodeKind::Return || ret.children.len() != 1 {
        return false;
    }
    let expr = ret.children[0];
    subtree_size(arena, expr) <= INLINE_BODY_MAX_NODES
        && !any_node(arena, expr, &|n| n.kind == NodeKind::Call)
}


// Round 3 — one decision rule per node kind
fn decide_mul(arena: &Arena, node: NodeId, tier: u8) -> EmitStrategy {
    if !any_unlocked(tier, &[EmitStrategy::LeaChain, EmitStrategy::ShiftAdd]) {
        return EmitStrategy::Direct;
    }
    let kids = &arena.get(node).children;
    let Some(c) = int_value(arena, kids[1]).or_else(|| int_value(arena, kids[0])) else {
        return EmitStrategy::Direct; // variable * variable: plain imul
    };
    // The ladder, best first.
    if EmitStrategy::LeaChain.unlocked_at(tier) && matches!(c, 3 | 5 | 9) {
        EmitStrategy::LeaChain
    } else if EmitStrategy::ShiftAdd.unlocked_at(tier) && c >= 2 && c.count_ones() <= 2 {
        EmitStrategy::ShiftAdd
    } else {
        EmitStrategy::Direct
    }
}

fn decide_if(arena: &Arena, node: NodeId, tier: u8) -> EmitStrategy {
    if !any_unlocked(tier, &[EmitStrategy::Cmov]) {
        return EmitStrategy::Direct;
    }
    if cmov_eligible(arena, node) {
        EmitStrategy::Cmov
    } else {
        EmitStrategy::Direct
    }
}

/// `for` and `parallel for` share this rule. A `parallel for` promises that
/// iterations may run in any order, and ascending order is one such order,
/// so it is emitted sequentially and unrolled exactly like `for`.
fn decide_loop(arena: &Arena, node: NodeId, tier: u8) -> EmitStrategy {
    if !any_unlocked(tier, &[EmitStrategy::Unroll2, EmitStrategy::Unroll4]) {
        return EmitStrategy::Direct;
    }
    let Some(bound) = unroll_bound(arena, node) else {
        return EmitStrategy::Direct;
    };
    // The ladder: the most the tier allows, then fall back while the
    // iteration count still divides evenly (no leftover loop is built).
    if EmitStrategy::Unroll4.unlocked_at(tier) && bound % 4 == 0 {
        EmitStrategy::Unroll4
    } else if EmitStrategy::Unroll2.unlocked_at(tier) && bound % 2 == 0 {
        EmitStrategy::Unroll2
    } else {
        EmitStrategy::Direct
    }
}

fn decide_match(arena: &Arena, node: NodeId, tier: u8) -> EmitStrategy {
    if !any_unlocked(tier, &[EmitStrategy::SublimateJumpTable]) {
        return EmitStrategy::Direct;
    }
    if jump_table_eligible(arena, node) {
        EmitStrategy::SublimateJumpTable
    } else {
        EmitStrategy::Direct
    }
}

fn decide_call(arena: &Arena, node: NodeId, tier: u8, dos: &DosStore) -> EmitStrategy {
    if !any_unlocked(tier, &[EmitStrategy::Inline]) {
        return EmitStrategy::Direct;
    }
    if inline_eligible(arena, node, dos) {
        EmitStrategy::Inline
    } else {
        EmitStrategy::Direct
    }
}

/// The DOS's single entry point. Reads the tier the weighting engine wrote
/// (including any `@N` override) and never recomputes it. Node kinds with
/// no menu entry get `Direct`.
pub fn dos_strategy(arena: &Arena, node: NodeId, dos: &DosStore) -> EmitStrategy {
    let n = arena.get(node);
    let tier = n.weight.dos_tier;
    match n.kind {
        NodeKind::BinaryMul => decide_mul(arena, node, tier),
        NodeKind::If => decide_if(arena, node, tier),
        NodeKind::Loop | NodeKind::ParallelLoop => decide_loop(arena, node, tier),
        NodeKind::Match => decide_match(arena, node, tier),
        NodeKind::Call => decide_call(arena, node, tier, dos),
        _ => EmitStrategy::Direct,
    }
}


// Round 4 — the strategy log: which dish did each node get?
/// One slot per AST node, indexed by `NodeId`. Arena ids are dense
/// (0..len), so a plain `Vec` is a perfect lookup table: no hashing.
/// `Option<EmitStrategy>` still fits in one byte (spare enum values hold
/// the `None`).
pub struct StrategyLog {
    slots: Vec<Option<EmitStrategy>>,
}

impl StrategyLog {
    pub fn new(node_count: usize) -> Self {
        Self { slots: vec![None; node_count] }
    }

    /// The emitter calls this once per node as it walks. Panics if `id` is
    /// outside the size given to `new` (a bug, not a runtime condition).
    pub fn record(&mut self, id: NodeId, strategy: EmitStrategy) {
        self.slots[id.0 as usize] = Some(strategy);
    }

    pub fn get(&self, id: NodeId) -> Option<EmitStrategy> {
        self.slots.get(id.0 as usize).copied().flatten()
    }

    /// Every recorded (node, strategy) pair in arena order: the raw
    /// material for `--trace-emit`.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, EmitStrategy)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|s| (NodeId(i as u32), s)))
    }

    /// The same data keyed by `AstNode::key()`, for the feedback loop, which
    /// finds nodes by span hash rather than by arena position. Built once,
    /// at the boundary, so the hot path never pays for hashing.
    pub fn by_key(&self, arena: &Arena) -> HashMap<u64, EmitStrategy> {
        self.iter().map(|(id, s)| (arena.get(id).key(), s)).collect()
    }
}

/// Asks the DOS about every node. The DOS only reads the tree, so the order
/// of asking cannot matter, and a flat loop over the arena is enough.
pub fn plan_all(arena: &Arena, dos: &DosStore) -> StrategyLog {
    let mut log = StrategyLog::new(arena.len());
    for i in 0..arena.len() {
        let id = NodeId(i as u32);
        log.record(id, dos_strategy(arena, id, dos));
    }
    log
}


// Round 5 — tests

#[cfg(test)]
mod tests {
    use super::*;
    use c0m_frontend::parse_source;
    use c0m_weighting::{analyze, no_history};
    use EmitStrategy::*;

    const MAX_TIER: u8 = 3;

    // ── helpers ──────────────────────────────────────────────────────

    /// Ids of all nodes of `kind`, in arena order (children before parents,
    /// so an inner loop comes before the loop around it).
    fn ids_of(arena: &Arena, kind: NodeKind) -> Vec<NodeId> {
        (0..arena.len() as u32)
            .map(NodeId)
            .filter(|&id| arena.get(id).kind == kind)
            .collect()
    }

    /// Parses `src`, FORCES `tier` onto every node of `kind` (bypassing the
    /// weighting engine), and returns the DOS answer for each, in arena
    /// order. Tests one rule in isolation.
    fn strategies_at(src: &str, kind: NodeKind, tier: u8) -> Vec<EmitStrategy> {
        let (mut arena, root) = parse_source(src, 0).expect("should parse");
        let ids = ids_of(&arena, kind);
        for &id in &ids {
            arena.get_mut(id).weight.dos_tier = tier;
        }
        let dos = DosStore::build(&arena, root);
        ids.iter().map(|&id| dos_strategy(&arena, id, &dos)).collect()
    }

    /// The full pipeline: parse, weigh for real, then plan.
    fn weighted_plan(src: &str) -> (Arena, StrategyLog) {
        let (mut arena, root) = parse_source(src, 0).expect("should parse");
        analyze(&mut arena, root, &no_history);
        let dos = DosStore::build(&arena, root);
        let log = plan_all(&arena, &dos);
        (arena, log)
    }

    fn planned(arena: &Arena, log: &StrategyLog, kind: NodeKind) -> Vec<EmitStrategy> {
        ids_of(arena, kind)
            .into_iter()
            .map(|id| log.get(id).expect("every node is planned"))
            .collect()
    }

    fn mul(expr: &str, tier: u8) -> EmitStrategy {
        let src = format!("fn f(x: i64, y: i64) -> i64 {{ return {expr}; }}");
        strategies_at(&src, NodeKind::BinaryMul, tier)[0]
    }

    /// Strategy of the OUTERMOST `if` in `stmt` (the last one allocated).
    fn if_stmt(stmt: &str, tier: u8) -> EmitStrategy {
        let src = format!("fn f(x: i64) -> () {{ let mut r: i64 = 0; {stmt} }}");
        *strategies_at(&src, NodeKind::If, tier).last().unwrap()
    }

    fn match_arms(arms: &str, tier: u8) -> EmitStrategy {
        let src = format!("fn f(x: i64) -> () {{ match x {{ {arms} }} }}");
        strategies_at(&src, NodeKind::Match, tier)[0]
    }

    const CALLS: &str = "fn double(x: i64) -> i64 { return x * 2; } \
                         fn quad(x: i64) -> i64 { return double(double(x)); } \
                         fn main() -> () { let r: i64 = quad(5); }";

    // ── Round 1: menu and gates ──────────────────────────────────────

    #[test]
    fn tier_zero_unlocks_nothing_but_direct() {
        for s in EmitStrategy::ALL {
            assert_eq!(s.unlocked_at(0), s == Direct, "{s:?}");
        }
    }

    #[test]
    fn higher_gates_stay_latent_below_their_tier() {
        assert!(!Unroll4.unlocked_at(1));
        assert!(Unroll4.unlocked_at(2));
        assert!(!SublimateJumpTable.unlocked_at(1));
        assert!(Cmov.unlocked_at(1));
    }

    #[test]
    fn gates_are_nested() {
        for s in EmitStrategy::ALL {
            for t in 0..MAX_TIER {
                if s.unlocked_at(t) {
                    assert!(s.unlocked_at(t + 1), "{s:?} at {t} but not {}", t + 1);
                }
            }
        }
    }

    #[test]
    fn menu_list_matches_the_enum() {
        assert_eq!(EmitStrategy::ALL.len(), 8);
        for (i, a) in EmitStrategy::ALL.iter().enumerate() {
            for b in &EmitStrategy::ALL[i + 1..] {
                assert_ne!(a, b, "duplicate in ALL");
            }
        }
    }

    #[test]
    fn unweighted_trees_stay_direct_everywhere() {
        // Never weighed => every node is tier 0 => nothing is unlocked.
        let src = include_str!("../../../c0m-tests/04_hot_kernel.c0m");
        let (arena, root) = parse_source(src, 0).expect("should parse");
        let log = plan_all(&arena, &DosStore::build(&arena, root));
        assert!(log.iter().all(|(_, s)| s == Direct));
    }

    // ── Round 2/3: multiply ──────────────────────────────────────────

    #[test]
    fn multiply_by_constant_climbs_the_ladder() {
        assert_eq!(mul("x * 5", 1), LeaChain);
        assert_eq!(mul("5 * x", 1), LeaChain); // constant on either side
        assert_eq!(mul("x * 8", 1), ShiftAdd); // power of two
        assert_eq!(mul("x * 6", 1), ShiftAdd); // two set bits
    }

    #[test]
    fn multiply_falls_back_to_direct() {
        assert_eq!(mul("x * 7", 3), Direct); // three set bits
        assert_eq!(mul("x * y", 3), Direct); // no constant: plain imul
        assert_eq!(mul("x * 1", 3), Direct);
        assert_eq!(mul("x * 5", 0), Direct); // not earned yet
    }

    // ── Round 2/3: cmov ──────────────────────────────────────────────

    #[test]
    fn simple_if_else_becomes_cmov() {
        assert_eq!(if_stmt("if x > 0 { r = 1; } else { r = 2; }", 1), Cmov);
        assert_eq!(if_stmt("if x > 0 { r = 1; }", 1), Cmov); // no else
        assert_eq!(if_stmt("if x > 0 { r = 1; } else { r = 2; }", 0), Direct);
    }

    #[test]
    fn unsafe_or_complex_ifs_stay_direct() {
        // different targets
        assert_eq!(if_stmt("if x > 0 { r = 1; } else { s = 2; }", 3), Direct);
        // a call in an arm
        assert_eq!(if_stmt("if x > 0 { r = g(x); } else { r = 2; }", 3), Direct);
        // division can fault, and cmov would run it even when not chosen
        assert_eq!(if_stmt("if x > 0 { r = 10 / x; } else { r = 2; }", 3), Direct);
        // store through a pointer
        assert_eq!(if_stmt("if x > 0 { *p = 1; } else { *p = 2; }", 3), Direct);
        // more than one statement per arm
        assert_eq!(if_stmt("if x > 0 { r = 1; s = 2; } else { r = 2; s = 3; }", 3), Direct);
        // else-if chain (outermost if)
        assert_eq!(if_stmt("if x > 0 { r = 1; } else if x == 0 { r = 0; }", 3), Direct);
        // if used as a value, not a statement
        assert_eq!(if_stmt("let y: i64 = if x > 0 { 1 } else { 2 };", 3), Direct);
    }

    // ── Round 2/3: loops ─────────────────────────────────────────────

    #[test]
    fn tier_decides_how_much_to_unroll() {
        let src = "fn f() -> () { for i in 0..8 { let a: i64 = i; } }";
        assert_eq!(strategies_at(src, NodeKind::Loop, 0), vec![Direct]);
        assert_eq!(strategies_at(src, NodeKind::Loop, 1), vec![Unroll2]);
        assert_eq!(strategies_at(src, NodeKind::Loop, 2), vec![Unroll4]);
    }

    #[test]
    fn the_bound_must_divide_evenly_so_the_ladder_steps_down() {
        let six = "fn f() -> () { for i in 0..6 { let a: i64 = i; } }";
        let seven = "fn f() -> () { for i in 0..7 { let a: i64 = i; } }";
        assert_eq!(strategies_at(six, NodeKind::Loop, 3), vec![Unroll2]);
        assert_eq!(strategies_at(seven, NodeKind::Loop, 3), vec![Direct]);
    }

    #[test]
    fn unknown_bound_means_direct() {
        let src = "fn f(n: i64) -> () { for i in 0..n { let a: i64 = i; } }";
        assert_eq!(strategies_at(src, NodeKind::Loop, 3), vec![Direct]);
    }

    #[test]
    fn only_the_innermost_loop_is_unrolled() {
        let src = "fn f() -> () { for i in 0..8 { for j in 0..8 { let a: i64 = i; } } }";
        // arena order: inner first, then outer
        assert_eq!(strategies_at(src, NodeKind::Loop, 2), vec![Unroll4, Direct]);
    }

    #[test]
    fn writing_to_the_counter_or_a_huge_body_blocks_unrolling() {
        let writes = "fn f() -> () { for i in 0..8 { i = i + 1; } }";
        assert_eq!(strategies_at(writes, NodeKind::Loop, 3), vec![Direct]);

        let small = format!("fn f() -> () {{ for i in 0..8 {{ {} }} }}", "let a: i64 = 1; ".repeat(2));
        let huge = format!("fn f() -> () {{ for i in 0..8 {{ {} }} }}", "let a: i64 = 1; ".repeat(12));
        assert_eq!(strategies_at(&small, NodeKind::Loop, 1), vec![Unroll2]);
        assert_eq!(strategies_at(&huge, NodeKind::Loop, 1), vec![Direct]);
    }

    #[test]
    fn parallel_for_is_treated_like_for() {
        let src = "fn f() -> () { parallel for i in 0..8 { let a: i64 = i; } }";
        assert_eq!(strategies_at(src, NodeKind::ParallelLoop, 1), vec![Unroll2]);
        assert_eq!(strategies_at(src, NodeKind::ParallelLoop, 0), vec![Direct]);
    }

    // ── Round 2/3: match ─────────────────────────────────────────────

    #[test]
    fn dense_integer_match_becomes_a_jump_table() {
        let dense = "0 => 1, 1 => 2, 2 => 3, 3 => 4, _ => 0,";
        assert_eq!(match_arms(dense, 2), SublimateJumpTable);
        assert_eq!(match_arms(dense, 1), Direct); // tier gate
    }

    #[test]
    fn unsuitable_matches_stay_direct() {
        assert_eq!(match_arms("0 => 1, 1 => 2, 2 => 3, _ => 0,", 3), Direct); // too few
        assert_eq!(match_arms("0 => 1, 10 => 2, 20 => 3, 100 => 4,", 3), Direct); // sparse
        assert_eq!(match_arms("1 => 1, 1 => 2, 2 => 3, 3 => 4,", 3), Direct); // duplicate
        assert_eq!(match_arms("_ => 0, 1 => 1, 2 => 2, 3 => 3, 4 => 4,", 3), Direct); // catch-all first
        assert_eq!(match_arms(r#""a" => 1, "b" => 2, "c" => 3, "d" => 4,"#, 3), Direct); // strings
    }

    // ── Round 2/3: inline ────────────────────────────────────────────

    #[test]
    fn small_leaf_callees_are_inlined() {
        // arena order: inner double(x), outer double(..), quad(5) in main
        assert_eq!(strategies_at(CALLS, NodeKind::Call, 1), vec![Inline, Inline, Direct]);
        assert_eq!(strategies_at(CALLS, NodeKind::Call, 0), vec![Direct, Direct, Direct]);
    }

    #[test]
    fn calls_that_cannot_be_inlined_stay_direct() {
        let arity = "fn g(x: i64, y: i64) -> i64 { return x; } fn main() -> () { let r: i64 = g(1); }";
        let unknown = "fn main() -> () { let r: i64 = h(1); }";
        let multi = "fn g(x: i64) -> i64 { let y: i64 = x; return y; } fn main() -> () { let r: i64 = g(1); }";
        let recursive = "fn g(x: i64) -> i64 { return g(x); }";
        for src in [arity, unknown, multi, recursive] {
            assert!(strategies_at(src, NodeKind::Call, 3).iter().all(|&s| s == Direct), "{src}");
        }
    }

    #[test]
    fn an_empty_store_never_inlines() {
        let (mut arena, _root) = parse_source(CALLS, 0).expect("should parse");
        let id = ids_of(&arena, NodeKind::Call)[0];
        arena.get_mut(id).weight.dos_tier = 1;
        assert_eq!(dos_strategy(&arena, id, &DosStore::new()), Direct);
    }

    // ── Round 4: the log ─────────────────────────────────────────────

    #[test]
    fn log_records_and_reads_back() {
        let mut log = StrategyLog::new(3);
        assert_eq!(log.get(NodeId(1)), None);
        log.record(NodeId(1), Cmov);
        assert_eq!(log.get(NodeId(1)), Some(Cmov));
        assert_eq!(log.iter().collect::<Vec<_>>(), vec![(NodeId(1), Cmov)]);
    }

    #[test]
    fn an_optional_strategy_still_fits_in_one_byte() {
        assert_eq!(std::mem::size_of::<Option<EmitStrategy>>(), 1);
    }

    #[test]
    fn plan_covers_every_node_and_keys_stay_unique() {
        let (arena, log) = weighted_plan(include_str!("../../../c0m-tests/04_hot_kernel.c0m"));
        assert_eq!(log.iter().count(), arena.len());
        assert_eq!(log.by_key(&arena).len(), arena.len());
    }

    // ── Round 5: the real test programs, end to end ──────────────────

    #[test]
    fn suite_00_arithmetic_has_nothing_to_optimize() {
        let (_, log) = weighted_plan(include_str!("../../../c0m-tests/00_arithmetic.c0m"));
        assert!(log.iter().all(|(_, s)| s == Direct));
    }

    #[test]
    fn suite_01_branching_only_the_simple_inner_if_is_a_cmov() {
        let (arena, log) = weighted_plan(include_str!("../../../c0m-tests/01_branching.c0m"));
        // inner `if x == 0 {..} else {..}` first, then the outer else-if chain
        assert_eq!(planned(&arena, &log, NodeKind::If), vec![Cmov, Direct]);
    }

    #[test]
    fn suite_02_single_loop_is_unrolled_by_two() {
        let (arena, log) = weighted_plan(include_str!("../../../c0m-tests/02_single_loop.c0m"));
        assert_eq!(planned(&arena, &log, NodeKind::Loop), vec![Unroll2]);
    }

    #[test]
    fn suite_03_nested_loop_unrolls_only_the_inner_one() {
        let (arena, log) = weighted_plan(include_str!("../../../c0m-tests/03_nested_loop.c0m"));
        assert_eq!(planned(&arena, &log, NodeKind::Loop), vec![Unroll2, Direct]);
    }

    #[test]
    fn suite_04_hot_kernel_gets_the_heaviest_treatment_innermost() {
        let (arena, log) = weighted_plan(include_str!("../../../c0m-tests/04_hot_kernel.c0m"));
        // k (innermost), j, i
        assert_eq!(planned(&arena, &log, NodeKind::Loop), vec![Unroll4, Direct, Direct]);
        assert_eq!(planned(&arena, &log, NodeKind::If), vec![Cmov]);
    }

    #[test]
    fn suite_05_calls_inline_the_nested_leaf_calls_only() {
        let (arena, log) = weighted_plan(include_str!("../../../c0m-tests/05_calls.c0m"));
        assert_eq!(planned(&arena, &log, NodeKind::Call), vec![Inline, Inline, Direct]);
    }

    #[test]
    fn suite_06_an_override_raises_the_budget_but_cannot_invent_strategies() {
        let (arena, log) = weighted_plan(include_str!("../../../c0m-tests/06_tier_override.c0m"));
        assert!(arena.get(NodeId(0)).weight.dos_tier <= MAX_TIER);
        assert!(log.iter().all(|(_, s)| s == Direct));
    }
}
