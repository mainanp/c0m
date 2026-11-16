//! Increment 5 end-to-end: real compile, real run under perf_event_open,
//! real learning. Skipped (with a note) if gcc, addr2line or perf sampling
//! is unavailable.

use std::collections::HashMap;
use std::fs;
use std::process::Command;

use c0m_dos::DosStore;
use c0m_emitter::emit_program_with;
use c0m_feedback::pattern_memory::PatternMemory;
use c0m_feedback::{attribute, process_feedback, resolve, sampler};
use c0m_frontend::{parse_source, NodeId, NodeKind};
use c0m_weighting::{analyze, no_history};

const WORKLOAD: &str = include_str!("../../c0m-tests/07_workload.c0m");

fn tool_ok(name: &str) -> bool {
    Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

#[test]
fn real_sampling_teaches_the_memory() {
    if !tool_ok("gcc") || !tool_ok("addr2line") {
        eprintln!("gcc/addr2line not found: skipping feedback e2e");
        return;
    }
    let (mut arena, root) = parse_source(WORKLOAD, 0).expect("parse");
    analyze(&mut arena, root, &no_history);
    let dos = DosStore::build(&arena, root);
    let emitted = emit_program_with(&arena, root, &dos, true).expect("emit");

    let dir = std::env::temp_dir().join(format!("c0m_fb_e2e_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let (s, exe) = (dir.join("prog.s"), dir.join("prog"));
    fs::write(&s, &emitted.ctx.output).unwrap();
    let built = Command::new("gcc").arg("-no-pie").arg(&s).arg("-o").arg(&exe).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));

    let ips = match sampler::profile(&exe, &[], 3, 10_000) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sampler unavailable ({e}): skipping");
            let _ = fs::remove_dir_all(&dir);
            return;
        }
    };
    let lines = resolve::resolve_lines(&exe, &ips).expect("resolve");
    let line_to_key: HashMap<usize, u64> = emitted.ctx.trace.iter().map(|t| (t.line, t.key)).collect();
    let report = attribute(&lines, &line_to_key, &arena);
    assert!(report.attributed() > 0, "no samples reached any AST node");

    let mut mem = PatternMemory::new();
    let summary = process_feedback(&mut arena, root, &report, &mut mem, 30);
    assert!(summary.learned_functions >= 1);

    // some loop must have been learned about
    let learned_a_loop = (0..arena.len() as u32).map(NodeId).any(|id| {
        let n = arena.get(id);
        n.kind == NodeKind::Loop && mem.lookup(n.key()).map_or(false, |w| w > 0.0)
    });
    assert!(learned_a_loop);
    let _ = fs::remove_dir_all(&dir);
}
