//! Historical transaction-set profiling.
//!
//! `slipstream-replay` reconstructs transaction footprints for a recorded
//! ledger window and runs scheduling + contention analysis over them.
//!
//! Two source kinds are exposed via the [`ProfileSource`] trait:
//!
//! - [`FixtureSource`] reads the JSON transaction sets under `fixtures/`
//!   (deterministic, fully tested).
//! - [`ArchiveProfileSource`] reads a ledger-archive capture document — the
//!   minimal XDR capture subset encoded by [`xdr`] — and is the wired-up path
//!   for historical replay.
//!
//! The Stellar RPC source ([`RpcProfileSource`]) still requires a live
//! endpoint and reports [`ReplayError::Unavailable`] for now; it shares the
//! same XDR decoding layer ([`decode_capture_set`]) with the archive source so
//! footprint decoding is never duplicated.

pub mod xdr;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use slipstream_footprint::TransactionFootprint;
use slipstream_scheduler::{schedule, ConflictGraph, Schedule};
use slipstream_score::Summary;

use xdr::{ArchiveCapture, CaptureRecord, XdrError};

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
    #[error("failed to decode archive capture {path}: {source}")]
    Xdr {
        path: PathBuf,
        #[source]
        source: XdrError,
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

impl RpcProfileSource {
    /// Decodes RPC payload bytes through the shared capture decoder. Both the
    /// archive and RPC sources funnel capture bytes through
    /// [`decode_capture_set`] so the XDR layer is never duplicated.
    pub fn decode(&self, bytes: &[u8]) -> Result<TransactionSet, ReplayError> {
        decode_capture_set(&format!("stellar-rpc://{}", self.endpoint), bytes)
    }
}

/// Configuration for a ledger-archive-backed profile source.
#[derive(Debug, Clone)]
pub struct ArchiveProfileSource {
    /// Path to the capture document (XDR bytes) for the ledger range.
    pub bucket_path: String,
    pub from_ledger: u32,
    pub to_ledger: u32,
}

impl ProfileSource for ArchiveProfileSource {
    fn name(&self) -> &str {
        "ledger-archive"
    }

    fn load(&self) -> Result<TransactionSet, ReplayError> {
        let path = Path::new(&self.bucket_path);
        let raw = std::fs::read(path).map_err(|source| ReplayError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let provenance = format!(
            "ledger-archive capture: ledgers {}..={}, {} checkpoints, {} bytes from `{}`",
            self.from_ledger,
            self.to_ledger,
            self.checkpoint_count(&raw),
            raw.len(),
            self.bucket_path
        );
        decode_capture_set(&provenance, &raw)
    }
}

impl ArchiveProfileSource {
    /// The number of checkpoints spanned by the capture, derived from the
    /// decoded header when possible.
    fn checkpoint_count(&self, raw: &[u8]) -> u32 {
        xdr::decode_capture(raw)
            .map(|cap| cap.checkpoint_count)
            .unwrap_or(0)
    }
}

/// Decodes a capture document into a [`TransactionSet`], recording the
/// provenance string on the set. The single shared entry point for XDR-based
/// sources (archive and RPC).
pub fn decode_capture_set(provenance: &str, bytes: &[u8]) -> Result<TransactionSet, ReplayError> {
    let capture = xdr::decode_capture(bytes).map_err(|source| ReplayError::Xdr {
        path: provenance.into(),
        source,
    })?;
    Ok(records_to_set(provenance, &capture))
}

/// Converts decoded capture records into a transaction set.
fn records_to_set(provenance: &str, capture: &ArchiveCapture) -> TransactionSet {
    let records = capture
        .records
        .iter()
        .map(record_to_transaction)
        .collect::<Vec<_>>();
    TransactionSet {
        captured_from: Some(provenance.to_string()),
        records,
    }
}

fn record_to_transaction(rec: &CaptureRecord) -> TransactionRecord {
    let tx_hash = rec.tx_hash.iter().map(|b| format!("{b:02x}")).collect();
    let mut footprint = TransactionFootprint::new();
    for key in &rec.read_only {
        footprint = footprint.read(key.to_ledger_key());
    }
    for key in &rec.read_write {
        footprint = footprint.read_write(key.to_ledger_key());
    }
    TransactionRecord {
        tx_hash,
        source_account: rec.source_account.clone(),
        footprint,
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
    pub weighted_critical_path_weight: u64,
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
        weighted_critical_path_weight: summary.weighted_critical_path.weight,
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

    fn archive_capture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("archive")
            .join("capture.xdr")
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

    #[test]
    fn archive_source_loads_capture_fixture() {
        let source = ArchiveProfileSource {
            bucket_path: archive_capture_path().display().to_string(),
            from_ledger: 100,
            to_ledger: 103,
        };
        let set = source.load().expect("archive capture present and valid");
        assert_eq!(set.len(), 3, "three transactions in the capture");

        let provenance = set.captured_from.as_ref().expect("provenance recorded");
        assert!(
            provenance.contains("ledgers 100..=103"),
            "range in provenance: {provenance}"
        );
        assert!(provenance.contains("checkpoints"), "{provenance}");
        assert!(provenance.contains("bytes"), "{provenance}");

        let footprints = set.footprints();
        assert_eq!(footprints.len(), 3);
        let mut writes = footprints
            .iter()
            .flat_map(|f| f.writes().iter().map(|k| k.to_string()))
            .collect::<Vec<_>>();
        writes.sort();
        assert_eq!(
            writes,
            vec!["contract:C0000000000000000000000000000000000000000000000000000000000000001:shard:0",
                 "contract:C0000000000000000000000000000000000000000000000000000000000000001:shard:1"]
        );
    }

    #[test]
    fn archive_source_profiles_deterministically() {
        let source = ArchiveProfileSource {
            bucket_path: archive_capture_path().display().to_string(),
            from_ledger: 100,
            to_ledger: 103,
        };
        let set = source.load().expect("archive capture present and valid");
        let r1 = profile(&set);
        let r2 = profile(&set);
        assert_eq!(r1, r2, "archive profiling must be deterministic");
        assert_eq!(r1.transaction_count, set.len());
        assert!(r1.schedule.is_complete(set.len()));
        assert!(r1.schedule.is_conflict_free(&set.footprints()));
    }

    #[test]
    fn rpc_source_shares_archive_xdr_decoding() {
        let bytes = std::fs::read(archive_capture_path()).expect("capture file");
        let rpc = RpcProfileSource {
            endpoint: "https://rpc.example.invalid".into(),
            network_passphrase: "Test SDF Network ; September 2015".into(),
        };
        let via_rpc = rpc.decode(&bytes).expect("shared decode works");
        let archive = ArchiveProfileSource {
            bucket_path: archive_capture_path().display().to_string(),
            from_ledger: 100,
            to_ledger: 103,
        };
        let via_archive = archive.load().expect("archive load");
        assert_eq!(
            via_rpc.records, via_archive.records,
            "RPC and archive sources decode identically"
        );
    }

    #[test]
    fn archive_decode_rejects_corrupt_bytes() {
        let mut bytes = std::fs::read(archive_capture_path()).expect("capture file");
        bytes[0] = 0x00;
        let res = decode_capture_set("test", &bytes);
        assert!(matches!(res, Err(ReplayError::Xdr { .. })));
    }
}
