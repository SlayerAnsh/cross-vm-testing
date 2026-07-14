//! EVM/Solidity chain provider for the cross-vm testing suite.
//!
//! Wraps `revm` behind the shared [`cross_vm_core::ChainProvider`] trait.
//!
//! ```no_run
//! use std::rc::Rc;
//! use cross_vm_solidity::chains::ETHEREUM;
//! use cross_vm_core::{ChainProvider, WalletFactory};
//!
//! # async fn demo() {
//! let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
//! let mut chain = ETHEREUM.mock(wallets);
//! let alice = chain.new_account("alice").await;
//! assert!(chain.balance(&alice).await.unwrap() > alloy_primitives::U256::ZERO);
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

pub use alloy_primitives::{Address, Bytes, Log, B256, U256};
pub use asset::EvmAsset;
pub use chain::EvmChain;
pub use chains::EvmChainInfo;
pub use error::EvmError;
pub use provider::{
    EvmDeploy, EvmExecution, EvmGas, EvmInner, EvmMockProvider, EvmRpcProvider, DEFAULT_FUNDING_WEI,
};
