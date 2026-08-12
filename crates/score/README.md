# slipstream-score

Contention metrics, critical-path analysis and hot-key ranking for Slipstream.

Turns transaction footprints and a schedule into deterministic numbers:
per-transaction metrics, critical-path length, weighted critical path under a
[`CostModel`], hot-key rankings, and per-key contention attribution.

## Quick example

```rust
use slipstream_footprint::{TransactionFootprint, keys::contract_data};
use slipstream_scheduler::schedule;
use slipstream_score::summarize;

let footprints = vec![
    TransactionFootprint::new().read_write(contract_data("C1", "k0")),
    TransactionFootprint::new().read_write(contract_data("C1", "k1")),
    TransactionFootprint::new().read_write(contract_data("C1", "k0")),
];

let (graph, schedule) = schedule(&footprints);
let summary = summarize(&footprints, &graph, &schedule, 5);

assert_eq!(summary.critical_path.length, 2);
assert_eq!(summary.hot_keys[0].key, contract_data("C1", "k0"));
```

## Metrics

- Per-transaction metrics (`TxnMetric`): footprint size, write count, conflict
  count, assigned stage.
- `CriticalPath`: longest conflict chain (a lower bound on serial depth).
- `WeightedCriticalPath`: the same chain weighted by transaction access costs
  under a [`CostModel`] (default read=1, write=2).
- `rank_hot_keys`: keys ordered by writes then reads.
- `key_contention`: conflict cost attributed to the key that caused it.

## License

MIT
