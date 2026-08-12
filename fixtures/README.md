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

## Provenance policy

Fixtures are labelled with their provenance. Files whose data was *measured*
from a live network must record where and when. Files that are illustrative
or synthetic (used purely to exercise the pipeline) must say so explicitly and
must never be presented as measured data. See `mainnet_fragment.json` for an
example of a clearly-labelled illustrative fixture.
