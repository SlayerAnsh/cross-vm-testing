//! Cross-VM environment.
//!
//! [`MultiChainEnv`] holds chains from any supported VM (CosmWasm, EVM, Solana) and models a
//! chain simulation with two phases. During **setup** you inject chains and declare
//! funding; [`MultiChainEnv::start`] applies that plan and returns a **running** environment
//! where only chain execution is allowed.
//!
//! ```no_run
//! use std::rc::Rc;
//! use cross_vm_framework::prelude::*;
//!
//! # async fn demo() {
//! let wallets = Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).unwrap());
//! let mut env = MultiChainEnv::new("swap-test", wallets.clone());
//! env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets.clone())));
//! env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets)));
//!
//! let alice = env.cosmwasm("osmosis").unwrap().new_account("alice").await;
//! env.fund("osmosis", &alice, "uosmo", 1_000_000u128).unwrap();
//!
//! let mut env = env.start().await.unwrap();    // -> running phase
//! // env.fund(...);                            // compile error: not available when running
//! let bal = env.cosmwasm("osmosis").unwrap().balance(&alice).await.unwrap();
//! assert!(bal >= 1_000_000);
//! # }
//! ```

mod any_chain;
mod contract;
mod env;
mod error;
mod fund;
mod shortfall;
mod wallets;

pub mod harness;
pub mod prelude;

#[cfg(test)]
mod tests;

pub use any_chain::AnyChain;
pub use contract::{
    Account, AppResponse, BeforeContext, ContractBase, HookContext, Hooks, RawResponse,
};
pub use env::{MultiChainEnv, Running, Setup};
pub use error::EnvError;
pub use fund::FundTarget;
pub use shortfall::Shortfall;
pub use wallets::{EmptyWallets, TestWallets, EMPTY_WALLETS, TEST_WALLETS};

// Re-export the building blocks so users need only depend on this crate.
pub use cross_vm_core;
pub use cross_vm_cosmwasm;
pub use cross_vm_solana;
pub use cross_vm_solidity;
pub use cross_vm_tron;
