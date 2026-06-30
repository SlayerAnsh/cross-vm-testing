//! In-process CosmWasm provider backed by `cw-multi-test`.
//!
//! [`CwMockProvider`] wraps a `cw-multi-test` `App` configured with the chain's bech32
//! prefix, so generated addresses carry the chain's prefix (e.g. `osmo1...`).

use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use cosmwasm_std::testing::MockStorage;
use cosmwasm_std::{coins, Addr, Coin, Empty};
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use cw_multi_test::{
    App, AppBuilder, BankKeeper, Contract, DistributionKeeper, Executor, FailingModule,
    GovFailingModule, IbcFailingModule, IntoBech32, MockApiBech32, StakeKeeper, StargateFailing,
    WasmKeeper,
};

use crate::chains::CosmosChainInfo;
use crate::error::CwError;
use crate::msg::CwSerde;

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
}

impl CwMockProvider {
    /// Build a fresh mock chain from a predefined [`CosmosChainInfo`].
    pub fn new(info: CosmosChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let mut app = AppBuilder::new()
            .with_api(MockApiBech32::new(info.bech32_prefix))
            .build(|_router, _api, _storage| {});
        // Override cw-multi-test's 2019-era default block time with the shared mock clock so a
        // cross-VM packet's timeout (stamped on one VM, checked on another) compares correctly.
        app.update_block(|block| {
            block.time = cosmwasm_std::Timestamp::from_seconds(cross_vm_core::MOCK_BLOCK_TIMESTAMP);
        });
        Self {
            app: Rc::new(RefCell::new(app)),
            info,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Borrow the underlying `cw-multi-test` `App` for advanced use.
    pub fn app(&self) -> Ref<'_, CwApp> {
        self.app.borrow()
    }

    /// Mutably borrow the underlying `App`.
    pub fn app_mut(&self) -> RefMut<'_, CwApp> {
        self.app.borrow_mut()
    }

    /// Upload wasm to the chain and return its code id.
    pub async fn store_code(&self, code: CwCode) -> u64 {
        self.app.borrow_mut().store_code(code)
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
            .map_err(|e| CwError::Deploy(e.to_string()))
    }

    /// Execute a state-mutating message against a contract instance.
    pub async fn execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        sender: &Addr,
        funds: &[Coin],
    ) -> Result<cw_multi_test::AppResponse, CwError> {
        self.app
            .borrow_mut()
            .execute_contract(sender.clone(), addr.clone(), &msg, funds)
            .map_err(|e| CwError::Execute(e.to_string()))
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
        let _ = self.set_balance(&addr, DEFAULT_FUNDING).await;
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

    async fn set_balance(&mut self, addr: &Addr, amount: u128) -> Result<(), CwError> {
        let denom = self.info.native_denom;
        let addr = addr.clone();
        self.app
            .borrow_mut()
            .init_modules(|router, _api, storage| {
                router
                    .bank
                    .init_balance(storage, &addr, coins(amount, denom))
            })
            .map_err(|e| CwError::Balance(e.to_string()))
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
