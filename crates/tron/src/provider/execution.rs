//! The result of a state-mutating Tron call.

use alloy_primitives::{Bytes, Log, B256};

/// The result of a state-mutating call: return data, emitted logs (events), and (on the live
/// RPC backend) the broadcast transaction hash.
///
/// Tron logs are EVM-shaped (`address` / `topics` / `data`); the mock surfaces `revm`'s logs
/// directly. The only Tron divergence is presentation: a log's `address` is the 20-byte form
/// without the `0x41` prefix. Source: <https://developers.tron.network/docs/event>
#[derive(Clone, Debug, Default)]
pub struct TronExecution {
    /// ABI-encoded return data.
    pub output: Bytes,
    /// Logs (events) emitted during execution, in order.
    pub logs: Vec<Log>,
    /// The transaction hash. The real broadcast hash on the live RPC backend; a synthetic,
    /// deterministic hash on the mock (in-process, no real tx) so callers need not branch on
    /// backend. `None` only appears if a backend explicitly omits it.
    pub tx_hash: Option<B256>,
}
