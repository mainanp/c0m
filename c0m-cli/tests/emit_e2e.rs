//! Increment 4 end-to-end tests: compile Suda with the real pipeline,
//! assemble + link the output with `gcc` together with a tiny C driver,
//! run it, and compare against a result computed here in Rust.
//!
//! Skipped (with a note) when `gcc` is not installed.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use c0m_dos::DosStore;
use c0m_emitter::emit_program;
use c0m_frontend::parse_source;
use c0m_weighting::{analyze, no_history};

fn compile(src: &str) -> String {
    let (mut arena, root) = parse_source(src, 0).expect("should parse");
    analyze(&mut arena, root, &no_history);
    let dos = DosStore::build(&arena, root);
    emit_program(&arena, root, &dos).expect("should emit").ctx.output
}

fn gcc_available() -> bool {
    Command::new("gcc").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn suite(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("c0m-tests").join(name);
    fs::read_to_string(p).expect("test program should exist")
}

/// Compiles `src`, links it with `driver` (C source defining `main`),
/// runs it and returns stdout. Panics with the assembly on any failure.
fn run(name: &str, src: &str, driver: &str) -> String {
    let asm = compile(src);
    let dir = std::env::temp_dir().join(format!("c0m_e2e_{}_{}", std::process::id(), name));
    fs::create_dir_all(&dir).unwrap();
    let (s, c, exe) = (dir.join("prog.s"), dir.join("driver.c"), dir.join("prog"));
    fs::write(&s, &asm).unwrap();
    fs::write(&c, driver).unwrap();
    let build = Command::new("gcc").arg(&c).arg(&s).arg("-o").arg(&exe).output().unwrap();
    assert!(
        build.status.success(),
        "gcc failed:\n{}\n--- assembly ---\n{asm}",
        String::from_utf8_lossy(&build.stderr)
    );
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "program crashed ({:?})\n--- assembly ---\n{asm}", out.status);
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

macro_rules! need_gcc {
    () => {
        if !gcc_available() {
            eprintln!("gcc not found: skipping end-to-end test");
            return;
        }
    };
}

#[test]
fn suite_00_main_exits_with_status_zero() {
    need_gcc!();
    let asm = compile(&suite("00_arithmetic.c0m"));
    let dir = std::env::temp_dir().join(format!("c0m_e2e_{}_main0", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let s = dir.join("prog.s");
    let exe = dir.join("prog");
    fs::write(&s, asm).unwrap();
    assert!(Command::new("gcc").arg(&s).arg("-o").arg(&exe).status().unwrap().success());
    assert!(Command::new(&exe).status().unwrap().success());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn suite_01_classify() {
    need_gcc!();
    let out = run(
        "classify",
        &suite("01_branching.c0m"),
        "#include <stdio.h>\nextern long classify(long);\n\
         int main(){ printf(\"%ld %ld %ld\\n\", classify(-5), classify(0), classify(9)); return 0; }",
    );
    assert_eq!(out, "-1 0 1");
}

#[test]
fn suite_02_single_loop() {
    need_gcc!();
    let out = run(
        "sum",
        &suite("02_single_loop.c0m"),
        "#include <stdio.h>\nextern long sum(void);\nint main(){ printf(\"%ld\\n\", sum()); return 0; }",
    );
    assert_eq!(out, (0..100).sum::<i64>().to_string());
}

#[test]
fn suite_03_nested_loop() {
    need_gcc!();
    let out = run(
        "grid",
        &suite("03_nested_loop.c0m"),
        "#include <stdio.h>\nextern long grid(void);\nint main(){ printf(\"%ld\\n\", grid()); return 0; }",
    );
    let mut t = 0i64;
    for i in 0..10 {
        for j in 0..10 {
            t += i * j;
        }
    }
    assert_eq!(out, t.to_string());
}

#[test]
fn suite_04_hot_kernel() {
    need_gcc!();
    let out = run(
        "kernel",
        &suite("04_hot_kernel.c0m"),
        "#include <stdio.h>\nextern long kernel(void);\nint main(){ printf(\"%ld\\n\", kernel()); return 0; }",
    );
    let mut acc = 0i64;
    for i in 0..32 {
        for j in 0..32 {
            for k in 0..32 {
                if (i + j + k) % 2 == 0 {
                    acc += i * j * k;
                }
            }
        }
    }
    assert_eq!(out, acc.to_string());
}

#[test]
fn suite_05_calls_inline_and_return_the_right_value() {
    need_gcc!();
    // The suite file has a Suda `main`, which would clash with the C driver's
    // `main`, so rename it here and add a function that exposes quad(5).
    let src = format!(
        "{}\nfn twenty() -> i64 {{ return quad(5); }}\n",
        suite("05_calls.c0m").replace("fn main", "fn suda_entry")
    );
    let out = run(
        "calls",
        &src,
        "#include <stdio.h>\nextern long twenty(void);\nint main(){ printf(\"%ld\\n\", twenty()); return 0; }",
    );
    assert_eq!(out, "20");
}

#[test]
fn every_strategy_in_hot_nested_loops() {
    need_gcc!();
    // i*5 -> lea, i*8 -> shift, j*6 -> shift+add, (i+j)*7 -> imul, i*3, j*9
    let src = "fn mix(n: i64) -> i64 {
        let mut acc: i64 = 0;
        for i in 0..8 {
            for j in 0..8 {
                acc = acc + i * 5 + j * 6 + i * 8 + (i + j) * 7 + i * 3 + j * 9 + 2 * j;
            }
        }
        return acc + n;
    }";
    let out = run(
        "mix",
        src,
        "#include <stdio.h>\nextern long mix(long);\nint main(){ printf(\"%ld\\n\", mix(1000)); return 0; }",
    );
    let mut acc = 0i64;
    for i in 0..8i64 {
        for j in 0..8i64 {
            acc += i * 5 + j * 6 + i * 8 + (i + j) * 7 + i * 3 + j * 9 + 2 * j;
        }
    }
    assert_eq!(out, (acc + 1000).to_string());
}

#[test]
fn cmov_in_hot_loops_keeps_the_right_value() {
    need_gcc!();
    let src = "fn count() -> i64 {
        let mut r: i64 = 0;
        for i in 0..8 {
            for j in 0..8 {
                if i > j { r = r + 1; } else { r = r + 2; }
                if i == j { r = r + 10; }
            }
        }
        return r;
    }";
    let out = run(
        "cmov",
        src,
        "#include <stdio.h>\nextern long count(void);\nint main(){ printf(\"%ld\\n\", count()); return 0; }",
    );
    let mut r = 0i64;
    for i in 0..8 {
        for j in 0..8 {
            r += if i > j { 1 } else { 2 };
            if i == j {
                r += 10;
            }
        }
    }
    assert_eq!(out, r.to_string());
}

#[test]
fn jump_table_matches_the_plain_chain() {
    need_gcc!();
    let src = "fn pick(x: i64) -> i64 {
        @2 match x {
            2 => { return 20; },
            3 => { return 30; },
            4 => { return 40; },
            5 => { return 50; },
            _ => { return -1; },
        }
        return 0;
    }";
    let out = run(
        "table",
        src,
        "#include <stdio.h>\nextern long pick(long);\nint main(){ for(long x=-3;x<9;x++) printf(\"%ld \", pick(x)); return 0; }",
    );
    let expect: Vec<String> = (-3..9)
        .map(|x: i64| match x {
            2 => 20,
            3 => 30,
            4 => 40,
            5 => 50,
            _ => -1,
        })
        .map(|v| v.to_string())
        .collect();
    assert_eq!(out, expect.join(" "));
}

#[test]
fn match_as_value_with_a_binding_pattern() {
    need_gcc!();
    let src = "fn m(x: i64) -> i64 { let r: i64 = match x { 1 => 10, 2 => 20, n => n * 2, }; return r; }";
    let out = run(
        "matchval",
        src,
        "#include <stdio.h>\nextern long m(long);\nint main(){ printf(\"%ld %ld %ld\\n\", m(1), m(2), m(7)); return 0; }",
    );
    assert_eq!(out, "10 20 14");
}

#[test]
fn recursion_saves_temporaries_across_calls() {
    need_gcc!();
    let src = "fn fib(n: i64) -> i64 {
        if n < 2 { return n; }
        return fib(n - 1) + fib(n - 2);
    }";
    let out = run(
        "fib",
        src,
        "#include <stdio.h>\nextern long fib(long);\nint main(){ printf(\"%ld\\n\", fib(20)); return 0; }",
    );
    assert_eq!(out, "6765");
}

