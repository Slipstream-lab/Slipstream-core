//! CAP-0063 inspired stage and cluster construction.
//!
//! Stellar's phased execution model assigns transactions to "lanes" and runs
//! each lane in a series of stages; transactions in the same stage must not
//! conflict (their read/write sets must be disjoint on shared keys). Slipstream
//! models this with a *conflict graph* and constructs a stage assignment by
//! greedy graph coloring: every transaction is placed in the earliest stage in
//! which it conflicts with no other member.
//!
//! The model is deterministic: identical inputs always yield identical
//! schedules.

use std::collections::BTreeSet;

use slipstream_footprint::TransactionFootprint;

/// A set of transactions executed in the same stage. Members are indices into
/// the input transaction list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cluster {
    pub txns: Vec<usize>,
}

impl Cluster {
    /// The stage's width: how many transactions execute in parallel.
    pub fn width(&self) -> usize {
        self.txns.len()
    }
}

/// A schedule: an ordered list of clusters, one per stage.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Schedule {
    pub stages: Vec<Cluster>,
}

impl Schedule {
    /// Number of stages (the parallel span of the schedule).
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }

    /// Total number of transactions scheduled.
    pub fn transaction_count(&self) -> usize {
        self.stages.iter().map(Cluster::width).sum()
    }

    /// Average number of transactions per stage.
    pub fn parallelism(&self) -> f64 {
        if self.stages.is_empty() {
            0.0
        } else {
            self.transaction_count() as f64 / self.stage_count() as f64
        }
    }

    /// The stage index a transaction was assigned to, if scheduled.
    pub fn stage_of(&self, txn: usize) -> Option<usize> {
        self.stages.iter().position(|c| c.txns.contains(&txn))
    }

    /// True if no stage contains a conflicting pair of transactions.
    pub fn is_conflict_free(&self, footprints: &[TransactionFootprint]) -> bool {
        self.stages.iter().all(|cluster| {
            cluster.txns.iter().enumerate().all(|(i, &a)| {
                cluster.txns[i + 1..]
                    .iter()
                    .all(|&b| !footprints[a].conflicts_with(&footprints[b]))
            })
        })
    }

    /// Validates that every transaction appears exactly once.
    pub fn is_complete(&self, n_txns: usize) -> bool {
        let seen: BTreeSet<usize> = self
            .stages
            .iter()
            .flat_map(|c| c.txns.iter().copied())
            .collect();
        seen.len() == n_txns && (0..n_txns).all(|i| seen.contains(&i))
    }
}

/// The conflict graph over an ordered list of transaction footprints.
///
/// Vertices are transaction indices; an edge connects two vertices whose
/// footprints conflict.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConflictGraph {
    /// `adjacency[i]` lists the neighbors of transaction `i`.
    pub adjacency: Vec<Vec<usize>>,
}

impl ConflictGraph {
    pub fn order(&self) -> usize {
        self.adjacency.len()
    }

    pub fn edge_count(&self) -> usize {
        self.adjacency.iter().map(|n| n.len()).sum::<usize>() / 2
    }
}

/// Builds the conflict graph over an ordered list of footprints.
///
/// Complexity is O(n^2) key-set intersections; fine for the fixture-scale
/// transaction sets Slipstream targets, and a documented target for
/// optimization.
pub fn build_conflict_graph(footprints: &[TransactionFootprint]) -> ConflictGraph {
    let n = footprints.len();
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        for j in (i + 1)..n {
            if footprints[i].conflicts_with(&footprints[j]) {
                adjacency[i].push(j);
                adjacency[j].push(i);
            }
        }
    }
    ConflictGraph { adjacency }
}

