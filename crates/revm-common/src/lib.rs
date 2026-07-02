//! Shared in-process `revm` core for the EVM-derived chain providers.
//!
//! The EVM (`cross-vm-solidity`) and Tron (`cross-vm-tron`) mocks execute on the same `revm`
//! machinery; before this crate their `provider/mock.rs` files were structural clones. What is
//! genuinely shared lives here: VM construction, the transaction plumbing (`deploy_create`,
//! value-carrying `call`, `static_call`), and the account/block surface (`balance`,
//! `set_balance`, `block_height`, `advance_blocks`).
//!
//! What is NOT shared stays in each provider, by design:
//! - address and balance boundary types (Tron's base58check `TronAddress` and u64 sun convert to
//!   `Address`/`U256` at the provider surface),
//! - per-VM construction differences via the [`RevmCore::new`] `customize` hook (Tron injects the
//!   TIP-272 precompile set and the TVM-native opcodes; the EVM mock seeds its block number and
//!   the shared mock clock),
//! - per-VM semantics around the core calls (Tron's bandwidth accounting, its
//!   `DIVERGENCE(tron)` CREATE-address caveat), and each provider's error enum, built from
//!   [`ExecFailure`] with the provider's historical message shapes.

use std::cell::RefCell;
use std::rc::Rc;

