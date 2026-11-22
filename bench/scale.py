#!/usr/bin/env python3
"""Compile-time scaling of c0m on synthetic programs.

Run from the repository root, after:  cargo build --release -p c0m-cli
    python3 bench/scale.py          (REPS=10 by default)
Program size is measured in AST nodes (one line of --dump-ast per node).
"""
import math, os, statistics, subprocess, sys, tempfile, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
C0M = os.path.join(ROOT, "target", "release", "c0m")
REPS = int(os.environ.get("REPS", "10"))


def many_functions(m):
    """m small functions, each with a loop and an if (nesting depth fixed)."""
    one = ("fn f{k}(x: i64) -> i64 {{\n    let mut a: i64 = x;\n    for i in 0..8 {{\n"
           "        if a > i {{ a = a + i * 5; }} else {{ a = a - 1; }}\n    }}\n    return a;\n}}\n")
    return "".join(one.format(k=k) for k in range(m))


def one_big_function(n):
    """one function with n straight-line statements."""
    return "fn big() -> i64 {\n    let mut a: i64 = 0;\n" + "    a = a + 1;\n" * n + "    return a;\n}\n"


def nodes(path):
    out = subprocess.run([C0M, path, "--no-memory", "--dump-ast"], capture_output=True, text=True, check=True).stdout
    return out.count("\n")


def compile_seconds(path):
    cmd = [C0M, path, "--no-memory", "-o", os.devnull]
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)  # warm-up
    times = []
    for _ in range(REPS):
        t0 = time.perf_counter()
        subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)
        times.append(time.perf_counter() - t0)
    return statistics.median(times)


def exponent(points):
    """slope of log(time) against log(nodes): ~1 = linear, ~2 = quadratic."""
    xs = [math.log(n) for n, _ in points]
    ys = [math.log(t) for _, t in points]
    mx, my = sum(xs) / len(xs), sum(ys) / len(ys)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs)


def series(title, make, sizes):
    print(f"\n{title}\n")
    print("| Size parameter | AST nodes | Median compile time (ms) | Microseconds per node |")
    print("|---|---|---|---|")
    pts = []
    with tempfile.TemporaryDirectory() as d:
        for s in sizes:
            p = os.path.join(d, f"p{s}.c0m")
            open(p, "w").write(make(s))
            n, t = nodes(p), compile_seconds(p)
            pts.append((n, t))
            print(f"| {s:,} | {n:,} | {t * 1000:.1f} | {t * 1e6 / n:.3f} |")
    print(f"\nGrowth exponent over the three largest sizes: {exponent(pts[-3:]):.2f} (1.00 = linear)")


if __name__ == "__main__":
    if not os.path.exists(C0M):
        sys.exit("build the compiler first:  cargo build --release -p c0m-cli")
    series("Series A: many small functions (loop + if, fixed nesting)", many_functions, [1000, 2000, 4000, 8000, 16000])
    series("Series B: one function with n straight-line statements", one_big_function, [10000, 20000, 40000, 80000, 160000])
