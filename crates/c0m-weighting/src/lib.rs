//! Increment 2 — The Weighting Engine (Instinctive Automata).
//!
//! Single recursive pass over the arena AST. Information travels in two
//! directions during that one pass:
//!   DOWN  (as an argument): context, i.e. "where am I?"  (loop depth, ...)
//!   UP    (as the return value): the node's raw weight, i.e. "what is
//!         inside me?" — the parent adds its children's returns together.
//! Per node the engine fills in `WeightRec`:
//!   raw       structural score (own rule + children's raws)
//!   learned   what past runs taught us (0.0 if the node was never seen)
//!   combined  raw alone if unseen, else 0.6*raw + 0.4*learned
//!   dos_tier  max(tier of combined, tier of heat), unless @N overrides it

use c0m_frontend::{Arena, AstNode, NodeId, NodeKind};


// ── Tunable priors (the blueprint's coefficients) ───────────────────
const LOOP_COEFF: f32 = 3.0;
const BRANCH_COEFF: f32 = 1.5;
const CALL_COEFF: f32 = 2.0;
const CHILD_COEFF: f32 = 0.5;
/// Known loop bounds above this get a boost...
const BIG_LOOP_BOUND: u32 = 64;
/// ...of this factor.
const BIG_LOOP_BOOST: f32 = 1.5;

///What a node inherits from the nodes above it
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalyticsCtx {
    /// Number of loops whose BODY this node sits in.
    pub loop_depth: u8,
    /// Number of calls whose ARGUMENTS this node sits in.
    pub call_depth: u8,
    /// Number of if/match branches this node sits in.
    pub branch_depth: u8,
}

/// tier(n) per the thresholds: 4.0, 12.0, 30.0
pub fn tier_of(w_combined: f32) -> u8 {
    if w_combined >= 30.0 { 3 }
    else if w_combined >= 12.0 { 2 }
    else if w_combined >= 4.0 { 1 }
    else{ 0 }
}

/// EMA merge: w_combined = 0.6 * w_raw + 0.4 * w_learned
pub fn combine(w_raw: f32, w_learned: f32) -> f32 {
    0.6 * w_raw + 0.4 * w_learned
}

/// The lowest tier a node may have just because of WHERE it sits.
/// A node inside `d` loops runs about d times as often as its function,
/// so its heat is d * LOOP_COEFF, run through the same thresholds.
fn heat_floor(ctx: AnalyticsCtx) -> u8 {
    tier_of(ctx.loop_depth as f32 * LOOP_COEFF)
}

/// The "no history yet" lookup: every node is unseen.
pub fn no_history(_key: u64) -> Option<f32> {
    None
}

/// Weighs the whole tree under `root` and returns the root's raw weight.
///
/// `learned` maps a node key (see `AstNode::key`) to its learned weight:
/// `Some(w)` = seen before, `None` = never seen.
///
/// analyze(&mut arena, root, &no_history);
pub fn analyze(
    arena: &mut Arena,
    root: NodeId,
    learned: &dyn Fn(u64) -> Option<f32>,
) -> f32 {
    analyze_node(arena, root, AnalyticsCtx::default(), learned)
}

/// The recursive part. Returns this node's RAW weight (never `combined`)
/// so a learned note can't leak upward into its parent's raw weight.
fn analyze_node(
    arena: &mut Arena,
    id: NodeId,
    ctx: AnalyticsCtx,
    learned: &dyn Fn(u64) -> Option<f32>,
) -> f32 {
    // Short looks: each ends at its semicolon, copying out small values.
    let kind = arena.get(id).kind;
    let child_count = arena.get(id).children.len();

    // Recurse into the children. DOWN goes the context, UP comes the weight.
    let mut child_sum = 0.0_f32;
    for i in 0..child_count {
        let child = arena.get(id).children[i]; // copies a NodeId out
        let child_ctx = context_for_child(kind, i, ctx);
        child_sum += analyze_node(arena, child, child_ctx, learned);
    }

    // This node's own contribution plus what the children reported.
    let raw = local_score(arena.get(id), ctx) + child_sum;

    // Blend in history. Unseen nodes use raw alone (no cold-start shrink).
    let key = arena.get(id).key();
    let (learned_w, combined) = match learned(key) {
        Some(l) => (l, combine(raw, l)),
        None => (0.0, raw),
    };

    // Tier: the weight says one thing, the node's position another; take
    // the higher. An explicit @N from the programmer beats both.
    let natural = tier_of(combined).max(heat_floor(ctx));
    let tier = arena.get(id).tier_override.unwrap_or(natural);

    // Record everything on the node, then hand raw up.
    let node = arena.get_mut(id);
    node.weight.raw = raw;
    node.weight.learned = learned_w;
    node.weight.combined = combined;
    node.weight.dos_tier = tier;
    raw
}


