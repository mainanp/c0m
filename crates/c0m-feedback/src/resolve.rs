//! Turns sampled machine addresses into lines of the emitted `.s` file.
//!
//! In feedback mode the emitter writes `.loc 1 <N>` before every
//! instruction, where N is that instruction's own line in the `.s` file.
//! The assembler turns those into a line table, and `addr2line` (from
//! binutils) reads it. The executable must be linked with `-no-pie` so the
//! sampled addresses are absolute.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// "c0m.s:12", "c0m.s:12 (discriminator 3)" -> Some(12); "??:0" -> None.
pub fn parse_line(answer: &str) -> Option<usize> {
    let (_, rest) = answer.rsplit_once(':')?;
    let digits: String = rest.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: usize = digits.parse().ok()?;
    if n == 0 {
        None
    } else {
        Some(n)
    }
}

/// asm line -> number of samples that landed on it. Samples outside the
/// program's own code (libc, the loader) resolve to nothing and are dropped.
pub fn resolve_lines(exe: &Path, ips: &[u64]) -> io::Result<HashMap<usize, u64>> {
    // few distinct addresses, many samples: resolve each address once
    let mut per_ip: BTreeMap<u64, u64> = BTreeMap::new();
    for &ip in ips {
        *per_ip.entry(ip).or_insert(0) += 1;
    }
    if per_ip.is_empty() {
        return Ok(HashMap::new());
    }

    let mut child = Command::new("addr2line")
        .arg("-e")
        .arg(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("cannot run addr2line (install binutils): {e}")))?;
    {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        for ip in per_ip.keys() {
            writeln!(stdin, "0x{ip:x}")?;
        }
    }
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let answers: Vec<&str> = text.lines().collect();
    if answers.len() != per_ip.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("addr2line answered {} of {} addresses", answers.len(), per_ip.len()),
        ));
    }

    let mut counts: HashMap<usize, u64> = HashMap::new();
    for ((_, n), answer) in per_ip.iter().zip(answers) {
        if let Some(line) = parse_line(answer) {
            *counts.entry(line).or_insert(0) += n;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addr2line_answers() {
        assert_eq!(parse_line("c0m.s:12"), Some(12));
        assert_eq!(parse_line("/tmp/x/c0m.s:7 (discriminator 2)"), Some(7));
        assert_eq!(parse_line("??:0"), None);
        assert_eq!(parse_line("??:?"), None);
        assert_eq!(parse_line("garbage"), None);
    }
}
