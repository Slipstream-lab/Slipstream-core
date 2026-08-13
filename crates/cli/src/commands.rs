//! Implementations of the `slipstream` subcommands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use slipstream_analyzer::{analyze_files, AnalysisReport};
use slipstream_footprint::keys::contract_data;
use slipstream_footprint::TransactionFootprint;
use slipstream_replay::{
    profile as profile_set, ArchiveProfileSource, FixtureSource, ProfileSource,
};
use slipstream_scheduler::{schedule, Schedule};
use slipstream_score::summarize;

pub fn scan(path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let files = collect_rs_files(path)?;
    if files.is_empty() {
        eprintln!("slipstream: no `.rs` files found under {}", path.display());
    }
    let reports = analyze_files(&files)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        for report in &reports {
            print_scan_report(report);
        }
        print_scan_totals(&reports);
    }
    Ok(())
}

pub fn profile(
    fixture: Option<&Path>,
    archive: Option<&Path>,
    from: Option<u32>,
    to: Option<u32>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let set = match (fixture, archive) {
        (Some(path), None) => FixtureSource::new(path).load()?,
        (None, Some(path)) => {
            let source = ArchiveProfileSource {
                bucket_path: path.display().to_string(),
                from_ledger: from.unwrap_or(0),
                to_ledger: to.unwrap_or(0),
            };
            source.load()?
        }
        _ => {
            return Err("profile requires exactly one of --fixture or --archive".into());
        }
    };
    let report = profile_set(&set);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("profile: {}", report.source);
    println!("  transactions:    {}", report.transaction_count);
    println!("  distinct keys:   {}", report.distinct_keys);
    println!("  stages:          {}", report.stage_count);
    println!("  parallelism:     {:.2}", report.parallelism);
    println!("  critical path:   {} txns", report.critical_path_length);
    println!(
        "  weighted crit.:  {} (read=1, write=2)",
        report.weighted_critical_path_weight
    );
    println!("  total conflicts: {}", report.total_conflicts);
    if !report.hot_keys.is_empty() {
        println!("  hot keys:");
        for hk in &report.hot_keys {
            println!(
                "    {:<48} reads={:>4} writes={:>4}",
                hk.key, hk.reads, hk.writes
            );
        }
    }
    Ok(())
}

pub fn simulate(
    transactions: usize,
    distinct: usize,
    seed: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rng = Lcg::new(seed);
    let mut footprints = Vec::with_capacity(transactions);
    for i in 0..transactions {
        let mut fp = TransactionFootprint::new().read(contract_data("C1", "config"));
        let key = if i % 5 == 0 {
            contract_data("C1", format!("shard:{}", rng.range(distinct)))
        } else {
            contract_data("C1", format!("k{}", rng.range(distinct)))
        };
        fp = fp.read_write(key);
        footprints.push(fp);
    }
    let (graph, sched): (_, Schedule) = schedule(&footprints);
    let summary = summarize(&footprints, &graph, &sched, 10);
    println!(
        "simulate: synthetic set of {transactions} txns, {distinct} distinct keys, seed {seed}"
    );
    println!("  stages:          {}", summary.stage_count);
    println!("  parallelism:     {:.2}", summary.parallelism);
    println!("  critical path:   {} txns", summary.critical_path.length);
    println!(
        "  weighted crit.:  {} (read=1, write=2)",
        summary.weighted_critical_path.weight
    );
    println!("  total conflicts: {}", summary.total_conflicts);
    println!("  top hot keys:");
    for hk in &summary.hot_keys {
        println!(
            "    {:<48} reads={:>4} writes={:>4}",
            hk.key, hk.reads, hk.writes
        );
    }
    Ok(())
}

pub fn diff(left: &Path, right: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let left_reports = analyze_path(left)?;
    let right_reports = analyze_path(right)?;
    if json {
        return print_diff_json(left, &left_reports, right, &right_reports);
    }
    let left_totals = aggregate(&left_reports);
    let right_totals = aggregate(&right_reports);

    println!("diff: {} -> {}", left.display(), right.display());
    println!(
        "{:<28} {:>10} {:>10} {:>8}",
        "metric",
        left.display(),
        right.display(),
        "delta"
    );
    for (metric, l, r) in compare(&left_totals, &right_totals) {
        let sign = if r > l { "+" } else { "" };
        println!(
            "{metric:<28} {l:>10} {r:>10} {sign}{delta:>7}",
            delta = r as i64 - l as i64
        );
    }
    Ok(())
}

fn collect_rs_files(path: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else if path.is_dir() {
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
                    files.push(p);
                }
            }
        }
        files.sort();
    } else {
        return Err(format!("path does not exist: {}", path.display()).into());
    }
    Ok(files)
}

fn analyze_path(path: &Path) -> Result<Vec<AnalysisReport>, Box<dyn std::error::Error>> {
    let files = collect_rs_files(path)?;
    analyze_files(&files).map_err(|e| e.into())
}

fn totals_json(path: &Path, reports: &[AnalysisReport]) -> serde_json::Value {
    let totals = aggregate(reports);
    let detectors: serde_json::Map<String, serde_json::Value> = totals
        .detectors
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::from(*v)))
        .collect();
    serde_json::json!({
        "path": path.display().to_string(),
        "files": totals.files,
        "functions": totals.functions,
        "storage_reads": totals.reads,
        "storage_writes": totals.writes,
        "detector_findings": totals.detectors.values().sum::<usize>(),
        "detectors": detectors,
    })
}

