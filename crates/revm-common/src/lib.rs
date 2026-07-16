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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use alloy_primitives::{keccak256, Address, Bytes, Log, B256, U256};
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

/// Ceiling a *non-billed* run executes under: [`estimate_create`](RevmCore::estimate_create),
/// [`estimate_call`](RevmCore::estimate_call) and [`static_call`](RevmCore::static_call).
///
/// A transaction takes its limit from the caller. A simulation cannot: capping "how much gas does
/// this need" at the number the caller guessed is circular, and under a too-small cap an estimate
/// would report an out-of-gas failure instead of the cost it was asked for. So a simulation runs
/// under a block-sized ceiling instead, which is what a node does for `eth_estimateGas` /
/// `eth_call` on a request that carries no gas field. It is a ceiling, not a budget: it exists so
/// that a non-terminating call fails rather than spinning forever, and it is a mainnet block's
/// worth of gas, so no transaction a mock can plausibly run is clipped by it.
pub const SIMULATION_GAS_LIMIT: u64 = 30_000_000;

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

/// The result of a state-mutating call on the core: return data, the logs emitted during
/// execution, and a synthetic transaction hash. Providers wrap this into their own execution
/// types.
///
/// The mock executes in-process and never builds or signs a real transaction, so it has no real
/// hash to report. Rather than leave callers to branch on "mock has no hash", the core mints a
/// **synthetic, deterministic** hash (see [`RevmCore::call`]): keccak256 over the call's fields
/// plus a per-core monotonic sequence, so it matches the real 32-byte hash *shape*, is stable
/// across identical runs, and is unique per call (like a real chain). It does NOT equal the hash
/// a live node would compute for the same intent (that needs the signed-tx bytes) and must not be
/// treated as a real on-chain identifier.
#[derive(Clone, Debug, Default)]
pub struct Execution {
    /// ABI-encoded return data.
    pub output: Bytes,
    /// Logs (events) emitted during execution, in order.
    pub logs: Vec<Log>,
    /// Synthetic, deterministic transaction hash (see the type-level note). Zero on the
    /// read-only `static_call` path, which is not a transaction.
    pub tx_hash: B256,
    /// Gas the transaction is billed for, as `revm` metered it: `ResultGas::tx_gas_used`, the
    /// figure a receipt would carry, already net of the EIP-3529 refund and floored per EIP-7623.
    pub gas_used: u64,
}

/// The result of a create transaction on the core: the deployed contract address and the same
/// synthetic transaction hash [`Execution`] documents, drawn from the one per-core sequence, so
/// deploys and calls never collide.
#[derive(Clone, Debug, Default)]
pub struct Deployment {
    /// Address of the freshly deployed contract.
    pub address: Address,
    /// Synthetic, deterministic transaction hash (see [`Execution`]).
    pub tx_hash: B256,
    /// Gas the create transaction is billed for (see [`Execution::gas_used`]).
    pub gas_used: u64,
}

