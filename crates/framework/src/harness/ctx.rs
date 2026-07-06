//! The live execution context threaded through a [`Harness`](super::Harness) run.
//!
//! `Ctx` is the system-under-test's *infrastructure*: a started [`MultiChainEnv`]. It is kept
//! separate from the harness `World`, which holds only persisted state (the shadow model, flags,
//! and addresses learned so far). The split is what lets a contract that creates another contract
//! work: `apply` discovers the child's address, records it in the `World`, and a later step
//! rebuilds a handle for it from this context with [`Ctx::chain`].

#[cfg(feature = "cw")]
use cross_vm_cosmwasm::CwChain;
#[cfg(feature = "solana")]
use cross_vm_solana::SvmChain;
#[cfg(feature = "evm")]
use cross_vm_solidity::EvmChain;
#[cfg(feature = "tron")]
use cross_vm_tron::TronChain;

use crate::any_chain::AnyChain;
use crate::env::{MultiChainEnv, Running};
use crate::error::EnvError;

/// The started environment a harness operates against during a run.
pub struct Ctx {
    /// The live multi-chain environment (the system-under-test's chains and wallets).
    pub env: MultiChainEnv<Running>,
}

impl Ctx {
    /// Wrap a started environment.
    pub fn new(env: MultiChainEnv<Running>) -> Self {
        Self { env }
    }

    /// A cloned chain handle (shares the live chain's state) for building a contract wrapper
    /// bound to a known address, e.g. `Counter::instance(ctx.chain("eth")?, addr)`.
    pub fn chain(&self, label: &str) -> Result<AnyChain, EnvError> {
        self.env.chain(label)
    }

    /// Borrow the CosmWasm chain under `label`.
    #[cfg(feature = "cw")]
    pub fn cosmwasm(&mut self, label: &str) -> Result<&mut CwChain, EnvError> {
        self.env.cosmwasm(label)
    }

    /// Borrow the EVM chain under `label`.
    #[cfg(feature = "evm")]
    pub fn evm(&mut self, label: &str) -> Result<&mut EvmChain, EnvError> {
        self.env.evm(label)
    }

    /// Borrow the Solana chain under `label`.
    #[cfg(feature = "solana")]
    pub fn solana(&mut self, label: &str) -> Result<&mut SvmChain, EnvError> {
        self.env.solana(label)
    }

    /// Borrow the Tron chain under `label`.
    #[cfg(feature = "tron")]
    pub fn tron(&mut self, label: &str) -> Result<&mut TronChain, EnvError> {
        self.env.tron(label)
    }

    /// Advance every chain by `n` blocks/slots.
    pub async fn advance_all(&mut self, n: u64) {
        self.env.advance_all(n).await;
    }
}
