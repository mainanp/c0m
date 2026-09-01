//! Register Oracle: priority-weighted allocator. Dominant (high-weight)
//! regions claim registers first; weak regions self-spill. Function-scoped
//! by design (the "Action at a Distance" mitigation in Chapter 4).

pub const GP_REGISTER_COUNT: usize = 16;

pub struct RegOracle {
    pub used: [bool; GP_REGISTER_COUNT],
    pub priority: [f32; GP_REGISTER_COUNT],
    pub spill_depth: u8,
}

impl RegOracle {
    pub fn new() -> Self {
        Self { used: [false; GP_REGISTER_COUNT], priority: [0.0; GP_REGISTER_COUNT], spill_depth: 0 }
    }

    /// Reset between functions — no cross-function IR, no contamination.
    pub fn reset(&mut self) {
        self.used = [false; GP_REGISTER_COUNT];
        self.priority = [0.0; GP_REGISTER_COUNT];
        self.spill_depth = 0;
    }
}

impl Default for RegOracle {
    fn default() -> Self { Self::new() }
}
