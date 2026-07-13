//! In-process CosmWasm provider backed by `cw-multi-test`.
//!
//! [`CwMockProvider`] wraps a `cw-multi-test` `App` configured with the chain's bech32
//! prefix, so generated addresses carry the chain's prefix (e.g. `osmo1...`).

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use sha2::{Digest, Sha256};

use cosmwasm_std::testing::MockStorage;
use cosmwasm_std::{coin, Addr, BankMsg, Coin, CosmosMsg, Empty, Uint128};
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use cw_multi_test::{
    App, AppBuilder, BankKeeper, Contract, DistributionKeeper, Executor, FailingModule,
    GovFailingModule, IbcFailingModule, IntoBech32, MockApiBech32, StakeKeeper, StargateFailing,
    WasmKeeper,
};

use crate::chains::CosmosChainInfo;
use crate::error::{any_chain, CwError};
use crate::msg::CwSerde;
use crate::provider::CwExecution;

/// Default funding handed to accounts created via [`ChainProvider::new_account`].
pub const DEFAULT_FUNDING: u128 = 1_000_000_000_000;

/// Deployable contract code for [`CwMockProvider::store_code`].
pub type CwCode = Box<dyn Contract<Empty, Empty>>;

/// A `cw-multi-test` `App` parameterized with a bech32 mock API so generated
/// addresses carry the chain's prefix (e.g. `osmo1...`).
pub type CwApp = App<
    BankKeeper,
    MockApiBech32,
    MockStorage,
    FailingModule<Empty, Empty, Empty>,
    WasmKeeper<Empty, Empty>,
    StakeKeeper,
    DistributionKeeper,
    IbcFailingModule,
    GovFailingModule,
    StargateFailing,
>;

/// In-process CosmWasm provider backed by `cw-multi-test`.
///
/// The `App` lives behind `Rc<RefCell<_>>` so the handle is cheap to `clone` and every
/// clone shares one chain state. This lets a contract own its own handle
/// (`Contract::new(chain)`) while the test still drives the same chain, and lets the
/// contract operations run behind `&self`.
#[derive(Clone)]
pub struct CwMockProvider {
    app: Rc<RefCell<CwApp>>,
    info: CosmosChainInfo,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, crate::wallet::CosmosSigner>>>,
    /// Monotonic per-chain execute counter, folded into the synthetic tx hash so repeated
    /// identical executes get distinct hashes (a real chain never reuses one). Shared across clones.
    tx_seq: Rc<Cell<u64>>,
}

impl CwMockProvider {
    /// Build a fresh mock chain from a predefined [`CosmosChainInfo`].
    pub fn new(info: CosmosChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let mut app = AppBuilder::new()
            .with_api(MockApiBech32::new(info.bech32_prefix))
            .build(|_router, _api, _storage| {});
        // cw-multi-test seeds its `BlockInfo` from cosmwasm-std's `mock_env()`, so both the block
        // time and the chain id are defaults unrelated to the selected preset. Override the
        // 2019-era time with the shared mock clock, so a cross-VM packet's timeout (stamped on one
        // VM, checked on another) compares correctly; override the `cosmos-testnet-14002` chain id
        // with the preset's, so a contract reading `env.block.chain_id` sees the chain it runs on.
        app.update_block(|block| {
            block.time = cosmwasm_std::Timestamp::from_seconds(cross_vm_core::MOCK_BLOCK_TIMESTAMP);
            block.chain_id = info.chain_id.to_string();
        });
        Self {
            app: Rc::new(RefCell::new(app)),
            info,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
            tx_seq: Rc::new(Cell::new(0)),
        }
    }

    /// Mint the next synthetic tx hash: sha256 over the caller's `fields` (the tx's identifying
    /// parts) plus the monotonic sequence, rendered as uppercase hex to match Tendermint's
    /// tx-hash format. The mock does not build or sign a real Cosmos transaction, so this is a
    /// stand-in that lets the same test script read a hash on both the mock and the live RPC
    /// backend; it does not equal the hash a live node would compute and must not be treated as a
    /// real on-chain identifier.
    fn next_tx_hash(&self, fields: &[&[u8]]) -> String {
        let seq = self.tx_seq.get();
        self.tx_seq.set(seq + 1);
        let mut hasher = Sha256::new();
        for field in fields {
            hasher.update(field);
        }
        hasher.update(seq.to_be_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect()
    }

    /// Borrow the underlying `cw-multi-test` `App` for advanced use.
    pub fn app(&self) -> Ref<'_, CwApp> {
        self.app.borrow()
    }

