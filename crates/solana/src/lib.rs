//! Solana (SVM) chain provider for the cross-vm testing suite.
//!
//! Wraps `litesvm` behind the shared [`cross_vm_core::ChainProvider`] trait.
//!
//! ```no_run
//! use std::rc::Rc;
//! use cross_vm_solana::chains::SOLANA_DEVNET;
//! use cross_vm_core::{ChainProvider, WalletFactory};
//!
//! # async fn demo() {
//! let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
//! let mut chain = SOLANA_DEVNET.mock(wallets);
//! let alice = chain.new_account("alice").await;
//! assert!(chain.balance(&alice).await.unwrap() > 0);
//! # }
//! ```

mod asset;
mod chain;
pub mod chains;
mod error;
mod provider;
mod wallet;

#[cfg(test)]
mod tests;

pub use asset::SvmAsset;
pub use chain::SvmChain;
pub use chains::{Commitment, SolanaChainInfo};
pub use error::SvmError;
pub use provider::{SvmDeploy, SvmMockProvider, SvmRpcProvider, DEFAULT_FUNDING_LAMPORTS};
pub use solana_address::Address;
pub use wallet::SvmSigner;

/// The `litesvm` transaction result, re-exported so downstream crates can name the raw
/// result of a Solana execution without depending on `litesvm` directly.
pub use litesvm::types::TransactionMetadata;
