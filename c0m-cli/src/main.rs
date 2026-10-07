//! c0m — the compiler driver.
//!
//! Wires increments 1-5 into one pipeline:
//!   Suda source -> AST -> weighted AST -> DOS-gated strategy selection
//!   -> x86-64 assembly + traceability records -> (later) feedback loop.
//!
//! Flags:
//!   --dump-ast     print the AST and stop (increment 1 inspection)
//!   --trace-emit   print each instruction with its originating AST node
//!                  and source span (wired in increment 4)

use std::env;
use std::fs;
use std::process::ExitCode;

use c0m_frontend::dump::dump_tree;
use c0m_frontend::parse_source;

/// FNV-1a over bytes. Used to turn the file *path* into the file_hash that
/// SourceSpan mixes into every node's identity.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let Some(input_path) = args.iter().skip(1).find(|a| !a.starts_with("--")) else {
        eprintln!("usage: c0m <source.c0m> [--dump-ast] [--trace-emit]");
        return ExitCode::FAILURE;
    };
    let dump_ast = args.iter().any(|a| a == "--dump-ast");
    let _trace_emit = args.iter().any(|a| a == "--trace-emit");

    let source = match fs::read_to_string(input_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {input_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Hash the path, not the contents: node identities must stay stable
    // when the file is edited, or the feedback loop's learned weights
    // would be thrown away on every change.
    let file_hash = fnv1a(input_path.as_bytes());

    match parse_source(&source, file_hash) {
        Ok((arena, root)) => {
            if dump_ast {
                print!("{}", dump_tree(&arena, root));
                return ExitCode::SUCCESS;
            }
            eprintln!(
                "c0m: parsed {} AST nodes; later stages not wired yet (try --dump-ast)",
                arena.len()
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!(
                "{}:{}:{}: error: {}",
                input_path, e.span.line, e.span.col, e.message
            );
            ExitCode::FAILURE
        }
    }
}
