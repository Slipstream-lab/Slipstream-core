//! Hand-rolled microbenchmarks for schedule construction. Uses only the
//! standard library so `cargo bench` works on any stable toolchain.
//!
//! These are raw measurements, not a benchmarking framework. Run with:
//!
//! ```sh
//! cargo bench -p slipstream-scheduler
//! ```

use slipstream_footprint::keys::contract_data;
use slipstream_footprint::TransactionFootprint;
use slipstream_scheduler::{build_conflict_graph, greedy_schedule};
use std::time::{Duration, Instant};

fn timed<F: FnMut()>(mut f: F) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}

fn make_footprints(n: usize, distinct: usize) -> Vec<TransactionFootprint> {
    (0..n)
        .map(|i| {
            TransactionFootprint::new()
                .read(contract_data("C1", "config"))
                .read_write(contract_data("C1", format!("k{}", i % distinct)))
        })
        .collect()
}

fn bench(name: &str, n: usize, distinct: usize) {
    let fps = make_footprints(n, distinct);
    let graph_time = timed(|| {
        let _ = build_conflict_graph(&fps);
    });
    let graph = build_conflict_graph(&fps);
    let sched_time = timed(|| {
        let _ = greedy_schedule(&graph);
    });
    println!("{name}: n={n} distinct={distinct} graph={graph_time:?} greedy={sched_time:?}");
}

fn main() {
    bench("small", 64, 8);
    bench("medium", 256, 32);
    bench("large", 1024, 128);
    bench("xlarge", 4096, 512);
}
