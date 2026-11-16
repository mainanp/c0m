//! Increment 5: The Feedback Loop.
//!
//! After a compiled program has run:
//!   1. `sampler`  : where did the CPU spend its cycles? (instruction pointers)
//!   2. `resolve`  : which line of the emitted assembly is that?
//!   3. `attribute`: which AST node owns that line? (via the emitter's trace)
//!   4. `process_feedback`: add up samples per subtree, turn them into an
//!      `observed` weight, nudge the learned weights, re-decide tiers,
//!      and record them.
//!
//! Safety argument: this crate only ever writes weights and tiers. Tiers
//! only unlock DOS strategies, and every strategy computes the same result
//! as `Direct`, so a wrong measurement can make code slower, never wrong.
//!
//! Scope: learns HOTNESS only. Learning which strategy is best is future work.

pub mod pattern_memory;
pub mod resolve;
pub mod sampler;

use std::collections::HashMap;

use c0m_frontend::{Arena, NodeId, NodeKind, Payload};
use c0m_weighting::analyze_with_memory;
use pattern_memory::PatternMemory;

/// A function with fewer samples than this is not learned from (noise).
pub const DEFAULT_MIN_SAMPLES: u64 = 30;

/// The sensor's output after attribution: samples per node, by arena index.
#[derive(Debug, Clone, Default)]
pub struct PerfReport {
    /// `self_samples[id.0]` = samples on instructions this node owns itself.
    pub self_samples: Vec<u64>,
    /// Samples that matched no node (libc, loader, unknown lines).
    pub unattributed: u64,
}

impl PerfReport {
    pub fn new(node_count: usize) -> Self {
        Self { self_samples: vec![0; node_count], unattributed: 0 }
    }

    pub fn add(&mut self, id: NodeId, n: u64) {
        self.self_samples[id.0 as usize] += n;
    }

    pub fn attributed(&self) -> u64 {
        self.self_samples.iter().sum()
    }
}

/// asm line -> samples, plus the emitter's line -> node key table, becomes
/// samples per node.
pub fn attribute(
    lines: &HashMap<usize, u64>,
    line_to_key: &HashMap<usize, u64>,
    arena: &Arena,
) -> PerfReport {
    let mut key_to_id: HashMap<u64, NodeId> = HashMap::new();
    for i in 0..arena.len() {
        let id = NodeId(i as u32);
        key_to_id.insert(arena.get(id).key(), id);
    }
    let mut report = PerfReport::new(arena.len());
    for (line, &count) in lines {
        match line_to_key.get(line).and_then(|k| key_to_id.get(k)) {
            Some(&id) => report.add(id, count),
            None => report.unattributed += count,
        }
    }
    report
}

/// One node's before/after, for the printed table and the experiment.
#[derive(Debug, Clone)]
pub struct NodeUpdate {
    pub key: u64,
    pub kind: NodeKind,
    pub line: u32,
    pub col: u32,
    pub observed: f32,
    pub learned: f32,
    pub combined: f32,
    pub tier_before: u8,
    pub tier_after: u8,
}

#[derive(Debug, Default)]
pub struct FeedbackSummary {
    pub updates: Vec<NodeUpdate>,
    /// (function name, samples) for functions skipped as too quiet.
    pub skipped: Vec<(String, u64)>,
    pub learned_functions: usize,
}

/// Inclusive counts: a node's own samples plus everything below it.
fn rollup(arena: &Arena, id: NodeId, self_s: &[u64], out: &mut [u64]) -> u64 {
    let mut total = self_s.get(id.0 as usize).copied().unwrap_or(0);
    for &c in &arena.get(id).children {
        total += rollup(arena, c, self_s, out);
    }
    out[id.0 as usize] = total;
    total
}

fn collect(arena: &Arena, id: NodeId, out: &mut Vec<NodeId>) {
    out.push(id);
    for &c in &arena.get(id).children {
        collect(arena, c, out);
    }
}

