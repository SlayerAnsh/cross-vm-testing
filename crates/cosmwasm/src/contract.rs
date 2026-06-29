//! A CosmWasm contract handle that threads `code_id` and address through the deploy lifecycle.
//!
//! [`CwContract`] removes the address-passing boilerplate from contract calls. It is also the
//! receiver the `CwExecuteFns` / `CwQueryFns` derives (in `cross-vm-macros`) generate typed,
//! per-variant methods for, so a caller writes `contract.increment(wallet)` instead of building
//! an `ExecuteMsg` and naming the address.
//!
//! A handle can start unbound and walk the full lifecycle, carrying `code_id` then address
//! internally (cw-orch style):
//!
//! ```ignore
//! let counter = CwContract::new(chain)
//!     .store_code(wasm, wallet).await?       // stores code_id internally
//!     .instantiate(msg, wallet, &[], "counter").await?;  // stores address internally
//! counter.increment(wallet.as_str()).await?; // typed call, no address passing
//! let n = counter.get_count().await?.count;
//! ```
//!
//! or bind to an already-deployed address with [`CwContract::bound`] (via [`CwChain::contract`]).

use cosmwasm_std::{Addr, Coin};
use cross_vm_core::WalletLabel;

use crate::chain::CwChain;
use crate::error::CwError;
use crate::msg::CwSerde;
use crate::CwAppResponse;

/// A [`CwChain`] plus the deploy state (`code_id`, address) of one contract.
///
/// Cheap to construct: `CwChain` is `Rc`-backed, so the handle owns a clone and shares the
/// underlying chain state. `code_id` is set by [`store_code`](Self::store_code) and the address by
/// [`instantiate`](Self::instantiate); both are `None` on a fresh unbound handle.
#[derive(Clone)]
pub struct CwContract {
    chain: CwChain,
    code_id: Option<u64>,
    addr: Option<Addr>,
}

impl CwContract {
    /// A fresh, unbound handle on `chain`: no stored code, no address. Walk the lifecycle with
    /// [`store_code`](Self::store_code) then [`instantiate`](Self::instantiate).
    pub fn new(chain: CwChain) -> Self {
        Self {
            chain,
            code_id: None,
            addr: None,
        }
    }

    /// Bind `chain` to the contract already deployed at `addr` (no `code_id` known).
    pub fn bound(chain: CwChain, addr: Addr) -> Self {
        Self {
            chain,
            code_id: None,
            addr: Some(addr),
        }
    }

    /// Upload `wasm` and record the resulting `code_id` internally, then return the handle for
    /// chaining into [`instantiate`](Self::instantiate). Signed by wallet `wallet`.
    pub async fn store_code(
        mut self,
        wasm: Vec<u8>,
        wallet: WalletLabel<'_>,
    ) -> Result<Self, CwError> {
        self.code_id = Some(self.chain.store_code_wasm(wasm, wallet).await?);
        Ok(self)
    }

    /// Instantiate the previously stored `code_id` and record the resulting address internally,
    /// then return the bound handle. Requires [`store_code`](Self::store_code) to have run first.
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
        self.addr = Some(
            self.chain
                .instantiate(code_id, init, wallet, funds, label)
                .await?,
        );
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
    pub async fn execute<E: CwSerde>(
        &self,
        msg: E,
        wallet: &str,
    ) -> Result<CwAppResponse, CwError> {
        self.execute_with_funds(msg, wallet, &[]).await
    }

    /// Execute a message against the bound contract while attaching `funds`, signed by `wallet`.
    /// The funded path behind the `CwExecuteFns` derive's `#[payable]` variants.
    pub async fn execute_with_funds<E: CwSerde>(
        &self,
        msg: E,
        wallet: &str,
        funds: &[Coin],
    ) -> Result<CwAppResponse, CwError> {
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