/// Assigns every transaction to the earliest stage in which it conflicts with
/// no existing member (deterministic greedy coloring in index order).
pub fn greedy_schedule(graph: &ConflictGraph) -> Schedule {
    let n = graph.order();
    let mut stages: Vec<Cluster> = Vec::new();
    for txn in 0..n {
        let assigned = stages.iter_mut().find(|cluster| {
            cluster
                .txns
                .iter()
                .all(|&member| !graph.adjacency[txn].contains(&member))
        });
        match assigned {
            Some(cluster) => cluster.txns.push(txn),
            None => stages.push(Cluster { txns: vec![txn] }),
        }
    }
    Schedule { stages }
}

/// A baseline schedule that runs every transaction in its own stage. Used as
/// the "no parallelism" reference for scoring.
pub fn serial_schedule(n_txns: usize) -> Schedule {
    Schedule {
        stages: (0..n_txns).map(|txn| Cluster { txns: vec![txn] }).collect(),
    }
}

/// Convenience: builds the conflict graph for `footprints` and schedules it.
pub fn schedule(footprints: &[TransactionFootprint]) -> (ConflictGraph, Schedule) {
    let graph = build_conflict_graph(footprints);
    let schedule = greedy_schedule(&graph);
    (graph, schedule)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipstream_footprint::keys::contract_data;

    fn hot_key(i: u32) -> TransactionFootprint {
        TransactionFootprint::new().read_write(contract_data("C1", format!("key{i}")))
    }

    fn cold(i: u32) -> TransactionFootprint {
        TransactionFootprint::new().read(contract_data("C2", format!("key{i}")))
    }

    #[test]
    fn independent_transactions_schedule_in_one_stage() {
        let fps = vec![cold(0), cold(1), cold(2)];
        let (graph, schedule) = schedule(&fps);
        assert_eq!(graph.edge_count(), 0);
        assert_eq!(schedule.stage_count(), 1);
        assert_eq!(schedule.transaction_count(), 3);
        assert!(schedule.is_conflict_free(&fps));
        assert!(schedule.is_complete(3));
    }

    #[test]
    fn all_conflicting_transactions_serialize() {
        let fps = vec![hot_key(0), hot_key(0), hot_key(0)];
        let (graph, schedule) = schedule(&fps);
        assert_eq!(graph.edge_count(), 3);
        assert_eq!(schedule.stage_count(), 3);
        assert_eq!(schedule.parallelism(), 1.0);
        assert!(schedule.is_conflict_free(&fps));
    }

    #[test]
    fn greedy_places_transactions_in_earliest_compatible_stage() {
        // t0 writes key0; t1 writes key1; t2 writes key0 again -> t2 must not
        // share a stage with t0, but can share with t1 if t1 is in stage 0.
        let fps = vec![hot_key(0), hot_key(1), hot_key(0)];
        let (_, schedule) = schedule(&fps);
        assert_eq!(schedule.stage_count(), 2);
        assert!(schedule.is_conflict_free(&fps));
        assert_eq!(schedule.stage_of(0), schedule.stage_of(1));
        assert_ne!(schedule.stage_of(0), schedule.stage_of(2));
    }

    #[test]
    fn scheduling_is_deterministic() {
        let fps: Vec<_> = (0..16).map(|i| hot_key(i % 3)).collect();
        let (_, s1) = schedule(&fps);
        let (_, s2) = schedule(&fps);
        assert_eq!(s1, s2);
    }

    #[test]
    fn mixed_reads_and_writes() {
        // A read-only txn can share a stage with anything that does not write
        // its key; a writer must not share with another writer of that key.
        let fps = vec![
            TransactionFootprint::new().read(contract_data("C1", "shared")),
            hot_key(0),
            TransactionFootprint::new().read(contract_data("C1", "shared")),
            hot_key(0),
        ];
        let (_, schedule) = schedule(&fps);
        assert!(schedule.is_conflict_free(&fps));
        assert!(schedule.is_complete(4));
    }

    #[test]
    fn serial_schedule_is_a_valid_baseline() {
        let s = serial_schedule(5);
        assert_eq!(s.stage_count(), 5);
        assert_eq!(s.parallelism(), 1.0);
        assert!(s.is_complete(5));
    }
}
