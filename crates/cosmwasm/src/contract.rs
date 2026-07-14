//! A CosmWasm contract handle that threads `code_id` and address through the deploy lifecycle.
//!
//! [`CwContract`] removes the address-passing boilerplate from contract calls. Typed per-contract
//! methods come from `CwExecuteFns` / `CwQueryFns` derives scoped to a [`CwInterface`] marker:
//! `chain.contract_as::<CounterContract>(addr).increment(wallet)` only resolves when the handle
//! carries the matching interface type.
//!
//! A handle can start unbound and walk the full lifecycle, carrying `code_id` then address
//! internally (cw-orch style):
//!
//! ```ignore
//! let counter = CwContract::<CounterContract>::new(chain)
//!     .store_code(wasm, wallet).await?       // stores code_id + its tx hash + its gas internally
//!     .instantiate(msg, wallet, &[], "counter").await?;  // stores address + tx hash + gas
//! counter.increment(wallet.as_str()).await?; // typed call, no address passing
//! let n = counter.get_count().await?.count;
//! let deploy_tx = counter.instantiate_tx_hash().expect("instantiated");
//! // `Some(gas)` on a live chain, `Some(None)` on the mock, which cannot meter.
//! let deploy_gas = counter.instantiate_gas().expect("instantiated");
//! ```
//!
//! For dynamic message construction (no typed `*Fns` in scope), bind with
//! [`CwChain::contract`] to get an untyped [`CwContract<()>`] and call
//! [`execute`](Self::execute) / [`query`](Self::query) directly.

use std::marker::PhantomData;

use cosmwasm_std::{Addr, Coin};
use cross_vm_core::WalletLabel;

use crate::chain::CwChain;
use crate::error::CwError;
use crate::msg::CwSerde;
use crate::provider::CwCodeSource;
use crate::{CwExecution, CwGas};

/// Compile-time marker tying a CosmWasm contract's message types to a zero-sized handle tag.
///
/// Declare one per contract with [`cross_vm_macros::cross_vm_cw_interface`]; typed
/// `CwExecuteFns` / `CwQueryFns` impls are scoped to `CwContract<I>` where
/// `I: CwInterface<ExecuteMsg = ...>` / `I: CwInterface<QueryMsg = ...>`.
pub trait CwInterface {
    /// The contract's instantiate message type.
    type InstantiateMsg: CwSerde;
    /// The contract's execute message type.
    type ExecuteMsg: CwSerde;
    /// The contract's query message type.
    type QueryMsg: CwSerde;
}

/// A [`CwChain`] plus the deploy state (`code_id`, address) of one contract.
///
/// The type parameter `I` is a zero-sized [`CwInterface`] marker that scopes typed `*Fns`
/// methods to this contract. Use `I = ()` for an untyped handle (dynamic `execute` / `query`).
///
/// Cheap to construct: `CwChain` is `Rc`-backed, so the handle owns a clone and shares the
/// underlying chain state. `code_id` (with its tx hash and gas) is set by
/// [`store_code`](Self::store_code) and the address (with its tx hash and gas) by
/// [`instantiate`](Self::instantiate); all are `None` on a fresh unbound handle. The outer
/// `Option`s track which lifecycle steps this handle has run, not what the backend produced: a
/// step that ran always carries a hash, but only a metering backend carries a gas figure, so the
/// gas fields keep the backend's own [`CwGas`] `Option` nested inside. See
/// [`store_code_gas`](Self::store_code_gas).
#[derive(Clone)]
pub struct CwContract<I = ()> {
    chain: CwChain,
    code_id: Option<u64>,
    addr: Option<Addr>,
    store_code_tx: Option<String>,
    instantiate_tx: Option<String>,
    store_code_gas: Option<Option<CwGas>>,
    instantiate_gas: Option<Option<CwGas>>,
    _marker: PhantomData<I>,
}

impl<I> CwContract<I> {
    /// A fresh, unbound handle on `chain`: no stored code, no address. Walk the lifecycle with
    /// [`store_code`](Self::store_code) then [`instantiate`](Self::instantiate).
    pub fn new(chain: CwChain) -> Self {
        Self {
            chain,
            code_id: None,
            addr: None,
            store_code_tx: None,
            instantiate_tx: None,
            store_code_gas: None,
            instantiate_gas: None,
            _marker: PhantomData,
        }
    }

    /// Bind `chain` to the contract already deployed at `addr` (no `code_id`, no deploy tx hashes,
    /// no deploy gas: this handle did not run the deploy).
    pub fn bound(chain: CwChain, addr: Addr) -> Self {
        Self {
            chain,
            code_id: None,
            addr: Some(addr),
            store_code_tx: None,
            instantiate_tx: None,
            store_code_gas: None,
            instantiate_gas: None,
            _marker: PhantomData,
        }
    }

    /// Upload contract code and record the resulting `code_id`, tx hash and gas internally, then
    /// return the handle for chaining into [`instantiate`](Self::instantiate). Signed by wallet
    /// `wallet`. Read them back with [`store_code_tx_hash`](Self::store_code_tx_hash) and
    /// [`store_code_gas`](Self::store_code_gas).
    ///
    /// Backend-agnostic like [`CwChain::store_code`]: pass compiled wasm bytes for a live RPC
    /// chain, a native `cw-multi-test` contract object for the mock, or a
    /// [`CwCodeSource::both`] carrying both so the same handle code runs on either backend.
    pub async fn store_code(
        mut self,
        code: impl Into<CwCodeSource>,
        wallet: WalletLabel<'_>,
    ) -> Result<Self, CwError> {
        let stored = self.chain.store_code(code, wallet).await?;
        self.code_id = Some(stored.code_id);
        self.store_code_tx = Some(stored.tx_hash);
        self.store_code_gas = Some(stored.gas);
        Ok(self)
    }

