#!/usr/bin/env python3
"""Runtime benchmark: c0m output vs equivalent C.

Run from the repository root, after:  cargo build --release -p c0m-cli
    python3 bench/run_bench.py            # full run (10 repetitions)
    SCALE=0.1 RUNS=3 python3 bench/run_bench.py   # quick trial
Writes bench/results.csv (every raw timing) and prints a Markdown table.
"""
import csv, os, shutil, statistics, subprocess, sys, time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
BENCH = os.path.join(ROOT, "bench")
BUILD = os.path.join(BENCH, "build")
C0M = os.path.join(ROOT, "target", "release", "c0m")
RUNS = int(os.environ.get("RUNS", "10"))
SCALE = float(os.environ.get("SCALE", "1"))
# pin to one CPU that this process is allowed to use (reduces run-to-run noise)
PIN = ["taskset", "-c", str(max(os.sched_getaffinity(0)))] if shutil.which("taskset") else []

# (program in c0m-tests, function to call, calls per run)
KERNELS = [
    ("02_single_loop", "sum", 10_000_000),
    ("03_nested_loop", "grid", 10_000_000),
    ("04_hot_kernel", "kernel", 5_000),
    ("07_workload", "work", 100),
]
LABELS = ["c0m --no-dos", "c0m default", "gcc -O0", "gcc -O2"]


def sh(cmd):
    subprocess.run(cmd, check=True)


def build_variants(name, fn, calls):
    """Builds one executable per variant. Every variant links the SAME -O0 C driver,
    so the only difference between executables is the code of the kernel itself."""
    suda = open(os.path.join(ROOT, "c0m-tests", name + ".c0m")).read()
    suda = suda.replace("fn main", "fn suda_entry")  # the C driver owns main()
    src = os.path.join(BUILD, name + ".c0m")
    open(src, "w").write(suda)

    drv_c = os.path.join(BUILD, name + "_driver.c")
    open(drv_c, "w").write(
        "#include <stdio.h>\n"
        f"extern long {fn}(void);\n"
        "int main(void) {\n"
        f"    long s = 0;\n    for (long r = 0; r < {calls}; r++) s += {fn}();\n"
        '    printf("%ld\\n", s);\n    return 0;\n}\n'
    )
    drv_o = drv_c[:-2] + ".o"
    sh(["gcc", "-O0", "-c", drv_c, "-o", drv_o])

    objs = {}
    for label, extra, tag in (("c0m --no-dos", ["--no-dos"], "nodos"), ("c0m default", [], "default")):
        asm = os.path.join(BUILD, f"{name}_{tag}.s")
        # --no-memory: ignore ~/.c0m/pattern_memory.db so only static weighting applies
        sh([C0M, src, "--no-memory", *extra, "-o", asm])
        obj = asm[:-2] + ".o"
        sh(["gcc", "-c", asm, "-o", obj])
        objs[label] = obj
    ref = os.path.join(BENCH, f"ref_{name}.c")
    for label, opt in (("gcc -O0", "-O0"), ("gcc -O2", "-O2")):
        obj = os.path.join(BUILD, f"{name}_gcc{opt}.o")
        sh(["gcc", opt, "-c", ref, "-o", obj])
        objs[label] = obj

    exes = {}
    for label in LABELS:
        exe = objs[label][:-2] + ".bin"
        sh(["gcc", drv_o, objs[label], "-o", exe])
        exes[label] = exe
    return exes


def main():
    if not os.path.exists(C0M):
        sys.exit("build the compiler first:  cargo build --release -p c0m-cli")
    shutil.rmtree(BUILD, ignore_errors=True)
    os.makedirs(BUILD)

    raw, results = [], []
    for name, fn, calls in KERNELS:
        calls = max(1, int(calls * SCALE))
        exes = build_variants(name, fn, calls)

        # correctness first: every variant must print the same checksum
        sums = {}
        for label, exe in exes.items():
            sums[label] = subprocess.run(PIN + [exe], capture_output=True, text=True, check=True).stdout.strip()
        if len(set(sums.values())) != 1:
            sys.exit(f"CHECKSUM MISMATCH in {name}: {sums}")

        times = {label: [] for label in exes}
        for rep in range(RUNS):  # interleaved, so machine drift hits every variant equally
            for label, exe in exes.items():
                t0 = time.perf_counter()
                subprocess.run(PIN + [exe], stdout=subprocess.DEVNULL, check=True)
                dt = time.perf_counter() - t0
                times[label].append(dt)
                raw.append((name, label, rep + 1, f"{dt:.6f}"))
        results.append((name, calls, times, sums[LABELS[0]]))
        print(f"done {name}", file=sys.stderr)

    with open(os.path.join(BENCH, "results.csv"), "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["kernel", "variant", "run", "seconds"])
        w.writerows(raw)

    med = lambda v: statistics.median(v)
    print(f"\nMedian wall-clock seconds over {RUNS} runs (calls = kernel calls per run)\n")
    print("| Kernel | Calls | " + " | ".join(LABELS) + " |")
    print("|---|---|" + "---|" * len(LABELS))
    for name, calls, times, _ in results:
        print(f"| {name} | {calls:,} | " + " | ".join(f"{med(times[l]):.3f}" for l in LABELS) + " |")

    print("\nRatios of medians (read the headings carefully)\n")
    print("| Kernel | DOS speed-up (no-dos / default) | default / gcc -O0 | default / gcc -O2 |")
    print("|---|---|---|---|")
    for name, _, times, _ in results:
        m = {l: med(times[l]) for l in LABELS}
        print(f"| {name} | {m['c0m --no-dos'] / m['c0m default']:.2f}x | "
              f"{m['c0m default'] / m['gcc -O0']:.2f}x | {m['c0m default'] / m['gcc -O2']:.2f}x |")

    print("\nSpread (min-max seconds)\n")
    print("| Kernel | " + " | ".join(LABELS) + " |")
    print("|---|" + "---|" * len(LABELS))
    for name, _, times, _ in results:
        print(f"| {name} | " + " | ".join(f"{min(times[l]):.3f}-{max(times[l]):.3f}" for l in LABELS) + " |")

    print("\nChecksums (identical across all four variants): " +
          ", ".join(f"{n}={s}" for n, _, _, s in results))


if __name__ == "__main__":
    main()
