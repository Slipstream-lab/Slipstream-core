//! Minimal, self-contained XDR wire codec for ledger-archive captures.
//!
//! Slipstream deliberately does not couple to a full XDR schema: it is an
//! analytical engine and only needs the ledger keys a transaction touches.
//! This module implements just enough of the XDR wire format (big-endian,
//! 4-byte aligned, length-prefixed) to read and write the *capture subset*
//! that a replay source — ledger archive or RPC — is expected to emit.
//!
//! # Capture format (version 1)
//!
//! A capture is a single XDR document with this shape:
//!
//! ```text
//! Capture =
//!   u32 magic            // 0x534C5031 ("SLP1")
//!   u32 version          // 1
//!   u32 from_ledger
//!   u32 to_ledger
//!   u32 checkpoint_count
//!   TransactionRecord records<>
//!
//! TransactionRecord =
//!   opaque tx_hash[32]
//!   string source_account
//!   LedgerKey read_only<>
//!   LedgerKey read_write<>
//!
//! LedgerKey =
//!   u32 type             // see [`LedgerKeyType`]
//!   string field_1       // account/contract id, or raw for `Other`
//!   string field_2       // asset / contract key; empty when unused
//! ```
//!
//! The schema intentionally mirrors only the footprint-relevant slice of the
//! Stellar transaction set. Sources that produce captures are responsible for
//! translating their native transaction meta into this subset; both
//! [`crate::ArchiveProfileSource`] and [`crate::RpcProfileSource`] decode
//! through the same [`decode_capture`] entry point (no duplicated decoding).

use std::borrow::Cow;
use std::fmt;

use slipstream_footprint::LedgerKey;

/// Magic bytes identifying a Slipstream capture document.
pub const MAGIC: u32 = 0x534C_5031;
/// Current capture format version.
pub const VERSION: u32 = 1;

/// Errors produced while decoding XDR capture bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XdrError {
    /// The input ended before the field could be read.
    Overrun,
    /// A boolean field held a value other than 0 or 1.
    BadBool(u32),
    /// An opaque/string length exceeds the remaining input.
    LengthExceedsInput(usize, usize),
    /// A UTF-8 string field was not valid UTF-8.
    InvalidUtf8,
    /// The capture magic header did not match [`MAGIC`].
    BadMagic(u32),
    /// The capture version is not supported by this decoder.
    UnsupportedVersion(u32),
    /// An unknown [`LedgerKeyType`] discriminant was encountered.
    UnknownKeyType(u32),
}

impl fmt::Display for XdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XdrError::Overrun => write!(f, "XDR input overrun"),
            XdrError::BadBool(v) => write!(f, "invalid XDR bool value {v}"),
            XdrError::LengthExceedsInput(len, remain) => {
                write!(f, "XDR length {len} exceeds remaining input {remain}")
            }
            XdrError::InvalidUtf8 => write!(f, "XDR string field is not valid UTF-8"),
            XdrError::BadMagic(m) => write!(f, "bad capture magic 0x{m:08x}"),
            XdrError::UnsupportedVersion(v) => write!(f, "unsupported capture version {v}"),
            XdrError::UnknownKeyType(t) => write!(f, "unknown LedgerKey type {t}"),
        }
    }
}

impl std::error::Error for XdrError {}

/// The ledger entry type encoded in a capture [`LedgerKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerKeyType {
    Account = 0,
    TrustLine = 1,
    ContractData = 2,
    ContractCode = 3,
    ContractTtl = 4,
    Other = 255,
}

impl LedgerKeyType {
    fn from_discriminant(v: u32) -> Result<Self, XdrError> {
        match v {
            0 => Ok(Self::Account),
            1 => Ok(Self::TrustLine),
            2 => Ok(Self::ContractData),
            3 => Ok(Self::ContractCode),
            4 => Ok(Self::ContractTtl),
            255 => Ok(Self::Other),
            other => Err(XdrError::UnknownKeyType(other)),
        }
    }

    fn discriminant(self) -> u32 {
        self as u32
    }
}

/// A ledger key decoded from a capture, mirroring
/// [`slipstream_footprint::LedgerKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureKey {
    pub key_type: LedgerKeyType,
    pub field_1: String,
    pub field_2: String,
}

impl CaptureKey {
    /// Maps the capture key onto the footprint model.
    pub fn to_ledger_key(&self) -> LedgerKey {
        match self.key_type {
            LedgerKeyType::Account => LedgerKey::Account {
                account_id: self.field_1.clone(),
            },
            LedgerKeyType::TrustLine => LedgerKey::TrustLine {
                account_id: self.field_1.clone(),
                asset: self.field_2.clone(),
            },
            LedgerKeyType::ContractData => LedgerKey::ContractData {
                contract_id: self.field_1.clone(),
                key: self.field_2.clone(),
            },
            LedgerKeyType::ContractCode => LedgerKey::ContractCode {
                contract_id: self.field_1.clone(),
            },
            LedgerKeyType::ContractTtl => LedgerKey::ContractTtl {
                contract_id: self.field_1.clone(),
            },
            LedgerKeyType::Other => LedgerKey::Other(self.field_1.clone()),
        }
    }
}

