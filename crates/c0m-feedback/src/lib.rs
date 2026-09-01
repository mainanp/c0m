//! Increment 5 — The Feedback Loop.
//!
//! Post-execution: reads perf_event_open counters, maps hot addresses
//! back to AST source spans via DWARF, and updates pattern_memory.db
//! with an asymmetric EMA delta (0.15 reinforce / 0.10 penalize).
//! Persists to ~/.c0m/pattern_memory.db.

pub mod pattern_memory;

pub struct Hotspot {
    pub addr: u64,
    pub cycles_per_iter: f32,
}

pub struct PerfReport {
    pub hotspots: Vec<Hotspot>,
}

pub fn process_feedback(_report: &PerfReport, _mem: &mut pattern_memory::PatternMemory) {
    todo!("increment 5: port process_feedback() from the Chapter 3 pseudocode")
}
