//! Live-RPC Solana provider (read-only).
//!
//! [`SvmRpcProvider`] talks to a real Solana cluster over JSON-RPC (a thin `reqwest`
//! client). Read paths need no signer: [`block_height`] (`getSlot`), [`balance`]
//! (`getBalance`), and [`get_account`] (`getAccountInfo`). Write paths (`add_program`,
//! `send_transaction`, `transfer_funds`, `set_balance`) still return
//! [`SvmError::Unimplemented`] until signing and broadcast land, and so does
//! `estimate_transaction`, which needs the same transaction assembly they do.
//!
//! [`block_height`]: SvmRpcProvider::block_height
//! [`balance`]: SvmRpcProvider::balance
//! [`get_account`]: SvmRpcProvider::get_account

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::str::FromStr;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use cross_vm_core::{BlockTime, ChainProvider, FundError, WalletFactory};
use litesvm::types::TransactionMetadata;
use serde_json::{json, Value};
use solana_account::Account;
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::asset::SvmAsset;
use crate::chains::{Commitment, SolanaChainInfo};
use crate::error::SvmError;
use crate::provider::{SvmComputeBudget, SvmDeploy};
use crate::wallet::SvmSigner;

/// A live-RPC Solana provider. Read-only: chain-level reads and account reads hit a real
/// cluster; state-mutating operations remain [`SvmError::Unimplemented`].
#[derive(Clone)]
pub struct SvmRpcProvider {
    info: SolanaChainInfo,
    rpc_url: String,
    http: reqwest::Client,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, SvmSigner>>>,
}

