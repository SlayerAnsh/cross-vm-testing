//! Shared state and helpers every cross-VM contract wrapper builds on.
//!
//! A contract wrapper owns its chain handle (a cheap [`AnyChain`] clone) and, once deployed,
//! the contract's address. [`ContractBase`] provides the typed chain accessors
//! (`cosmwasm`/`evm`/`solana`) and typed address getters (`cw_addr`/`evm_addr`/`svm_addr`)
//! that a per-VM hook uses to reach the native API, each returning [`CrossVmError::WrongVm`]
//! when used against a different VM.

use std::cell::RefCell;

use cross_vm_core::{ChainKind, CrossVmError};
use cross_vm_cosmwasm::{Addr, CwChain};
use cross_vm_solana::{Address as SvmAddress, SvmChain};
use cross_vm_solidity::{Address as EvmAddress, EvmChain};
use cross_vm_tron::{TronAddress, TronChain};

use super::account::Account;
use super::hooks::{BeforeContext, HookContext, Hooks};
use super::response::AppResponse;
use crate::any_chain::AnyChain;

/// The chain handle plus the deployed contract address a wrapper shares across its hooks.
///
/// The address is stored behind a `RefCell` so a wrapper can deploy in a `&self` method
/// (`setup`) and still record the resulting address, keeping the whole contract API `&self`.
///
/// It also owns the per-contract [`Hooks`] registry. A wrapper registers before/after callbacks
/// with [`on_before`](Self::on_before) / [`on_after`](Self::on_after); its method dispatchers fire
/// them with [`run_before`](Self::run_before) / [`run_after`](Self::run_after) around the per-VM
/// execution.
pub struct ContractBase {
    chain: AnyChain,
    address: RefCell<Option<Account>>,
    hooks: RefCell<Hooks>,
}

impl ContractBase {
    /// A wrapper not yet bound to a deployed contract. Set the address with
    /// [`ContractBase::set_address`] after deploying.
    pub fn new(chain: AnyChain) -> Self {
        Self {
            chain,
            address: RefCell::new(None),
            hooks: RefCell::new(Hooks::default()),
        }
    }

    /// A wrapper attached to an already-deployed contract at `address`.
    pub fn with_address(chain: AnyChain, address: Account) -> Self {
        Self {
            chain,
            address: RefCell::new(Some(address)),
            hooks: RefCell::new(Hooks::default()),
        }
    }

    /// Which VM this contract lives on.
    pub fn kind(&self) -> ChainKind {
        self.chain.kind()
    }

    /// Borrow the underlying chain handle.
    pub fn chain(&self) -> &AnyChain {
        &self.chain
    }

    /// Record the deployed contract address (called from a wrapper's `setup`/`new`).
    pub fn set_address(&self, address: Account) {
        *self.address.borrow_mut() = Some(address);
    }

    /// The deployed contract address, if set.
    pub fn address(&self) -> Option<Account> {
        self.address.borrow().clone()
    }

    /// Borrow the CosmWasm chain, or [`CrossVmError::WrongVm`] for another VM.
    pub fn cosmwasm(&self) -> Result<&CwChain, CrossVmError> {
        match &self.chain {
            AnyChain::CosmWasm(c) => Ok(c),
            other => Err(CrossVmError::wrong_vm(ChainKind::CosmWasm, other.kind())),
        }
    }

    /// Borrow the EVM chain, or [`CrossVmError::WrongVm`] for another VM.
    pub fn evm(&self) -> Result<&EvmChain, CrossVmError> {
        match &self.chain {
            AnyChain::Evm(c) => Ok(c),
            other => Err(CrossVmError::wrong_vm(ChainKind::Evm, other.kind())),
        }
    }

    /// Borrow the Solana chain, or [`CrossVmError::WrongVm`] for another VM.
    pub fn solana(&self) -> Result<&SvmChain, CrossVmError> {
        match &self.chain {
            AnyChain::Svm(c) => Ok(c),
            other => Err(CrossVmError::wrong_vm(ChainKind::Svm, other.kind())),
        }
    }

