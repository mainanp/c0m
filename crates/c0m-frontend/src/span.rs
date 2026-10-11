/// FNV-1a constants. FNV-1a is a tiny hash: start from a fixed number,
/// then for every byte, XOR it in and multiply by a fixed prime.
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// Feeds more bytes into a running FNV-1a hash and returns the new hash.
/// `pub(crate)` = visible inside this crate only; ast.rs needs it, the
/// outside world does not.
pub(crate) fn fnv_mix(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= byte as u64;
        // wrapping_mul: overflow is the whole point of a hash, but plain
        // `*` would panic on overflow in debug builds.
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Byte-exact source location. This is the feedback loop's unique key:
/// stable across recompiles as long as the source at this location is
/// unchanged. See Chapter 3 (Stage 1B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub file_hash: u64,
    pub line: u32,
    pub col: u32,
    pub byte_offset: u32,
    pub length: u32,
}

impl SourceSpan {
    /// FNV-1a over (file_hash, byte_offset, length).
    ///
    /// `line` and `col` are deliberately NOT hashed: byte_offset already
    /// pins the exact spot, and line/col are only a human-friendly view
    /// of it. `length` IS hashed so a parent and its first child, which
    /// start at the same byte, get different hashes.
    pub fn hash(&self) -> u64 {
        let mut h = FNV_OFFSET;
        h = fnv_mix(h, &self.file_hash.to_le_bytes());
        h = fnv_mix(h, &self.byte_offset.to_le_bytes());
        h = fnv_mix(h, &self.length.to_le_bytes());
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(offset: u32, length: u32) -> SourceSpan {
        SourceSpan { file_hash: 7, line: 1, col: 1, byte_offset: offset, length }
    }

    #[test]
    fn same_span_gives_same_hash() {
        assert_eq!(span(4, 5).hash(), span(4, 5).hash());
    }

    #[test]
    fn parent_and_first_child_differ() {
        // `2 + 3` and its left `2` start at the same byte.
        assert_ne!(span(10, 5).hash(), span(10, 1).hash());
    }

    #[test]
    fn different_files_differ() {
        let a = span(4, 5);
        let b = SourceSpan { file_hash: 8, ..a };
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn line_and_col_are_for_humans_only() {
        let a = span(4, 5);
        let b = SourceSpan { line: 9, col: 9, ..a };
        assert_eq!(a.hash(), b.hash());
    }
}
