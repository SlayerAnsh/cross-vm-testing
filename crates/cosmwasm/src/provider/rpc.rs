//! Live-RPC CosmWasm provider.
//!
//! [`CwRpcProvider`] talks to a real Cosmos node over Tendermint RPC. Read paths use ABCI
//! queries with no signer: [`block_height`], [`balance`], and [`query_wasm_smart`]. Write paths
//! ([`store_code`], [`instantiate`], [`execute_contract`], [`transfer_funds`]) sign with the
//! wallet's secp256k1 key (account number + sequence + `SignDoc` + `broadcast_tx_commit`) and
//! broadcast; only `set_balance` stays [`CwError::Unimplemented`] (a live chain cannot mint).
//! Each write path has an `estimate_*` sibling that simulates the same message against the
//! node's `/cosmos.tx.v1beta1.Service/Simulate` endpoint and reports what the op would cost
//! ([`CwGas`]), without broadcasting anything.
//!
//! Every write path takes a required [`CwGasLimit`]: [`CwGasLimit::Exact`] is declared verbatim,
//! [`CwGasLimit::Estimated`] simulates the very message about to be broadcast and scales the
//! node's figure by the chain's [`CosmosChainInfo::gas_adjustment`]. The declared fee follows
//! from the resolved limit and the chain's `gas_price` alone, so the adjustment lands on the
//! limit once and never again on the fee.
//!
//! [`block_height`]: CwRpcProvider::block_height
//! [`balance`]: CwRpcProvider::balance
//! [`query_wasm_smart`]: CwRpcProvider::query_wasm_smart
//! [`store_code`]: CwRpcProvider::store_code
//! [`instantiate`]: CwRpcProvider::instantiate
//! [`execute_contract`]: CwRpcProvider::execute_contract
//! [`transfer_funds`]: CwRpcProvider::transfer_funds

