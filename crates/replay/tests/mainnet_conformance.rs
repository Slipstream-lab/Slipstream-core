//! Conformance tests over the *recorded* mainnet capture fixture.
//!
//! `fixtures/mainnet_ledger_63932550.json` is real data recorded from the
//! Stellar mainnet Soroban RPC by `tools/capture_mainnet.py`. These tests pin
//! the invariants documented in `fixtures/README.md`: provenance, counts,
//! hash/source-account validity, ledger range, raw XDR decodability, and the
//! agreement between each recorded source account and its envelope XDR.
//! Re-capture instructions live in `fixtures/README.md`.

use std::path::{Path, PathBuf};

use slipstream_replay::{
    profile, ProfileSource, RecordedCapture, RecordedCaptureSource, ReplayError,
};

fn recorded_capture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("mainnet_ledger_63932550.json")
}

fn load_capture() -> RecordedCapture {
    let source = RecordedCaptureSource::new(recorded_capture_path());
    source
        .load_capture()
        .expect("recorded capture present and valid")
}

#[test]
fn recorded_capture_loads_with_provenance() {
    let capture = load_capture();
    assert_eq!(capture.network, "mainnet");
    assert_eq!(capture.capture_tool, "tools/capture_mainnet.py");
    assert!(!capture.capture_time.is_empty());
    assert!(!capture.rpc_endpoint.is_empty());
    assert!(capture.latest_ledger_at_capture >= capture.ledger_range.to);
    assert!(!capture.transactions.is_empty());
    assert!(capture.captured_from.contains("recorded mainnet capture"));
}

#[test]
fn recorded_capture_counts_match_transactions() {
    let capture = load_capture();
    let statuses = capture
        .transactions
        .iter()
        .map(|t| t.status.as_str())
        .collect::<Vec<_>>();
    assert_eq!(capture.counts.total, capture.transactions.len());
    assert_eq!(
        capture.counts.success,
        statuses.iter().filter(|s| **s == "SUCCESS").count()
    );
    assert_eq!(
        capture.counts.failed,
        statuses.iter().filter(|s| **s == "FAILED").count()
    );
    assert_eq!(
        capture.counts.total,
        capture.counts.success + capture.counts.failed
    );
}

#[test]
fn recorded_capture_hashes_are_hex() {
    let capture = load_capture();
    for t in &capture.transactions {
        assert_eq!(
            t.tx_hash.len(),
            64,
            "tx hash is 32 bytes of hex: {}",
            t.tx_hash
        );
        assert!(
            t.tx_hash.chars().all(|c| c.is_ascii_hexdigit()),
            "tx hash must be hex: {}",
            t.tx_hash
        );
        assert!(
            t.status == "SUCCESS" || t.status == "FAILED",
            "status: {}",
            t.status
        );
    }
}

#[test]
fn recorded_capture_source_accounts_are_valid_strkeys() {
    let capture = load_capture();
    for t in &capture.transactions {
        assert!(
            t.source_account.starts_with('G'),
            "G... strkey: {}",
            t.source_account
        );
        assert_eq!(
            t.source_account.len(),
            56,
            "strkey length: {}",
            t.source_account
        );
        assert!(
            strkey_decode(t.source_account.as_str()).is_some(),
            "strkey checksum must validate: {}",
            t.source_account
        );
    }
}

#[test]
fn recorded_capture_ledgers_within_declared_range() {
    let capture = load_capture();
    let actual = capture
        .transactions
        .iter()
        .map(|t| t.ledger)
        .collect::<Vec<_>>();
    let actual_set = actual
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(capture.ledgers, actual_set.into_iter().collect::<Vec<_>>());
    for l in &actual {
        assert!(
            (*l >= capture.ledger_range.from) && (*l <= capture.ledger_range.to),
            "ledger {l} outside {capture:?}"
        );
    }
    for l in &actual {
        assert!(
            capture.ledgers.contains(l),
            "ledger {l} not listed in `ledgers`"
        );
    }
}

#[test]
fn recorded_capture_xdr_blobs_are_base64() {
    let capture = load_capture();
    for t in &capture.transactions {
        assert!(
            base64_decode(t.envelope_xdr.as_str()).is_some(),
            "envelopeXdr is valid base64"
        );
        assert!(
            base64_decode(t.result_meta_xdr.as_str()).is_some(),
            "resultMetaXdr is valid base64"
        );
    }
}

