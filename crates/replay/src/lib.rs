//! Historical transaction-set profiling.
//!
//! `slipstream-replay` reconstructs transaction footprints for a recorded
//! ledger window and runs scheduling + contention analysis over them.
//!
//! Live sources (Stellar RPC, Horizon, ledger archives) require external
//! services and are exposed as [`ProfileSource`] trait implementations whose
//! `load()` currently reports [`ReplayError::Unavailable`]. The deterministic,
//! fully tested path is the fixture-based [`FixtureSource`], which reads the
//! JSON transaction sets under `fixtures/`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use slipstream_footprint::TransactionFootprint;
use slipstream_scheduler::{schedule, ConflictGraph, Schedule};
use slipstream_score::Summary;

/// A single recorded transaction and its reconstructed footprint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransactionRecord {
    pub tx_hash: String,
    pub source_account: String,
    pub footprint: TransactionFootprint,
}

/// An ordered, de-duplicated transaction set from one capture window.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransactionSet {
    /// Free-form provenance note describing where the set came from.
    pub captured_from: Option<String>,
    pub records: Vec<TransactionRecord>,
}

impl TransactionSet {
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn footprints(&self) -> Vec<TransactionFootprint> {
        self.records.iter().map(|r| r.footprint.clone()).collect()
    }
}

/// Errors produced while loading or replaying a transaction set.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("unable to load {name}: {message}")]
    Unavailable { name: String, message: String },
    #[error("failed to read fixture {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse fixture {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// A source of historical transaction sets.
pub trait ProfileSource {
    /// A short identifier for the source, used in reports and errors.
    fn name(&self) -> &str;

    /// Loads the transaction set this source represents.
    fn load(&self) -> Result<TransactionSet, ReplayError>;
}

/// Configuration for a future Stellar RPC-backed profile source.
#[derive(Debug, Clone)]
pub struct RpcProfileSource {
    pub endpoint: String,
    pub network_passphrase: String,
}

impl ProfileSource for RpcProfileSource {
    fn name(&self) -> &str {
        "stellar-rpc"
    }

    fn load(&self) -> Result<TransactionSet, ReplayError> {
        Err(ReplayError::Unavailable {
            name: self.name().into(),
            message: format!(
                "RPC capture from `{}` requires the Stellar RPC service; \
                 not wired up yet. See issue: replay RPC ingestion.",
                self.endpoint
            ),
        })
    }
}

/// Configuration for a future ledger-archive-backed profile source.
#[derive(Debug, Clone)]
pub struct ArchiveProfileSource {
    pub bucket_path: String,
    pub from_ledger: u32,
    pub to_ledger: u32,
}

impl ProfileSource for ArchiveProfileSource {
    fn name(&self) -> &str {
        "ledger-archive"
    }

    fn load(&self) -> Result<TransactionSet, ReplayError> {
        Err(ReplayError::Unavailable {
            name: self.name().into(),
            message: format!(
                "archive replay of ledgers {}..{} requires a local ledger archive; \
                 not wired up yet. See issue: replay archive ingestion.",
                self.from_ledger, self.to_ledger
            ),
        })
    }
}

/// Loads a transaction set from a JSON fixture file.
#[derive(Debug, Clone)]
pub struct FixtureSource {
    pub path: PathBuf,
}

impl FixtureSource {
    pub fn new(path: impl AsRef<Path>) -> Self {
        FixtureSource {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl ProfileSource for FixtureSource {
    fn name(&self) -> &str {
        "fixture"
    }

    fn load(&self) -> Result<TransactionSet, ReplayError> {
        load_fixture(&self.path)
    }
}

/// Reads and parses a JSON fixture into a transaction set.
pub fn load_fixture(path: &Path) -> Result<TransactionSet, ReplayError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ReplayError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| ReplayError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// The result of profiling one transaction set.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProfileReport {
    pub source: String,
    pub transaction_count: usize,
    pub distinct_keys: usize,
    pub stage_count: usize,
    pub parallelism: f64,
    pub critical_path_length: usize,
    pub total_conflicts: usize,
    pub hot_keys: Vec<slipstream_score::HotKey>,
    /// The full schedule, for inspection and visualization.
    pub schedule: Schedule,
}

/// Profiles a transaction set: builds the conflict graph, schedules it, and
/// scores contention. Fully deterministic.
pub fn profile(set: &TransactionSet) -> ProfileReport {
    let footprints = set.footprints();
    let (graph, sched): (ConflictGraph, Schedule) = schedule(&footprints);
    let summary: Summary = slipstream_score::summarize(&footprints, &graph, &sched, 10);
    let distinct_keys = footprints
        .iter()
        .flat_map(TransactionFootprint::keys)
        .collect::<BTreeSet<_>>()
        .len();
    ProfileReport {
        source: set
            .captured_from
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        transaction_count: set.len(),
        distinct_keys,
        stage_count: summary.stage_count,
        parallelism: summary.parallelism,
        critical_path_length: summary.critical_path.length,
        total_conflicts: summary.total_conflicts,
        hot_keys: summary.hot_keys,
        schedule: sched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("mainnet_fragment.json")
    }

    #[test]
    fn fixture_loads_and_profiles_deterministically() {
        let source = FixtureSource::new(repo_fixture_path());
        let set = source.load().expect("fixture present and valid");
        assert!(!set.is_empty());
        let r1 = profile(&set);
        let r2 = profile(&set);
        assert_eq!(r1, r2, "profiling must be deterministic");
        assert!(r1.schedule.is_complete(set.len()));
        assert!(r1.schedule.is_conflict_free(&set.footprints()));
        assert_eq!(r1.transaction_count, set.len());
        assert!(!r1.hot_keys.is_empty());
    }

    #[test]
    fn rpc_source_reports_unavailable_cleanly() {
        let src = RpcProfileSource {
            endpoint: "https://example.invalid".into(),
            network_passphrase: "Test SDF Network ; September 2015".into(),
        };
        match src.load() {
            Err(ReplayError::Unavailable { .. }) => {}
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn missing_fixture_is_a_read_error() {
        let res = load_fixture(Path::new("/does/not/exist.json"));
        assert!(matches!(res, Err(ReplayError::Read { .. })));
    }
}
