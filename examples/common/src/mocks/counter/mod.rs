//! `Counter` contract bindings: deploy, increment, read a count.

#[cfg(feature = "cw")]
pub mod cw;
#[cfg(feature = "evm")]
pub mod evm;
#[cfg(feature = "solana")]
pub mod svm;
#[cfg(feature = "tron")]
pub mod tron;
