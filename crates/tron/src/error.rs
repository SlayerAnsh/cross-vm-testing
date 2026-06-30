//! Errors surfaced by the Tron providers.

use cross_vm_core::{ChainKind, CrossVmError};
use thiserror::Error;

/// Errors surfaced by the Tron providers.
#[derive(Debug, Error)]
pub enum TronError {
    /// Contract creation failed.
    #[error("deploy: {0}")]
    Deploy(String),
    /// A state-mutating call failed.
    #[error("execute: {0}")]
    Execute(String),
    /// A read-only call failed.
    #[error("query: {0}")]
    Query(String),
    /// A balance operation failed.
    #[error("balance: {0}")]
    Balance(String),
    /// An RPC transport / decode failure (connection, JSON-RPC, ABI).
    #[error("rpc: {0}")]
    Rpc(String),
    /// Feature not implemented yet (live java-tron write paths in a later phase).
    #[error("unimplemented: {0}")]
    Unimplemented(String),
    /// A wallet lookup, key-derivation, or address-encoding step failed.
    #[error("wallet: {0}")]
    Wallet(String),
}

impl From<TronError> for CrossVmError {
    fn from(e: TronError) -> Self {
        let kind = ChainKind::Tron;
        match e {
            TronError::Deploy(reason) => CrossVmError::Deploy { kind, reason },
            TronError::Execute(reason) => CrossVmError::Execute { kind, reason },
            TronError::Query(reason) => CrossVmError::Query { kind, reason },
            TronError::Balance(reason) => CrossVmError::Balance { kind, reason },
            TronError::Rpc(reason) => CrossVmError::Query { kind, reason },
            TronError::Unimplemented(what) => CrossVmError::Unimplemented { kind, what },
            TronError::Wallet(reason) => CrossVmError::Wallet { reason },
        }
    }
}

impl From<CrossVmError> for TronError {
    fn from(e: CrossVmError) -> Self {
        TronError::Wallet(e.to_string())
    }
}
