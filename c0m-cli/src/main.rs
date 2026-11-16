//! c0m: the compiler driver.
//!
//!   Suda source -> AST -> weighted AST (uses pattern memory) -> DOS ->
//!   x86-64 assembly + traceability records -> (optional) feedback loop.
//!
//! Usage: c0m <source.c0m> [flags]
//!
//! Inspection flags:
//!   --dump-tokens --dump-ast --dump-weights --dump-strategies
//!   --trace-emit        print each instruction with its AST node
//!   --no-dos            skip weighting (everything tier 0)
//!   -o <file>           write the assembly to a file instead of stdout
//!
//! Memory flags:
//!   --memory <file>     pattern memory file (default ~/.c0m/pattern_memory.db)
//!   --no-memory         ignore the memory entirely
//!
//! Feedback flags (increment 5):
//!   --feedback          compile, build with gcc, run under the sampler,
//!                       learn, and save the memory
//!   --runs <n>          how many times to run the program (default 5)
//!   --period <cycles>   cycles between samples (default 10000)
//!   --min-samples <n>   skip functions with fewer samples (default 30)
//!   --driver <file.c>   C file with `main`, for programs without a Suda `main`

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use c0m_dos::DosStore;
use c0m_emitter::{emit_program_with, EmitResult};
use c0m_feedback::pattern_memory::{LoadStatus, PatternMemory};
use c0m_feedback::{attribute, process_feedback, resolve, sampler, FeedbackSummary, PerfReport, DEFAULT_MIN_SAMPLES};
use c0m_frontend::dump::dump_tree;
use c0m_frontend::{parse_source, Arena, NodeId, NodeKind};
use c0m_weighting::analyze_with_memory;

/// FNV-1a over bytes: turns the file *path* into the `file_hash` that
/// SourceSpan mixes into every node's identity.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const VALUE_FLAGS: [&str; 6] = ["-o", "--memory", "--runs", "--period", "--min-samples", "--driver"];

fn opt_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn opt_num<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    opt_value(args, flag).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn input_path(args: &[String]) -> Option<String> {
    args.iter()
        .enumerate()
        .skip(1)
        .find(|(i, a)| !a.starts_with('-') && !VALUE_FLAGS.contains(&args[i - 1].as_str()))
        .map(|(_, a)| a.clone())
}

struct FbOpts {
    runs: u32,
    period: u64,
    min_samples: u64,
    driver: Option<String>,
}

/// Build the emitted assembly, run it under the sampler, learn from it.
fn run_feedback(
    arena: &mut Arena,
    root: NodeId,
    emitted: &EmitResult,
    memory: &mut PatternMemory,
    o: &FbOpts,
) -> Result<(FeedbackSummary, PerfReport), String> {
    let dir = env::temp_dir().join(format!("c0m_fb_{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let result = (|| -> Result<(FeedbackSummary, PerfReport), String> {
        let asm = dir.join("prog.s");
        let exe = dir.join("prog");
        fs::write(&asm, &emitted.ctx.output).map_err(|e| format!("cannot write assembly: {e}"))?;

        // -no-pie: sampled addresses are then absolute, no load base needed
        let mut cmd = Command::new("gcc");
        cmd.arg("-no-pie");
        if let Some(d) = &o.driver {
            cmd.arg(d);
        }
        cmd.arg(&asm).arg("-o").arg(&exe);
        let built = cmd.output().map_err(|e| format!("cannot run gcc: {e}"))?;
        if !built.status.success() {
            return Err(format!(
                "gcc failed (does the program have a `main`? try --driver):\n{}",
                String::from_utf8_lossy(&built.stderr)
            ));
        }

        let ips = sampler::profile(&exe, &[], o.runs, o.period).map_err(|e| e.to_string())?;
        if ips.is_empty() {
            return Err("no samples captured: lower --period or raise --runs".to_string());
        }
        let lines = resolve::resolve_lines(&exe, &ips).map_err(|e| e.to_string())?;

        let line_to_key: HashMap<usize, u64> =
            emitted.ctx.trace.iter().map(|t| (t.line, t.key)).collect();
        let report = attribute(&lines, &line_to_key, arena);
        eprintln!(
            "samples: {} captured, {} attributed to AST nodes",
            ips.len(),
            report.attributed()
        );
        let summary = process_feedback(arena, root, &report, memory, o.min_samples);
        Ok((summary, report))
    })();

    let _ = fs::remove_dir_all(&dir);
    result
}

