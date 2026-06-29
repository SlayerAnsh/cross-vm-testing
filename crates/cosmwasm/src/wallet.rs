//! Cosmos key derivation: BIP-39 mnemonic + HD path -> secp256k1 signing key + bech32 address.
//!
//! Cosmos is BIP-44 coin type 118. The bech32 address depends on the chain's prefix
//! (`osmo`, `juno`, ...), so derivation reads it from [`ChainProvider::chain_info`]. cosmrs'
//! `SigningKey` is not `Clone`, so [`CosmosSigner`] wraps it in an `Rc` and bundles the
//! pre-computed [`Addr`].

use std::rc::Rc;

use cosmrs::bip32;
use cosmrs::crypto::secp256k1::SigningKey;
use cosmwasm_std::Addr;
use cross_vm_core::{bip44_account_path, ChainProvider, WalletDeriver};

use crate::chain::CwChain;
use crate::error::CwError;

/// A Cosmos signer: the secp256k1 key (shared, since it is not `Clone`) plus its bech32 address.
#[derive(Clone)]
pub struct CosmosSigner {
    /// The signing key, used to sign transactions on the live-RPC write paths.
    pub key: Rc<SigningKey>,
    /// The bech32 address this key controls, with the chain's prefix.
    pub address: Addr,
}

impl WalletDeriver for CwChain {
    type Signer = CosmosSigner;

    const COIN_TYPE: u32 = 118;

    fn default_hd_path(index: u32) -> String {
        bip44_account_path(Self::COIN_TYPE, index)
    }

    fn derive_signer(&self, mnemonic: &str, hd_path: &str) -> Result<CosmosSigner, CwError> {
        let prefix = self.chain_info().bech32_prefix;
        // cosmrs' bundled bip32 has its `Mnemonic` feature-gated off, so use the `bip39` crate
        // for the phrase -> seed step, and bip32 only for the HD derivation.
        let mnemonic = bip39::Mnemonic::parse_normalized(mnemonic)
            .map_err(|e| CwError::Wallet(format!("bad mnemonic: {e}")))?;
        let seed = mnemonic.to_seed("");
        let path = hd_path
            .parse::<bip32::DerivationPath>()
            .map_err(|e| CwError::Wallet(format!("bad hd path `{hd_path}`: {e}")))?;
        let xprv = bip32::XPrv::derive_from_path(seed, &path)
            .map_err(|e| CwError::Wallet(format!("deriving key: {e}")))?;
        let key = SigningKey::from_slice(xprv.private_key().to_bytes().as_slice())
            .map_err(|e| CwError::Wallet(format!("building signing key: {e}")))?;
        signer_from_key(key, prefix)
    }

    fn derive_from_key(&self, private_key: &str) -> Result<CosmosSigner, CwError> {
        let prefix = self.chain_info().bech32_prefix;
        let bytes = hex::decode(private_key.trim_start_matches("0x"))
            .map_err(|e| CwError::Wallet(format!("bad private key hex: {e}")))?;
        let key = SigningKey::from_slice(&bytes)
            .map_err(|e| CwError::Wallet(format!("building signing key: {e}")))?;
        signer_from_key(key, prefix)
    }

    fn signer_address(&self, signer: &CosmosSigner) -> Addr {
        signer.address.clone()
    }
}

/// Bundle a secp256k1 [`SigningKey`] with its bech32 address under the chain's prefix.
fn signer_from_key(key: SigningKey, prefix: &str) -> Result<CosmosSigner, CwError> {
    let account_id = key
        .public_key()
        .account_id(prefix)
        .map_err(|e| CwError::Wallet(format!("deriving account id: {e}")))?;
    Ok(CosmosSigner {
        key: Rc::new(key),
        address: Addr::unchecked(account_id.to_string()),
    })
}