    /// Instantiate the previously stored `code_id` and record the resulting address, tx hash and
    /// gas internally, then return the bound handle. Requires [`store_code`](Self::store_code) to
    /// have run first. Read them back with
    /// [`instantiate_tx_hash`](Self::instantiate_tx_hash) and
    /// [`instantiate_gas`](Self::instantiate_gas).
    pub async fn instantiate<Init: CwSerde>(
        mut self,
        init: Init,
        wallet: WalletLabel<'_>,
        funds: &[Coin],
        label: &str,
    ) -> Result<Self, CwError> {
        let code_id = self.code_id.ok_or_else(|| {
            CwError::Deploy("store_code() must be called before instantiate()".into())
        })?;
        let instantiated = self
            .chain
            .instantiate(code_id, init, wallet, funds, label)
            .await?;
        self.addr = Some(instantiated.address);
        self.instantiate_tx = Some(instantiated.tx_hash);
        self.instantiate_gas = Some(instantiated.gas);
        Ok(self)
    }

    /// The bound contract address, or `None` if not yet instantiated.
    pub fn address(&self) -> Option<&Addr> {
        self.addr.as_ref()
    }

    /// The stored code id, or `None` if [`store_code`](Self::store_code) has not run.
    pub fn code_id(&self) -> Option<u64> {
        self.code_id
    }

    /// The tx hash of the upload, or `None` if [`store_code`](Self::store_code) has not run.
    pub fn store_code_tx_hash(&self) -> Option<&str> {
        self.store_code_tx.as_deref()
    }

    /// The tx hash of the instantiation, or `None` if this handle did not run
    /// [`instantiate`](Self::instantiate) (a handle from [`bound`](Self::bound) never does).
    pub fn instantiate_tx_hash(&self) -> Option<&str> {
        self.instantiate_tx.as_deref()
    }

    /// What the upload cost, or `None` if [`store_code`](Self::store_code) has not run.
    ///
    /// The two `Option`s are two different facts and neither may be dropped. The outer one is the
    /// handle's, exactly as on [`store_code_tx_hash`](Self::store_code_tx_hash): `None` means this
    /// handle never uploaded (fresh from [`new`](Self::new), or from [`bound`](Self::bound), which
    /// skips the deploy). The inner one is the backend's, verbatim from [`CwStoreCode::gas`]:
    /// `Some(gas)` on live RPC, `None` on the mock, which has no gas meter and reports absence
    /// rather than a fabricated zero (see [`CwGas`]).
    ///
    /// So `Some(None)` is "uploaded, on a backend that cannot price it" and `None` is "never
    /// uploaded". A caller that flattens the two cannot tell a step it skipped from a cost the
    /// chain could not report.
    ///
    /// [`CwStoreCode::gas`]: crate::CwStoreCode::gas
    pub fn store_code_gas(&self) -> Option<Option<CwGas>> {
        self.store_code_gas
    }

    /// What the instantiation cost, or `None` if this handle did not run
    /// [`instantiate`](Self::instantiate) (a handle from [`bound`](Self::bound) never does).
    ///
    /// Nested exactly like [`store_code_gas`](Self::store_code_gas): the outer `Option` is whether
    /// this handle ran the step, the inner one whether the backend could meter it.
    pub fn instantiate_gas(&self) -> Option<Option<CwGas>> {
        self.instantiate_gas
    }

    /// The bound address, or an error if the handle is not yet instantiated.
    fn require_addr(&self) -> Result<&Addr, CwError> {
        self.addr.as_ref().ok_or_else(|| {
            CwError::Execute(
                "contract not instantiated; call store_code() then instantiate()".into(),
            )
        })
    }

    /// Execute a state-mutating message against the bound contract, signed by wallet `wallet`,
    /// sending no funds. For a funded call use [`execute_with_funds`](Self::execute_with_funds)
    /// (the path the `CwExecuteFns` derive's `#[payable]` variants take).
    pub async fn execute<E: CwSerde>(&self, msg: E, wallet: &str) -> Result<CwExecution, CwError> {
        self.execute_with_funds(msg, wallet, &[]).await
    }

    /// Execute a message against the bound contract while attaching `funds`, signed by `wallet`.
    /// The funded path behind the `CwExecuteFns` derive's `#[payable]` variants.
    pub async fn execute_with_funds<E: CwSerde>(
        &self,
        msg: E,
        wallet: &str,
        funds: &[Coin],
    ) -> Result<CwExecution, CwError> {
        self.chain
            .execute_contract(self.require_addr()?, msg, WalletLabel::wrap(wallet), funds)
            .await
    }

    /// Run a read-only smart query against the bound contract.
    pub async fn query<Q: CwSerde, R: CwSerde>(&self, msg: Q) -> Result<R, CwError> {
        let addr = self.addr.as_ref().ok_or_else(|| {
            CwError::Query("contract not instantiated; call instantiate()".into())
        })?;
        self.chain.query_wasm_smart(addr, msg).await
    }
}
