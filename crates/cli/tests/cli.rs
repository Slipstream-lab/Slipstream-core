//! End-to-end CLI integration tests. These run the real `slipstream` binary.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn slipstream(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slipstream"))
        .args(args)
        .output()
        .expect("binary runs")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("slipstream-cli-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

const GLOBAL_COUNTER: &str = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

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

#[test]
fn scan_reports_detectors_on_sample_contract() {
    let dir = temp_dir("scan");
    let contract = dir.join("counter.rs");
    fs::write(&contract, GLOBAL_COUNTER).expect("write contract");
    let out = slipstream(&["scan", contract.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("global-static-write"), "{stdout}");
    assert!(stdout.contains("increment"), "{stdout}");
}

#[test]
fn scan_json_output_is_valid_json() {
    let dir = temp_dir("scan-json");
    let contract = dir.join("counter.rs");
    fs::write(&contract, GLOBAL_COUNTER).expect("write contract");
    let out = slipstream(&["scan", "--json", contract.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert!(parsed.is_array());
}

#[test]
fn profile_prints_summary_for_fixture() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("mainnet_fragment.json");
    let out = slipstream(&["profile", "--fixture", fixture.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("transactions:"), "{stdout}");
    assert!(stdout.contains("stages:"), "{stdout}");
    assert!(stdout.contains("hot keys:"), "{stdout}");
}

#[test]
fn profile_json_output_is_valid_and_matches_summary() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("mainnet_fragment.json");
    let args = ["profile", "--fixture", fixture.to_str().unwrap(), "--json"];
    let out1 = slipstream(&args);
    let out2 = slipstream(&args);
    assert!(out1.status.success(), "{out1:?}");
    assert_eq!(
        out1.stdout, out2.stdout,
        "profile --json must be deterministic"
    );
    let stdout = String::from_utf8_lossy(&out1.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    // The JSON must expose the full ProfileReport contract downstream relies on.
    assert!(parsed["transaction_count"].is_u64());
    assert!(parsed["stage_count"].is_u64());
    assert!(parsed["parallelism"].is_number());
    assert!(parsed["critical_path_length"].is_u64());
    assert!(parsed["weighted_critical_path_weight"].is_u64());
    assert!(parsed["total_conflicts"].is_u64());
    assert!(parsed["hot_keys"].is_array());
    assert!(
        parsed["schedule"]["stages"].is_array(),
        "the full schedule must be included: {stdout}"
    );
    // The JSON path must produce nothing but JSON (no human-readable preamble).
    assert!(!stdout.contains("profile:"), "{stdout}");
}

#[test]
fn profile_archive_capture_works_end_to_end() {
    let capture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("archive")
        .join("capture.xdr");
    let out = slipstream(&[
        "profile",
        "--archive",
        capture.to_str().unwrap(),
        "--from",
        "100",
        "--to",
        "103",
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("transactions:"), "{stdout}");
    assert!(stdout.contains("ledger-archive"), "{stdout}");

    let jout = slipstream(&[
        "profile",
        "--archive",
        capture.to_str().unwrap(),
        "--from",
        "100",
        "--to",
        "103",
        "--json",
    ]);
    assert!(jout.status.success(), "{jout:?}");
    let jstdout = String::from_utf8_lossy(&jout.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&jstdout).expect("valid json");
    assert_eq!(parsed["transaction_count"], 3);
    assert!(parsed["hot_keys"].is_array());
    assert!(!jstdout.contains("profile:"), "{jstdout}");
}

#[test]
fn profile_requires_exactly_one_source() {
    let out = slipstream(&["profile"]);
    assert!(!out.status.success(), "no source must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--fixture") || stderr.contains("--archive"),
        "{stderr}"
    );
}

#[test]
fn simulate_runs_deterministically() {
    let out1 = slipstream(&["simulate", "--transactions", "64", "--seed", "7"]);
    let out2 = slipstream(&["simulate", "--transactions", "64", "--seed", "7"]);
    assert!(out1.status.success() && out2.status.success());
    assert_eq!(out1.stdout, out2.stdout);
    let stdout = String::from_utf8_lossy(&out1.stdout);
    assert!(stdout.contains("stages:"), "{stdout}");
}

#[test]
fn diff_compares_two_implementations() {
    let dir = temp_dir("diff");
    let naive = dir.join("naive.rs");
    let optimized = dir.join("optimized.rs");
    fs::write(&naive, GLOBAL_COUNTER).expect("write naive");
    let optimized_src = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

#[contract]
pub struct Sharded;

#[contractimpl]
impl Sharded {
    pub fn increment(env: Env, shard: u32) -> u32 {
        let key = symbol_short!("shard");
        let mut count: u32 = env.storage().instance().get(&key).unwrap_or(0);
        count += 1;
        env.storage().instance().put(&key, &count);
        count
    }
}
"#;
    fs::write(&optimized, optimized_src).expect("write optimized");
    let out = slipstream(&["diff", naive.to_str().unwrap(), optimized.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("detector findings"), "{stdout}");
}

#[test]
fn diff_json_output_is_stable_and_valid() {
    let dir = temp_dir("diff-json");
    let naive = dir.join("naive.rs");
    let optimized = dir.join("optimized.rs");
    fs::write(&naive, GLOBAL_COUNTER).expect("write naive");
    let optimized_src = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

#[contract]
pub struct Sharded;

#[contractimpl]
impl Sharded {
    pub fn increment(env: Env, shard: u32) -> u32 {
        let mut count: u32 = env.storage().instance().get(&symbol_short!("shard")).unwrap_or(0);
        count += 1;
        env.storage().instance().put(&symbol_short!("shard"), &count);
        count
    }
}
"#;
    fs::write(&optimized, optimized_src).expect("write optimized");
    let args = [
        "diff",
        "--json",
        naive.to_str().unwrap(),
        optimized.to_str().unwrap(),
    ];
    let out1 = slipstream(&args);
    let out2 = slipstream(&args);
    assert!(out1.status.success(), "{out1:?}");
    assert_eq!(
        out1.stdout, out2.stdout,
        "diff --json must be deterministic"
    );
    let stdout = String::from_utf8_lossy(&out1.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert!(parsed["left"].is_object());
    assert!(parsed["right"].is_object());
    assert!(parsed["summary"]["detector_findings_delta"].is_i64());
    assert!(parsed["per_function_deltas"].is_array());
}
