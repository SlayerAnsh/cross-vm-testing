//! Errors surfaced by the CosmWasm providers.

use cross_vm_core::{ChainKind, CrossVmError};
use thiserror::Error;

/// Errors surfaced by the CosmWasm providers.
#[derive(Debug, Error)]
pub enum CwError {
    /// `store_code` / `instantiate_contract` failed.
    #[error("deploy: {0}")]
    Deploy(String),
    /// `execute_contract` failed.
    #[error("execute: {0}")]
    Execute(String),
    /// `query_wasm_smart` failed.
    #[error("query: {0}")]
    Query(String),
    /// A bank operation failed.
    #[error("balance: {0}")]
    Balance(String),
    /// An RPC transport / decode failure (connection, ABCI query, protobuf).
    #[error("rpc: {0}")]
    Rpc(String),
    /// Feature not implemented yet (live RPC in phase 1).
    #[error("unimplemented: {0}")]
    Unimplemented(String),
    /// A wallet lookup or key-derivation step failed.
    #[error("wallet: {0}")]
    Wallet(String),
}

impl From<CwError> for CrossVmError {
    fn from(e: CwError) -> Self {
        let kind = ChainKind::CosmWasm;
        match e {
            CwError::Deploy(reason) => CrossVmError::Deploy { kind, reason },
            CwError::Execute(reason) => CrossVmError::Execute { kind, reason },
            CwError::Query(reason) => CrossVmError::Query { kind, reason },
            CwError::Balance(reason) => CrossVmError::Balance { kind, reason },
            CwError::Rpc(reason) => CrossVmError::Query { kind, reason },
            CwError::Unimplemented(what) => CrossVmError::Unimplemented { kind, what },
            CwError::Wallet(reason) => CrossVmError::Wallet { reason },
        }
    }
}

impl From<CrossVmError> for CwError {
    fn from(e: CrossVmError) -> Self {
        CwError::Wallet(e.to_string())
    }
}
