//! A batch of CosmWasm messages signed and broadcast as one atomic transaction.
//!
//! [`CwBatch`] collects several messages (contract executes, bank sends, migrations, or raw
//! module messages) and hands them to [`crate::CwChain::execute_batch`], which broadcasts the
//! whole set under a single transaction (one hash, all-or-nothing). A member that fails rolls the
//! entire batch back, so partial application never happens on either backend.
//!
//! The builder stores each member in a backend-neutral shape: the sender is unknown until
//! broadcast (it comes from the wallet the caller signs with), so a member records only what the
//! caller supplied. Each backend renders its own message shape lazily at broadcast: the mock maps
//! members to `cw-multi-test` [`CosmosMsg`]s for `App::execute_multi`, while the live RPC backend
//! maps them to protobuf [`cosmrs::Any`] messages and signs them into one transaction.
//!
//! ```ignore
//! let batch = CwBatch::new()
//!     .execute(&counter, ExecuteMsg::Increment {}, &[])
//!     .execute(&counter, ExecuteMsg::Increment {}, &[])
//!     .send(&sink, 1_000, "uosmo");
//! let receipt = chain.execute_batch(&batch, wallet, CwGasLimit::Estimated).await?;
//! // One hash covers both increments and the send; either all landed or none did.
//! ```

use cosmwasm_std::{coin, Addr, BankMsg, Binary, Coin, CosmosMsg, WasmMsg};

use crate::error::CwError;
use crate::msg::CwSerde;

/// One member of a [`CwBatch`], stored backend-neutrally: contract/bank message payloads are
/// serialized to JSON bytes at build time (the shape both backends need), and the sender is left
/// out because it is only known at broadcast. [`CwBatchMember::Raw`] carries an already-encoded
/// protobuf message and is live-RPC only (the mock has no `CosmosMsg` equivalent for it).
pub(crate) enum CwBatchMember {
    /// Execute `msg` (JSON bytes) against `contract`, attaching `funds`.
    Execute {
        contract: Addr,
        msg: Vec<u8>,
        funds: Vec<Coin>,
    },
    /// Bank-send `amount` base units of `denom` to `to`.
    Send {
        to: Addr,
        amount: u128,
        denom: String,
    },
    /// Migrate `contract` to `new_code_id`, running its `migrate` entry point with `msg` (JSON
    /// bytes).
    Migrate {
        contract: Addr,
        new_code_id: u64,
        msg: Vec<u8>,
    },
    /// A raw protobuf message, passed through verbatim on the live RPC backend.
    Raw(cosmrs::Any),
}

/// A set of CosmWasm messages to sign and broadcast as one atomic transaction.
///
/// Build it by chaining [`execute`](Self::execute), [`send`](Self::send),
/// [`migrate`](Self::migrate), and [`raw`](Self::raw), then hand it to
/// [`crate::CwChain::execute_batch`]. An empty batch is a caller error (there is nothing to
/// broadcast), reported when the batch runs rather than while it is built.
#[derive(Default)]
pub struct CwBatch {
    members: Vec<CwBatchMember>,
    // First message-encoding failure hit while building, surfaced when the batch runs. A chainable
    // builder cannot return a `Result` mid-chain, so the error is deferred here (as its reason
    // string: `CwError` is not `Clone`) and raised by `execute_batch` before anything broadcasts.
    deferred: Option<String>,
}

impl CwBatch {
    /// A fresh, empty batch. Add members with [`execute`](Self::execute), [`send`](Self::send),
    /// [`migrate`](Self::migrate), and [`raw`](Self::raw).
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a contract execution: run `msg` against `contract`, attaching `funds`.
    ///
    /// `msg` is serialized to JSON now (the shape both backends need); an encoding failure is
    /// stashed and surfaced when the batch runs.
    pub fn execute<E: CwSerde>(mut self, contract: &Addr, msg: E, funds: &[Coin]) -> Self {
        match serde_json::to_vec(&msg) {
            Ok(bytes) => self.members.push(CwBatchMember::Execute {
                contract: contract.clone(),
                msg: bytes,
                funds: funds.to_vec(),
            }),
            Err(e) => self.defer(format!("encode batch execute: {e}")),
        }
        self
    }

