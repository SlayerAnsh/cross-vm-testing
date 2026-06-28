//! CosmWasm chain providers.
//!
//! [`CwMockProvider`] wraps a `cw-multi-test` `App` configured with the chain's bech32
//! prefix. [`CwRpcProvider`] is a phase-1 stub: it constructs and implements the trait
//! but every operation returns [`CwError::Unimplemented`] until live RPC lands.

use cosmwasm_std::testing::MockStorage;
use cosmwasm_std::{coins, Addr, Empty};
use cross_vm_core::{ChainKind, ChainProvider, CrossVmError};
use cw_multi_test::{
    next_block, App, AppBuilder, BankKeeper, Contract, DistributionKeeper, Executor,
    FailingModule, GovFailingModule, IbcFailingModule, IntoBech32, MockApiBech32, StakeKeeper,
    StargateFailing, WasmKeeper,
};
use thiserror::Error;

use crate::chains::CosmosChainInfo;

/// Default funding handed to accounts created via [`ChainProvider::new_account`].
pub const DEFAULT_FUNDING: u128 = 1_000_000_000_000;

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

/// Errors surfaced by the CosmWasm providers.
#[derive(Debug, Error)]
pub enum CwError {
    /// `store_code` / `instantiate_contract` failed.
    #[error("deploy: {0}")]
    Deploy(String),
    /// `execute_contract` failed.
    #[error("execute: {0}")]
    Execute(String),
    /// `query_wasm_smart` failed.
    #[error("query: {0}")]
    Query(String),
    /// A bank operation failed.
    #[error("balance: {0}")]
    Balance(String),
    /// Feature not implemented yet (live RPC in phase 1).
    #[error("unimplemented: {0}")]
    Unimplemented(String),
}

impl From<CwError> for CrossVmError {
    fn from(e: CwError) -> Self {
        let kind = ChainKind::CosmWasm;
        match e {
            CwError::Deploy(reason) => CrossVmError::Deploy { kind, reason },
            CwError::Execute(reason) => CrossVmError::Execute { kind, reason },
            CwError::Query(reason) => CrossVmError::Query { kind, reason },
            CwError::Balance(reason) => CrossVmError::Balance { kind, reason },
            CwError::Unimplemented(what) => CrossVmError::Unimplemented { kind, what },
        }
    }
}

/// In-process CosmWasm provider backed by `cw-multi-test`.
pub struct CwMockProvider {
    app: CwApp,
    info: CosmosChainInfo,
}

impl CwMockProvider {
    /// Build a fresh mock chain from a predefined [`CosmosChainInfo`].
    pub fn new(info: CosmosChainInfo) -> Self {
        let app = AppBuilder::new()
            .with_api(MockApiBech32::new(info.bech32_prefix))
            .build(|_router, _api, _storage| {});
        Self { app, info }
    }

    /// Borrow the underlying `cw-multi-test` `App` for advanced use.
    pub fn app(&self) -> &CwApp {
        &self.app
    }

    /// Mutably borrow the underlying `App`.
    pub fn app_mut(&mut self) -> &mut CwApp {
        &mut self.app
    }
}

impl ChainProvider for CwMockProvider {
    type Spec = CosmosChainInfo;
    type Address = Addr;
    type Account = Addr;
    type Code = Box<dyn Contract<Empty, Empty>>;
    type InitMsg = serde_json::Value;
    type ExecMsg = serde_json::Value;
    type QueryMsg = serde_json::Value;
    type ContractRef = Addr;
    type Response = cw_multi_test::AppResponse;
    type QueryResponse = serde_json::Value;
    type Balance = u128;
    type Error = CwError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    fn new_account(&mut self, label: &str) -> Addr {
        let addr = label.into_bech32_with_prefix(self.info.bech32_prefix);
        // Best-effort default funding; ignore the (infallible in practice) result.
        let _ = self.set_balance(&addr, DEFAULT_FUNDING);
        addr
    }

