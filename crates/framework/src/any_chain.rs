//! Heterogeneous storage for chains of different VMs.

use cross_vm_core::{ChainKind, ChainProvider};
use cross_vm_cosmwasm::{CwChain, CwMockProvider, CwRpcProvider};
use cross_vm_solana::{SvmChain, SvmMockProvider, SvmRpcProvider};
use cross_vm_solidity::{EvmChain, EvmMockProvider, EvmRpcProvider};
use cross_vm_tron::{TronChain, TronMockProvider, TronRpcProvider};

use crate::contract::Account;

/// A chain of any supported VM, stored by the environment.
///
/// `ChainProvider` is not object safe, so this enum (rather than a trait object) is how
/// CosmWasm, EVM, and Solana chains live side by side in one map.
// Per-VM mock states differ in size; the gap is inherent to wrapping three VMs.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum AnyChain {
    /// A CosmWasm chain.
    CosmWasm(CwChain),
    /// An EVM chain.
    Evm(EvmChain),
    /// A Solana chain.
    Svm(SvmChain),
    /// A Tron chain.
    Tron(TronChain),
}

impl AnyChain {
    /// Which VM this chain belongs to.
    pub fn kind(&self) -> ChainKind {
        match self {
            AnyChain::CosmWasm(_) => ChainKind::CosmWasm,
            AnyChain::Evm(_) => ChainKind::Evm,
            AnyChain::Svm(_) => ChainKind::Svm,
            AnyChain::Tron(_) => ChainKind::Tron,
        }
    }

    /// Create a fresh account on this chain and return it as a VM-agnostic [`Account`].
    ///
    /// On the mock backends the account is also funded with a default native balance, so a
    /// cross-VM test can deploy and execute without an explicit funding step.
    pub async fn new_account(&mut self, label: &str) -> Account {
        match self {
            AnyChain::CosmWasm(c) => Account::CosmWasm(c.new_account(label).await),
            AnyChain::Evm(c) => Account::Evm(c.new_account(label).await),
            AnyChain::Svm(c) => Account::Svm(c.new_account(label).await),
            AnyChain::Tron(c) => Account::Tron(c.new_account(label).await),
        }
    }

    /// Current block height / slot of the underlying chain.
    ///
    /// Forwards to the VM provider's [`ChainProvider::block_height`]. Used by the endurance
    /// runner to confirm block progression across a multi-chain world.
    pub async fn block_height(&self) -> u64 {
        match self {
            AnyChain::CosmWasm(c) => c.block_height().await,
            AnyChain::Evm(c) => c.block_height().await,
            AnyChain::Svm(c) => c.block_height().await,
            AnyChain::Tron(c) => c.block_height().await,
        }
    }

    /// Advance the underlying chain by `n` blocks/slots.
    ///
    /// Forwards to the VM provider's [`ChainProvider::advance_blocks`]. The harness `advance`
    /// hook calls this on every chain it holds so time progresses uniformly.
    pub async fn advance_blocks(&mut self, n: u64) {
        match self {
            AnyChain::CosmWasm(c) => c.advance_blocks(n).await,
            AnyChain::Evm(c) => c.advance_blocks(n).await,
            AnyChain::Svm(c) => c.advance_blocks(n).await,
            AnyChain::Tron(c) => c.advance_blocks(n).await,
        }
    }
}

macro_rules! into_any {
    ($($ty:ty => $variant:ident via $wrap:ident),* $(,)?) => {
        $(
            impl From<$ty> for AnyChain {
                fn from(p: $ty) -> Self {
                    AnyChain::$variant($wrap::from(p))
                }
            }
        )*
    };
}

into_any! {
    CwMockProvider  => CosmWasm via CwChain,
    CwRpcProvider   => CosmWasm via CwChain,
    EvmMockProvider => Evm      via EvmChain,
    EvmRpcProvider  => Evm      via EvmChain,
    SvmMockProvider => Svm      via SvmChain,
    SvmRpcProvider  => Svm      via SvmChain,
    TronMockProvider => Tron via TronChain,
    TronRpcProvider  => Tron via TronChain,
}

impl From<CwChain> for AnyChain {
    fn from(c: CwChain) -> Self {
        AnyChain::CosmWasm(c)
    }
}
impl From<EvmChain> for AnyChain {
    fn from(c: EvmChain) -> Self {
        AnyChain::Evm(c)
    }
}
impl From<SvmChain> for AnyChain {
    fn from(c: SvmChain) -> Self {
        AnyChain::Svm(c)
    }
}
impl From<TronChain> for AnyChain {
    fn from(c: TronChain) -> Self {
        AnyChain::Tron(c)
    }
}
