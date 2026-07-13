//! Backend-agnostic Tron chain handle and asset funding.
//!
//! [`TronChain`] wraps either a mock or an RPC provider and implements [`ChainProvider`] by
//! delegating for chain-level operations. Contract operations use idiomatic methods
//! (`deploy_create`, `call`, `static_call`). [`TronChain::ensure_asset`] backs the testing
//! environment's funding phase.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::{Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{
    wallet_lock, BlockTime, ChainProvider, ChainSpec, FundError, WalletDeriver, WalletFactory,
    WalletLabel,
};
use tokio::sync::OwnedMutexGuard;

use crate::asset::TronAsset;
use crate::chains::TronChainInfo;
use crate::error::TronError;
use crate::provider::address::TronAddress;
use crate::provider::{TronExecution, TronMockProvider, TronRpcProvider};

/// `balanceOf(address)` selector (TRC20 is ERC20-shaped, same selector).
const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// A Tron chain backed by either a mock or an RPC provider.
// Mock holds the full revm state; RPC stub is tiny. Intentional size gap.
#[derive(Clone)]
pub enum TronChain {
    /// In-process `revm`-based TVM backend.
    Mock(TronMockProvider),
    /// Live java-tron RPC backend (stub parity in v1).
    Rpc(TronRpcProvider),
}

impl From<TronMockProvider> for TronChain {
    fn from(p: TronMockProvider) -> Self {
        TronChain::Mock(p)
    }
}

impl From<TronRpcProvider> for TronChain {
    fn from(p: TronRpcProvider) -> Self {
        TronChain::Rpc(p)
    }
}

impl TronChain {
    fn wallets(&self) -> &Rc<WalletFactory> {
        match self {
            TronChain::Mock(p) => &p.wallets,
            TronChain::Rpc(p) => &p.wallets,
        }
    }

    fn signers(&self) -> &Rc<RefCell<HashMap<String, PrivateKeySigner>>> {
        match self {
            TronChain::Mock(p) => &p.signers,
            TronChain::Rpc(p) => &p.signers,
        }
    }

