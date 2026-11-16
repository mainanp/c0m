//! pattern_memory.db: the compiler's long-term opinions about nodes.
//!
//! One entry per node key: the learned hotness (same scale as a node's raw
//! weight) and the tier decided for it last time (for hysteresis).
//!
//! File layout (little-endian):
//!   "C0MP" | format version u32 | NodeKind canary u32 | count u32 |
//!   count x ( key u64 | learned f32 | tier u8 )
//!
//! Loading never panics: a missing, stale or damaged file just means
//! "start fresh", and the caller is told which.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use c0m_frontend::NodeKind;

/// Smoothing factors of the update rule (the EMA).
pub const ALPHA_UP: f32 = 0.15; // observed > learned: heat rises quickly
pub const ALPHA_DOWN: f32 = 0.10; // observed < learned: heat fades slowly
/// No learned weight may exceed this; a freak value cannot run away.
pub const MAX_WEIGHT: f32 = 1000.0;

const MAGIC: &[u8; 4] = b"C0MP";
/// Bump when the file layout changes, or when NodeKind variants are reordered.
pub const FORMAT_VERSION: u32 = 1;
const HEADER_BYTES: usize = 16;
const ENTRY_BYTES: usize = 13;

/// Changes whenever a NodeKind variant is inserted before the last one,
/// which would silently change every node key.
fn kind_canary() -> u32 {
    NodeKind::TypeRefMut as u32
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub learned: f32,
    pub tier: u8,
}

#[derive(Debug, PartialEq)]
pub enum LoadStatus {
    /// Loaded this many entries.
    Loaded(usize),
    /// No file yet: first run.
    Missing,
    /// File present but unusable (reason inside); memory starts empty.
    Discarded(String),
}

#[derive(Debug, Default, Clone)]
pub struct PatternMemory {
    entries: HashMap<u64, Entry>,
}

impl PatternMemory {
    pub fn new() -> Self {
        Self { entries: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: u64) -> bool {
        self.entries.contains_key(&key)
    }

    /// `None` = never seen. This is what the weighting engine consumes:
    /// an unseen node must stay unseen (combined = raw, no cold-start shrink).
    pub fn lookup(&self, key: u64) -> Option<f32> {
        self.entries.get(&key).map(|e| e.learned)
    }

    /// The tier recorded after the last feedback round, if any.
    pub fn prev_tier(&self, key: u64) -> Option<u8> {
        self.entries.get(&key).map(|e| e.tier)
    }

    /// The update rule. First sighting records the observation as is;
    /// afterwards the opinion moves part of the way toward it.
    pub fn observe(&mut self, key: u64, observed: f32) {
        if !observed.is_finite() {
            return; // never learn from garbage
        }
        let observed = observed.clamp(0.0, MAX_WEIGHT);
        match self.entries.get_mut(&key) {
            None => {
                self.entries.insert(key, Entry { learned: observed, tier: 0 });
            }
            Some(e) => {
                let alpha = if observed > e.learned { ALPHA_UP } else { ALPHA_DOWN };
                e.learned = (e.learned + alpha * (observed - e.learned)).clamp(0.0, MAX_WEIGHT);
            }
        }
    }

    /// Records the tier in effect. Does nothing for unknown keys.
    pub fn set_tier(&mut self, key: u64, tier: u8) {
        if let Some(e) = self.entries.get_mut(&key) {
            e.tier = tier.min(3);
        }
    }

    // ── file format ─────────────────────────────────────────────────

    pub fn encode(&self) -> Vec<u8> {
        let mut keys: Vec<u64> = self.entries.keys().copied().collect();
        keys.sort_unstable(); // same memory => same bytes
        let mut out = Vec::with_capacity(HEADER_BYTES + keys.len() * ENTRY_BYTES);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&kind_canary().to_le_bytes());
        out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for k in keys {
            let e = self.entries[&k];
            out.extend_from_slice(&k.to_le_bytes());
            out.extend_from_slice(&e.learned.to_le_bytes());
            out.push(e.tier);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_BYTES {
            return Err("file too short".to_string());
        }
        if &bytes[0..4] != MAGIC {
            return Err("not a pattern memory file (bad magic)".to_string());
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version != FORMAT_VERSION {
            return Err(format!("format version {version}, this build expects {FORMAT_VERSION}"));
        }
        let canary = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if canary != kind_canary() {
            return Err("written by a build with a different NodeKind layout".to_string());
        }
        let count = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let expected = count
            .checked_mul(ENTRY_BYTES)
            .and_then(|n| n.checked_add(HEADER_BYTES))
            .ok_or("entry count overflow")?;
        if bytes.len() != expected {
            return Err(format!(
                "size mismatch: header says {count} entries ({expected} bytes), file has {}",
                bytes.len()
            ));
        }
        let mut mem = Self::new();
        for i in 0..count {
            let o = HEADER_BYTES + i * ENTRY_BYTES;
            let key = u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
            let learned = f32::from_le_bytes(bytes[o + 8..o + 12].try_into().unwrap());
            let tier = bytes[o + 12];
            if !learned.is_finite() {
                return Err("non-finite weight in file".to_string());
            }
            if tier > 3 {
                return Err("invalid tier in file".to_string());
            }
            mem.entries.insert(key, Entry { learned: learned.clamp(0.0, MAX_WEIGHT), tier });
        }
        Ok(mem)
    }

    /// Never fails: the status says what happened.
    pub fn load(path: &Path) -> (Self, LoadStatus) {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Self::new(), LoadStatus::Missing),
            Err(e) => return (Self::new(), LoadStatus::Discarded(format!("cannot read: {e}"))),
        };
        match Self::decode(&bytes) {
            Ok(m) => {
                let n = m.len();
                (m, LoadStatus::Loaded(n))
            }
            Err(why) => (Self::new(), LoadStatus::Discarded(why)),
        }
    }

