//! EVM chain providers.
//!
//! [`EvmMockProvider`] wraps a `revm` in-memory EVM. [`EvmRpcProvider`] is a phase-1
//! stub returning [`EvmError::Unimplemented`] for every operation.
//!
//! The mock holds the EVM inside a `RefCell` so the read-only [`ChainProvider::query`]
//! (which `revm` still implements via a `&mut` static call) can run behind `&self`.

use std::cell::RefCell;

use cross_vm_core::{ChainKind, ChainProvider, CrossVmError};
use revm::context::result::{ExecutionResult, Output};
use revm::context::{Context, TxEnv};
use revm::context_interface::JournalTr;
use revm::database::InMemoryDB;
use revm::handler::{MainnetContext, MainnetEvm};
use revm::primitives::{keccak256, Address, Bytes, U256};
use revm::{DatabaseRef, ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};
use thiserror::Error;

use crate::chains::EvmChainInfo;

/// Default funding handed to accounts created via [`ChainProvider::new_account`]:
/// 100 ETH in wei.
pub const DEFAULT_FUNDING_WEI: u128 = 100_000_000_000_000_000_000;

/// Gas limit used for every mock transaction.
const TX_GAS_LIMIT: u64 = 30_000_000;

/// The concrete in-memory `revm` instance used by the mock provider.
pub type EvmInner = MainnetEvm<MainnetContext<InMemoryDB>>;

/// Errors surfaced by the EVM providers.
#[derive(Debug, Error)]
pub enum EvmError {
    /// Contract creation failed.
    #[error("deploy: {0}")]
    Deploy(String),
    /// A state-mutating call failed.
    #[error("execute: {0}")]
    Execute(String),
    /// A read-only call failed.
    #[error("query: {0}")]
    Query(String),
    /// A balance operation failed.
    #[error("balance: {0}")]
    Balance(String),
    /// Feature not implemented yet (live RPC in phase 1).
    #[error("unimplemented: {0}")]
    Unimplemented(String),
}

impl From<EvmError> for CrossVmError {
    fn from(e: EvmError) -> Self {
        let kind = ChainKind::Evm;
        match e {
            EvmError::Deploy(reason) => CrossVmError::Deploy { kind, reason },
            EvmError::Execute(reason) => CrossVmError::Execute { kind, reason },
            EvmError::Query(reason) => CrossVmError::Query { kind, reason },
            EvmError::Balance(reason) => CrossVmError::Balance { kind, reason },
            EvmError::Unimplemented(what) => CrossVmError::Unimplemented { kind, what },
        }
    }
}

/// Derive a deterministic address from a label (keccak of the label, low 20 bytes).
fn address_from_label(label: &str) -> Address {
    let h = keccak256(label.as_bytes());
    Address::from_slice(&h[12..])
}

/// In-process EVM provider backed by `revm`.
pub struct EvmMockProvider {
    evm: RefCell<EvmInner>,
    info: EvmChainInfo,
}

impl EvmMockProvider {
    /// Build a fresh mock chain from a predefined [`EvmChainInfo`].
    pub fn new(info: EvmChainInfo) -> Self {
        let mut ctx = Context::mainnet();
        ctx.cfg.chain_id = info.numeric_id();
        ctx.cfg.spec = info.spec_id;
        // A test harness should not fight nonce bookkeeping across many calls.
        ctx.cfg.disable_nonce_check = true;
        let evm = ctx.with_db(InMemoryDB::default()).build_mainnet();
        Self {
            evm: RefCell::new(evm),
            info,
        }
    }

