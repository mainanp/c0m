//! c0m — the compiler driver.
//!
//! Wires increments 1-4 into one pipeline:
//!   Suda source -> AST -> weighted AST -> DOS-gated strategy selection
//!   -> x86-64 assembly + traceability records -> (later) feedback loop.
//!
//! Usage: c0m <source.c0m> [--dump-ast] [--trace-emit] [-o out.s]
//!
//! Flags:
//!   --dump-ast     print the AST and stop (increment 1 inspection)
//!   --trace-emit   print each instruction with its originating AST node
//!                  kind and source position instead of the plain assembly
//!   -o <file>      write the assembly to a file instead of stdout

use std::env;
use std::fs;
use std::process::ExitCode;

use c0m_dos::DosStore;
use c0m_emitter::emit_program;
use c0m_frontend::dump::dump_tree;
use c0m_frontend::parse_source;
use c0m_weighting::{analyze, no_history};

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

    // `-o` takes the argument after it; everything else without `--` is the input.
    let out_path = args
        .iter()
        .position(|a| a == "-o")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let input_path = args
        .iter()
        .enumerate()
        .skip(1)
        .find(|(i, a)| {
            !a.starts_with('-') && !(*i > 0 && args.get(i - 1).map_or(false, |p| p == "-o"))
        })
        .map(|(_, a)| a.clone());
    let Some(input_path) = input_path else {
        eprintln!("usage: c0m <source.c0m> [--dump-ast] [--trace-emit] [-o out.s]");
        return ExitCode::FAILURE;
    };
    let dump_ast = args.iter().any(|a| a == "--dump-ast");
    let trace_emit = args.iter().any(|a| a == "--trace-emit");

    let source = match fs::read_to_string(&input_path) {
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

    if args.iter().any(|a| a == "--dump-tokens") {
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

    // Increment 2: weigh the tree (no learned history until increment 5).
    let no_dos = args.iter().any(|a| a == "--no-dos");
    if !no_dos {
        analyze(&mut arena, root, &no_history);
    }
    if args.iter().any(|a| a == "--dump-weights") {
        print!("{}", c0m_frontend::dump::dump_tree_weighted(&arena, root));
        return ExitCode::SUCCESS;
    }

    // Increment 3: index the functions the DOS needs for inlining.
    let dos = DosStore::build(&arena, root);
    
    // Increment 4: emit.
    let emitted = match emit_program(&arena, root, &dos) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}:{}:{}: error: {}", input_path, e.span.line, e.span.col, e.message);
            return ExitCode::FAILURE;
        }
    };

    if args.iter().any(|a| a == "--dump-strategies") {
        for (id, s) in emitted.strategies.iter().filter(|(_, s)| *s != c0m_dos::EmitStrategy::Direct) {
            let n = arena.get(id);
            println!("{:?} @ {}:{} -> {:?}", n.kind, n.span.line, n.span.col, s);
        }
        return ExitCode::SUCCESS;
    }

    let text = if trace_emit {
        emitted.ctx.annotated_listing()
    } else {
        emitted.ctx.output.clone()
    };
    match out_path {
        Some(p) => {
            if let Err(e) = fs::write(&p, text) {
                eprintln!("error writing {p}: {e}");
                return ExitCode::FAILURE;
            }
        }
        None => print!("{text}"),
    }
    ExitCode::SUCCESS
}