/// The context a node hands to its i-th child.
///
/// Only the CONDITIONAL / REPEATED parts of a construct get deeper:
///   Loop  [lo, hi, body]      -> body (2) repeats; the bounds run once
///   If    [cond, then, else?] -> then/else (1, 2) are conditional
///   Match [scrutinee, arms..] -> arms (1..) are conditional
///   Call  [callee, args..]    -> args (1..) are nested calls' territory
fn context_for_child(kind: NodeKind, child_index: usize, ctx: AnalyticsCtx) -> AnalyticsCtx {
    match kind {
        NodeKind::Loop | NodeKind::ParallelLoop if child_index == 2 => AnalyticsCtx {
            // saturating_add: stay at 255 instead of overflowing a u8.
            loop_depth: ctx.loop_depth.saturating_add(1),
            ..ctx
        },
        NodeKind::If | NodeKind::Match if child_index >= 1 => AnalyticsCtx {
            branch_depth: ctx.branch_depth.saturating_add(1),
            ..ctx
        },
        NodeKind::Call if child_index >= 1 => AnalyticsCtx {
            call_depth: ctx.call_depth.saturating_add(1),
            ..ctx
        },
        _ => ctx,
    }
}

/// A node's own score, before adding its children's weights.
/// Rule of thumb: "count me and everything around me", so the +1.
fn local_score(node: &AstNode, ctx: AnalyticsCtx) -> f32 {
    match node.kind {
        NodeKind::Loop | NodeKind::ParallelLoop => {
            let base = (ctx.loop_depth as f32 + 1.0) * LOOP_COEFF;
            match node.loop_bound_hint {
                // `Some(n) if n > ...` = "there is a hint, and it is big"
                Some(n) if n > BIG_LOOP_BOUND => base * BIG_LOOP_BOOST,
                _ => base,
            }
        }
        NodeKind::If | NodeKind::Match => (ctx.branch_depth as f32 + 1.0) * BRANCH_COEFF,
        NodeKind::Call => (ctx.call_depth as f32 + 1.0) * CALL_COEFF,
        _ => node.children.len() as f32 * CHILD_COEFF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use c0m_frontend::parse_source;

    // ── helpers ──────────────────────────────────────────────────────

    fn weigh(src: &str) -> (Arena, NodeId) {
        let (mut arena, root) = parse_source(src, 0).expect("should parse");
        analyze(&mut arena, root, &no_history);
        (arena, root)
    }

    fn child(arena: &Arena, id: NodeId, i: usize) -> NodeId {
        arena.get(id).children[i]
    }

    /// Program -> first function -> its body Block -> first statement.
    fn first_statement(arena: &Arena, root: NodeId) -> NodeId {
        let func = child(arena, root, 0);
        let body = child(arena, func, 2);
        child(arena, body, 0)
    }

    fn raw(arena: &Arena, id: NodeId) -> f32 {
        arena.get(id).weight.raw
    }

    fn tier(arena: &Arena, id: NodeId) -> u8 {
        arena.get(id).weight.dos_tier
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "{a} != {b}");
    }

    // ── thresholds ───────────────────────────────────────────────────

    #[test]
    fn tier_thresholds_match_blueprint() {
        assert_eq!(tier_of(3.9), 0);
        assert_eq!(tier_of(4.0), 1);
        assert_eq!(tier_of(11.9), 1);
        assert_eq!(tier_of(30.0), 3);
    }

    // ── context flows DOWN ───────────────────────────────────────────

    #[test]
    fn only_a_loops_body_gets_deeper() {
        let ctx = AnalyticsCtx::default();
        assert_eq!(context_for_child(NodeKind::Loop, 0, ctx).loop_depth, 0); // lo
        assert_eq!(context_for_child(NodeKind::Loop, 1, ctx).loop_depth, 0); // hi
        assert_eq!(context_for_child(NodeKind::Loop, 2, ctx).loop_depth, 1); // body
        assert_eq!(context_for_child(NodeKind::Block, 0, ctx).loop_depth, 0);
    }

    #[test]
    fn loop_depth_saturates_instead_of_overflowing() {
        let ctx = AnalyticsCtx { loop_depth: 255, ..AnalyticsCtx::default() };
        assert_eq!(context_for_child(NodeKind::Loop, 2, ctx).loop_depth, 255);
    }

    #[test]
    fn branches_and_calls_deepen_only_their_conditional_parts() {
        let ctx = AnalyticsCtx::default();
        // If: [cond, then, else]
        assert_eq!(context_for_child(NodeKind::If, 0, ctx).branch_depth, 0);
        assert_eq!(context_for_child(NodeKind::If, 1, ctx).branch_depth, 1);
        assert_eq!(context_for_child(NodeKind::If, 2, ctx).branch_depth, 1);
        // Call: [callee, args...]
        assert_eq!(context_for_child(NodeKind::Call, 0, ctx).call_depth, 0);
        assert_eq!(context_for_child(NodeKind::Call, 1, ctx).call_depth, 1);
        // the three signals stay separate
        assert_eq!(context_for_child(NodeKind::If, 1, ctx).loop_depth, 0);
        assert_eq!(context_for_child(NodeKind::Call, 1, ctx).branch_depth, 0);
    }

    // ── weight flows UP (worked by hand) ─────────────────────────────

    #[test]
    fn nested_loops_score_deeper_and_sum_upward() {
        let (arena, root) =
            weigh("fn f() -> () { for i in 0..4 { for j in 0..4 { let x: i64 = 1; } } }");
        let outer = first_statement(&arena, root);
        let outer_body = child(&arena, outer, 2);
        let inner = child(&arena, outer_body, 0);

        approx(raw(&arena, inner), 7.5);  // 6.0 own (depth 1) + 1.5 body
        approx(raw(&arena, outer), 11.0); // 3.0 own (depth 0) + 8.0 body
        approx(raw(&arena, root), 13.5);
    }

    // ── regression: the blueprint's never-decremented counter ────────

    #[test]
    fn second_loop_is_not_penalised_for_the_first() {
        let (arena, root) = weigh(
            "fn f() -> () { for i in 0..4 { let a: i64 = 1; } for j in 0..4 { let b: i64 = 2; } }",
        );
        let body = child(&arena, child(&arena, root, 0), 2);
        let first = child(&arena, body, 0);
        let second = child(&arena, body, 1);

        approx(raw(&arena, first), 4.5);
        approx(raw(&arena, second), 4.5);
    }

    // ── Round 3: the per-kind rules ──────────────────────────────────

    #[test]
    fn big_known_loop_bounds_get_a_boost() {
        let (small, r1) = weigh("fn f() -> () { for i in 0..64 { } }");
        let (big, r2) = weigh("fn f() -> () { for i in 0..65 { } }");
        approx(raw(&small, first_statement(&small, r1)), 3.0); // 64 is NOT over 64
        approx(raw(&big, first_statement(&big, r2)), 4.5);     // 3.0 * 1.5
    }

    #[test]
    fn an_if_counts_itself_and_its_condition() {
        let (arena, root) = weigh("fn f(x: i64) -> () { if x > 0 { } }");
        let the_if = first_statement(&arena, root);
        // own 1.5 + condition (CmpGt: 2 children * 0.5 = 1.0) + empty then-block 0
        approx(raw(&arena, the_if), 2.5);
    }

    #[test]
    fn else_if_chains_get_heavier_at_each_step() {
        let (arena, root) = weigh("fn f(a: bool, b: bool) -> () { if a { } else if b { } }");
        let outer = first_statement(&arena, root);
        let inner = child(&arena, outer, 2); // the else branch IS the nested if

        approx(raw(&arena, inner), 3.0); // (1 + 1) * 1.5
        approx(raw(&arena, outer), 4.5); // 1.5 own + 3.0 from the else
    }

    #[test]
    fn nested_call_arguments_are_deeper_but_the_callee_is_not() {
        let (arena, root) = weigh("fn f() -> () { g(h(1)); }");
        let outer = first_statement(&arena, root);
        let inner = child(&arena, outer, 1); // h(1) is an argument of g

        approx(raw(&arena, inner), 4.0); // (1 + 1) * 2.0
        approx(raw(&arena, outer), 6.0); // 2.0 own + 0 (callee name) + 4.0
    }

    #[test]
    fn match_counts_as_a_branch() {
        let (arena, root) = weigh("fn f(x: i64) -> () { match x { 1 => 5, _ => 6, } }");
        let the_match = first_statement(&arena, root);
        // own 1.5 + two arms of (2 children * 0.5) = 1.0 each
        approx(raw(&arena, the_match), 3.5);
    }

    // ── Round 4: learned blend, overrides, tiers ─────────────────────

    #[test]
    fn unseen_nodes_use_raw_alone() {
        let (arena, root) = weigh("fn f() -> () { for i in 0..4 { } }");
        let w = arena.get(first_statement(&arena, root)).weight;
        approx(w.raw, 3.0);
        approx(w.learned, 0.0);
        approx(w.combined, 3.0); // NOT 0.6 * 3.0 = 1.8
    }

    #[test]
    fn learned_weight_blends_for_that_node_only() {
        let src = "fn f() -> () { for i in 0..4 { } }";
        let (mut arena, root) = parse_source(src, 0).expect("should parse");
        let the_loop = first_statement(&arena, root);
        let key = arena.get(the_loop).key();

        analyze(&mut arena, root, &|k| if k == key { Some(10.0) } else { None });

        let w = arena.get(the_loop).weight;
        approx(w.raw, 3.0);
        approx(w.learned, 10.0);
        approx(w.combined, 5.8); // 0.6 * 3.0 + 0.4 * 10.0
        assert_eq!(w.dos_tier, 1);

        // the learned note does not leak upward...
        approx(raw(&arena, root), 5.5);
        // ...or sideways: the enclosing function is still unseen
        let func = child(&arena, root, 0);
        approx(arena.get(func).weight.combined, arena.get(func).weight.raw);
    }

    #[test]
    fn tier_override_can_raise_a_tier() {
        let (arena, root) = weigh("fn f() -> () { @3 for i in 0..4 { } }");
        let the_loop = first_statement(&arena, root);
        assert_eq!(tier(&arena, the_loop), 3);
        approx(raw(&arena, the_loop), 3.0); // the weight itself stays honest
    }

    #[test]
    fn tier_override_can_lower_a_tier() {
        let (arena, root) = weigh("fn f() -> () { @0 for i in 0..1000 { } }");
        let the_loop = first_statement(&arena, root);
        approx(raw(&arena, the_loop), 4.5); // would naturally be tier 1
        assert_eq!(tier(&arena, the_loop), 0);
    }

    #[test]
    fn statements_inside_nested_loops_get_a_tier_floor() {
        let (arena, root) =
            weigh("fn f() -> () { for i in 0..4 { for j in 0..4 { let x: i64 = 1; } } }");
        let outer = first_statement(&arena, root);
        let inner = child(&arena, child(&arena, outer, 2), 0);
        let let_in = child(&arena, child(&arena, inner, 2), 0);
        approx(raw(&arena, let_in), 1.0); // own weight says tier 0...
        assert_eq!(tier(&arena, let_in), 1); // ...but it runs inside two loops

        // contrast: one loop deep is not enough for a floor
        let (arena2, root2) = weigh("fn f() -> () { for i in 0..4 { let a: i64 = 1; } }");
        let lp = first_statement(&arena2, root2);
        let let_a = child(&arena2, child(&arena2, lp, 2), 0);
        assert_eq!(tier(&arena2, let_a), 0);
    }

    #[test]
    fn a_triple_nested_loop_is_tier_2_not_3() {
        let (arena, root) = weigh(
            "fn f() -> () { for a in 0..4 { for b in 0..4 { for c in 0..4 { let x: i64 = 1; } } } }",
        );
        let outer = first_statement(&arena, root);
        let middle = child(&arena, child(&arena, outer, 2), 0);
        let inner = child(&arena, child(&arena, middle, 2), 0);

        approx(raw(&arena, inner), 10.5);
        approx(raw(&arena, middle), 17.0);
        approx(raw(&arena, outer), 20.5);
        assert_eq!(tier(&arena, inner), 1);
        assert_eq!(tier(&arena, middle), 2);
        assert_eq!(tier(&arena, outer), 2);
    }

    #[test]
    fn straight_line_leaves_stay_tier_0_but_containers_add_up() {
        // the c0m-tests/00_arithmetic.c0m program
        let (arena, root) = weigh("fn main() -> () { let x = 2 + 3; let y = x * 4; }");
        let let_x = first_statement(&arena, root);
        let add = child(&arena, let_x, 0); // untyped let: children = [value]
        approx(raw(&arena, add), 1.0);
        assert_eq!(tier(&arena, add), 0);
        approx(raw(&arena, root), 6.0);
        assert_eq!(tier(&arena, root), 1);
    }
}
