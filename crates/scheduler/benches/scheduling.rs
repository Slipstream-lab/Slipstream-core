//! Criterion benchmarks for schedule construction.
//!
//! Measures the two hot paths of `slipstream-scheduler` over a deterministic
//! workload generator across a scaling sweep:
//!
//! * `conflict_graph` — per-key index construction of the conflict graph;
//! * `greedy_schedule` — greedy stage assignment over that graph.
//!
//! Run with:
//!
//! ```sh
//! cargo bench -p slipstream-scheduler
//! ```
//!
//! Workloads are fully deterministic (a fixed generator, no RNG), so runs are
//! comparable across machines for the *same* environment. Never report a number
//! from this harness without also recording the machine, OS and `rustc`
//! version, per `benches/README.md`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use slipstream_footprint::keys::contract_data;
use slipstream_footprint::TransactionFootprint;
use slipstream_scheduler::{build_conflict_graph, greedy_schedule};
use std::hint::black_box;

/// Deterministic workload: every transaction reads a shared `config` key and
/// writes one of `distinct` hot keys, so contention scales inversely with
/// `distinct`. No randomness — identical output for identical parameters.
fn make_footprints(n: usize, distinct: usize) -> Vec<TransactionFootprint> {
    (0..n)
        .map(|i| {
            TransactionFootprint::new()
                .read(contract_data("C1", "config"))
                .read_write(contract_data("C1", format!("k{}", i % distinct)))
        })
        .collect()
}

/// The scaling sweep: (transactions, distinct hot keys).
const SWEEP: &[(usize, usize)] = &[(64, 8), (256, 32), (1024, 128), (4096, 512)];

fn bench_conflict_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("conflict_graph");
    for &(n, distinct) in SWEEP {
        let fps = make_footprints(n, distinct);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("n{n}_d{distinct}")),
            &fps,
            |b, fps| b.iter(|| black_box(build_conflict_graph(black_box(fps)))),
        );
    }
    group.finish();
}

fn bench_greedy_schedule(c: &mut Criterion) {
    let mut group = c.benchmark_group("greedy_schedule");
    for &(n, distinct) in SWEEP {
        let fps = make_footprints(n, distinct);
        let graph = build_conflict_graph(&fps);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("n{n}_d{distinct}")),
            &graph,
            |b, graph| b.iter(|| black_box(greedy_schedule(black_box(graph)))),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_conflict_graph, bench_greedy_schedule);
criterion_main!(benches);
