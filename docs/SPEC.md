# Slipstream Core — Architecture and Analytical Model

Slipstream answers a question Stellar developers cannot currently answer with
off-the-shelf tooling: **how efficiently do a smart contract's transaction
footprints parallelize under Stellar's phased execution model?**

This document describes the architecture of `slipstream-core` and the formal
model behind its analysis.

## 1. High-level pipeline

```
contract sources ──▶ analyzer ──────────────┐   (static footprint inference)
                                            ▼
recorded tx set ──▶ replay ──▶ footprints ─▶ scheduler ──▶ schedule
                                            │                  │
                                            └──────▶ score ◀───┘
                                                         │
                                                    report / CLI
```

Two ingestion paths converge on the same core representation:

1. **Static path** (`slipstream-analyzer`): parse Soroban contract source with
   `syn`, infer the storage keys each function reads and writes, and flag
   contention anti-patterns.
2. **Replay path** (`slipstream-replay`): reconstruct footprints for recorded
   ledger windows from RPC / ledger archives (currently fixture-based).

Both produce `TransactionFootprint` values (in `slipstream-footprint`), which
are consumed by the scheduler and the scorer.

## 2. The footprint model

A transaction's **footprint** is the set of ledger entries it touches, split
into `read_only` and `read_write` sets. Ledger entries are modelled as
[`LedgerKey`] values: accounts, trustlines, contract data, contract code, and
contract TTL entries.

**Conflict rule.** Two transactions conflict when one writes a key the other
touches in any mode (write/write or write/read). Shared read-only access is
never a conflict. This mirrors the invariant Stellar enforces when grouping
transactions: transactions that run in the same stage must be conflict-free.

## 3. Scheduling (CAP-0063 model)

Stellar's phased execution proposal assigns transactions to lanes and runs
each lane in stages; within a stage transactions execute in parallel, so a
stage must contain no conflicting pair. `slipstream-scheduler` models this
as:

- a **conflict graph** — vertices are transactions, edges connect conflicting
  pairs (built in O(n^2) key-set intersections);
- a **conflict graph** — vertices are transactions, edges connect conflicting
  pairs. Built in near-linear time (for bounded per-key fan-out) via a per-key
  index of writers and readers, rather than O(n^2) pairwise intersection;
- a **schedule** — an ordered list of *clusters* (stages), each a set of
  mutually conflict-free transactions;
- a **greedy stage assignment** — each transaction, in index order, is placed
  in the earliest stage with which it has no conflict.

The greedy scheduler is deterministic and produces a valid (conflict-free,
complete) schedule for any input. It is not optimal; improving it is tracked
as a roadmap item.

### 3.1 Lanes

CAP-0063 additionally partitions transactions into **lanes** that are intended
to run concurrently, each lane scheduled as its own sequence of stages.
Slipstream keeps the two concerns separate:

- **Lane assignment** (`LaneAssignment`) is a pluggable policy that partitions
  transactions into lanes. This is where contention-aware domain knowledge
  belongs; the model does not commit to a production policy. The default
  `SingleLane` places every transaction in one lane and reproduces the flat
  `Schedule` and its metrics *exactly*. `RoundRobinLanes` is a structural
  baseline used to exercise the machinery.
- **Stage construction** is unchanged: each lane's transactions are scheduled
  with the same greedy colorer.

A `LaneSchedule`'s aggregate metrics are defined so the single-lane case
matches `Schedule` exactly:

- `stage_span` = **max** stage count over lanes — the number of sequential
  stage barriers on the critical (widest) lane, i.e. the parallel span of the
  whole schedule when lanes run concurrently;
- `total_transactions` = **sum** of lane widths;
- `parallelism` = `total_transactions / stage_span` — average transactions
  committed per sequential stage across all concurrently-running lanes.

Because lane assignment is decoupled from stage construction, a policy may
place conflicting transactions in different lanes. Such pairs are surfaced
explicitly as `cross_lane_conflicts` rather than silently hidden: a safe,
contention-aware policy yields none, and the model makes an unsafe partition
observable and testable.

## 4. Contention scoring

`slipstream-score` derives numbers that are comparable across contracts and
transaction sets:

- **Per-transaction metrics** — footprint size, write count, number of
  conflicting peers, assigned stage.
- **Critical path** — the longest write-conflict chain respecting transaction
  order. Because conflict edges are directed forward in index order, the
  conflict graph is a DAG and the longest path is well defined; it is a lower
  bound on the serial depth of the workload.
- **Weighted critical path** — the same chain, but each transaction contributes
  its access cost under a [`CostModel`] (default: read = 1, write = 2). This
  ranks *why* a workload is slow: a heavy transaction on a short chain can
  dominate a longer chain of cheap ones.
- **Key contention contribution** — conflict cost attributed to the key that
  caused it (write/write pairs count double the write cost, write/read pairs
  count write + read), ranked by cost. This surfaces the concrete keys to
  shard or de-amp.
- **Parallelism** — average transactions per stage (`n_txns / stages`).
- **Hot-key ranking** — keys ordered by writes (primary) and reads
  (secondary). Hot keys are the concrete targets of contract optimization.

## 5. Static analysis

`slipstream-analyzer` walks contract source and records every call to a
storage method (`get`, `has`, `put`, `set`, `remove`, `del`) on a receiver
that references `storage`. Key expressions are resolved to `StaticKey`
segment lists; unresolvable expressions become the `(dynamic)` segment. The
heuristic detector suite is documented in [DETECTORS.md](DETECTORS.md).

Static analysis is deliberately conservative: it reports *potential*
contention and never claims a dynamic key is safe.

## 6. Replay

`slipstream-replay` defines the `ProfileSource` trait over historical
transaction sets. Live RPC and ledger-archive sources are integration points
whose `load()` reports `Unavailable` until the respective services are wired
up. The fixture-based source is fully implemented and deterministic.

## 7. Crate boundaries and dependencies

Dependencies point strictly downward:

```
cli ──▶ replay ──▶ score ──▶ scheduler ──▶ footprint
        └──────▶ analyzer ────────────────┘
```

`slipstream-footprint` has no workspace dependencies. No cycles are allowed.

## 8. CLI

`slipstream` provides four commands:

| Command     | Purpose                                                        |
| ----------- | -------------------------------------------------------------- |
| `scan`      | Static analysis + detectors over contract sources              |
| `profile`   | Replay + scheduling + scoring of a recorded transaction set    |
| `simulate`  | Scheduling/scoring over a deterministic synthetic set          |
| `diff`      | Comparison of two implementations (naive vs optimized)         |

## 9. Determinism and correctness guarantees

- All core algorithms are deterministic: identical input produces identical
  output across platforms and runs.
- Schedules are validated for conflict-freedom and completeness before they
  are trusted (tests assert both).
- Fixtures carry explicit provenance labels; illustrative data is never
  presented as measured data.
- No analytical result is fabricated to satisfy a test.

## 10. Roadmap

The issue tracker contains the full roadmap. High-level threads: RPC and
archive replay ingestion, optimal schedule construction, benchmark harness
(Criterion + recorded workloads), WASM/foreign-function analysis in the
analyzer, and publishing individual crates to crates.io.
