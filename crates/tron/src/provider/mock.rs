//! In-process Tron (TVM) provider backed by `revm`.
//!
//! [`TronMockProvider`] is a thin boundary over the shared [`RevmCore`] (the same core the EVM
//! mock builds on). Tron layers on top:
//!   * Addresses are the 0x41-prefixed [`TronAddress`]; the inner 20 bytes equal the EVM address,
//!     so the VM executes on [`TronAddress::as_evm`] while every surface shows the Tron form.
//!   * Balances are `u64` sun (1 TRX = 1_000_000 sun); the conversion to/from `revm`'s `U256`
//!     happens at this provider's boundary.
//!   * The TVM precompile set ([`tron_precompiles`]) replaces the stock Ethereum set: TIP-272
//!     relocations plus `validatemultisign`, injected via the core's construction hook. Source:
//!     <https://github.com/tronprotocol/tips/blob/master/tip-272.md>
//!   * tronc-emitted TRON-native opcodes (TRC-10 token guards + ISCONTRACT) are injected the same
//!     way, so tronc bytecode does not halt with OpcodeNotFound.
//!   * An energy/bandwidth [`ResourceTracker`] is held alongside the VM as a coarse,
//!     account-level accounting shim. Source:
//!     <https://developers.tron.network/docs/resource-model>

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use alloy_primitives::{Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use cross_vm_revm_common::{ExecFailure, RevmCore};
use revm::interpreter::Instruction;
use revm::precompile::Precompiles;

use crate::chains::TronChainInfo;
use crate::error::TronError;
use crate::provider::address::{address_from_label, TronAddress};
use crate::provider::execution::{TronCompute, TronDeploy, TronExecution, TronResources};
use crate::tvm::opcodes;
use crate::tvm::precompiles::tron_precompiles;
use crate::tvm::resources::{ResourceTracker, SUN_PER_TRX};

/// Default funding handed to accounts created via [`ChainProvider::new_account`]:
/// 10_000 TRX in sun.
pub const DEFAULT_FUNDING_SUN: u64 = 10_000 * SUN_PER_TRX;

/// The concrete in-memory `revm` instance used by the mock provider.
pub type TronInner = cross_vm_revm_common::RevmInner;

/// The TVM precompile set as a `'static` reference.
///
/// `revm`'s [`EthPrecompiles`](revm::handler::EthPrecompiles) holds a `&'static Precompiles`, so
/// the owned set from [`tron_precompiles`] is built once and cached here for injection into every
/// VM instance.
fn tron_precompiles_static() -> &'static Precompiles {
    static CELL: OnceLock<Precompiles> = OnceLock::new();
    CELL.get_or_init(tron_precompiles)
}

/// In-process Tron provider backed by `revm`.
///
/// The VM lives behind `Rc<RefCell<_>>` (inside [`RevmCore`]) so the handle is cheap to `clone`
/// and every clone shares one chain state. This lets a contract own its own handle while the test
/// still drives the same chain, and lets the contract operations run behind `&self`.
#[derive(Clone)]
pub struct TronMockProvider {
    core: RevmCore,
    info: TronChainInfo,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    /// Consumed by the `TronChain`/`WalletDeriver` wiring in a later phase (mirrors the EVM mock).
    #[allow(dead_code)]
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    /// Consumed by the `TronChain`/`WalletDeriver` wiring in a later phase (mirrors the EVM mock).
    #[allow(dead_code)]
    pub(crate) signers: Rc<RefCell<HashMap<String, PrivateKeySigner>>>,
    /// Energy/bandwidth accounting shim, shared across clones.
    pub(crate) resources: Rc<RefCell<ResourceTracker>>,
}

