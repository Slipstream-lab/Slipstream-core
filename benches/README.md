# Benchmarks

Slipstream Core treats performance as a property to measure, not a claim to
make. Benchmarks here are raw measurements only; no numbers are ever reported
outside of `cargo bench` output.

## Harness

Benchmarks use [Criterion](https://bheisler.github.io/criterion.rs/) (a
statistics-driven micro-benchmarking framework) with `harness = false`. Each
library crate that has a hot path carries its own `benches/`:

| Crate                  | Benchmark    | Covers                                                            |
| ---------------------- | ------------ | ---------------------------------------------------------------- |
| `slipstream-scheduler` | `scheduling` | conflict-graph construction and greedy scheduling (scaling sweep) |
| `slipstream-analyzer`  | `analysis`   | `syn`-based analysis + detectors over a representative corpus     |
| `slipstream-replay`    | `profiling`  | fixture parsing and end-to-end profile of the largest fixture    |

Run all benchmarks:

```sh
cargo bench --workspace
```

Run one crate's benchmarks:

```sh
cargo bench -p slipstream-scheduler
cargo bench -p slipstream-analyzer
cargo bench -p slipstream-replay
```

Criterion writes detailed reports (including comparisons against the previous
run) under `target/criterion/`.

## Workloads

- **Scheduler** — a deterministic generator: `n` transactions each writing one
  of `distinct` hot keys and reading a shared `config` key. Contention scales
  inversely with `distinct`. Sweep: `n ∈ {64, 256, 1024, 4096}`.
- **Analyzer** — an embedded corpus of Soroban contract sources spanning every
  detector pattern plus a clean control.
- **Replay** — the largest `.json` fixture under `fixtures/`, profiled
  end-to-end. Fixtures carry explicit provenance and illustrative labels.

All workloads are deterministic (fixed generators / recorded fixtures, no RNG),
so a run is comparable against another run **in the same environment**.

## Policy

- Never commit a fabricated or extrapolated benchmark number.
- When a benchmark produces a number that is reported anywhere (issues, PRs,
  docs), report the command, environment (OS, CPU, `rustc --version`), and raw
  output that produced it. A number without its environment is not a result.
- A benchmark is only meaningful if the workload generator is deterministic
  (fixed seed) or recorded from a real capture.
