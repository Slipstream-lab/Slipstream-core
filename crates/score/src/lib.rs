//! Contention metrics, critical-path analysis and hot-key ranking.
//!
//! Given transaction footprints and the schedule constructed for them, this
//! crate produces the numbers that let Slipstream answer "how contentious is
//! this transaction set?" deterministically.

use std::collections::BTreeMap;

use slipstream_footprint::{LedgerKey, TransactionFootprint};
use slipstream_scheduler::{ConflictGraph, Schedule};

/// Per-transaction contention metrics.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TxnMetric {
    pub index: usize,
    /// Number of distinct keys touched.
    pub footprint_size: usize,
    /// Number of keys written.
    pub write_count: usize,
    /// Number of other transactions this transaction conflicts with.
    pub conflict_count: usize,
    /// Stage this transaction was assigned to, if scheduled.
    pub stage: Option<usize>,
}

/// A single hot key and its access profile.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HotKey {
    pub key: LedgerKey,
    /// Number of transactions that read the key.
    pub reads: usize,
    /// Number of transactions that write the key.
    pub writes: usize,
    /// Total transactions touching the key.
    pub touch_count: usize,
}

/// The longest write-conflict chain, respecting the original transaction
/// order. Because edges only point forward in index order, the conflict graph
/// is a DAG and a longest path is well defined.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CriticalPath {
    /// Number of transactions on the longest chain.
    pub length: usize,
    /// The transactions on the chain, in order.
    pub path: Vec<usize>,
}

/// A cost model for weighted contention metrics.
///
/// Weights are applied per access: a key a transaction writes costs
/// `write`, a key it only reads costs `read`. The default model (`read = 1`,
/// `write = 2`) expresses that a write is costlier than a read because it
/// forces ordering with any other touch of the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CostModel {
    pub read: u64,
    pub write: u64,
}

impl Default for CostModel {
    fn default() -> Self {
        CostModel { read: 1, write: 2 }
    }
}

impl CostModel {
    /// A model that weighs every access equally.
    pub fn uniform() -> Self {
        CostModel { read: 1, write: 1 }
    }
}

/// The total access cost of a footprint under a cost model.
pub fn access_cost(fp: &TransactionFootprint, model: &CostModel) -> u64 {
    let reads = fp.read_only.len() + fp.read_write.len();
    let writes = fp.read_write.len();
    reads as u64 * model.read + writes as u64 * model.write
}

/// The longest conflict chain weighted by transaction access costs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WeightedCriticalPath {
    /// Total weighted cost along the chain.
    pub weight: u64,
    /// The transactions on the chain, in order.
    pub path: Vec<usize>,
}

/// A key's share of total conflict cost, attributed to the conflicts that key
/// causes (write/write and write/read pairs).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeyContention {
    pub key: LedgerKey,
    /// Number of conflicting transaction pairs this key participates in.
    pub conflict_pairs: usize,
    /// The cost contribution of those conflicts under the cost model.
    pub cost: u64,
}

/// A complete summary of a transaction set's contention profile.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    pub transaction_count: usize,
    pub distinct_keys: usize,
    pub stage_count: usize,
    pub parallelism: f64,
    pub total_conflicts: usize,
    pub average_conflicts_per_txn: f64,
    pub critical_path: CriticalPath,
    pub weighted_critical_path: WeightedCriticalPath,
    pub hot_keys: Vec<HotKey>,
    pub key_contention: Vec<KeyContention>,
}

/// Computes per-transaction metrics from footprints and a schedule.
pub fn compute_metrics(footprints: &[TransactionFootprint], schedule: &Schedule) -> Vec<TxnMetric> {
    footprints
        .iter()
        .enumerate()
        .map(|(index, fp)| TxnMetric {
            index,
            footprint_size: fp.key_count(),
            write_count: fp.write_count(),
            conflict_count: conflict_count(footprints, index),
            stage: schedule.stage_of(index),
        })
        .collect()
}