    /// Borrow the Tron chain, or [`CrossVmError::WrongVm`] for another VM.
    pub fn tron(&self) -> Result<&TronChain, CrossVmError> {
        match &self.chain {
            AnyChain::Tron(c) => Ok(c),
            other => Err(CrossVmError::wrong_vm(ChainKind::Tron, other.kind())),
        }
    }

    /// The deployed CosmWasm contract address, or an error if undeployed / another VM.
    pub fn cw_addr(&self) -> Result<Addr, CrossVmError> {
        match self.require_address()? {
            Account::CosmWasm(a) => Ok(a),
            other => Err(CrossVmError::wrong_vm(ChainKind::CosmWasm, other.kind())),
        }
    }

    /// The deployed EVM contract address, or an error if undeployed / another VM.
    pub fn evm_addr(&self) -> Result<EvmAddress, CrossVmError> {
        match self.require_address()? {
            Account::Evm(a) => Ok(a),
            other => Err(CrossVmError::wrong_vm(ChainKind::Evm, other.kind())),
        }
    }

    /// The deployed Solana program/account address, or an error if undeployed / another VM.
    pub fn svm_addr(&self) -> Result<SvmAddress, CrossVmError> {
        match self.require_address()? {
            Account::Svm(a) => Ok(a),
            other => Err(CrossVmError::wrong_vm(ChainKind::Svm, other.kind())),
        }
    }

    /// The deployed Tron contract address, or an error if undeployed / another VM.
    pub fn tron_addr(&self) -> Result<TronAddress, CrossVmError> {
        match self.require_address()? {
            Account::Tron(a) => Ok(a),
            other => Err(CrossVmError::wrong_vm(ChainKind::Tron, other.kind())),
        }
    }

    fn require_address(&self) -> Result<Account, CrossVmError> {
        self.address
            .borrow()
            .clone()
            .ok_or_else(|| CrossVmError::Other {
                kind: self.kind(),
                reason: "contract has no address yet; deploy it (setup/new) first".into(),
            })
    }

    /// Register a callback that runs before each method that calls [`run_before`](Self::run_before).
    ///
    /// An `Err` from the callback aborts the method before its transaction runs.
    pub fn on_before(&self, f: impl FnMut(&BeforeContext) -> Result<(), CrossVmError> + 'static) {
        self.hooks.borrow_mut().push_before(Box::new(f));
    }

    /// Register a callback that runs after each method that calls [`run_after`](Self::run_after),
    /// observing the uniform [`AppResponse`].
    ///
    /// An `Err` from the callback becomes the method's error.
    pub fn on_after(&self, f: impl FnMut(&HookContext) -> Result<(), CrossVmError> + 'static) {
        self.hooks.borrow_mut().push_after(Box::new(f));
    }

    /// Fire the before-hooks for a logical method named `label`. Call this in a wrapper's method
    /// dispatcher before the per-VM execution; propagate its `Err` with `?`.
    ///
    /// A hook must not re-enter the same contract's `run_before`/`run_after`: the hook registry is
    /// borrowed for the duration, so re-entry panics on the `RefCell`.
    pub fn run_before(&self, label: &str) -> Result<(), CrossVmError> {
        let ctx = BeforeContext::new(label, self.kind());
        self.hooks.borrow_mut().fire_before(&ctx)
    }

    /// Fire the after-hooks for a logical method named `label`, passing them `resp`. Returns `resp`
    /// unchanged on success so a dispatcher can `return self.base.run_after(label, resp)`.
    ///
    /// Same re-entrancy caveat as [`run_before`](Self::run_before).
    pub fn run_after<T>(
        &self,
        label: &str,
        resp: AppResponse<T>,
    ) -> Result<AppResponse<T>, CrossVmError> {
        let ctx = HookContext::new(label, resp.raw());
        self.hooks.borrow_mut().fire_after(&ctx)?;
        Ok(resp)
    }
}