/// The compare-and-adjust step.
///
/// `arena` must already be weighted (raw weights are the scale for
/// `observed`). Per function:
///   observed(node) = (samples under node / samples in function) x raw(function)
/// Then each known node's learned weight moves toward `observed` (EMA),
/// the tree is re-weighted with the new memory, and the tiers now in effect
/// are recorded for hysteresis.
pub fn process_feedback(
    arena: &mut Arena,
    root: NodeId,
    report: &PerfReport,
    mem: &mut PatternMemory,
    min_samples: u64,
) -> FeedbackSummary {
    let mut summary = FeedbackSummary::default();
    let mut inclusive = vec![0u64; arena.len()];
    let funcs: Vec<NodeId> = arena.get(root).children.clone();
    for &f in &funcs {
        rollup(arena, f, &report.self_samples, &mut inclusive);
    }

    let mut touched: Vec<(NodeId, f32, u8)> = Vec::new(); // (node, observed, tier before)
    for &f in &funcs {
        let total = inclusive[f.0 as usize];
        let name = match &arena.get(f).payload {
            Some(Payload::Ident(n)) => n.clone(),
            _ => "?".to_string(),
        };
        if total < min_samples {
            summary.skipped.push((name, total));
            continue;
        }
        summary.learned_functions += 1;
        let f_raw = arena.get(f).weight.raw;

        let mut nodes = Vec::new();
        collect(arena, f, &mut nodes);
        for id in nodes {
            let count = inclusive[id.0 as usize];
            let key = arena.get(id).key();
            // No samples and no opinion yet: no evidence, skip. A node that
            // already has an opinion and went quiet decays toward 0.
            if count == 0 && !mem.contains(key) {
                continue;
            }
            let observed = (count as f32 / total as f32) * f_raw;
            mem.observe(key, observed);
            touched.push((id, observed, arena.get(id).weight.dos_tier));
        }
    }

    // Re-decide tiers with the updated memory (hysteresis uses the old tiers).
    {
        let m: &PatternMemory = &*mem;
        analyze_with_memory(arena, root, &|k| m.lookup(k), &|k| m.prev_tier(k));
    }
    // Record the tiers now in effect (not for @N-overridden nodes).
    for i in 0..arena.len() {
        let n = arena.get(NodeId(i as u32));
        if n.tier_override.is_none() && mem.contains(n.key()) {
            mem.set_tier(n.key(), n.weight.dos_tier);
        }
    }

    for (id, observed, before) in touched {
        let n = arena.get(id);
        let key = n.key();
        summary.updates.push(NodeUpdate {
            key,
            kind: n.kind,
            line: n.span.line,
            col: n.span.col,
            observed,
            learned: mem.lookup(key).unwrap_or(0.0),
            combined: n.weight.combined,
            tier_before: before,
            tier_after: n.weight.dos_tier,
        });
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use c0m_frontend::parse_source;
    use c0m_weighting::{analyze, no_history};

    fn weighed(src: &str) -> (Arena, NodeId) {
        let (mut arena, root) = parse_source(src, 0).expect("should parse");
        analyze(&mut arena, root, &no_history);
        (arena, root)
    }

    fn first_of(arena: &Arena, kind: NodeKind) -> NodeId {
        (0..arena.len() as u32)
            .map(NodeId)
            .find(|&i| arena.get(i).kind == kind)
            .expect("node of that kind")
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "{a} != {b}");
    }

    /// 20 lets (raw 1.0 each) + an empty loop (raw 3.0): the loop is
    /// structurally cheap (tier 0) inside a structurally big function (raw 35).
    fn big_function() -> String {
        format!("fn f() -> () {{ {} for i in 0..4 {{ }} }}", "let a: i64 = 1; ".repeat(20))
    }

    #[test]
    fn attribution_routes_lines_to_nodes() {
        let (arena, _root) = weighed("fn f() -> () { for i in 0..4 { } }");
        let lp = first_of(&arena, NodeKind::Loop);
        let lines: HashMap<usize, u64> = [(7, 4), (9, 2)].into();
        let line_to_key: HashMap<usize, u64> = [(7, arena.get(lp).key())].into();
        let r = attribute(&lines, &line_to_key, &arena);
        assert_eq!(r.self_samples[lp.0 as usize], 4);
        assert_eq!(r.unattributed, 2);
    }

    #[test]
    fn a_node_is_credited_for_everything_beneath_it() {
        // samples only on the `let` INSIDE the loop; the loop's own
        // cmp/jge/add/jmp got none, yet the loop must be credited.
        let (mut arena, root) = weighed("fn f() -> () { for i in 0..4 { let a: i64 = 1; } }");
        let lp = first_of(&arena, NodeKind::Loop);
        let the_let = first_of(&arena, NodeKind::Let);
        let mut report = PerfReport::new(arena.len());
        report.add(the_let, 50);
        let mut mem = PatternMemory::new();
        process_feedback(&mut arena, root, &report, &mut mem, 10);
        // function raw = 6.5 and all samples are under the loop => observed 6.5
        approx(mem.lookup(arena.get(lp).key()).unwrap(), 6.5);
    }

    #[test]
    fn a_hot_report_raises_a_tier_and_the_next_compile_agrees() {
        let src = big_function();
        let (mut arena, root) = weighed(&src);
        let lp = first_of(&arena, NodeKind::Loop);
        assert_eq!(arena.get(lp).weight.dos_tier, 0);

        let mut report = PerfReport::new(arena.len());
        report.add(lp, 100);
        let mut mem = PatternMemory::new();
        let s = process_feedback(&mut arena, root, &report, &mut mem, 10);

        let key = arena.get(lp).key();
        approx(mem.lookup(key).unwrap(), 35.0); // first sighting: recorded as is
        assert_eq!(arena.get(lp).weight.dos_tier, 2); // 0.6*3 + 0.4*35 = 15.8
        assert_eq!(mem.prev_tier(key), Some(2));
        assert!(s.updates.iter().any(|u| u.key == key && u.tier_before == 0 && u.tier_after == 2));

        // the next compile, reading only the memory, reaches the same tier
        let (mut arena2, root2) = parse_source(&src, 0).expect("parse");
        analyze_with_memory(&mut arena2, root2, &|k| mem.lookup(k), &|k| mem.prev_tier(k));
        assert_eq!(arena2.get(first_of(&arena2, NodeKind::Loop)).weight.dos_tier, 2);
    }

    #[test]
    fn a_node_that_goes_quiet_decays_slowly() {
        let src = big_function();
        let (mut arena, root) = weighed(&src);
        let lp = first_of(&arena, NodeKind::Loop);
        let the_let = first_of(&arena, NodeKind::Let);
        let mut mem = PatternMemory::new();

        let mut hot = PerfReport::new(arena.len());
        hot.add(lp, 100);
        process_feedback(&mut arena, root, &hot, &mut mem, 10);

        let (mut arena, root) = weighed(&src); // next compile's fresh weights
        let mut elsewhere = PerfReport::new(arena.len());
        elsewhere.add(the_let, 100);
        process_feedback(&mut arena, root, &elsewhere, &mut mem, 10);

        // the loop had an opinion (35) and now saw 0 samples: 35 + 0.10*(0-35)
        approx(mem.lookup(arena.get(lp).key()).unwrap(), 31.5);
    }

    #[test]
    fn quiet_functions_are_not_learned_from() {
        let (mut arena, root) = weighed(&big_function());
        let lp = first_of(&arena, NodeKind::Loop);
        let mut report = PerfReport::new(arena.len());
        report.add(lp, 5);
        let mut mem = PatternMemory::new();
        let s = process_feedback(&mut arena, root, &report, &mut mem, 10);
        assert!(mem.is_empty());
        assert_eq!(s.skipped.len(), 1);
        assert_eq!(s.learned_functions, 0);
    }
}
