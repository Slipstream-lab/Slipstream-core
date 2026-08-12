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

use std::collections::{BTreeMap, BTreeSet};

use slipstream_footprint::{LedgerKey, TransactionFootprint};

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
/// Uses a per-key index: every key maps to the transactions that write it and
/// the transactions that read it (read-only). Edges are emitted as write/write
/// pairs within a key's writer list and write/read pairs between a key's
/// writers and readers, then de-duplicated in a sorted set. This is near-linear
/// in the total footprint size for workloads with bounded key fan-out, versus
/// the naive O(n^2) pairwise comparison.
pub fn build_conflict_graph(footprints: &[TransactionFootprint]) -> ConflictGraph {
    let n = footprints.len();
    let mut index: BTreeMap<&LedgerKey, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, fp) in footprints.iter().enumerate() {
        for key in &fp.read_write {
            index.entry(key).or_default().0.push(i);
        }
        for key in &fp.read_only {
            index.entry(key).or_default().1.push(i);
        }
    }

    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (writers, readers) in index.into_values() {
        for a in 0..writers.len() {
            for b in (a + 1)..writers.len() {
                edges.insert((writers[a], writers[b]));
            }
        }
        for &writer in &writers {
            for &reader in &readers {
                if writer != reader {
                    let (lo, hi) = if writer < reader {
                        (writer, reader)
                    } else {
                        (reader, writer)
                    };
                    edges.insert((lo, hi));
                }
            }
        }
    }

    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a, b) in edges {
        adjacency[a].push(b);
        adjacency[b].push(a);
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

// ---------------------------------------------------------------------------
// CAP-0063 lane model
// ---------------------------------------------------------------------------
//
// CAP-0063 assigns transactions to *lanes*; lanes are intended to run
// concurrently, and each lane is scheduled as its own sequence of stages.
// Slipstream separates the two concerns deliberately:
//
//   1. **Lane assignment** — a pluggable policy ([`LaneAssignment`]) that
//      partitions transactions into lanes. This is where domain knowledge
//      (which keys are hot, which transactions should be isolated) lives. We do
//      not commit to a specific production policy yet.
//   2. **Stage construction** — unchanged: each lane's transactions are
//      scheduled into conflict-free stages with the existing greedy colorer.
//
// The default policy, [`SingleLane`], places every transaction in one lane and
// therefore reproduces the single-lane [`Schedule`] and its metrics *exactly*.

/// A lane assignment policy: maps each transaction (by original index) to a
/// lane id. Lane ids need not be contiguous; [`schedule_lanes`] compacts them
/// into ordered [`Lane`]s.
///
/// Lane assignment is intentionally decoupled from stage construction. A policy
/// is responsible for the *safety* of its partition: transactions that conflict
/// but are placed in different lanes become [`LaneSchedule::cross_lane_conflicts`],
/// which the model reports rather than silently hides.
pub trait LaneAssignment {
    /// Returns the lane id for every transaction, in input order. The returned
    /// vector must have one entry per footprint.
    fn assign(&self, footprints: &[TransactionFootprint]) -> Vec<usize>;

    /// A short identifier for the policy, used in reports.
    fn name(&self) -> &str;
}

/// The default policy: a single lane containing every transaction. Preserves
/// exact single-lane scheduling behavior and metrics.
#[derive(Debug, Clone, Copy, Default)]
pub struct SingleLane;

impl LaneAssignment for SingleLane {
    fn assign(&self, footprints: &[TransactionFootprint]) -> Vec<usize> {
        vec![0; footprints.len()]
    }

    fn name(&self) -> &str {
        "single-lane"
    }
}

/// A structural round-robin policy (txn `i` → lane `i % lanes`).
///
/// This exists to exercise the multi-lane machinery and as a baseline; it is
/// **not** contention-aware and can place conflicting transactions in different
/// lanes (reported via [`LaneSchedule::cross_lane_conflicts`]). Real,
/// contention-aware assignment is a separate concern left to future policies.
#[derive(Debug, Clone, Copy)]
pub struct RoundRobinLanes {
    pub lanes: usize,
}

impl LaneAssignment for RoundRobinLanes {
    fn assign(&self, footprints: &[TransactionFootprint]) -> Vec<usize> {
        let lanes = self.lanes.max(1);
        (0..footprints.len()).map(|i| i % lanes).collect()
    }

    fn name(&self) -> &str {
        "round-robin"
    }
}

/// A single CAP-0063 lane: the transactions assigned to it and their stage
/// schedule. Stage clusters reference the original (global) transaction
/// indices, so a [`Lane`] can be validated against the full footprint list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lane {
    pub id: usize,
    /// Transactions in this lane, in original index order.
    pub txns: Vec<usize>,
    /// The lane's own stage schedule (global indices).
    pub schedule: Schedule,
}

