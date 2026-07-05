//! Backend-agnostic Solana chain handle and asset funding.
//!
//! [`SvmChain`] wraps either a mock or an RPC provider and implements [`ChainProvider`]
//! by delegating for chain-level operations. Program operations use idiomatic methods
//! (`add_program`, `send_transaction`, `get_account`). [`SvmChain::ensure_asset`] backs
//! the testing environment's funding phase.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cross_vm_core::{
    wallet_lock, BlockTime, ChainProvider, ChainSpec, FundError, WalletDeriver, WalletFactory,
    WalletLabel,
};
use litesvm::types::TransactionMetadata;
use solana_account::Account;
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;

use crate::asset::SvmAsset;
use crate::chains::SolanaChainInfo;
use crate::error::SvmError;
use crate::provider::{SvmMockProvider, SvmRpcProvider};
use crate::wallet::SvmSigner;

/// Byte offset of the `amount` field (u64 LE) in an SPL token account.
const SPL_AMOUNT_OFFSET: usize = 64;

/// A Solana chain backed by either a mock or an RPC provider.
// Mock holds the full litesvm state; RPC stub is tiny. Intentional size gap.
#[derive(Clone)]
pub enum SvmChain {
    /// In-process `litesvm` backend.
    Mock(SvmMockProvider),
    /// Live RPC backend (phase-1 stub).
    Rpc(SvmRpcProvider),
}

impl From<SvmMockProvider> for SvmChain {
    fn from(p: SvmMockProvider) -> Self {
        SvmChain::Mock(p)
    }
}

impl From<SvmRpcProvider> for SvmChain {
    fn from(p: SvmRpcProvider) -> Self {
        SvmChain::Rpc(p)
    }
}

impl SvmChain {
    fn wallets(&self) -> &Rc<WalletFactory> {
        match self {
            SvmChain::Mock(p) => &p.wallets,
            SvmChain::Rpc(p) => &p.wallets,
        }
    }

    fn signers(&self) -> &Rc<RefCell<HashMap<String, SvmSigner>>> {
        match self {
            SvmChain::Mock(p) => &p.signers,
            SvmChain::Rpc(p) => &p.signers,
        }
    }