    /// Decode an [`ExecutionResult`] into output bytes or a descriptive error.
    fn output_or_err(result: ExecutionResult, ctx: &str) -> Result<Bytes, EvmError> {
        match result {
            ExecutionResult::Success { output, .. } => Ok(output.into_data()),
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
    type Code = Bytes;
    type InitMsg = Bytes;
    type ExecMsg = Bytes;
    type QueryMsg = Bytes;
    type ContractRef = Address;
    type Response = Bytes;
    type QueryResponse = Bytes;
    type Balance = U256;
    type Error = EvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    fn new_account(&mut self, label: &str) -> Address {
        let addr = address_from_label(label);
        let _ = self.set_balance(&addr, U256::from(DEFAULT_FUNDING_WEI));
        addr
    }

    fn balance(&self, addr: &Address) -> Result<U256, EvmError> {
        let evm = self.evm.borrow();
        let info = evm
            .ctx
            .journaled_state
            .db()
            .basic_ref(*addr)
            .map_err(|e| EvmError::Balance(format!("{e:?}")))?;
        Ok(info.map(|i| i.balance).unwrap_or_default())
    }

    fn set_balance(&mut self, addr: &Address, amount: U256) -> Result<(), EvmError> {
        let db = self.evm.get_mut().ctx.journaled_state.db_mut();
        let mut info = db.basic_ref(*addr).ok().flatten().unwrap_or_default();
        info.balance = amount;
        db.insert_account_info(*addr, info);
        Ok(())
    }

    fn block_height(&self) -> u64 {
        self.evm.borrow().ctx.block.number.saturating_to::<u64>()
    }

    fn advance_blocks(&mut self, n: u64) {
        self.evm.get_mut().ctx.block.number += U256::from(n);
    }

    fn deploy(
        &mut self,
        code: Bytes,
        init: Bytes,
        sender: &Address,
    ) -> Result<Address, EvmError> {
        // EVM constructor args are appended to the creation bytecode.
        let mut initcode = code.to_vec();
        initcode.extend_from_slice(&init);
        let tx = TxEnv::builder()
            .caller(*sender)
            .chain_id(None)
            .create()
            .data(Bytes::from(initcode))
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let result = self
            .evm
            .get_mut()
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

    fn execute(
        &mut self,
        contract: &Address,
        msg: Bytes,
        sender: &Address,
    ) -> Result<Bytes, EvmError> {
        let tx = TxEnv::builder()
            .caller(*sender)
            .chain_id(None)
            .call(*contract)
            .data(msg)
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let result = self
            .evm
            .get_mut()
            .transact_commit(tx)
            .map_err(|e| EvmError::Execute(format!("{e:?}")))?;
        Self::output_or_err(result, "execute")
    }

    fn query(&self, contract: &Address, msg: Bytes) -> Result<Bytes, EvmError> {
        // A static call from the zero address; transact (not transact_commit) so the
        // call leaves no state behind.
        let tx = TxEnv::builder()
            .caller(Address::ZERO)
            .chain_id(None)
            .call(*contract)
            .data(msg)
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let outcome = self
            .evm
            .borrow_mut()
            .transact(tx)
            .map_err(|e| EvmError::Query(format!("{e:?}")))?;
        Self::output_or_err(outcome.result, "query").map_err(|e| match e {
            EvmError::Execute(s) => EvmError::Query(s),
            other => other,
        })
    }
}

/// Phase-1 stub for a live-RPC EVM provider. Constructs fine; every operation returns
/// [`EvmError::Unimplemented`].
pub struct EvmRpcProvider {
    info: EvmChainInfo,
}

impl EvmRpcProvider {
    /// Create an RPC provider bound to a chain's metadata.
    pub fn new(info: EvmChainInfo) -> Self {
        Self { info }
    }
}

impl ChainProvider for EvmRpcProvider {
    type Spec = EvmChainInfo;
    type Address = Address;
    type Account = Address;
    type Code = Bytes;
    type InitMsg = Bytes;
    type ExecMsg = Bytes;
    type QueryMsg = Bytes;
    type ContractRef = Address;
    type Response = Bytes;
    type QueryResponse = Bytes;
    type Balance = U256;
    type Error = EvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    fn new_account(&mut self, label: &str) -> Address {
        address_from_label(label)
    }

    fn balance(&self, _addr: &Address) -> Result<U256, EvmError> {
        Err(EvmError::Unimplemented("rpc balance".into()))
    }

    fn set_balance(&mut self, _addr: &Address, _amount: U256) -> Result<(), EvmError> {
        Err(EvmError::Unimplemented("rpc set_balance".into()))
    }

    fn block_height(&self) -> u64 {
        0
    }

    fn advance_blocks(&mut self, _n: u64) {}

    fn deploy(&mut self, _code: Bytes, _init: Bytes, _sender: &Address) -> Result<Address, EvmError> {
        Err(EvmError::Unimplemented("rpc deploy".into()))
    }

    fn execute(
        &mut self,
        _contract: &Address,
        _msg: Bytes,
        _sender: &Address,
    ) -> Result<Bytes, EvmError> {
        Err(EvmError::Unimplemented("rpc execute".into()))
    }

    fn query(&self, _contract: &Address, _msg: Bytes) -> Result<Bytes, EvmError> {
        Err(EvmError::Unimplemented("rpc query".into()))
    }
}
