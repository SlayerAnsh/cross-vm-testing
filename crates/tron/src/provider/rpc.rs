//! Live java-tron RPC provider over the TronGrid HTTP REST API.
//!
//! [`TronRpcProvider`] mirrors the EVM `EvmRpcProvider`: chain reads ([`balance`],
//! [`block_height`]) and read-only [`static_call`] need no signer; the write paths
//! ([`deploy_create`], [`call`]) sign the transaction id with the wallet's secp256k1 key and
//! broadcast. Only `set_balance` stays [`TronError::Unimplemented`] (a live chain cannot mint).
//!
//! Transport is TronGrid HTTP (`/wallet/*`), not gRPC, so the crate keeps no Tron-specific
//! dependency (just `reqwest` + `serde_json`). The flow for a write is the standard java-tron
//! three step: build the unsigned transaction at the node (`/wallet/deploycontract` or
//! `/wallet/triggersmartcontract`), sign its `txID` locally, then `/wallet/broadcasttransaction`.
//! Addresses cross the wire in 0x41 hex form (`visible=false`).
//!
//! [`balance`]: TronRpcProvider::balance
//! [`block_height`]: ChainProvider::block_height
//! [`static_call`]: TronRpcProvider::static_call
//! [`deploy_create`]: TronRpcProvider::deploy_create
//! [`call`]: TronRpcProvider::call

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use alloy_primitives::{Address, Bytes, Log, LogData, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use serde_json::{json, Value};

use crate::chains::TronChainInfo;
use crate::error::TronError;
use crate::provider::address::{address_from_label, address_from_pubkey, TronAddress};
use crate::provider::execution::TronExecution;

/// Default fee ceiling for a write, in sun (1000 TRX). Tron rejects a tx that would burn more.
const DEFAULT_FEE_LIMIT: u64 = 1_000_000_000;
/// Energy cap a deployed contract may borrow from a caller, per invocation.
const DEFAULT_ORIGIN_ENERGY_LIMIT: u64 = 10_000_000;
/// How many times to poll `gettransactioninfobyid` for a broadcast tx's receipt.
const TX_POLL_ATTEMPTS: u32 = 20;
/// Delay between receipt polls (Nile/Tron block time is ~3s).
const TX_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// A live-RPC Tron provider over the TronGrid HTTP API.
#[derive(Clone)]
pub struct TronRpcProvider {
    info: TronChainInfo,
    rpc_url: String,
    http: reqwest::Client,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, PrivateKeySigner>>>,
}

impl TronRpcProvider {
    /// Create an RPC provider bound to a chain's metadata.
    ///
    /// Stays infallible so `NILE.rpc(wallets)` sugar keeps working; a missing or empty `rpc_url`
    /// surfaces as an error at the first network call instead.
    pub fn new(info: TronChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let rpc_url = info.rpc_url.unwrap_or("").trim_end_matches('/').to_string();
        Self {
            info,
            rpc_url,
            http: reqwest::Client::new(),
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// POST `body` to a `/wallet/<path>` endpoint and return the decoded JSON.
    async fn post(&self, path: &str, body: Value) -> Result<Value, TronError> {
        if self.rpc_url.is_empty() {
            return Err(TronError::Rpc(format!(
                "chain '{}' has no rpc_url; use a chain preset with an endpoint",
                self.info.chain_id
            )));
        }
        let url = format!("{}/wallet/{path}", self.rpc_url);
        self.http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| TronError::Rpc(e.to_string()))?
            .json::<Value>()
            .await
            .map_err(|e| TronError::Rpc(format!("decode {path}: {e}")))
    }

    /// Current block number. Inherent fallible variant of the trait's infallible
    /// [`ChainProvider::block_height`].
    pub async fn try_block_height(&self) -> Result<u64, TronError> {
        let v = self.post("getnowblock", json!({})).await?;
        v["block_header"]["raw_data"]["number"]
            .as_u64()
            .ok_or_else(|| TronError::Rpc("getnowblock: missing block number".into()))
    }

    /// Deploy bytecode via a create transaction signed by `signer`, returning the new contract
    /// address the node assigns.
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
    ) -> Result<TronAddress, TronError> {
        let owner = signer_address(signer);
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args.as_ref());
        let unsigned = self
            .post(
                "deploycontract",
                json!({
                    "owner_address": owner.to_hex(),
                    "abi": "[]",
                    "bytecode": hex::encode(&initcode),
                    "fee_limit": DEFAULT_FEE_LIMIT,
                    "call_value": 0,
                    "consume_user_resource_percent": 100,
                    "origin_energy_limit": DEFAULT_ORIGIN_ENERGY_LIMIT,
                    "visible": false,
                }),
            )
            .await?;
        check_node_ok(&unsigned, "deploycontract")?;
        let contract_hex = unsigned["contract_address"]
            .as_str()
            .or_else(|| {
                unsigned["raw_data"]["contract"][0]["parameter"]["value"]["contract_address"]
                    .as_str()
            })
            .ok_or_else(|| TronError::Deploy("deploycontract: no contract_address".into()))?;
        let addr = tron_address_from_hex(contract_hex).map_err(|e| TronError::Deploy(e.0))?;
        self.sign_and_broadcast(unsigned, signer)
            .await
            .map_err(|e| TronError::Deploy(e.to_string()))?;
        Ok(addr)
    }

    /// Execute a state-mutating call against `to`, signed by `signer`.
    ///
    /// java-tron's broadcast carries no logs, so after broadcasting this polls
    /// `gettransactioninfobyid` until the transaction is mined (or times out), then returns the
    /// `TransactionInfo`'s return data and EVM-shaped logs. An on-chain failure (revert,
    /// out-of-energy) surfaces as [`TronError::Execute`].
    pub async fn call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
    ) -> Result<TronExecution, TronError> {
        self.call_value(to, calldata, signer, U256::ZERO).await
    }

    /// Execute a state-mutating call against `to` carrying `value` sun (a payable call), signed by
    /// `signer`. On a live chain the signer must already hold the value (no minting). `value` is
    /// sun (native TRX base unit), narrowed to the `u64` `call_value` java-tron expects.
    pub async fn call_value(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
        value: U256,
    ) -> Result<TronExecution, TronError> {
        let owner = signer_address(signer);
        let resp = self
            .post(
                "triggersmartcontract",
                json!({
                    "owner_address": owner.to_hex(),
                    "contract_address": to.to_hex(),
                    "data": hex::encode(calldata.as_ref()),
                    "call_value": value.saturating_to::<u64>(),
                    "fee_limit": DEFAULT_FEE_LIMIT,
                    "visible": false,
                }),
            )
            .await?;
        check_node_ok(&resp["result"], "triggersmartcontract")?;
        let unsigned = resp["transaction"].clone();
        let txid = unsigned["txID"]
            .as_str()
            .ok_or_else(|| TronError::Execute("triggersmartcontract: no txID".into()))?
            .to_string();
        self.sign_and_broadcast(unsigned, signer)
            .await
            .map_err(|e| TronError::Execute(e.to_string()))?;
        let info = self.await_tx_info(&txid).await?;
        parse_tx_info(&info, &txid)
    }

    /// Poll `gettransactioninfobyid` until the transaction is mined, returning its
    /// `TransactionInfo`. Errors if the receipt does not appear within the poll budget.
    async fn await_tx_info(&self, txid: &str) -> Result<Value, TronError> {
        for _ in 0..TX_POLL_ATTEMPTS {
            let info = self
                .post("gettransactioninfobyid", json!({ "value": txid }))
                .await?;
            // An unmined tx returns `{}`; a mined one carries its `id`.
            if info.get("id").and_then(Value::as_str).is_some() {
                return Ok(info);
            }
            tokio::time::sleep(TX_POLL_INTERVAL).await;
        }
        Err(TronError::Execute(format!(
            "transaction {txid} not confirmed after {}s",
            u64::from(TX_POLL_ATTEMPTS) * TX_POLL_INTERVAL.as_secs()
        )))
    }

    /// Run a read-only constant call (`triggerconstantcontract`) against `to`.
    pub async fn static_call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, TronError> {
        let resp = self
            .post(
                "triggerconstantcontract",
                json!({
                    // The zero address is a valid caller for a constant (no-state-change) call.
                    "owner_address": "410000000000000000000000000000000000000000",
                    "contract_address": to.to_hex(),
                    "data": hex::encode(calldata.as_ref()),
                    "visible": false,
                }),
            )
            .await?;
        check_node_ok(&resp["result"], "triggerconstantcontract")?;
        let hexstr = resp["constant_result"][0].as_str().unwrap_or("");
        let bytes = hex::decode(hexstr)
            .map_err(|e| TronError::Query(format!("constant_result hex: {e}")))?;
        Ok(Bytes::from(bytes))
    }

    /// Sign an unsigned transaction's `txID` with `signer` and broadcast it.
    async fn sign_and_broadcast(
        &self,
        mut tx: Value,
        signer: &PrivateKeySigner,
    ) -> Result<(), TronError> {
        let txid_hex = tx["txID"]
            .as_str()
            .ok_or_else(|| TronError::Rpc("transaction has no txID".into()))?;
        let txid =
            hex::decode(txid_hex).map_err(|e| TronError::Rpc(format!("bad txID hex: {e}")))?;
        let sig = sign_txid(signer, &txid)?;
        tx["signature"] = json!([hex::encode(sig)]);
        let res = self.post("broadcasttransaction", tx).await?;
        if res["result"].as_bool() == Some(true) {
            return Ok(());
        }
        let code = res["code"].as_str().unwrap_or("FAILED");
        let msg = res["message"]
            .as_str()
            .map(decode_hex_message)
            .unwrap_or_default();
        Err(TronError::Rpc(format!(
            "broadcast rejected ({code}): {msg}"
        )))
    }
}

