# slipstream-footprint

Ledger key model, read/write set algebra and overlap analysis for Slipstream.

This crate is the foundation of the Slipstream analytical engine. It models
the ledger entries a Soroban transaction touches ([`LedgerKey`]) and the
read/write footprint algebra that determines when two transactions conflict.

## Quick example

```rust
use slipstream_footprint::{TransactionFootprint, keys::contract_data};

let a = TransactionFootprint::new()
    .read(contract_data("C1", "config"))
    .read_write(contract_data("C1", "balance"));

let b = TransactionFootprint::new().read(contract_data("C1", "balance"));

// Read/write on the same key is a conflict; shared read-only is not.
assert!(a.conflicts_with(&b));
let overlap = slipstream_footprint::overlap(&a, &b);
assert!(overlap.is_conflicting());
```

## Model

- [`LedgerKey`] covers accounts, trustlines, contract data, contract code and
  contract TTL entries, encoded as plain strings so the crate stays decoupled
  from any XDR codec.
- A conflict is a key written by either footprint and touched in any mode by
  the other. Shared read-only access is never a conflict.

## License

MIT