    /// Resolve a wallet label to its signer (derived once and cached). Broadcast serialization is
    /// handled separately on the RPC path via [`cross_vm_core::wallet_lock`] keyed by the live
    /// account; the in-process mock backend needs no lock.
    async fn acquire<'a>(&self, label: WalletLabel<'a>) -> Result<SvmSigner, SvmError> {
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

    /// Acquire the global broadcast lock for `addr` on this RPC cluster, keyed by `(chain, address)`
    /// so the same live account serializes process-wide. Held across the whole send -> confirm.
    async fn broadcast_guard(
        p: &SvmRpcProvider,
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

    /// Derive (and cache) a wallet's pubkey without acquiring the broadcast lock. Useful for
    /// funding the wallet in the setup phase or asserting on its address.
    pub async fn wallet_address<'a>(&self, label: WalletLabel<'a>) -> Result<Address, SvmError> {
        let key = label.as_str();
        if let Some(signer) = self.signers().borrow().get(key).cloned() {
            return Ok(signer.pubkey());
        }
        let def = self.wallets().resolve(label)?;
        let signer = self.signer_for(&def)?;
        let pubkey = signer.pubkey();
        self.signers().borrow_mut().insert(key.to_string(), signer);
        Ok(pubkey)
    }

    /// Load a program into the chain and return its program id.
    pub async fn add_program(&self, bytecode: Vec<u8>) -> Result<Address, SvmError> {
        match self {
            SvmChain::Mock(p) => p.add_program(bytecode).await,
            SvmChain::Rpc(p) => p.add_program(bytecode).await,
        }
    }

    /// Load a program at a specific program id (required for Anchor's `declare_id!`).
    pub async fn add_program_at(
        &self,
        program_id: Address,
        bytecode: Vec<u8>,
    ) -> Result<Address, SvmError> {
        match self {
            SvmChain::Mock(p) => p.add_program_at(program_id, bytecode).await,
            SvmChain::Rpc(p) => p.add_program_at(program_id, bytecode).await,
        }
    }

    /// Sign and send a transaction built from `instructions`, signed by wallet `wallet`.
    pub async fn send_transaction(
        &self,
        instructions: impl AsRef<[Instruction]>,
        wallet: WalletLabel<'_>,
    ) -> Result<TransactionMetadata, SvmError> {
        let signer = self.acquire(wallet).await?;
        match self {
            SvmChain::Mock(p) => p.send_transaction(instructions, signer.keypair()).await,
            SvmChain::Rpc(p) => {
                let _g = Self::broadcast_guard(p, &signer.pubkey()).await;
                p.send_transaction(instructions, &signer).await
            }
        }
    }

    /// Read on-chain account data for `pubkey`.
    pub async fn get_account(&self, pubkey: &Address) -> Result<Option<Account>, SvmError> {
        match self {
            SvmChain::Mock(p) => Ok(p.get_account(pubkey).await),
            SvmChain::Rpc(p) => p.get_account(pubkey).await,
        }
    }

    /// Ensure `who` holds at least `amount` of `asset`.
    ///
    /// Mock native: mints (sets) the lamport balance. Mock SPL: validates the token
    /// account's `amount`. RPC native: validates the real balance (no minting on a live
    /// cluster) and reports a [`FundError::Shortfall`] if underfunded. RPC SPL: still
    /// [`FundError::Unimplemented`].
    pub async fn ensure_asset(
        &mut self,
        who: &Address,
        asset: SvmAsset,
        amount: u64,
    ) -> Result<(), FundError> {
        let p = match self {
            SvmChain::Mock(p) => p,
            SvmChain::Rpc(p) => return p.ensure_asset(who, asset, amount).await,
        };
        match asset {
            SvmAsset::Native => {
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
            SvmAsset::Spl(token_account) => {
                let actual = p
                    .svm()
                    .get_account(&token_account)
                    .and_then(|acc| {
                        acc.data
                            .get(SPL_AMOUNT_OFFSET..SPL_AMOUNT_OFFSET + 8)
                            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
                    })
                    .unwrap_or(0);
                if actual < amount {
                    Err(FundError::Shortfall {
                        asset: format!("spl-account:{token_account}"),
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

impl ChainProvider for SvmChain {
    type Spec = SolanaChainInfo;
    type Address = Address;
    type Account = Keypair;
    type Balance = u64;
    type Error = SvmError;

    fn chain_info(&self) -> &Self::Spec {
        match self {
            SvmChain::Mock(p) => p.chain_info(),
            SvmChain::Rpc(p) => p.chain_info(),
        }
    }

    async fn new_account(&mut self, label: &str) -> Address {
        match self {
            SvmChain::Mock(p) => p.new_account(label).await,
            SvmChain::Rpc(p) => p.new_account(label).await,
        }
    }

    async fn balance(&self, addr: &Address) -> Result<u64, SvmError> {
        match self {
            SvmChain::Mock(p) => p.balance(addr).await,
            SvmChain::Rpc(p) => p.balance(addr).await,
        }
    }

    async fn set_balance(
        &mut self,
        addr: &Address,
        denom: &str,
        amount: u64,
    ) -> Result<(), SvmError> {
        match self {
            SvmChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            SvmChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }

    async fn block_height(&self) -> u64 {
        match self {
            SvmChain::Mock(p) => p.block_height().await,
            SvmChain::Rpc(p) => p.block_height().await,
        }
    }

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        match self {
            SvmChain::Mock(p) => p.advance_blocks(n, time).await,
            SvmChain::Rpc(p) => p.advance_blocks(n, time).await,
        }
    }
}
