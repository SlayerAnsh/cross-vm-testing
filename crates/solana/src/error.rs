//! Errors surfaced by the Solana providers.

use cross_vm_core::{ChainKind, CrossVmError};
use thiserror::Error;

/// Errors surfaced by the Solana providers.
#[derive(Debug, Error)]
pub enum SvmError {
    /// Program deployment failed.
    #[error("deploy: {0}")]
    Deploy(String),
    /// Transaction execution failed.
    #[error("execute: {0}")]
    Execute(String),
    /// A query failed.
    #[error("query: {0}")]
    Query(String),
    /// A balance operation failed.
    #[error("balance: {0}")]
    Balance(String),
    /// An RPC transport / decode failure (connection, JSON-RPC, base64/base58).
    #[error("rpc: {0}")]
    Rpc(String),
    /// Feature not implemented yet (live RPC in phase 1).
    #[error("unimplemented: {0}")]
    Unimplemented(String),
    /// A wallet lookup or key-derivation step failed.
    #[error("wallet: {0}")]
    Wallet(String),
}

impl From<SvmError> for CrossVmError {
    fn from(e: SvmError) -> Self {
        let kind = ChainKind::Svm;
        match e {
            SvmError::Deploy(reason) => CrossVmError::Deploy { kind, reason },
            SvmError::Execute(reason) => CrossVmError::Execute { kind, reason },
            SvmError::Query(reason) => CrossVmError::Query { kind, reason },
            SvmError::Balance(reason) => CrossVmError::Balance { kind, reason },
            SvmError::Rpc(reason) => CrossVmError::Query { kind, reason },
            SvmError::Unimplemented(what) => CrossVmError::Unimplemented { kind, what },
            SvmError::Wallet(reason) => CrossVmError::Wallet { reason },
        }
    }
}

impl From<CrossVmError> for SvmError {
    fn from(e: CrossVmError) -> Self {
        SvmError::Wallet(e.to_string())
    }
}
