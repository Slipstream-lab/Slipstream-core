# Fixtures

Recorded or illustrative transaction sets used for conformance and
determinism testing of the replay, scheduler and scoring pipelines.

## Format

Each fixture is a JSON document with:

| Field          | Type                 | Description                                        |
| -------------- | -------------------- | -------------------------------------------------- |
| `captured_from`| `string \| null`     | Provenance note for the capture window             |
| `records`      | `array`              | Ordered transaction records                        |

Each record:

| Field           | Type     | Description                          |
| --------------- | -------- | ------------------------------------ |
| `tx_hash`       | `string` | Transaction hash                     |
| `source_account`| `string` | Transaction source account           |
| `footprint`     | `object` | `{ read_only: LedgerKey[], read_write: LedgerKey[] }` |

`LedgerKey` is the serialized form of `slipstream_footprint::LedgerKey`
(externally tagged JSON, e.g. `{ "ContractData": { "contract_id": "...",
"key": "shard:0" } }`).

## Ledger-archive captures

`archive/capture.xdr` is a binary capture in the Slipstream capture XDR subset
(documented in `crates/replay/src/xdr.rs`), paired with a JSON manifest
(`archive/manifest.json`). It is a truncated, illustrative capture used to
exercise the ledger-archive replay path; see `archive/README.md`.

## Recorded mainnet captures

`mainnet_ledger_63932550.json` is *real* data recorded from the Stellar
mainnet Soroban RPC by `tools/capture_mainnet.py`. It is a 10-transaction
sample of a single ledger (`63932550`), including one failed transaction, so
the conformance tests exercise both statuses. Unlike the illustrative
fixtures it carries only what can be vouched for from the live network and
never fabricates footprints.

Each recorded capture is a JSON document with:

| Field                   | Type       | Description                                        |
| ----------------------- | ---------- | -------------------------------------------------- |
| `captured_from`         | `string`   | Provenance note recorded at capture time           |
| `network`               | `string`   | `mainnet`                                          |
| `ledger_range`          | `object`   | Requested window: `{ from, to }`                   |
| `capture_tool`          | `string`   | Always `tools/capture_mainnet.py`                  |
| `capture_time`          | `string`   | UTC timestamp when the capture ran                 |
| `rpc_endpoint`          | `string`   | RPC endpoint queried                               |
| `latest_ledger_at_capture` | `number`| Network head at capture time                       |
| `counts`                | `object`   | `{ total, success, failed }`                       |
| `ledgers`               | `array`    | Ledgers actually present in `transactions`         |
| `transactions`          | `array`    | Recorded transactions                              |

Each transaction record:

| Field          | Type     | Description                                        |
| -------------- | -------- | -------------------------------------------------- |
| `tx_hash`      | `string` | 64-hex transaction hash                            |
| `source_account`| `string`| G... strkey, decoded from `envelopeXdr`            |
| `status`       | `string` | `SUCCESS` or `FAILED`                              |
| `ledger`       | `number` | Ledger that closed the transaction                 |
| `created_at`   | `number` | Unix close time                                    |
| `envelopeXdr`  | `string` | Base64 `ENVELOPE_TYPE_TX` v1 envelope XDR          |
| `resultMetaXdr`| `string` | Base64 result-meta XDR                             |

### Re-capturing (procedure)

The capture must be re-run against a live Stellar mainnet Soroban RPC. The
tool is deliberately minimal (stdlib-only) and does not decode footprints.

```bash
python3 tools/capture_mainnet.py \
  --start 63932550 --end 63932550 \
  --endpoint https://mainnet.sorobanrpc.com \
  --max-tx 10 \
  --out fixtures/mainnet_ledger_63932550.json
```

Notes for re-runs:

- The endpoint rate-limits requests, so the tool sends a browser-like
  `User-Agent` and retries 403/429/5xx with backoff. Do not raise `--max-tx`
  too aggressively or reduce the inter-page delay.
- `getTransactions` returns transactions oldest-ledger-first and pages by
  cursor; the window `--start..=--end` is the *requested* range and `ledgers`
  records what was actually captured.
- Any new captured file must have its invariants covered by
  `crates/replay/tests/mainnet_conformance.rs` (provenance, counts, hex hashes,
  strkey validity, ledger range, base64 XDR, envelope ↔ source-account match,
  determinism). The fixture file is byte-compared across reloads, so
  re-captures change the committed bytes deliberately — update the fixture in
  the same commit as the re-capture and keep the ledger/range in the filename.

## Provenance policy

Fixtures are labelled with their provenance. Files whose data was *measured*
from a live network must record where and when. Files that are illustrative
or synthetic (used purely to exercise the pipeline) must say so explicitly and
must never be presented as measured data. See `mainnet_fragment.json` for an
example of a clearly-labelled illustrative fixture.
