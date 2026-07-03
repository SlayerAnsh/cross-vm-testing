//! CosmWasm chain provider for the cross-vm testing suite.
//!
//! Wraps `cw-multi-test` behind the shared [`cross_vm_core::ChainProvider`] trait.
//!
//! ```no_run
//! use std::rc::Rc;
//! use cross_vm_cosmwasm::chains::OSMOSIS;
//! use cross_vm_core::{ChainProvider, WalletFactory};
//!
//! # async fn demo() {
//! let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
//! let mut chain = OSMOSIS.mock(wallets);
//! let alice = chain.new_account("alice").await;
//! assert!(chain.balance(&alice).await.unwrap() > 0);
//! # }
//! ```

mod asset;
mod chain;
pub mod chains;
mod contract;
mod error;
mod msg;
mod provider;
mod wallet;

#[cfg(test)]
mod tests;

pub use asset::CwAsset;
pub use chain::CwChain;
pub use chains::CosmosChainInfo;
pub use contract::{CwContract, CwInterface};
pub use cosmwasm_std::{Addr, Event};
pub use error::CwError;
pub use msg::CwSerde;
pub use provider::{CwApp, CwCode, CwMockProvider, CwRpcProvider, DEFAULT_FUNDING};
pub use wallet::CosmosSigner;

/// The `cw-multi-test` execution response, re-exported so downstream crates can name the
/// raw result of a CosmWasm execution without depending on `cw-multi-test` directly.
pub use cw_multi_test::AppResponse as CwAppResponse;
