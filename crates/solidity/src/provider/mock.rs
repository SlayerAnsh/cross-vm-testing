//! In-process EVM provider backed by `revm`.
//!
//! [`EvmMockProvider`] is a thin boundary over the shared [`RevmCore`] (the same core the Tron
//! mock builds on): it keeps the EVM-specific pieces (chain info, wallet roster, signer cache,
//! error mapping) and delegates VM construction and execution to the core.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, Bytes, Log, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use cross_vm_revm_common::{ExecFailure, RevmCore};

use crate::chains::EvmChainInfo;
use crate::error::EvmError;
use crate::provider::address::address_from_label;

/// Default funding handed to accounts created via [`ChainProvider::new_account`]:
/// 100 ETH in wei.
pub const DEFAULT_FUNDING_WEI: u128 = 100_000_000_000_000_000_000;

/// The concrete in-memory `revm` instance used by the mock provider.
pub type EvmInner = cross_vm_revm_common::RevmInner;

/// In-process EVM provider backed by `revm`.
///
/// The EVM lives behind `Rc<RefCell<_>>` (inside [`RevmCore`]) so the handle is cheap to `clone`
/// and every clone shares one chain state. This lets a contract own its own handle
/// (`Contract::new(chain)`) while the test still drives the same chain, and lets the contract
/// operations run behind `&self`.
#[derive(Clone)]
pub struct EvmMockProvider {
    core: RevmCore,
    info: EvmChainInfo,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, PrivateKeySigner>>>,
}

impl EvmMockProvider {
    /// Build a fresh mock chain from a predefined [`EvmChainInfo`].
    pub fn new(info: EvmChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let core = RevmCore::new(info.numeric_id(), info.spec_id, |evm| {
            // Start at block 1 (a 0 marker is indistinguishable from "unset" in contracts that
            // record `pending[seq] = block.number`) and at the shared mock clock so cross-VM
            // packet timeouts compare correctly against the cosmos chain.
            evm.ctx.block.number = U256::from(1u64);
            evm.ctx.block.timestamp = U256::from(cross_vm_core::MOCK_BLOCK_TIMESTAMP);
        });
        Self {
            core,
            info,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Deploy bytecode via a create transaction, appending constructor args to the initcode.
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        from: &Address,
    ) -> Result<Address, EvmError> {
        self.core
            .deploy_create(bytecode, constructor_args.as_ref(), *from)
            .map_err(|f| EvmError::Deploy(f.deploy_message()))
    }

    /// Execute a state-mutating call against `to`, returning its output plus emitted logs.
    pub async fn call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
    ) -> Result<EvmExecution, EvmError> {
        self.call_value(to, calldata, from, U256::ZERO).await
    }

    /// Execute a state-mutating call against `to` carrying `value` wei (a payable call), returning
    /// its output plus emitted logs. The caller's balance is topped up to cover `value` first (the
    /// mock mints native funds on demand, like [`ChainProvider::new_account`]).
    pub async fn call_value(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
        value: U256,
    ) -> Result<EvmExecution, EvmError> {
        self.core
            .call(*to, calldata.as_ref(), *from, value)
            .map(EvmExecution::from)
            .map_err(|f| EvmError::Execute(f.call_message("call")))
    }

    /// Run a read-only static call against `to`.
    pub async fn static_call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, EvmError> {
        self.core
            .static_call(*to, calldata.as_ref())
            .map_err(|f| match f {
                // A transact error is a query-infra failure; a revert/halt is an execution error,
                // exactly as before the RevmCore extraction.
                ExecFailure::Internal(s) => EvmError::Query(s),
                other => EvmError::Execute(other.call_message("static_call")),
            })
    }
}

/// The result of a state-mutating EVM [`call`](EvmMockProvider::call): the return data, the
/// logs (events) emitted during execution, and (on the live RPC backend) the broadcast
/// transaction hash.
#[derive(Clone, Debug, Default)]
pub struct EvmExecution {
    /// ABI-encoded return data.
    pub output: Bytes,
    /// Logs (events) emitted during execution, in order.
    pub logs: Vec<Log>,
    /// The broadcast transaction hash. `Some` on the live RPC backend; `None` on the mock,
    /// which executes in-process without a transaction hash.
    pub tx_hash: Option<B256>,
}

impl From<cross_vm_revm_common::Execution> for EvmExecution {
    fn from(e: cross_vm_revm_common::Execution) -> Self {
        Self {
            output: e.output,
            logs: e.logs,
            tx_hash: None,
        }
    }
}

impl ChainProvider for EvmMockProvider {
    type Spec = EvmChainInfo;
    type Address = Address;
    type Account = Address;
    type Balance = U256;
    type Error = EvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, label: &str) -> Address {
        let addr = address_from_label(label);
        let denom = self.info.native_symbol;
        let _ = self
            .set_balance(&addr, denom, U256::from(DEFAULT_FUNDING_WEI))
            .await;
        addr
    }

    async fn balance(&self, addr: &Address) -> Result<U256, EvmError> {
        self.core
            .balance(*addr)
            .map_err(|f| EvmError::Balance(f.call_message("balance")))
    }

    async fn set_balance(
        &mut self,
        addr: &Address,
        denom: &str,
        amount: U256,
    ) -> Result<(), EvmError> {
        if !denom.eq_ignore_ascii_case(self.info.native_symbol) {
            return Err(EvmError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{}'",
                self.info.native_symbol
            )));
        }
        self.core.set_balance(*addr, amount);
        Ok(())
    }

    async fn block_height(&self) -> u64 {
        self.core.block_height()
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        self.core.advance_blocks(n, time);
    }
}