    /// Atomic save: write a temp file, then rename over the real one.
    pub fn persist(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                fs::create_dir_all(dir)?;
            }
        }
        let tmp = path.with_extension("db.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&self.encode())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }

    /// `~/.c0m/pattern_memory.db` (or `./.c0m/...` if HOME is unset).
    pub fn default_path() -> PathBuf {
        match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".c0m").join("pattern_memory.db"),
            None => PathBuf::from(".c0m").join("pattern_memory.db"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "{a} != {b}");
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("c0m_pm_{}_{}.db", std::process::id(), name))
    }

    // ── update rule ──────────────────────────────────────────────────

    #[test]
    fn unseen_is_none_not_zero() {
        let m = PatternMemory::new();
        assert_eq!(m.lookup(1), None);
        assert_eq!(m.prev_tier(1), None);
    }

    #[test]
    fn first_sighting_records_without_reward() {
        let mut m = PatternMemory::new();
        m.observe(1, 10.0);
        assert_eq!(m.lookup(1), Some(10.0));
    }

    #[test]
    fn heat_rises_at_015() {
        let mut m = PatternMemory::new();
        m.observe(1, 10.0);
        m.observe(1, 20.0);
        approx(m.lookup(1).unwrap(), 11.5);
        m.observe(1, 20.0);
        approx(m.lookup(1).unwrap(), 12.775);
    }

    #[test]
    fn heat_fades_at_010() {
        let mut m = PatternMemory::new();
        m.observe(1, 20.0);
        m.observe(1, 10.0);
        approx(m.lookup(1).unwrap(), 19.0);
        m.observe(1, 10.0);
        approx(m.lookup(1).unwrap(), 18.1);
    }

    #[test]
    fn constant_observation_converges() {
        let mut m = PatternMemory::new();
        m.observe(1, 10.0);
        for _ in 0..200 {
            m.observe(1, 50.0);
        }
        approx(m.lookup(1).unwrap(), 50.0);
    }

    #[test]
    fn values_are_clamped_and_garbage_ignored() {
        let mut m = PatternMemory::new();
        m.observe(1, 1e9);
        assert_eq!(m.lookup(1), Some(MAX_WEIGHT));
        m.observe(2, -5.0);
        assert_eq!(m.lookup(2), Some(0.0));
        m.observe(3, f32::NAN);
        assert_eq!(m.lookup(3), None);
    }

    #[test]
    fn set_tier_ignores_unknown_keys() {
        let mut m = PatternMemory::new();
        m.set_tier(9, 2);
        assert!(m.is_empty());
    }

    // ── file ─────────────────────────────────────────────────────────

    #[test]
    fn round_trip() {
        let mut m = PatternMemory::new();
        m.observe(7, 12.5);
        m.set_tier(7, 2);
        m.observe(u64::MAX, 3.25);
        let p = temp("round");
        m.persist(&p).unwrap();
        let (back, status) = PatternMemory::load(&p);
        assert_eq!(status, LoadStatus::Loaded(2));
        assert_eq!(back.lookup(7), Some(12.5));
        assert_eq!(back.prev_tier(7), Some(2));
        assert_eq!(back.lookup(u64::MAX), Some(3.25));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn encoding_is_deterministic() {
        let mut a = PatternMemory::new();
        let mut b = PatternMemory::new();
        for k in [5u64, 1, 9] {
            a.observe(k, k as f32);
        }
        for k in [9u64, 5, 1] {
            b.observe(k, k as f32);
        }
        assert_eq!(a.encode(), b.encode());
    }

    #[test]
    fn missing_file_means_fresh_start() {
        let (m, status) = PatternMemory::load(&temp("does_not_exist"));
        assert!(m.is_empty());
        assert_eq!(status, LoadStatus::Missing);
    }

    #[test]
    fn damaged_files_are_discarded_not_fatal() {
        let mut m = PatternMemory::new();
        m.observe(1, 5.0);
        let good = m.encode();

        assert!(PatternMemory::decode(b"garbage").is_err());
        assert!(PatternMemory::decode(&good[..good.len() - 1]).is_err()); // truncated
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(PatternMemory::decode(&trailing).is_err());
        let mut bad_version = good.clone();
        bad_version[4] = 99;
        assert!(PatternMemory::decode(&bad_version).is_err());
        let mut nan = good.clone();
        nan[HEADER_BYTES + 8..HEADER_BYTES + 12].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(PatternMemory::decode(&nan).is_err());

        let p = temp("corrupt");
        fs::write(&p, b"garbage").unwrap();
        let (m2, status) = PatternMemory::load(&p);
        assert!(m2.is_empty());
        assert!(matches!(status, LoadStatus::Discarded(_)));
        let _ = fs::remove_file(&p);
    }
}