fn conflict_count(footprints: &[TransactionFootprint], index: usize) -> usize {
    footprints
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .filter(|(_, fp)| footprints[index].conflicts_with(fp))
        .count()
}

/// Computes the longest conflict chain in the order-respecting conflict DAG.
pub fn critical_path(graph: &ConflictGraph) -> CriticalPath {
    let n = graph.order();
    if n == 0 {
        return CriticalPath {
            length: 0,
            path: Vec::new(),
        };
    }
    // `longest[i]` = longest chain ending at transaction i.
    let mut longest = vec![1usize; n];
    let mut prev = vec![None; n];
    for i in 1..n {
        for &j in &graph.adjacency[i] {
            if j < i && longest[j] + 1 > longest[i] {
                longest[i] = longest[j] + 1;
                prev[i] = Some(j);
            }
        }
    }
    let end = (0..n).max_by_key(|&i| longest[i]).unwrap();
    let length = longest[end];
    let mut path = Vec::new();
    let mut cur = Some(end);
    while let Some(i) = cur {
        path.push(i);
        cur = prev[i];
    }
    path.reverse();
    CriticalPath { length, path }
}

/// Ranks the top `top_n` keys by accesses, weighted toward writes.
pub fn rank_hot_keys(footprints: &[TransactionFootprint], top_n: usize) -> Vec<HotKey> {
    let mut reads: BTreeMap<LedgerKey, usize> = BTreeMap::new();
    let mut writes: BTreeMap<LedgerKey, usize> = BTreeMap::new();
    for fp in footprints {
        for k in &fp.read_only {
            *reads.entry(k.clone()).or_default() += 1;
        }
        for k in &fp.read_write {
            *writes.entry(k.clone()).or_default() += 1;
            *reads.entry(k.clone()).or_default() += 1;
        }
    }
    let mut hot: Vec<HotKey> = reads
        .keys()
        .map(|k| HotKey {
            key: k.clone(),
            reads: reads[k],
            writes: *writes.get(k).unwrap_or(&0),
            touch_count: reads[k],
        })
        .collect();
    hot.sort_by(|a, b| {
        b.writes
            .cmp(&a.writes)
            .then_with(|| b.reads.cmp(&a.reads))
            .then_with(|| a.key.cmp(&b.key))
    });
    hot.truncate(top_n);
    hot
}

/// Aggregates every contention metric for a transaction set and its schedule.
/// Uses the default [`CostModel`] for weighted metrics.
pub fn summarize(
    footprints: &[TransactionFootprint],
    graph: &ConflictGraph,
    schedule: &Schedule,
    top_n_hot_keys: usize,
) -> Summary {
    summarize_with_model(
        footprints,
        graph,
        schedule,
        top_n_hot_keys,
        &CostModel::default(),
    )
}

/// Aggregates every contention metric for a transaction set and its schedule
/// under an explicit cost model.
pub fn summarize_with_model(
    footprints: &[TransactionFootprint],
    graph: &ConflictGraph,
    schedule: &Schedule,
    top_n_hot_keys: usize,
    model: &CostModel,
) -> Summary {
    let metrics = compute_metrics(footprints, schedule);
    let total_conflicts: usize = metrics.iter().map(|m| m.conflict_count).sum();
    let distinct_keys = footprints
        .iter()
        .flat_map(TransactionFootprint::keys)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    Summary {
        transaction_count: footprints.len(),
        distinct_keys,
        stage_count: schedule.stage_count(),
        parallelism: schedule.parallelism(),
        total_conflicts,
        average_conflicts_per_txn: if footprints.is_empty() {
            0.0
        } else {
            total_conflicts as f64 / footprints.len() as f64
        },
        critical_path: critical_path(graph),
        weighted_critical_path: critical_path_weighted(graph, footprints, model),
        hot_keys: rank_hot_keys(footprints, top_n_hot_keys),
        key_contention: key_contention(footprints, model),
    }
}

