//! CosmWasm chain providers: the in-process mock and the live-RPC stub.

mod mock;
mod rpc;

pub use mock::{CwApp, CwCode, CwMockProvider, DEFAULT_FUNDING};
pub use rpc::CwRpcProvider;

use cosmwasm_std::Addr;

use crate::CwAppResponse;

/// Backend-neutral contract code for [`crate::CwChain::store_code`].
///
/// The two backends run different code representations: the in-process mock executes native
/// `cw-multi-test` contract objects ([`CwCode`]), while the live RPC backend uploads compiled
/// wasm bytecode. This struct carries either representation (or both), so one `store_code` call
/// works on any backend without the caller branching. A [`CwCode`] or `Vec<u8>` converts via
/// `From` (setting one field, leaving the other `None`), and [`CwCodeSource::both`] supplies
/// both representations so identical deploy code runs unchanged on the mock and on a live chain.
pub struct CwCodeSource {
    /// Native `cw-multi-test` contract object, runnable on the mock backend.
    pub native: Option<CwCode>,
    /// Compiled wasm bytecode, deployable on a live RPC chain.
    pub wasm: Option<Vec<u8>>,
}

impl CwCodeSource {
    /// Carry both representations so the same deploy code runs on either backend.
    pub fn both(native: CwCode, wasm: Vec<u8>) -> Self {
        Self {
            native: Some(native),
            wasm: Some(wasm),
        }
    }
}

impl From<CwCode> for CwCodeSource {
    fn from(code: CwCode) -> Self {
        Self {
            native: Some(code),
            wasm: None,
        }
    }
}

impl From<Vec<u8>> for CwCodeSource {
    fn from(wasm: Vec<u8>) -> Self {
        Self {
            native: None,
            wasm: Some(wasm),
        }
    }
}

/// What a CosmWasm transaction cost: the gas the chain metered for it, and the fee it paid.
///
/// Carried as an `Option` on [`CwStoreCode`], [`CwInstantiate`], and [`CwExecution`]: `Some` on
/// the live RPC backend, where the node reports `gas_used` in the tx result and the signed fee is
/// known, and `None` on the in-process mock.
///
/// `None` means *unmeasured*, not *free*. `cw-multi-test` has no gas meter at all (its response is
/// `{events, data}`, and [`crate::CosmosChainInfo::gas_price`] is documented as metadata the mock
/// does not charge), so the mock has no figure to report. Reporting `0` there would be
/// indistinguishable from a transaction that genuinely cost nothing, so the mock reports absence.
///
/// One `Option` wraps both fields because a backend either meters a transaction or it does not.
/// There is no CosmWasm backend that knows the gas but not the fee, or the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CwGas {
    /// Gas units the chain metered for the transaction (Tendermint's `tx_result.gas_used`).
    pub used: u64,
    /// The fee paid, in base units of the chain's native denom
    /// ([`crate::CosmosChainInfo::native_denom`]).
    ///
    /// This is the whole fee declared in the signed transaction, which is what the sender actually
    /// paid: the Cosmos SDK deducts the declared fee up front and does not refund the gas the
    /// transaction left unspent. It is therefore *not* `used * gas_price`.
    pub fee: u128,
}

/// The gas limit a mutating CosmWasm op runs under. Required on every one of them: there is no
/// default and no fallback, because a limit that is wrong by default is a limit that fails in
/// production.
///
/// The unit is CosmWasm gas, the same quantity [`CwGas::used`] reports, so a limit and a receipt
/// are directly comparable.
///
/// On the live RPC backend the resolved limit is what the signed transaction declares
/// (`Fee::gas_limit`), and the fee follows from it: `ceil(limit * gas_price)` of the chain's
/// native denom. The Cosmos SDK deducts that declared fee in full and refunds nothing, so a
/// limit is not free headroom: raising it raises what the sender pays.
///
/// On the mock backend a limit is inert: `cw-multi-test` has no gas meter, so it cannot run out of
/// gas and has nothing to simulate against. It still takes the limit, so one script runs on either
/// backend. An out-of-gas failure is therefore only reproducible against live RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwGasLimit {
    /// Declare exactly this many gas units. Broadcast as given, never simulated.
    Exact(u64),
    /// Simulate the transaction against the node and scale the reported figure by the chain's
    /// [`crate::CosmosChainInfo::gas_adjustment`]. Costs one extra round trip (the simulation),
    /// and the adjustment is applied exactly once, here: the fee is then computed from the
    /// resolved limit like any other, with no second multiplication.
    Estimated,
}

/// The result of uploading contract code: the assigned code id, the transaction hash, and what
/// the upload cost.
///
/// `tx_hash` follows the same rule as [`CwExecution::tx_hash`]: real on live RPC, synthetic on
/// the mock, always present. `gas` follows [`CwGas`]: `Some` on live RPC, `None` on the mock.
#[derive(Debug, Clone)]
pub struct CwStoreCode {
    /// The code id the chain assigned to the uploaded code.
    pub code_id: u64,
    /// The transaction hash of the upload.
    pub tx_hash: String,
    /// The gas the upload consumed and the fee it paid, or `None` on the mock, which cannot
    /// meter gas. See [`CwGas`].
    pub gas: Option<CwGas>,
}

/// The result of instantiating a contract: the new instance's address, the transaction hash, and
/// what the instantiation cost.
///
/// `tx_hash` follows the same rule as [`CwExecution::tx_hash`]: real on live RPC, synthetic on
/// the mock, always present. `gas` follows [`CwGas`]: `Some` on live RPC, `None` on the mock.
#[derive(Debug, Clone)]
pub struct CwInstantiate {
    /// The address of the newly instantiated contract.
    pub address: Addr,
    /// The transaction hash of the instantiation.
    pub tx_hash: String,
    /// The gas the instantiation consumed and the fee it paid, or `None` on the mock, which
    /// cannot meter gas. See [`CwGas`].
    pub gas: Option<CwGas>,
}

/// The result of a CosmWasm contract execution: the raw `cw-multi-test`-shaped
/// [`CwAppResponse`], the transaction hash, and what the execution cost.
///
/// `tx_hash` is the real Tendermint `broadcast_tx_commit` hash on the live RPC backend and a
/// synthetic, deterministic stand-in on the in-process mock (which never broadcasts), so it is
/// always present and the same test script reads a hash on either backend. The external
/// `cw_multi_test::AppResponse` has no slot for a hash (nor for a gas figure, which it never
/// produces), so this wrapper carries both alongside.
///
/// Derefs to the inner [`CwAppResponse`] so existing `.events` / `.data` access keeps working.
#[derive(Debug, Clone)]
pub struct CwExecution {
    /// The transaction hash: real on live RPC, synthetic on the in-process mock.
    pub tx_hash: String,
    /// The gas the execution consumed and the fee it paid, or `None` on the mock, which cannot
    /// meter gas. See [`CwGas`].
    pub gas: Option<CwGas>,
    /// The raw `cw-multi-test` execution response (emitted events and data).
    pub response: CwAppResponse,
}

impl std::ops::Deref for CwExecution {
    type Target = CwAppResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}