impl SvmRpcProvider {
    /// Create an RPC provider bound to a cluster's metadata.
    ///
    /// Stays infallible so `SOLANA_DEVNET.rpc(wallets)` sugar keeps working; a missing or empty
    /// `rpc_url` surfaces as an error at the first network call instead.
    pub fn new(info: SolanaChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let rpc_url = info.rpc_url.unwrap_or("").to_string();
        Self {
            info,
            rpc_url,
            http: reqwest::Client::new(),
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// The configured commitment as the string the JSON-RPC API expects.
    fn commitment(&self) -> &'static str {
        match self.info.commitment {
            Commitment::Processed => "processed",
            Commitment::Confirmed => "confirmed",
            Commitment::Finalized => "finalized",
        }
    }

    /// Issue a JSON-RPC call and return its `result` value.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, SvmError> {
        if self.rpc_url.is_empty() {
            return Err(SvmError::Rpc(format!(
                "cluster '{}' has no rpc_url; use a cluster preset with an endpoint",
                self.info.chain_id
            )));
        }
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SvmError::Rpc(e.to_string()))?;
        let mut v: Value = resp
            .json()
            .await
            .map_err(|e| SvmError::Rpc(e.to_string()))?;
        if let Some(err) = v.get("error") {
            return Err(SvmError::Rpc(err.to_string()));
        }
        Ok(v.get_mut("result").map(Value::take).unwrap_or(Value::Null))
    }

    /// Current slot. Inherent fallible variant of the trait's infallible
    /// [`ChainProvider::block_height`]. Matches the mock, whose block height is the slot.
    pub async fn try_block_height(&self) -> Result<u64, SvmError> {
        let result = self
            .rpc("getSlot", json!([{ "commitment": self.commitment() }]))
            .await?;
        result
            .as_u64()
            .ok_or_else(|| SvmError::Rpc(format!("getSlot: unexpected result {result}")))
    }

    /// Ensure `who` holds at least `amount` lamports of `asset` on the live cluster.
    ///
    /// A real cluster cannot mint, so this validates rather than funds: native reads the
    /// actual balance and reports a [`FundError::Shortfall`] when underfunded (top up via a
    /// faucet). Spl funding stays [`FundError::Unimplemented`].
    pub async fn ensure_asset(
        &mut self,
        who: &Address,
        asset: SvmAsset,
        amount: u64,
    ) -> Result<(), FundError> {
        match asset {
            SvmAsset::Native => {
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
            SvmAsset::Spl(_) => Err(FundError::Unimplemented("solana rpc spl funding".into())),
        }
    }

    // ----- Write paths: unimplemented until signing + tx broadcast land. -----

    /// Load a program into the chain at a fresh program id.
    pub async fn add_program(&self, _bytecode: Vec<u8>) -> Result<SvmDeploy, SvmError> {
        Err(SvmError::Unimplemented("rpc add_program".into()))
    }

    /// Load a program at a specific program id.
    pub async fn add_program_at(
        &self,
        _program_id: Address,
        _bytecode: Vec<u8>,
    ) -> Result<SvmDeploy, SvmError> {
        Err(SvmError::Unimplemented("rpc add_program_at".into()))
    }

    /// Sign and send a transaction built from `instructions`, capped at `budget` compute units,
    /// signed by `signer`.
    ///
    /// The wallet signer is plumbed through, but live broadcast is not yet implemented: the
    /// return type is litesvm's [`TransactionMetadata`], which a bare `sendTransaction` (which
    /// yields only a signature) cannot produce. A focused follow-up will decouple the return
    /// type. The per-wallet broadcast lock is already enforced at the chain level.
    pub async fn send_transaction(
        &self,
        _instructions: impl AsRef<[Instruction]>,
        _signer: &SvmSigner,
        _budget: SvmComputeBudget,
    ) -> Result<TransactionMetadata, SvmError> {
        Err(SvmError::Unimplemented("rpc send_transaction".into()))
    }

    /// Report what the transaction built from `instructions` would consume and pay if `signer`
    /// sent it, without sending it.
    ///
    /// Unimplemented, like every other write path here. `simulateTransaction` exists on the
    /// JSON-RPC API, but it needs the same signing and message assembly the write paths do, and
    /// its response cannot fill a [`TransactionMetadata`] (it reports no fee). Returning a zero
    /// would claim a live cluster executes for free.
    pub async fn estimate_transaction(
        &self,
        _instructions: impl AsRef<[Instruction]>,
        _signer: &SvmSigner,
    ) -> Result<TransactionMetadata, SvmError> {
        Err(SvmError::Unimplemented("rpc estimate_transaction".into()))
    }

    /// Transfer `amount` base units (lamports) of `denom` from `signer` to `to`, capped at `budget`
    /// compute units, returning the base58 transaction signature.
    pub async fn transfer_funds(
        &self,
        _to: &Address,
        _denom: &str,
        _amount: u64,
        _signer: &SvmSigner,
        _budget: SvmComputeBudget,
    ) -> Result<String, SvmError> {
        Err(SvmError::Unimplemented("solana rpc transfer_funds".into()))
    }

    /// Read on-chain account data (`getAccountInfo`) for `pubkey`.
    pub async fn get_account(&self, pubkey: &Address) -> Result<Option<Account>, SvmError> {
        let result = self
            .rpc(
                "getAccountInfo",
                json!([pubkey.to_string(), { "encoding": "base64", "commitment": self.commitment() }]),
            )
            .await?;
        let value = &result["value"];
        if value.is_null() {
            return Ok(None);
        }
        let data_b64 = value["data"][0]
            .as_str()
            .ok_or_else(|| SvmError::Query("getAccountInfo: missing base64 data".into()))?;
        let data = STANDARD
            .decode(data_b64)
            .map_err(|e| SvmError::Query(format!("getAccountInfo: bad base64: {e}")))?;
        let owner = value["owner"]
            .as_str()
            .and_then(|s| Address::from_str(s).ok())
            .ok_or_else(|| SvmError::Query("getAccountInfo: bad owner".into()))?;
        Ok(Some(Account {
            lamports: value["lamports"].as_u64().unwrap_or(0),
            data,
            owner,
            executable: value["executable"].as_bool().unwrap_or(false),
            rent_epoch: value["rentEpoch"].as_u64().unwrap_or(0),
        }))
    }

    /// Read the raw account data bytes (`getAccountInfo`) for `pubkey` (SVM equivalent of raw storage).
    pub async fn get_account_data(&self, pubkey: &Address) -> Result<Option<Vec<u8>>, SvmError> {
        Ok(self.get_account(pubkey).await?.map(|a| a.data))
    }

    /// Read a fixed-width window `[offset, offset + len)` of `pubkey`'s account data via
    /// `getAccountInfo` with a server-side `dataSlice` (only the requested bytes cross the wire).
    ///
    /// Semantics are normalized to match the mock exactly, so a `Some` result always carries
    /// exactly `len` bytes: a missing account yields `Ok(None)`, and because the RPC silently
    /// clamps a slice that runs past the end of the account data, any decoded length other than
    /// `len` (i.e. an out-of-range window) is also reported as `Ok(None)` rather than a short
    /// buffer.
    pub async fn get_account_data_slice(
        &self,
        pubkey: &Address,
        offset: usize,
        len: usize,
    ) -> Result<Option<Vec<u8>>, SvmError> {
        let result = self
            .rpc(
                "getAccountInfo",
                json!([pubkey.to_string(), {
                    "encoding": "base64",
                    "commitment": self.commitment(),
                    "dataSlice": { "offset": offset, "length": len },
                }]),
            )
            .await?;
        let value = &result["value"];
        if value.is_null() {
            return Ok(None);
        }
        let data_b64 = value["data"][0]
            .as_str()
            .ok_or_else(|| SvmError::Query("getAccountInfo: missing base64 data".into()))?;
        let data = STANDARD
            .decode(data_b64)
            .map_err(|e| SvmError::Query(format!("getAccountInfo: bad base64: {e}")))?;
        // The RPC clamps an out-of-range slice to whatever bytes exist; treat any short read as
        // "range not fully present" to mirror the mock's all-or-nothing `get(offset..offset+len)`.
        if data.len() != len {
            return Ok(None);
        }
        Ok(Some(data))
    }
}

impl ChainProvider for SvmRpcProvider {
    type Spec = SolanaChainInfo;
    type Address = Address;
    type Account = Keypair;
    type Balance = u64;
    type Error = SvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, _label: &str) -> Address {
        // No signing backend in the read-only phase; the keypair is discarded. The write
        // phase will retain keypairs (like the mock's `HashMap<Address, Keypair>`) to sign.
        Keypair::new().pubkey()
    }

    async fn balance(&self, addr: &Address) -> Result<u64, SvmError> {
        let result = self
            .rpc(
                "getBalance",
                json!([addr.to_string(), { "commitment": self.commitment() }]),
            )
            .await?;
        result["value"]
            .as_u64()
            .ok_or_else(|| SvmError::Balance(format!("getBalance: unexpected result {result}")))
    }

    async fn set_balance(
        &mut self,
        _addr: &Address,
        _denom: &str,
        _amount: u64,
    ) -> Result<(), SvmError> {
        // Cannot mint on a real cluster. Use a faucet; declared funding is validated, not minted.
        Err(SvmError::Unimplemented("rpc set_balance".into()))
    }

    async fn block_height(&self) -> u64 {
        self.try_block_height().await.unwrap_or(0)
    }

    async fn advance_blocks(&mut self, _n: u64, _time: BlockTime) {
        // No-op: a real cluster advances on its own; tests poll instead of forcing slots.
    }
}
