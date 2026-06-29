//! EVM key derivation: BIP-39 mnemonic + HD path -> `PrivateKeySigner` (secp256k1).
//!
//! Ethereum is BIP-44 coin type 60. The signer is alloy's [`PrivateKeySigner`], which is
//! `Clone` and holds the private key directly (no `Rc` wrapper needed), and signs EIP-1559
//! transactions on the live-RPC write paths.

use alloy_primitives::Address;
use alloy_signer_local::coins_bip39::English;
use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner};
use cross_vm_core::{bip44_account_path, WalletDeriver};

use crate::chain::EvmChain;
use crate::error::EvmError;

impl WalletDeriver for EvmChain {
    type Signer = PrivateKeySigner;

    const COIN_TYPE: u32 = 60;

    fn default_hd_path(index: u32) -> String {
        bip44_account_path(Self::COIN_TYPE, index)
    }

    fn derive_signer(&self, mnemonic: &str, hd_path: &str) -> Result<PrivateKeySigner, EvmError> {
        MnemonicBuilder::<English>::default()
            .phrase(mnemonic)
            .derivation_path(hd_path)
            .map_err(|e| EvmError::Wallet(format!("bad hd path `{hd_path}`: {e}")))?
            .build()
            .map_err(|e| EvmError::Wallet(format!("deriving key: {e}")))
    }

    fn derive_from_key(&self, private_key: &str) -> Result<PrivateKeySigner, EvmError> {
        // alloy's `PrivateKeySigner` parses a 32-byte secp256k1 key in hex (with or without `0x`).
        private_key
            .parse::<PrivateKeySigner>()
            .map_err(|e| EvmError::Wallet(format!("bad private key: {e}")))
    }

    fn signer_address(&self, signer: &PrivateKeySigner) -> Address {
        signer.address()
    }
}
