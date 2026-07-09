//! CosmWasm chain providers: the in-process mock and the live-RPC stub.

mod mock;
mod rpc;

pub use mock::{CwApp, CwCode, CwMockProvider, DEFAULT_FUNDING};
pub use rpc::CwRpcProvider;

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

/// The result of a CosmWasm contract execution: the raw `cw-multi-test`-shaped
/// [`CwAppResponse`] plus the broadcast transaction hash when the backend provides one.
///
/// `tx_hash` is the real Tendermint `broadcast_tx_commit` hash on the live RPC backend and a
/// synthetic, deterministic stand-in on the in-process mock (which never broadcasts). The
/// external `cw_multi_test::AppResponse` has no slot for a hash, so this wrapper carries it
/// alongside; the mock hash lets the same test script read a hash on both backends.
///
/// Derefs to the inner [`CwAppResponse`] so existing `.events` / `.data` access keeps working.
#[derive(Debug, Clone)]
pub struct CwExecution {
    /// The broadcast transaction hash. `Some` on live RPC; `None` on the in-process mock.
    pub tx_hash: Option<String>,
    /// The raw `cw-multi-test` execution response (emitted events and data).
    pub response: CwAppResponse,
}

impl std::ops::Deref for CwExecution {
    type Target = CwAppResponse;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}