#[test]
fn deep_expression_forces_spills() {
    need_gcc!();
    let mut e = String::from("x");
    for k in (1..=20).rev() {
        e = format!("{k} * 3 + ({e})");
    }
    let src = format!("fn f(x: i64) -> i64 {{ return {e}; }}");
    let out = run(
        "deep",
        &src,
        "#include <stdio.h>\nextern long f(long);\nint main(){ printf(\"%ld\\n\", f(1000)); return 0; }",
    );
    let expect: i64 = 1000 + (1..=20).map(|k| k * 3).sum::<i64>();
    assert_eq!(out, expect.to_string());
}

#[test]
fn division_modulo_and_negation() {
    need_gcc!();
    let src = "fn f(a: i64, b: i64) -> i64 { return a / b + a % b * 100 + -a; }";
    let out = run(
        "divmod",
        src,
        "#include <stdio.h>\nextern long f(long,long);\n\
         int main(){ printf(\"%ld %ld\\n\", f(47, 5), f(-47, 5)); return 0; }",
    );
    let f = |a: i64, b: i64| a / b + a % b * 100 + -a;
    assert_eq!(out, format!("{} {}", f(47, 5), f(-47, 5)));
}

#[test]
fn logical_operators_short_circuit_and_yield_values() {
    need_gcc!();
    let src = "fn f(a: i64, b: i64, c: i64) -> i64 {
        let mut r: i64 = 0;
        if a < b && b < c || a == 100 { r = r + 1; }
        let t: bool = a < b && b < c;
        if t { r = r + 10; }
        if !(a == b) { r = r + 100; }
        return r;
    }";
    let out = run(
        "logic",
        src,
        "#include <stdio.h>\nextern long f(long,long,long);\n\
         int main(){ printf(\"%ld %ld %ld %ld\\n\", f(1,2,3), f(3,2,1), f(100,0,0), f(5,5,5)); return 0; }",
    );
    let f = |a: i64, b: i64, c: i64| {
        let mut r = 0;
        if (a < b && b < c) || a == 100 {
            r += 1;
        }
        if a < b && b < c {
            r += 10;
        }
        if a != b {
            r += 100;
        }
        r
    };
    assert_eq!(out, format!("{} {} {} {}", f(1, 2, 3), f(3, 2, 1), f(100, 0, 0), f(5, 5, 5)));
}

