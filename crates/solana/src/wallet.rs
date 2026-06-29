//! Solana key derivation: BIP-39 mnemonic + HD path -> ed25519 `Keypair`.
//!
//! Solana is BIP-44 coin type 501 and uses SLIP-10 ed25519 derivation with a shorter path
//! shape (`m/44'/501'/<account>'/0'`). `Keypair` is not `Clone`, so [`SvmSigner`] shares it
//! behind an `Rc`.

use std::rc::Rc;

use cross_vm_core::WalletDeriver;
use solana_address::Address;
use solana_derivation_path::DerivationPath;
use solana_keypair::seed_derivable::keypair_from_seed_and_derivation_path;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::chain::SvmChain;
use crate::error::SvmError;

/// A Solana signer: the ed25519 keypair, shared (it is not `Clone`).
#[derive(Clone)]
pub struct SvmSigner(pub Rc<Keypair>);

impl SvmSigner {
    /// The signer's public key (its on-chain address).
    pub fn pubkey(&self) -> Address {
        self.0.pubkey()
    }

    /// The underlying keypair, used to sign transactions.
    pub fn keypair(&self) -> &Keypair {
        &self.0
    }
}

impl WalletDeriver for SvmChain {
    type Signer = SvmSigner;

    const COIN_TYPE: u32 = 501;

    fn default_hd_path(index: u32) -> String {
        // Solana's standard path is one level shorter than EVM/Cosmos.
        format!("m/44'/{}'/{index}'/0'", Self::COIN_TYPE)
    }

    fn derive_signer(&self, mnemonic: &str, hd_path: &str) -> Result<SvmSigner, SvmError> {
        let mnemonic = bip39::Mnemonic::parse_normalized(mnemonic)
            .map_err(|e| SvmError::Wallet(format!("bad mnemonic: {e}")))?;
        let seed = mnemonic.to_seed("");
        let path = DerivationPath::from_absolute_path_str(hd_path)
            .map_err(|e| SvmError::Wallet(format!("bad hd path `{hd_path}`: {e}")))?;
        let keypair = keypair_from_seed_and_derivation_path(&seed, Some(path))
            .map_err(|e| SvmError::Wallet(format!("deriving keypair: {e}")))?;
        Ok(SvmSigner(Rc::new(keypair)))
    }

    fn derive_from_key(&self, private_key: &str) -> Result<SvmSigner, SvmError> {
        // Solana secret keys are a base58-encoded 64-byte ed25519 keypair (secret || public).
        let keypair = Keypair::try_from_base58_string(private_key)
            .map_err(|e| SvmError::Wallet(format!("bad private key base58: {e}")))?;
        Ok(SvmSigner(Rc::new(keypair)))
    }

    fn signer_address(&self, signer: &SvmSigner) -> Address {
        signer.pubkey()
    }
}
