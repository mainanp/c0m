//! Register Oracle: hands out x86-64 registers for the emitter.
//!
//! Rules of the house:
//!   - `rsp` and `rbp` run the stack frame: never handed out.
//!   - `rax` and `rdx` are scratch for the emitter itself (division,
//!     returns, jump tables, spilled operands): never handed out.
//!   - Everything else (12 registers) is the pool. Caller-saved registers
//!     are given out first because they cost nothing to use; callee-saved
//!     ones (`rbx`, `r12`-`r15`) must be saved/restored by the function
//!     prologue/epilogue, so the oracle remembers which ones were touched.
//!   - Variable "homes" (a register that holds a hot variable for its whole
//!     scope) come ONLY from the callee-saved set, because those survive
//!     `call` instructions.
//!
//! Function-scoped by design (the "Action at a Distance" mitigation in
//! Chapter 4): `reset()` runs at the start of every function.

pub const GP_REGISTER_COUNT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Reg {
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The register's name as the assembler spells it (Intel syntax).
    /// A `match`, so a new register can't be added without a name.
    pub const fn name(self) -> &'static str {
        match self {
            Reg::Rax => "rax",
            Reg::Rcx => "rcx",
            Reg::Rdx => "rdx",
            Reg::Rbx => "rbx",
            Reg::Rsp => "rsp",
            Reg::Rbp => "rbp",
            Reg::Rsi => "rsi",
            Reg::Rdi => "rdi",
            Reg::R8 => "r8",
            Reg::R9 => "r9",
            Reg::R10 => "r10",
            Reg::R11 => "r11",
            Reg::R12 => "r12",
            Reg::R13 => "r13",
            Reg::R14 => "r14",
            Reg::R15 => "r15",
        }
    }

    /// System V: a function must give these back unchanged.
    pub const fn is_callee_saved(self) -> bool {
        matches!(self, Reg::Rbx | Reg::Rbp | Reg::R12 | Reg::R13 | Reg::R14 | Reg::R15)
    }
}

/// Order temporaries are handed out in: free-to-use registers first.
pub const TEMP_ORDER: [Reg; 12] = [
    Reg::Rcx, Reg::Rsi, Reg::Rdi, Reg::R8, Reg::R9, Reg::R10, Reg::R11,
    Reg::Rbx, Reg::R12, Reg::R13, Reg::R14, Reg::R15,
];

/// Registers a variable may live in for a whole scope (survive calls).
pub const HOME_ORDER: [Reg; 5] = [Reg::Rbx, Reg::R12, Reg::R13, Reg::R14, Reg::R15];

pub struct RegOracle {
    pub used: [bool; GP_REGISTER_COUNT],
    /// The node weight each busy register was handed out for. Kept for
    /// diagnostics: the weight's real effect is the gate on variable homes
    /// (see `HOME_MIN_TIER` in lib.rs), because a busy register can never be
    /// taken back mid-function without a liveness analysis.
    pub priority: [f32; GP_REGISTER_COUNT],
    /// Operands currently parked on the stack because registers ran short.
    pub spill_depth: u8,
    /// How many operands were parked in total (statistics).
    pub spills_total: u32,
    touched: [bool; GP_REGISTER_COUNT],
}

impl RegOracle {
    pub fn new() -> Self {
        Self {
            used: [false; GP_REGISTER_COUNT],
            priority: [0.0; GP_REGISTER_COUNT],
            spill_depth: 0,
            spills_total: 0,
            touched: [false; GP_REGISTER_COUNT],
        }
    }

