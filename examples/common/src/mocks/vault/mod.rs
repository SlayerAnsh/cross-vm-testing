//! `Vault` contract bindings: a collateralized-debt ledger (deposit / withdraw / borrow / repay).

#[cfg(feature = "cw")]
pub mod cw;
#[cfg(feature = "evm")]
pub mod evm;
#[cfg(feature = "solana")]
pub mod svm;
#[cfg(feature = "tron")]
pub mod tron;