fn per_function_metrics(reports: &[AnalysisReport]) -> BTreeMap<String, (usize, usize)> {
    let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for report in reports {
        for f in &report.functions {
            let entry = out.entry(f.function_name.clone()).or_default();
            entry.0 += f.storage_reads.len();
            entry.1 += f.storage_writes.len();
        }
    }
    out
}

fn print_diff_json(
    left: &Path,
    left_reports: &[AnalysisReport],
    right: &Path,
    right_reports: &[AnalysisReport],
) -> Result<(), Box<dyn std::error::Error>> {
    let l = totals_json(left, left_reports);
    let r = totals_json(right, right_reports);
    let lf = per_function_metrics(left_reports);
    let rf = per_function_metrics(right_reports);
    let deltas: Vec<serde_json::Value> = lf
        .keys()
        .chain(rf.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|name| {
            let l = lf.get(name).copied().unwrap_or((0, 0));
            let rr = rf.get(name).copied().unwrap_or((0, 0));
            serde_json::json!({
                "function": name,
                "reads_delta": rr.0 as i64 - l.0 as i64,
                "writes_delta": rr.1 as i64 - l.1 as i64,
            })
        })
        .collect();
    let l_findings = l["detector_findings"].as_u64().unwrap_or(0) as i64;
    let r_findings = r["detector_findings"].as_u64().unwrap_or(0) as i64;
    let l_reads = l["storage_reads"].as_u64().unwrap_or(0) as i64;
    let r_reads = r["storage_reads"].as_u64().unwrap_or(0) as i64;
    let l_writes = l["storage_writes"].as_u64().unwrap_or(0) as i64;
    let r_writes = r["storage_writes"].as_u64().unwrap_or(0) as i64;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "left": l,
            "right": r,
            "per_function_deltas": deltas,
            "summary": {
                "detector_findings_delta": r_findings - l_findings,
                "storage_reads_delta": r_reads - l_reads,
                "storage_writes_delta": r_writes - l_writes,
            },
        }))?
    );
    Ok(())
}

fn print_scan_report(report: &AnalysisReport) {
    let reads: usize = report.functions.iter().map(|f| f.storage_reads.len()).sum();
    let writes: usize = report
        .functions
        .iter()
        .map(|f| f.storage_writes.len())
        .sum();
    println!("== {} ==", report.source_name);
    println!(
        "  functions: {}   reads: {}   writes: {}   detectors: {}",
        report.functions.len(),
        reads,
        writes,
        report.detectors.len()
    );
    for finding in &report.detectors {
        let location = match (&finding.function, &finding.key) {
            (Some(f), Some(k)) => format!("{f} / {k}"),
            (Some(f), None) => f.clone(),
            (None, Some(k)) => k.clone(),
            (None, None) => String::new(),
        };
        println!("  [{}] {} ({location})", finding.detector, finding.message);
    }
}

fn print_scan_totals(reports: &[AnalysisReport]) {
    let functions: usize = reports.iter().map(|r| r.functions.len()).sum();
    let detectors: usize = reports.iter().map(|r| r.detectors.len()).sum();
    println!(
        "scanned {} file(s): {} function(s), {} detector finding(s)",
        reports.len(),
        functions,
        detectors
    );
}

#[derive(Debug, Default, Clone)]
struct Totals {
    files: usize,
    functions: usize,
    reads: usize,
    writes: usize,
    detectors: BTreeMap<String, usize>,
}

fn aggregate(reports: &[AnalysisReport]) -> Totals {
    let mut totals = Totals {
        files: reports.len(),
        ..Totals::default()
    };
    for report in reports {
        totals.functions += report.functions.len();
        for f in &report.functions {
            totals.reads += f.storage_reads.len();
            totals.writes += f.storage_writes.len();
        }
        for d in &report.detectors {
            *totals.detectors.entry(d.detector.to_string()).or_default() += 1;
        }
    }
    totals
}

fn compare(left: &Totals, right: &Totals) -> Vec<(String, usize, usize)> {
    let mut rows = vec![
        ("files".to_string(), left.files, right.files),
        ("functions".to_string(), left.functions, right.functions),
        ("storage reads".to_string(), left.reads, right.reads),
        ("storage writes".to_string(), left.writes, right.writes),
        (
            "detector findings".to_string(),
            left.detectors.values().sum(),
            right.detectors.values().sum(),
        ),
    ];
    let mut keys: BTreeMap<_, _> = BTreeMap::new();
    for k in left.detectors.keys().chain(right.detectors.keys()) {
        keys.insert(k.clone(), ());
    }
    for k in keys.into_keys() {
        rows.push((
            format!("  [{}]", k),
            left.detectors.get(&k).copied().unwrap_or(0),
            right.detectors.get(&k).copied().unwrap_or(0),
        ));
    }
    rows
}

/// A small deterministic LCG used to generate synthetic footprints for
/// `simulate`. Not cryptographically secure; determinism is the point.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    fn range(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}
