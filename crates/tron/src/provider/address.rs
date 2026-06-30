//! Tron address: 0x41-prefixed 21-byte form, base58check display.
//!
//! Encoding per <https://developers.tron.network/docs/account>:
//!   addr21       = 0x41 || keccak256(pubkey)[12..32]
//!   base58check  = base58( addr21 || sha256(sha256(addr21))[..4] )
//!
//! The inner 20 bytes equal the EVM address, so `revm` executes on [`TronAddress::as_evm`]
//! while every surface shows the Tron form. Tron accounts use secp256k1 (same curve as
//! Ethereum), NOT ed25519.

use std::fmt;
use std::str::FromStr;

use alloy_primitives::{keccak256, Address};
use sha2::{Digest, Sha256};

use crate::error::TronError;

/// Tron mainnet address prefix byte.
const TRON_MAINNET_PREFIX: u8 = 0x41;

/// A Tron account address: a 21-byte `0x41`-prefixed value, shown as base58check.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TronAddress([u8; 21]);

impl TronAddress {
    /// Wrap a 20-byte EVM address as a Tron address (prepend the `0x41` prefix).
    pub fn from_evm(a: Address) -> Self {
        let mut b = [0u8; 21];
        b[0] = TRON_MAINNET_PREFIX;
        b[1..].copy_from_slice(a.as_slice());
        Self(b)
    }

    /// The inner 20-byte EVM address used at the `revm` execution boundary.
    pub fn as_evm(&self) -> Address {
        Address::from_slice(&self.0[1..])
    }

    /// Lower-case hex of the 21-byte form (starts with `41`).
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// base58check string (starts with `T`).
    pub fn to_base58(&self) -> String {
        let checksum = double_sha256(&self.0);
        let mut buf = self.0.to_vec();
        buf.extend_from_slice(&checksum[..4]);
        bs58::encode(buf).into_string()
    }

    /// Parse a base58check Tron address, validating the 4-byte double-sha256 checksum.
    pub fn from_base58(s: &str) -> Result<Self, TronError> {
        let raw = bs58::decode(s)
            .into_vec()
            .map_err(|e| TronError::Wallet(format!("base58: {e}")))?;
        if raw.len() != 25 {
            return Err(TronError::Wallet(format!("address length {}", raw.len())));
        }
        let (body, check) = raw.split_at(21);
        if double_sha256(body)[..4] != check[..4] {
            return Err(TronError::Wallet("address checksum mismatch".into()));
        }
        let mut b = [0u8; 21];
        b.copy_from_slice(body);
        Ok(Self(b))
    }
}

/// Double SHA-256, used for the base58check checksum.
fn double_sha256(bytes: &[u8]) -> [u8; 32] {
    let h1 = Sha256::digest(bytes);
    let h2 = Sha256::digest(h1);
    h2.into()
}

/// secp256k1 uncompressed pubkey (65 bytes incl. `0x04` tag, or 64-byte body) -> Tron address.
/// keccak256 over the 64-byte body, low 20 bytes, `0x41` prefix.
/// Source: <https://developers.tron.network/docs/account>
pub fn address_from_pubkey(uncompressed_pubkey: &[u8]) -> TronAddress {
    let body = if uncompressed_pubkey.len() == 65 {
        &uncompressed_pubkey[1..]
    } else {
        uncompressed_pubkey
    };
    let h = keccak256(body);
    TronAddress::from_evm(Address::from_slice(&h[12..]))
}

/// Deterministic test address from a label (keccak of the label, low 20 bytes, `0x41` prefix).
pub(crate) fn address_from_label(label: &str) -> TronAddress {
    let h = keccak256(label.as_bytes());
    TronAddress::from_evm(Address::from_slice(&h[12..]))
}

impl fmt::Display for TronAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base58())
    }
}

impl FromStr for TronAddress {
    type Err = TronError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_base58(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base58_roundtrip_zero() {
        let a = TronAddress::from_evm(Address::ZERO);
        let s = a.to_base58();
        assert!(s.starts_with('T'));
        assert_eq!(TronAddress::from_base58(&s).unwrap(), a);
    }

    #[test]
    fn evm_roundtrip_preserves_low_20() {
        let evm = alloy_primitives::address!("dac17f958d2ee523a2206206994597c13d831ec7");
        let t = TronAddress::from_evm(evm);
        assert_eq!(t.as_evm(), evm);
        assert_eq!(&t.to_hex()[..2], "41");
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut s = TronAddress::from_evm(Address::ZERO).to_base58();
        s.pop();
        s.push('x');
        assert!(TronAddress::from_base58(&s).is_err());
    }

    #[test]
    fn label_address_is_tron_shaped() {
        let a = address_from_label("alice");
        assert!(a.to_base58().starts_with('T'));
        assert_eq!(a.to_hex().len(), 42); // 21 bytes
    }
}
