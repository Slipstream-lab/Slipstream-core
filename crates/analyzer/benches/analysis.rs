//! Criterion benchmark for static analysis.
//!
//! Runs the `syn`-based analyzer + detector suite over a representative corpus
//! of Soroban contract sources spanning the patterns the detectors target
//! (global static writes, writes in loops, read-modify-write, duplicate reads,
//! and a clean control). The corpus is embedded and deterministic.
//!
//! Run with:
//!
//! ```sh
//! cargo bench -p slipstream-analyzer
//! ```
//!
//! Numbers are only meaningful alongside the environment that produced them;
//! see `benches/README.md`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use slipstream_analyzer::analyze;
use std::hint::black_box;

const GLOBAL_COUNTER: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env, Symbol};
#[contract]
pub struct Counter;
#[contractimpl]
impl Counter {
    pub fn increment(env: Env) -> u32 {
        let mut count: u32 = env.storage().instance().get(&symbol_short!("count")).unwrap_or(0);
        count += 1;
        env.storage().instance().put(&symbol_short!("count"), &count);
        count
    }
    pub fn reset(env: Env) {
        env.storage().instance().put(&symbol_short!("count"), &0u32);
    }
}
"#;

const LOOP_WRITER: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, Env, Symbol};
#[contract]
pub struct BulkWriter;
#[contractimpl]
impl BulkWriter {
    pub fn write_all(env: Env, owner: Symbol, items: soroban_sdk::Vec<u32>) {
        for (i, item) in items.iter().enumerate() {
            let key = Symbol::new(&format!("{owner}_{i}"));
            env.storage().persistent().put(&key, &item);
        }
    }
}
"#;

const READ_MODIFY_WRITE: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};
#[contract]
pub struct Balance;
#[contractimpl]
impl Balance {
    pub fn bump(env: Env) -> u32 {
        let b: u32 = env.storage().instance().get(&symbol_short!("bal")).unwrap_or(0);
        env.storage().instance().put(&symbol_short!("bal"), &(b + 1));
        b
    }
}
"#;

const CLEAN: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};
#[contract]
pub struct Settings;
#[contractimpl]
impl Settings {
    pub fn get(env: Env) -> u32 {
        env.storage().instance().get(&symbol_short!("value")).unwrap_or(0)
    }
    pub fn set(env: Env, v: u32) {
        env.storage().instance().put(&symbol_short!("value"), &v);
    }
}
"#;

const CORPUS: &[(&str, &str)] = &[
    ("counter.rs", GLOBAL_COUNTER),
    ("bulk.rs", LOOP_WRITER),
    ("balance.rs", READ_MODIFY_WRITE),
    ("settings.rs", CLEAN),
];

fn bench_analyze_corpus(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze");
    for (name, src) in CORPUS {
        group.bench_with_input(BenchmarkId::from_parameter(name), src, |b, src| {
            b.iter(|| black_box(analyze(black_box(src), *name).expect("parses")))
        });
    }
    // Whole-corpus pass: the realistic "scan a directory" cost.
    group.bench_function("full_corpus", |b| {
        b.iter(|| {
            for (name, src) in CORPUS {
                black_box(analyze(black_box(src), *name).expect("parses"));
            }
        })
    });
    group.finish();
}

criterion_group!(benches, bench_analyze_corpus);
criterion_main!(benches);