    /// Resolve a wallet label to its signer (derived once and cached). Broadcast serialization is
    /// handled separately on the RPC path via [`cross_vm_core::wallet_lock`] keyed by the live
    /// account; the in-process mock backend needs no lock.
    async fn acquire<'a>(&self, label: WalletLabel<'a>) -> Result<PrivateKeySigner, TronError> {
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
    async fn broadcast_guard(p: &TronRpcProvider, addr: &TronAddress) -> OwnedMutexGuard<()> {
        let info = p.chain_info();
        wallet_lock::lock_broadcast(&wallet_lock::lock_key(
            info.kind(),
            info.chain_id(),
            &addr.to_string(),
        ))
        .await
    }

    /// Derive (and cache) a wallet's address without acquiring the broadcast lock.
    pub async fn wallet_address<'a>(
        &self,
        label: WalletLabel<'a>,
    ) -> Result<TronAddress, TronError> {
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

    /// Deploy bytecode via a create transaction signed by wallet `wallet`.
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
    ) -> Result<TronAddress, TronError> {
        let signer = self.acquire(wallet).await?;
        let addr = self.signer_address(&signer);
        match self {
            TronChain::Mock(p) => p.deploy_create(bytecode, constructor_args, &addr).await,
            TronChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, &addr).await;
                p.deploy_create(bytecode, constructor_args, &signer).await
            }
        }
    }

    /// Execute a state-mutating call against `to`, signed by wallet `wallet`.
    pub async fn call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
    ) -> Result<TronExecution, TronError> {
        self.call_value(to, calldata, wallet, U256::ZERO).await
    }

    /// Execute a state-mutating call against `to` carrying `value` sun (a payable call), signed by
    /// wallet `wallet`. On the mock the caller's balance is topped up to cover `value`.
    pub async fn call_value(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        wallet: WalletLabel<'_>,
        value: U256,
    ) -> Result<TronExecution, TronError> {
        let signer = self.acquire(wallet).await?;
        let addr = self.signer_address(&signer);
        match self {
            TronChain::Mock(p) => p.call_value(to, calldata, &addr, value).await,
            TronChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, &addr).await;
                p.call_value(to, calldata, &signer, value).await
            }
        }
    }

    /// Transfer `amount` base units (sun) of `denom` from wallet `wallet` to `to`, returning the
    /// transaction hash as unprefixed hex (the shape java-tron renders a `txID` in).
    ///
    /// `denom` must name this chain's native token (`TRX`, case-insensitively), matching
    /// [`ChainProvider::set_balance`]; a TRC20 transfer goes through the token contract's
    /// [`call`](Self::call). A sender that cannot cover `amount` is an error on both backends.
    pub async fn transfer_funds(
        &self,
        to: &TronAddress,
        denom: &str,
        amount: u64,
        wallet: WalletLabel<'_>,
    ) -> Result<String, TronError> {
        let symbol = self.chain_info().native_symbol;
        if !denom.eq_ignore_ascii_case(symbol) {
            return Err(TronError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{symbol}'"
            )));
        }
        let signer = self.acquire(wallet).await?;
        let addr = self.signer_address(&signer);
        match self {
            TronChain::Mock(p) => p.transfer_funds(to, amount, &addr).await,
            TronChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, &addr).await;
                p.transfer_funds(to, amount, &signer).await
            }
        }
    }

    /// Run a read-only static call against `to`.
    pub async fn static_call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, TronError> {
        match self {
            TronChain::Mock(p) => p.static_call(to, calldata).await,
            TronChain::Rpc(p) => p.static_call(to, calldata).await,
        }
    }

    /// Read the raw storage value at `slot` for `addr`.
    pub async fn get_storage_at(&self, addr: &TronAddress, slot: U256) -> Result<U256, TronError> {
        match self {
            TronChain::Mock(p) => p.get_storage_at(addr, slot).await,
            TronChain::Rpc(p) => p.get_storage_at(addr, slot).await,
        }
    }

    /// Ensure `who` holds at least `amount` (sun, or token base units) of `asset`.
    ///
    /// Mock native: mints the shortfall. Mock TRC20: validates `balanceOf`. RPC native:
    /// validates the real balance (no minting on a live chain). RPC TRC20: still
    /// [`FundError::Unimplemented`].
    pub async fn ensure_asset(
        &mut self,
        who: &TronAddress,
        asset: TronAsset,
        amount: u64,
    ) -> Result<(), FundError> {
        let p = match self {
            TronChain::Mock(p) => p,
            TronChain::Rpc(p) => {
                return match asset {
                    TronAsset::Native => {
                        let current = p
                            .balance(who)
                            .await
                            .map_err(|e| FundError::Provider(e.to_string()))?;
                        if current < amount {
                            Err(FundError::Shortfall {
                                asset: p.chain_info().native_symbol.to_string(),
                                required: amount.to_string(),
                                actual: current.to_string(),
                            })
                        } else {
                            Ok(())
                        }
                    }
                    TronAsset::Trc20(_) => {
                        Err(FundError::Unimplemented("tron rpc trc20 funding".into()))
                    }
                };
            }
        };
        match asset {
            TronAsset::Native => {
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
            TronAsset::Trc20(token) => {
                // calldata = selector ++ 32-byte left-padded holder address (low 20 bytes)
                let mut data = Vec::with_capacity(36);
                data.extend_from_slice(&BALANCE_OF_SELECTOR);
                data.extend_from_slice(&[0u8; 12]);
                data.extend_from_slice(who.as_evm().as_slice());
                let out = p
                    .static_call(&token, Bytes::from(data))
                    .await
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                let actual = if out.len() >= 32 {
                    U256::from_be_slice(&out[..32])
                } else {
                    U256::ZERO
                };
                if actual < U256::from(amount) {
                    Err(FundError::Shortfall {
                        asset: format!("trc20:{}", token.to_base58()),
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

impl ChainProvider for TronChain {
    type Spec = TronChainInfo;
    type Address = TronAddress;
    type Account = TronAddress;
    type Balance = u64;
    type Error = TronError;

    fn chain_info(&self) -> &Self::Spec {
        match self {
            TronChain::Mock(p) => p.chain_info(),
            TronChain::Rpc(p) => p.chain_info(),
        }
    }

    async fn new_account(&mut self, label: &str) -> TronAddress {
        match self {
            TronChain::Mock(p) => p.new_account(label).await,
            TronChain::Rpc(p) => p.new_account(label).await,
        }
    }

    async fn balance(&self, addr: &TronAddress) -> Result<u64, TronError> {
        match self {
            TronChain::Mock(p) => p.balance(addr).await,
            TronChain::Rpc(p) => p.balance(addr).await,
        }
    }

    async fn set_balance(
        &mut self,
        addr: &TronAddress,
        denom: &str,
        amount: u64,
    ) -> Result<(), TronError> {
        match self {
            TronChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            TronChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }

    async fn block_height(&self) -> u64 {
        match self {
            TronChain::Mock(p) => p.block_height().await,
            TronChain::Rpc(p) => p.block_height().await,
        }
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        match self {
            TronChain::Mock(p) => p.advance_blocks(n, time).await,
            TronChain::Rpc(p) => p.advance_blocks(n, time).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chains::LOCAL;
    use crate::tvm::resources::SUN_PER_TRX;
    use cross_vm_macros::define_wallet_roster;
    use std::rc::Rc;

    define_wallet_roster! {
        pub const TEST_WALLETS: TestWallets = {
            alice: auto @ 0,
            bob: auto @ 1,
        };
    }

    /// A mock chain carrying the test roster (its `auto` wallets start unfunded).
    fn chain_with_wallets() -> TronChain {
        TronChain::from(LOCAL.mock(Rc::new(
            WalletFactory::from_roster(TestWallets::SPECS).unwrap(),
        )))
    }

    #[tokio::test]
    async fn mock_chain_funds_account() {
        let mut chain =
            TronChain::from(LOCAL.mock(Rc::new(WalletFactory::from_roster(&[]).unwrap())));
        let a = chain.new_account("alice").await;
        assert!(a.to_base58().starts_with('T'));
        assert!(chain.balance(&a).await.unwrap() > 0);
    }

    #[tokio::test]
    async fn get_storage_at_plumbs_through_chain() {
        // Exercise the `TronChain` enum dispatch: an unset slot reads as zero.
        let mut chain =
            TronChain::from(LOCAL.mock(Rc::new(WalletFactory::from_roster(&[]).unwrap())));
        let a = chain.new_account("alice").await;
        assert_eq!(
            chain.get_storage_at(&a, U256::ZERO).await.unwrap(),
            U256::ZERO
        );
    }

    #[tokio::test]
    async fn native_ensure_asset_mints_on_mock() {
        let mut chain =
            TronChain::from(LOCAL.mock(Rc::new(WalletFactory::from_roster(&[]).unwrap())));
        let a = chain.new_account("bob").await;
        let huge = crate::DEFAULT_FUNDING_SUN * 2;
        chain
            .ensure_asset(&a, TronAsset::Native, huge)
            .await
            .unwrap();
        assert!(chain.balance(&a).await.unwrap() >= huge);
    }

    #[tokio::test]
    async fn transfer_funds_moves_native_balance_on_mock() {
        let mut chain = chain_with_wallets();
        let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
        let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
        chain
            .set_balance(&alice, "TRX", 10 * SUN_PER_TRX)
            .await
            .unwrap();

        let hash = chain
            .transfer_funds(&bob, "TRX", 4 * SUN_PER_TRX, TEST_WALLETS.alice)
            .await
            .expect("native transfer succeeds");

        // The mock reports the core's synthetic hash in the RPC arm's textual shape: a 32-byte
        // hash as unprefixed hex.
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(chain.balance(&bob).await.unwrap(), 4 * SUN_PER_TRX);
        assert_eq!(chain.balance(&alice).await.unwrap(), 6 * SUN_PER_TRX);
    }

    #[tokio::test]
    async fn transfer_funds_rejects_unknown_denom() {
        let mut chain = chain_with_wallets();
        let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
        let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
        chain
            .set_balance(&alice, "TRX", 10 * SUN_PER_TRX)
            .await
            .unwrap();

        let err = chain
            .transfer_funds(&bob, "BTC", SUN_PER_TRX, TEST_WALLETS.alice)
            .await
            .expect_err("BTC is not this chain's native token");
        assert!(matches!(err, TronError::Balance(_)));
        assert_eq!(chain.balance(&bob).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn transfer_funds_rejects_insufficient_balance() {
        // The mock mints native funds on demand for a payable `call_value`; a transfer must NOT,
        // so an underfunded sender errors instead of silently minting the shortfall.
        let mut chain = chain_with_wallets();
        let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
        let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
        chain.set_balance(&alice, "TRX", SUN_PER_TRX).await.unwrap();

        let err = chain
            .transfer_funds(&bob, "TRX", 5 * SUN_PER_TRX, TEST_WALLETS.alice)
            .await
            .expect_err("alice cannot cover 5 TRX");
        assert!(matches!(err, TronError::Balance(_)));
        assert_eq!(chain.balance(&alice).await.unwrap(), SUN_PER_TRX);
        assert_eq!(chain.balance(&bob).await.unwrap(), 0);
    }
}
