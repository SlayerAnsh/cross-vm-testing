//! TVM contract-address derivation.
//!
//! Differs from Ethereum: CREATE hashes the transaction id and a per-root-call nonce; CREATE2
//! prefixes `0x41` instead of `0xff`.
//! Sources: <https://github.com/tronprotocol/tips/issues/26> and
//! <https://developers.tron.network/docs/tvm>

use alloy_primitives::{keccak256, Address};

use crate::provider::address::TronAddress;

/// CREATE: `0x41 || keccak256(tx_id || nonce_be)[12..32]`.
pub fn tron_create_address(tx_id: [u8; 32], nonce: u64) -> TronAddress {
    let mut buf = tx_id.to_vec();
    buf.extend_from_slice(&nonce.to_be_bytes());
    TronAddress::from_evm(Address::from_slice(&keccak256(&buf)[12..]))
}

/// CREATE2: `0x41 || keccak256(0x41 || caller20 || salt || keccak256(init_code))[12..32]`.
pub fn tron_create2_address(caller: TronAddress, salt: [u8; 32], init_code: &[u8]) -> TronAddress {
    let mut buf = Vec::with_capacity(1 + 20 + 32 + 32);
    buf.push(0x41);
    buf.extend_from_slice(caller.as_evm().as_slice());
    buf.extend_from_slice(&salt);
    buf.extend_from_slice(keccak256(init_code).as_slice());
    TronAddress::from_evm(Address::from_slice(&keccak256(&buf)[12..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;

    #[test]
    fn create_matches_formula() {
        let tx = [7u8; 32];
        let got = tron_create_address(tx, 1);
        let mut buf = tx.to_vec();
        buf.extend_from_slice(&1u64.to_be_bytes());
        let want = TronAddress::from_evm(Address::from_slice(&keccak256(&buf)[12..]));
        assert_eq!(got, want);
    }

    #[test]
    fn create2_uses_0x41_prefix() {
        let caller = TronAddress::from_evm(Address::ZERO);
        let got = tron_create2_address(caller, [0u8; 32], b"\x60\x00");
        let mut buf = vec![0x41u8];
        buf.extend_from_slice(caller.as_evm().as_slice());
        buf.extend_from_slice(&[0u8; 32]);
        buf.extend_from_slice(keccak256(b"\x60\x00").as_slice());
        let want = TronAddress::from_evm(Address::from_slice(&keccak256(&buf)[12..]));
        assert_eq!(got, want);
    }
}
