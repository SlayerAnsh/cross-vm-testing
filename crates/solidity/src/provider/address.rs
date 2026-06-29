//! Deterministic account-address derivation shared by the EVM providers.

use alloy_primitives::{keccak256, Address};

/// Derive a deterministic address from a label (keccak of the label, low 20 bytes).
pub(crate) fn address_from_label(label: &str) -> Address {
    let h = keccak256(label.as_bytes());
    Address::from_slice(&h[12..])
}