/// The Tron address a secp256k1 signer controls.
fn signer_address(signer: &PrivateKeySigner) -> TronAddress {
    let encoded = signer.credential().verifying_key().to_encoded_point(false);
    address_from_pubkey(encoded.as_bytes())
}

/// Sign the 32-byte `txID` with secp256k1, returning the 65-byte `r || s || v` Tron signature.
fn sign_txid(signer: &PrivateKeySigner, txid: &[u8]) -> Result<[u8; 65], TronError> {
    let (sig, recid) = signer
        .credential()
        .sign_prehash_recoverable(txid)
        .map_err(|e| TronError::Wallet(format!("sign txID: {e}")))?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = recid.to_byte();
    Ok(out)
}

/// A small error wrapper so address parsing can flow through the various `TronError` variants.
struct AddrErr(String);

/// Parse a node-returned 0x41 hex address into a [`TronAddress`].
fn tron_address_from_hex(h: &str) -> Result<TronAddress, AddrErr> {
    let bytes = hex::decode(h).map_err(|e| AddrErr(format!("address hex: {e}")))?;
    if bytes.len() != 21 || bytes[0] != 0x41 {
        return Err(AddrErr(format!("not a 0x41 address: {h}")));
    }
    Ok(TronAddress::from_evm(
        alloy_primitives::Address::from_slice(&bytes[1..]),
    ))
}

