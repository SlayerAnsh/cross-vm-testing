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
use crate::provider::execution::{
    with_headroom, TronCompute, TronDeploy, TronEnergyPolicy, TronExecution, TronLimit,
    TronResources,
};
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

    /// Deploy bytecode via a create transaction, appending constructor args to the initcode, under
    /// the EVM gas budget `limit` resolves to (see [`TronLimit`]).
    ///
    /// `energy_policy` is accepted and ignored: it configures how a live java-tron chain
    /// apportions a future caller's energy between that caller and the contract's owner, and
    /// `revm` bills one payer and meters no energy at all. The parameter is required all the same,
    /// so a deploy states the policy it will carry on the chain it is destined for rather than
    /// inheriting one silently.
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
        limit: TronLimit,
        energy_policy: TronEnergyPolicy,
    ) -> Result<TronDeploy, TronError> {
        let _ = energy_policy;
        let args = constructor_args.as_ref();
        let gas_limit = match limit {
            TronLimit::Gas(gas) => gas,
            TronLimit::Fee(sun) => return Err(TronError::Deploy(fee_limit_on_the_mock(sun))),
            TronLimit::Estimated => self
                .core
                .estimate_create(bytecode.clone(), args, from.as_evm())
                .map(|gas| with_headroom(gas, self.info.gas_adjustment))
                .map_err(|f| TronError::Deploy(f.deploy_message()))?,
        };
        let bandwidth = self.charge_bandwidth(from, bytecode.len() + args.len());
        self.core
            .deploy_create(bytecode, args, from.as_evm(), gas_limit)
            .map(|d| mock_deploy(d, bandwidth))
            .map_err(|f| TronError::Deploy(f.deploy_message()))
    }

    /// Forecast what a [`deploy_create`](Self::deploy_create) of this bytecode would consume,
    /// without deploying it.
    ///
    /// The compute figure is EVM gas ([`TronCompute::Gas`]), because that is what the backing
    /// `revm` meters; see [`mock_resources`]. A reverting constructor is an error, not a resource
    /// figure.
    pub async fn estimate_deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        from: &TronAddress,
    ) -> Result<TronResources, TronError> {
        let args = constructor_args.as_ref();
        let bandwidth = self.forecast_bandwidth(from, bytecode.len() + args.len());
        self.core
            .estimate_create(bytecode, args, from.as_evm())
            .map(|gas| mock_resources(gas, bandwidth))
            .map_err(|f| TronError::Deploy(f.deploy_message()))
    }

    /// Forecast what a [`call`](Self::call) with these arguments would consume, without running it.
    pub async fn estimate_call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
    ) -> Result<TronResources, TronError> {
        self.estimate_call_value(to, calldata, from, U256::ZERO)
            .await
    }

    /// Forecast what a [`call_value`](Self::call_value) with these arguments would consume, without
    /// running it. As on the executing path, a caller that cannot cover `value` is topped up first,
    /// so an estimate does not fail where the call it forecasts would succeed; the top-up is
    /// journaled and discarded with the rest of the simulated state.
    pub async fn estimate_call_value(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
        value: U256,
    ) -> Result<TronResources, TronError> {
        let bandwidth = self.forecast_bandwidth(from, calldata.as_ref().len());
        self.core
            .estimate_call(to.as_evm(), calldata.as_ref(), from.as_evm(), value)
            .map(|gas| mock_resources(gas, bandwidth))
            .map_err(|f| TronError::Execute(f.call_message("estimate_call")))
    }

    /// Execute a state-mutating call against `to` under the EVM gas budget `limit` resolves to
    /// (see [`TronLimit`]), returning its output plus emitted logs.
    pub async fn call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
        limit: TronLimit,
    ) -> Result<TronExecution, TronError> {
        self.call_value(to, calldata, from, U256::ZERO, limit).await
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
        limit: TronLimit,
    ) -> Result<TronExecution, TronError> {
        let gas_limit = match limit {
            TronLimit::Gas(gas) => gas,
            TronLimit::Fee(sun) => return Err(TronError::Execute(fee_limit_on_the_mock(sun))),
            TronLimit::Estimated => self
                .core
                .estimate_call(to.as_evm(), calldata.as_ref(), from.as_evm(), value)
                .map(|gas| with_headroom(gas, self.info.gas_adjustment))
                .map_err(|f| TronError::Execute(f.call_message("estimate_call")))?,
        };
        // Coarse bandwidth accounting: charge the caller by encoded calldata length. The mock does
        // not gate execution on the outcome, it only reports what was deducted.
        let bandwidth = self.charge_bandwidth(from, calldata.as_ref().len());
        self.core
            .call(
                to.as_evm(),
                calldata.as_ref(),
                from.as_evm(),
                value,
                gas_limit,
            )
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
    ///
    /// It takes no [`TronLimit`], because java-tron's `TransferContract` has no `fee_limit` field
    /// to take: a native transfer runs no code, burns no energy, and is billed only in bandwidth,
    /// which the sender cannot cap. `revm` still needs a budget, so the mock measures the one this
    /// transfer needs (it is the flat intrinsic transaction cost, and the estimate runs against
    /// the state the transfer then runs against, so it is exact and needs no headroom).
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
        let value = U256::from(amount);
        let gas_limit = self
            .core
            .estimate_call(to.as_evm(), &[], from.as_evm(), value)
            .map_err(|f| TronError::Execute(f.call_message("transfer_funds")))?;
        self.core
            .call(to.as_evm(), &[], from.as_evm(), value, gas_limit)
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

    /// Read the deployed runtime bytecode at `addr` (empty for an ordinary account or an undeployed
    /// address). The TVM executes on the inner EVM address ([`TronAddress::as_evm`]), so this reads
    /// the code the backing `revm` holds there.
    pub async fn get_code(&self, addr: &TronAddress) -> Result<Bytes, TronError> {
        self.core
            .code(addr.as_evm())
            .map_err(|f| TronError::Query(f.call_message("get_code")))
    }

    /// Generic JSON-RPC escape hatch. Unsupported on the in-process mock: there is no node to
    /// answer an arbitrary method, so the call names one that does not exist here.
    pub async fn raw_request(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, TronError> {
        Err(TronError::Unimplemented(format!(
            "mock raw_request '{method}'"
        )))
    }

    /// Generic java-tron REST escape hatch. Unsupported on the mock for the same reason as
    /// [`raw_request`](Self::raw_request): there is no `/wallet/*` node to POST to.
    pub async fn wallet_request(
        &self,
        path: &str,
        _body: serde_json::Value,
    ) -> Result<serde_json::Value, TronError> {
        Err(TronError::Unimplemented(format!(
            "mock wallet_request '{path}'"
        )))
    }

    /// Sign an unsigned transaction's `txID`. Unsupported on the mock, which executes in-process and
    /// signs no real transaction (see the synthetic-hash note on [`mock_execution`]).
    pub async fn sign_transaction(
        &self,
        _unsigned: serde_json::Value,
    ) -> Result<serde_json::Value, TronError> {
        Err(TronError::Unimplemented("mock sign_transaction".into()))
    }

    /// Broadcast a signed transaction. Unsupported on the mock, which has no node to broadcast to.
    pub async fn broadcast_transaction(
        &self,
        _signed: serde_json::Value,
    ) -> Result<String, TronError> {
        Err(TronError::Unimplemented(
            "mock broadcast_transaction".into(),
        ))
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

    /// The points [`charge_bandwidth`](Self::charge_bandwidth) *would* deduct for a
    /// `payload_bytes`-long transaction from `from`, deducting nothing: an estimate must not mutate
    /// the chain, and the shim's allowance is chain state. The shim charges by payload length
    /// alone, so this is exact without executing anything.
    fn forecast_bandwidth(&self, from: &TronAddress, payload_bytes: usize) -> u64 {
        let needed = payload_bytes as u64;
        if needed <= self.resources.borrow().bandwidth(from) {
            needed
        } else {
            0
        }
    }
}

/// Why a sun fee cap cannot bound a mock transaction.
///
/// A `fee_limit` is an energy ceiling denominated in sun: java-tron divides it by the energy price
/// to get the energy the transaction may burn. The mock is `revm`, which meters EVM gas
/// (see [`TronCompute`]), has no energy, and has no price at which to buy any. Honoring the number
/// would mean inventing both, so the cap is rejected rather than silently ignored or converted.
fn fee_limit_on_the_mock(sun: u64) -> String {
    format!(
        "a fee limit of {sun} sun cannot bound a mock transaction: the mock is revm, which budgets \
         in EVM gas and meters no energy to price into sun; use TronLimit::Gas or \
         TronLimit::Estimated"
    )
}

/// The resources a mock operation reports, whether executed or merely forecast.
///
/// The compute figure is `revm`'s EVM gas, reported as [`TronCompute::Gas`], never as energy: the
/// mock is `revm`, so gas is the quantity it genuinely meters, while the energy shim sits outside
/// `revm`'s gas loop and is never touched by execution. `fee` is `None` for the same reason: a Tron
/// fee is priced off energy, which nothing here metered.
fn mock_resources(gas: u64, bandwidth: u64) -> TronResources {
    TronResources {
        compute: TronCompute::Gas(gas),
        bandwidth,
        fee: None,
    }
}

/// This is the one place a `revm` `B256` becomes a Tron transaction hash: the mock has no real
/// broadcast hash, so the core mints a synthetic, deterministic one, and it is rendered here into
/// the unprefixed hex a java-tron `txID` is spoken in. Every other Tron surface already holds the
/// hash as that `String`, so nothing round-trips.
fn mock_execution(e: cross_vm_revm_common::Execution, bandwidth: u64) -> TronExecution {
    TronExecution {
        output: e.output,
        logs: e.logs,
        tx_hash: hex::encode(e.tx_hash),
        resources: mock_resources(e.gas_used, bandwidth),
    }
}

/// The create-transaction counterpart of [`mock_execution`]; the same reporting rules apply.
fn mock_deploy(d: cross_vm_revm_common::Deployment, bandwidth: u64) -> TronDeploy {
    TronDeploy {
        address: TronAddress::from_evm(d.address),
        tx_hash: hex::encode(d.tx_hash),
        resources: mock_resources(d.gas_used, bandwidth),
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

    /// A budget generous enough for every transaction below, so a test that is not about the limit
    /// does not accidentally become one.
    const AMPLE: TronLimit = TronLimit::Gas(30_000_000);

    /// The caller pays all of a call's energy, so the contract owner's ceiling never binds. None of
    /// the deploys below is about how the deployed contract bills its future callers.
    const CALLER_PAYS: TronEnergyPolicy = TronEnergyPolicy {
        consume_user_resource_percent: 100,
        origin_energy_limit: 0,
    };

    fn provider() -> TronMockProvider {
        TronMockProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()))
    }

    /// A mock chain whose forecasts are scaled by `gas_adjustment`, to prove the chain's number is
    /// what sizes an [`TronLimit::Estimated`] limit.
    fn provider_with_adjustment(gas_adjustment: f64) -> TronMockProvider {
        let info = TronChainInfo {
            gas_adjustment,
            ..LOCAL
        };
        TronMockProvider::new(info, Rc::new(WalletFactory::from_roster(&[]).unwrap()))
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
            .deploy_create(initcode.clone(), [], &deployer, AMPLE, CALLER_PAYS)
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
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
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
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;

        let first = c
            .call(&addr, [], &deployer, AMPLE)
            .await
            .expect("call succeeds");
        let second = c
            .call(&addr, [], &deployer, AMPLE)
            .await
            .expect("call succeeds");
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
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds");
        let e = c
            .call(&d.address, [], &deployer, AMPLE)
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
            .deploy_create(initcode, [0xaa, 0xbb], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds");
        assert_eq!(d.resources.bandwidth, 7);
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 7);

        // A call is charged by calldata length, and the reported figure is the deduction.
        let e = c
            .call(&d.address, [0x01, 0x02, 0x03], &deployer, AMPLE)
            .await
            .expect("call succeeds");
        assert_eq!(e.resources.bandwidth, 3);
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 10);

        // Beyond the free allowance the shim deducts nothing (it models the burn-for-fee fallback
        // as free), so nothing is reported as consumed: a live receipt that burns TRX for its bytes
        // likewise carries `net_usage: 0`.
        let big = vec![0u8; FREE_BANDWIDTH_PER_DAY as usize];
        let e = c
            .call(&d.address, big, &deployer, AMPLE)
            .await
            .expect("call succeeds");
        assert_eq!(e.resources.bandwidth, 0);
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 10);
    }

    /// Initcode returning `runtime` (a CODECOPY of the bytes trailing this 12-byte prologue).
    fn initcode_returning(runtime: &[u8]) -> Bytes {
        let n = runtime.len() as u8;
        let mut code = vec![
            0x60, n, 0x60, 0x0c, 0x60, 0x00, 0x39, 0x60, n, 0x60, 0x00, 0xf3,
        ];
        code.extend_from_slice(runtime);
        Bytes::from(code)
    }

    #[tokio::test]
    async fn estimate_reports_revm_gas_and_never_energy() {
        // The same crux as the executed path: a forecast from a revm-backed mock is a forecast of
        // EVM gas. Relabelling it energy would misstate the quantity, so the estimate reports the
        // unit it measured, exactly as a receipt does.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        c.freeze_for_energy(&deployer, SUN_PER_TRX);

        let deploy = c
            .estimate_deploy_create(initcode.clone(), [], &deployer)
            .await
            .expect("empty-runtime deploy estimates");
        let addr = c
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;
        let call = c
            .estimate_call(&addr, [], &deployer)
            .await
            .expect("call estimates");

        for r in [deploy, call] {
            let TronCompute::Gas(gas) = r.compute else {
                panic!("the mock forecasts EVM gas, not Tron energy: got {r:?}");
            };
            assert!(
                gas > 0,
                "revm bills every transaction at least the tx floor"
            );
            // A Tron fee is priced off energy, which nothing here metered, so there is none to
            // forecast either.
            assert_eq!(r.fee, None);
        }
    }

    #[tokio::test]
    async fn estimate_matches_the_resources_the_op_then_reports() {
        // The point of reusing `TronResources` for a forecast: it is comparable to the receipt.
        // The estimate commits nothing, so the op it forecasts runs against the very state the
        // estimate measured, and the two figures agree exactly.
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        // Fallback writes 0x2a to slot 0: PUSH1 0x2a, PUSH1 0x00, SSTORE, STOP.
        let initcode = initcode_returning(&[0x60, 0x2a, 0x60, 0x00, 0x55, 0x00]);

        let estimated = c
            .estimate_deploy_create(initcode.clone(), [], &deployer)
            .await
            .expect("deploy estimates");
        let deploy = c
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("deploy succeeds");
        assert_eq!(estimated, deploy.resources);

        let estimated = c
            .estimate_call(&deploy.address, [0x01, 0x02, 0x03], &deployer)
            .await
            .expect("call estimates");
        let exec = c
            .call(&deploy.address, [0x01, 0x02, 0x03], &deployer, AMPLE)
            .await
            .expect("call succeeds");
        assert_eq!(estimated, exec.resources);
        assert_eq!(
            estimated.bandwidth, 3,
            "the shim charges by calldata length"
        );
    }

    #[tokio::test]
    async fn estimating_does_not_deduct_bandwidth() {
        // An estimate forecasts the shim's charge; it must not levy it. The shim's allowance is
        // chain state, and a forecast that spends the thing it is forecasting is a transaction.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;

        let deploy = c
            .estimate_deploy_create(initcode.clone(), [0xaa, 0xbb], &deployer)
            .await
            .expect("deploy estimates");
        assert_eq!(deploy.bandwidth, 7, "initcode (5) + constructor args (2)");
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY);

        let addr = c
            .deploy_create(initcode, [0xaa, 0xbb], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 7);

        // Repeated estimates keep forecasting the same figure, because none of them spends it.
        for _ in 0..3 {
            let e = c
                .estimate_call(&addr, [0x01, 0x02, 0x03], &deployer)
                .await
                .expect("call estimates");
            assert_eq!(e.bandwidth, 3);
            assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 7);
        }

        // Past the free allowance the shim would deduct nothing (it models the burn-for-fee
        // fallback as free), and the forecast says so rather than inventing a charge.
        let big = vec![0u8; FREE_BANDWIDTH_PER_DAY as usize];
        let e = c
            .estimate_call(&addr, big, &deployer)
            .await
            .expect("call estimates");
        assert_eq!(e.bandwidth, 0);
        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY - 7);
    }

    #[tokio::test]
    async fn estimating_a_reverting_tx_errors_rather_than_reporting_resources() {
        // A caller told "42_000 gas" for a transaction that cannot succeed has been misinformed.
        let mut c = provider();
        let deployer = c.new_account("deployer").await;

        // Initcode that reverts: the create never completes, so there is no figure to hand back.
        let err = c
            .estimate_deploy_create(
                Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xfd]),
                [],
                &deployer,
            )
            .await
            .expect_err("estimating a reverting create must error");
        assert!(matches!(err, TronError::Deploy(_)), "got {err:?}");

        // A contract whose fallback reverts with empty data.
        let target = c
            .deploy_create(
                initcode_returning(&[0x60, 0x00, 0x60, 0x00, 0xfd]),
                [],
                &deployer,
                AMPLE,
                CALLER_PAYS,
            )
            .await
            .expect("deploy succeeds")
            .address;
        let err = c
            .estimate_call(&target, [], &deployer)
            .await
            .expect_err("estimating a reverting call must error");
        assert!(matches!(err, TronError::Execute(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn estimating_a_payable_call_tops_the_caller_up_without_committing_it() {
        // `call_value` mints the caller the funds it lacks, so its estimate must too, or the
        // forecast would fail where the call succeeds. The top-up must not survive the estimate.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let addr = c
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;
        let pauper = c.new_account("pauper").await;
        c.set_balance(&pauper, "TRX", 0).await.unwrap();

        let value = U256::from(5 * SUN_PER_TRX);
        c.estimate_call_value(&addr, [], &pauper, value)
            .await
            .expect("a payable call from an empty account still estimates");

        assert_eq!(
            c.balance(&pauper).await.unwrap(),
            0,
            "minted funds must die with the estimate"
        );
        assert_eq!(
            c.balance(&addr).await.unwrap(),
            0,
            "an estimate moves nothing"
        );
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
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
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
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;
        assert_eq!(c.balance(&addr).await.unwrap(), 0);

        let value = 3 * SUN_PER_TRX;
        c.call_value(&addr, [], &deployer, U256::from(value), AMPLE)
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
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;

        c.call(&addr, [], &deployer, AMPLE)
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
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
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
    async fn an_exact_gas_limit_is_honored_and_one_below_the_cost_runs_out_of_gas() {
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        // Fallback writes 0x2a to slot 0: PUSH1 0x2a, PUSH1 0x00, SSTORE, STOP.
        let storing = initcode_returning(&[0x60, 0x2a, 0x60, 0x00, 0x55, 0x00]);

        let needed = match c
            .estimate_deploy_create(storing.clone(), [], &deployer)
            .await
            .expect("deploy estimates")
            .compute
        {
            TronCompute::Gas(gas) => gas,
            other => panic!("the mock forecasts gas: got {other:?}"),
        };
        let err = c
            .deploy_create(
                storing.clone(),
                [],
                &deployer,
                TronLimit::Gas(needed - 1),
                CALLER_PAYS,
            )
            .await
            .expect_err("a budget under the true cost cannot deploy");
        assert!(matches!(err, TronError::Deploy(_)), "got {err:?}");

        // Exactly the forecast cost suffices: the limit is a budget, not a fee.
        let deploy = c
            .deploy_create(storing, [], &deployer, TronLimit::Gas(needed), CALLER_PAYS)
            .await
            .expect("a budget equal to the true cost deploys");
        assert_eq!(deploy.resources.compute, TronCompute::Gas(needed));

        let needed = match c
            .estimate_call(&deploy.address, [], &deployer)
            .await
            .expect("call estimates")
            .compute
        {
            TronCompute::Gas(gas) => gas,
            other => panic!("the mock forecasts gas: got {other:?}"),
        };
        let err = c
            .call(&deploy.address, [], &deployer, TronLimit::Gas(needed - 1))
            .await
            .expect_err("a budget under the true cost cannot run");
        assert!(matches!(err, TronError::Execute(_)), "got {err:?}");
        let exec = c
            .call(&deploy.address, [], &deployer, TronLimit::Gas(needed))
            .await
            .expect("a budget equal to the true cost runs");
        assert_eq!(exec.resources.compute, TronCompute::Gas(needed));
    }

    #[tokio::test]
    async fn an_estimated_limit_covers_the_op_and_is_sized_by_the_chains_adjustment() {
        // `Estimated` is the forecast times the chain's `gas_adjustment`, so on a chain whose
        // adjustment is generous the op runs, and on one whose adjustment is under 1.0 (which the
        // config layer rejects, and which only a hand-built chain can produce) the very same op
        // runs out of gas. That is the proof the chain's number reaches revm rather than a
        // constant standing in for it.
        let storing = initcode_returning(&[0x60, 0x2a, 0x60, 0x00, 0x55, 0x00]);

        let mut c = provider_with_adjustment(1.3);
        let deployer = c.new_account("deployer").await;
        let deploy = c
            .deploy_create(
                storing.clone(),
                [],
                &deployer,
                TronLimit::Estimated,
                CALLER_PAYS,
            )
            .await
            .expect("a forecast with 30% headroom covers the deploy");
        c.call(&deploy.address, [], &deployer, TronLimit::Estimated)
            .await
            .expect("a forecast with 30% headroom covers the call");

        let mut starved = provider_with_adjustment(0.5);
        let deployer = starved.new_account("deployer").await;
        let err = starved
            .deploy_create(storing, [], &deployer, TronLimit::Estimated, CALLER_PAYS)
            .await
            .expect_err("half the forecast cannot cover the deploy");
        assert!(matches!(err, TronError::Deploy(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_sun_fee_limit_is_rejected_rather_than_ignored_or_converted() {
        // The mock is revm: it meters EVM gas and has no energy, so it has no price at which to
        // buy energy with sun. Honoring a `fee_limit` would mean inventing both an energy figure
        // and a price for it; silently ignoring it would leave the caller believing a cap is in
        // force that is not. So it is an error, exactly as `TronCompute` refuses to call the
        // mock's gas energy.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;

        let err = c
            .deploy_create(
                initcode.clone(),
                [],
                &deployer,
                TronLimit::Fee(1_000_000_000),
                CALLER_PAYS,
            )
            .await
            .expect_err("a sun fee cap cannot bound a revm transaction");
        assert!(
            matches!(&err, TronError::Deploy(m) if m.contains("sun") && m.contains("revm")),
            "got {err:?}"
        );

        let addr = c
            .deploy_create(initcode, [], &deployer, AMPLE, CALLER_PAYS)
            .await
            .expect("empty-runtime deploy succeeds")
            .address;
        let err = c
            .call(&addr, [], &deployer, TronLimit::Fee(1_000_000_000))
            .await
            .expect_err("a sun fee cap cannot bound a revm transaction");
        assert!(
            matches!(&err, TronError::Execute(m) if m.contains("sun") && m.contains("revm")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn a_rejected_limit_commits_nothing() {
        // The limit is resolved before the transaction is submitted, so a rejected one must not
        // have charged bandwidth or moved the chain on its way out.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let height = c.block_height().await;

        c.deploy_create(
            initcode,
            [0xaa, 0xbb],
            &deployer,
            TronLimit::Fee(1_000_000_000),
            CALLER_PAYS,
        )
        .await
        .expect_err("a sun fee cap cannot bound a revm transaction");

        assert_eq!(c.bandwidth(&deployer), FREE_BANDWIDTH_PER_DAY);
        assert_eq!(c.block_height().await, height);
    }

    #[tokio::test]
    async fn get_code_returns_runtime_for_a_contract_and_empty_for_an_eoa() {
        let mut c = provider();
        let deployer = c.new_account("deployer").await;

        // Deploy a contract whose runtime is a fixed non-empty byte string, then read it back.
        let runtime = [0x60u8, 0x00, 0x00];
        let addr = c
            .deploy_create(
                initcode_returning(&runtime),
                [],
                &deployer,
                AMPLE,
                CALLER_PAYS,
            )
            .await
            .expect("deploy succeeds")
            .address;
        assert_eq!(
            c.get_code(&addr)
                .await
                .expect("read contract code")
                .as_ref(),
            runtime,
            "get_code returns the deployed runtime bytecode"
        );

        // An ordinary account (the deployer is an EOA) carries no code.
        assert!(
            c.get_code(&deployer)
                .await
                .expect("read eoa code")
                .is_empty(),
            "an EOA has no runtime bytecode"
        );
    }

    #[tokio::test]
    async fn raw_escape_hatches_are_unimplemented_on_the_mock() {
        // The mock is in-process: there is no node behind the JSON-RPC / REST escape hatches, and no
        // real transaction to sign or broadcast, so each is an explicit `Unimplemented`.
        let c = provider();
        assert!(matches!(
            c.raw_request("eth_chainId", serde_json::json!([])).await,
            Err(TronError::Unimplemented(_))
        ));
        assert!(matches!(
            c.wallet_request("getnowblock", serde_json::json!({})).await,
            Err(TronError::Unimplemented(_))
        ));
        assert!(matches!(
            c.sign_transaction(serde_json::json!({})).await,
            Err(TronError::Unimplemented(_))
        ));
        assert!(matches!(
            c.broadcast_transaction(serde_json::json!({})).await,
            Err(TronError::Unimplemented(_))
        ));
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