/// One transaction recovered from an archive capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRecord {
    pub tx_hash: [u8; 32],
    pub source_account: String,
    pub read_only: Vec<CaptureKey>,
    pub read_write: Vec<CaptureKey>,
}

/// The decoded contents of an archive capture document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveCapture {
    pub from_ledger: u32,
    pub to_ledger: u32,
    pub checkpoint_count: u32,
    pub records: Vec<CaptureRecord>,
}

/// Reads big-endian, 4-byte-aligned XDR fields from a byte slice.
pub struct XdrReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> XdrReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_n(&mut self, n: usize) -> Result<&'a [u8], XdrError> {
        if n > self.remaining() {
            return Err(XdrError::Overrun);
        }
        let end = self.pos + n;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn read_u32(&mut self) -> Result<u32, XdrError> {
        let b = self.read_n(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_u64(&mut self) -> Result<u64, XdrError> {
        let hi = u64::from(self.read_u32()?);
        let lo = u64::from(self.read_u32()?);
        Ok((hi << 32) | lo)
    }

    pub fn read_i64(&mut self) -> Result<i64, XdrError> {
        Ok(self.read_u64()? as i64)
    }

    pub fn read_bool(&mut self) -> Result<bool, XdrError> {
        match self.read_u32()? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(XdrError::BadBool(v)),
        }
    }

    /// Reads `n` opaque bytes, consuming the padding to the 4-byte boundary.
    pub fn read_opaque(&mut self, n: usize) -> Result<&'a [u8], XdrError> {
        let out = self.read_n(n)?;
        self.read_n(pad(n))?;
        Ok(out)
    }

    /// Reads a length-prefixed byte field, including padding.
    pub fn read_bytes(&mut self) -> Result<&'a [u8], XdrError> {
        let len = self.read_u32()? as usize;
        self.read_opaque(len)
    }

    /// Reads a length-prefixed UTF-8 string field.
    pub fn read_string(&mut self) -> Result<Cow<'a, str>, XdrError> {
        let bytes = self.read_bytes()?;
        std::str::from_utf8(bytes)
            .map(Cow::Borrowed)
            .map_err(|_| XdrError::InvalidUtf8)
    }

    /// Reads a length-prefixed variable array by running `f` per element.
    pub fn read_vec<T>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> Result<T, XdrError>,
    ) -> Result<Vec<T>, XdrError> {
        let n = self.read_u32()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }
}

/// Appends big-endian, 4-byte-aligned XDR fields to a byte buffer.
#[derive(Debug, Default)]
pub struct XdrWriter {
    buf: Vec<u8>,
}

impl XdrWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    fn write_n(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        self.buf.resize(self.buf.len() + pad(bytes.len()), 0);
    }

    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_u64(&mut self, v: u64) {
        self.write_u32((v >> 32) as u32);
        self.write_u32(v as u32);
    }

    pub fn write_bool(&mut self, v: bool) {
        self.write_u32(u32::from(v));
    }

    pub fn write_opaque(&mut self, bytes: &[u8]) {
        self.write_n(bytes);
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_u32(bytes.len() as u32);
        self.write_n(bytes);
    }

    pub fn write_string(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
    }

    pub fn write_vec<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) {
        self.write_u32(items.len() as u32);
        for item in items {
            f(self, item);
        }
    }
}

/// Number of padding bytes needed to reach a 4-byte boundary after `n` bytes.
fn pad(n: usize) -> usize {
    (4 - (n % 4)) % 4
}

fn read_capture_key(rd: &mut XdrReader<'_>) -> Result<CaptureKey, XdrError> {
    let key_type = LedgerKeyType::from_discriminant(rd.read_u32()?)?;
    let field_1 = rd.read_string()?.into_owned();
    let field_2 = rd.read_string()?.into_owned();
    Ok(CaptureKey {
        key_type,
        field_1,
        field_2,
    })
}

fn read_record(rd: &mut XdrReader<'_>) -> Result<CaptureRecord, XdrError> {
    let tx_hash = rd
        .read_opaque(32)?
        .try_into()
        .map_err(|_| XdrError::Overrun)?;
    let source_account = rd.read_string()?.into_owned();
    let read_only = rd.read_vec(read_capture_key)?;
    let read_write = rd.read_vec(read_capture_key)?;
    Ok(CaptureRecord {
        tx_hash,
        source_account,
        read_only,
        read_write,
    })
}

