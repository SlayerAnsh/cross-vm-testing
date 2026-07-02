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
use crate::provider::execution::TronExecution;
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
    ) -> Result<TronAddress, TronError> {
        self.core
            .deploy_create(bytecode, constructor_args.as_ref(), from.as_evm())
            .map(TronAddress::from_evm)
            .map_err(|f| TronError::Deploy(f.deploy_message()))
    }

    /// Execute a state-mutating call against `to`, returning its output plus emitted logs.
    pub async fn call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
    ) -> Result<TronExecution, TronError> {
        // Coarse bandwidth accounting: charge the caller by encoded calldata length. The mock
        // does not gate execution on the result (it models the burn-for-fee fallback as free).
        // Source: <https://developers.tron.network/docs/resource-model>
        self.resources
            .borrow_mut()
            .consume_bandwidth(from, calldata.as_ref().len());
        self.core
            .call(to.as_evm(), calldata.as_ref(), from.as_evm(), U256::ZERO)
            .map(TronExecution::from)
            .map_err(|f| TronError::Execute(f.call_message("call")))
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
}

impl From<cross_vm_revm_common::Execution> for TronExecution {
    fn from(e: cross_vm_revm_common::Execution) -> Self {
        Self {
            output: e.output,
            logs: e.logs,
            tx_hash: None,
        }
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
        let _ = self.set_balance(&addr, DEFAULT_FUNDING_SUN).await;
        addr
    }

    async fn balance(&self, addr: &TronAddress) -> Result<u64, TronError> {
        // Convert revm's U256 wei-shaped balance back to u64 sun at the boundary.
        self.core
            .balance(addr.as_evm())
            .map(|b| b.saturating_to::<u64>())
            .map_err(|f| TronError::Balance(f.call_message("balance")))
    }

    async fn set_balance(&mut self, addr: &TronAddress, amount: u64) -> Result<(), TronError> {
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
        c.set_balance(&a, 42 * SUN_PER_TRX).await.unwrap();
        assert_eq!(c.balance(&a).await.unwrap(), 42 * SUN_PER_TRX);
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
    async fn deploy_create_returns_tron_address() {
        // Minimal initcode that deploys an empty runtime: PUSH1 0x00, PUSH1 0x00, RETURN.
        // It returns a zero-length runtime, so the deploy succeeds and yields a contract address.
        let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
        let mut c = provider();
        let deployer = c.new_account("deployer").await;
        let addr = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("empty-runtime deploy succeeds");
        assert!(addr.to_base58().starts_with('T'));
        // The deployed (empty) account has no balance.
        assert_eq!(c.balance(&addr).await.unwrap(), 0);
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
        let addr = c
            .deploy_create(initcode, [], &deployer)
            .await
            .expect("tronc token-guard opcodes decode and deploy succeeds");
        assert!(addr.to_base58().starts_with('T'));
    }

    #[tokio::test]
    async fn advance_blocks_moves_height() {
        let mut c = provider();
        let start = c.block_height().await;
        c.advance_blocks(5, BlockTime::Increment(1)).await;
        assert_eq!(c.block_height().await, start + 5);
    }
}