impl TronMockProvider {
    /// Build a fresh mock chain from a predefined [`TronChainInfo`].
    pub fn new(info: TronChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let core = RevmCore::new(info.numeric_id(), info.spec_id, |evm| {
            // Start at block 1 (a 0 marker is indistinguishable from "unset" in contracts that
            // record `pending[seq] = block.number`) and at the shared mock clock so cross-VM
            // packet timeouts compare correctly against the EVM, cosmos, and Solana chains.
            // `revm`'s `BlockEnv::default()` is `number: 0, timestamp: 1`, which would otherwise
            // put a TRON contract in 1970 while every other mock sits at `MOCK_BLOCK_TIMESTAMP`.
            evm.ctx.block.number = U256::from(1u64);
            evm.ctx.block.timestamp = U256::from(cross_vm_core::MOCK_BLOCK_TIMESTAMP);
            // Replace the stock Ethereum precompile set with the TVM set (TIP-272 relocations +
            // validatemultisign). The VM was built at `info.spec_id`, so `set_spec` will see an
            // unchanged spec on the first transaction and will NOT overwrite this injection.
            // Source: <https://github.com/tronprotocol/tips/blob/master/tip-272.md>
            evm.precompiles.precompiles = tron_precompiles_static();
            // tronc emits TRON-native opcodes (TRC-10 token ops + ISCONTRACT) that stock revm does
            // not decode, so tronc-compiled bytecode otherwise halts with OpcodeNotFound. Inject
            // minimal implementations. Like the precompile swap above, the spec is fixed, so the
            // per-tx `set_spec` sees no change and leaves these in place.
            evm.instruction.insert_instruction(
                opcodes::TOKENBALANCE,
                Instruction::new(opcodes::token_balance),
                opcodes::TVM_OPCODE_GAS,
            );
            evm.instruction.insert_instruction(
                opcodes::CALLTOKENVALUE,
                Instruction::new(opcodes::call_token_value),
                opcodes::TVM_OPCODE_GAS,
            );
            evm.instruction.insert_instruction(
                opcodes::CALLTOKENID,
                Instruction::new(opcodes::call_token_id),
                opcodes::TVM_OPCODE_GAS,
            );
            evm.instruction.insert_instruction(
                opcodes::ISCONTRACT,
                Instruction::new(opcodes::is_contract),
                opcodes::TVM_OPCODE_GAS,
            );
        });
        Self {
            core,
            info,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
            resources: Rc::new(RefCell::new(ResourceTracker::new())),
        }
    }

