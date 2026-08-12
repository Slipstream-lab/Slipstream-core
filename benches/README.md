# Benchmarks

Slipstream Core treats performance as a property to measure, not a claim to
make. Benchmarks here are raw measurements only; no numbers are ever reported
outside of `cargo bench` output.

## Current state

- `crates/scheduler/benches/scheduling.rs` — hand-rolled, std-only
  microbenchmarks for conflict-graph construction and greedy scheduling.

```sh
cargo bench -p slipstream-scheduler
```

The scheduler's O(n^2) conflict-graph construction is the known hot spot and
the primary target of optimization.

## Policy

- Never commit a fabricated or extrapolated benchmark number.
- When a benchmark produces a number that is reported anywhere (issues, PRs,
  docs), report the command, environment, and raw output that produced it.
- A benchmark is only meaningful if the workload generator is deterministic
  (fixed seed) or recorded from a real capture.
