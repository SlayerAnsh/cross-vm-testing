//! In-process EVM provider backed by `revm`.
//!
//! [`EvmMockProvider`] is a thin boundary over the shared [`RevmCore`] (the same core the Tron
//! mock builds on): it keeps the EVM-specific pieces (chain info, wallet roster, signer cache,
//! error mapping) and delegates VM construction and execution to the core.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use cross_vm_revm_common::{ExecFailure, RevmCore};

use crate::chains::EvmChainInfo;
use crate::error::EvmError;
use crate::provider::address::address_from_label;
use crate::provider::{EvmDeploy, EvmExecution, EvmGas, EvmGasLimit};

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

    /// Deploy bytecode via a create transaction, appending constructor args to the initcode,
    /// returning the new contract address plus the transaction hash.
    ///
    /// `limit` is the gas the create may burn: [`EvmGasLimit::Exact`] verbatim (out of gas if it
    /// does not suffice), [`EvmGasLimit::Estimated`] from an uncommitted simulation of this very
    /// create, scaled by the chain's `gas_adjustment`.
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        from: &Address,
        limit: EvmGasLimit,
    ) -> Result<EvmDeploy, EvmError> {
        let gas_limit = match limit {
            EvmGasLimit::Exact(n) => n,
            EvmGasLimit::Estimated => {
                let quote = self
                    .estimate_deploy_create(bytecode.clone(), constructor_args.as_ref(), from)
                    .await?;
                self.info.adjusted_gas_limit(quote.used)
            }
        };
        self.core
            .deploy_create(bytecode, constructor_args.as_ref(), *from, gas_limit)
            .map(EvmDeploy::from)
            .map_err(|f| EvmError::Deploy(f.deploy_message()))
    }

    /// Gas a [`deploy_create`](Self::deploy_create) of this bytecode would be billed, measured by
    /// running it against current state without committing it.
    ///
    /// `fee` is `None`: `revm` prices the transaction at zero and this repo carries no EVM
    /// gas-price config, so a forecast fee would be a fabrication (see [`EvmGas::fee`]).
    pub async fn estimate_deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        from: &Address,
    ) -> Result<EvmGas, EvmError> {
        self.core
            .estimate_create(bytecode, constructor_args.as_ref(), *from)
            .map(|used| EvmGas { used, fee: None })
            .map_err(|f| EvmError::Deploy(f.deploy_message()))
    }

    /// Execute a state-mutating call against `to`, returning its output plus emitted logs.
    pub async fn call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
        limit: EvmGasLimit,
    ) -> Result<EvmExecution, EvmError> {
        self.call_value(to, calldata, from, U256::ZERO, limit).await
    }

    /// Execute a state-mutating call against `to` carrying `value` wei (a payable call), returning
    /// its output plus emitted logs. The caller's balance is topped up to cover `value` first (the
    /// mock mints native funds on demand, like [`ChainProvider::new_account`]).
    ///
    /// `limit` is the gas the call may burn (see [`deploy_create`](Self::deploy_create)).
    pub async fn call_value(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
        value: U256,
        limit: EvmGasLimit,
    ) -> Result<EvmExecution, EvmError> {
        let gas_limit = match limit {
            EvmGasLimit::Exact(n) => n,
            EvmGasLimit::Estimated => {
                let quote = self
                    .estimate_call_value(to, calldata.as_ref(), from, value)
                    .await?;
                self.info.adjusted_gas_limit(quote.used)
            }
        };
        self.core
            .call(*to, calldata.as_ref(), *from, value, gas_limit)
            .map(EvmExecution::from)
            .map_err(|f| EvmError::Execute(f.call_message("call")))
    }

    /// Gas a [`call`](Self::call) with these arguments would be billed (see
    /// [`estimate_call_value`](Self::estimate_call_value)).
    pub async fn estimate_call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
    ) -> Result<EvmGas, EvmError> {
        self.estimate_call_value(to, calldata, from, U256::ZERO)
            .await
    }

    /// Gas a [`call_value`](Self::call_value) with these arguments would be billed, measured by
    /// running it against current state without committing it. Like the call it forecasts, the
    /// caller is topped up to cover `value`, and like it, a revert or halt is an error rather than
    /// a gas figure.
    ///
    /// `fee` is `None` (see [`estimate_deploy_create`](Self::estimate_deploy_create)).
    pub async fn estimate_call_value(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
        value: U256,
    ) -> Result<EvmGas, EvmError> {
        self.core
            .estimate_call(*to, calldata.as_ref(), *from, value)
            .map(|used| EvmGas { used, fee: None })
            .map_err(|f| EvmError::Execute(f.call_message("estimate_call")))
    }

    /// Transfer `amount` wei of native funds from `from` to `to`: a value-carrying call with empty
    /// calldata.
    ///
    /// Unlike [`call_value`](Self::call_value), the sender is *not* topped up: a transfer moves
    /// funds that already exist, so a short balance is an error here, exactly as a live chain would
    /// report it.
    pub async fn transfer_funds(
        &self,
        to: &Address,
        from: &Address,
        amount: U256,
        limit: EvmGasLimit,
    ) -> Result<EvmExecution, EvmError> {
        let held = self.balance(from).await?;
        if held < amount {
            return Err(EvmError::Balance(format!(
                "insufficient funds: {from} holds {held} wei, transfer needs {amount}"
            )));
        }
        self.call_value(to, [], from, amount, limit).await
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

    /// Read the raw storage value at `slot` for `addr`.
    pub async fn get_storage_at(&self, addr: &Address, slot: U256) -> Result<U256, EvmError> {
        self.core
            .storage(*addr, slot)
            .map_err(|f| EvmError::Query(f.call_message("get_storage_at")))
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
