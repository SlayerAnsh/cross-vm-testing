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
use crate::provider::{
    CwCodeSource, CwExecution, CwGas, CwGasLimit, CwInstantiate, CwMigrate, CwMockProvider,
    CwRpcProvider, CwStoreCode,
};
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

    /// Upload contract code to the chain, uploaded by wallet `wallet`, and return its code id
    /// plus the transaction hash.
    ///
    /// One backend-agnostic entry point: pass anything convertible into a [`CwCodeSource`]. A
    /// native `cw-multi-test` contract object ([`crate::CwCode`]) runs on the mock backend,
    /// compiled wasm bytes (`Vec<u8>`) upload on the live RPC backend, and
    /// [`CwCodeSource::both`] carries the two representations so identical deploy code runs on
    /// either backend without branching. A source missing the representation the active backend
    /// needs surfaces as [`CwError::Unimplemented`].
    ///
    /// The mock records the wallet's address as the code creator and mints a synthetic tx hash;
    /// the RPC path signs and broadcasts a `MsgStoreCode` under the process-wide broadcast lock
    /// and reports the real one.
    ///
    /// `gas` is required, as on every mutating op. [`CwGasLimit::Estimated`] resolves inside the
    /// RPC path, which already holds the broadcast lock: it simulates through the *provider*'s
    /// unlocked estimator, not through [`Self::estimate_store_code`], which would re-enter this
    /// account's lock and deadlock. It is inert on the mock, which cannot run out of gas.
    pub async fn store_code(
        &self,
        code: impl Into<CwCodeSource>,
        wallet: WalletLabel<'_>,
        gas: CwGasLimit,
    ) -> Result<CwStoreCode, CwError> {
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
                Ok(p.store_code(&signer.address, native, gas).await)
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
                p.store_code(wasm, &signer, gas).await
            }
        }
    }

    /// Transfer `amount` base units of the bank denom `denom` from wallet `wallet` to `to`, and
    /// return the transaction hash.
    ///
    /// Any bank denom moves verbatim (`uosmo`, `ibc/...`), not just the chain's native denom. An
    /// underfunded sender surfaces as [`CwError::Execute`] on both backends.
    ///
    /// The mock performs a real bank send inside its in-process `App` and returns a synthetic,
    /// deterministic hash in the same textual shape a live node uses; the RPC path signs and
    /// broadcasts a `MsgSend` under the process-wide broadcast lock.
    ///
    /// `gas` is required and behaves as on [`Self::store_code`].
    pub async fn transfer_funds(
        &self,
        to: &Addr,
        denom: &str,
        amount: u128,
        wallet: WalletLabel<'_>,
        gas: CwGasLimit,
    ) -> Result<String, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(p) => {
                p.transfer_funds(to, denom, amount, &signer.address, gas)
                    .await
            }
            CwChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.transfer_funds(to, denom, amount, &signer, gas).await
            }
        }
    }

    /// Instantiate a contract from an uploaded code id under `gas`, signed by wallet `wallet`, and
    /// return the new instance's address plus the transaction hash.
    ///
    /// `gas` is required and behaves as on [`Self::store_code`].
    pub async fn instantiate<Init: CwSerde>(
        &self,
        code_id: u64,
        init: Init,
        wallet: WalletLabel<'_>,
        funds: &[Coin],
        label: &str,
        gas: CwGasLimit,
    ) -> Result<CwInstantiate, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(p) => {
                p.instantiate(code_id, init, &signer.address, funds, label, gas)
                    .await
            }
            CwChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.instantiate(code_id, init, &signer, funds, label, gas)
                    .await
            }
        }
    }

    /// Execute a state-mutating message against a contract instance under `gas`, signed by wallet
    /// `wallet`.
    ///
    /// The returned [`CwExecution`] carries the transaction hash (the broadcast one on the live
    /// RPC backend, a synthetic one on the in-process mock) alongside the raw execution response.
    ///
    /// `gas` is required and behaves as on [`Self::store_code`].
    pub async fn execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        wallet: WalletLabel<'_>,
        funds: &[Coin],
        gas: CwGasLimit,
    ) -> Result<CwExecution, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(p) => {
                p.execute_contract(addr, msg, &signer.address, funds, gas)
                    .await
            }
            CwChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.execute_contract(addr, msg, &signer, funds, gas).await
            }
        }
    }

    /// Migrate a contract to `new_code_id` under `gas`, signed by wallet `wallet`, running the new
    /// code's `migrate` entry point with `msg`, and return the transaction hash plus what the
    /// migration cost.
    ///
    /// `wallet` must be the contract's admin (both backends enforce it): the mock migrates inside
    /// its in-process `App` and returns a synthetic hash; the RPC path signs and broadcasts a
    /// `MsgMigrateContract` under the process-wide broadcast lock.
    ///
    /// `gas` is required and behaves as on [`Self::store_code`].
    pub async fn migrate_contract<Migrate: CwSerde>(
        &self,
        contract: &Addr,
        new_code_id: u64,
        msg: Migrate,
        wallet: WalletLabel<'_>,
        gas: CwGasLimit,
    ) -> Result<CwMigrate, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(p) => {
                p.migrate_contract(contract, new_code_id, msg, &signer.address, gas)
                    .await
            }
            CwChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.migrate_contract(contract, new_code_id, msg, &signer, gas)
                    .await
            }
        }
    }

    /// The hex-encoded sha256 checksum of the wasm code behind `code_id` (lowercase hex on both
    /// backends): wasmd's `data_hash` on the live RPC path, `cw-multi-test`'s code checksum on the
    /// mock.
    pub async fn code_checksum(&self, code_id: u64) -> Result<String, CwError> {
        match self {
            CwChain::Mock(p) => p.code_checksum(code_id).await,
            CwChain::Rpc(p) => p.code_checksum(code_id).await,
        }
    }

    /// Sign and broadcast a caller-assembled set of `msgs` under `gas` and `memo`, signed by
    /// wallet `wallet`, returning the transaction hash, what it cost, and its emitted events.
    ///
    /// The framework's escape hatch for module messages the typed paths do not wrap: pass raw
    /// protobuf [`cosmrs::Any`] messages and they broadcast through the same signing and gas
    /// resolution the typed write paths use ([`CwGasLimit::Estimated`] simulates the exact
    /// `msgs`). Live RPC only: the mock builds no Cosmos transactions, so it returns
    /// [`CwError::Unimplemented`].
    pub async fn sign_and_broadcast(
        &self,
        msgs: Vec<cosmrs::Any>,
        wallet: WalletLabel<'_>,
        gas: CwGasLimit,
        memo: &str,
    ) -> Result<CwExecution, CwError> {
        let signer = self.acquire(wallet).await?;
        match self {
            CwChain::Mock(_) => Err(CwError::Unimplemented(
                "mock sign_and_broadcast: the in-process backend builds no Cosmos transactions; \
                 raw message broadcast requires the live RPC backend"
                    .into(),
            )),
            CwChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                p.sign_and_broadcast_msgs(msgs, &signer, gas, memo).await
            }
        }
    }

    // ----- Estimation: gas forecasts without broadcasting. -----
    //
    // Every `estimate_*` mirrors its mutating sibling's shape and reports the same type a
    // receipt does, `Option<CwGas>`, so a forecast and the receipt it forecasts are directly
    // comparable. The RPC backend simulates the exact message it would broadcast
    // (`/cosmos.tx.v1beta1.Service/Simulate`): `used` is the node's raw simulated figure and
    // `fee` is what a broadcast under `CwGasLimit::Estimated` would actually declare and pay
    // (the adjusted limit priced at the chain's `gas_price`; see `estimated_gas` in the RPC
    // provider). The mock reports `None` because `cw-multi-test` has no gas meter, so there is
    // nothing to simulate against and no honest figure to fabricate (the same rule as the `gas`
    // field on the op results; see `NO_GAS_METER` on the mock provider).
    //
    // The RPC arms hold the per-account broadcast lock: the simulated tx carries the account's
    // current on-chain sequence, so racing an in-flight broadcast from the same account could
    // capture a sequence mid-bump.
    //
    // The provider's own `estimate_*` methods deliberately do not lock, and that asymmetry is
    // load-bearing: it is what lets a write path, which already holds this account's lock, resolve
    // its own `CwGasLimit::Estimated` by simulating in place. The write paths therefore call the
    // provider, never these methods. Calling one of these from inside a write path would re-enter
    // a lock the same task already holds, which is a deadlock, not a wait.

    /// Estimate what [`Self::store_code`] would cost, without broadcasting anything.
    /// `Some(CwGas)` on the RPC backend, `None` on the mock (which cannot meter).
    pub async fn estimate_store_code(
        &self,
        code: impl Into<CwCodeSource>,
        wallet: WalletLabel<'_>,
    ) -> Result<Option<CwGas>, CwError> {
        match self {
            CwChain::Mock(_) => Ok(None),
            CwChain::Rpc(p) => {
                let wasm = code.into().wasm.ok_or_else(|| {
                    CwError::Unimplemented(
                        "rpc estimate_store_code cannot simulate a native contract object; \
                         provide compiled wasm bytes (via From<Vec<u8>> or CwCodeSource::both)"
                            .into(),
                    )
                })?;
                let signer = self.acquire(wallet).await?;
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                Ok(Some(p.estimate_store_code(wasm, &signer).await?))
            }
        }
    }

    /// Estimate what [`Self::instantiate`] would cost, without broadcasting anything.
    /// `Some(CwGas)` on the RPC backend, `None` on the mock (which cannot meter).
    pub async fn estimate_instantiate<Init: CwSerde>(
        &self,
        code_id: u64,
        init: Init,
        wallet: WalletLabel<'_>,
        funds: &[Coin],
        label: &str,
    ) -> Result<Option<CwGas>, CwError> {
        match self {
            CwChain::Mock(_) => Ok(None),
            CwChain::Rpc(p) => {
                let signer = self.acquire(wallet).await?;
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                Ok(Some(
                    p.estimate_instantiate(code_id, init, &signer, funds, label)
                        .await?,
                ))
            }
        }
    }

    /// Estimate what [`Self::execute_contract`] would cost, without broadcasting anything.
    /// `Some(CwGas)` on the RPC backend, `None` on the mock (which cannot meter).
    pub async fn estimate_execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        wallet: WalletLabel<'_>,
        funds: &[Coin],
    ) -> Result<Option<CwGas>, CwError> {
        match self {
            CwChain::Mock(_) => Ok(None),
            CwChain::Rpc(p) => {
                let signer = self.acquire(wallet).await?;
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                Ok(Some(
                    p.estimate_execute_contract(addr, msg, &signer, funds)
                        .await?,
                ))
            }
        }
    }

    /// Estimate what [`Self::migrate_contract`] would cost, without broadcasting anything.
    /// `Some(CwGas)` on the RPC backend, `None` on the mock (which cannot meter).
    pub async fn estimate_migrate_contract<Migrate: CwSerde>(
        &self,
        contract: &Addr,
        new_code_id: u64,
        msg: Migrate,
        wallet: WalletLabel<'_>,
    ) -> Result<Option<CwGas>, CwError> {
        match self {
            CwChain::Mock(_) => Ok(None),
            CwChain::Rpc(p) => {
                let signer = self.acquire(wallet).await?;
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                Ok(Some(
                    p.estimate_migrate_contract(contract, new_code_id, msg, &signer)
                        .await?,
                ))
            }
        }
    }

    /// Estimate what [`Self::transfer_funds`] would cost, without broadcasting anything.
    /// `Some(CwGas)` on the RPC backend, `None` on the mock (which cannot meter).
    pub async fn estimate_transfer_funds(
        &self,
        to: &Addr,
        denom: &str,
        amount: u128,
        wallet: WalletLabel<'_>,
    ) -> Result<Option<CwGas>, CwError> {
        match self {
            CwChain::Mock(_) => Ok(None),
            CwChain::Rpc(p) => {
                let signer = self.acquire(wallet).await?;
                let _g = Self::broadcast_guard(p, signer.address.as_str()).await;
                Ok(Some(
                    p.estimate_transfer_funds(to, denom, amount, &signer)
                        .await?,
                ))
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