impl Lane {
    /// Number of stages this lane runs in sequence.
    pub fn stage_count(&self) -> usize {
        self.schedule.stage_count()
    }

    /// Number of transactions in the lane.
    pub fn width(&self) -> usize {
        self.txns.len()
    }
}

/// A multi-lane schedule: independent, concurrently-executing lanes, each with
/// its own sequence of stages.
///
/// ## Aggregate metrics
///
/// Because lanes are intended to execute *concurrently*, the aggregate metrics
/// are defined so that the single-lane case matches [`Schedule`] exactly:
///
/// * [`stage_span`](LaneSchedule::stage_span) is the **maximum** stage count
///   over all lanes — the number of sequential stage barriers on the critical
///   (widest) lane, i.e. the parallel span of the whole schedule.
/// * [`total_transactions`](LaneSchedule::total_transactions) is the **sum** of
///   lane widths.
/// * [`parallelism`](LaneSchedule::parallelism) is
///   `total_transactions / stage_span`: the average number of transactions
///   committed per sequential stage across all concurrently-running lanes.
///
/// For a single lane these reduce to that lane's `stage_count`, transaction
/// count, and `Schedule::parallelism` respectively.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LaneSchedule {
    pub lanes: Vec<Lane>,
}

impl LaneSchedule {
    /// Number of lanes.
    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    /// Total transactions across all lanes.
    pub fn total_transactions(&self) -> usize {
        self.lanes.iter().map(Lane::width).sum()
    }

    /// The parallel span: the maximum stage count over lanes (0 if empty).
    pub fn stage_span(&self) -> usize {
        self.lanes.iter().map(Lane::stage_count).max().unwrap_or(0)
    }

    /// Average transactions committed per sequential stage, across all
    /// concurrently-running lanes. `0.0` when there are no stages.
    pub fn parallelism(&self) -> f64 {
        let span = self.stage_span();
        if span == 0 {
            0.0
        } else {
            self.total_transactions() as f64 / span as f64
        }
    }

    /// True if every lane's stages are internally conflict-free.
    ///
    /// This validates *stage construction*. It does not assert that the lane
    /// partition itself is safe — see [`cross_lane_conflicts`](Self::cross_lane_conflicts).
    pub fn is_conflict_free(&self, footprints: &[TransactionFootprint]) -> bool {
        self.lanes
            .iter()
            .all(|lane| lane.schedule.is_conflict_free(footprints))
    }

    /// True if every transaction in `0..n_txns` appears exactly once across all
    /// lanes and stages.
    pub fn is_complete(&self, n_txns: usize) -> bool {
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for lane in &self.lanes {
            for stage in &lane.schedule.stages {
                for &txn in &stage.txns {
                    if !seen.insert(txn) {
                        return false; // duplicate
                    }
                }
            }
        }
        seen.len() == n_txns && (0..n_txns).all(|i| seen.contains(&i))
    }