/// Computes the weighted longest conflict chain.
///
/// Each transaction contributes its [`access_cost`]; the weight of a chain is
/// the sum of its transactions' costs.
pub fn critical_path_weighted(
    graph: &ConflictGraph,
    footprints: &[TransactionFootprint],
    model: &CostModel,
) -> WeightedCriticalPath {
    let n = graph.order();
    if n == 0 {
        return WeightedCriticalPath {
            weight: 0,
            path: Vec::new(),
        };
    }
    let costs: Vec<u64> = footprints.iter().map(|fp| access_cost(fp, model)).collect();
    let mut longest = costs.clone();
    let mut prev = vec![None; n];
    for i in 1..n {
        for &j in &graph.adjacency[i] {
            if j < i && longest[j] + costs[i] > longest[i] {
                longest[i] = longest[j] + costs[i];
                prev[i] = Some(j);
            }
        }
    }
    let end = (0..n).max_by_key(|&i| longest[i]).unwrap();
    let mut path = Vec::new();
    let mut cur = Some(end);
    while let Some(i) = cur {
        path.push(i);
        cur = prev[i];
    }
    path.reverse();
    WeightedCriticalPath {
        weight: longest[end],
        path,
    }
}

/// Attributes conflict cost to the keys that cause it, ranked by cost.
///
/// A write/write pair contributes twice the write cost; a write/read pair
/// contributes write + read cost.
pub fn key_contention(
    footprints: &[TransactionFootprint],
    model: &CostModel,
) -> Vec<KeyContention> {
    let mut index: BTreeMap<&LedgerKey, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, fp) in footprints.iter().enumerate() {
        for key in &fp.read_write {
            index.entry(key).or_default().0.push(i);
        }
        for key in &fp.read_only {
            index.entry(key).or_default().1.push(i);
        }
    }
    let mut out: Vec<KeyContention> = index
        .into_iter()
        .map(|(key, (writers, readers))| {
            let w = writers.len() as u64;
            let r = readers.len() as u64;
            let ww_pairs = w.saturating_mul(w.saturating_sub(1)) / 2;
            let wr_pairs = w * r;
            let conflict_pairs = (ww_pairs + wr_pairs) as usize;
            let cost = ww_pairs * 2 * model.write + wr_pairs * (model.write + model.read);
            KeyContention {
                key: key.clone(),
                conflict_pairs,
                cost,
            }
        })
        .collect();
    out.sort_by(|a, b| b.cost.cmp(&a.cost).then_with(|| a.key.cmp(&b.key)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipstream_footprint::keys::contract_data;
    use slipstream_scheduler::{build_conflict_graph, greedy_schedule, serial_schedule};

    fn writer(key: u32) -> TransactionFootprint {
        TransactionFootprint::new().read_write(contract_data("C1", format!("k{key}")))
    }

    #[test]
    fn metrics_count_conflicts_and_stages() {
        let fps = vec![writer(0), writer(1), writer(0)];
        let (_, schedule) = {
            let g = build_conflict_graph(&fps);
            let s = greedy_schedule(&g);
            (g, s)
        };
        let metrics = compute_metrics(&fps, &schedule);
        assert_eq!(metrics[0].conflict_count, 1); // conflicts with txn 2
        assert_eq!(metrics[2].conflict_count, 1);
        assert_eq!(metrics[1].conflict_count, 0);
        assert_eq!(metrics[0].stage, Some(0));
        assert_eq!(metrics[2].stage, Some(1));
    }

    #[test]
    fn critical_path_follows_longest_chain() {
        // k0 written by t0, t2, t4 -> chain of length 3 (0 -> 2 -> 4).
        // k1 written by t1, t3 -> chain of length 2.
        let fps = vec![writer(0), writer(1), writer(0), writer(1), writer(0)];
        let graph = build_conflict_graph(&fps);
        let cp = critical_path(&graph);
        assert_eq!(cp.length, 3);
        assert_eq!(cp.path, vec![0, 2, 4]);
    }

    #[test]
    fn critical_path_of_empty_graph_is_zero() {
        let graph = build_conflict_graph(&[]);
        let cp = critical_path(&graph);
        assert_eq!(cp.length, 0);
        assert!(cp.path.is_empty());
    }

    #[test]
    fn hot_keys_rank_writes_first() {
        let fps = vec![
            TransactionFootprint::new()
                .read(contract_data("C1", "config"))
                .read_write(contract_data("C1", "count")),
            TransactionFootprint::new().read_write(contract_data("C1", "count")),
            TransactionFootprint::new().read(contract_data("C1", "config")),
        ];
        let hot = rank_hot_keys(&fps, 10);
        assert_eq!(hot[0].key, contract_data("C1", "count"));
        assert_eq!(hot[0].writes, 2);
        assert_eq!(hot[0].reads, 2);
        assert_eq!(hot[1].writes, 0);
    }

    #[test]
    fn summary_aggregates_deterministically() {
        let fps = vec![writer(0), writer(1), writer(0)];
        let graph = build_conflict_graph(&fps);
        let schedule = greedy_schedule(&graph);
        let s = summarize(&fps, &graph, &schedule, 5);
        assert_eq!(s.transaction_count, 3);
        assert_eq!(s.stage_count, 2);
        assert_eq!(s.total_conflicts, 2);
        assert_eq!(s.critical_path.length, 2);
        assert!((s.parallelism - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn serial_baseline_has_parallelism_one() {
        let fps = vec![writer(0), writer(1), writer(0)];
        let schedule = serial_schedule(fps.len());
        assert_eq!(schedule.parallelism(), 1.0);
        assert!(schedule.is_conflict_free(&fps));
    }

    #[test]
    fn access_cost_uses_default_model() {
        let model = CostModel::default();
        let fp = TransactionFootprint::new()
            .read(contract_data("C1", "config"))
            .read_write(contract_data("C1", "count"));
        // reads = 2 (one read-only + one read-write), writes = 1 -> 2*1 + 1*2
        assert_eq!(access_cost(&fp, &model), 4);
    }

    #[test]
    fn weighted_critical_path_sums_node_costs() {
        let fps = vec![writer(0), writer(0)];
        let graph = build_conflict_graph(&fps);
        let w = critical_path_weighted(&graph, &fps, &CostModel::default());
        // each writer: 1 read + 1 write = 3 under default model
        assert_eq!(w.weight, 6);
        assert_eq!(w.path, vec![0, 1]);
    }

    #[test]
    fn weights_can_change_which_chain_dominates() {
        // Unweighted longest chain is 0 -> 2 (length 2), but the single
        // heavier transaction 1 outweighs it under the default cost model.
        let fps = vec![
            writer(0),
            TransactionFootprint::new()
                .read_write(contract_data("C1", "k1"))
                .read(contract_data("C1", "a"))
                .read(contract_data("C1", "b"))
                .read(contract_data("C1", "c"))
                .read(contract_data("C1", "d"))
                .read(contract_data("C1", "e")),
            writer(0),
        ];
        let graph = build_conflict_graph(&fps);
        assert_eq!(critical_path(&graph).length, 2);
        let w = critical_path_weighted(&graph, &fps, &CostModel::default());
        assert_eq!(w.path, vec![1]);
        // reads = 1 + 5, writes = 1 -> cost 6*1 + 1*2 = 8
        assert_eq!(w.weight, 8);
    }

    #[test]
    fn key_contention_attributes_conflict_cost() {
        let fps = vec![
            writer(0),
            writer(0),
            TransactionFootprint::new().read(contract_data("C1", "k0")),
        ];
        let contention = key_contention(&fps, &CostModel::default());
        assert_eq!(contention.len(), 1);
        let entry = &contention[0];
        assert_eq!(entry.conflict_pairs, 3); // 1 write/write + 2 write/read
        assert_eq!(entry.cost, 10);
    }
}