use cosmrs::proto::cosmos::auth::v1beta1::{
    BaseAccount, QueryAccountRequest, QueryAccountResponse,
};
use cosmrs::proto::cosmos::bank::v1beta1::{QueryBalanceRequest, QueryBalanceResponse};
use cosmrs::proto::cosmos::base::query::v1beta1::PageRequest;
use cosmrs::proto::cosmos::tx::v1beta1::{SimulateRequest, SimulateResponse, TxRaw};
use cosmrs::proto::cosmwasm::wasm::v1::{
    CodeInfoResponse, QueryAllContractStateRequest, QueryAllContractStateResponse,
    QueryCodeRequest, QueryRawContractStateRequest, QueryRawContractStateResponse,
    QuerySmartContractStateRequest, QuerySmartContractStateResponse,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cosmrs::bank::MsgSend;
use cosmrs::cosmwasm::{
    MsgExecuteContract, MsgInstantiateContract, MsgMigrateContract, MsgStoreCode,
};
use cosmrs::rpc::endpoint::{abci_query, broadcast::tx_commit, status};
use cosmrs::rpc::{Request as RpcRequest, Response as _, SimpleRequest};
use cosmrs::tendermint::abci::Event as TmEvent;
use cosmrs::tx::{Body, Fee, Msg, SignDoc, SignerInfo};
use cosmrs::{AccountId, Coin as CosmrsCoin, Denom};
use cosmwasm_std::{Addr, Coin, Event};
use cross_vm_core::{BlockTime, ChainProvider, FundError, WalletFactory};
use cw_multi_test::IntoBech32;
use prost::Message;

use crate::asset::CwAsset;
use crate::batch::{CwBatch, CwBatchMember};
use crate::chains::CosmosChainInfo;
use crate::error::CwError;
use crate::msg::CwSerde;
use crate::provider::{CwExecution, CwGas, CwGasLimit, CwInstantiate, CwMigrate, CwStoreCode};
use crate::transport::{CosmosTransport, HttpTransport};
use crate::wallet::CosmosSigner;

/// A live-RPC CosmWasm provider. Chain-level reads and contract queries hit a real node via
/// ABCI queries; the write paths ([`store_code`](Self::store_code),
/// [`instantiate`](Self::instantiate), [`execute_contract`](Self::execute_contract)) sign with
/// the wallet's secp256k1 key and broadcast. Only `set_balance` stays
/// [`CwError::Unimplemented`] (a live chain cannot mint).
#[derive(Clone)]
pub struct CwRpcProvider {
    info: CosmosChainInfo,
    /// The JSON-RPC transport every network call rides. Defaults to [`HttpTransport`] (one POST
    /// per call); [`Self::new_with_transport`] injects anything else (batching, instrumentation,
    /// a test fake).
    transport: Rc<dyn CosmosTransport>,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, CosmosSigner>>>,
}

impl CwRpcProvider {
    /// Create an RPC provider bound to a chain's metadata, riding the default [`HttpTransport`].
    ///
    /// Stays infallible so `OSMOSIS_TESTNET.rpc(wallets)` sugar keeps working; a missing or empty
    /// `rpc_url` surfaces as an error at the first network call instead (the transport raises it).
    pub fn new(info: CosmosChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let transport = Rc::new(HttpTransport::new(info.rpc_url, info.chain_id));
        Self::new_with_transport(info, wallets, transport)
    }

    /// Create an RPC provider riding a caller-supplied [`CosmosTransport`].
    pub fn new_with_transport(
        info: CosmosChainInfo,
        wallets: Rc<WalletFactory>,
        transport: Rc<dyn CosmosTransport>,
    ) -> Self {
        Self {
            info,
            transport,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Run one typed Tendermint RPC request over the transport: serialize the request into its
    /// JSON-RPC envelope, send it, and parse the response envelope back into the request's typed
    /// output (JSON-RPC error envelopes become typed errors here, in the parser).
    async fn perform<R>(&self, req: R) -> Result<R::Output, CwError>
    where
        R: SimpleRequest,
    {
        let resp = self.transport.call(req.into_json()).await?;
        let parsed = <R as RpcRequest>::Response::from_string(resp)
            .map_err(|e| CwError::Rpc(e.to_string()))?;
        Ok(parsed.into())
    }

    /// Run a raw ABCI query and return the response bytes.
    async fn abci_query(&self, path: &str, data: Vec<u8>) -> Result<Vec<u8>, CwError> {
        let res = self
            .perform(abci_query::Request::new(
                Some(path.to_string()),
                data,
                None,
                false,
            ))
            .await?
            .response;
        if res.code.is_err() {
            return Err(CwError::Query(format!(
                "abci_query {path} failed (code {:?}): {}",
                res.code, res.log
            )));
        }
        Ok(res.value)
    }

    /// Current block height from the node's sync info. Inherent fallible variant of the
    /// trait's infallible [`ChainProvider::block_height`].
    pub async fn try_block_height(&self) -> Result<u64, CwError> {
        let status = self.perform(status::Request).await?;
        Ok(status.sync_info.latest_block_height.value())
    }

    /// Ensure `who` holds at least `amount` of `asset` on the live chain.
    ///
    /// A real chain cannot mint, so this validates rather than funds: it reads the actual
    /// native balance and reports a [`FundError::Shortfall`] when the account is underfunded
    /// (top up via a faucet). Only the chain's native denom is supported; cw20 and other
    /// denoms remain [`FundError::Unimplemented`].
    pub async fn ensure_asset(
        &mut self,
        who: &Addr,
        asset: CwAsset,
        amount: u128,
    ) -> Result<(), FundError> {
        match asset {
            CwAsset::Native(denom) if denom == self.info.native_denom => {
                let current = self
                    .balance(who)
                    .await
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                if current < amount {
                    Err(FundError::Shortfall {
                        asset: denom,
                        required: amount.to_string(),
                        actual: current.to_string(),
                    })
                } else {
                    Ok(())
                }
            }
            CwAsset::Native(denom) => Err(FundError::Unimplemented(format!(
                "rpc native funding for non-native denom '{denom}'"
            ))),
            CwAsset::Cw20(_) => Err(FundError::Unimplemented("cosmwasm rpc cw20 funding".into())),
        }
    }

    // ----- Write paths: sign with the wallet key and broadcast over Tendermint RPC. -----

    /// Query an account's `(account_number, sequence)` from the auth module. The account must
    /// exist on chain (it does once it has received funds), else this errors.
    async fn account_info(&self, address: &str) -> Result<(u64, u64), CwError> {
        let req = QueryAccountRequest {
            address: address.to_string(),
        };
        let bytes = self
            .abci_query("/cosmos.auth.v1beta1.Query/Account", req.encode_to_vec())
            .await?;
        let resp = QueryAccountResponse::decode(bytes.as_slice())
            .map_err(|e| CwError::Rpc(e.to_string()))?;
        let any = resp.account.ok_or_else(|| {
            CwError::Execute(format!(
                "account {address} not found on chain; fund it (testnet faucet) first"
            ))
        })?;
        let base = BaseAccount::decode(any.value.as_slice())
            .map_err(|e| CwError::Rpc(format!("decode BaseAccount: {e}")))?;
        Ok((base.account_number, base.sequence))
    }

    /// Build, sign, and broadcast a transaction carrying `msgs` under `memo`, waiting for it to
    /// commit. Returns the tx hash, the delivered events, and what the transaction cost
    /// ([`CwGas`]). Fails on a nonzero check/deliver code.
    async fn sign_and_broadcast(
        &self,
        msgs: Vec<cosmrs::Any>,
        signer: &CosmosSigner,
        gas_limit: u64,
        memo: &str,
    ) -> Result<(String, Vec<TmEvent>, CwGas), CwError> {
        let (account_number, sequence) = self.account_info(signer.address.as_str()).await?;

        let chain_id = self
            .info
            .chain_id
            .parse::<cosmrs::tendermint::chain::Id>()
            .map_err(|e| CwError::Rpc(format!("chain id: {e}")))?;
        let body = Body::new(msgs, memo, 0u16);

        let fee_amount = fee_for(gas_limit, self.info.gas_price);
        let denom = self
            .info
            .native_denom
            .parse::<Denom>()
            .map_err(|e| CwError::Execute(format!("denom: {e}")))?;
        let fee = Fee::from_amount_and_gas(
            CosmrsCoin {
                denom,
                amount: fee_amount,
            },
            gas_limit,
        );

        let auth_info =
            SignerInfo::single_direct(Some(signer.key.public_key()), sequence).auth_info(fee);
        let sign_doc = SignDoc::new(&body, &auth_info, &chain_id, account_number)
            .map_err(|e| CwError::Execute(format!("sign doc: {e}")))?;
        let raw = sign_doc
            .sign(signer.key.as_ref())
            .map_err(|e| CwError::Execute(format!("sign: {e}")))?;

        let tx_bytes = raw
            .to_bytes()
            .map_err(|e| CwError::Rpc(format!("encode tx: {e}")))?;
        let resp = self.perform(tx_commit::Request::new(tx_bytes)).await?;
        if resp.check_tx.code.is_err() {
            return Err(CwError::Execute(format!(
                "check_tx failed (code {:?}): {}",
                resp.check_tx.code, resp.check_tx.log
            )));
        }
        if resp.tx_result.code.is_err() {
            return Err(CwError::Execute(format!(
                "tx failed (code {:?}): {}",
                resp.tx_result.code, resp.tx_result.log
            )));
        }

        // Tendermint types `gas_used` as `i64` (protobuf has no unsigned varint in this schema),
        // but a gas meter only counts up: a negative figure is a node protocol violation, not a
        // value to clamp to zero. Surface it instead of silently reporting a plausible number.
        let used = u64::try_from(resp.tx_result.gas_used).map_err(|_| {
            CwError::Rpc(format!(
                "node reported a negative gas_used ({}) for tx {}",
                resp.tx_result.gas_used, resp.hash
            ))
        })?;
        let gas = CwGas {
            used,
            fee: fee_amount,
        };
        Ok((resp.hash.to_string(), resp.tx_result.events, gas))
    }

    // ----- Estimation: simulate a tx against the node without broadcasting it. -----

    /// Simulate a transaction carrying `msgs` against the node and return the gas the node
    /// reports it would consume (the Simulate response's `gas_info.gas_used`).
    ///
    /// The tx is assembled like [`Self::sign_and_broadcast`]'s (same messages, the signer's real
    /// public key and on-chain sequence) but carries a dummy signature and is never broadcast
    /// (see [`simulate_tx_bytes`] for why that is correct). Chain state is untouched and the
    /// account's sequence does not advance.
    ///
    /// The figure is the node's raw report against its latest committed state. Real delivery can
    /// land somewhat higher (a later block, different block context), which is why a gas *limit*
    /// derived from this figure needs a buffer on top; applying one is the caller's business.
    async fn simulate(
        &self,
        msgs: Vec<cosmrs::Any>,
        signer: &CosmosSigner,
    ) -> Result<u64, CwError> {
        let (_, sequence) = self.account_info(signer.address.as_str()).await?;
        let req = SimulateRequest {
            tx_bytes: simulate_tx_bytes(msgs, signer.key.public_key(), sequence)?,
            // Field 1 (`tx`) is deprecated in favor of `tx_bytes`; leave it unset.
            ..Default::default()
        };
        let bytes = self
            .abci_query("/cosmos.tx.v1beta1.Service/Simulate", req.encode_to_vec())
            .await?;
        parse_simulate_gas(&bytes)
    }

    /// Resolve a [`CwGasLimit`] into the gas figure the transaction carrying `msgs` will declare.
    ///
    /// [`CwGasLimit::Exact`] passes straight through. [`CwGasLimit::Estimated`] simulates `msgs`
    /// and scales the node's figure by the chain's [`CosmosChainInfo::gas_adjustment`] (see
    /// [`adjust_gas`]). That is the *only* place the adjustment is applied: the fee is then
    /// derived from the resolved limit by [`fee_for`], which knows nothing about it, so an
    /// `Estimated` limit and an `Exact` one of the same size cost the sender exactly the same.
    ///
    /// Called from inside the write paths, which the caller ([`crate::CwChain`]) runs while
    /// holding the per-account broadcast lock. This is why it reaches for the provider-level
    /// [`Self::simulate`], which takes no lock: a write path can compose its own estimate
    /// without deadlocking against a lock it already holds.
    async fn resolve_gas_limit(
        &self,
        limit: CwGasLimit,
        msgs: Vec<cosmrs::Any>,
        signer: &CosmosSigner,
    ) -> Result<u64, CwError> {
        match limit {
            CwGasLimit::Exact(gas) => Ok(gas),
            CwGasLimit::Estimated => {
                let simulated = self.simulate(msgs, signer).await?;
                Ok(adjust_gas(simulated, self.info.gas_adjustment))
            }
        }
    }

    /// What a simulated op would cost, as a [`CwGas`] forecast comparable to a receipt.
    ///
    /// See [`estimated_gas`]; this just supplies the chain's `gas_adjustment` and `gas_price`.
    fn forecast(&self, simulated: u64) -> CwGas {
        estimated_gas(simulated, self.info.gas_adjustment, self.info.gas_price)
    }

    /// Estimate what uploading `wasm` would cost, by simulating the `MsgStoreCode` against the
    /// node without broadcasting it. See [`Self::simulate`] and [`estimated_gas`].
    pub async fn estimate_store_code(
        &self,
        wasm: Vec<u8>,
        signer: &CosmosSigner,
    ) -> Result<CwGas, CwError> {
        let simulated = self
            .simulate(vec![store_code_msg(wasm, signer)?], signer)
            .await?;
        Ok(self.forecast(simulated))
    }

    /// Estimate what instantiating `code_id` with `init` would cost, by simulating the
    /// `MsgInstantiateContract` against the node without broadcasting it. See
    /// [`Self::simulate`] and [`estimated_gas`].
    pub async fn estimate_instantiate<Init: CwSerde>(
        &self,
        code_id: u64,
        init: Init,
        signer: &CosmosSigner,
        funds: &[Coin],
        label: &str,
    ) -> Result<CwGas, CwError> {
        let simulated = self
            .simulate(
                vec![instantiate_msg(code_id, &init, signer, funds, label)?],
                signer,
            )
            .await?;
        Ok(self.forecast(simulated))
    }

    /// Estimate what executing `msg` against `addr` would cost, by simulating the
    /// `MsgExecuteContract` against the node without broadcasting it. See [`Self::simulate`]
    /// and [`estimated_gas`].
    pub async fn estimate_execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        signer: &CosmosSigner,
        funds: &[Coin],
    ) -> Result<CwGas, CwError> {
        let simulated = self
            .simulate(vec![execute_msg(addr, &msg, signer, funds)?], signer)
            .await?;
        Ok(self.forecast(simulated))
    }

    /// Estimate what a bank send of `amount` `denom` to `to` would cost, by simulating the
    /// `MsgSend` against the node without broadcasting it. See [`Self::simulate`] and
    /// [`estimated_gas`].
    pub async fn estimate_transfer_funds(
        &self,
        to: &Addr,
        denom: &str,
        amount: u128,
        signer: &CosmosSigner,
    ) -> Result<CwGas, CwError> {
        let simulated = self
            .simulate(vec![transfer_msg(to, denom, amount, signer)?], signer)
            .await?;
        Ok(self.forecast(simulated))
    }

    /// Estimate what migrating `contract` to `new_code_id` with `msg` would cost, by simulating
    /// the `MsgMigrateContract` against the node without broadcasting it. See [`Self::simulate`]
    /// and [`estimated_gas`].
    pub async fn estimate_migrate_contract<Migrate: CwSerde>(
        &self,
        contract: &Addr,
        new_code_id: u64,
        msg: Migrate,
        signer: &CosmosSigner,
    ) -> Result<CwGas, CwError> {
        let simulated = self
            .simulate(
                vec![migrate_msg(contract, new_code_id, &msg, signer)?],
                signer,
            )
            .await?;
        Ok(self.forecast(simulated))
    }

    /// Upload raw wasm bytecode to the chain under `gas`, signed by `signer`, and return its code
    /// id, the broadcast transaction hash, and what the upload cost ([`CwGas`]).
    ///
    /// This is the RPC arm of [`crate::CwChain::store_code`]: a live chain takes compiled wasm
    /// bytes, while the mock's `store_code` takes a native `cw-multi-test` `Contract` object.
    pub async fn store_code(
        &self,
        wasm: Vec<u8>,
        signer: &CosmosSigner,
        gas: CwGasLimit,
    ) -> Result<CwStoreCode, CwError> {
        let any = store_code_msg(wasm, signer)?;
        let gas_limit = self
            .resolve_gas_limit(gas, vec![any.clone()], signer)
            .await?;
        let (tx_hash, events, gas) = self
            .sign_and_broadcast(vec![any], signer, gas_limit, "")
            .await?;
        let code_id = find_attr(&events, "store_code", "code_id")?
            .parse::<u64>()
            .map_err(|e| CwError::Execute(format!("parse code_id: {e}")))?;
        Ok(CwStoreCode {
            code_id,
            tx_hash,
            gas: Some(gas),
        })
    }

    /// Send `amount` base units of bank `denom` from `signer` to `to` under `gas`, and return the
    /// broadcast transaction hash.
    ///
    /// Any bank denom moves verbatim (`uosmo`, `ibc/...`), not just the chain's native denom.
    pub async fn transfer_funds(
        &self,
        to: &Addr,
        denom: &str,
        amount: u128,
        signer: &CosmosSigner,
        gas: CwGasLimit,
    ) -> Result<String, CwError> {
        let any = transfer_msg(to, denom, amount, signer)?;
        let gas_limit = self
            .resolve_gas_limit(gas, vec![any.clone()], signer)
            .await?;
        let (tx_hash, _, _) = self
            .sign_and_broadcast(vec![any], signer, gas_limit, "")
            .await?;
        Ok(tx_hash)
    }

    /// Instantiate a contract from an uploaded code id under `gas`, signed by `signer`, and
    /// return the new instance's address, the broadcast transaction hash, and what the
    /// instantiation cost ([`CwGas`]).
    pub async fn instantiate<Init: CwSerde>(
        &self,
        code_id: u64,
        init: Init,
        signer: &CosmosSigner,
        funds: &[Coin],
        label: &str,
        gas: CwGasLimit,
    ) -> Result<CwInstantiate, CwError> {
        let any = instantiate_msg(code_id, &init, signer, funds, label)?;
        let gas_limit = self
            .resolve_gas_limit(gas, vec![any.clone()], signer)
            .await?;
        let (tx_hash, events, gas) = self
            .sign_and_broadcast(vec![any], signer, gas_limit, "")
            .await?;
        let addr = find_attr(&events, "instantiate", "_contract_address")?;
        Ok(CwInstantiate {
            address: Addr::unchecked(addr),
            tx_hash,
            gas: Some(gas),
        })
    }

    /// Execute a state-mutating message against a contract instance under `gas`, signed by
    /// `signer`.
    ///
    /// The returned [`CwExecution`] carries the broadcast transaction hash (`tx_hash`), what the
    /// execution cost (`gas`, always `Some` here: the node meters it), plus a
    /// [`cw_multi_test::AppResponse`] holding the chain's emitted events (mapped to
    /// `cosmwasm_std::Event`); `data` is left `None` (the raw tx data is proto-wrapped, not the
    /// contract's response payload).
    pub async fn execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        signer: &CosmosSigner,
        funds: &[Coin],
        gas: CwGasLimit,
    ) -> Result<CwExecution, CwError> {
        let any = execute_msg(addr, &msg, signer, funds)?;
        let gas_limit = self
            .resolve_gas_limit(gas, vec![any.clone()], signer)
            .await?;
        let (tx_hash, events, gas) = self
            .sign_and_broadcast(vec![any], signer, gas_limit, "")
            .await?;
        Ok(CwExecution {
            tx_hash,
            gas: Some(gas),
            response: cw_multi_test::AppResponse {
                events: events.iter().map(to_cw_event).collect(),
                data: None,
                msg_responses: Vec::new(),
            },
        })
    }

    /// Migrate `contract` to `new_code_id` under `gas`, signed by `signer`, and return the
    /// broadcast transaction hash and what the migration cost ([`CwGas`]).
    ///
    /// `signer` must be the contract's admin on chain (a live node rejects the migration
    /// otherwise); the migration runs the new code's `migrate` entry point with `msg`.
    pub async fn migrate_contract<Migrate: CwSerde>(
        &self,
        contract: &Addr,
        new_code_id: u64,
        msg: Migrate,
        signer: &CosmosSigner,
        gas: CwGasLimit,
    ) -> Result<CwMigrate, CwError> {
        let any = migrate_msg(contract, new_code_id, &msg, signer)?;
        let gas_limit = self
            .resolve_gas_limit(gas, vec![any.clone()], signer)
            .await?;
        let (tx_hash, _events, gas) = self
            .sign_and_broadcast(vec![any], signer, gas_limit, "")
            .await?;
        Ok(CwMigrate {
            tx_hash,
            gas: Some(gas),
        })
    }

    /// Sign and broadcast a caller-supplied set of `msgs` under `gas` and `memo`, signed by
    /// `signer`, and return the tx hash, what it cost, and its emitted events.
    ///
    /// The framework's escape hatch: it broadcasts any protobuf `Any` messages a caller assembles
    /// (module messages the typed paths do not wrap), reusing the same signing, gas resolution
    /// (so [`CwGasLimit::Estimated`] simulates the exact `msgs`), and broadcast plumbing the typed
    /// write paths use. `data` on the response is left `None` (the raw tx data is proto-wrapped).
    pub async fn sign_and_broadcast_msgs(
        &self,
        msgs: Vec<cosmrs::Any>,
        signer: &CosmosSigner,
        gas: CwGasLimit,
        memo: &str,
    ) -> Result<CwExecution, CwError> {
        let gas_limit = self.resolve_gas_limit(gas, msgs.clone(), signer).await?;
        let (tx_hash, events, gas) = self
            .sign_and_broadcast(msgs, signer, gas_limit, memo)
            .await?;
        Ok(CwExecution {
            tx_hash,
            gas: Some(gas),
            response: cw_multi_test::AppResponse {
                events: events.iter().map(to_cw_event).collect(),
                data: None,
                msg_responses: Vec::new(),
            },
        })
    }

    /// Sign and broadcast every member of `batch` as one atomic transaction under `gas`, signed by
    /// `signer`, and return one [`CwExecution`] carrying the single broadcast tx hash, what it
    /// cost, and its emitted events.
    ///
    /// The members map to protobuf `Any` messages ([`batch_member_to_any`]) and ride one signed
    /// transaction, so they commit all-or-nothing under a single hash. Reuses the same signing, gas
    /// resolution ([`CwGasLimit::Estimated`] simulates the exact set), and broadcast plumbing the
    /// typed write paths use.
    pub async fn execute_batch(
        &self,
        batch: &CwBatch,
        signer: &CosmosSigner,
        gas: CwGasLimit,
    ) -> Result<CwExecution, CwError> {
        let msgs = batch_msgs(batch, signer)?;
        let gas_limit = self.resolve_gas_limit(gas, msgs.clone(), signer).await?;
        let (tx_hash, events, gas) = self.sign_and_broadcast(msgs, signer, gas_limit, "").await?;
        Ok(CwExecution {
            tx_hash,
            gas: Some(gas),
            response: cw_multi_test::AppResponse {
                events: events.iter().map(to_cw_event).collect(),
                data: None,
                msg_responses: Vec::new(),
            },
        })
    }

    /// Estimate what broadcasting `batch` would cost, by simulating the whole message set against
    /// the node without broadcasting it. See [`Self::simulate`] and [`estimated_gas`].
    pub async fn estimate_execute_batch(
        &self,
        batch: &CwBatch,
        signer: &CosmosSigner,
    ) -> Result<CwGas, CwError> {
        let simulated = self.simulate(batch_msgs(batch, signer)?, signer).await?;
        Ok(self.forecast(simulated))
    }

    /// The hex-encoded sha256 checksum of the wasm blob behind `code_id` (wasmd's `data_hash`).
    pub async fn code_checksum(&self, code_id: u64) -> Result<String, CwError> {
        // cosmos-sdk-proto carries no `CodeInfo`-named request/response type, but wasmd's
        // `Query/CodeInfo` takes `{code_id}` and returns `{code_id, creator, data_hash, ...}`,
        // wire-identical to `QueryCodeRequest` / `CodeInfoResponse`, so those stand in here (and
        // avoid `Query/Code`, which would also stream back the whole wasm blob).
        let req = QueryCodeRequest { code_id };
        let bytes = self
            .abci_query("/cosmwasm.wasm.v1.Query/CodeInfo", req.encode_to_vec())
            .await?;
        let resp = CodeInfoResponse::decode(bytes.as_slice())
            .map_err(|e| CwError::Query(e.to_string()))?;
        Ok(hex::encode(resp.data_hash))
    }

    /// Run a read-only smart query against a contract instance.
    pub async fn query_wasm_smart<Query: CwSerde, Resp: CwSerde>(
        &self,
        addr: &Addr,
        msg: Query,
    ) -> Result<Resp, CwError> {
        let req = QuerySmartContractStateRequest {
            address: addr.to_string(),
            query_data: serde_json::to_vec(&msg).map_err(|e| CwError::Query(e.to_string()))?,
        };
        let bytes = self
            .abci_query(
                "/cosmwasm.wasm.v1.Query/SmartContractState",
                req.encode_to_vec(),
            )
            .await?;
        let resp = QuerySmartContractStateResponse::decode(bytes.as_slice())
            .map_err(|e| CwError::Query(e.to_string()))?;
        serde_json::from_slice(&resp.data).map_err(|e| CwError::Query(e.to_string()))
    }

    /// Read a raw storage entry from a contract instance by its exact key.
    ///
    /// A missing key yields empty response data on the wasm module's raw query, which maps to
    /// `None` here so the live and mock backends agree: `Some(bytes)` when the key exists,
    /// `None` when it is absent.
    pub async fn query_wasm_raw(
        &self,
        addr: &Addr,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, CwError> {
        let req = QueryRawContractStateRequest {
            address: addr.to_string(),
            query_data: key.to_vec(),
        };
        let bytes = self
            .abci_query(
                "/cosmwasm.wasm.v1.Query/RawContractState",
                req.encode_to_vec(),
            )
            .await?;
        let resp = QueryRawContractStateResponse::decode(bytes.as_slice())
            .map_err(|e| CwError::Query(e.to_string()))?;
        if resp.data.is_empty() {
            Ok(None)
        } else {
            Ok(Some(resp.data))
        }
    }

    /// Dump every raw key-value pair held in a contract's storage, in ascending key order.
    ///
    /// Pages through the wasm module's `AllContractState` query, following each response's
    /// `next_key` until it comes back empty and accumulating every `(key, value)` model. The
    /// order follows wasmd (ascending by raw key), so this agrees with the mock backend's dump.
    pub async fn get_contract_states(
        &self,
        addr: &Addr,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, CwError> {
        let mut states = Vec::new();
        let mut next_key: Vec<u8> = Vec::new();
        loop {
            let req = QueryAllContractStateRequest {
                address: addr.to_string(),
                // An empty `key` starts at the first page; a non-empty one resumes after it.
                pagination: Some(PageRequest {
                    key: next_key,
                    offset: 0,
                    limit: 0,
                    count_total: false,
                    reverse: false,
                }),
            };
            let bytes = self
                .abci_query(
                    "/cosmwasm.wasm.v1.Query/AllContractState",
                    req.encode_to_vec(),
                )
                .await?;
            let resp = QueryAllContractStateResponse::decode(bytes.as_slice())
                .map_err(|e| CwError::Query(e.to_string()))?;
            states.extend(resp.models.into_iter().map(|m| (m.key, m.value)));
            match resp.pagination {
                Some(page) if !page.next_key.is_empty() => next_key = page.next_key,
                _ => break,
            }
        }
        Ok(states)
    }
}

// ----- Message builders, shared by the broadcast paths and their estimate siblings. -----

/// Build the `MsgStoreCode` uploading `wasm`, encoded as a protobuf `Any`.
fn store_code_msg(wasm: Vec<u8>, signer: &CosmosSigner) -> Result<cosmrs::Any, CwError> {
    MsgStoreCode {
        sender: signer_account(signer)?,
        wasm_byte_code: wasm,
        instantiate_permission: None,
    }
    .to_any()
    .map_err(|e| CwError::Execute(format!("encode store_code: {e}")))
}

/// Build the `MsgInstantiateContract` instantiating `code_id` with `init`, encoded as a
/// protobuf `Any`.
fn instantiate_msg<Init: CwSerde>(
    code_id: u64,
    init: &Init,
    signer: &CosmosSigner,
    funds: &[Coin],
    label: &str,
) -> Result<cosmrs::Any, CwError> {
    MsgInstantiateContract {
        sender: signer_account(signer)?,
        // Set the instantiator as the contract's admin, so it can later be migrated (wasmd rejects
        // a migration from a non-admin, and an admin-less contract is immutable).
        admin: Some(signer_account(signer)?),
        code_id,
        label: Some(label.to_string()),
        msg: serde_json::to_vec(init).map_err(|e| CwError::Deploy(e.to_string()))?,
        funds: to_cosmrs_coins(funds)?,
    }
    .to_any()
    .map_err(|e| CwError::Deploy(format!("encode instantiate: {e}")))
}

/// Build the `MsgMigrateContract` migrating `contract` to `new_code_id` with `msg`, encoded as a
/// protobuf `Any`.
fn migrate_msg<Migrate: CwSerde>(
    contract: &Addr,
    new_code_id: u64,
    msg: &Migrate,
    signer: &CosmosSigner,
) -> Result<cosmrs::Any, CwError> {
    let bytes = serde_json::to_vec(msg).map_err(|e| CwError::Deploy(e.to_string()))?;
    migrate_msg_bytes(contract, new_code_id, bytes, signer)
}

/// Build the `MsgMigrateContract` migrating `contract` to `new_code_id`, carrying `msg` already
/// serialized to JSON bytes. Shared by the typed [`migrate_msg`] and the batch path, which
/// serializes its members at build time.
fn migrate_msg_bytes(
    contract: &Addr,
    new_code_id: u64,
    msg: Vec<u8>,
    signer: &CosmosSigner,
) -> Result<cosmrs::Any, CwError> {
    MsgMigrateContract {
        sender: signer_account(signer)?,
        contract: contract
            .as_str()
            .parse()
            .map_err(|e| CwError::Deploy(format!("contract addr: {e}")))?,
        code_id: new_code_id,
        msg,
    }
    .to_any()
    .map_err(|e| CwError::Deploy(format!("encode migrate: {e}")))
}

/// Build the `MsgExecuteContract` running `msg` against `addr`, encoded as a protobuf `Any`.
fn execute_msg<Exec: CwSerde>(
    addr: &Addr,
    msg: &Exec,
    signer: &CosmosSigner,
    funds: &[Coin],
) -> Result<cosmrs::Any, CwError> {
    let bytes = serde_json::to_vec(msg).map_err(|e| CwError::Execute(e.to_string()))?;
    execute_msg_bytes(addr, bytes, signer, funds)
}

/// Build the `MsgExecuteContract` running `msg` (already serialized to JSON bytes) against `addr`,
/// encoded as a protobuf `Any`. Shared by the typed [`execute_msg`] and the batch path, which
/// serializes its members at build time.
fn execute_msg_bytes(
    addr: &Addr,
    msg: Vec<u8>,
    signer: &CosmosSigner,
    funds: &[Coin],
) -> Result<cosmrs::Any, CwError> {
    MsgExecuteContract {
        sender: signer_account(signer)?,
        contract: addr
            .as_str()
            .parse()
            .map_err(|e| CwError::Execute(format!("contract addr: {e}")))?,
        msg,
        funds: to_cosmrs_coins(funds)?,
    }
    .to_any()
    .map_err(|e| CwError::Execute(format!("encode execute: {e}")))
}

/// Map every member of `batch` to the protobuf `Any` messages the RPC path signs into one tx.
fn batch_msgs(batch: &CwBatch, signer: &CosmosSigner) -> Result<Vec<cosmrs::Any>, CwError> {
    batch
        .members()
        .iter()
        .map(|m| batch_member_to_any(m, signer))
        .collect()
}

/// Map a single [`CwBatchMember`] to the protobuf `Any` the RPC path signs, filling in `signer`
/// as the sender (unknown until broadcast). A [`CwBatchMember::Raw`] member passes through
/// verbatim.
fn batch_member_to_any(
    member: &CwBatchMember,
    signer: &CosmosSigner,
) -> Result<cosmrs::Any, CwError> {
    match member {
        CwBatchMember::Execute {
            contract,
            msg,
            funds,
        } => execute_msg_bytes(contract, msg.clone(), signer, funds),
        CwBatchMember::Send { to, amount, denom } => transfer_msg(to, denom, *amount, signer),
        CwBatchMember::Migrate {
            contract,
            new_code_id,
            msg,
        } => migrate_msg_bytes(contract, *new_code_id, msg.clone(), signer),
        CwBatchMember::Raw(any) => Ok(any.clone()),
    }
}

/// Build the bank `MsgSend` moving `amount` `denom` to `to`, encoded as a protobuf `Any`.
fn transfer_msg(
    to: &Addr,
    denom: &str,
    amount: u128,
    signer: &CosmosSigner,
) -> Result<cosmrs::Any, CwError> {
    MsgSend {
        from_address: signer_account(signer)?,
        to_address: to
            .as_str()
            .parse()
            .map_err(|e| CwError::Execute(format!("recipient addr: {e}")))?,
        amount: vec![CosmrsCoin {
            denom: denom
                .parse::<Denom>()
                .map_err(|e| CwError::Execute(format!("denom {denom}: {e}")))?,
            amount,
        }],
    }
    .to_any()
    .map_err(|e| CwError::Execute(format!("encode transfer: {e}")))
}

/// Encode the transaction the Simulate endpoint expects: fully assembled, dummy-signed.
///
/// Two deliberate oddities here, both simulation-specific and NOT bugs:
///
/// - The signature is a single empty byte string. Simulate runs the tx through the same ante
///   chain as delivery but in simulate mode, where `SigVerificationDecorator` requires one
///   signature slot per signer yet skips verifying its contents. Sending an empty signature is
///   the established convention (cosmjs and the SDK's own tx factory do the same); a real
///   signature would also pass, it would just spend a signing pass to prove nothing.
/// - The fee is zero coins with a zero gas limit. Simulation runs on an infinite gas meter and
///   skips fee deduction, so no declared fee is needed and none skews the report.
///
/// The signer's *real* public key and sequence still go in: the ante handlers meter signature
/// verification by the declared key type, so omitting the key would understate the figure.
fn simulate_tx_bytes(
    msgs: Vec<cosmrs::Any>,
    public_key: cosmrs::crypto::PublicKey,
    sequence: u64,
) -> Result<Vec<u8>, CwError> {
    let body = Body::new(msgs, "", 0u16);
    let fee = Fee {
        amount: Vec::new(),
        gas_limit: 0,
        payer: None,
        granter: None,
    };
    let auth_info = SignerInfo::single_direct(Some(public_key), sequence).auth_info(fee);
    let raw = TxRaw {
        body_bytes: body
            .into_bytes()
            .map_err(|e| CwError::Execute(format!("encode tx body: {e}")))?,
        auth_info_bytes: auth_info
            .into_bytes()
            .map_err(|e| CwError::Execute(format!("encode auth info: {e}")))?,
        signatures: vec![Vec::new()],
    };
    Ok(raw.encode_to_vec())
}

/// Pull the simulated gas figure out of a raw Simulate response.
///
/// `gas_info` is optional in the proto but a successful simulation always carries it, so its
/// absence is a malformed response, not a zero.
fn parse_simulate_gas(bytes: &[u8]) -> Result<u64, CwError> {
    let resp = SimulateResponse::decode(bytes)
        .map_err(|e| CwError::Rpc(format!("decode SimulateResponse: {e}")))?;
    resp.gas_info
        .map(|g| g.gas_used)
        .ok_or_else(|| CwError::Rpc("simulate response carried no gas_info".into()))
}

/// Scale a simulated gas figure into the limit a transaction declares: `ceil(simulated * adj)`.
///
/// `adjustment` is the chain's [`CosmosChainInfo::gas_adjustment`], validated `>= 1.0` where it is
/// configured, so this only ever rounds a limit up. The float cast saturates at [`u64::MAX`]
/// rather than wrapping.
fn adjust_gas(simulated: u64, adjustment: f64) -> u64 {
    (simulated as f64 * adjustment).ceil() as u64
}

/// The fee a transaction declares for `gas_limit`: `ceil(gas_limit * gas_price)`, in base units of
/// the chain's native denom.
///
/// A pure function of the *resolved* limit, whichever way that limit was arrived at. An
/// `Estimated` limit already carries the chain's `gas_adjustment` ([`adjust_gas`]); multiplying
/// here again would apply it twice and silently overpay by that factor. The SDK deducts the
/// declared fee in full and refunds no unspent gas, so an overpayment is real money, not headroom.
fn fee_for(gas_limit: u64, gas_price: f64) -> u128 {
    (gas_limit as f64 * gas_price).ceil() as u128
}

/// The [`CwGas`] an estimate reports, shaped exactly like a receipt so the two compare directly.
///
/// `used` is the node's raw simulated figure, the forecast of the `gas_used` a receipt reports.
/// `fee` is what a broadcast under [`CwGasLimit::Estimated`] would actually declare and pay:
/// the fee for the *adjusted* limit ([`fee_for`] of [`adjust_gas`]), not for the raw figure,
/// because the SDK deducts the declared fee in full and the declared limit carries the
/// adjustment. A fee computed from the raw figure would systematically undershoot every receipt.
fn estimated_gas(simulated: u64, adjustment: f64, gas_price: f64) -> CwGas {
    CwGas {
        used: simulated,
        fee: fee_for(adjust_gas(simulated, adjustment), gas_price),
    }
}

/// The signer's bech32 address as a cosmrs [`AccountId`].
fn signer_account(signer: &CosmosSigner) -> Result<AccountId, CwError> {
    signer
        .address
        .as_str()
        .parse()
        .map_err(|e| CwError::Execute(format!("sender addr: {e}")))
}

/// Convert cosmwasm [`Coin`]s into cosmrs coins for a tx message.
fn to_cosmrs_coins(funds: &[Coin]) -> Result<Vec<CosmrsCoin>, CwError> {
    funds
        .iter()
        .map(|c| {
            Ok(CosmrsCoin {
                denom: c
                    .denom
                    .parse::<Denom>()
                    .map_err(|e| CwError::Execute(format!("denom {}: {e}", c.denom)))?,
                amount: c.amount.u128(),
            })
        })
        .collect()
}

/// Find an attribute value within the first event of `event_type` in a tx result.
fn find_attr(events: &[TmEvent], event_type: &str, key: &str) -> Result<String, CwError> {
    for ev in events.iter().filter(|e| e.kind == event_type) {
        for attr in &ev.attributes {
            if attr.key_str().map(|k| k == key).unwrap_or(false) {
                return attr
                    .value_str()
                    .map(|v| v.to_string())
                    .map_err(|e| CwError::Execute(format!("attr {key}: {e}")));
            }
        }
    }
    Err(CwError::Execute(format!(
        "event '{event_type}' attribute '{key}' not found in tx result"
    )))
}

/// Map a Tendermint ABCI event to a `cosmwasm_std::Event`.
fn to_cw_event(ev: &TmEvent) -> Event {
    let mut out = Event::new(ev.kind.clone());
    for attr in &ev.attributes {
        if let (Ok(k), Ok(v)) = (attr.key_str(), attr.value_str()) {
            out = out.add_attribute(k, v);
        }
    }
    out
}

impl ChainProvider for CwRpcProvider {
    type Spec = CosmosChainInfo;
    type Address = Addr;
    type Account = Addr;
    type Balance = u128;
    type Error = CwError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, label: &str) -> Addr {
        // No signing backend in the read-only phase; return a deterministic placeholder
        // address. Real key derivation arrives with the write (sign + broadcast) pass.
        label.into_bech32_with_prefix(self.info.bech32_prefix)
    }

    async fn balance(&self, addr: &Addr) -> Result<u128, CwError> {
        let req = QueryBalanceRequest {
            address: addr.to_string(),
            denom: self.info.native_denom.to_string(),
        };
        let bytes = self
            .abci_query("/cosmos.bank.v1beta1.Query/Balance", req.encode_to_vec())
            .await?;
        let resp = QueryBalanceResponse::decode(bytes.as_slice())
            .map_err(|e| CwError::Balance(e.to_string()))?;
        match resp.balance {
            Some(coin) => coin
                .amount
                .parse::<u128>()
                .map_err(|e| CwError::Balance(e.to_string())),
            None => Ok(0),
        }
    }

    async fn set_balance(
        &mut self,
        _addr: &Addr,
        _denom: &str,
        _amount: u128,
    ) -> Result<(), CwError> {
        // Cannot mint on a real chain. Use a faucet; declared funding is validated, not minted.
        Err(CwError::Unimplemented("rpc set_balance".into()))
    }

    async fn block_height(&self) -> u64 {
        self.try_block_height().await.unwrap_or(0)
    }

    async fn advance_blocks(&mut self, _n: u64, _time: BlockTime) {
        // No-op: a real chain advances on its own; tests poll instead of forcing blocks.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmrs::proto::cosmos::base::abci::v1beta1::GasInfo;
    use cosmrs::proto::cosmos::tx::v1beta1::{AuthInfo as ProtoAuthInfo, TxBody};

    #[test]
    fn parse_simulate_gas_reads_gas_used() {
        let resp = SimulateResponse {
            gas_info: Some(GasInfo {
                gas_wanted: 0,
                gas_used: 173_456,
            }),
            result: None,
        };
        assert_eq!(parse_simulate_gas(&resp.encode_to_vec()).unwrap(), 173_456);
    }

    #[test]
    fn parse_simulate_gas_rejects_a_response_without_gas_info() {
        let resp = SimulateResponse {
            gas_info: None,
            result: None,
        };
        let err = parse_simulate_gas(&resp.encode_to_vec()).unwrap_err();
        assert!(matches!(err, CwError::Rpc(_)), "unexpected error: {err:?}");
    }

    #[test]
    fn parse_simulate_gas_rejects_garbage_bytes() {
        // 0xFF opens a field with the invalid tag 0.
        assert!(parse_simulate_gas(&[0xFF, 0xFF, 0xFF]).is_err());
    }

    #[test]
    fn adjust_gas_scales_the_simulated_figure_and_rounds_up() {
        assert_eq!(adjust_gas(100_000, 1.3), 130_000);
        // Never rounds a limit down: 3 * 1.3 = 3.9 gas is 4 gas of headroom, not 3.
        assert_eq!(adjust_gas(3, 1.3), 4);
        // An adjustment of exactly 1.0 (the configured floor) is the raw simulated figure.
        assert_eq!(adjust_gas(173_456, 1.0), 173_456);
    }

    /// The `gas_adjustment` scales the gas *limit* and nothing else. The fee is a pure function of
    /// the resolved limit, so it inherits the adjustment exactly once, through the limit. Applying
    /// it a second time to the fee (the shape of the deleted `FEE_BUFFER`) would overpay by the
    /// adjustment factor on every transaction, and the SDK refunds none of it.
    #[test]
    fn the_gas_adjustment_reaches_the_fee_exactly_once() {
        const GAS_PRICE: f64 = 0.025;
        const ADJUSTMENT: f64 = 1.3;

        let limit = adjust_gas(100_000, ADJUSTMENT);
        assert_eq!(limit, 130_000);
        assert_eq!(fee_for(limit, GAS_PRICE), 3_250);

        // What double-applying would produce, spelled out so the two cannot be confused.
        let double_applied = (limit as f64 * GAS_PRICE * ADJUSTMENT).ceil() as u128;
        assert_eq!(double_applied, 4_225);
        assert_ne!(fee_for(limit, GAS_PRICE), double_applied);

        // And an `Exact` limit of the same size costs the same: the fee cannot tell where the
        // limit came from.
        assert_eq!(fee_for(130_000, GAS_PRICE), fee_for(limit, GAS_PRICE));
    }

    /// An estimate is a receipt-shaped forecast: `used` is the raw simulated figure (what the
    /// receipt's `used` forecasts), while `fee` prices the *adjusted* limit, because that is
    /// what a broadcast under `Estimated` declares and the SDK deducts in full. Pricing the raw
    /// figure instead would undershoot every receipt by the adjustment factor.
    #[test]
    fn an_estimate_reports_raw_gas_but_prices_the_adjusted_limit() {
        let est = estimated_gas(100_000, 1.3, 0.025);
        assert_eq!(est.used, 100_000, "used is the node's raw simulated figure");
        // fee_for(adjust_gas(100_000, 1.3)) = fee_for(130_000) = 3_250, not fee_for(100_000).
        assert_eq!(est.fee, 3_250);
        assert_ne!(est.fee, fee_for(100_000, 0.025));

        // Exactly what a broadcast under `Estimated` would declare and pay for this simulation.
        assert_eq!(est.fee, fee_for(adjust_gas(100_000, 1.3), 0.025));
    }

    #[test]
    fn fee_rounds_up_so_a_sub_unit_fee_is_never_free() {
        // 1 gas at 0.025 is 0.025 base units, which the chain cannot express: pay 1, not 0.
        assert_eq!(fee_for(1, 0.025), 1);
        // A chain with a zero gas price (the LOCAL preset) declares a zero fee.
        assert_eq!(fee_for(300_000, 0.0), 0);
    }

    /// The simulate tx must decode back to exactly what the Simulate endpoint requires: the
    /// real message, the real public key and sequence, a zero fee, and one signature slot per
    /// signer holding an *empty* signature (present because the ante chain demands a slot,
    /// empty because simulate mode never verifies it).
    #[test]
    fn simulate_tx_carries_a_dummy_signature_and_zero_fee() {
        let key = cosmrs::crypto::secp256k1::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let account = AccountId::new("osmo", &[7u8; 20]).unwrap();
        let msg = MsgSend {
            from_address: account.clone(),
            to_address: account,
            amount: vec![CosmrsCoin {
                denom: "uosmo".parse().unwrap(),
                amount: 1,
            }],
        }
        .to_any()
        .unwrap();

        let bytes = simulate_tx_bytes(vec![msg], key.public_key(), 42).unwrap();

        let raw = TxRaw::decode(bytes.as_slice()).unwrap();
        assert_eq!(
            raw.signatures,
            vec![Vec::<u8>::new()],
            "exactly one signature slot, empty"
        );

        let auth = ProtoAuthInfo::decode(raw.auth_info_bytes.as_slice()).unwrap();
        let fee = auth.fee.expect("fee present");
        assert_eq!(fee.gas_limit, 0, "simulation declares no gas limit");
        assert!(fee.amount.is_empty(), "simulation declares no fee");
        assert_eq!(auth.signer_infos.len(), 1);
        assert_eq!(auth.signer_infos[0].sequence, 42);
        assert!(
            auth.signer_infos[0].public_key.is_some(),
            "the real key must be declared: ante handlers meter sig verification by key type"
        );

        let body = TxBody::decode(raw.body_bytes.as_slice()).unwrap();
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].type_url, "/cosmos.bank.v1beta1.MsgSend");
    }

    /// A fake [`CosmosTransport`] answering `/status` with a canned node status at a fixed
    /// height. Proves the whole `perform` path (envelope out, typed parse back) without a node.
    struct StatusTransport {
        height: u64,
    }

    impl CosmosTransport for StatusTransport {
        fn call(&self, request: String) -> crate::transport::TransportFuture<'_> {
            Box::pin(async move {
                let req: serde_json::Value =
                    serde_json::from_str(&request).expect("request envelope is JSON");
                assert_eq!(req["method"], "status", "try_block_height sends /status");
                // The response body a CometBFT 0.38 node returns for /status (trimmed from the
                // tendermint-rpc kvstore fixture), with the id echoed back per JSON-RPC.
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "result": {
                        "node_info": {
                            "channels": "40202122233038606100",
                            "id": "cf4a66aa29e5123abfdfbdf485da6788bb8e46d1",
                            "listen_addr": "tcp://0.0.0.0:26656",
                            "moniker": "fake-node",
                            "network": "cosmos-testing",
                            "other": {"rpc_address": "tcp://0.0.0.0:26657", "tx_index": "on"},
                            "protocol_version": {"app": "1", "block": "11", "p2p": "8"},
                            "version": "0.38.0"
                        },
                        "sync_info": {
                            "catching_up": false,
                            "earliest_app_hash": "0000000000000000",
                            "earliest_block_hash":
                                "6CD5CF4E23A49D9BC073D6F305D29D1B8B5193B534C237696D42FEA5AFBCD520",
                            "earliest_block_height": "1",
                            "earliest_block_time": "2023-05-17T14:12:48.347696215Z",
                            "latest_app_hash": "0600000000000000",
                            "latest_block_hash":
                                "B647CF507155BADAC86FADD00E38B065C63A84953A847AA9FA99DB1CEE6C4DA9",
                            "latest_block_height": self.height.to_string(),
                            "latest_block_time": "2023-05-17T14:14:48.530153458Z"
                        },
                        "validator_info": {
                            "address": "2DD9F44FD9067555C322243C3C913BA7B51D2BE0",
                            "pub_key": {
                                "type": "tendermint/PubKeyEd25519",
                                "value": "bNNlGls5R25wC3Sd8720F/3+7IZBhXcD22MNFtPk/v0="
                            },
                            "voting_power": "10"
                        }
                    }
                })
                .to_string())
            })
        }
    }

    #[tokio::test]
    async fn try_block_height_rides_an_injected_transport() {
        let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
        let provider = CwRpcProvider::new_with_transport(
            crate::chains::LOCAL,
            wallets,
            Rc::new(StatusTransport { height: 4242 }),
        );

        let height = provider.try_block_height().await.expect("status parses");
        assert_eq!(height, 4242);
    }
}