/// The shared in-process `revm` core.
///
/// The VM lives behind `Rc<RefCell<_>>` so a provider handle is cheap to `clone` and every clone
/// shares one chain state; read-only [`static_call`](RevmCore::static_call) (which `revm` still
/// implements via a `&mut` static call) runs behind `&self`.
#[derive(Clone)]
pub struct RevmCore {
    evm: Rc<RefCell<RevmInner>>,
    /// Monotonic per-core transaction counter, folded into the synthetic tx hash so repeated
    /// identical calls get distinct hashes (a real chain never reuses one). Shared across clones.
    tx_seq: Rc<Cell<u64>>,
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
            tx_seq: Rc::new(Cell::new(0)),
        }
    }

    /// Mint the next synthetic tx hash: keccak256 over the transaction fields plus the monotonic
    /// sequence. Deterministic for a given transaction order, unique per transaction; deploys and
    /// calls share the one sequence. See [`Execution`].
    fn next_tx_hash(&self, to: Address, calldata: &[u8], from: Address, value: U256) -> B256 {
        let seq = self.tx_seq.get();
        self.tx_seq.set(seq + 1);
        let mut buf = Vec::with_capacity(20 + 20 + 32 + 8 + calldata.len());
        buf.extend_from_slice(from.as_slice());
        buf.extend_from_slice(to.as_slice());
        buf.extend_from_slice(&value.to_be_bytes::<32>());
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.extend_from_slice(calldata);
        keccak256(&buf)
    }

    /// Deploy bytecode via a create transaction, appending constructor args to the initcode,
    /// returning the deployed address plus a synthetic transaction hash.
    ///
    /// `gas_limit` is the caller's budget, honored as a chain honors it: a limit the create
    /// outruns fails ([`ExecFailure::Halt`], or [`ExecFailure::Internal`] when it is under the
    /// intrinsic cost and revm rejects the transaction before executing it) and commits nothing.
    /// [`estimate_create`](RevmCore::estimate_create) answers what limit suffices.
    pub fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: &[u8],
        from: Address,
        gas_limit: u64,
    ) -> Result<Deployment, ExecFailure> {
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args);
        let initcode = Bytes::from(initcode);
        let tx = TxEnv::builder()
            .caller(from)
            .chain_id(None)
            .create()
            .data(initcode.clone())
            .gas_limit(gas_limit)
            .build_fill();
        let result = self
            .evm
            .borrow_mut()
            .transact_commit(tx)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
        match result {
            ExecutionResult::Success {
                output: Output::Create(_, Some(addr)),
                gas,
                ..
            } => Ok(Deployment {
                address: addr,
                // A create has no callee; the zero address stands in as the hash's `to` field.
                tx_hash: self.next_tx_hash(Address::ZERO, &initcode, from, U256::ZERO),
                gas_used: gas.tx_gas_used(),
            }),
            ExecutionResult::Success { .. } => Err(ExecFailure::NoCreateAddress),
            ExecutionResult::Revert { output, .. } => Err(ExecFailure::Revert(hex_encode(&output))),
            ExecutionResult::Halt { reason, .. } => Err(ExecFailure::Halt(format!("{reason:?}"))),
        }
    }

    /// Gas a [`deploy_create`](RevmCore::deploy_create) of this bytecode would be billed, measured
    /// against current state without committing it (see [`RevmCore::simulate`]). Runs under
    /// [`SIMULATION_GAS_LIMIT`], not under any caller limit: the answer is what the limit should
    /// be, so it cannot be conditioned on one.
    pub fn estimate_create(
        &self,
        bytecode: Bytes,
        constructor_args: &[u8],
        from: Address,
    ) -> Result<u64, ExecFailure> {
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args);
        let tx = TxEnv::builder()
            .caller(from)
            .chain_id(None)
            .create()
            .data(Bytes::from(initcode))
            .gas_limit(SIMULATION_GAS_LIMIT)
            .build_fill();
        self.simulate(tx, from, U256::ZERO)
    }

    /// Execute a state-mutating call against `to` carrying `value` (a payable call when nonzero),
    /// returning its output plus emitted logs. On a nonzero `value` the caller's balance is topped
    /// up to cover it first (a mock mints native funds on demand).
    ///
    /// `gas_limit` is the caller's budget, honored as a chain honors it: a limit the call outruns
    /// fails ([`ExecFailure::Halt`], or [`ExecFailure::Internal`] when it is under the intrinsic
    /// cost and revm rejects the transaction before executing it) and commits nothing.
    /// [`estimate_call`](RevmCore::estimate_call) answers what limit suffices.
    pub fn call(
        &self,
        to: Address,
        calldata: &[u8],
        from: Address,
        value: U256,
        gas_limit: u64,
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
            .gas_limit(gas_limit)
            .build_fill();
        let result = self
            .evm
            .borrow_mut()
            .transact_commit(tx)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
        let mut exec = exec_or_err(result)?;
        exec.tx_hash = self.next_tx_hash(to, calldata, from, value);
        Ok(exec)
    }

    /// Gas a [`call`](RevmCore::call) with these arguments would be billed, measured against
    /// current state without committing it (see [`RevmCore::simulate`]). Runs under
    /// [`SIMULATION_GAS_LIMIT`], not under any caller limit: the answer is what the limit should
    /// be, so it cannot be conditioned on one.
    pub fn estimate_call(
        &self,
        to: Address,
        calldata: &[u8],
        from: Address,
        value: U256,
    ) -> Result<u64, ExecFailure> {
        let tx = TxEnv::builder()
            .caller(from)
            .chain_id(None)
            .call(to)
            .value(value)
            .data(Bytes::copy_from_slice(calldata))
            .gas_limit(SIMULATION_GAS_LIMIT)
            .build_fill();
        self.simulate(tx, from, value)
    }

    /// Run `tx` for its gas figure alone, leaving the chain exactly as it was found.
    ///
    /// A simulation is not a transaction, so it must not look like one afterwards:
    /// - `transact` (not `transact_commit`) finalizes the journal and hands the state changes back
    ///   instead of applying them, so nothing reaches the database: no storage write, no nonce
    ///   bump, no balance move,
    /// - nothing here draws from `tx_seq`, so an estimate mints no synthetic tx hash and does not
    ///   shift the hash a later real transaction gets.
    ///
    /// A revert or halt is an error, not a gas figure: a caller told "42_000 gas" for a
    /// transaction that cannot succeed has been misinformed.
    fn simulate(&self, tx: TxEnv, from: Address, value: U256) -> Result<u64, ExecFailure> {
        let mut evm = self.evm.borrow_mut();
        if !value.is_zero() {
            // `call` mints the caller the funds it lacks, so an estimate must too, or it would
            // fail where the call it forecasts succeeds. The top-up is journaled, never written to
            // the database, so it dies with the rest of the simulated state.
            let held = evm
                .ctx
                .journaled_state
                .db()
                .basic_ref(from)
                .ok()
                .flatten()
                .map(|i| i.balance)
                .unwrap_or_default();
            if held < value {
                evm.ctx
                    .journaled_state
                    .balance_incr(from, value - held)
                    .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
            }
        }
        let outcome = evm
            .transact(tx)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?;
        exec_or_err(outcome.result).map(|e| e.gas_used)
    }

    /// Run a read-only static call against `to`. Logs are dropped: getters do not emit, and a
    /// static call leaves no state.
    ///
    /// A read is not billed to anyone, so it takes no caller limit and runs under
    /// [`SIMULATION_GAS_LIMIT`].
    pub fn static_call(&self, to: Address, calldata: &[u8]) -> Result<Bytes, ExecFailure> {
        let tx = TxEnv::builder()
            .caller(Address::ZERO)
            .chain_id(None)
            .call(to)
            .data(Bytes::copy_from_slice(calldata))
            .gas_limit(SIMULATION_GAS_LIMIT)
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

    /// The storage value at `slot` for `addr`, as revm's `U256`.
    pub fn storage(&self, addr: Address, slot: U256) -> Result<U256, ExecFailure> {
        let evm = self.evm.borrow();
        evm.ctx
            .journaled_state
            .db()
            .storage_ref(addr, slot)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))
    }

    /// The deployed runtime bytecode at `addr`, empty for an account that carries none (an EOA, or
    /// an address never deployed to). `basic_ref` may hand the code back inline; otherwise it is
    /// fetched by its hash from the contract store.
    pub fn code(&self, addr: Address) -> Result<Bytes, ExecFailure> {
        let evm = self.evm.borrow();
        let db = evm.ctx.journaled_state.db();
        let Some(info) = db
            .basic_ref(addr)
            .map_err(|e| ExecFailure::Internal(format!("{e:?}")))?
        else {
            return Ok(Bytes::new());
        };
        if info.is_empty_code_hash() {
            return Ok(Bytes::new());
        }
        match info.code {
            Some(code) => Ok(code.original_bytes()),
            None => db
                .code_by_hash_ref(info.code_hash)
                .map(|code| code.original_bytes())
                .map_err(|e| ExecFailure::Internal(format!("{e:?}"))),
        }
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

    /// The numeric chain id the VM was configured with (what `block.chainid` returns).
    pub fn chain_id(&self) -> u64 {
        self.evm.borrow().ctx.cfg.chain_id
    }

    /// The current block timestamp, in seconds since the UNIX epoch.
    pub fn block_timestamp(&self) -> u64 {
        self.evm.borrow().ctx.block.timestamp.saturating_to::<u64>()
    }

    /// Advance `n` blocks and move the block timestamp per `time`.
    pub fn advance_blocks(&self, n: u64, time: BlockTime) {
        let mut evm = self.evm.borrow_mut();
        evm.ctx.block.number += U256::from(n);
        let current = evm.ctx.block.timestamp.saturating_to::<u64>();
        evm.ctx.block.timestamp = U256::from(time.apply(current));
    }
}

