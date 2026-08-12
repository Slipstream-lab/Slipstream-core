# Slipstream Core

The core analytical engine of Slipstream: transaction-footprint analysis,
contention analysis, CAP-0063 stage/cluster construction, static contract
analysis, historical replay, and scoring.

Slipstream measures how efficiently a Soroban smart-contract's transaction
footprints parallelize under Stellar's phased execution model. It turns
"this contract is contention-heavy" into concrete, reproducible evidence:
footprint overlap, conflict graphs, critical-path length, hot-key rankings,
and detector findings for known anti-patterns.

## Workspace layout

```
crates/
├── footprint/   # LedgerKey model, read/write set algebra, overlap analysis
├── scheduler/   # CAP-0063 inspired stage and cluster construction
├── score/       # Contention metrics, critical path, hot-key ranking
├── analyzer/    # syn-based static footprint inference and detectors
├── replay/      # Historical transaction-set profiling (RPC / ledger archives)
└── cli/         # slipstream scan | profile | simulate | diff
```

## Quick start

```sh
cargo build --workspace
cargo test --workspace
```

The command-line interface:

```sh
# Statically analyze a directory of Soroban contract sources
slipstream scan path/to/contracts

# Profile a recorded transaction set (fixture or future RPC source)
slipstream profile --fixture fixtures/mainnet_fragment.json

# Simulate scheduling over a synthetic transaction set
slipstream simulate --transactions 128 --seed 42

# Diff two contract implementations (naive vs optimized)
slipstream diff path/to/naive path/to/optimized
```

## Architecture and analytical model

See [docs/SPEC.md](docs/SPEC.md) for the full architecture and the formal
analytical model behind footprints, conflicts, and contention scoring.
[docs/DETECTORS.md](docs/DETECTORS.md) documents the static-analysis
detectors and their known limitations.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). All contributors are expected to
abide by the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Licensed under the MIT license. See [LICENSE](LICENSE).
