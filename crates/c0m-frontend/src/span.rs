/// Byte-exact source location. This is the feedback loop's unique key:
/// stable across recompiles as long as the source at this location is
/// unchanged. See Chapter 3 (Stage 1B) — FNV1a(file_hash XOR line XOR col).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub file_hash: u64,
    pub line: u32,
    pub col: u32,
    pub byte_offset: u32,
    pub length: u32,
}

impl SourceSpan {
    /// FNV-1a hash used as the pattern_memory.db key.
    pub fn hash(&self) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;
        let mixed = self.file_hash ^ (self.line as u64) ^ (self.col as u64);
        let mut h = FNV_OFFSET;
        for byte in mixed.to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        h
    }
}