/// java-tron encodes some error messages as hex; decode to UTF-8 when it parses, else pass through.
fn decode_hex_message(m: &str) -> String {
    match hex::decode(m) {
        Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| m.to_string()),
        Err(_) => m.to_string(),
    }
}

/// Surface a node-level `{result:{result:false, code, message}}` (or bare `{Error}`) as an error.
fn check_node_ok(result: &Value, ctx: &str) -> Result<(), TronError> {
    if let Some(err) = result["Error"].as_str() {
        return Err(TronError::Rpc(format!("{ctx}: {err}")));
    }
    // `triggerconstantcontract`/`triggersmartcontract` wrap status in `result.result`.
    if result.get("result").is_some() && result["result"].as_bool() == Some(false) {
        let msg = result["message"]
            .as_str()
            .map(decode_hex_message)
            .unwrap_or_default();
        return Err(TronError::Rpc(format!("{ctx} failed: {msg}")));
    }
    Ok(())
}

/// Map a mined `TransactionInfo` into a [`TronExecution`], surfacing an on-chain failure as an
/// error. Tron logs are EVM-shaped; the log `address` is the 20-byte form without the `0x41`
/// prefix. Source: <https://developers.tron.network/docs/event>
fn parse_tx_info(info: &Value, txid: &str) -> Result<TronExecution, TronError> {
    // A reverted / out-of-energy tx is `result == "FAILED"` (top level) or a non-`SUCCESS`
    // `receipt.result`; mirror the mock, which errors rather than returning a bad success.
    let failed = info["result"].as_str() == Some("FAILED")
        || info["receipt"]["result"]
            .as_str()
            .is_some_and(|r| r != "SUCCESS");
    if failed {
        let msg = info["resMessage"]
            .as_str()
            .map(decode_hex_message)
            .unwrap_or_default();
        let reason = info["receipt"]["result"].as_str().unwrap_or("FAILED");
        return Err(TronError::Execute(format!("tx {reason}: {msg}")));
    }

    let output = info["contractResult"][0]
        .as_str()
        .and_then(|h| hex::decode(h).ok())
        .map(Bytes::from)
        .unwrap_or_default();

    let mut logs = Vec::new();
    if let Some(entries) = info["log"].as_array() {
        for l in entries {
            let address = l["address"]
                .as_str()
                .and_then(|h| hex::decode(h).ok())
                .filter(|b| b.len() == 20)
                .map(|b| Address::from_slice(&b))
                .unwrap_or_default();
            let topics = l["topics"]
                .as_array()
                .map(|ts| {
                    ts.iter()
                        .filter_map(Value::as_str)
                        .filter_map(|h| hex::decode(h).ok())
                        .filter(|b| b.len() == 32)
                        .map(|b| B256::from_slice(&b))
                        .collect()
                })
                .unwrap_or_default();
            let data = l["data"]
                .as_str()
                .and_then(|h| hex::decode(h).ok())
                .unwrap_or_default();
            logs.push(Log {
                address,
                data: LogData::new_unchecked(topics, Bytes::from(data)),
            });
        }
    }

    let tx_hash = hex::decode(txid)
        .ok()
        .filter(|b| b.len() == 32)
        .map(|b| B256::from_slice(&b));

    Ok(TronExecution {
        output,
        logs,
        tx_hash,
    })
}

