//! Errors surfaced by the CosmWasm providers.

use cross_vm_core::{ChainKind, CrossVmError};
use cw_multi_test::error::AnyError;
use thiserror::Error;

/// Flatten an `anyhow` error chain into a single string, root cause included.
///
/// `cw-multi-test` layers `.context(..)` over the contract's own error (the outermost layer is
/// `"Error executing WasmMsg: sender: .. Execute { .. }"`), and `AnyError`'s `Display` prints
/// only that outermost layer. Rendering every link keeps the real failure (`Unauthorized`,
/// `Std error`, an overflow, ..) in the message instead of discarding it.
pub(crate) fn any_chain(e: &AnyError) -> String {
    let mut out = e.to_string();
    for cause in e.chain().skip(1) {
        out.push_str("\n  caused by: ");
        out.push_str(&cause.to_string());
    }
    out
}

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
        // Keep the variant across the round trip, so an execution revert that re-enters as a
        // `CwError` still reads as `execute: ..` rather than being relabelled `wallet: ..`.
        match e {
            CrossVmError::Deploy { reason, .. } => CwError::Deploy(reason),
            CrossVmError::Execute { reason, .. } => CwError::Execute(reason),
            CrossVmError::Query { reason, .. } => CwError::Query(reason),
            CrossVmError::Balance { reason, .. } => CwError::Balance(reason),
            CrossVmError::Unimplemented { what, .. } => CwError::Unimplemented(what),
            CrossVmError::Wallet { reason } => CwError::Wallet(reason),
            other => CwError::Wallet(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cw_multi_test::error::{anyhow, AnyContext};

    #[test]
    fn any_chain_keeps_the_root_cause() {
        // The shape `cw-multi-test` produces: a WasmMsg context layered over the contract error.
        let e: AnyError = Err::<(), _>(anyhow!("Unauthorized"))
            .context("Error executing WasmMsg:\n  sender: euclid1dz6")
            .unwrap_err();

        assert_eq!(
            e.to_string(),
            "Error executing WasmMsg:\n  sender: euclid1dz6"
        );
        assert_eq!(
            any_chain(&e),
            "Error executing WasmMsg:\n  sender: euclid1dz6\n  caused by: Unauthorized"
        );
    }

    #[test]
    fn any_chain_renders_every_link() {
        let e: AnyError = Err::<(), _>(anyhow!("overflow"))
            .context("contract error")
            .context("submsg 0")
            .unwrap_err();
        assert_eq!(
            any_chain(&e),
            "submsg 0\n  caused by: contract error\n  caused by: overflow"
        );
    }

    #[test]
    fn cross_vm_round_trip_keeps_the_variant() {
        let e = CrossVmError::Execute {
            kind: ChainKind::CosmWasm,
            reason: "Unauthorized".into(),
        };
        assert!(matches!(CwError::from(e), CwError::Execute(r) if r == "Unauthorized"));
    }
}
