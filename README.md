# C0m

An IR-free compiler for Suda, translating directly from an AST to x86-64
assembly. Companion codebase to the project proposal
"C0m: An Intermediate-Representation-Free Compiler for Auditable and
Traceable Machine Code Generation" (Patrick Ngangaa Maina, 167052,
Strathmore University, supervised by Mr. Tiberius Tabulu).

## Layout

This is a Cargo workspace with one crate per methodology increment
(Chapter 3.3):

| Crate            | Increment | Delivers                                   |
|-------------------|-----------|---------------------------------------------|
| `c0m-frontend`     | 1         | Lexer, parser, arena-allocated AST          |
| `c0m-weighting`    | 2         | Instinctive Automata (W_raw / W_combined)   |
| `c0m-dos`          | 3         | Deferred Optimization Store (tiered gating) |
| `c0m-emitter`      | 4         | Direct emitter + Register Oracle            |
| `c0m-feedback`     | 5         | perf_event_open + pattern_memory.db         |
| `c0m-cli`          | —         | Driver binary wiring the pipeline together  |

## Getting started

```
rustup toolchain install stable
rustup component add rustfmt clippy
cargo build --workspace
cargo test --workspace
```

`cargo test --workspace` doubles as the V-model verification step
described in Chapter 3.3.6 — unit tests live inside each crate,
integration tests live in `c0m-cli/tests/`.

## perf_event_open access (needed from increment 5 onward)

```
cat /proc/sys/kernel/perf_event_paranoid
sudo sysctl kernel.perf_event_paranoid=1   # or grant CAP_PERFMON via setcap
```

## Test programs

`suda-tests/` holds the curated suite referenced in Chapter 3.5.3,
ordered by structural complexity (arithmetic -> branching -> loops ->
nested loops -> function calls), to exercise every DOS tier.
