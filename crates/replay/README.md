# slipstream-replay

Historical transaction-set profiling for Slipstream.

Reconstructs transaction footprints for a recorded ledger window and runs
scheduling + contention analysis over them. Live sources (Stellar RPC, ledger
archives) are exposed through the [`ProfileSource`] trait; the fully
implemented, deterministic path is fixture-based.

## Quick example

```rust
use slipstream_footprint::keys::contract_data;
use slipstream_footprint::TransactionFootprint;
use slipstream_replay::{TransactionRecord, TransactionSet, profile};

let set = TransactionSet {
    captured_from: Some("example".into()),
    records: vec![TransactionRecord {
        tx_hash: "0x01".into(),
        source_account: "G1".into(),
        footprint: TransactionFootprint::new().read_write(contract_data("C1", "k0")),
    }],
};

let report = profile(&set);
assert_eq!(report.transaction_count, 1);
```

## Sources

- `FixtureSource` — deterministic JSON transaction sets (see the workspace
  `fixtures/` directory).
- `RpcProfileSource`, `ArchiveProfileSource` — integration points for live
  Stellar RPC and ledger archives; `load()` reports `Unavailable` until
  ingestion is implemented.

## License

MIT
