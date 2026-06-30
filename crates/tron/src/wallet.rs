//! Tron key derivation: BIP-39 mnemonic + HD path -> `PrivateKeySigner` (secp256k1).
//!
//! Tron is BIP-44 coin type 195 and uses secp256k1, the SAME curve as Ethereum (NOT ed25519).
//! The derivation path shape matches EVM (`m/44'/195'/<index>'/0/0`); only the address encoding
//! differs (keccak of the pubkey, low 20 bytes, 0x41 prefix, base58check).
//! Source: <https://github.com/tronprotocol/tips/issues/102>

use alloy_signer_local::coins_bip39::English;
use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner};
use cross_vm_core::{bip44_account_path, WalletDeriver};

use crate::chain::TronChain;
use crate::error::TronError;
use crate::provider::address::{address_from_pubkey, TronAddress};

impl WalletDeriver for TronChain {
    type Signer = PrivateKeySigner;

    const COIN_TYPE: u32 = 195;

    fn default_hd_path(index: u32) -> String {
        bip44_account_path(Self::COIN_TYPE, index)
    }

    fn derive_signer(&self, mnemonic: &str, hd_path: &str) -> Result<PrivateKeySigner, TronError> {
        MnemonicBuilder::<English>::default()
            .phrase(mnemonic)
            .derivation_path(hd_path)
            .map_err(|e| TronError::Wallet(format!("bad hd path `{hd_path}`: {e}")))?
            .build()
            .map_err(|e| TronError::Wallet(format!("deriving key: {e}")))
    }

    fn derive_from_key(&self, private_key: &str) -> Result<PrivateKeySigner, TronError> {
        // `PrivateKeySigner` parses a 32-byte secp256k1 key in hex (with or without `0x`).
        private_key
            .parse::<PrivateKeySigner>()
            .map_err(|e| TronError::Wallet(format!("bad private key: {e}")))
    }

    fn signer_address(&self, signer: &PrivateKeySigner) -> TronAddress {
        // secp256k1 public key -> uncompressed SEC1 -> Tron address (keccak low 20 bytes, 0x41).
        let encoded = signer.credential().verifying_key().to_encoded_point(false);
        address_from_pubkey(encoded.as_bytes())
    }
}