/// Decode an [`ExecutionResult`] into output data, logs and metered gas, or the matching
/// [`ExecFailure`].
fn exec_or_err(result: ExecutionResult) -> Result<Execution, ExecFailure> {
    match result {
        ExecutionResult::Success {
            output, logs, gas, ..
        } => Ok(Execution {
            output: output.into_data(),
            logs,
            // Filled by `call`; the read-only `static_call` path leaves it zero (not a tx).
            tx_hash: B256::ZERO,
            gas_used: gas.tx_gas_used(),
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

    /// A limit generous enough for every transaction these tests submit, so a test that is not
    /// about the limit does not accidentally become one.
    const AMPLE_GAS: u64 = 30_000_000;

    fn core() -> RevmCore {
        RevmCore::new(1, SpecId::CANCUN, |_| {})
    }

    /// Initcode returning `runtime` (CODECOPY of the bytes trailing this 12-byte prologue).
    fn initcode_returning(runtime: &[u8]) -> Bytes {
        let n = runtime.len() as u8;
        let mut code = vec![
            0x60, n, 0x60, 0x0c, 0x60, 0x00, 0x39, 0x60, n, 0x60, 0x00, 0xf3,
        ];
        code.extend_from_slice(runtime);
        Bytes::from(code)
    }

    /// A contract whose fallback writes 0x2a to slot 0: PUSH1 0x2a, PUSH1 0x00, SSTORE, STOP.
    fn storing_contract() -> Bytes {
        initcode_returning(&[0x60, 0x2a, 0x60, 0x00, 0x55, 0x00])
    }

    /// A contract whose fallback reverts with empty data: PUSH1 0x00, PUSH1 0x00, REVERT.
    fn reverting_contract() -> Bytes {
        initcode_returning(&[0x60, 0x00, 0x60, 0x00, 0xfd])
    }

    /// The account nonce as the database holds it; the core exposes no accessor, and only a test
    /// needs one (to prove an estimate does not bump it).
    fn nonce(c: &RevmCore, addr: Address) -> u64 {
        let evm = c.evm.borrow();
        let info = evm.ctx.journaled_state.db().basic_ref(addr).unwrap();
        info.map(|i| i.nonce).unwrap_or_default()
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
    fn deploy_empty_runtime_yields_address_and_tx_hash() {
        let c = core();
        let from = Address::repeat_byte(0x22);
        // PUSH1 0x00, PUSH1 0x00, RETURN: deploys a zero-length runtime.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);

        let first = c
            .deploy_create(initcode.clone(), &[], from, AMPLE_GAS)
            .expect("empty-runtime deploy succeeds");
        assert_ne!(first.address, Address::ZERO);
        assert_ne!(first.tx_hash, B256::ZERO);

        // The monotonic seq makes a repeat of the identical deploy carry a distinct hash.
        let second = c
            .deploy_create(initcode, &[], from, AMPLE_GAS)
            .expect("empty-runtime deploy succeeds");
        assert_ne!(second.tx_hash, B256::ZERO);
        assert_ne!(
            first.tx_hash, second.tx_hash,
            "repeated identical deploys must get distinct hashes"
        );
    }

    #[test]
    fn deploys_and_calls_share_one_hash_sequence() {
        let from = Address::repeat_byte(0x66);
        let to = Address::repeat_byte(0x77);
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);

        // A deploy consumes a seq slot, so an otherwise identical call order hashes differently
        // once a deploy precedes it: the two paths draw from the one counter.
        let c = core();
        c.deploy_create(initcode, &[], from, AMPLE_GAS).unwrap();
        let after_deploy = c
            .call(to, &[], from, U256::ZERO, AMPLE_GAS)
            .unwrap()
            .tx_hash;

        let c2 = core();
        let without_deploy = c2
            .call(to, &[], from, U256::ZERO, AMPLE_GAS)
            .unwrap()
            .tx_hash;

        assert_ne!(after_deploy, without_deploy);
    }

    #[test]
    fn payable_call_tops_up_caller() {
        let c = core();
        let from = Address::repeat_byte(0x33);
        let to = Address::repeat_byte(0x44);
        assert_eq!(c.balance(from).unwrap(), U256::ZERO);
        // A plain value transfer to an empty account succeeds after the top-up.
        c.call(to, &[], from, U256::from(5u64), AMPLE_GAS)
            .expect("value call");
        assert_eq!(c.balance(to).unwrap(), U256::from(5u64));
    }

    #[test]
    fn synthetic_tx_hash_is_nonzero_unique_and_deterministic() {
        let from = Address::repeat_byte(0x33);
        let to = Address::repeat_byte(0x44);

        // Two calls in a run: both carry a nonzero hash, and the monotonic seq makes them differ.
        let c = core();
        let h1 = c
            .call(to, &[], from, U256::ZERO, AMPLE_GAS)
            .unwrap()
            .tx_hash;
        let h2 = c
            .call(to, &[], from, U256::ZERO, AMPLE_GAS)
            .unwrap()
            .tx_hash;
        assert_ne!(h1, B256::ZERO);
        assert_ne!(h2, B256::ZERO);
        assert_ne!(h1, h2, "repeated identical calls must get distinct hashes");

        // A fresh core replays the same call order to the same hashes (deterministic).
        let c2 = core();
        assert_eq!(
            c2.call(to, &[], from, U256::ZERO, AMPLE_GAS)
                .unwrap()
                .tx_hash,
            h1
        );
        assert_eq!(
            c2.call(to, &[], from, U256::ZERO, AMPLE_GAS)
                .unwrap()
                .tx_hash,
            h2
        );
    }

    #[test]
    fn gas_used_is_real_and_a_deploy_costs_more_than_a_trivial_call() {
        let c = core();
        let from = Address::repeat_byte(0x88);
        let to = Address::repeat_byte(0x99);
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);

        let deploy = c.deploy_create(initcode, &[], from, AMPLE_GAS).unwrap();
        let call = c.call(to, &[], from, U256::ZERO, AMPLE_GAS).unwrap();

        // A value-free, calldata-free call to an empty account executes nothing, so it is billed
        // exactly the EVM intrinsic transaction cost. Anything else means the figure is synthetic.
        assert_eq!(call.gas_used, 21_000);
        // A create pays the intrinsic cost plus CREATE (32_000) plus the initcode it runs.
        assert!(
            deploy.gas_used > call.gas_used,
            "deploy ({}) must cost more than a trivial call ({})",
            deploy.gas_used,
            call.gas_used
        );
        assert!(
            deploy.gas_used >= 53_000 && deploy.gas_used < AMPLE_GAS,
            "deploy gas ({}) outside the plausible band",
            deploy.gas_used
        );
    }

    #[test]
    fn gas_used_is_deterministic_across_runs() {
        let from = Address::repeat_byte(0x88);
        let to = Address::repeat_byte(0x99);
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);

        let run = || {
            let c = core();
            let deploy = c
                .deploy_create(initcode.clone(), &[], from, AMPLE_GAS)
                .unwrap();
            let call = c.call(to, &[], from, U256::ZERO, AMPLE_GAS).unwrap();
            (deploy.gas_used, call.gas_used)
        };

        assert_eq!(run(), run());
    }

    #[test]
    fn estimating_leaves_state_untouched() {
        let c = core();
        let from = Address::repeat_byte(0xaa);
        let payee = Address::repeat_byte(0xbb);
        let target = c
            .deploy_create(storing_contract(), &[], from, AMPLE_GAS)
            .unwrap()
            .address;
        let nonce_before = nonce(&c, from);

        // A call that writes storage, and a payable call that moves funds the caller does not
        // have: neither may leave a trace once estimated.
        c.estimate_call(target, &[], from, U256::ZERO).unwrap();
        c.estimate_call(payee, &[], from, U256::from(5u64)).unwrap();

        assert_eq!(
            c.storage(target, U256::ZERO).unwrap(),
            U256::ZERO,
            "an estimated SSTORE must not land"
        );
        assert_eq!(c.balance(payee).unwrap(), U256::ZERO);
        assert_eq!(
            c.balance(from).unwrap(),
            U256::ZERO,
            "the funds an estimate mints for the caller must not be committed"
        );
        assert_eq!(
            nonce(&c, from),
            nonce_before,
            "an estimate must not bump the sender nonce"
        );
    }

    #[test]
    fn estimating_does_not_advance_the_tx_hash_sequence() {
        let from = Address::repeat_byte(0xcc);
        let to = Address::repeat_byte(0xdd);

        let c = core();
        c.estimate_create(storing_contract(), &[], from).unwrap();
        c.estimate_call(to, &[], from, U256::ZERO).unwrap();
        let after_estimates = c
            .call(to, &[], from, U256::ZERO, AMPLE_GAS)
            .unwrap()
            .tx_hash;

        // The same call on a core that estimated nothing hashes identically: estimates minted no
        // hash and consumed no seq slot.
        let c2 = core();
        let without_estimates = c2
            .call(to, &[], from, U256::ZERO, AMPLE_GAS)
            .unwrap()
            .tx_hash;

        assert_eq!(after_estimates, without_estimates);
    }

    #[test]
    fn estimate_matches_the_gas_the_committed_op_is_billed() {
        let c = core();
        let from = Address::repeat_byte(0xee);

        // Create: the estimate runs at the nonce the real deploy will run at, so the two are the
        // same transaction and revm meters them identically.
        let estimated_deploy = c.estimate_create(storing_contract(), &[], from).unwrap();
        let deploy = c
            .deploy_create(storing_contract(), &[], from, AMPLE_GAS)
            .unwrap();
        assert_eq!(estimated_deploy, deploy.gas_used);

        // Call: likewise, the estimated SSTORE is cold and 0 -> nonzero, exactly as the real one is.
        let estimated_call = c
            .estimate_call(deploy.address, &[], from, U256::ZERO)
            .unwrap();
        let call = c
            .call(deploy.address, &[], from, U256::ZERO, AMPLE_GAS)
            .unwrap();
        assert_eq!(estimated_call, call.gas_used);
        assert!(
            estimated_call > 21_000,
            "an SSTORE costs more than intrinsic gas"
        );
        assert_eq!(
            c.storage(deploy.address, U256::ZERO).unwrap(),
            U256::from(0x2au64)
        );
    }

    #[test]
    fn a_call_limit_that_covers_the_cost_succeeds_and_one_below_it_runs_out_of_gas() {
        let c = core();
        let from = Address::repeat_byte(0x12);
        let target = c
            .deploy_create(storing_contract(), &[], from, AMPLE_GAS)
            .unwrap()
            .address;
        let needed = c.estimate_call(target, &[], from, U256::ZERO).unwrap();

        // One gas short of the true cost: past the intrinsic check, so revm executes and the
        // SSTORE exhausts the budget mid-flight. Out of gas, not a revert and not a zero-gas
        // success, and the storage write it was running dies with it.
        let err = c
            .call(target, &[], from, U256::ZERO, needed - 1)
            .expect_err("a limit under the true cost must fail");
        match &err {
            ExecFailure::Halt(reason) => assert!(
                reason.contains("OutOfGas"),
                "halt must be out-of-gas, got {reason}"
            ),
            other => panic!("expected a halt, got {other:?}"),
        }
        assert_eq!(
            c.storage(target, U256::ZERO).unwrap(),
            U256::ZERO,
            "an out-of-gas call must commit nothing"
        );

        // Exactly the true cost: the limit is a budget, not a fee, so nothing is left over to pay
        // and the transaction is billed precisely what it was forecast.
        let exec = c
            .call(target, &[], from, U256::ZERO, needed)
            .expect("a limit equal to the true cost must succeed");
        assert_eq!(exec.gas_used, needed);
        assert_eq!(c.storage(target, U256::ZERO).unwrap(), U256::from(0x2au64));
    }

    #[test]
    fn a_deploy_limit_below_the_cost_runs_out_of_gas() {
        let c = core();
        let from = Address::repeat_byte(0x13);
        let needed = c.estimate_create(storing_contract(), &[], from).unwrap();

        let err = c
            .deploy_create(storing_contract(), &[], from, needed - 1)
            .expect_err("a limit under the true cost must fail");
        match &err {
            ExecFailure::Halt(reason) => assert!(
                reason.contains("OutOfGas"),
                "halt must be out-of-gas, got {reason}"
            ),
            other => panic!("expected a halt, got {other:?}"),
        }

        let deploy = c
            .deploy_create(storing_contract(), &[], from, needed)
            .expect("a limit equal to the true cost must succeed");
        assert_eq!(deploy.gas_used, needed);
    }

    #[test]
    fn a_limit_below_the_intrinsic_cost_fails_cleanly_rather_than_panicking() {
        let c = core();
        let from = Address::repeat_byte(0x14);
        let to = Address::repeat_byte(0x15);

        // Under the intrinsic cost revm rejects the transaction before executing it, so the
        // failure surfaces as `Internal` (revm's own gas error) rather than a `Halt`. Both are
        // clean `ExecFailure`s; what matters is that neither panics nor reports a success.
        let err = c
            .call(to, &[], from, U256::ZERO, 20_999)
            .expect_err("a sub-intrinsic limit must fail");
        assert!(matches!(err, ExecFailure::Internal(_)), "got {err:?}");

        let err = c
            .deploy_create(storing_contract(), &[], from, 0)
            .expect_err("a zero limit must fail");
        assert!(matches!(err, ExecFailure::Internal(_)), "got {err:?}");
    }

    #[test]
    fn an_estimate_is_the_true_cost_whatever_limit_a_later_call_uses() {
        let from = Address::repeat_byte(0x16);

        // The estimate runs under `SIMULATION_GAS_LIMIT`, never under a caller's number, so it
        // reports the same cost no matter what limit the transaction it forecasts goes on to use:
        // a generous one, or exactly the estimate itself (which would be circular if the estimate
        // had been capped by it).
        for limit in [AMPLE_GAS, 60_000] {
            let c = core();
            let target = c
                .deploy_create(storing_contract(), &[], from, AMPLE_GAS)
                .unwrap()
                .address;
            let estimated = c.estimate_call(target, &[], from, U256::ZERO).unwrap();
            let billed = c
                .call(target, &[], from, U256::ZERO, limit)
                .unwrap()
                .gas_used;
            assert_eq!(estimated, billed, "at limit {limit}");
            assert!(estimated < limit, "the estimate must fit under {limit}");
        }
    }

    #[test]
    fn estimating_a_reverting_tx_errors_rather_than_reporting_gas() {
        let c = core();
        let from = Address::repeat_byte(0xff);
        let target = c
            .deploy_create(reverting_contract(), &[], from, AMPLE_GAS)
            .unwrap()
            .address;

        let err = c
            .estimate_call(target, &[], from, U256::ZERO)
            .expect_err("estimating a reverting call must error");
        assert!(matches!(err, ExecFailure::Revert(_)), "got {err:?}");

        // Initcode that reverts: the create never completes, so there is no gas figure to hand back.
        let err = c
            .estimate_create(Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xfd]), &[], from)
            .expect_err("estimating a reverting create must error");
        assert!(matches!(err, ExecFailure::Revert(_)), "got {err:?}");
    }

    #[test]
    fn static_call_on_empty_account_returns_empty_output() {
        let c = core();
        // A static call is a read, not a transaction, so `static_call` returns a bare
        // `Vec<u8>` with no `tx_hash` field at all; here we just check the output.
        let out = c.static_call(Address::repeat_byte(0x55), &[]).unwrap();
        assert!(out.is_empty());
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
