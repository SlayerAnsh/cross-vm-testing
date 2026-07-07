//! CosmWasm chain providers: the in-process mock and the live-RPC stub.

mod mock;
mod rpc;

pub use mock::{CwApp, CwCode, CwMockProvider, DEFAULT_FUNDING};
pub use rpc::CwRpcProvider;

use crate::CwAppResponse;

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