    /// Deploy bytecode via a create transaction, appending constructor args to the initcode.
    ///
    /// DIVERGENCE(tron): the mock returns `revm`'s EVM-derived CREATE address (wrapped as a
    /// [`TronAddress`]); real Tron derives the address from the transaction id and a per-root-call
    /// nonce via [`tron_create_address`](crate::tvm::tron_create_address). `revm` computes the
    /// CREATE address inside its frame handler and exposes only the finished address in
    /// `Output::Create`, so overriding it cleanly is not possible on the pinned revm 41 API
    /// without forking the handler. Source: <https://github.com/tronprotocol/tips/issues/26>
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        from: &TronAddress,
    ) -> Result<TronDeploy, TronError> {
        let args = constructor_args.as_ref();
        let bandwidth = self.charge_bandwidth(from, bytecode.len() + args.len());
        self.core
            .deploy_create(bytecode, args, from.as_evm())
            .map(|d| mock_deploy(d, bandwidth))
            .map_err(|f| TronError::Deploy(f.deploy_message()))
    }

    /// Execute a state-mutating call against `to`, returning its output plus emitted logs.
    pub async fn call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
    ) -> Result<TronExecution, TronError> {
        self.call_value(to, calldata, from, U256::ZERO).await
    }

    /// Execute a state-mutating call against `to` carrying `value` sun (a payable call), returning
    /// its output plus emitted logs. The caller's balance is topped up to cover `value` first (the
    /// mock mints native funds on demand, like [`ChainProvider::new_account`]). `value` is sun
    /// stored 1:1 as revm's `U256`, matching [`ChainProvider::set_balance`].
    pub async fn call_value(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
        value: U256,
    ) -> Result<TronExecution, TronError> {
        // Coarse bandwidth accounting: charge the caller by encoded calldata length. The mock does
        // not gate execution on the outcome, it only reports what was deducted.
        let bandwidth = self.charge_bandwidth(from, calldata.as_ref().len());
        self.core
            .call(to.as_evm(), calldata.as_ref(), from.as_evm(), value)
            .map(|e| mock_execution(e, bandwidth))
            .map_err(|f| TronError::Execute(f.call_message("call")))
    }

    /// Transfer `amount` sun of the native token from `from` to `to`, returning the transaction
    /// hash as unprefixed hex (the shape java-tron renders a `txID` in).
    ///
    /// A native transfer is a value-carrying call with empty calldata, so it runs through the same
    /// [`RevmCore`] path as [`call_value`](Self::call_value) and carries the core's synthetic tx
    /// hash. Unlike `call_value` it does NOT mint on demand: the core tops the caller up to cover
    /// the value, so the sender's balance is checked first and a shortfall errors, as a live chain
    /// would reject it. `amount` is sun, stored 1:1 as revm's `U256` at the boundary.
    pub async fn transfer_funds(
        &self,
        to: &TronAddress,
        amount: u64,
        from: &TronAddress,
    ) -> Result<String, TronError> {
        let current = self.balance(from).await?;
        if current < amount {
            return Err(TronError::Balance(format!(
                "insufficient balance: {from} holds {current} sun, needs {amount}"
            )));
        }
        self.core
            .call(to.as_evm(), &[], from.as_evm(), U256::from(amount))
            .map(|e| hex::encode(e.tx_hash))
            .map_err(|f| TronError::Execute(f.call_message("transfer_funds")))
    }

    /// Run a read-only static call against `to`.
    pub async fn static_call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, TronError> {
        self.core
            .static_call(to.as_evm(), calldata.as_ref())
            .map_err(|f| match f {
                // A transact error is a query-infra failure; a revert/halt is an execution error,
                // exactly as before the RevmCore extraction.
                ExecFailure::Internal(s) => TronError::Query(s),
                other => TronError::Execute(other.call_message("static_call")),
            })
    }

    /// Read the raw storage value at `slot` for `addr`.
    pub async fn get_storage_at(&self, addr: &TronAddress, slot: U256) -> Result<U256, TronError> {
        self.core
            .storage(addr.as_evm(), slot)
            .map_err(|f| TronError::Query(f.call_message("get_storage_at")))
    }

    /// Freeze `trx_sun` sun of TRX for `who`, granting energy.
    /// Source: <https://developers.tron.network/docs/resource-model>
    pub fn freeze_for_energy(&self, who: &TronAddress, trx_sun: u64) {
        self.resources.borrow_mut().freeze_for_energy(*who, trx_sun);
    }

    /// Current energy units available to `who`.
    pub fn energy(&self, who: &TronAddress) -> u64 {
        self.resources.borrow().energy(who)
    }

    /// Remaining free bandwidth for `who`.
    pub fn bandwidth(&self, who: &TronAddress) -> u64 {
        self.resources.borrow().bandwidth(who)
    }

    /// Charge `payload_bytes` of bandwidth to `from` through the [resource shim], returning the
    /// points it actually deducted (what the operation then reports as consumed).
    ///
    /// Over the free allowance the shim deducts nothing (it models the burn-for-fee fallback as
    /// free), which is also the shape a live receipt takes: a transaction that burns TRX for its
    /// bytes carries `net_usage: 0` and a `net_fee` instead.
    ///
    /// [resource shim]: crate::tvm::resources
    /// Source: <https://developers.tron.network/docs/resource-model>
    fn charge_bandwidth(&self, from: &TronAddress, payload_bytes: usize) -> u64 {
        let deducted = self
            .resources
            .borrow_mut()
            .consume_bandwidth(from, payload_bytes);
        if deducted {
            payload_bytes as u64
        } else {
            0
        }
    }
}

