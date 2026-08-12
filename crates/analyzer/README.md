# slipstream-analyzer

syn-based static footprint inference and contention detectors for Slipstream.

Parses Soroban contract source with [`syn`] and infers which storage keys each
function reads and writes, then runs heuristic detectors that flag contention
anti-patterns (global static-key writes, writes in loops, read-modify-write
patterns, duplicate reads).

## Quick example

```rust
use slipstream_analyzer::analyze;

let source = r#"
#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, Env};

#[contract]
pub struct Counter;

#[contractimpl]
impl Counter {
    pub fn increment(env: Env) -> u32 {
        let mut n: u32 = env.storage().instance().get(&symbol_short!("count")).unwrap_or(0);
        n += 1;
        env.storage().instance().put(&symbol_short!("count"), &n);
        n
    }
}
"#;

let report = analyze(source, "counter.rs").unwrap();
assert_eq!(report.functions.len(), 1);
assert_eq!(report.functions[0].storage_writes.len(), 1);
```

## Detectors

`global-static-write`, `write-in-loop`, `read-modify-write`,
`duplicate-read`. Detectors are conservative heuristics: they may produce
false positives but should not silently miss an obvious pattern. Dynamic key
expressions are treated conservatively. See `docs/DETECTORS.md` in the
workspace for semantics and limitations.

## License

MIT
