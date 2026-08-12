# Detectors

`slipstream-analyzer` ships a suite of heuristic detectors that flag
contention anti-patterns in Soroban contract source. Detectors run on the raw
storage-access records collected by the AST visitor.

## Semantics and limitations

Detectors are **conservative static heuristics**. They may produce false
positives (a flagged pattern that is benign in context) but are designed not
to silently miss an obvious instance of the pattern. Dynamic key expressions
(anything the analyzer cannot resolve to a constant) are treated
conservatively and never assumed safe. Detector output is *evidence for human
review*, not a verdict.

| Detector               | Pattern                                                   | Why it matters                                                        |
| ---------------------- | --------------------------------------------------------- | --------------------------------------------------------------------- |
| `global-static-write`  | One static key written from multiple functions            | A global counter/bucket serializes every writer through one key       |
| `write-in-loop`        | A storage write inside a loop body                        | Write amplification; footprint grows with loop iterations             |
| `read-modify-write`    | A function reads and writes the same key                  | Read-modify-write access serializes concurrent writers to that key    |
| `duplicate-read`       | The same static key read more than once in a function     | Redundant reads amplify the read set                                  |

## Current heuristic limits

- Only calls reachable through a receiver chain containing an identifier
  named `storage` are recognized (e.g. `env.storage().instance()`,
  `env.storage().persistent()`).
- Key resolution recognizes string literals, byte-string literals, simple
  paths (`DataKey::Owner`), and `Symbol::new(...)`/`Name::new(...)`
  constructors; everything else becomes `(dynamic)`.
- Reads and writes are counted per source function; control-flow sensitivity
  (branches, calls into other contracts) is not modelled.
- `delete`-style methods (`remove`, `del`) are treated as writes.

These limits are documented here so detector results are read with the right
expectations. Extending the analyzer (WASM analysis, cross-contract calls,
data-flow through functions) is tracked in the issue tracker.
