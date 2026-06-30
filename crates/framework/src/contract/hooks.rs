//! Before/after callbacks that fire around a cross-VM contract method.
//!
//! A developer registers hooks on a wrapper's [`ContractBase`](super::ContractBase) to wire
//! side-logic (an indexer, a bridge relay, an event listener) that runs when a transaction
//! executes. A before-hook sees the method about to run; an after-hook sees the uniform
//! [`AppResponse`](super::AppResponse) the VM produced, read-only.
//!
//! Hooks are synchronous `FnMut`. The mock backends are themselves synchronous, and the runtime
//! is current-thread (futures are not `Send`), so async side-effects flow through a channel or an
//! `Rc<RefCell<_>>` buffer captured by the closure and drained later.
//!
//! Both kinds return `Result<(), CrossVmError>`. The first `Err` aborts: a before-`Err` stops the
//! transaction from running; an after-`Err` becomes the method's error.

use cross_vm_core::{ChainKind, CrossVmError};
use cross_vm_cosmwasm::Event;
use cross_vm_solidity::Log;

use super::response::RawResponse;

/// What a before-hook is handed: the logical method about to run, and its VM. No response yet.
pub struct BeforeContext<'a> {
    label: &'a str,
    kind: ChainKind,
}

impl<'a> BeforeContext<'a> {
    pub(crate) fn new(label: &'a str, kind: ChainKind) -> Self {
        Self { label, kind }
    }

    /// The logical method name (e.g. `"increment"`).
    pub fn label(&self) -> &str {
        self.label
    }

    /// Which VM the method runs on.
    pub fn kind(&self) -> ChainKind {
        self.kind
    }
}

/// What an after-hook is handed: the executed method plus the uniform response, read-only.
pub struct HookContext<'a> {
    label: &'a str,
    raw: &'a RawResponse,
}

impl<'a> HookContext<'a> {
    pub(crate) fn new(label: &'a str, raw: &'a RawResponse) -> Self {
        Self { label, raw }
    }

    /// The logical method name (e.g. `"increment"`).
    pub fn label(&self) -> &str {
        self.label
    }

    /// Which VM produced the response.
    pub fn kind(&self) -> ChainKind {
        self.raw.kind()
    }

    /// Borrow the raw, VM-specific result.
    pub fn raw(&self) -> &RawResponse {
        self.raw
    }

    /// The transaction hash, when the backend provides one (Solana only on the mocks).
    pub fn transaction_hash(&self) -> Result<String, CrossVmError> {
        self.raw.transaction_hash()
    }

    /// Gas / compute units consumed, when the backend reports it.
    pub fn gas_used(&self) -> Option<u128> {
        self.raw.gas_used()
    }

    /// The events emitted by a CosmWasm execution, or [`CrossVmError::WrongVm`] for another VM.
    pub fn cosmwasm_events(&self) -> Result<&[Event], CrossVmError> {
        self.raw.cosmwasm_events()
    }

    /// The logs (events) emitted by an EVM call, or [`CrossVmError::WrongVm`] for another VM.
    pub fn evm_logs(&self) -> Result<&[Log], CrossVmError> {
        self.raw.evm_logs()
    }

    /// The program log lines from a Solana execution, or [`CrossVmError::WrongVm`] for another VM.
    pub fn solana_logs(&self) -> Result<&[String], CrossVmError> {
        self.raw.solana_logs()
    }

    /// The logs (events) emitted by a Tron call, or [`CrossVmError::WrongVm`] for another VM.
    /// Tron logs are EVM-shaped (`address`/`topics`/`data`).
    pub fn tron_logs(&self) -> Result<&[Log], CrossVmError> {
        self.raw.tron_logs()
    }
}

type BeforeHook = Box<dyn FnMut(&BeforeContext) -> Result<(), CrossVmError>>;
type AfterHook = Box<dyn FnMut(&HookContext) -> Result<(), CrossVmError>>;

/// The per-contract registry of before/after callbacks, owned by [`ContractBase`](super::ContractBase).
#[derive(Default)]
pub struct Hooks {
    before: Vec<BeforeHook>,
    after: Vec<AfterHook>,
}

impl Hooks {
    /// Append a before-hook.
    pub fn push_before(&mut self, f: BeforeHook) {
        self.before.push(f);
    }

    /// Append an after-hook.
    pub fn push_after(&mut self, f: AfterHook) {
        self.after.push(f);
    }

    /// Fire every before-hook in registration order, stopping at the first `Err`.
    pub fn fire_before(&mut self, ctx: &BeforeContext) -> Result<(), CrossVmError> {
        for h in &mut self.before {
            h(ctx)?;
        }
        Ok(())
    }

    /// Fire every after-hook in registration order, stopping at the first `Err`.
    pub fn fire_after(&mut self, ctx: &HookContext) -> Result<(), CrossVmError> {
        for h in &mut self.after {
            h(ctx)?;
        }
        Ok(())
    }
}
