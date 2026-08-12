//! Ledger key model for the ledger entry types a Soroban transaction can
//! touch: accounts, trustlines, contract data, contract code and contract TTL
//! entries.

/// An address-like identifier (account or contract) used in ledger keys.
pub type Address = String;

/// A Stellar asset identifier, encoded as `code:issuer`.
pub type AssetId = String;

/// A Soroban contract storage key, encoded as a string.
pub type ScKey = String;

/// The ledger entry types a transaction may touch.
///
/// Identifiers are intentionally plain strings: Slipstream is an analytical
/// engine and must not couple to a specific XDR codec. Backends that decode
/// XDR (RPC, Horizon, ledger archives) are responsible for producing these
/// identifiers.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum LedgerKey {
    /// An account balance / sequence-number entry.
    Account { account_id: Address },
    /// A trustline between an account and an asset.
    TrustLine { account_id: Address, asset: AssetId },
    /// A Soroban contract data entry (a contract storage key).
    ContractData { contract_id: Address, key: ScKey },
    /// A Soroban contract's code (Wasm) blob.
    ContractCode { contract_id: Address },
    /// A Soroban contract's TTL entry.
    ContractTtl { contract_id: Address },
    /// Any entry type we do not model explicitly.
    Other(String),
}

impl std::fmt::Display for LedgerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerKey::Account { account_id } => write!(f, "account:{account_id}"),
            LedgerKey::TrustLine { account_id, asset } => {
                write!(f, "trustline:{account_id}:{asset}")
            }
            LedgerKey::ContractData { contract_id, key } => {
                write!(f, "contract:{contract_id}:{key}")
            }
            LedgerKey::ContractCode { contract_id } => write!(f, "code:{contract_id}"),
            LedgerKey::ContractTtl { contract_id } => write!(f, "ttl:{contract_id}"),
            LedgerKey::Other(raw) => write!(f, "other:{raw}"),
        }
    }
}

/// Convenience constructor for a contract data key.
pub fn contract_data(contract_id: impl Into<String>, key: impl Into<String>) -> LedgerKey {
    LedgerKey::ContractData {
        contract_id: contract_id.into(),
        key: key.into(),
    }
}

/// Convenience constructor for an account key.
pub fn account(account_id: impl Into<String>) -> LedgerKey {
    LedgerKey::Account {
        account_id: account_id.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_formats_are_stable() {
        assert_eq!(
            contract_data("C1", "balance").to_string(),
            "contract:C1:balance"
        );
        assert_eq!(account("GABC").to_string(), "account:GABC");
        assert_eq!(
            LedgerKey::TrustLine {
                account_id: "GABC".into(),
                asset: "USDC:GBISSUER".into(),
            }
            .to_string(),
            "trustline:GABC:USDC:GBISSUER"
        );
    }

    #[test]
    fn ordering_is_deterministic() {
        let mut keys = vec![
            contract_data("C2", "a"),
            account("G1"),
            contract_data("C1", "z"),
        ];
        keys.sort();
        assert_eq!(
            keys,
            vec![
                account("G1"),
                contract_data("C1", "z"),
                contract_data("C2", "a"),
            ]
        );
    }
}
