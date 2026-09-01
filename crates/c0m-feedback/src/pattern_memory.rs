use std::collections::HashMap;

/// Learned weight store, keyed by SourceSpan FNV1a hash.
/// This is pattern_memory.db — a lightweight binary file per Chapter 1.6
/// (no DBMS required).
pub struct PatternMemory {
    weights: HashMap<u64, f32>,
    baselines: HashMap<u64, f32>,
}

impl PatternMemory {
    pub fn new() -> Self {
        Self { weights: HashMap::new(), baselines: HashMap::new() }
    }

    pub fn get(&self, span_hash: u64) -> f32 {
        *self.weights.get(&span_hash).unwrap_or(&0.0)
    }

    pub fn update(&mut self, span_hash: u64, value: f32) {
        self.weights.insert(span_hash, value.max(0.0));
    }

    pub fn get_baseline(&self, span_hash: u64) -> f32 {
        *self.baselines.get(&span_hash).unwrap_or(&f32::MAX)
    }

    pub fn update_baseline(&mut self, span_hash: u64, cycles: f32) {
        self.baselines.insert(span_hash, cycles);
    }

    pub fn persist(&self, _path: &str) -> std::io::Result<()> {
        todo!("increment 5: binary serialize weights + baselines to disk")
    }

    pub fn load(_path: &str) -> std::io::Result<Self> {
        todo!("increment 5: binary deserialize, or Self::new() if absent")
    }
}

impl Default for PatternMemory {
    fn default() -> Self { Self::new() }
}