impl ChainProvider for TronRpcProvider {
    type Spec = TronChainInfo;
    type Address = TronAddress;
    type Account = TronAddress;
    type Balance = u64;
    type Error = TronError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, label: &str) -> TronAddress {
        // No signing backend here; the real address comes from the wallet roster via the chain
        // handle's `wallet_address`. Return a deterministic placeholder, as the EVM RPC does.
        address_from_label(label)
    }

    async fn balance(&self, addr: &TronAddress) -> Result<u64, TronError> {
        let v = self
            .post(
                "getaccount",
                json!({ "address": addr.to_hex(), "visible": false }),
            )
            .await?;
        // An account that has never transacted returns `{}`; treat a missing balance as zero.
        Ok(v["balance"].as_u64().unwrap_or(0))
    }

    async fn set_balance(
        &mut self,
        _addr: &TronAddress,
        _denom: &str,
        _amount: u64,
    ) -> Result<(), TronError> {
        // Cannot mint on a real chain. Use a faucet; declared funding is validated, not minted.
        Err(TronError::Unimplemented("rpc set_balance".into()))
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
    use crate::chains::{LOCAL, NILE};
    use cross_vm_core::{ChainProvider, WalletFactory};
    use std::rc::Rc;

    #[tokio::test]
    async fn set_balance_unimplemented() {
        let mut c = TronRpcProvider::new(NILE, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let a = c.new_account("x").await;
        assert!(matches!(
            c.set_balance(&a, "TRX", 1).await,
            Err(TronError::Unimplemented(_))
        ));
    }

    #[tokio::test]
    async fn new_account_is_tron_shaped() {
        let mut c = TronRpcProvider::new(NILE, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let a = c.new_account("x").await;
        assert!(a.to_base58().starts_with('T'));
    }

    #[tokio::test]
    async fn no_endpoint_errors_offline() {
        // LOCAL has no rpc_url, so a network call fails fast without touching the network.
        let c = TronRpcProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let signer = PrivateKeySigner::random();
        let res = c
            .deploy_create(Bytes::new(), Vec::<u8>::new(), &signer)
            .await;
        assert!(matches!(res, Err(TronError::Deploy(_) | TronError::Rpc(_))));
    }

    #[test]
    fn signer_address_is_tron_shaped() {
        let signer = PrivateKeySigner::random();
        assert!(signer_address(&signer).to_base58().starts_with('T'));
    }
}
