//! Criterion benchmark for replay profiling.
//!
//! Loads the largest checked-in fixture and measures the end-to-end
//! `profile` path (conflict-graph construction, scheduling and scoring) as well
//! as fixture parsing in isolation.
//!
//! Run with:
//!
//! ```sh
//! cargo bench -p slipstream-replay
//! ```
//!
//! The fixture is deterministic and explicitly labelled as illustrative in its
//! provenance field. Report numbers only with the environment that produced
//! them (see `benches/README.md`).

use criterion::{criterion_group, criterion_main, Criterion};
use slipstream_replay::{load_fixture, profile};
use std::hint::black_box;
use std::path::PathBuf;

/// Resolves the largest fixture by scanning the workspace `fixtures/` directory
/// and picking the largest `.json` file. Falls back to the known fragment.
fn largest_fixture() -> PathBuf {
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures");
    let mut best: Option<(u64, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(&fixtures_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let is_better = match &best {
                    Some((b, _)) => size > *b,
                    None => true,
                };
                if is_better {
                    best = Some((size, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
        .unwrap_or_else(|| fixtures_dir.join("mainnet_fragment.json"))
}

fn bench_profile(c: &mut Criterion) {
    let path = largest_fixture();
    let set = load_fixture(&path).expect("largest fixture loads");
    let label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("fixture")
        .to_string();

    let mut group = c.benchmark_group("replay");
    group.bench_function(format!("parse/{label}"), |b| {
        b.iter(|| black_box(load_fixture(black_box(&path)).expect("loads")))
    });
    group.bench_function(format!("profile/{label}"), |b| {
        b.iter(|| black_box(profile(black_box(&set))))
    });
    group.finish();
}

criterion_group!(benches, bench_profile);
criterion_main!(benches);
