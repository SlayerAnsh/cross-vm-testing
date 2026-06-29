//! Errors surfaced by the EVM providers.

use cross_vm_core::{ChainKind, CrossVmError};
use thiserror::Error;

/// Errors surfaced by the EVM providers.
#[derive(Debug, Error)]
pub enum EvmError {
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
    /// Feature not implemented yet (live RPC in phase 1).
    #[error("unimplemented: {0}")]
    Unimplemented(String),
    /// A wallet lookup or key-derivation step failed.
    #[error("wallet: {0}")]
    Wallet(String),
}

impl From<EvmError> for CrossVmError {
    fn from(e: EvmError) -> Self {
        let kind = ChainKind::Evm;
        match e {
            EvmError::Deploy(reason) => CrossVmError::Deploy { kind, reason },
            EvmError::Execute(reason) => CrossVmError::Execute { kind, reason },
            EvmError::Query(reason) => CrossVmError::Query { kind, reason },
            EvmError::Balance(reason) => CrossVmError::Balance { kind, reason },
            EvmError::Rpc(reason) => CrossVmError::Query { kind, reason },
            EvmError::Unimplemented(what) => CrossVmError::Unimplemented { kind, what },
            EvmError::Wallet(reason) => CrossVmError::Wallet { reason },
        }
    }
}

impl From<CrossVmError> for EvmError {
    fn from(e: CrossVmError) -> Self {
        EvmError::Wallet(e.to_string())
    }
}