#[test]
fn recorded_source_account_matches_envelope_xdr() {
    // The recorded source account must be exactly the ed25519 key inside the
    // transaction envelope XDR. This ties the derived field to the raw data.
    let capture = load_capture();
    for t in &capture.transactions {
        let raw = base64_decode(t.envelope_xdr.as_str()).expect("envelope base64");
        let key = envelope_source_key(&raw).expect("parseable envelope");
        let strkey = strkey_decode(t.source_account.as_str()).expect("valid strkey");
        assert_eq!(key, strkey, "source account mismatch for {}", t.tx_hash);
    }
}

#[test]
fn recorded_capture_loads_and_profiles_deterministically() {
    let source = RecordedCaptureSource::new(recorded_capture_path());
    let set = source.load().expect("recorded capture present and valid");
    assert_eq!(set.len(), load_capture().counts.total);
    assert_eq!(
        set.captured_from.as_deref(),
        Some(load_capture().captured_from.as_str())
    );

    // Records carry empty footprints (none are fabricated); profiling must
    // still be deterministic and the schedule complete and conflict-free.
    let r1 = profile(&set);
    let r2 = profile(&set);
    assert_eq!(r1, r2, "profiling recorded data must be deterministic");
    assert_eq!(r1.transaction_count, set.len());
    assert!(r1.schedule.is_complete(set.len()));
    assert!(r1.schedule.is_conflict_free(&set.footprints()));
}

#[test]
fn recorded_capture_is_stable_across_reloads() {
    let set1 = RecordedCaptureSource::new(recorded_capture_path())
        .load()
        .expect("load");
    let set2 = RecordedCaptureSource::new(recorded_capture_path())
        .load()
        .expect("load");
    assert_eq!(set1, set2, "reloading the fixture must be byte-stable");
}

#[test]
fn missing_recorded_capture_is_a_read_error() {
    let source = RecordedCaptureSource::new(Path::new("/does/not/exist.json"));
    assert!(matches!(source.load(), Err(ReplayError::Read { .. })));
}

/// Minimal RFC 4648 base64 decoder (no padding tolerance: accepts both).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const T: [i16; 256] = {
        let mut t = [-1i16; 256];
        let mut i = 0usize;
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        while i < 64 {
            t[alphabet[i] as usize] = i as i16;
            i += 1;
        }
        t
    };
    let bytes = s.bytes().filter(|b| *b != b'=').collect::<Vec<_>>();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for b in bytes {
        let v = T[b as usize];
        if v < 0 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn strkey_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits = 0u32;
    let mut acc = 0u32;
    let mut data = Vec::new();
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|a| *a == c)? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            data.push((acc >> bits) as u8);
        }
    }
    if data.len() < 3 {
        return None;
    }
    let split = data.len() - 2;
    let (with_version, crc_bytes) = data.split_at(split);
    let crc = ((crc_bytes[0] as u16) << 8) | crc_bytes[1] as u16;
    if crc16_xmodem(with_version) != crc {
        return None;
    }
    // First byte is the version byte (0x30 for ed25519 public keys).
    if with_version.first() != Some(&0x30) {
        return None;
    }
    Some(with_version[1..].to_vec())
}

fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Extracts the ed25519 public key from an `ENVELOPE_TYPE_TX` v1 envelope.
fn envelope_source_key(xdr: &[u8]) -> Option<Vec<u8>> {
    fn read_i32(xdr: &[u8], pos: usize) -> Option<i32> {
        let bytes = xdr.get(pos..pos + 4)?;
        Some(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
    let mut pos = 0;
    let envelope_type = read_i32(xdr, pos)?;
    pos += 4;
    if envelope_type != 2 {
        return None; // only ENVELOPE_TYPE_TX v1 is captured
    }
    let key_type = read_i32(xdr, pos)?;
    pos += 4;
    match key_type {
        0 => Some(xdr.get(pos..pos + 32)?.to_vec()), // KEY_TYPE_ED25519
        256 => Some(xdr.get(pos + 8..pos + 40)?.to_vec()), // KEY_TYPE_MUXED_ED25519
        _ => None,
    }
}
