//! Backend-agnostic CosmWasm chain handle and asset funding.
//!
//! [`CwChain`] wraps either a mock or an RPC provider and implements
//! [`ChainProvider`] by delegating for chain-level operations. Contract operations
//! use idiomatic methods (`store_code`, `instantiate`, `execute_contract`, `query_wasm_smart`).
//! [`CwChain::ensure_asset`] backs the testing environment's funding phase.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cosmwasm_std::{Addr, Coin};
use cross_vm_core::{
    wallet_lock, BlockTime, ChainProvider, ChainSpec, FundError, WalletDeriver, WalletFactory,
    WalletLabel,
};
use serde::{Deserialize, Serialize};

use crate::asset::CwAsset;
use crate::chains::CosmosChainInfo;
use crate::error::CwError;
use crate::msg::CwSerde;
use crate::provider::{CwCodeSource, CwExecution, CwMockProvider, CwRpcProvider};
use crate::wallet::CosmosSigner;

/// CW20 balance query message for [`CwChain::ensure_asset`].
#[derive(Serialize, Deserialize)]
struct Cw20BalanceQuery {
    balance: Cw20BalanceAddress,
}

#[derive(Serialize, Deserialize)]
struct Cw20BalanceAddress {
    address: Addr,
}

#[derive(Serialize, Deserialize)]
struct Cw20BalanceResponse {
    balance: String,
}

/// A CosmWasm chain backed by either a mock or an RPC provider.
// The mock holds full in-process chain state; the RPC stub is tiny. The size gap is
// intentional and the value is not stored in bulk, so boxing would only add indirection.
#[derive(Clone)]
pub enum CwChain {
    /// In-process `cw-multi-test` backend.
    Mock(CwMockProvider),
    /// Live RPC backend (phase-1 stub).
    Rpc(CwRpcProvider),
}

impl From<CwMockProvider> for CwChain {
    fn from(p: CwMockProvider) -> Self {
        CwChain::Mock(p)
    }
}

impl From<CwRpcProvider> for CwChain {
    fn from(p: CwRpcProvider) -> Self {
        CwChain::Rpc(p)
    }
}

impl CwChain {
    /// Bind this chain to a deployed contract `addr`, returning an untyped [`crate::CwContract`]
    /// handle for dynamic `execute` / `query` calls.
    pub fn contract(&self, addr: Addr) -> crate::CwContract<()> {
        crate::CwContract::bound(self.clone(), addr)
    }

    /// Bind this chain to a deployed contract `addr`, returning a typed [`crate::CwContract`]
    /// handle scoped to the [`crate::CwInterface`] marker `I`.
    pub fn contract_as<I: crate::CwInterface>(&self, addr: Addr) -> crate::CwContract<I> {
        crate::CwContract::bound(self.clone(), addr)
    }

    fn wallets(&self) -> &Rc<WalletFactory> {
        match self {
            CwChain::Mock(p) => &p.wallets,
            CwChain::Rpc(p) => &p.wallets,
        }
    }

    fn signers(&self) -> &Rc<RefCell<HashMap<String, CosmosSigner>>> {
        match self {
            CwChain::Mock(p) => &p.signers,
            CwChain::Rpc(p) => &p.signers,
        }
    }

