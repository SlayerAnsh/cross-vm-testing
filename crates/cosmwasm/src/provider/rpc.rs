//! Live-RPC CosmWasm provider.
//!
//! [`CwRpcProvider`] talks to a real Cosmos node over Tendermint RPC. Read paths use ABCI
//! queries with no signer: [`block_height`], [`balance`], and [`query_wasm_smart`]. Write paths
//! ([`store_code_wasm`], [`instantiate`], [`execute_contract`]) sign with the wallet's secp256k1
//! key (account number + sequence + `SignDoc` + `broadcast_tx_commit`) and broadcast; only
//! `set_balance` stays [`CwError::Unimplemented`] (a live chain cannot mint).
//!
//! [`block_height`]: CwRpcProvider::block_height
//! [`balance`]: CwRpcProvider::balance
//! [`query_wasm_smart`]: CwRpcProvider::query_wasm_smart
//! [`store_code_wasm`]: CwRpcProvider::store_code_wasm
//! [`instantiate`]: CwRpcProvider::instantiate
//! [`execute_contract`]: CwRpcProvider::execute_contract

use cosmrs::proto::cosmos::auth::v1beta1::{
    BaseAccount, QueryAccountRequest, QueryAccountResponse,
};
use cosmrs::proto::cosmos::bank::v1beta1::{QueryBalanceRequest, QueryBalanceResponse};
use cosmrs::proto::cosmwasm::wasm::v1::{
    QuerySmartContractStateRequest, QuerySmartContractStateResponse,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cosmrs::cosmwasm::{MsgExecuteContract, MsgInstantiateContract, MsgStoreCode};
use cosmrs::rpc::{Client, HttpClient};
use cosmrs::tendermint::abci::Event as TmEvent;
use cosmrs::tx::{Body, Fee, Msg, SignDoc, SignerInfo};
use cosmrs::{AccountId, Coin as CosmrsCoin, Denom};
use cosmwasm_std::{Addr, Coin, Event};
use cross_vm_core::{ChainProvider, FundError, WalletFactory};
use cw_multi_test::IntoBech32;
use prost::Message;

use crate::asset::CwAsset;
use crate::chains::CosmosChainInfo;
use crate::error::CwError;
use crate::msg::CwSerde;
use crate::provider::CwCode;
use crate::wallet::CosmosSigner;

/// A live-RPC CosmWasm provider. Chain-level reads and contract queries hit a real node via
/// ABCI queries; the write paths ([`store_code_wasm`](Self::store_code_wasm),
/// [`instantiate`](Self::instantiate), [`execute_contract`](Self::execute_contract)) sign with
/// the wallet's secp256k1 key and broadcast. Only `set_balance` (and the trait-object
/// [`store_code`](Self::store_code)) stay [`CwError::Unimplemented`].
#[derive(Clone)]
pub struct CwRpcProvider {
    info: CosmosChainInfo,
    rpc_url: String,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, CosmosSigner>>>,
}

impl CwRpcProvider {
    /// Create an RPC provider bound to a chain's metadata.
    ///
    /// Stays infallible so `OSMOSIS_TESTNET.rpc(wallets)` sugar keeps working; a missing or empty
    /// `rpc_url` surfaces as an error at the first network call instead.
    pub fn new(info: CosmosChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let rpc_url = info.rpc_url.unwrap_or("").to_string();
        Self {
            info,
            rpc_url,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Build a Tendermint HTTP client for this chain's endpoint.
    ///
    /// Cheap (just constructs a reqwest client, no connection), so callers build per request.
    fn client(&self) -> Result<HttpClient, CwError> {
        if self.rpc_url.is_empty() {
            return Err(CwError::Rpc(format!(
                "chain '{}' has no rpc_url; use a chain preset with an endpoint",
                self.info.chain_id
            )));
        }
        HttpClient::new(self.rpc_url.as_str()).map_err(|e| CwError::Rpc(e.to_string()))
    }

    /// Run a raw ABCI query and return the response bytes.
    async fn abci_query(&self, path: &str, data: Vec<u8>) -> Result<Vec<u8>, CwError> {
        let client = self.client()?;
        let res = client
            .abci_query(Some(path.to_string()), data, None, false)
            .await
            .map_err(|e| CwError::Rpc(e.to_string()))?;
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
        let client = self.client()?;
        let status = client
            .status()
            .await
            .map_err(|e| CwError::Rpc(e.to_string()))?;
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

    /// Build, sign, and broadcast a transaction carrying `msgs`, waiting for it to commit.
    /// Returns the tx hash and the delivered events. Fails on a nonzero check/deliver code.
    async fn sign_and_broadcast(
        &self,
        msgs: Vec<cosmrs::Any>,
        signer: &CosmosSigner,
        gas_limit: u64,
    ) -> Result<(String, Vec<TmEvent>), CwError> {
        let client = self.client()?;
        let (account_number, sequence) = self.account_info(signer.address.as_str()).await?;

        let chain_id = self
            .info
            .chain_id
            .parse::<cosmrs::tendermint::chain::Id>()
            .map_err(|e| CwError::Rpc(format!("chain id: {e}")))?;
        let body = Body::new(msgs, "", 0u16);

        // Fee = ceil(gas_limit * gas_price * buffer) of the native denom. The buffer covers a
        // node min-gas-price higher than the preset's indicative `gas_price` (the rounding/excess
        // is refunded-style irrelevant on testnets and keeps the tx from bouncing on `check_tx`).
        const FEE_BUFFER: f64 = 2.0;
        let fee_amount = (gas_limit as f64 * self.info.gas_price * FEE_BUFFER).ceil() as u128;
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

        let resp = raw
            .broadcast_commit(&client)
            .await
            .map_err(|e| CwError::Rpc(format!("broadcast: {e}")))?;
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
        Ok((resp.hash.to_string(), resp.tx_result.events))
    }

    /// Upload raw wasm bytecode to the chain, signed by `signer`, and return its code id.
    ///
    /// This is the RPC analogue of the mock's [`store_code`](Self::store_code): the mock takes a
    /// `cw-multi-test` `Contract` object, a live chain takes compiled wasm bytes.
    pub async fn store_code_wasm(
        &self,
        wasm: Vec<u8>,
        signer: &CosmosSigner,
    ) -> Result<u64, CwError> {
        let msg = MsgStoreCode {
            sender: signer_account(signer)?,
            wasm_byte_code: wasm,
            instantiate_permission: None,
        };
        let any = msg
            .to_any()
            .map_err(|e| CwError::Execute(format!("encode store_code: {e}")))?;
        // Storing a contract is gas-heavy (scales with wasm size); a ~260 KB contract uses ~8M.
        let (_, events) = self
            .sign_and_broadcast(vec![any], signer, 15_000_000)
            .await?;
        find_attr(&events, "store_code", "code_id")?
            .parse::<u64>()
            .map_err(|e| CwError::Execute(format!("parse code_id: {e}")))
    }

    /// `store_code` is unavailable on the live RPC path because [`CwCode`] is a cw-multi-test
    /// trait object, not wasm bytes; use [`store_code_wasm`](Self::store_code_wasm) with compiled
    /// wasm instead.
    pub async fn store_code(&self, _code: CwCode) -> Result<u64, CwError> {
        Err(CwError::Unimplemented(
            "rpc store_code (use store_code_wasm with compiled wasm bytes)".into(),
        ))
    }

    /// Instantiate a contract from an uploaded code id, signed by `signer`.
    pub async fn instantiate<Init: CwSerde>(
        &self,
        code_id: u64,
        init: Init,
        signer: &CosmosSigner,
        funds: &[Coin],
        label: &str,
    ) -> Result<Addr, CwError> {
        let msg = MsgInstantiateContract {
            sender: signer_account(signer)?,
            admin: None,
            code_id,
            label: Some(label.to_string()),
            msg: serde_json::to_vec(&init).map_err(|e| CwError::Deploy(e.to_string()))?,
            funds: to_cosmrs_coins(funds)?,
        };
        let any = msg
            .to_any()
            .map_err(|e| CwError::Deploy(format!("encode instantiate: {e}")))?;
        let (_, events) = self.sign_and_broadcast(vec![any], signer, 400_000).await?;
        let addr = find_attr(&events, "instantiate", "_contract_address")?;
        Ok(Addr::unchecked(addr))
    }

    /// Execute a state-mutating message against a contract instance, signed by `signer`.
    ///
    /// The returned [`cw_multi_test::AppResponse`] carries the chain's emitted events (mapped to
    /// `cosmwasm_std::Event`); `data` is left `None` (the raw tx data is proto-wrapped, not the
    /// contract's response payload).
    pub async fn execute_contract<Exec: CwSerde>(
        &self,
        addr: &Addr,
        msg: Exec,
        signer: &CosmosSigner,
        funds: &[Coin],
    ) -> Result<cw_multi_test::AppResponse, CwError> {
        let m = MsgExecuteContract {
            sender: signer_account(signer)?,
            contract: addr
                .as_str()
                .parse()
                .map_err(|e| CwError::Execute(format!("contract addr: {e}")))?,
            msg: serde_json::to_vec(&msg).map_err(|e| CwError::Execute(e.to_string()))?,
            funds: to_cosmrs_coins(funds)?,
        };
        let any = m
            .to_any()
            .map_err(|e| CwError::Execute(format!("encode execute: {e}")))?;
        let (_, events) = self.sign_and_broadcast(vec![any], signer, 300_000).await?;
        Ok(cw_multi_test::AppResponse {
            events: events.iter().map(to_cw_event).collect(),
            data: None,
            msg_responses: Vec::new(),
        })
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

    async fn set_balance(&mut self, _addr: &Addr, _amount: u128) -> Result<(), CwError> {
        // Cannot mint on a real chain. Use a faucet; declared funding is validated, not minted.
        Err(CwError::Unimplemented("rpc set_balance".into()))
    }

    async fn block_height(&self) -> u64 {
        self.try_block_height().await.unwrap_or(0)
    }

    async fn advance_blocks(&mut self, _n: u64) {
        // No-op: a real chain advances on its own; tests poll instead of forcing blocks.
    }
}
