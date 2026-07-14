//! Live java-tron RPC provider over the TronGrid HTTP REST API.
//!
//! [`TronRpcProvider`] mirrors the EVM `EvmRpcProvider`: chain reads ([`balance`],
//! [`block_height`]) and read-only [`static_call`] need no signer; the write paths
//! ([`deploy_create`], [`call`], [`transfer_funds`]) sign the transaction id with the wallet's
//! secp256k1 key and broadcast. Only `set_balance` stays [`TronError::Unimplemented`] (a live
//! chain cannot mint).
//!
//! Transport is TronGrid HTTP (`/wallet/*`, plus the Ethereum-compatible `/jsonrpc` endpoint for
//! [`get_storage_at`]), not gRPC, so the crate keeps no Tron-specific
//! dependency (just `reqwest` + `serde_json`). The flow for a write is the standard java-tron
//! three step: build the unsigned transaction at the node (`/wallet/deploycontract`,
//! `/wallet/triggersmartcontract`, or `/wallet/createtransaction` for a native transfer), sign its
//! `txID` locally, then `/wallet/broadcasttransaction`.
//! Addresses cross the wire in 0x41 hex form (`visible=false`).
//!
//! [`balance`]: TronRpcProvider::balance
//! [`block_height`]: ChainProvider::block_height
//! [`static_call`]: TronRpcProvider::static_call
//! [`get_storage_at`]: TronRpcProvider::get_storage_at
//! [`deploy_create`]: TronRpcProvider::deploy_create
//! [`call`]: TronRpcProvider::call
//! [`transfer_funds`]: TronRpcProvider::transfer_funds

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use alloy_primitives::{Address, Bytes, Log, LogData, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use serde_json::{json, Value};

use crate::chains::TronChainInfo;
use crate::error::TronError;
use crate::provider::address::{address_from_label, address_from_pubkey, TronAddress};
use crate::provider::execution::{
    with_headroom, TronCompute, TronDeploy, TronEnergyPolicy, TronExecution, TronLimit,
    TronResources,
};

/// Bytes java-tron bills a transaction for on top of its `raw_data`: the 65-byte secp256k1
/// signature with its protobuf framing, plus the 64-byte result slot every transaction reserves
/// (`MAX_RESULT_SIZE_IN_TX`). Verified against mined mainnet receipts, where
/// `receipt.net_usage - raw_data_hex/2` is exactly 134 for transfers, delegations and contract
/// calls alike. Source: <https://developers.tron.network/docs/resource-model>
const TX_BANDWIDTH_OVERHEAD: u64 = 134;
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

    /// POST a JSON-RPC `method` to TronGrid's Ethereum-compatible endpoint (`<rest_base>/jsonrpc`)
    /// and return the `result` member.
    ///
    /// TronGrid mounts the standard Ethereum JSON-RPC alongside the `/wallet/*` REST API; a
    /// JSON-RPC-level `error` member surfaces as [`TronError::Rpc`].
    /// Source: <https://developers.tron.network/reference/eth_getstorageat>
    async fn post_jsonrpc(&self, method: &str, params: Value) -> Result<Value, TronError> {
        if self.rpc_url.is_empty() {
            return Err(TronError::Rpc(format!(
                "chain '{}' has no rpc_url; use a chain preset with an endpoint",
                self.info.chain_id
            )));
        }
        let url = format!("{}/jsonrpc", self.rpc_url);
        let resp = self
            .http
            .post(url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .map_err(|e| TronError::Rpc(e.to_string()))?
            .json::<Value>()
            .await
            .map_err(|e| TronError::Rpc(format!("decode {method}: {e}")))?;
        if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
            let msg = err["message"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| err.to_string());
            return Err(TronError::Rpc(format!("{method}: {msg}")));
        }
        Ok(resp["result"].clone())
    }

    /// Current block number. Inherent fallible variant of the trait's infallible
    /// [`ChainProvider::block_height`].
    pub async fn try_block_height(&self) -> Result<u64, TronError> {
        let v = self.post("getnowblock", json!({})).await?;
        v["block_header"]["raw_data"]["number"]
            .as_u64()
            .ok_or_else(|| TronError::Rpc("getnowblock: missing block number".into()))
    }

    /// The current price of one unit of energy, in sun: the node's `getEnergyFee` chain parameter.
    ///
    /// This is the exact divisor java-tron applies to a transaction's `fee_limit` to decide how
    /// much energy the transaction may burn, so pricing a forecast in energy into a fee cap with it
    /// is a conversion, not a guess. It is a governance parameter, changed by vote, so it is read
    /// from the node rather than pinned here; a [`TronLimit::Estimated`] operation pays one round
    /// trip for it, on top of the one its forecast already costs.
    ///
    /// An absent `getEnergyFee` is an error, not a default: java-tron's protobuf omits a zero
    /// value, and a chain that gives energy away for free would price every fee cap at zero sun,
    /// which is the one number that cannot buy any energy at all.
    /// Source: <https://developers.tron.network/reference/getchainparameters>
    async fn energy_price(&self) -> Result<u64, TronError> {
        let params = self.post("getchainparameters", json!({})).await?;
        energy_price_of(&params)
    }

    /// Resolve `limit` into the `fee_limit` (sun) java-tron takes on a contract transaction.
    ///
    /// [`TronLimit::Estimated`] prices `forecast` (the energy a `triggerconstantcontract` measured
    /// for this very operation) into sun at the chain's current [energy price](Self::energy_price),
    /// with the chain's `gas_adjustment` headroom applied to the energy and not to the sun: the
    /// price is exact, only the forecast is approximate. `forecast` is awaited on that path alone,
    /// so an exact limit pays for neither round trip.
    async fn fee_limit(
        &self,
        limit: TronLimit,
        forecast: impl Future<Output = Result<TronResources, TronError>>,
    ) -> Result<u64, TronError> {
        match limit {
            TronLimit::Fee(sun) => Ok(sun),
            TronLimit::Gas(gas) => Err(TronError::Rpc(format!(
                "a gas limit of {gas} cannot bound a live Tron transaction: java-tron meters \
                 energy and caps a transaction by fee_limit, in sun; use TronLimit::Fee or \
                 TronLimit::Estimated"
            ))),
            TronLimit::Estimated => {
                let forecast = forecast.await?;
                // Unreachable: this backend's estimator denominates in energy by construction
                // (`parse_estimate`). A gas figure here would mean it had reported the mock's unit.
                let TronCompute::Energy(energy) = forecast.compute else {
                    return Err(TronError::Rpc(format!(
                        "a live node forecasts energy, not gas: got {:?}",
                        forecast.compute
                    )));
                };
                let price = self.energy_price().await?;
                Ok(with_headroom(energy, self.info.gas_adjustment).saturating_mul(price))
            }
        }
    }

    /// Deploy bytecode via a create transaction signed by `signer`, returning the new contract
    /// address the node assigns, the broadcast transaction's `txID`, and the resources it consumed.
    ///
    /// `limit` caps what this create transaction may burn (see [`TronLimit`]); `energy_policy` is
    /// not a cap on it at all, but the two `DeployContract` fields that persist on the deployed
    /// contract and bill every future call to it (see [`TronEnergyPolicy`]).
    ///
    /// The node reports energy, bandwidth and fee only on the mined receipt, so this polls
    /// `gettransactioninfobyid` after broadcasting, as the [`call`](Self::call) path does. A deploy
    /// that fails on chain (out of energy, reverting constructor) therefore surfaces as
    /// [`TronError::Deploy`] instead of a success carrying an address no code lives at.
    pub async fn deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        signer: &PrivateKeySigner,
        limit: TronLimit,
        energy_policy: TronEnergyPolicy,
    ) -> Result<TronDeploy, TronError> {
        let owner = signer_address(signer);
        let fee_limit = self
            .fee_limit(
                limit,
                self.estimate_deploy_create(bytecode.clone(), constructor_args.as_ref(), &owner),
            )
            .await?;
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args.as_ref());
        let unsigned = self
            .post(
                "deploycontract",
                json!({
                    "owner_address": owner.to_hex(),
                    "abi": "[]",
                    "bytecode": hex::encode(&initcode),
                    "fee_limit": fee_limit,
                    "call_value": 0,
                    "consume_user_resource_percent": energy_policy.consume_user_resource_percent,
                    "origin_energy_limit": energy_policy.origin_energy_limit,
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
        let address = tron_address_from_hex(contract_hex).map_err(|e| TronError::Deploy(e.0))?;
        let tx_hash = self
            .sign_and_broadcast(unsigned, signer)
            .await
            .map_err(|e| TronError::Deploy(e.to_string()))?;
        let info = self.await_tx_info(&tx_hash).await?;
        if let Some(msg) = tx_failure(&info) {
            return Err(TronError::Deploy(msg));
        }
        Ok(TronDeploy {
            address,
            tx_hash,
            resources: parse_resources(&info),
        })
    }

    /// Forecast what a [`deploy_create`](Self::deploy_create) of this bytecode would consume,
    /// without deploying it: the node runs the initcode as a constant call and reports the energy
    /// it burned.
    ///
    /// The endpoint is `triggerconstantcontract` with `data` (initcode ++ constructor args) and no
    /// `contract_address`, which java-tron reads as a create. `estimateenergy` is the other
    /// candidate and is NOT used: it is off unless a node operator sets both `vm.estimateEnergy`
    /// and `vm.supportConstant` (mainnet TronGrid answers `this node does not support estimate
    /// energy`), it returns only `energy_required` with no transaction to size bandwidth from, and
    /// Tron's own docs name `triggerconstantcontract` as the fallback when it is unavailable. That
    /// endpoint is also already the one [`static_call`](Self::static_call) speaks.
    /// Source: <https://developers.tron.network/docs/set-feelimit>
    ///
    /// Nothing is signed or broadcast, so this needs an address, not a signer. A constructor that
    /// reverts is an error, not a resource figure.
    pub async fn estimate_deploy_create(
        &self,
        bytecode: Bytes,
        constructor_args: impl AsRef<[u8]>,
        from: &TronAddress,
    ) -> Result<TronResources, TronError> {
        let mut initcode = bytecode.to_vec();
        initcode.extend_from_slice(constructor_args.as_ref());
        let resp = self
            .post(
                "triggerconstantcontract",
                json!({
                    "owner_address": from.to_hex(),
                    "data": hex::encode(&initcode),
                    "visible": false,
                }),
            )
            .await?;
        if let Some(msg) = constant_call_failure(&resp) {
            return Err(TronError::Deploy(format!("estimate_deploy_create: {msg}")));
        }
        Ok(parse_estimate(&resp))
    }

    /// Forecast what a [`call`](Self::call) with these arguments would consume, without running it.
    pub async fn estimate_call(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
    ) -> Result<TronResources, TronError> {
        self.estimate_call_value(to, calldata, from, U256::ZERO)
            .await
    }

    /// Forecast what a [`call_value`](Self::call_value) with these arguments would consume, without
    /// running it. The node executes the call against current state and discards the result; see
    /// [`estimate_deploy_create`](Self::estimate_deploy_create) for why this endpoint.
    pub async fn estimate_call_value(
        &self,
        to: &TronAddress,
        calldata: impl AsRef<[u8]>,
        from: &TronAddress,
        value: U256,
    ) -> Result<TronResources, TronError> {
        let resp = self
            .post(
                "triggerconstantcontract",
                json!({
                    "owner_address": from.to_hex(),
                    "contract_address": to.to_hex(),
                    "data": hex::encode(calldata.as_ref()),
                    "call_value": value.saturating_to::<u64>(),
                    "visible": false,
                }),
            )
            .await?;
        if let Some(msg) = constant_call_failure(&resp) {
            return Err(TronError::Execute(format!("estimate_call: {msg}")));
        }
        Ok(parse_estimate(&resp))
    }

    /// Execute a state-mutating call against `to`, signed by `signer`, under the `fee_limit`
    /// `limit` resolves to (see [`TronLimit`]).
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
        limit: TronLimit,
    ) -> Result<TronExecution, TronError> {
        self.call_value(to, calldata, signer, U256::ZERO, limit)
            .await
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
        limit: TronLimit,
    ) -> Result<TronExecution, TronError> {
        let owner = signer_address(signer);
        let fee_limit = self
            .fee_limit(
                limit,
                self.estimate_call_value(to, calldata.as_ref(), &owner, value),
            )
            .await?;
        let resp = self
            .post(
                "triggersmartcontract",
                json!({
                    "owner_address": owner.to_hex(),
                    "contract_address": to.to_hex(),
                    "data": hex::encode(calldata.as_ref()),
                    "call_value": value.saturating_to::<u64>(),
                    "fee_limit": fee_limit,
                    "visible": false,
                }),
            )
            .await?;
        check_node_ok(&resp["result"], "triggersmartcontract")?;
        let txid = self
            .sign_and_broadcast(resp["transaction"].clone(), signer)
            .await
            .map_err(|e| TronError::Execute(e.to_string()))?;
        let info = self.await_tx_info(&txid).await?;
        parse_tx_info(&info, &txid)
    }

    /// Transfer `amount` sun of the native token to `to`, signed by `signer`, returning the
    /// broadcast transaction's `txID`.
    ///
    /// A native TRX transfer is its own java-tron transaction type, not a contract call: the node
    /// builds the unsigned `TransferContract` at `/wallet/createtransaction`, then the standard
    /// sign-`txID`-and-broadcast step applies. The node validates the sender's balance, so an
    /// underfunded transfer is rejected at broadcast. Confirmation is polled before returning, as
    /// on the [`call`](Self::call) path.
    ///
    /// It takes no [`TronLimit`], because a `TransferContract` has no `fee_limit` field to take:
    /// it runs no code, burns no energy, and is billed only in bandwidth, which the sender cannot
    /// cap. The only knobs a `fee_limit` would bound do not exist on this transaction.
    /// Source: <https://developers.tron.network/reference/createtransaction>
    pub async fn transfer_funds(
        &self,
        to: &TronAddress,
        amount: u64,
        signer: &PrivateKeySigner,
    ) -> Result<String, TronError> {
        let owner = signer_address(signer);
        let unsigned = self
            .post(
                "createtransaction",
                json!({
                    "owner_address": owner.to_hex(),
                    "to_address": to.to_hex(),
                    "amount": amount,
                    "visible": false,
                }),
            )
            .await?;
        check_node_ok(&unsigned, "createtransaction")?;
        let txid = self
            .sign_and_broadcast(unsigned, signer)
            .await
            .map_err(|e| TronError::Execute(e.to_string()))?;
        self.await_tx_info(&txid).await?;
        Ok(txid)
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
        if let Some(msg) = constant_call_failure(&resp) {
            return Err(TronError::Execute(format!("static_call: {msg}")));
        }
        let hexstr = resp["constant_result"][0].as_str().unwrap_or("");
        let bytes = hex::decode(hexstr)
            .map_err(|e| TronError::Query(format!("constant_result hex: {e}")))?;
        Ok(Bytes::from(bytes))
    }

    /// Read the raw 32-byte storage value at `slot` for `addr` via TronGrid's Ethereum-compatible
    /// JSON-RPC (`eth_getStorageAt` at `<rest_base>/jsonrpc`).
    ///
    /// The address crosses the wire as the 20-byte EVM form ([`TronAddress::as_evm`]), not the
    /// base58 or `0x41` Tron form. TRON only supports the `"latest"` block tag, so historical slot
    /// reads are unavailable. Source: <https://developers.tron.network/reference/eth_getstorageat>
    pub async fn get_storage_at(&self, addr: &TronAddress, slot: U256) -> Result<U256, TronError> {
        let addr_hex = format!("{:#x}", addr.as_evm());
        let slot_hex = format!("{slot:#x}");
        let result = self
            .post_jsonrpc("eth_getStorageAt", json!([addr_hex, slot_hex, "latest"]))
            .await?;
        let s = result
            .as_str()
            .ok_or_else(|| TronError::Query("eth_getStorageAt: non-string result".into()))?;
        U256::from_str_radix(s.trim_start_matches("0x"), 16)
            .map_err(|e| TronError::Query(format!("eth_getStorageAt parse: {e}")))
    }

    /// Sign an unsigned transaction's `txID` with `signer` and broadcast it, returning that `txID`
    /// (unprefixed hex): the broadcast transaction's hash, which every write path reports.
    async fn sign_and_broadcast(
        &self,
        mut tx: Value,
        signer: &PrivateKeySigner,
    ) -> Result<String, TronError> {
        let txid_hex = tx["txID"]
            .as_str()
            .ok_or_else(|| TronError::Rpc("transaction has no txID".into()))?
            .to_string();
        let txid =
            hex::decode(&txid_hex).map_err(|e| TronError::Rpc(format!("bad txID hex: {e}")))?;
        let sig = sign_txid(signer, &txid)?;
        tx["signature"] = json!([hex::encode(sig)]);
        let res = self.post("broadcasttransaction", tx).await?;
        if res["result"].as_bool() == Some(true) {
            return Ok(txid_hex);
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

/// The failure message of a mined `TransactionInfo` that reverted or ran out of energy:
/// `result == "FAILED"` (top level) or a non-`SUCCESS` `receipt.result`. `None` if it succeeded.
/// The caller wraps the message in the [`TronError`] variant its operation reports, mirroring the
/// mock, which errors rather than returning a bad success.
fn tx_failure(info: &Value) -> Option<String> {
    let failed = info["result"].as_str() == Some("FAILED")
        || info["receipt"]["result"]
            .as_str()
            .is_some_and(|r| r != "SUCCESS");
    failed.then(|| {
        let msg = info["resMessage"]
            .as_str()
            .map(decode_hex_message)
            .unwrap_or_default();
        let reason = info["receipt"]["result"].as_str().unwrap_or("FAILED");
        format!("tx {reason}: {msg}")
    })
}

/// The `getEnergyFee` member of a `getchainparameters` response: the sun one unit of energy costs.
///
/// The parameters arrive as a flat `[{key, value}, ..]` list. An absent `getEnergyFee` is an error
/// rather than a default: guessing the divisor java-tron will apply to the `fee_limit` would size
/// every estimated cap off a number the chain does not use.
fn energy_price_of(params: &Value) -> Result<u64, TronError> {
    params["chainParameter"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["key"] == "getEnergyFee"))
        .and_then(|p| p["value"].as_u64())
        .ok_or_else(|| TronError::Rpc("getchainparameters: no getEnergyFee".into()))
}

/// The resources a mined `TransactionInfo` reports: energy (`receipt.energy_usage_total`, the sum
/// of the energy the caller staked for, the contract paid, and any that was burned for), bandwidth
/// (`receipt.net_usage`), and the `fee` the transaction was billed, in sun.
///
/// A member java-tron omits means the transaction consumed none of that resource: a fully
/// staked-for transaction burns nothing and carries no `fee`, and a transaction that burned TRX for
/// its bytes carries no `net_usage`. Energy is what a live chain actually meters, so it is reported
/// as [`TronCompute::Energy`] (never as gas; see [`TronCompute`]).
/// Source: <https://developers.tron.network/docs/resource-model>
fn parse_resources(info: &Value) -> TronResources {
    let receipt = &info["receipt"];
    TronResources {
        compute: TronCompute::Energy(receipt["energy_usage_total"].as_u64().unwrap_or(0)),
        bandwidth: receipt["net_usage"].as_u64().unwrap_or(0),
        fee: Some(info["fee"].as_u64().unwrap_or(0)),
    }
}

/// The failure message of a `triggerconstantcontract` response, or `None` if the constant call ran
/// to completion.
///
/// java-tron reports a failed constant call in two shapes, and testing `result.result` alone
/// catches neither: protobuf omits a `false` boolean, so a rejected call carries only
/// `result.{code,message}` and no `result.result` at all, and a REVERT comes back as
/// `result.result: true` *with* a `message`, still quoting an `energy_used`. Both are failures. A
/// caller handed an energy figure for a transaction that cannot succeed has been misinformed, and
/// the empty `constant_result` of a reverted read is not an answer either.
fn constant_call_failure(resp: &Value) -> Option<String> {
    if let Some(err) = resp["Error"].as_str() {
        return Some(err.to_string());
    }
    let result = &resp["result"];
    let message = result["message"].as_str().map(decode_hex_message);
    if result["result"].as_bool() == Some(true) && message.is_none() {
        return None;
    }
    let code = result["code"].as_str().unwrap_or("FAILED");
    Some(format!("{code}: {}", message.unwrap_or_default()))
}

/// The resources a `triggerconstantcontract` response forecasts: the energy the node measured while
/// running the call (`energy_used`, basic plus penalty), and the bandwidth the transaction it built
/// for the estimate would be billed.
///
/// Energy is what a live chain meters, so a live forecast is denominated in it, exactly as the
/// receipt is ([`TronCompute::Energy`], never gas; see [`TronCompute`]). Two honest gaps:
///
/// * Bandwidth is derived, not reported: java-tron bills a transaction by its serialized size, and
///   the node hands back the transaction it built (`raw_data_hex`). The tx that actually broadcasts
///   is a handful of bytes larger, because it carries the `fee_limit` a constant call has no use
///   for (7 bytes, measured). The figure is also what the transaction is *billed for*: whether
///   those points are deducted from the sender's allowance or paid for by burning TRX (leaving
///   `net_usage: 0` on the receipt) depends on what the sender has staked at broadcast time.
/// * `fee` is `None`. Pricing energy and bandwidth into sun needs the sender's staked resources at
///   broadcast, which a constant call does not see; a number here would be a guess.
///
/// Source: <https://developers.tron.network/docs/resource-model>
fn parse_estimate(resp: &Value) -> TronResources {
    let bandwidth = resp["transaction"]["raw_data_hex"]
        .as_str()
        .map_or(0, |h| h.len() as u64 / 2 + TX_BANDWIDTH_OVERHEAD);
    TronResources {
        compute: TronCompute::Energy(resp["energy_used"].as_u64().unwrap_or(0)),
        bandwidth,
        fee: None,
    }
}

/// Map a mined `TransactionInfo` into a [`TronExecution`], surfacing an on-chain failure as an
/// error. Tron logs are EVM-shaped; the log `address` is the 20-byte form without the `0x41`
/// prefix. Source: <https://developers.tron.network/docs/event>
fn parse_tx_info(info: &Value, txid: &str) -> Result<TronExecution, TronError> {
    if let Some(msg) = tx_failure(info) {
        return Err(TronError::Execute(msg));
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

    Ok(TronExecution {
        output,
        logs,
        // The node's `txID`, verbatim, exactly as `transfer_funds` reports it. It is already known
        // to be hex (`sign_and_broadcast` decoded it to sign it), and a hash is a `String` here, so
        // there is nothing left to parse or validate.
        tx_hash: txid.to_string(),
        resources: parse_resources(info),
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

    /// The caller pays all of a call's energy, so the contract owner's ceiling never binds.
    const CALLER_PAYS: TronEnergyPolicy = TronEnergyPolicy {
        consume_user_resource_percent: 100,
        origin_energy_limit: 0,
    };

    #[tokio::test]
    async fn no_endpoint_errors_offline() {
        // LOCAL has no rpc_url, so a network call fails fast without touching the network.
        let c = TronRpcProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let signer = PrivateKeySigner::random();
        let res = c
            .deploy_create(
                Bytes::new(),
                Vec::<u8>::new(),
                &signer,
                TronLimit::Fee(1_000_000_000),
                CALLER_PAYS,
            )
            .await;
        assert!(matches!(res, Err(TronError::Deploy(_) | TronError::Rpc(_))));
    }

    #[tokio::test]
    async fn a_gas_limit_is_rejected_before_the_node_is_asked_anything() {
        // java-tron has no gas: it meters energy and caps a transaction by `fee_limit`, in sun.
        // A gas budget is the mock's (revm's) unit, so it is an error rather than a number quietly
        // reinterpreted as sun. LOCAL has no rpc_url, so reaching the network at all would surface
        // as a transport error instead: the rejection provably precedes any request.
        let c = TronRpcProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let signer = PrivateKeySigner::random();
        let to = address_from_label("x");

        for res in [
            c.deploy_create(
                Bytes::new(),
                Vec::<u8>::new(),
                &signer,
                TronLimit::Gas(30_000_000),
                CALLER_PAYS,
            )
            .await
            .map(|_| ()),
            c.call(&to, [], &signer, TronLimit::Gas(30_000_000))
                .await
                .map(|_| ()),
        ] {
            let err = res.expect_err("a gas budget cannot bound a live Tron transaction");
            assert!(
                matches!(&err, TronError::Rpc(m) if m.contains("gas limit") && m.contains("fee_limit")),
                "got {err:?}"
            );
        }
    }

    #[test]
    fn energy_price_is_read_from_the_chain_parameters() {
        // The node returns a flat key/value list; `getEnergyFee` is the sun one energy unit costs,
        // and the exact divisor java-tron applies to a `fee_limit`.
        let params = json!({
            "chainParameter": [
                { "key": "getMaintenanceTimeInterval", "value": 21_600_000 },
                { "key": "getEnergyFee", "value": 210 },
                { "key": "getMaxFeeLimit", "value": 15_000_000_000i64 },
            ],
        });
        assert_eq!(energy_price_of(&params).unwrap(), 210);

        // A missing `getEnergyFee` is an error, not a guessed divisor: an estimated fee cap sized
        // off a price the chain does not use is a cap in name only.
        let missing = json!({ "chainParameter": [{ "key": "getMaxFeeLimit", "value": 1 }] });
        assert!(matches!(energy_price_of(&missing), Err(TronError::Rpc(_))));
    }

    #[test]
    fn an_estimated_fee_cap_prices_the_forecast_energy_with_headroom() {
        // What `TronLimit::Estimated` computes on this backend: the node's forecast energy, scaled
        // by the chain's `gas_adjustment`, priced into sun at the chain's energy price. The
        // headroom lands on the energy (the approximate half) and never on the price (the exact
        // half), so the cap buys the energy the operation is forecast to need, plus the margin.
        let energy = parse_estimate(&constant_call()).compute;
        let TronCompute::Energy(energy) = energy else {
            panic!("the node forecasts energy");
        };
        let price = energy_price_of(&json!({
            "chainParameter": [{ "key": "getEnergyFee", "value": 210 }],
        }))
        .unwrap();

        let cap = with_headroom(energy, NILE.gas_adjustment).saturating_mul(price);
        assert_eq!(cap, 92_451 * 210, "71_116 energy * 1.3, rounded up");
        assert!(
            cap >= energy * price,
            "the cap must at least buy the forecast energy"
        );
    }

    #[tokio::test]
    async fn get_storage_at_no_endpoint_errors_offline() {
        // LOCAL has no rpc_url: the JSON-RPC slot read fails fast as `Rpc` (a live method now, not
        // `Unimplemented`), without touching the network.
        let mut c = TronRpcProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let addr = c.new_account("x").await;
        let res = c.get_storage_at(&addr, U256::ZERO).await;
        assert!(matches!(res, Err(TronError::Rpc(_))));
    }

    #[test]
    fn signer_address_is_tron_shaped() {
        let signer = PrivateKeySigner::random();
        assert!(signer_address(&signer).to_base58().starts_with('T'));
    }

    /// A mined `gettransactioninfobyid` receipt, trimmed to the members the parser reads.
    fn receipt() -> Value {
        json!({
            "id": "aa".repeat(32),
            "fee": 1_345_600,
            "contractResult": [""],
            "receipt": {
                "energy_usage": 3_000,
                "energy_fee": 1_260_000,
                "energy_usage_total": 13_456,
                "net_usage": 345,
                "result": "SUCCESS",
            },
        })
    }

    #[test]
    fn rpc_reports_tron_energy_bandwidth_and_fee() {
        // The live chain meters energy, so that is what the RPC backend reports: never gas, which
        // is the mock's (revm's) unit and a different quantity entirely.
        let r = parse_resources(&receipt());
        assert_eq!(r.compute, TronCompute::Energy(13_456));
        assert_eq!(r.bandwidth, 345);
        assert_eq!(r.fee, Some(1_345_600));
    }

    #[test]
    fn a_receipt_without_a_fee_burned_nothing() {
        // java-tron omits `fee` and `net_usage` when the transaction consumed none of them (fully
        // covered by staked resources); that is a real zero, not an unknown.
        let info = json!({ "id": "bb".repeat(32), "receipt": { "energy_usage_total": 7, "result": "SUCCESS" } });
        let r = parse_resources(&info);
        assert_eq!(r.compute, TronCompute::Energy(7));
        assert_eq!(r.bandwidth, 0);
        assert_eq!(r.fee, Some(0));
    }

    #[test]
    fn parse_tx_info_carries_the_receipt_resources() {
        let txid = "aa".repeat(32);
        let exec = parse_tx_info(&receipt(), &txid).expect("SUCCESS receipt");
        assert_eq!(exec.tx_hash, txid);
        assert_eq!(exec.resources.compute, TronCompute::Energy(13_456));
        assert_eq!(exec.resources.bandwidth, 345);
        assert_eq!(exec.resources.fee, Some(1_345_600));
    }

    /// A `triggerconstantcontract` response, trimmed to the members the estimator reads. The
    /// `raw_data_hex` is 8 bytes long, so the bandwidth forecast is 8 + the billing overhead.
    fn constant_call() -> Value {
        json!({
            "result": { "result": true },
            "energy_used": 71_116,
            "constant_result": [""],
            "transaction": { "raw_data_hex": "0a0270cf22084142" },
        })
    }

    #[test]
    fn rpc_estimate_reports_node_energy_never_gas() {
        // The node meters energy, so that is the unit its forecast is denominated in, exactly as
        // its receipts are. Gas is the mock's (revm's) unit and a different quantity entirely.
        let r = parse_estimate(&constant_call());
        assert_eq!(r.compute, TronCompute::Energy(71_116));
        assert_eq!(r.bandwidth, 8 + TX_BANDWIDTH_OVERHEAD);
        // Pricing energy into sun needs the sender's staked resources at broadcast; a constant call
        // cannot see them, and a guess would be worse than an honest `None`.
        assert_eq!(r.fee, None);
    }

    #[test]
    fn a_reverting_constant_call_is_an_error_not_an_energy_figure() {
        // The trap this guards: java-tron answers a REVERT with `result: true` AND a message, still
        // quoting an `energy_used` (a real Nile/mainnet response shape). Reporting that energy
        // would forecast a transaction that cannot succeed.
        let mut resp = constant_call();
        resp["result"] = json!({
            "result": true,
            "message": hex::encode("REVERT opcode executed"),
        });
        let msg = constant_call_failure(&resp).expect("a REVERT is a failure");
        assert!(msg.contains("REVERT opcode executed"), "got {msg}");

        // And the other shape: protobuf omits `result.result` when it is false, leaving only a code
        // and a message, so a rejected call has no boolean to test at all.
        let rejected = json!({
            "result": { "code": "OTHER_ERROR", "message": hex::encode("stack too small") },
        });
        let msg = constant_call_failure(&rejected).expect("a rejected call is a failure");
        assert!(
            msg.contains("OTHER_ERROR") && msg.contains("stack too small"),
            "got {msg}"
        );

        // A clean constant call is not a failure.
        assert!(constant_call_failure(&constant_call()).is_none());
    }

    #[tokio::test]
    async fn estimates_error_offline() {
        // LOCAL has no rpc_url, so both estimate paths fail fast without touching the network.
        let mut c = TronRpcProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let addr = c.new_account("x").await;
        assert!(matches!(
            c.estimate_deploy_create(Bytes::new(), [], &addr).await,
            Err(TronError::Rpc(_))
        ));
        assert!(matches!(
            c.estimate_call(&addr, [], &addr).await,
            Err(TronError::Rpc(_))
        ));
    }

    #[test]
    fn a_failed_receipt_is_an_error_not_a_zero_cost_success() {
        let mut info = receipt();
        info["receipt"]["result"] = json!("OUT_OF_ENERGY");
        assert!(tx_failure(&info).is_some_and(|m| m.contains("OUT_OF_ENERGY")));
        assert!(matches!(
            parse_tx_info(&info, &"aa".repeat(32)),
            Err(TronError::Execute(_))
        ));
    }
}
