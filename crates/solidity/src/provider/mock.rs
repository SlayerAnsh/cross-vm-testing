//! In-process EVM provider backed by `revm`.
//!
//! [`EvmMockProvider`] holds the EVM inside a `RefCell` so read-only
//! [`EvmMockProvider::static_call`] (which `revm` still implements via a `&mut` static
//! call) can run behind `&self`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, Bytes, Log, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use revm::context::result::{ExecutionResult, Output};
use revm::context::{Context, TxEnv};
use revm::context_interface::JournalTr;
use revm::database::InMemoryDB;
use revm::handler::{MainnetContext, MainnetEvm};
use revm::{DatabaseRef, ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

use crate::chains::EvmChainInfo;
use crate::error::EvmError;
use crate::provider::address::address_from_label;

/// Default funding handed to accounts created via [`ChainProvider::new_account`]:
/// 100 ETH in wei.
pub const DEFAULT_FUNDING_WEI: u128 = 100_000_000_000_000_000_000;

/// Gas limit used for every mock transaction.
const TX_GAS_LIMIT: u64 = 30_000_000;

/// The concrete in-memory `revm` instance used by the mock provider.
pub type EvmInner = MainnetEvm<MainnetContext<InMemoryDB>>;

/// In-process EVM provider backed by `revm`.
///
/// The EVM lives behind `Rc<RefCell<_>>` so the handle is cheap to `clone` and every clone
/// shares one chain state. This lets a contract own its own handle (`Contract::new(chain)`)
/// while the test still drives the same chain, and lets the contract operations run behind
/// `&self`.
#[derive(Clone)]
pub struct EvmMockProvider {
    evm: Rc<RefCell<EvmInner>>,
    info: EvmChainInfo,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, PrivateKeySigner>>>,
}

impl EvmMockProvider {
    /// Build a fresh mock chain from a predefined [`EvmChainInfo`].
    pub fn new(info: EvmChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let mut ctx = Context::mainnet();
        ctx.cfg.chain_id = info.numeric_id();
        ctx.cfg.spec = info.spec_id;
        // A test harness should not fight nonce bookkeeping across many calls.
        ctx.cfg.disable_nonce_check = true;
        let mut evm = ctx.with_db(InMemoryDB::default()).build_mainnet();
        // Start at block 1 (a 0 marker is indistinguishable from "unset" in contracts that record
        // `pending[seq] = block.number`) and at the shared mock clock so cross-VM packet timeouts
        // compare correctly against the cosmos chain.
        evm.ctx.block.number = U256::from(1u64);
        evm.ctx.block.timestamp = U256::from(cross_vm_core::MOCK_BLOCK_TIMESTAMP);
        Self {
            evm: Rc::new(RefCell::new(evm)),
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
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args.as_ref());
        let tx = TxEnv::builder()
            .caller(*from)
            .chain_id(None)
            .create()
            .data(Bytes::from(initcode))
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let result = self
            .evm
            .borrow_mut()
            .transact_commit(tx)
            .map_err(|e| EvmError::Deploy(format!("{e:?}")))?;
        match result {
            ExecutionResult::Success {
                output: Output::Create(_, Some(addr)),
                ..
            } => Ok(addr),
            ExecutionResult::Success { .. } => {
                Err(EvmError::Deploy("no contract address returned".into()))
            }
            ExecutionResult::Revert { output, .. } => Err(EvmError::Deploy(format!(
                "reverted: 0x{}",
                hex_encode(&output)
            ))),
            ExecutionResult::Halt { reason, .. } => {
                Err(EvmError::Deploy(format!("halted: {reason:?}")))
            }
        }
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
        if !value.is_zero() {
            let mut evm = self.evm.borrow_mut();
            let db = evm.ctx.journaled_state.db_mut();
            let mut info = db.basic_ref(*from).ok().flatten().unwrap_or_default();
            if info.balance < value {
                info.balance = value;
                db.insert_account_info(*from, info);
            }
        }
        let tx = TxEnv::builder()
            .caller(*from)
            .chain_id(None)
            .call(*to)
            .value(value)
            .data(Bytes::copy_from_slice(calldata.as_ref()))
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let result = self
            .evm
            .borrow_mut()
            .transact_commit(tx)
            .map_err(|e| EvmError::Execute(format!("{e:?}")))?;
        Self::exec_or_err(result, "call")
    }

    /// Run a read-only static call against `to`.
    pub async fn static_call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, EvmError> {
        let tx = TxEnv::builder()
            .caller(Address::ZERO)
            .chain_id(None)
            .call(*to)
            .data(Bytes::copy_from_slice(calldata.as_ref()))
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let outcome = self
            .evm
            .borrow_mut()
            .transact(tx)
            .map_err(|e| EvmError::Query(format!("{e:?}")))?;
        // A read drops the logs: getters do not emit, and a static call leaves no state.
        Self::exec_or_err(outcome.result, "static_call").map(|e| e.output)
    }

    /// Decode an [`ExecutionResult`] into output data plus logs, or a descriptive error.
    fn exec_or_err(result: ExecutionResult, ctx: &str) -> Result<EvmExecution, EvmError> {
        match result {
            ExecutionResult::Success { output, logs, .. } => Ok(EvmExecution {
                output: output.into_data(),
                logs,
                tx_hash: None,
            }),
            ExecutionResult::Revert { output, .. } => Err(EvmError::Execute(format!(
                "{ctx} reverted: 0x{}",
                hex_encode(&output)
            ))),
            ExecutionResult::Halt { reason, .. } => {
                Err(EvmError::Execute(format!("{ctx} halted: {reason:?}")))
            }
        }
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

/// Minimal hex encoder so we do not pull a dependency for error messages.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
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
        let _ = self
            .set_balance(&addr, U256::from(DEFAULT_FUNDING_WEI))
            .await;
        addr
    }

    async fn balance(&self, addr: &Address) -> Result<U256, EvmError> {
        let evm = self.evm.borrow();
        let info = evm
            .ctx
            .journaled_state
            .db()
            .basic_ref(*addr)
            .map_err(|e| EvmError::Balance(format!("{e:?}")))?;
        Ok(info.map(|i| i.balance).unwrap_or_default())
    }

    async fn set_balance(&mut self, addr: &Address, amount: U256) -> Result<(), EvmError> {
        let mut evm = self.evm.borrow_mut();
        let db = evm.ctx.journaled_state.db_mut();
        let mut info = db.basic_ref(*addr).ok().flatten().unwrap_or_default();
        info.balance = amount;
        db.insert_account_info(*addr, info);
        Ok(())
    }

    async fn block_height(&self) -> u64 {
        self.evm.borrow().ctx.block.number.saturating_to::<u64>()
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        let mut evm = self.evm.borrow_mut();
        evm.ctx.block.number += U256::from(n);
        let current = evm.ctx.block.timestamp.saturating_to::<u64>();
        evm.ctx.block.timestamp = U256::from(time.apply(current));
    }
}
