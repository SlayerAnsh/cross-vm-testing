//! Backend-agnostic EVM chain handle and asset funding.
//!
//! [`EvmChain`] wraps either a mock or an RPC provider and implements [`ChainProvider`]
//! by delegating for chain-level operations. Contract operations use idiomatic methods
//! (`deploy_create`, `call`, `static_call`). [`EvmChain::ensure_asset`] backs the testing
//! environment's funding phase.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Address, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{
    wallet_lock, BlockTime, ChainProvider, ChainSpec, FundError, WalletDeriver, WalletFactory,
    WalletLabel,
};

use crate::asset::EvmAsset;
use crate::chains::EvmChainInfo;
use crate::error::EvmError;
use crate::provider::{
    EvmDeploy, EvmExecution, EvmGas, EvmGasLimit, EvmMockProvider, EvmRpcProvider,
};

/// `balanceOf(address)` selector.
const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// An EVM chain backed by either a mock or an RPC provider.
// Mock holds the full revm state; RPC stub is tiny. Intentional size gap.
#[derive(Clone)]
pub enum EvmChain {
    /// In-process `revm` backend.
    Mock(EvmMockProvider),
    /// Live RPC backend (phase-1 stub).
    Rpc(EvmRpcProvider),
}

impl From<EvmMockProvider> for EvmChain {
    fn from(p: EvmMockProvider) -> Self {
        EvmChain::Mock(p)
    }
}

impl From<EvmRpcProvider> for EvmChain {
    fn from(p: EvmRpcProvider) -> Self {
        EvmChain::Rpc(p)
    }
}

impl EvmChain {
    fn wallets(&self) -> &Rc<WalletFactory> {
        match self {
            EvmChain::Mock(p) => &p.wallets,
            EvmChain::Rpc(p) => &p.wallets,
        }
    }

    fn signers(&self) -> &Rc<RefCell<HashMap<String, PrivateKeySigner>>> {
        match self {
            EvmChain::Mock(p) => &p.signers,
            EvmChain::Rpc(p) => &p.signers,
        }
    }

