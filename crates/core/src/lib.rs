//! Core abstractions shared by every chain provider in the cross-vm testing suite.
//!
//! The suite spans three execution environments (CosmWasm, EVM, Solana). Each one
//! ships a *chain provider*: the analogue of alloy's `Provider`, cw-orch's `CwEnv`,
//! or test-tube's `Runner`. Every provider wraps an in-process VM ("mock") today and
//! a live RPC connection later, behind the single [`ChainProvider`] trait defined here.
//!
//! Because the three VMs disagree on almost every concrete type (`Addr` vs `Address`
//! vs `Pubkey`, bech32 messages vs ABI calldata vs Borsh instructions), the trait is
//! built from associated types. Each VM keeps its idiomatic types while sharing one
//! method vocabulary, so cross-vm scripts read the same regardless of target.

mod chain_kind;
mod chain_provider;
mod chain_spec;
mod error;
mod fund_error;
mod time;
mod wallet;
pub mod wallet_lock;

pub use chain_kind::ChainKind;
pub use chain_provider::ChainProvider;
pub use chain_spec::ChainSpec;
pub use error::CrossVmError;
pub use fund_error::FundError;
pub use time::{BlockTime, MOCK_BLOCK_TIMESTAMP};
pub use wallet::{
    bip44_account_path, WalletDef, WalletDeriver, WalletFactory, WalletLabel, WalletSource,
    WalletSpec,
};
