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

use alloy::eips::eip2718::Encodable2718;
use alloy::network::{EthereumWallet, ReceiptResponse, TransactionBuilder};
use alloy::providers::{Provider, ProviderBuilder, SendableTx};
use alloy::rpc::types::TransactionRequest;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{BlockTime, ChainProvider, FundError, WalletFactory};

use crate::asset::EvmAsset;
use crate::chains::EvmChainInfo;
use crate::error::EvmError;
use crate::provider::address::address_from_label;
use crate::provider::{EvmDeploy, EvmExecution, EvmGas, EvmGasLimit};
use crate::transport::{EvmTransport, HttpTransport};

/// The gas a mined receipt reports: the billed gas, plus the fee it implies. The effective price
/// already folds in the EIP-1559 base fee and priority tip, so `used * price` is the whole fee.
fn receipt_gas<R: ReceiptResponse>(receipt: &R) -> EvmGas {
    let used = receipt.gas_used();
    EvmGas {
        used,
        fee: Some(u128::from(used) * receipt.effective_gas_price()),
    }
}

/// A live-RPC EVM provider. Chain-level reads and static calls hit a real node; the write paths
/// ([`deploy_create`](Self::deploy_create), [`call`](Self::call)) sign with the wallet's
/// `EthereumWallet` and broadcast. Only `set_balance` stays [`EvmError::Unimplemented`].
#[derive(Clone)]
pub struct EvmRpcProvider {
    info: EvmChainInfo,
    /// Pluggable JSON-RPC transport: a factory over alloy's `RpcClient` seam. Defaults to
    /// [`HttpTransport`]; a caller can inject any [`EvmTransport`] (custom stack, mock, ...).
    transport: Rc<dyn EvmTransport>,
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
        let transport = Rc::new(HttpTransport::new(
            info.rpc_url.map(str::to_string),
            info.chain_id.to_string(),
        ));
        Self::new_with_transport(info, wallets, transport)
    }

    /// Create an RPC provider bound to a chain's metadata over a caller-supplied [`EvmTransport`].
    ///
    /// The transport is the factory for alloy's `RpcClient`; use this to attach a custom HTTP
    /// stack, an instrumenting wrapper, a websocket transport, or a `RpcClient::mocked` asserter in
    /// tests. [`new`](Self::new) is the sugar that defaults to [`HttpTransport`].
    pub fn new_with_transport(
        info: EvmChainInfo,
        wallets: Rc<WalletFactory>,
        transport: Rc<dyn EvmTransport>,
    ) -> Self {
        Self {
            info,
            transport,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Build an alloy provider that signs and fills (nonce/gas/chain-id) with `signer`.
    ///
    /// Async because the transport resolves its `RpcClient` asynchronously (http resolves
    /// immediately; a future websocket transport connects on first use).
    async fn signing_provider(&self, signer: &PrivateKeySigner) -> Result<impl Provider, EvmError> {
        let client = self.transport.rpc_client().await?;
        let wallet = EthereumWallet::from(signer.clone());
        Ok(ProviderBuilder::new().wallet(wallet).connect_client(client))
    }

    /// Build an alloy provider for this chain's endpoint.
    ///
    /// Cheap (the http transport just builds a reqwest client, no connection), so callers build per
    /// request. Async for the same reason as [`signing_provider`](Self::signing_provider).
    async fn provider(&self) -> Result<impl Provider, EvmError> {
        let client = self.transport.rpc_client().await?;
        Ok(ProviderBuilder::new().connect_client(client))
    }

    /// Current block number. Inherent fallible variant of the trait's infallible
    /// [`ChainProvider::block_height`].
    pub async fn try_block_height(&self) -> Result<u64, EvmError> {
        self.provider().await?
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
    /// address and the broadcast transaction hash from the mined receipt.
    ///
    /// `limit` becomes the transaction's `gas` field: [`EvmGasLimit::Exact`] verbatim (the node
    /// mines it out of gas if it does not suffice), [`EvmGasLimit::Estimated`] from an
    /// `eth_estimateGas` of this very create, scaled by the chain's `gas_adjustment`. Setting the
    /// field explicitly also stops alloy's gas filler from estimating on its own.
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
        limit: EvmGasLimit,
    ) -> Result<EvmDeploy, EvmError> {
        let gas_limit = match limit {
            EvmGasLimit::Exact(n) => n,
            EvmGasLimit::Estimated => {
                let quote = self
                    .estimate_deploy_create(
                        bytecode.clone(),
                        constructor_args.as_ref(),
                        &signer.address(),
                    )
                    .await?;
                self.info.adjusted_gas_limit(quote.used)
            }
        };
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args.as_ref());
        let provider = self.signing_provider(signer).await?;
        // `with_deploy_code` sets the input and marks the tx kind as Create; setting only the
        // input leaves the recipient ambiguous and the wallet filler rejects it.
        let tx = TransactionRequest::default()
            .with_deploy_code(Bytes::from(initcode))
            .with_gas_limit(gas_limit);
        let receipt = provider
            .send_transaction(tx)
            .await
            .map_err(|e| EvmError::Deploy(e.to_string()))?
            .get_receipt()
            .await
            .map_err(|e| EvmError::Deploy(e.to_string()))?;
        let address = receipt
            .contract_address()
            .ok_or_else(|| EvmError::Deploy("receipt carried no contract address".into()))?;
        Ok(EvmDeploy {
            address,
            tx_hash: receipt.transaction_hash(),
            gas: receipt_gas(&receipt),
        })
    }

    /// Gas a [`deploy_create`](Self::deploy_create) of this bytecode would be billed
    /// (`eth_estimateGas`), plus the fee it would cost at the node's current gas price.
    ///
    /// Signs nothing and broadcasts nothing, so it takes the sender's address rather than its
    /// signer, and needs no broadcast lock.
    pub async fn estimate_deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        from: &Address,
    ) -> Result<EvmGas, EvmError> {
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args.as_ref());
        let tx = TransactionRequest::default()
            .with_from(*from)
            .with_deploy_code(Bytes::from(initcode));
        let provider = self.provider().await?;
        let used = provider
            .estimate_gas(tx)
            .await
            .map_err(|e| EvmError::Deploy(e.to_string()))?;
        self.priced(&provider, used).await
    }

    /// Gas a [`call`](Self::call) with these arguments would be billed (see
    /// [`estimate_call_value`](Self::estimate_call_value)).
    pub async fn estimate_call(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
    ) -> Result<EvmGas, EvmError> {
        self.estimate_call_value(to, calldata, from, U256::ZERO)
            .await
    }

    /// Gas a [`call_value`](Self::call_value) with these arguments would be billed
    /// (`eth_estimateGas`), plus the fee it would cost at the node's current gas price.
    ///
    /// `eth_estimateGas` executes the transaction on the node, so a call that would revert comes
    /// back as an error (carrying the node's revert reason) rather than as a gas figure. That error
    /// is propagated, not swallowed.
    pub async fn estimate_call_value(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        from: &Address,
        value: U256,
    ) -> Result<EvmGas, EvmError> {
        let tx = TransactionRequest::default()
            .with_from(*from)
            .to(*to)
            .value(value)
            .input(Bytes::copy_from_slice(calldata.as_ref()).into());
        let provider = self.provider().await?;
        let used = provider
            .estimate_gas(tx)
            .await
            .map_err(|e| EvmError::Execute(e.to_string()))?;
        self.priced(&provider, used).await
    }

    /// Price `used` gas at the node's current gas price (`eth_gasPrice`), which already folds the
    /// EIP-1559 base fee and a tip estimate into one number, so the forecast is denominated exactly
    /// like the `effective_gas_price` the receipt will report. It is a quote at estimation time: the
    /// base fee moves block to block, so the fee actually paid can differ.
    async fn priced(&self, provider: &impl Provider, used: u64) -> Result<EvmGas, EvmError> {
        let price = provider
            .get_gas_price()
            .await
            .map_err(|e| EvmError::Rpc(e.to_string()))?;
        Ok(EvmGas {
            used,
            fee: Some(u128::from(used) * price),
        })
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
        limit: EvmGasLimit,
    ) -> Result<EvmExecution, EvmError> {
        self.call_value(to, calldata, signer, U256::ZERO, limit)
            .await
    }

    /// Execute a state-mutating call against `to` carrying `value` wei (a payable call), signed by
    /// `signer`. On a live chain the signer must already hold the value (no minting).
    ///
    /// `limit` becomes the transaction's `gas` field (see [`deploy_create`](Self::deploy_create)).
    pub async fn call_value(
        &self,
        to: &Address,
        calldata: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
        value: U256,
        limit: EvmGasLimit,
    ) -> Result<EvmExecution, EvmError> {
        let gas_limit = match limit {
            EvmGasLimit::Exact(n) => n,
            EvmGasLimit::Estimated => {
                let quote = self
                    .estimate_call_value(to, calldata.as_ref(), &signer.address(), value)
                    .await?;
                self.info.adjusted_gas_limit(quote.used)
            }
        };
        let provider = self.signing_provider(signer).await?;
        let tx = TransactionRequest::default()
            .to(*to)
            .value(value)
            .input(Bytes::copy_from_slice(calldata.as_ref()).into())
            .with_gas_limit(gas_limit);
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
            tx_hash: receipt.transaction_hash(),
            gas: receipt_gas(&receipt),
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
        self.provider().await?
            .call(tx)
            .await
            .map_err(|e| EvmError::Query(e.to_string()))
    }

    /// Read the raw storage value at `slot` for `addr` (`eth_getStorageAt`).
    pub async fn get_storage_at(&self, addr: &Address, slot: U256) -> Result<U256, EvmError> {
        self.provider().await?
            .get_storage_at(*addr, slot)
            .await
            .map_err(|e| EvmError::Query(e.to_string()))
    }

    /// Read the deployed runtime bytecode at `address` (`eth_getCode`); empty for an EOA or an
    /// undeployed address.
    pub async fn get_code(&self, address: &Address) -> Result<Bytes, EvmError> {
        self.provider().await?
            .get_code_at(*address)
            .await
            .map_err(|e| EvmError::Query(e.to_string()))
    }

    /// Generic JSON-RPC escape hatch: send `method` with `params` and return the raw result, for
    /// node methods this provider exposes no typed wrapper for.
    pub async fn raw_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, EvmError> {
        self.provider().await?
            .raw_request(method.to_string().into(), params)
            .await
            .map_err(|e| EvmError::Rpc(e.to_string()))
    }

    /// Fill and sign `tx` into broadcastable RLP bytes, an escape hatch for a transaction the typed
    /// write paths do not build. The signing provider's fillers supply whatever the request leaves
    /// unset (nonce, chain id, gas limit, EIP-1559 fees) before the wallet signs it; a request the
    /// fillers cannot complete into an envelope is an error rather than a half-signed transaction.
    pub async fn sign_transaction(
        &self,
        tx: TransactionRequest,
        signer: &PrivateKeySigner,
    ) -> Result<Bytes, EvmError> {
        // `fill` is an inherent method on the concrete signing provider, so it is built here rather
        // than through `signing_provider`, whose `impl Provider` erases it. `connect_client` keeps
        // the provider concrete, so the inherent `fill` survives the transport indirection.
        let client = self.transport.rpc_client().await?;
        let wallet = EthereumWallet::from(signer.clone());
        let provider = ProviderBuilder::new().wallet(wallet).connect_client(client);
        match provider
            .fill(tx)
            .await
            .map_err(|e| EvmError::Execute(e.to_string()))?
        {
            SendableTx::Envelope(env) => Ok(Bytes::from(env.encoded_2718())),
            SendableTx::Builder(_) => Err(EvmError::Execute(
                "transaction request could not be fully filled for signing".into(),
            )),
        }
    }

    /// Broadcast raw signed transaction bytes (`eth_sendRawTransaction`) and wait for the mined
    /// receipt, returning its transaction hash. Waiting on the receipt mirrors the typed write
    /// paths, so a caller that sees a hash back knows the transaction was mined.
    pub async fn send_raw_transaction(&self, raw: &[u8]) -> Result<B256, EvmError> {
        let receipt = self
            .provider()
            .await?
            .send_raw_transaction(raw)
            .await
            .map_err(|e| EvmError::Execute(e.to_string()))?
            .get_receipt()
            .await
            .map_err(|e| EvmError::Execute(e.to_string()))?;
        Ok(receipt.transaction_hash())
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
        self.provider().await?
            .get_balance(*addr)
            .await
            .map_err(|e| EvmError::Balance(e.to_string()))
    }

    async fn set_balance(
        &mut self,
        _addr: &Address,
        _denom: &str,
        _amount: U256,
    ) -> Result<(), EvmError> {
        // Cannot mint on a real chain. Use a faucet; declared funding is validated, not minted.
        Err(EvmError::Unimplemented("rpc set_balance".into()))
    }

    async fn block_height(&self) -> u64 {
        self.try_block_height().await.unwrap_or(0)
    }

    async fn advance_blocks(&mut self, _n: u64, _time: BlockTime) {
        // No-op: a real chain advances on its own; tests poll instead of forcing blocks.
    }
}