    /// Resolve a wallet label to its signer (derived once and cached). Broadcast serialization is
    /// handled separately on the RPC path via [`cross_vm_core::wallet_lock`] keyed by the live
    /// account; the in-process mock backend needs no lock.
    async fn acquire<'a>(&self, label: WalletLabel<'a>) -> Result<PrivateKeySigner, EvmError> {
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
    async fn broadcast_guard(
        p: &EvmRpcProvider,
        addr: &Address,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let info = p.chain_info();
        wallet_lock::lock_broadcast(&wallet_lock::lock_key(
            info.kind(),
            info.chain_id(),
            &addr.to_string(),
        ))
        .await
    }

    /// Derive (and cache) a wallet's address without acquiring the broadcast lock. Useful for
    /// funding the wallet in the setup phase or asserting on its address.
    pub async fn wallet_address<'a>(&self, label: WalletLabel<'a>) -> Result<Address, EvmError> {
        let key = label.as_str();
        if let Some(signer) = self.signers().borrow().get(key).cloned() {
            return Ok(self.signer_address(&signer));
        }
        let def = self.wallets().resolve(label)?;
        let signer = self.signer_for(&def)?;
        let addr = self.signer_address(&signer);
        self.signers().borrow_mut().insert(key.to_string(), signer);
        Ok(addr)
    }

    /// Deploy bytecode via a create transaction signed by wallet `wallet`, returning the new
    /// contract address plus the transaction hash.
    ///
    /// `limit` is required (see [`EvmGasLimit`]). Resolving an
    /// [`EvmGasLimit::Estimated`](EvmGasLimit::Estimated) happens inside the provider, under the
    /// broadcast guard on the RPC path: safe because an estimate signs and broadcasts nothing, so
    /// it never reaches for that lock (see [`estimate_deploy_create`](Self::estimate_deploy_create)).
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
        limit: EvmGasLimit,
    ) -> Result<EvmDeploy, EvmError> {
        let signer = self.acquire(wallet).await?;
        let addr = self.signer_address(&signer);
        match self {
            EvmChain::Mock(p) => {
                p.deploy_create(bytecode, constructor_args, &addr, limit)
                    .await
            }
            EvmChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, &addr).await;
                p.deploy_create(bytecode, constructor_args, &signer, limit)
                    .await
            }
        }
    }

    /// Forecast what a [`deploy_create`](Self::deploy_create) of this bytecode would cost, without
    /// deploying it.
    ///
    /// An estimate signs and broadcasts nothing, so it resolves the wallet's address without taking
    /// the broadcast lock, and leaves the chain able to run the very op it forecast. A deploy that
    /// would revert is an error here, not a gas number.
    ///
    /// Both backends report `used`; only the RPC backend reports a `fee` (see [`EvmGas::fee`]).
    pub async fn estimate_deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
    ) -> Result<EvmGas, EvmError> {
        let addr = self.wallet_address(wallet).await?;
        match self {
            EvmChain::Mock(p) => {
                p.estimate_deploy_create(bytecode, constructor_args, &addr)
                    .await
            }
            EvmChain::Rpc(p) => {
                p.estimate_deploy_create(bytecode, constructor_args, &addr)
                    .await
            }
        }
    }

    /// Forecast what a [`call`](Self::call) would cost (see
    /// [`estimate_call_value`](Self::estimate_call_value)).
    pub async fn estimate_call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
    ) -> Result<EvmGas, EvmError> {
        self.estimate_call_value(to, calldata, wallet, U256::ZERO)
            .await
    }

    /// Forecast what a [`call_value`](Self::call_value) would cost, without executing it. A call
    /// that would revert is an error here, not a gas number (see
    /// [`estimate_deploy_create`](Self::estimate_deploy_create)).
    pub async fn estimate_call_value(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
        value: U256,
    ) -> Result<EvmGas, EvmError> {
        let addr = self.wallet_address(wallet).await?;
        match self {
            EvmChain::Mock(p) => p.estimate_call_value(to, calldata, &addr, value).await,
            EvmChain::Rpc(p) => p.estimate_call_value(to, calldata, &addr, value).await,
        }
    }

    /// Execute a state-mutating call against `to`, signed by wallet `wallet`.
    pub async fn call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
        limit: EvmGasLimit,
    ) -> Result<EvmExecution, EvmError> {
        self.call_value(to, calldata, wallet, U256::ZERO, limit)
            .await
    }

    /// Execute a state-mutating call against `to` carrying `value` wei (a payable call), signed by
    /// wallet `wallet`. On the mock the caller's balance is topped up to cover `value`.
    ///
    /// `limit` is required (see [`deploy_create`](Self::deploy_create)).
    pub async fn call_value(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
        value: U256,
        limit: EvmGasLimit,
    ) -> Result<EvmExecution, EvmError> {
        let signer = self.acquire(wallet).await?;
        let addr = self.signer_address(&signer);
        match self {
            EvmChain::Mock(p) => p.call_value(to, calldata, &addr, value, limit).await,
            EvmChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, &addr).await;
                p.call_value(to, calldata, &signer, value, limit).await
            }
        }
    }

    /// Transfer `amount` base units (wei) of the chain's native token from wallet `wallet` to `to`,
    /// returning the transaction hash (`0x`-prefixed lowercase hex).
    ///
    /// `denom` must name this chain's native token (case-insensitively); there is no ERC-20 path
    /// here. The mock returns its synthetic hash (see [`EvmExecution::tx_hash`]), the RPC backend
    /// the real broadcast hash.
    pub async fn transfer_funds(
        &self,
        to: &Address,
        denom: &str,
        amount: U256,
        wallet: WalletLabel<'_>,
        limit: EvmGasLimit,
    ) -> Result<String, EvmError> {
        let native = self.chain_info().native_symbol;
        if !denom.eq_ignore_ascii_case(native) {
            return Err(EvmError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{native}'"
            )));
        }
        let signer = self.acquire(wallet).await?;
        let addr = self.signer_address(&signer);
        // A native transfer is just a value-carrying call with empty calldata.
        let exec = match self {
            EvmChain::Mock(p) => p.transfer_funds(to, &addr, amount, limit).await?,
            EvmChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, &addr).await;
                p.call_value(to, [], &signer, amount, limit).await?
            }
        };
        Ok(exec.tx_hash.to_string())
    }

    /// Run a read-only static call against `to`.
    pub async fn static_call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, EvmError> {
        match self {
            EvmChain::Mock(p) => p.static_call(to, calldata).await,
            EvmChain::Rpc(p) => p.static_call(to, calldata).await,
        }
    }

    /// Read the raw storage value at `slot` for `addr`.
    pub async fn get_storage_at(&self, addr: &Address, slot: U256) -> Result<U256, EvmError> {
        match self {
            EvmChain::Mock(p) => p.get_storage_at(addr, slot).await,
            EvmChain::Rpc(p) => p.get_storage_at(addr, slot).await,
        }
    }

    /// Ensure `who` holds at least `amount` of `asset`.
    ///
    /// Mock native: mints the shortfall. Mock ERC-20: validates `balanceOf`. RPC native:
    /// validates the real balance (no minting on a live chain) and reports a
    /// [`FundError::Shortfall`] if underfunded. RPC ERC-20: still
    /// [`FundError::Unimplemented`].
    pub async fn ensure_asset(
        &mut self,
        who: &Address,
        asset: EvmAsset,
        amount: U256,
    ) -> Result<(), FundError> {
        let p = match self {
            EvmChain::Mock(p) => p,
            EvmChain::Rpc(p) => return p.ensure_asset(who, asset, amount).await,
        };
        match asset {
            EvmAsset::Native => {
                let current = p
                    .balance(who)
                    .await
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                if current < amount {
                    let denom = p.chain_info().native_symbol;
                    p.set_balance(who, denom, amount)
                        .await
                        .map_err(|e| FundError::Provider(e.to_string()))?;
                }
                Ok(())
            }
            EvmAsset::Erc20(token) => {
                // calldata = selector ++ 32-byte left-padded holder address
                let mut data = Vec::with_capacity(36);
                data.extend_from_slice(&BALANCE_OF_SELECTOR);
                data.extend_from_slice(&[0u8; 12]);
                data.extend_from_slice(who.as_slice());
                let out = p
                    .static_call(&token, Bytes::from(data))
                    .await
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                let actual = if out.len() >= 32 {
                    U256::from_be_slice(&out[..32])
                } else {
                    U256::ZERO
                };
                if actual < amount {
                    Err(FundError::Shortfall {
                        asset: format!("erc20:{token}"),
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

impl ChainProvider for EvmChain {
    type Spec = EvmChainInfo;
    type Address = Address;
    type Account = Address;
    type Balance = U256;
    type Error = EvmError;

    fn chain_info(&self) -> &Self::Spec {
        match self {
            EvmChain::Mock(p) => p.chain_info(),
            EvmChain::Rpc(p) => p.chain_info(),
        }
    }

    async fn new_account(&mut self, label: &str) -> Address {
        match self {
            EvmChain::Mock(p) => p.new_account(label).await,
            EvmChain::Rpc(p) => p.new_account(label).await,
        }
    }

    async fn balance(&self, addr: &Address) -> Result<U256, EvmError> {
        match self {
            EvmChain::Mock(p) => p.balance(addr).await,
            EvmChain::Rpc(p) => p.balance(addr).await,
        }
    }

    async fn set_balance(
        &mut self,
        addr: &Address,
        denom: &str,
        amount: U256,
    ) -> Result<(), EvmError> {
        match self {
            EvmChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            EvmChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }

    async fn block_height(&self) -> u64 {
        match self {
            EvmChain::Mock(p) => p.block_height().await,
            EvmChain::Rpc(p) => p.block_height().await,
        }
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        match self {
            EvmChain::Mock(p) => p.advance_blocks(n, time).await,
            EvmChain::Rpc(p) => p.advance_blocks(n, time).await,
        }
    }
}