/// The mock's compute figure is `revm`'s EVM gas, reported as [`TronCompute::Gas`], never as
/// energy: the mock is `revm`, so gas is the quantity it genuinely meters, while the energy shim
/// sits outside `revm`'s gas loop and is never touched by execution. `fee` is `None` for the same
/// reason: a Tron fee is priced off energy, which nothing here metered.
///
/// This is also the one place a `revm` `B256` becomes a Tron transaction hash: the mock has no real
/// broadcast hash, so the core mints a synthetic, deterministic one, and it is rendered here into
/// the unprefixed hex a java-tron `txID` is spoken in. Every other Tron surface already holds the
/// hash as that `String`, so nothing round-trips.
fn mock_execution(e: cross_vm_revm_common::Execution, bandwidth: u64) -> TronExecution {
    TronExecution {
        output: e.output,
        logs: e.logs,
        tx_hash: hex::encode(e.tx_hash),
        resources: TronResources {
            compute: TronCompute::Gas(e.gas_used),
            bandwidth,
            fee: None,
        },
    }
}

/// The create-transaction counterpart of [`mock_execution`]; the same reporting rules apply.
fn mock_deploy(d: cross_vm_revm_common::Deployment, bandwidth: u64) -> TronDeploy {
    TronDeploy {
        address: TronAddress::from_evm(d.address),
        tx_hash: hex::encode(d.tx_hash),
        resources: TronResources {
            compute: TronCompute::Gas(d.gas_used),
            bandwidth,
            fee: None,
        },
    }
}

