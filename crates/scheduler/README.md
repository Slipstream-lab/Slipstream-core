# slipstream-scheduler

CAP-0063 inspired stage and cluster construction for Slipstream.

Builds the conflict graph over a set of transaction footprints and assigns
every transaction to the earliest stage in which it conflicts with no other
member (deterministic greedy coloring).

## Quick example

```rust
use slipstream_footprint::{TransactionFootprint, keys::contract_data};
use slipstream_scheduler::schedule;

let footprints = vec![
    TransactionFootprint::new().read_write(contract_data("C1", "k0")),
    TransactionFootprint::new().read_write(contract_data("C1", "k1")),
    TransactionFootprint::new().read_write(contract_data("C1", "k0")),
];

let (graph, schedule) = schedule(&footprints);
assert_eq!(schedule.stage_count(), 2);       // k0-writers split across stages
assert!(schedule.is_conflict_free(&footprints));
assert!(schedule.is_complete(footprints.len()));
```

## Model

- `ConflictGraph`: vertices are transaction indices; edges connect conflicting
  pairs. Built in near-linear time via a per-key index.
- `Schedule`: an ordered list of `Cluster`s (one per stage). A stage must be
  conflict-free.
- `serial_schedule`: the no-parallelism baseline for scoring.

## License

MIT