#[test]
fn if_as_a_value() {
    need_gcc!();
    let src = "fn sign(x: i64) -> i64 { let y: i64 = if x > 0 { 1 } else if x == 0 { 0 } else { -1 }; return y; }";
    let out = run(
        "ifval",
        src,
        "#include <stdio.h>\nextern long sign(long);\nint main(){ printf(\"%ld %ld %ld\\n\", sign(9), sign(0), sign(-4)); return 0; }",
    );
    assert_eq!(out, "1 0 -1");
}

#[test]
fn pointers_borrow_and_dereference() {
    need_gcc!();
    let src = "fn bump(p: &mut i64) -> () { *p = *p + 1; }
               fn go() -> i64 { let mut x: i64 = 5; bump(&mut x); bump(&mut x); return x; }";
    let out = run(
        "ptr",
        src,
        "#include <stdio.h>\nextern long go(void);\nint main(){ printf(\"%ld\\n\", go()); return 0; }",
    );
    assert_eq!(out, "7");
}

#[test]
fn two_return_values_use_rax_and_rdx() {
    need_gcc!();
    let src = "fn two(a: i64) -> (i64, i64) { return a + 1, a * 2; }";
    let out = run(
        "two",
        src,
        "#include <stdio.h>\nstruct P { long a; long b; };\nextern struct P two(long);\n\
         int main(){ struct P p = two(10); printf(\"%ld %ld\\n\", p.a, p.b); return 0; }",
    );
    assert_eq!(out, "11 20");
}

#[test]
fn many_arguments_and_parallel_for() {
    need_gcc!();
    let src = "fn add6(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64) -> i64 {
                   return a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6;
               }
               fn total() -> i64 {
                   let mut t: i64 = 0;
                   parallel for i in 0..16 { t = t + add6(i, 1, 2, 3, 4, 5); }
                   return t;
               }";
    let out = run(
        "args",
        src,
        "#include <stdio.h>\nextern long total(void);\nint main(){ printf(\"%ld\\n\", total()); return 0; }",
    );
    let t: i64 = (0..16).map(|i| i + 2 + 6 + 12 + 20 + 30).sum();
    assert_eq!(out, t.to_string());
}

#[test]
fn callee_saved_registers_survive_a_hot_function() {
    need_gcc!();
    // the C driver keeps a live value in a local across the call; if the
    // prologue/epilogue failed to save rbx/r12-r15 the result would be garbage
    let src = suite("04_hot_kernel.c0m");
    let out = run(
        "saved",
        &src,
        "#include <stdio.h>\nextern long kernel(void);\n\
         int main(){ volatile long keep1 = 11, keep2 = 22; long a = kernel(); long b = kernel(); \
         printf(\"%d %ld\\n\", a == b, keep1 + keep2); return 0; }",
    );
    assert_eq!(out, "1 33");
}

#[test]
fn nonzero_loop_start_and_variable_bound() {
    need_gcc!();
    let src = "fn f(n: i64) -> i64 { let mut s: i64 = 0; for i in 3..n { s = s + i; } return s; }";
    let out = run(
        "varbound",
        src,
        "#include <stdio.h>\nextern long f(long);\nint main(){ printf(\"%ld %ld\\n\", f(10), f(2)); return 0; }",
    );
    assert_eq!(out, format!("{} 0", (3..10).sum::<i64>()));
}