use alloy_primitives::{Address, Bytes, Log, U256};
use cross_vm_core::BlockTime;
use revm::context::result::{ExecutionResult, Output};
use revm::context::{Context, TxEnv};
use revm::context_interface::JournalTr;
use revm::database::InMemoryDB;
use revm::handler::{MainnetContext, MainnetEvm};
use revm::primitives::hardfork::SpecId;
use revm::{DatabaseRef, ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

/// The concrete in-memory `revm` instance behind every EVM-derived mock provider.
pub type RevmInner = MainnetEvm<MainnetContext<InMemoryDB>>;

/// Gas limit used for every mock transaction.
pub const TX_GAS_LIMIT: u64 = 30_000_000;

/// How one `revm` interaction failed, transport-agnostic and message-preserving.
///
/// Providers map this into their own error enums; the [`deploy_message`](ExecFailure::deploy_message)
/// and [`call_message`](ExecFailure::call_message) helpers reproduce the exact strings the
/// providers emitted before the extraction, so tests and log consumers see no change.
#[derive(Debug, Clone)]
pub enum ExecFailure {
    /// `transact`/`transact_commit` itself errored (`format!("{e:?}")` of the revm error).
    Internal(String),
    /// The transaction reverted; the payload is the hex-encoded revert output (no `0x` prefix).
    Revert(String),
    /// The VM halted; the payload is `format!("{reason:?}")`.
    Halt(String),
    /// A create transaction succeeded but returned no contract address.
    NoCreateAddress,
}

impl ExecFailure {
    /// The historical deploy-path message: the raw internal error, `no contract address
    /// returned`, `reverted: 0x..`, or `halted: ..`.
    pub fn deploy_message(&self) -> String {
        match self {
            ExecFailure::Internal(s) => s.clone(),
            ExecFailure::NoCreateAddress => "no contract address returned".into(),
            ExecFailure::Revert(hex) => format!("reverted: 0x{hex}"),
            ExecFailure::Halt(reason) => format!("halted: {reason}"),
        }
    }

    /// The historical call-path message, prefixed by the operation (`call` / `static_call`):
    /// the raw internal error, `{ctx} reverted: 0x..`, or `{ctx} halted: ..`.
    pub fn call_message(&self, ctx: &str) -> String {
        match self {
            ExecFailure::Internal(s) => s.clone(),
            // Unreachable on the call paths (only create yields it); message kept sensible anyway.
            ExecFailure::NoCreateAddress => "no contract address returned".into(),
            ExecFailure::Revert(hex) => format!("{ctx} reverted: 0x{hex}"),
            ExecFailure::Halt(reason) => format!("{ctx} halted: {reason}"),
        }
    }
}

/// The result of a state-mutating call on the core: return data plus the logs emitted during
/// execution. Providers wrap this into their own execution types (adding e.g. a tx hash slot).
#[derive(Clone, Debug, Default)]
pub struct Execution {
    /// ABI-encoded return data.
    pub output: Bytes,
    /// Logs (events) emitted during execution, in order.
    pub logs: Vec<Log>,
}

/// The shared in-process `revm` core.
///
/// The VM lives behind `Rc<RefCell<_>>` so a provider handle is cheap to `clone` and every clone
/// shares one chain state; read-only [`static_call`](RevmCore::static_call) (which `revm` still
/// implements via a `&mut` static call) runs behind `&self`.
#[derive(Clone)]
pub struct RevmCore {
    evm: Rc<RefCell<RevmInner>>,
}

impl RevmCore {
    /// Build a fresh in-memory VM at `chain_id`/`spec` (nonce checking disabled: a test harness
    /// should not fight nonce bookkeeping across many calls). `customize` runs once on the built
    /// VM before it is shared: Tron injects its precompile set and TVM opcodes here, the EVM mock
    /// seeds its block number and mock clock. Because the spec is fixed at construction, revm's
    /// per-transaction `set_spec` sees no change and leaves those customizations in place.
    pub fn new(chain_id: u64, spec: SpecId, customize: impl FnOnce(&mut RevmInner)) -> Self {
        let mut ctx = Context::mainnet();
        ctx.cfg.chain_id = chain_id;
        ctx.cfg.spec = spec;
        ctx.cfg.disable_nonce_check = true;
        let mut evm = ctx.with_db(InMemoryDB::default()).build_mainnet();
        customize(&mut evm);
        Self {
            evm: Rc::new(RefCell::new(evm)),
        }
    }

    /// Deploy bytecode via a create transaction, appending constructor args to the initcode.
    pub fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: &[u8],
        from: Address,
    ) -> Result<Address, ExecFailure> {
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args);
        let tx = TxEnv::builder()
            .caller(from)
            .chain_id(None)
            .create()
            .data(Bytes::from(initcode))
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let result = self
            .evm
            .borrow_mut()
            .transact_commit(tx)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
        match result {
            ExecutionResult::Success {
                output: Output::Create(_, Some(addr)),
                ..
            } => Ok(addr),
            ExecutionResult::Success { .. } => Err(ExecFailure::NoCreateAddress),
            ExecutionResult::Revert { output, .. } => Err(ExecFailure::Revert(hex_encode(&output))),
            ExecutionResult::Halt { reason, .. } => Err(ExecFailure::Halt(format!("{reason:?}"))),
        }
    }

    /// Execute a state-mutating call against `to` carrying `value` (a payable call when nonzero),
    /// returning its output plus emitted logs. On a nonzero `value` the caller's balance is topped
    /// up to cover it first (a mock mints native funds on demand).
    pub fn call(
        &self,
        to: Address,
        calldata: &[u8],
        from: Address,
        value: U256,
    ) -> Result<Execution, ExecFailure> {
        if !value.is_zero() {
            let mut evm = self.evm.borrow_mut();
            let db = evm.ctx.journaled_state.db_mut();
            let mut info = db.basic_ref(from).ok().flatten().unwrap_or_default();
            if info.balance < value {
                info.balance = value;
                db.insert_account_info(from, info);
            }
        }
        let tx = TxEnv::builder()
            .caller(from)
            .chain_id(None)
            .call(to)
            .value(value)
            .data(Bytes::copy_from_slice(calldata))
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let result = self
            .evm
            .borrow_mut()
            .transact_commit(tx)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
        exec_or_err(result)
    }

    /// Run a read-only static call against `to`. Logs are dropped: getters do not emit, and a
    /// static call leaves no state.
    pub fn static_call(&self, to: Address, calldata: &[u8]) -> Result<Bytes, ExecFailure> {
        let tx = TxEnv::builder()
            .caller(Address::ZERO)
            .chain_id(None)
            .call(to)
            .data(Bytes::copy_from_slice(calldata))
            .gas_limit(TX_GAS_LIMIT)
            .build_fill();
        let outcome = self
            .evm
            .borrow_mut()
            .transact(tx)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
        exec_or_err(outcome.result).map(|e| e.output)
    }

    /// The native balance of `addr`, as revm's `U256`.
    pub fn balance(&self, addr: Address) -> Result<U256, ExecFailure> {
        let evm = self.evm.borrow();
        let info = evm
            .ctx
            .journaled_state
            .db()
            .basic_ref(addr)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
        Ok(info.map(|i| i.balance).unwrap_or_default())
    }

    /// Set the native balance of `addr`.
    pub fn set_balance(&self, addr: Address, amount: U256) {
        let mut evm = self.evm.borrow_mut();
        let db = evm.ctx.journaled_state.db_mut();
        let mut info = db.basic_ref(addr).ok().flatten().unwrap_or_default();
        info.balance = amount;
        db.insert_account_info(addr, info);
    }

    /// The current block number.
    pub fn block_height(&self) -> u64 {
        self.evm.borrow().ctx.block.number.saturating_to::<u64>()
    }

    /// Advance `n` blocks and move the block timestamp per `time`.
    pub fn advance_blocks(&self, n: u64, time: BlockTime) {
        let mut evm = self.evm.borrow_mut();
        evm.ctx.block.number += U256::from(n);
        let current = evm.ctx.block.timestamp.saturating_to::<u64>();
        evm.ctx.block.timestamp = U256::from(time.apply(current));
    }
}