    /// Reset between functions — no cross-function IR, no contamination.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn take(&mut self, r: Reg, priority: f32) -> Reg {
        self.used[r.index()] = true;
        self.priority[r.index()] = priority;
        if r.is_callee_saved() {
            self.touched[r.index()] = true;
        }
        r
    }

    /// A register for a short-lived value. `None` means the pool is empty.
    pub fn alloc_temp(&mut self, priority: f32) -> Option<Reg> {
        let r = TEMP_ORDER.iter().copied().find(|r| !self.used[r.index()])?;
        Some(self.take(r, priority))
    }

    /// A callee-saved register to be a variable's home. `None` = none left.
    pub fn alloc_home(&mut self, priority: f32) -> Option<Reg> {
        let r = HOME_ORDER.iter().copied().find(|r| !self.used[r.index()])?;
        Some(self.take(r, priority))
    }

    pub fn free(&mut self, r: Reg) {
        debug_assert!(self.used[r.index()], "freeing a register that is not in use: {r:?}");
        self.used[r.index()] = false;
        self.priority[r.index()] = 0.0;
    }

    pub fn is_used(&self, r: Reg) -> bool {
        self.used[r.index()]
    }

    /// How many pool registers are free right now.
    pub fn free_count(&self) -> usize {
        TEMP_ORDER.iter().filter(|r| !self.used[r.index()]).count()
    }

    /// Busy registers a `call` would destroy (caller-saved ones).
    pub fn live_caller_saved(&self) -> Vec<Reg> {
        TEMP_ORDER
            .iter()
            .copied()
            .filter(|r| !r.is_callee_saved() && self.used[r.index()])
            .collect()
    }

    /// Callee-saved registers this function touched: the prologue must save
    /// them and the epilogue must restore them.
    pub fn callee_saved_touched(&self) -> Vec<Reg> {
        HOME_ORDER.iter().copied().filter(|r| self.touched[r.index()]).collect()
    }

    pub fn note_spill(&mut self) {
        self.spill_depth += 1;
        self.spills_total += 1;
    }

    pub fn end_spill(&mut self) {
        self.spill_depth -= 1;
    }
}

impl Default for RegOracle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temps_come_out_caller_saved_first() {
        let mut o = RegOracle::new();
        assert_eq!(o.alloc_temp(0.0), Some(Reg::Rcx));
        assert_eq!(o.alloc_temp(0.0), Some(Reg::Rsi));
        assert_eq!(o.alloc_temp(0.0), Some(Reg::Rdi));
        assert!(o.callee_saved_touched().is_empty());
    }

    #[test]
    fn scratch_and_frame_registers_are_never_handed_out() {
        let mut o = RegOracle::new();
        let mut all = Vec::new();
        while let Some(r) = o.alloc_temp(0.0) {
            all.push(r);
        }
        assert_eq!(all.len(), 12);
        for banned in [Reg::Rax, Reg::Rdx, Reg::Rsp, Reg::Rbp] {
            assert!(!all.contains(&banned), "{banned:?} must stay out of the pool");
        }
    }

    #[test]
    fn exhaustion_then_free_makes_room_again() {
        let mut o = RegOracle::new();
        while o.alloc_temp(0.0).is_some() {}
        assert_eq!(o.free_count(), 0);
        o.free(Reg::Rsi);
        assert_eq!(o.free_count(), 1);
        assert_eq!(o.alloc_temp(0.0), Some(Reg::Rsi));
    }

    #[test]
    fn homes_are_callee_saved_only_and_run_out_after_five() {
        let mut o = RegOracle::new();
        let homes: Vec<Reg> = std::iter::from_fn(|| o.alloc_home(2.0)).collect();
        assert_eq!(homes, vec![Reg::Rbx, Reg::R12, Reg::R13, Reg::R14, Reg::R15]);
        assert_eq!(o.callee_saved_touched().len(), 5);
    }

    #[test]
    fn temps_fall_back_to_callee_saved_and_mark_them_touched() {
        let mut o = RegOracle::new();
        for _ in 0..7 {
            o.alloc_temp(0.0);
        }
        assert!(o.callee_saved_touched().is_empty());
        assert_eq!(o.alloc_temp(0.0), Some(Reg::Rbx));
        assert_eq!(o.callee_saved_touched(), vec![Reg::Rbx]);
    }

    #[test]
    fn live_caller_saved_lists_only_busy_caller_saved() {
        let mut o = RegOracle::new();
        let a = o.alloc_temp(0.0).unwrap();
        let _h = o.alloc_home(1.0).unwrap();
        assert_eq!(o.live_caller_saved(), vec![a]);
    }

    #[test]
    fn priority_is_recorded_and_reset_clears_everything() {
        let mut o = RegOracle::new();
        let r = o.alloc_home(2.5).unwrap();
        assert_eq!(o.priority[r.index()], 2.5);
        o.reset();
        assert!(!o.is_used(r));
        assert!(o.callee_saved_touched().is_empty());
        assert_eq!(o.free_count(), 12);
    }

    #[test]
    fn every_register_has_a_distinct_name() {
        let names: std::collections::HashSet<_> = [
            Reg::Rax, Reg::Rcx, Reg::Rdx, Reg::Rbx, Reg::Rsp, Reg::Rbp, Reg::Rsi, Reg::Rdi,
            Reg::R8, Reg::R9, Reg::R10, Reg::R11, Reg::R12, Reg::R13, Reg::R14, Reg::R15,
        ]
        .iter()
        .map(|r| r.name())
        .collect();
        assert_eq!(names.len(), GP_REGISTER_COUNT);
    }
}
