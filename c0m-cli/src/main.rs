//! c0m — the compiler driver.
//!
//! Wires increments 1-5 into one pipeline:
//!   Suda source -> AST -> weighted AST -> DOS-gated strategy selection
//!   -> x86-64 assembly + traceability records -> (later) feedback loop.
//!
//! `--trace-emit` will print each instruction with its originating
//! AST node and source span, per the auditability claim in the abstract.

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let Some(input_path) = args.get(1) else {
        eprintln!("usage: c0m <source.c0m> [--trace-emit]");
        return ExitCode::FAILURE;
    };

    let _trace_emit = args.iter().any(|a| a == "--trace-emit");

    let _source = match fs::read_to_string(input_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {input_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!("c0m: pipeline not yet wired — implement increment 1 first");
    ExitCode::FAILURE
}