    /// Append a bank send: move `amount` base units of `denom` to `to`. Any bank denom moves
    /// verbatim (`uosmo`, `ibc/...`), not just the chain's native one.
    pub fn send(mut self, to: &Addr, amount: u128, denom: &str) -> Self {
        self.members.push(CwBatchMember::Send {
            to: to.clone(),
            amount,
            denom: denom.to_string(),
        });
        self
    }

    /// Append a migration: move `contract` to `new_code_id`, running the new code's `migrate`
    /// entry point with `msg`. The signing wallet must be the contract's admin.
    ///
    /// `msg` is serialized to JSON now; an encoding failure is stashed and surfaced when the batch
    /// runs.
    pub fn migrate<M: CwSerde>(mut self, contract: &Addr, new_code_id: u64, msg: M) -> Self {
        match serde_json::to_vec(&msg) {
            Ok(bytes) => self.members.push(CwBatchMember::Migrate {
                contract: contract.clone(),
                new_code_id,
                msg: bytes,
            }),
            Err(e) => self.defer(format!("encode batch migrate: {e}")),
        }
        self
    }

    /// Append a raw protobuf message, broadcast verbatim. Live RPC only: the mock has no
    /// `cw-multi-test` `CosmosMsg` equivalent, so a batch carrying a raw member fails there with
    /// [`CwError::Unimplemented`].
    pub fn raw(mut self, msg: cosmrs::Any) -> Self {
        self.members.push(CwBatchMember::Raw(msg));
        self
    }

    /// How many members the batch carries.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the batch carries no members.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Record the first build-time encoding failure, keeping later ones from masking it.
    fn defer(&mut self, reason: String) {
        if self.deferred.is_none() {
            self.deferred = Some(reason);
        }
    }

    /// The build-time encoding error, if any member failed to serialize while the batch was built.
    pub(crate) fn deferred_error(&self) -> Option<CwError> {
        self.deferred.clone().map(CwError::Execute)
    }

    /// The batch's members, for the RPC backend to map into protobuf messages.
    pub(crate) fn members(&self) -> &[CwBatchMember] {
        &self.members
    }

    /// Map every member to a `cw-multi-test` [`CosmosMsg`] for the mock backend's
    /// `App::execute_multi`. A [`CwBatchMember::Raw`] member has no `CosmosMsg` equivalent, so it
    /// fails with [`CwError::Unimplemented`] rather than being silently dropped.
    pub(crate) fn cosmos_msgs(&self) -> Result<Vec<CosmosMsg>, CwError> {
        self.members
            .iter()
            .map(|m| match m {
                CwBatchMember::Execute {
                    contract,
                    msg,
                    funds,
                } => Ok(CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: contract.to_string(),
                    msg: Binary::from(msg.clone()),
                    funds: funds.clone(),
                })),
                CwBatchMember::Send { to, amount, denom } => Ok(CosmosMsg::Bank(BankMsg::Send {
                    to_address: to.to_string(),
                    amount: vec![coin(*amount, denom)],
                })),
                CwBatchMember::Migrate {
                    contract,
                    new_code_id,
                    msg,
                } => Ok(CosmosMsg::Wasm(WasmMsg::Migrate {
                    contract_addr: contract.to_string(),
                    new_code_id: *new_code_id,
                    msg: Binary::from(msg.clone()),
                })),
                CwBatchMember::Raw(_) => Err(CwError::Unimplemented(
                    "mock execute_batch: a raw cosmrs::Any member has no cw-multi-test CosmosMsg \
                     equivalent; raw members require the live RPC backend"
                        .into(),
                )),
            })
            .collect()
    }
}