/// Decode an [`ExecutionResult`] into output data plus logs, or the matching [`ExecFailure`].
fn exec_or_err(result: ExecutionResult) -> Result<Execution, ExecFailure> {
    match result {
        ExecutionResult::Success { output, logs, .. } => Ok(Execution {
            output: output.into_data(),
            logs,
        }),
        ExecutionResult::Revert { output, .. } => Err(ExecFailure::Revert(hex_encode(&output))),
        ExecutionResult::Halt { reason, .. } => Err(ExecFailure::Halt(format!("{reason:?}"))),
    }
}

/// Minimal hex encoder so error messages need no extra dependency.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> RevmCore {
        RevmCore::new(1, SpecId::CANCUN, |_| {})
    }

    #[test]
    fn balance_roundtrip_and_default_zero() {
        let c = core();
        let a = Address::repeat_byte(0x11);
        assert_eq!(c.balance(a).unwrap(), U256::ZERO);
        c.set_balance(a, U256::from(7u64));
        assert_eq!(c.balance(a).unwrap(), U256::from(7u64));
    }

    #[test]
    fn deploy_empty_runtime_yields_address() {
        let c = core();
        // PUSH1 0x00, PUSH1 0x00, RETURN: deploys a zero-length runtime.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let addr = c
            .deploy_create(initcode, &[], Address::repeat_byte(0x22))
            .expect("empty-runtime deploy succeeds");
        assert_ne!(addr, Address::ZERO);
    }

    #[test]
    fn payable_call_tops_up_caller() {
        let c = core();
        let from = Address::repeat_byte(0x33);
        let to = Address::repeat_byte(0x44);
        assert_eq!(c.balance(from).unwrap(), U256::ZERO);
        // A plain value transfer to an empty account succeeds after the top-up.
        c.call(to, &[], from, U256::from(5u64)).expect("value call");
        assert_eq!(c.balance(to).unwrap(), U256::from(5u64));
    }

    #[test]
    fn failure_messages_match_the_historical_shapes() {
        assert_eq!(
            ExecFailure::Revert("beef".into()).deploy_message(),
            "reverted: 0xbeef"
        );
        assert_eq!(
            ExecFailure::Revert("beef".into()).call_message("call"),
            "call reverted: 0xbeef"
        );
        assert_eq!(
            ExecFailure::Halt("OutOfGas".into()).call_message("static_call"),
            "static_call halted: OutOfGas"
        );
        assert_eq!(
            ExecFailure::NoCreateAddress.deploy_message(),
            "no contract address returned"
        );
    }

    #[test]
    fn advance_blocks_moves_height_and_clock() {
        let c = core();
        let h = c.block_height();
        c.advance_blocks(3, BlockTime::Increment(10));
        assert_eq!(c.block_height(), h + 3);
    }
}