    fn balance(&self, addr: &Addr) -> Result<u128, CwError> {
        let coin = self
            .app
            .wrap()
            .query_balance(addr, self.info.native_denom)
            .map_err(|e| CwError::Balance(e.to_string()))?;
        coin.amount
            .to_string()
            .parse::<u128>()
            .map_err(|e| CwError::Balance(e.to_string()))
    }

    fn set_balance(&mut self, addr: &Addr, amount: u128) -> Result<(), CwError> {
        let denom = self.info.native_denom;
        let addr = addr.clone();
        self.app
            .init_modules(|router, _api, storage| {
                router
                    .bank
                    .init_balance(storage, &addr, coins(amount, denom))
            })
            .map_err(|e| CwError::Balance(e.to_string()))
    }

    fn block_height(&self) -> u64 {
        self.app.block_info().height
    }

    fn advance_blocks(&mut self, n: u64) {
        for _ in 0..n {
            self.app.update_block(next_block);
        }
    }

    fn deploy(
        &mut self,
        code: Self::Code,
        init: serde_json::Value,
        sender: &Addr,
    ) -> Result<Addr, CwError> {
        let code_id = self.app.store_code(code);
        self.app
            .instantiate_contract(code_id, sender.clone(), &init, &[], "contract", None)
            .map_err(|e| CwError::Deploy(e.to_string()))
    }

    fn execute(
        &mut self,
        contract: &Addr,
        msg: serde_json::Value,
        sender: &Addr,
    ) -> Result<Self::Response, CwError> {
        self.app
            .execute_contract(sender.clone(), contract.clone(), &msg, &[])
            .map_err(|e| CwError::Execute(e.to_string()))
    }

    fn query(
        &self,
        contract: &Addr,
        msg: serde_json::Value,
    ) -> Result<serde_json::Value, CwError> {
        self.app
            .wrap()
            .query_wasm_smart(contract, &msg)
            .map_err(|e| CwError::Query(e.to_string()))
    }
}

/// Phase-1 stub for a live-RPC CosmWasm provider. Constructs fine; every operation
/// returns [`CwError::Unimplemented`].
pub struct CwRpcProvider {
    info: CosmosChainInfo,
}

impl CwRpcProvider {
    /// Create an RPC provider bound to a chain's metadata.
    pub fn new(info: CosmosChainInfo) -> Self {
        Self { info }
    }
}

impl ChainProvider for CwRpcProvider {
    type Spec = CosmosChainInfo;
    type Address = Addr;
    type Account = Addr;
    type Code = Box<dyn Contract<Empty, Empty>>;
    type InitMsg = serde_json::Value;
    type ExecMsg = serde_json::Value;
    type QueryMsg = serde_json::Value;
    type ContractRef = Addr;
    type Response = cw_multi_test::AppResponse;
    type QueryResponse = serde_json::Value;
    type Balance = u128;
    type Error = CwError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    fn new_account(&mut self, label: &str) -> Addr {
        // No signing backend yet; return a deterministic placeholder address.
        label.into_bech32_with_prefix(self.info.bech32_prefix)
    }

    fn balance(&self, _addr: &Addr) -> Result<u128, CwError> {
        Err(CwError::Unimplemented("rpc balance".into()))
    }

    fn set_balance(&mut self, _addr: &Addr, _amount: u128) -> Result<(), CwError> {
        Err(CwError::Unimplemented("rpc set_balance".into()))
    }

    fn block_height(&self) -> u64 {
        0
    }

    fn advance_blocks(&mut self, _n: u64) {}

    fn deploy(
        &mut self,
        _code: Self::Code,
        _init: serde_json::Value,
        _sender: &Addr,
    ) -> Result<Addr, CwError> {
        Err(CwError::Unimplemented("rpc deploy".into()))
    }

    fn execute(
        &mut self,
        _contract: &Addr,
        _msg: serde_json::Value,
        _sender: &Addr,
    ) -> Result<Self::Response, CwError> {
        Err(CwError::Unimplemented("rpc execute".into()))
    }

    fn query(
        &self,
        _contract: &Addr,
        _msg: serde_json::Value,
    ) -> Result<serde_json::Value, CwError> {
        Err(CwError::Unimplemented("rpc query".into()))
    }
}
