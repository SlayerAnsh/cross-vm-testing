//! Live-RPC EVM provider (read-only).
//!
//! [`EvmRpcProvider`] talks to a real EVM node over JSON-RPC (alloy HTTP provider). Read
//! paths use no signer: [`block_height`], [`balance`], and [`static_call`] (`eth_call`). Write
//! paths ([`deploy_create`], [`call`]) sign with the wallet's `PrivateKeySigner` (alloy
//! `EthereumWallet`) and broadcast; only `set_balance` stays [`EvmError::Unimplemented`]
//! (a live chain cannot mint).
//!
//! [`deploy_create`]: EvmRpcProvider::deploy_create
//! [`call`]: EvmRpcProvider::call
//!
//! [`block_height`]: EvmRpcProvider::block_height
//! [`balance`]: EvmRpcProvider::balance
//! [`static_call`]: EvmRpcProvider::static_call

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy::network::{EthereumWallet, ReceiptResponse, TransactionBuilder};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy_primitives::{Address, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{ChainProvider, FundError, WalletFactory};

use crate::asset::EvmAsset;
use crate::chains::EvmChainInfo;
use crate::error::EvmError;
use crate::provider::address::address_from_label;
use crate::provider::EvmExecution;

/// A live-RPC EVM provider. Read-only: chain-level reads and static calls hit a real node;
/// state-mutating operations remain [`EvmError::Unimplemented`].
#[derive(Clone)]
pub struct EvmRpcProvider {
    info: EvmChainInfo,
    rpc_url: String,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, PrivateKeySigner>>>,
}

impl EvmRpcProvider {
    /// Create an RPC provider bound to a chain's metadata.
    ///
    /// Stays infallible so `SEPOLIA.rpc(wallets)` sugar keeps working; a missing or empty `rpc_url`
    /// surfaces as an error at the first network call instead.
    pub fn new(info: EvmChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let rpc_url = info.rpc_url.unwrap_or("").to_string();
        Self {
            info,
            rpc_url,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Build an alloy HTTP provider that signs and fills (nonce/gas/chain-id) with `signer`.
    fn signing_provider(&self, signer: &PrivateKeySigner) -> Result<impl Provider, EvmError> {
        if self.rpc_url.is_empty() {
            return Err(EvmError::Rpc(format!(
                "chain '{}' has no rpc_url; use a chain preset with an endpoint",
                self.info.chain_id
            )));
        }
        let url = self
            .rpc_url
            .parse()
            .map_err(|e| EvmError::Rpc(format!("invalid rpc url: {e}")))?;
        let wallet = EthereumWallet::from(signer.clone());
        Ok(ProviderBuilder::new().wallet(wallet).connect_http(url))
    }

    /// Build an alloy HTTP provider for this chain's endpoint.
    ///
    /// Cheap (just a reqwest client, no connection), so callers build per request.
    fn provider(&self) -> Result<impl Provider, EvmError> {
        if self.rpc_url.is_empty() {
            return Err(EvmError::Rpc(format!(
                "chain '{}' has no rpc_url; use a chain preset with an endpoint",
                self.info.chain_id
            )));
        }
        let url = self
            .rpc_url
            .parse()
            .map_err(|e| EvmError::Rpc(format!("invalid rpc url: {e}")))?;
        Ok(ProviderBuilder::new().connect_http(url))
    }

    /// Current block number. Inherent fallible variant of the trait's infallible
    /// [`ChainProvider::block_height`].
    pub async fn try_block_height(&self) -> Result<u64, EvmError> {
        self.provider()?
            .get_block_number()
            .await
            .map_err(|e| EvmError::Rpc(e.to_string()))
    }

    /// Ensure `who` holds at least `amount` of `asset` on the live chain.
    ///
    /// A real chain cannot mint, so this validates rather than funds: native reads the actual
    /// balance and reports a [`FundError::Shortfall`] when the account is underfunded (top up
    /// via a faucet). Erc20 funding stays [`FundError::Unimplemented`].
    pub async fn ensure_asset(
        &mut self,
        who: &Address,
        asset: EvmAsset,
        amount: U256,
    ) -> Result<(), FundError> {
        match asset {
            EvmAsset::Native => {
                let current = self
                    .balance(who)
                    .await
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                if current < amount {
                    Err(FundError::Shortfall {
                        asset: self.info.native_symbol.to_string(),
                        required: amount.to_string(),
                        actual: current.to_string(),
                    })
                } else {
                    Ok(())
                }
            }
            EvmAsset::Erc20(_) => Err(FundError::Unimplemented("evm rpc erc20 funding".into())),
        }
    }

    // ----- Write paths: sign with the wallet signer and broadcast to the live chain. -----

    /// Deploy bytecode via a create transaction signed by `signer`, returning the new contract
    /// address from the mined receipt.
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
    ) -> Result<Address, EvmError> {
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args.as_ref());
        let provider = self.signing_provider(signer)?;
        // `with_deploy_code` sets the input and marks the tx kind as Create; setting only the
        // input leaves the recipient ambiguous and the wallet filler rejects it.
        let tx = TransactionRequest::default().with_deploy_code(Bytes::from(initcode));
        let receipt = provider
            .send_transaction(tx)
            .await
            .map_err(|e| EvmError::Deploy(e.to_string()))?
            .get_receipt()
            .await
            .map_err(|e| EvmError::Deploy(e.to_string()))?;
        receipt
            .contract_address()
            .ok_or_else(|| EvmError::Deploy("receipt carried no contract address".into()))
    }

    /// Execute a state-mutating call against `to`, signed by `signer`.
    ///
    /// Unlike the mock, a broadcast transaction yields no return data; the [`EvmExecution`]
    /// carries the receipt's logs with empty `output`.
    pub async fn call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
    ) -> Result<EvmExecution, EvmError> {
        let provider = self.signing_provider(signer)?;
        let tx = TransactionRequest::default()
            .to(*to)
            .input(Bytes::copy_from_slice(calldata.as_ref()).into());
        let receipt = provider
            .send_transaction(tx)
            .await
            .map_err(|e| EvmError::Execute(e.to_string()))?
            .get_receipt()
            .await
            .map_err(|e| EvmError::Execute(e.to_string()))?;
        let logs = receipt
            .inner
            .logs()
            .iter()
            .map(|l| l.inner.clone())
            .collect();
        Ok(EvmExecution {
            output: Bytes::new(),
            logs,
            tx_hash: Some(receipt.transaction_hash()),
        })
    }

    /// Run a read-only static call (`eth_call`) against `to`.
    pub async fn static_call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, EvmError> {
        let tx = TransactionRequest::default()
            .to(*to)
            .input(Bytes::copy_from_slice(calldata.as_ref()).into());
        self.provider()?
            .call(tx)
            .await
            .map_err(|e| EvmError::Query(e.to_string()))
    }
}

impl ChainProvider for EvmRpcProvider {
    type Spec = EvmChainInfo;
    type Address = Address;
    type Account = Address;
    type Balance = U256;
    type Error = EvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, label: &str) -> Address {
        // No signing backend in the read-only phase; return a deterministic placeholder
        // address. Real key derivation arrives with the write (sign + broadcast) pass.
        address_from_label(label)
    }

    async fn balance(&self, addr: &Address) -> Result<U256, EvmError> {
        self.provider()?
            .get_balance(*addr)
            .await
            .map_err(|e| EvmError::Balance(e.to_string()))
    }

    async fn set_balance(&mut self, _addr: &Address, _amount: U256) -> Result<(), EvmError> {
        // Cannot mint on a real chain. Use a faucet; declared funding is validated, not minted.
        Err(EvmError::Unimplemented("rpc set_balance".into()))
    }

    async fn block_height(&self) -> u64 {
        self.try_block_height().await.unwrap_or(0)
    }

    async fn advance_blocks(&mut self, _n: u64) {
        // No-op: a real chain advances on its own; tests poll instead of forcing blocks.
    }
}
