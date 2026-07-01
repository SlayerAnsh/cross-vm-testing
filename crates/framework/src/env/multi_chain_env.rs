//! The cross-VM environment and its VM-typed accessors.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;

use cross_vm_core::{BlockTime, WalletFactory};
#[cfg(feature = "cw")]
use cross_vm_cosmwasm::CwChain;
#[cfg(feature = "solana")]
use cross_vm_solana::SvmChain;
#[cfg(feature = "evm")]
use cross_vm_solidity::EvmChain;
#[cfg(feature = "tron")]
use cross_vm_tron::TronChain;

use crate::any_chain::AnyChain;
use crate::env::phase::Setup;
use crate::error::EnvError;
use crate::fund::Pending;

/// A cross-VM simulation. Starts in [`Setup`]; [`crate::MultiChainEnv::start`] transitions
/// to [`crate::Running`], after which funding and injection are no longer reachable.
pub struct MultiChainEnv<S = Setup> {
    pub(crate) label: String,
    pub(crate) chains: HashMap<String, AnyChain>,
    pub(crate) pending: Vec<Pending>,
    /// The shared wallet factory passed to [`MultiChainEnv::new`](crate::MultiChainEnv::new).
    /// Chains are injected already holding their own clone of it, so this is the env's own
    /// handle, not something distributed to the chains at `start`.
    pub(crate) wallets: Rc<WalletFactory>,
    pub(crate) _marker: PhantomData<S>,
}

impl<S> MultiChainEnv<S> {
    /// The environment's label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Number of chains injected.
    pub fn len(&self) -> usize {
        self.chains.len()
    }

    /// Whether no chains are injected.
    pub fn is_empty(&self) -> bool {
        self.chains.is_empty()
    }

    /// Borrow the shared wallet factory.
    pub fn wallets(&self) -> &Rc<WalletFactory> {
        &self.wallets
    }

    /// A cloned, VM-agnostic handle to the chain under `label`.
    ///
    /// The mock backends are `Rc<RefCell<_>>`-backed, so the clone shares the live chain's
    /// state: funding, deploying, or querying through it acts on the one underlying chain. This
    /// is how the property-test harness rebuilds a contract wrapper (`Counter::instance(..)`) on
    /// demand from an address without holding the wrapper in its persisted world.
    pub fn chain(&self, label: &str) -> Result<AnyChain, EnvError> {
        self.chains
            .get(label)
            .cloned()
            .ok_or_else(|| EnvError::UnknownChain(label.to_string()))
    }

    /// Advance every injected chain by `n` blocks/slots between endurance operations so block
    /// height progresses across the whole world. The block timestamp advances by `n` seconds on
    /// every VM, so all chains stay on the same clock and cross-VM packet timeouts compare correctly.
    ///
    /// On mock backends this forces `n` blocks; on RPC backends `advance_blocks` is a no-op (a
    /// live chain advances on its own), so the endurance loop simply paces against real block
    /// production instead of forcing it.
    pub async fn advance_all(&mut self, n: u64) {
        for chain in self.chains.values_mut() {
            chain.advance_blocks(n, BlockTime::Increment(n)).await;
        }
    }

    /// Borrow a CosmWasm chain by label.
    #[cfg(feature = "cw")]
    #[allow(unreachable_patterns)]
    pub fn cosmwasm(&mut self, label: &str) -> Result<&mut CwChain, EnvError> {
        match self.chains.get_mut(label) {
            Some(AnyChain::CosmWasm(c)) => Ok(c),
            Some(other) => Err(EnvError::WrongVm {
                label: label.to_string(),
                expected: cross_vm_core::ChainKind::CosmWasm,
                found: other.kind(),
            }),
            None => Err(EnvError::UnknownChain(label.to_string())),
        }
    }

    /// Borrow an EVM chain by label.
    #[cfg(feature = "evm")]
    #[allow(unreachable_patterns)]
    pub fn evm(&mut self, label: &str) -> Result<&mut EvmChain, EnvError> {
        match self.chains.get_mut(label) {
            Some(AnyChain::Evm(c)) => Ok(c),
            Some(other) => Err(EnvError::WrongVm {
                label: label.to_string(),
                expected: cross_vm_core::ChainKind::Evm,
                found: other.kind(),
            }),
            None => Err(EnvError::UnknownChain(label.to_string())),
        }
    }

    /// Borrow a Solana chain by label.
    #[cfg(feature = "solana")]
    #[allow(unreachable_patterns)]
    pub fn solana(&mut self, label: &str) -> Result<&mut SvmChain, EnvError> {
        match self.chains.get_mut(label) {
            Some(AnyChain::Svm(c)) => Ok(c),
            Some(other) => Err(EnvError::WrongVm {
                label: label.to_string(),
                expected: cross_vm_core::ChainKind::Svm,
                found: other.kind(),
            }),
            None => Err(EnvError::UnknownChain(label.to_string())),
        }
    }

    /// Borrow a Tron chain by label.
    #[cfg(feature = "tron")]
    #[allow(unreachable_patterns)]
    pub fn tron(&mut self, label: &str) -> Result<&mut TronChain, EnvError> {
        match self.chains.get_mut(label) {
            Some(AnyChain::Tron(c)) => Ok(c),
            Some(other) => Err(EnvError::WrongVm {
                label: label.to_string(),
                expected: cross_vm_core::ChainKind::Tron,
                found: other.kind(),
            }),
            None => Err(EnvError::UnknownChain(label.to_string())),
        }
    }
}