impl ChainProvider for TronMockProvider {
    type Spec = TronChainInfo;
    type Address = TronAddress;
    type Account = TronAddress;
    type Balance = u64;
    type Error = TronError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, label: &str) -> TronAddress {
        let addr = address_from_label(label);
        let denom = self.chain_info().native_symbol;
        let _ = self.set_balance(&addr, denom, DEFAULT_FUNDING_SUN).await;
        addr
    }

    async fn balance(&self, addr: &TronAddress) -> Result<u64, TronError> {
        // Convert revm's U256 wei-shaped balance back to u64 sun at the boundary.
        self.core
            .balance(addr.as_evm())
            .map(|b| b.saturating_to::<u64>())
            .map_err(|f| TronError::Balance(f.call_message("balance")))
    }

    async fn set_balance(
        &mut self,
        addr: &TronAddress,
        denom: &str,
        amount: u64,
    ) -> Result<(), TronError> {
        let symbol = self.chain_info().native_symbol;
        if !denom.eq_ignore_ascii_case(symbol) {
            return Err(TronError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{symbol}'"
            )));
        }
        // Store the u64 sun balance as revm's U256 at the boundary.
        self.core.set_balance(addr.as_evm(), U256::from(amount));
        Ok(())
    }

    async fn block_height(&self) -> u64 {
        self.core.block_height()
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        self.core.advance_blocks(n, time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chains::LOCAL;
    use crate::tvm::resources::FREE_BANDWIDTH_PER_DAY;
    use cross_vm_core::{ChainProvider, WalletFactory};
    use std::rc::Rc;

    fn provider() -> TronMockProvider {
        TronMockProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()))
    }

    #[tokio::test]
    async fn new_account_is_funded_and_tron_shaped() {
        let mut c = provider();
        let a = c.new_account("alice").await;
        assert!(a.to_base58().starts_with('T'));
        assert!(c.balance(&a).await.unwrap() > 0);
    }

    #[tokio::test]
    async fn set_and_read_balance() {
        let mut c = provider();
        let a = c.new_account("alice").await;
        c.set_balance(&a, "TRX", 42 * SUN_PER_TRX).await.unwrap();
        assert_eq!(c.balance(&a).await.unwrap(), 42 * SUN_PER_TRX);
    }

    #[tokio::test]
    async fn set_balance_validates_denom() {
        let mut c = provider();
        let a = c.new_account("alice").await;

        assert!(c.set_balance(&a, "BTC", 1).await.is_err());

        c.set_balance(&a, "trx", 7 * SUN_PER_TRX).await.unwrap();
        assert_eq!(c.balance(&a).await.unwrap(), 7 * SUN_PER_TRX);
    }

    #[tokio::test]
    async fn freeze_grants_energy() {
        let mut c = provider();
        let a = c.new_account("alice").await;
        assert_eq!(c.energy(&a), 0);
        c.freeze_for_energy(&a, SUN_PER_TRX);
        assert!(c.energy(&a) > 0);
    }

    #[tokio::test]
    async fn deploy_create_returns_tron_address_and_tx_hash() {
        // Minimal initcode that deploys an empty runtime: PUSH1 0x00, PUSH1 0x00, RETURN.
        // It returns a zero-length runtime, so the deploy succeeds and yields a contract address.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let d = c
            .deploy_create(initcode.clone(), [], &deployer)
            .await
            .expect("empty-runtime deploy succeeds");
        assert!(d.address.to_base58().starts_with('T'));
        // The core's synthetic hash in the RPC arm's shape: a 32-byte hash as unprefixed hex.
        assert_eq!(d.tx_hash.len(), 64);
        assert!(d.tx_hash.chars().all(|c| c.is_ascii_hexdigit()));
        // The deployed (empty) account has no balance.
        assert_eq!(c.balance(&d.address).await.unwrap(), 0);

        // A repeat of the identical deploy is a distinct transaction, so it carries a distinct hash.
        let again = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("empty-runtime deploy succeeds");
        assert_ne!(d.tx_hash, again.tx_hash);
    }

    #[tokio::test]
    async fn call_reports_a_tx_hash() {
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let addr = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;

        let first = c.call(&addr, [], &deployer).await.expect("call succeeds");
        let second = c.call(&addr, [], &deployer).await.expect("call succeeds");
        // A 32-byte hash as unprefixed hex, the shape java-tron renders a `txID` in.
        assert_eq!(first.tx_hash.len(), 64);
        assert!(first.tx_hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(
            first.tx_hash, second.tx_hash,
            "repeated identical calls are distinct transactions"
        );
    }

    #[tokio::test]
    async fn mock_reports_revm_gas_and_never_energy() {
        // The crux of the mock's honesty: it IS revm, so it meters EVM gas, and Tron energy is not
        // EVM gas. Freezing TRX grants energy that contract execution never touches (the shim sits
        // outside revm's gas loop), so the mock has no energy figure and must not relabel gas as
        // one.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        c.freeze_for_energy(&deployer, SUN_PER_TRX);
        let energy_before = c.energy(&deployer);

        let d = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("empty-runtime deploy succeeds");
        let e = c
            .call(&d.address, [], &deployer)
            .await
            .expect("call succeeds");

        for compute in [d.resources.compute, e.resources.compute] {
            let TronCompute::Gas(gas) = compute else {
                panic!("the mock meters EVM gas, not Tron energy: got {compute:?}");
            };
            assert!(
                gas > 0,
                "revm bills every transaction at least the tx floor"
            );
        }
        // A create costs strictly more than a bare call to an empty runtime.
        let (TronCompute::Gas(deploy_gas), TronCompute::Gas(call_gas)) =
            (d.resources.compute, e.resources.compute)
        else {
            unreachable!("asserted above")
        };
        assert!(deploy_gas > call_gas, "{deploy_gas} !> {call_gas}");

        // The shim's energy is untouched by execution, which is exactly why the gas figure above
        // cannot be reported as energy.
        assert_eq!(c.energy(&deployer), energy_before);
        // No fee: a Tron fee is priced off energy, and nothing here metered energy.
        assert_eq!(d.resources.fee, None);
        assert_eq!(e.resources.fee, None);
    }

    #[tokio::test]
    async fn mock_reports_the_bandwidth_the_shim_charged() {
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;

        // The deploy is charged for its payload (initcode + constructor args).
        let d = c
            .deploy_create(initcode, [0xaa, 0xbb], &deployer)
            .await
            .expect("empty-runtime deploy succeeds");
        assert_eq!(d.resources.bandwidth, 7);
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 7);

        // A call is charged by calldata length, and the reported figure is the deduction.
        let e = c
            .call(&d.address, [0x01, 0x02, 0x03], &deployer)
            .await
            .expect("call succeeds");
        assert_eq!(e.resources.bandwidth, 3);
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 10);

        // Beyond the free allowance the shim deducts nothing (it models the burn-for-fee fallback
        // as free), so nothing is reported as consumed: a live receipt that burns TRX for its bytes
        // likewise carries `net_usage: 0`.
        let big = vec![0u8; FREE_BANDWIDTH_PER_DAY as usize];
        let e = c
            .call(&d.address, big, &deployer)
            .await
            .expect("call succeeds");
        assert_eq!(e.resources.bandwidth, 0);
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 10);
    }

    #[tokio::test]
    async fn tvm_token_opcodes_are_decoded() {
        // Stock revm halts with OpcodeNotFound on TRON's token opcodes; the mock injects them.
        // Exercise all four, checking arity, then deploy an empty runtime:
        //   CALLTOKENID (0xd3) -> [0]; CALLTOKENVALUE (0xd2) -> [0, 0];
        //   TOKENBALANCE (0xd1) pops 2, pushes 0 -> [0]; ISCONTRACT (0xd4) pops 1, pushes 0 -> [0];
        //   PUSH1 0 -> [0, 0]; RETURN pops (offset, len) = (0, 0) -> empty runtime.
        let initcode = Bytes::from(vec![0xd3, 0xd2, 0xd1, 0xd4, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let d = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("tronc token-guard opcodes decode and deploy succeeds");
        assert!(d.address.to_base58().starts_with('T'));
    }

    #[tokio::test]
    async fn call_value_transfers_native_to_callee() {
        // Deploy an empty-runtime account, then send a payable call carrying value. The mock mints
        // the caller's funds on demand, and revm credits the value (in sun, stored 1:1 as U256) to
        // the callee. Prove the callee's balance rose by exactly the value sent.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let addr = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;
        assert_eq!(c.balance(&addr).await.unwrap(), 0);

        let value = 3 * SUN_PER_TRX;
        c.call_value(&addr, [], &deployer, U256::from(value))
            .await
            .expect("payable call succeeds");
        assert_eq!(c.balance(&addr).await.unwrap(), value);
    }

    #[tokio::test]
    async fn plain_call_sends_zero_value() {
        // The value-less `call` must leave the callee's balance untouched.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let addr = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;

        c.call(&addr, [], &deployer)
            .await
            .expect("value-less call succeeds");
        assert_eq!(c.balance(&addr).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn reads_storage_slot_written_by_constructor() {
        // Initcode whose constructor writes 42 into storage slot 0, then returns an empty runtime:
        //   PUSH1 0x2a, PUSH1 0x00, SSTORE, PUSH1 0x00, PUSH1 0x00, RETURN.
        let initcode = Bytes::from(vec![
            0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x60, 0x00, 0xf3,
        ]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let addr = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("storage-writing deploy succeeds")
            .address;
        // The constructor wrote 42 at slot 0; an untouched slot reads as zero.
        assert_eq!(
            c.get_storage_at(&addr, U256::ZERO).await.unwrap(),
            U256::from(42u64)
        );
        assert_eq!(
            c.get_storage_at(&addr, U256::from(1u64)).await.unwrap(),
            U256::ZERO
        );
    }

    #[tokio::test]
    async fn advance_blocks_moves_height() {
        let mut c = provider();
        let start = c.block_height().await;
        c.advance_blocks(5, BlockTime::Increment(1)).await;
        assert_eq!(c.block_height().await, start + 5);
    }

    #[tokio::test]
    async fn starts_on_the_shared_mock_clock() {
        let c = provider();
        // Not `revm`'s `BlockEnv::default()` of `number: 0, timestamp: 1`: the mock has to agree
        // with the EVM, cosmos, and Solana chains so cross-VM timeouts compare correctly.
        assert_eq!(c.block_height().await, 1);
        assert_eq!(
            c.core.block_timestamp(),
            cross_vm_core::MOCK_BLOCK_TIMESTAMP
        );
    }

    #[tokio::test]
    async fn chain_id_comes_from_the_preset() {
        assert_eq!(provider().core.chain_id(), LOCAL.numeric_id());
    }
}