fn print_summary(s: &FeedbackSummary) {
    for (name, n) in &s.skipped {
        eprintln!("skipped `{name}`: only {n} samples");
    }
    eprintln!("learned from {} function(s)", s.learned_functions);
    let mut rows: Vec<_> = s
        .updates
        .iter()
        .filter(|u| {
            matches!(
                u.kind,
                NodeKind::Loop | NodeKind::ParallelLoop | NodeKind::If | NodeKind::Match | NodeKind::Call | NodeKind::FuncDef
            )
        })
        .collect();
    rows.sort_by(|a, b| b.observed.partial_cmp(&a.observed).unwrap_or(std::cmp::Ordering::Equal));
    for u in rows {
        eprintln!(
            "{:<12} @ {:>3}:{:<3} observed {:>6.1}  learned {:>6.1}  combined {:>6.1}  T{} -> T{}",
            format!("{:?}", u.kind),
            u.line,
            u.col,
            u.observed,
            u.learned,
            u.combined,
            u.tier_before,
            u.tier_after
        );
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let has = |f: &str| args.iter().any(|a| a == f);

    let out_path = opt_value(&args, "-o");
    let Some(input_path) = input_path(&args) else {
        eprintln!("usage: c0m <source.c0m> [--dump-ast] [--trace-emit] [--feedback] [-o out.s]");
        return ExitCode::FAILURE;
    };
    let (dump_ast, trace_emit, no_dos, feedback) =
        (has("--dump-ast"), has("--trace-emit"), has("--no-dos"), has("--feedback"));

    let source = match fs::read_to_string(&input_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {input_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Hash the path, not the contents: node identities must stay stable
    // when the file is edited, or learned weights would be thrown away.
    let file_hash = fnv1a(input_path.as_bytes());

    if has("--dump-tokens") {
        match c0m_frontend::lexer::tokenize(&source, file_hash) {
            Ok(toks) => {
                for (t, s) in toks {
                    println!("{}:{}  {:?}", s.line, s.col, t);
                }
            }
            Err(e) => eprintln!("{}:{}: error: {}", e.span.line, e.span.col, e.message),
        }
        return ExitCode::SUCCESS;
    }

    // Pattern memory (read here; written only by --feedback).
    let mem_path: Option<PathBuf> = if has("--no-memory") {
        None
    } else {
        Some(opt_value(&args, "--memory").map(PathBuf::from).unwrap_or_else(PatternMemory::default_path))
    };
    let mut memory = PatternMemory::new();
    if let Some(p) = &mem_path {
        let (m, status) = PatternMemory::load(p);
        if let LoadStatus::Discarded(why) = &status {
            eprintln!("warning: ignoring {}: {why}", p.display());
        }
        memory = m;
    }

    let (mut arena, root) = match parse_source(&source, file_hash) {
        Ok(ok) => ok,
        Err(e) => {
            eprintln!("{}:{}:{}: error: {}", input_path, e.span.line, e.span.col, e.message);
            return ExitCode::FAILURE;
        }
    };

    if dump_ast {
        print!("{}", dump_tree(&arena, root));
        return ExitCode::SUCCESS;
    }

    // Increments 2 + 5: weigh the tree, using what past runs taught us.
    if !no_dos {
        analyze_with_memory(&mut arena, root, &|k| memory.lookup(k), &|k| memory.prev_tier(k));
    }
    if has("--dump-weights") {
        print!("{}", c0m_frontend::dump::dump_tree_weighted(&arena, root));
        return ExitCode::SUCCESS;
    }

    // Increment 3: index the functions the DOS needs for inlining.
    let dos = DosStore::build(&arena, root);

    // Increment 4: emit (with .loc lines when we are going to profile).
    let emitted = match emit_program_with(&arena, root, &dos, feedback) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}:{}:{}: error: {}", input_path, e.span.line, e.span.col, e.message);
            return ExitCode::FAILURE;
        }
    };

    if has("--dump-strategies") {
        for (id, s) in emitted.strategies.iter().filter(|(_, s)| *s != c0m_dos::EmitStrategy::Direct) {
            let n = arena.get(id);
            println!("{:?} @ {}:{} -> {:?}", n.kind, n.span.line, n.span.col, s);
        }
        return ExitCode::SUCCESS;
    }

    let text = if trace_emit { emitted.ctx.annotated_listing() } else { emitted.ctx.output.clone() };
    match &out_path {
        Some(p) => {
            if let Err(e) = fs::write(p, text) {
                eprintln!("error writing {p}: {e}");
                return ExitCode::FAILURE;
            }
        }
        None => print!("{text}"),
    }

    // Increment 5: the feedback loop.
    if feedback {
        let Some(p) = &mem_path else {
            eprintln!("--feedback needs a pattern memory file (drop --no-memory)");
            return ExitCode::FAILURE;
        };
        if no_dos {
            eprintln!("--feedback cannot be combined with --no-dos");
            return ExitCode::FAILURE;
        }
        let opts = FbOpts {
            runs: opt_num(&args, "--runs", 5u32),
            period: opt_num(&args, "--period", 10_000u64),
            min_samples: opt_num(&args, "--min-samples", DEFAULT_MIN_SAMPLES),
            driver: opt_value(&args, "--driver"),
        };
        match run_feedback(&mut arena, root, &emitted, &mut memory, &opts) {
            Ok((summary, _report)) => {
                print_summary(&summary);
                match memory.persist(p) {
                    Ok(()) => eprintln!("pattern memory saved to {} ({} entries)", p.display(), memory.len()),
                    Err(e) => {
                        eprintln!("error saving {}: {e}", p.display());
                        return ExitCode::FAILURE;
                    }
                }
            }
            Err(e) => {
                eprintln!("feedback failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
