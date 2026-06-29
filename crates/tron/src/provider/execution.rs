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
    /// The broadcast transaction hash. `Some` on the live RPC backend; `None` on the mock,
    /// which executes in-process without a transaction hash.
    pub tx_hash: Option<B256>,
}