    /// Resolve a wallet label to its signer (derived once and cached). Broadcast serialization is
    /// handled separately on the RPC path via [`cross_vm_core::wallet_lock`] keyed by the live
    /// account; the in-process mock backend needs no lock.
    async fn acquire<'a>(&self, label: WalletLabel<'a>) -> Result<CosmosSigner, CwError> {
        let key = label.as_str();
        if let Some(signer) = self.signers().borrow().get(key).cloned() {
            return Ok(signer);
        }
        let def = self.wallets().resolve(label)?;
        let signer = self.signer_for(&def)?;
        self.signers()
            .borrow_mut()
            .insert(key.to_string(), signer.clone());
        Ok(signer)
    }

    /// Acquire the global broadcast lock for `addr` on this RPC chain, keyed by `(chain, address)`
    /// so the same live account serializes process-wide. Held across the whole send -> confirm.
    async fn broadcast_guard(p: &CwRpcProvider, addr: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let info = p.chain_info();
        wallet_lock::lock_broadcast(&wallet_lock::lock_key(info.kind(), info.chain_id(), addr))
            .await
    }

    /// Derive (and cache) a wallet's bech32 address without acquiring the broadcast lock.
    /// Useful for funding the wallet in the setup phase or asserting on its address.
    pub async fn wallet_address<'a>(&self, label: WalletLabel<'a>) -> Result<Addr, CwError> {
        let key = label.as_str();
        if let Some(signer) = self.signers().borrow().get(key).cloned() {
            return Ok(signer.address);
        }
        let def = self.wallets().resolve(label)?;
        let signer = self.signer_for(&def)?;
        let addr = signer.address.clone();
        self.signers().borrow_mut().insert(key.to_string(), signer);
        Ok(addr)
    }

    /// Upload contract code to the chain, uploaded by wallet `wallet`, and return its code id.
    ///
    /// One backend-agnostic entry point: pass anything convertible into a [`CwCodeSource`]. A
    /// native `cw-multi-test` contract object ([`crate::CwCode`]) runs on the mock backend,
    /// compiled wasm bytes (`Vec<u8>`) upload on the live RPC backend, and
    /// [`CwCodeSource::both`] carries the two representations so identical deploy code runs on
    /// either backend without branching. A source missing the representation the active backend
    /// needs surfaces as [`CwError::Unimplemented`].
    ///
    /// The mock records the wallet's address as the code creator; the RPC path signs and
    /// broadcasts a `MsgStoreCode` under the process-wide broadcast lock.
    pub async fn store_code(
        &self,
        code: impl Into<CwCodeSource>,
        wallet: WalletLabel<'_>,
    ) -> Result<u64, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(p) => {
                let native = code.into().native.ok_or_else(|| {
                    CwError::Unimplemented(
                        "mock store_code cannot run wasm bytes; provide a native cw-multi-test \
                         contract object (via From<CwCode> or CwCodeSource::both)"
                            .into(),
                    )
                })?;
                Ok(p.store_code(&signer.address, native).await)
            }
            CwChain::Rpc(p) => {
                let wasm = code.into().wasm.ok_or_else(|| {
                    CwError::Unimplemented(
                        "rpc store_code cannot run a native contract object; provide compiled \
                         wasm bytes (via From<Vec<u8>> or CwCodeSource::both)"
                            .into(),
                    )
                })?;
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.store_code(wasm, &signer).await
            }
        }
    }

    /// Instantiate a contract from an uploaded code id, signed by wallet `wallet`.
    pub async fn instantiate<Init: CwSerde>(
        &self,
        code_id: u64,
        init: Init,
        wallet: WalletLabel<'_>,
        funds: &[Coin],
        label: &str,
    ) -> Result<Addr, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(p) => {
                p.instantiate(code_id, init, &signer.address, funds, label)
                    .await
            }
            CwChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.instantiate(code_id, init, &signer, funds, label).await
            }
        }
    }

    /// Execute a state-mutating message against a contract instance, signed by wallet `wallet`.
    ///
    /// The returned [`CwExecution`] carries the broadcast transaction hash on the live RPC
    /// backend (`None` on the in-process mock) alongside the raw execution response.
    pub async fn execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        wallet: WalletLabel<'_>,
        funds: &[Coin],
    ) -> Result<CwExecution, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(p) => p.execute_contract(addr, msg, &signer.address, funds).await,
            CwChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.execute_contract(addr, msg, &signer, funds).await
            }
        }
    }

    /// Run a read-only smart query against a contract instance.
    pub async fn query_wasm_smart<Query: CwSerde, Resp: CwSerde>(
        &self,
        addr: &Addr,
        msg: Query,
    ) -> Result<Resp, CwError> {
        match self {
            CwChain::Mock(p) => p.query_wasm_smart(addr, msg).await,
            CwChain::Rpc(p) => p.query_wasm_smart(addr, msg).await,
        }
    }

    /// Read a raw storage entry from a contract instance by its exact key.
    ///
    /// Returns `Some(bytes)` when the key exists and `None` when it is absent, on both backends.
    pub async fn query_wasm_raw(
        &self,
        addr: &Addr,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, CwError> {
        match self {
            CwChain::Mock(p) => p.query_wasm_raw(addr, key).await,
            CwChain::Rpc(p) => p.query_wasm_raw(addr, key).await,
        }
    }

    /// Dump every raw key-value pair held in a contract's storage, in ascending key order.
    ///
    /// Returns all `(key, value)` entries the contract has written, on both backends.
    pub async fn get_contract_states(
        &self,
        addr: &Addr,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, CwError> {
        match self {
            CwChain::Mock(p) => p.get_contract_states(addr).await,
            CwChain::Rpc(p) => p.get_contract_states(addr).await,
        }
    }

    /// Ensure `who` holds at least `amount` of `asset`.
    ///
    /// Mock native: mints the shortfall. Mock cw20: validates the real balance. RPC native:
    /// validates the real balance (no minting on a live chain) and reports a
    /// [`FundError::Shortfall`] if the account is underfunded. RPC cw20: still
    /// [`FundError::Unimplemented`].
    pub async fn ensure_asset(
        &mut self,
        who: &Addr,
        asset: CwAsset,
        amount: u128,
    ) -> Result<(), FundError> {
        let p = match self {
            CwChain::Mock(p) => p,
            CwChain::Rpc(p) => return p.ensure_asset(who, asset, amount).await,
        };
        match asset {
            CwAsset::Native(denom) => {
                let current = p
                    .app()
                    .wrap()
                    .query_balance(who, &denom)
                    .map_err(|e| FundError::Provider(e.to_string()))?
                    .amount
                    .to_string()
                    .parse::<u128>()
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                if current < amount {
                    p.set_balance(who, &denom, amount)
                        .await
                        .map_err(|e| FundError::Provider(e.to_string()))?;
                }
                Ok(())
            }
            CwAsset::Cw20(contract) => {
                let resp: Cw20BalanceResponse = p
                    .app()
                    .wrap()
                    .query_wasm_smart(
                        &contract,
                        &Cw20BalanceQuery {
                            balance: Cw20BalanceAddress {
                                address: who.clone(),
                            },
                        },
                    )
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                let actual = resp
                    .balance
                    .parse::<u128>()
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                if actual < amount {
                    Err(FundError::Shortfall {
                        asset: format!("cw20:{contract}"),
                        required: amount.to_string(),
                        actual: actual.to_string(),
                    })
                } else {
                    Ok(())
                }
            }
        }
    }
}

impl ChainProvider for CwChain {
    type Spec = CosmosChainInfo;
    type Address = Addr;
    type Account = Addr;
    type Balance = u128;
    type Error = CwError;

    fn chain_info(&self) -> &Self::Spec {
        match self {
            CwChain::Mock(p) => p.chain_info(),
            CwChain::Rpc(p) => p.chain_info(),
        }
    }

    async fn new_account(&mut self, label: &str) -> Addr {
        match self {
            CwChain::Mock(p) => p.new_account(label).await,
            CwChain::Rpc(p) => p.new_account(label).await,
        }
    }

    async fn balance(&self, addr: &Addr) -> Result<u128, CwError> {
        match self {
            CwChain::Mock(p) => p.balance(addr).await,
            CwChain::Rpc(p) => p.balance(addr).await,
        }
    }

    async fn set_balance(&mut self, addr: &Addr, denom: &str, amount: u128) -> Result<(), CwError> {
        match self {
            CwChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            CwChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }

    async fn block_height(&self) -> u64 {
        match self {
            CwChain::Mock(p) => p.block_height().await,
            CwChain::Rpc(p) => p.block_height().await,
        }
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        match self {
            CwChain::Mock(p) => p.advance_blocks(n, time).await,
            CwChain::Rpc(p) => p.advance_blocks(n, time).await,
        }
    }
}