/// Decodes a version-1 capture document from XDR bytes.
pub fn decode_capture(bytes: &[u8]) -> Result<ArchiveCapture, XdrError> {
    let mut rd = XdrReader::new(bytes);
    let magic = rd.read_u32()?;
    if magic != MAGIC {
        return Err(XdrError::BadMagic(magic));
    }
    let version = rd.read_u32()?;
    if version != VERSION {
        return Err(XdrError::UnsupportedVersion(version));
    }
    let from_ledger = rd.read_u32()?;
    let to_ledger = rd.read_u32()?;
    let checkpoint_count = rd.read_u32()?;
    let records = rd.read_vec(read_record)?;
    Ok(ArchiveCapture {
        from_ledger,
        to_ledger,
        checkpoint_count,
        records,
    })
}

fn write_capture_key(w: &mut XdrWriter, key: &CaptureKey) {
    w.write_u32(key.key_type.discriminant());
    w.write_string(&key.field_1);
    w.write_string(&key.field_2);
}

fn write_record(w: &mut XdrWriter, rec: &CaptureRecord) {
    w.write_opaque(&rec.tx_hash);
    w.write_string(&rec.source_account);
    w.write_vec(&rec.read_only, write_capture_key);
    w.write_vec(&rec.read_write, write_capture_key);
}

/// Encodes a capture document to XDR bytes (used by tooling and fixtures).
pub fn encode_capture(cap: &ArchiveCapture) -> Vec<u8> {
    let mut w = XdrWriter::new();
    w.write_u32(MAGIC);
    w.write_u32(VERSION);
    w.write_u32(cap.from_ledger);
    w.write_u32(cap.to_ledger);
    w.write_u32(cap.checkpoint_count);
    w.write_vec(&cap.records, write_record);
    w.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_capture() -> ArchiveCapture {
        ArchiveCapture {
            from_ledger: 100,
            to_ledger: 103,
            checkpoint_count: 4,
            records: vec![
                CaptureRecord {
                    tx_hash: [0x11; 32],
                    source_account: "GAAAACAPTURE1".into(),
                    read_only: vec![CaptureKey {
                        key_type: LedgerKeyType::ContractData,
                        field_1: "CAAAACAPTURE1".into(),
                        field_2: "state".into(),
                    }],
                    read_write: vec![CaptureKey {
                        key_type: LedgerKeyType::ContractData,
                        field_1: "CAAAACAPTURE1".into(),
                        field_2: "shard:0".into(),
                    }],
                },
                CaptureRecord {
                    tx_hash: [0x22; 32],
                    source_account: "GAAAACAPTURE2".into(),
                    read_only: vec![],
                    read_write: vec![
                        CaptureKey {
                            key_type: LedgerKeyType::Account,
                            field_1: "GAAAACAPTURE2".into(),
                            field_2: String::new(),
                        },
                        CaptureKey {
                            key_type: LedgerKeyType::Other,
                            field_1: "CONFIG_SETTING:100".into(),
                            field_2: String::new(),
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn capture_round_trips_through_xdr() {
        let cap = sample_capture();
        let bytes = encode_capture(&cap);
        let back = decode_capture(&bytes).expect("decode");
        assert_eq!(cap, back);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let cap = sample_capture();
        let mut bytes = encode_capture(&cap);
        bytes[0] = 0x00;
        assert!(matches!(decode_capture(&bytes), Err(XdrError::BadMagic(_))));
    }

    #[test]
    fn decode_rejects_unsupported_version() {
        let cap = sample_capture();
        let mut bytes = encode_capture(&cap);
        bytes[4..8].copy_from_slice(&99u32.to_be_bytes());
        assert!(matches!(
            decode_capture(&bytes),
            Err(XdrError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn decode_overrun_reports_cleanly() {
        let cap = sample_capture();
        let bytes = encode_capture(&cap);
        let truncated = &bytes[..bytes.len() - 5];
        assert!(decode_capture(truncated).is_err());
    }

    #[test]
    fn capture_key_maps_to_footprint_model() {
        let key = CaptureKey {
            key_type: LedgerKeyType::ContractData,
            field_1: "C1".into(),
            field_2: "balance".into(),
        };
        assert_eq!(
            key.to_ledger_key(),
            LedgerKey::ContractData {
                contract_id: "C1".into(),
                key: "balance".into(),
            }
        );
    }

    #[test]
    fn writer_pads_variable_fields_to_four_bytes() {
        let mut w = XdrWriter::new();
        w.write_string("abc");
        assert_eq!(w.into_vec().len(), 8, "len(4) + 3 bytes + 1 pad");
    }
}