    /// Mutably borrow the underlying `App`.
    pub fn app_mut(&self) -> RefMut<'_, CwApp> {
        self.app.borrow_mut()
    }

    /// Upload a contract to the chain and return its code id, recording `creator` as the
    /// uploading account (mirroring the sender a live chain's `MsgStoreCode` records).
    pub async fn store_code(&self, creator: &Addr, code: CwCode) -> u64 {
        self.app
            .borrow_mut()
            .store_code_with_creator(creator.clone(), code)
    }

    /// Instantiate a contract from an uploaded code id.
    pub async fn instantiate<Init: CwSerde>(
        &self,
        code_id: u64,
        init: Init,
        sender: &Addr,
        funds: &[Coin],
        label: &str,
    ) -> Result<Addr, CwError> {
        self.app
            .borrow_mut()
            .instantiate_contract(code_id, sender.clone(), &init, funds, label, None)
            .map_err(|e| CwError::Deploy(any_chain(&e)))
    }

    /// Execute a state-mutating message against a contract instance.
    ///
    /// The in-process backend does not broadcast a real transaction, so the returned
    /// [`CwExecution`] carries a synthetic, deterministic `tx_hash` (see [`Self::next_tx_hash`])
    /// rather than `None`, so the same test script reads a hash on both the mock and live RPC.
    pub async fn execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        sender: &Addr,
        funds: &[Coin],
    ) -> Result<CwExecution, CwError> {
        let response = self
            .app
            .borrow_mut()
            .execute_contract(sender.clone(), addr.clone(), &msg, funds)
            .map_err(|e| CwError::Execute(any_chain(&e)))?;
        Ok(CwExecution {
            tx_hash: Some(self.next_tx_hash(&[sender.as_bytes(), addr.as_bytes()])),
            response,
        })
    }

    /// Send `amount` base units of bank `denom` from `sender` to `to`, and return the synthetic
    /// tx hash (see [`Self::next_tx_hash`]).
    ///
    /// Any bank denom moves verbatim (`uosmo`, `ibc/...`), not just the chain's native denom. An
    /// underfunded sender surfaces as [`CwError::Execute`].
    pub async fn transfer_funds(
        &self,
        to: &Addr,
        denom: &str,
        amount: u128,
        sender: &Addr,
    ) -> Result<String, CwError> {
        let msg = BankMsg::Send {
            to_address: to.to_string(),
            amount: vec![coin(amount, denom)],
        };
        self.app
            .borrow_mut()
            .execute(sender.clone(), CosmosMsg::Bank(msg))
            .map_err(|e| CwError::Execute(any_chain(&e)))?;
        Ok(self.next_tx_hash(&[
            sender.as_bytes(),
            to.as_bytes(),
            denom.as_bytes(),
            &amount.to_be_bytes(),
        ]))
    }

    /// Run a read-only smart query against a contract instance.
    pub async fn query_wasm_smart<Query: CwSerde, Resp: CwSerde>(
        &self,
        addr: &Addr,
        msg: Query,
    ) -> Result<Resp, CwError> {
        self.app
            .borrow()
            .wrap()
            .query_wasm_smart(addr, &msg)
            .map_err(|e| CwError::Query(e.to_string()))
    }

    /// Read a raw storage entry from a contract instance by its exact key.
    ///
    /// Returns `Some(bytes)` when the key exists and `None` when it is absent, matching the
    /// live RPC backend (where an empty raw response maps to `None`).
    pub async fn query_wasm_raw(
        &self,
        addr: &Addr,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, CwError> {
        self.app
            .borrow()
            .wrap()
            .query_wasm_raw(addr, key)
            .map_err(|e| CwError::Query(e.to_string()))
    }

    /// Dump every raw key-value pair held in a contract's storage, in ascending key order.
    ///
    /// Returns all `(key, value)` entries the contract has written, matching the live RPC
    /// backend's `AllContractState` query. Ordering follows wasmd (ascending by raw key).
    pub async fn get_contract_states(
        &self,
        addr: &Addr,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, CwError> {
        // `App::dump_wasm_raw` ranges the contract's prefixed storage in ascending key order.
        Ok(self.app.borrow().dump_wasm_raw(addr))
    }
}

impl ChainProvider for CwMockProvider {
    type Spec = CosmosChainInfo;
    type Address = Addr;
    type Account = Addr;
    type Balance = u128;
    type Error = CwError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, label: &str) -> Addr {
        let addr = label.into_bech32_with_prefix(self.info.bech32_prefix);
        // Best-effort default funding; ignore the (infallible in practice) result.
        let denom = self.info.native_denom;
        let _ = self.set_balance(&addr, denom, DEFAULT_FUNDING).await;
        addr
    }

    async fn balance(&self, addr: &Addr) -> Result<u128, CwError> {
        let coin = self
            .app
            .borrow()
            .wrap()
            .query_balance(addr, self.info.native_denom)
            .map_err(|e| CwError::Balance(e.to_string()))?;
        coin.amount
            .to_string()
            .parse::<u128>()
            .map_err(|e| CwError::Balance(e.to_string()))
    }

    async fn set_balance(&mut self, addr: &Addr, denom: &str, amount: u128) -> Result<(), CwError> {
        let addr = addr.clone();
        // `BankKeeper::init_balance` replaces the account's whole coin vector, so read,
        // merge the one denom, and write the full list back to preserve other denoms.
        #[allow(deprecated)]
        // cosmwasm-std 2.3 deprecates query_all_balances; no non-paginated replacement.
        let mut balances = self
            .app
            .borrow()
            .wrap()
            .query_all_balances(&addr)
            .map_err(|e| CwError::Balance(e.to_string()))?;
        match balances.iter_mut().find(|c| c.denom == denom) {
            Some(entry) => entry.amount = Uint128::new(amount),
            None => balances.push(coin(amount, denom)),
        }
        balances.retain(|c| !c.amount.is_zero());
        self.app
            .borrow_mut()
            .init_modules(|router, _api, storage| {
                router.bank.init_balance(storage, &addr, balances)
            })
            .map_err(|e| CwError::Balance(any_chain(&e)))
    }

    async fn block_height(&self) -> u64 {
        self.app.borrow().block_info().height
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        self.app.borrow_mut().update_block(|b| {
            b.height += n;
            b.time = cosmwasm_std::Timestamp::from_seconds(time.apply(b.time.seconds()));
        });
    }
}
