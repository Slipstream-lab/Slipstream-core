#!/usr/bin/env python3
"""Captures a real mainnet transaction set from a Stellar Soroban RPC endpoint.

This is the *recorded-data capture* path for the conformance fixtures in
`fixtures/`. It queries `getTransactions` for a ledger window, extracts the
transaction hash and source account (decoded from the envelope XDR), and writes
`fixtures/<network>_ledgers_<from>-<to>.json` with full provenance.

The footprint XDR is NOT decoded here: the capture records only the fields
Slipstream can vouch for (hash, source account, status, ledger, timestamp) plus
the raw envelope/result XDR for later decoding. It deliberately does not
fabricate read/write footprints.

Usage:
    python3 tools/capture_mainnet.py --start 63932550 --end 63932555 \
        --endpoint https://mainnet.sorobanrpc.com --max-tx 10 \
        --out fixtures/mainnet_ledgers_63932550-63932555.json
"""
import argparse
import base64
import datetime
import json
import ssl
import struct
import time
import urllib.error
import urllib.request

PAGE_SIZE = 100
MAX_TX = 10
KEY_TYPE_ED25519 = 0
ENVELOPE_TYPE_TX = 2

# strkey version bytes (ed25519 public key -> "G...")
STRKEY_ED25519 = 6 << 3

# The Python stdlib often lacks the OS CA bundle on macOS; this capture tool is
# a one-shot data-gathering script, so skip cert verification.
_CTX = ssl.create_default_context()
_CTX.check_hostname = False
_CTX.verify_mode = ssl.CERT_NONE


def crc16_xmodem(data: bytes) -> int:
    crc = 0
    for b in data:
        crc ^= b << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
    return crc


def strkey_encode(payload: bytes) -> str:
    with_ver = bytes([STRKEY_ED25519]) + payload
    crc = crc16_xmodem(with_ver)
    return base64.b32encode(with_ver + struct.pack(">H", crc)).decode().rstrip("=")


def decode_source_account(envelope_xdr_b64: str) -> str:
    """Decodes the transaction source account from an ENVELOPE_TYPE_TX v1
    envelope XDR. Returns the strkey (G...) form."""
    raw = base64.b64decode(envelope_xdr_b64)
    pos = 0
    (envelope_type,) = struct.unpack_from(">i", raw, pos)
    pos += 4
    if envelope_type != ENVELOPE_TYPE_TX:
        return "UNKNOWN"
    (key_type,) = struct.unpack_from(">i", raw, pos)
    pos += 4
    if key_type == KEY_TYPE_ED25519:
        pubkey = raw[pos : pos + 32]
        return strkey_encode(pubkey)
    # KEY_TYPE_MUXED_ED25519 = 256: skip 8-byte id, then ed25519 key.
    pos += 8
    pubkey = raw[pos : pos + 32]
    return strkey_encode(pubkey)


def rpc(endpoint: str, method: str, params: dict):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    last_err = None
    for attempt in range(4):
        try:
            req = urllib.request.Request(
                endpoint,
                data=body,
                headers={
                    "Content-Type": "application/json",
                    "User-Agent": "slipstream-capture/0.1 (conformance fixtures)",
                },
            )
            with urllib.request.urlopen(req, timeout=30, context=_CTX) as resp:
                data = json.load(resp)
            if "error" in data:
                raise RuntimeError(f"RPC error: {data['error']}")
            return data["result"]
        except urllib.error.HTTPError as e:
            last_err = e
            if e.code in (429, 403, 500, 502, 503):
                time.sleep(1.5 * (attempt + 1))
                continue
            raise
    raise last_err


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--start", type=int, required=True)
    ap.add_argument("--end", type=int, required=True)
    ap.add_argument("--endpoint", default="https://mainnet.sorobanrpc.com")
    ap.add_argument("--max-tx", type=int, default=MAX_TX)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    captures = []
    cursor = None
    latest = rpc(args.endpoint, "getLatestLedger", {})["sequence"]

    while len(captures) < args.max_tx:
        params = {"startLedger": args.start, "limit": PAGE_SIZE}
        if cursor:
            params["cursor"] = cursor
        result = rpc(args.endpoint, "getTransactions", params)
        out_of_window = False
        for t in result.get("transactions", []):
            ledger = t["ledger"]
            if ledger > args.end:
                out_of_window = True
                break
            captures.append(
                {
                    "tx_hash": t["txHash"],
                    "source_account": decode_source_account(t["envelopeXdr"]),
                    "status": t["status"],
                    "ledger": ledger,
                    "created_at": t.get("createdAt"),
                    "envelopeXdr": t["envelopeXdr"],
                    "resultMetaXdr": t["resultMetaXdr"],
                }
            )
            if len(captures) >= args.max_tx:
                break
        if out_of_window:
            break
        next_cursor = result.get("cursor")
        if not next_cursor or next_cursor == cursor:
            break
        cursor = next_cursor
        time.sleep(0.5)

    captures.sort(key=lambda c: (c["ledger"], c["tx_hash"]))
    successful = [c for c in captures if c["status"] == "SUCCESS"]
    failed = [c for c in captures if c["status"] == "FAILED"]
    ledgers = sorted({c["ledger"] for c in captures})

    doc = {
        "captured_from": (
            "recorded mainnet capture: sample subset (up to --max-tx) of the "
            f"real transaction set in ledger range {args.start}..={args.end} "
            "from the Stellar mainnet Soroban RPC. Network=mainnet, captured "
            f"{datetime.datetime.now(datetime.UTC).isoformat()}Z "
            f"via tools/capture_mainnet.py from {args.endpoint}. "
            "Only hashes, source accounts, status and raw XDR are recorded; "
            "footprints are not decoded here."
        ),
        "network": "mainnet",
        "ledger_range": {"from": args.start, "to": args.end},
        "capture_tool": "tools/capture_mainnet.py",
        "capture_time": datetime.datetime.now(datetime.UTC).isoformat() + "Z",
        "rpc_endpoint": args.endpoint,
        "latest_ledger_at_capture": latest,
        "counts": {"total": len(captures), "success": len(successful), "failed": len(failed)},
        "ledgers": ledgers,
        "transactions": captures,
    }

    with open(args.out, "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")

    print(
        f"wrote {args.out}: {len(captures)} tx ({len(successful)} success, "
        f"{len(failed)} failed) over ledgers {ledgers[0]}..={ledgers[-1]}"
    )


if __name__ == "__main__":
    main()