    /// Pairs of conflicting transactions that were placed in *different* lanes.
    ///
    /// A safe (contention-aware) assignment policy yields none. Structural
    /// policies like [`RoundRobinLanes`] may yield some; reporting them makes
    /// the "lane assignment is a separate concern" boundary explicit and
    /// testable rather than silently unsafe. Returned pairs are `(lo, hi)` with
    /// `lo < hi`, in deterministic sorted order.
    pub fn cross_lane_conflicts(&self, footprints: &[TransactionFootprint]) -> Vec<(usize, usize)> {
        let mut lane_of: BTreeMap<usize, usize> = BTreeMap::new();
        for lane in &self.lanes {
            for &txn in &lane.txns {
                lane_of.insert(txn, lane.id);
            }
        }
        let mut out: BTreeSet<(usize, usize)> = BTreeSet::new();
        let indices: Vec<usize> = lane_of.keys().copied().collect();
        for (pos, &a) in indices.iter().enumerate() {
            for &b in &indices[pos + 1..] {
                if lane_of[&a] != lane_of[&b] && footprints[a].conflicts_with(&footprints[b]) {
                    out.insert((a, b));
                }
            }
        }
        out.into_iter().collect()
    }
}

/// Constructs a [`LaneSchedule`] by applying a lane-assignment policy and then
/// scheduling each lane's transactions into conflict-free stages.
///
/// Lanes are ordered by ascending lane id; within a lane, transactions keep
/// their original relative order. The [`SingleLane`] policy reproduces the
/// output of [`schedule`] exactly (one lane whose `schedule` equals the
/// single-lane [`Schedule`]).
pub fn schedule_lanes(
    footprints: &[TransactionFootprint],
    assignment: &dyn LaneAssignment,
) -> LaneSchedule {
    let lane_of = assignment.assign(footprints);
    assert_eq!(
        lane_of.len(),
        footprints.len(),
        "lane assignment must return one lane id per transaction"
    );

    // Group global indices by lane id, preserving input order within each lane.
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (global_idx, &lane_id) in lane_of.iter().enumerate() {
        groups.entry(lane_id).or_default().push(global_idx);
    }

    let lanes = groups
        .into_iter()
        .map(|(id, txns)| {
            // Schedule the lane's own sub-set, then remap local -> global.
            let sub: Vec<TransactionFootprint> =
                txns.iter().map(|&i| footprints[i].clone()).collect();
            let (_, local_schedule) = schedule(&sub);
            let stages = local_schedule
                .stages
                .into_iter()
                .map(|cluster| Cluster {
                    txns: cluster.txns.into_iter().map(|local| txns[local]).collect(),
                })
                .collect();
            Lane {
                id,
                txns,
                schedule: Schedule { stages },
            }
        })
        .collect();

    LaneSchedule { lanes }
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

    /// Naive pairwise reference used only to verify the index-based builder.
    fn naive_conflict_graph(footprints: &[TransactionFootprint]) -> ConflictGraph {
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

    #[test]
    fn index_graph_matches_naive_reference() {
        let fps = vec![
            hot_key(0),
            hot_key(1),
            hot_key(0),
            TransactionFootprint::new().read(contract_data("C1", "key0")),
            cold(5),
            hot_key(2),
            TransactionFootprint::new().read(contract_data("C1", "key1")),
        ];
        let fast = build_conflict_graph(&fps);
        let naive = naive_conflict_graph(&fps);
        assert_eq!(fast, naive);
    }

    #[test]
    fn index_graph_empty_input() {
        let fps: Vec<TransactionFootprint> = Vec::new();
        let graph = build_conflict_graph(&fps);
        assert_eq!(graph.order(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    // -- lane model ---------------------------------------------------------

    #[test]
    fn single_lane_reproduces_flat_schedule_exactly() {
        // The default policy must produce one lane whose schedule is identical
        // to the single-lane greedy schedule, so existing metrics are unchanged.
        let fps = vec![hot_key(0), hot_key(1), hot_key(0), cold(9)];
        let (_, flat) = schedule(&fps);
        let lane_sched = schedule_lanes(&fps, &SingleLane);

        assert_eq!(lane_sched.lane_count(), 1);
        assert_eq!(lane_sched.lanes[0].id, 0);
        assert_eq!(lane_sched.lanes[0].schedule, flat);
        assert_eq!(lane_sched.stage_span(), flat.stage_count());
        assert_eq!(lane_sched.total_transactions(), flat.transaction_count());
        assert_eq!(lane_sched.parallelism(), flat.parallelism());
        assert!(lane_sched.is_conflict_free(&fps));
        assert!(lane_sched.is_complete(fps.len()));
        assert!(lane_sched.cross_lane_conflicts(&fps).is_empty());
    }

    #[test]
    fn single_lane_of_empty_input_is_well_defined() {
        let fps: Vec<TransactionFootprint> = Vec::new();
        let ls = schedule_lanes(&fps, &SingleLane);
        assert_eq!(ls.total_transactions(), 0);
        assert_eq!(ls.stage_span(), 0);
        assert_eq!(ls.parallelism(), 0.0);
        assert!(ls.is_complete(0));
    }

    #[test]
    fn multi_lane_stages_are_conflict_free_and_complete() {
        // Round-robin over 2 lanes; each lane is scheduled independently and
        // every lane's stages must be internally conflict-free, with every
        // transaction scheduled exactly once.
        let fps: Vec<_> = (0..8).map(|i| hot_key(i % 3)).collect();
        let ls = schedule_lanes(&fps, &RoundRobinLanes { lanes: 2 });
        assert_eq!(ls.lane_count(), 2);
        assert!(ls.is_conflict_free(&fps));
        assert!(ls.is_complete(fps.len()));
        // Global indices are preserved: lane 0 holds evens, lane 1 holds odds.
        assert_eq!(ls.lanes[0].txns, vec![0, 2, 4, 6]);
        assert_eq!(ls.lanes[1].txns, vec![1, 3, 5, 7]);
    }

    #[test]
    fn stage_span_is_the_max_over_lanes() {
        // Lane 0 gets three mutual conflicts on key0 -> 3 stages.
        // Lane 1 gets three independent cold reads -> 1 stage.
        // Span is the max (3), parallelism = 6 txns / 3 stages = 2.0.
        let fps = vec![
            hot_key(0),
            cold(1),
            hot_key(0),
            cold(2),
            hot_key(0),
            cold(3),
        ];
        let ls = schedule_lanes(&fps, &RoundRobinLanes { lanes: 2 });
        assert_eq!(ls.lanes[0].stage_count(), 3);
        assert_eq!(ls.lanes[1].stage_count(), 1);
        assert_eq!(ls.stage_span(), 3);
        assert_eq!(ls.total_transactions(), 6);
        assert_eq!(ls.parallelism(), 2.0);
    }

    #[test]
    fn round_robin_reports_cross_lane_conflicts() {
        // Two writers of the same key forced into different lanes: the model
        // must surface the unsafe pair rather than hide it.
        let fps = vec![hot_key(0), hot_key(0)];
        let ls = schedule_lanes(&fps, &RoundRobinLanes { lanes: 2 });
        assert_eq!(ls.lane_count(), 2);
        assert_eq!(ls.cross_lane_conflicts(&fps), vec![(0, 1)]);
        // Each lane is still internally conflict-free (one txn each).
        assert!(ls.is_conflict_free(&fps));
    }

    #[test]
    fn lane_scheduling_is_deterministic() {
        let fps: Vec<_> = (0..16).map(|i| hot_key(i % 4)).collect();
        let a = schedule_lanes(&fps, &RoundRobinLanes { lanes: 3 });
        let b = schedule_lanes(&fps, &RoundRobinLanes { lanes: 3 });
        assert_eq!(a, b);
    }
}
