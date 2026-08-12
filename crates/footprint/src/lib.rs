//! Transaction footprint model for Stellar / Soroban.
//!
//! A transaction footprint is the set of ledger entries a transaction reads
//! and writes. Two transactions that write the same key, or where one writes a
//! key the other reads, cannot safely execute in the same stage of Stellar's
//! phased execution model. This crate provides the [`LedgerKey`] model, the
//! [`TransactionFootprint`] read/write algebra, and overlap analysis.
//!
//! The model deliberately stays dependency-free and self-contained so it can
//! be reused by every other crate in the workspace (and eventually published
//! as its own crate to crates.io).

pub mod keys;

use std::collections::BTreeSet;

pub use keys::{AssetId, LedgerKey};

/// A transaction's ledger footprint: the entries it reads and the entries it
/// writes.
///
/// `read_only` keys are only read and can safely be shared with other
/// read-only accesses. `read_write` keys are written (and typically also
/// read); sharing a `read_write` key with any other transaction is a conflict.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransactionFootprint {
    pub read_only: BTreeSet<LedgerKey>,
    pub read_write: BTreeSet<LedgerKey>,
}

impl TransactionFootprint {
    /// Creates an empty footprint.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a read-only access to `key`.
    pub fn read(mut self, key: LedgerKey) -> Self {
        self.read_only.insert(key);
        self
    }

    /// Records a read/write access to `key`.
    pub fn read_write(mut self, key: LedgerKey) -> Self {
        self.read_write.insert(key);
        self
    }

    /// The full set of keys touched by the footprint (read-only or read/write).
    pub fn keys(&self) -> BTreeSet<LedgerKey> {
        self.read_only.union(&self.read_write).cloned().collect()
    }

    /// The set of keys this footprint writes.
    pub fn writes(&self) -> &BTreeSet<LedgerKey> {
        &self.read_write
    }

    /// The number of distinct keys touched by this footprint.
    pub fn key_count(&self) -> usize {
        self.keys().len()
    }

    /// The number of keys written by this footprint.
    pub fn write_count(&self) -> usize {
        self.read_write.len()
    }

    /// The keys that would conflict between `self` and `other`.
    ///
    /// A conflict is a key that is written by either footprint and touched by
    /// the other in any way, or a key written by both.
    pub fn conflict_keys(&self, other: &TransactionFootprint) -> BTreeSet<LedgerKey> {
        let mut conflicts = BTreeSet::new();
        for k in self.read_write.intersection(&other.read_write) {
            conflicts.insert(k.clone());
        }
        for k in self.read_write.intersection(&other.read_only) {
            conflicts.insert(k.clone());
        }
        for k in self.read_only.intersection(&other.read_write) {
            conflicts.insert(k.clone());
        }
        conflicts
    }

    /// Returns `true` if `self` and `other` cannot be scheduled in the same
    /// stage because they write a key the other touches.
    pub fn conflicts_with(&self, other: &TransactionFootprint) -> bool {
        !self.conflict_keys(other).is_empty()
    }

    /// Whether the two footprints touch any of the same keys (read or write).
    pub fn overlaps(&self, other: &TransactionFootprint) -> bool {
        self.read_only
            .intersection(&other.read_only)
            .next()
            .is_some()
            || self
                .read_only
                .intersection(&other.read_write)
                .next()
                .is_some()
            || self
                .read_write
                .intersection(&other.read_only)
                .next()
                .is_some()
            || self
                .read_write
                .intersection(&other.read_write)
                .next()
                .is_some()
    }
}

/// The result of comparing two footprints.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Overlap {
    /// Keys touched by both footprints, in any mode.
    pub shared_keys: BTreeSet<LedgerKey>,
    /// Keys that would cause a scheduling conflict (write/write or
    /// write/read).
    pub conflict_keys: BTreeSet<LedgerKey>,
    /// Whether the two footprints conflict.
    pub conflicts: bool,
}

impl Overlap {
    /// Returns `true` if the two footprints cannot share a stage.
    pub fn is_conflicting(&self) -> bool {
        self.conflicts
    }
}

/// Computes the full overlap relationship between two footprints.
pub fn overlap(a: &TransactionFootprint, b: &TransactionFootprint) -> Overlap {
    let conflict_keys = a.conflict_keys(b);
    let shared_keys = a
        .keys()
        .intersection(&b.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    Overlap {
        shared_keys,
        conflicts: !conflict_keys.is_empty(),
        conflict_keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keys::contract_data;

    fn config_key() -> LedgerKey {
        contract_data("C1", "config")
    }

    #[test]
    fn empty_footprint_touches_nothing() {
        let fp = TransactionFootprint::new();
        assert!(fp.keys().is_empty());
        assert_eq!(fp.key_count(), 0);
        assert_eq!(fp.write_count(), 0);
    }

    #[test]
    fn builder_accumulates_keys() {
        let fp = TransactionFootprint::new()
            .read(contract_data("C1", "config"))
            .read_write(contract_data("C1", "balance"));
        assert_eq!(fp.key_count(), 2);
        assert_eq!(fp.write_count(), 1);
        assert!(fp.read_only.contains(&config_key()));
        assert!(fp.read_write.contains(&contract_data("C1", "balance")));
    }

    #[test]
    fn shared_read_only_is_not_a_conflict() {
        let a = TransactionFootprint::new().read(config_key());
        let b = TransactionFootprint::new().read(config_key());
        assert!(a.overlaps(&b));
        assert!(!a.conflicts_with(&b));
        assert!(!overlap(&a, &b).is_conflicting());
    }

    #[test]
    fn read_write_shared_key_conflicts() {
        let key = contract_data("C1", "balance");
        let a = TransactionFootprint::new().read_write(key.clone());
        let b = TransactionFootprint::new().read(key);
        assert!(a.conflicts_with(&b));
        let o = overlap(&a, &b);
        assert!(o.is_conflicting());
        assert_eq!(o.conflict_keys.len(), 1);
    }

    #[test]
    fn disjoint_footprints_do_not_overlap() {
        let a = TransactionFootprint::new().read(contract_data("C1", "config"));
        let b = TransactionFootprint::new().read_write(contract_data("C2", "balance"));
        assert!(!a.overlaps(&b));
        assert!(!a.conflicts_with(&b));
    }

    #[test]
    fn keys_are_deduplicated_across_modes() {
        let key = config_key();
        let fp = TransactionFootprint::new()
            .read(key.clone())
            .read_write(key);
        assert_eq!(fp.key_count(), 1);
        assert_eq!(fp.write_count(), 1);
    }

    #[test]
    fn serde_round_trip() {
        let fp = TransactionFootprint::new()
            .read(contract_data("C1", "config"))
            .read_write(contract_data("C1", "balance"));
        let json = serde_json::to_string(&fp).expect("serialize");
        let back: TransactionFootprint = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(fp, back);
    }
}
