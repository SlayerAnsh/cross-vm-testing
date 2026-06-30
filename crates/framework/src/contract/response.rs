//! The uniform return envelope for cross-VM contract executions.
//!
//! A per-VM contract hook produces its native execution result; it wraps that, plus an
//! optional typed payload `T`, into an [`AppResponse<T>`]. The caller reads the typed payload
//! with [`AppResponse::value`] and reaches the raw per-VM result through the accessors.
//!
//! Accessors fail in two distinct ways, kept separate on purpose:
//! - [`CrossVmError::WrongVm`]: the caller used a VM-specific accessor (e.g. `raw_evm`) on a
//!   response from a different VM. Wrong path.
//! - [`CrossVmError::Unsupported`]: the VM matches but the backend does not carry the datum
//!   (e.g. `cw-multi-test` has no transaction hash). Right path, missing data.

use cross_vm_core::{ChainKind, CrossVmError};
use cross_vm_cosmwasm::{CwAppResponse, Event};
use cross_vm_solana::TransactionMetadata;
use cross_vm_solidity::{Bytes, Log};

/// The raw, VM-specific result of an execution.
pub enum RawResponse {
    /// A `cw-multi-test` execution response.
    CosmWasm(CwAppResponse),
    /// The return data and emitted logs of an EVM call.
    Evm {
        /// ABI-encoded return data.
        output: Bytes,
        /// Logs (events) emitted during execution.
        logs: Vec<Log>,
    },
    /// A `litesvm` transaction result.
    Svm(TransactionMetadata),
    /// The return data and emitted logs of a Tron call.
    Tron {
        /// ABI-encoded return data.
        output: Bytes,
        /// Logs (events) emitted during execution.
        logs: Vec<Log>,
    },
}

impl RawResponse {
    /// Which VM produced this result.
    pub fn kind(&self) -> ChainKind {
        match self {
            RawResponse::CosmWasm(_) => ChainKind::CosmWasm,
            RawResponse::Evm { .. } => ChainKind::Evm,
            RawResponse::Svm(_) => ChainKind::Svm,
            RawResponse::Tron { .. } => ChainKind::Tron,
        }
    }

    /// The transaction hash, when the backend provides one.
    ///
    /// Available on Solana (the signature). Returns [`CrossVmError::Unsupported`] on the EVM
    /// and CosmWasm mock backends, which do not expose a hash for an in-process execution.
    pub fn transaction_hash(&self) -> Result<String, CrossVmError> {
        match self {
            RawResponse::Svm(m) => Ok(m.signature.to_string()),
            RawResponse::CosmWasm(_) => Err(CrossVmError::unsupported(
                ChainKind::CosmWasm,
                "transaction hash",
            )),
            RawResponse::Evm { .. } => Err(CrossVmError::unsupported(
                ChainKind::Evm,
                "transaction hash",
            )),
            RawResponse::Tron { .. } => Err(CrossVmError::unsupported(
                ChainKind::Tron,
                "transaction hash",
            )),
        }
    }

    /// Gas / compute units consumed, when the backend reports it.
    pub fn gas_used(&self) -> Option<u128> {
        match self {
            RawResponse::Svm(m) => Some(m.compute_units_consumed as u128),
            // The current EVM and CosmWasm mock paths do not surface a gas figure.
            RawResponse::Evm { .. } | RawResponse::CosmWasm(_) | RawResponse::Tron { .. } => None,
        }
    }

    /// The events emitted by a CosmWasm execution, or [`CrossVmError::WrongVm`] for another VM.
    ///
    /// CosmWasm events are typed key/value attributes. See [`evm_logs`](Self::evm_logs) and
    /// [`solana_logs`](Self::solana_logs) for the other VMs' (differently shaped) event data.
    pub fn cosmwasm_events(&self) -> Result<&[Event], CrossVmError> {
        match self {
            RawResponse::CosmWasm(r) => Ok(&r.events),
            other => Err(CrossVmError::wrong_vm(ChainKind::CosmWasm, other.kind())),
        }
    }

    /// The logs (events) emitted by an EVM call, or [`CrossVmError::WrongVm`] for another VM.
    ///
    /// Each [`Log`] carries an emitting address, indexed topics, and data. Solidity `event`s are
    /// ABI-encoded into this shape.
    pub fn evm_logs(&self) -> Result<&[Log], CrossVmError> {
        match self {
            RawResponse::Evm { logs, .. } => Ok(logs),
            other => Err(CrossVmError::wrong_vm(ChainKind::Evm, other.kind())),
        }
    }

    /// The logs (events) emitted by a Tron call, or [`CrossVmError::WrongVm`] for another VM.
    ///
    /// Tron logs are EVM-shaped: each [`Log`] carries an emitting address, indexed topics, and
    /// data.
    pub fn tron_logs(&self) -> Result<&[Log], CrossVmError> {
        match self {
            RawResponse::Tron { logs, .. } => Ok(logs),
            other => Err(CrossVmError::wrong_vm(ChainKind::Tron, other.kind())),
        }
    }

    /// The program log lines from a Solana execution, or [`CrossVmError::WrongVm`] for another VM.
    ///
    /// These are the raw `msg!` / `sol_log` lines. Anchor `emit!` events are base64-encoded inside
    /// them (`Program data: <base64>`); decoding to a typed event is left to the caller.
    pub fn solana_logs(&self) -> Result<&[String], CrossVmError> {
        match self {
            RawResponse::Svm(m) => Ok(&m.logs),
            other => Err(CrossVmError::wrong_vm(ChainKind::Svm, other.kind())),
        }
    }
}

/// A typed payload `T` plus the raw per-VM execution result.
pub struct AppResponse<T> {
    value: T,
    raw: RawResponse,
}

impl<T> AppResponse<T> {
    /// Build a CosmWasm response.
    pub fn cosmwasm(value: T, raw: CwAppResponse) -> Self {
        Self {
            value,
            raw: RawResponse::CosmWasm(raw),
        }
    }

    /// Build an EVM response from the call's return data and emitted logs.
    pub fn evm(value: T, output: Bytes, logs: Vec<Log>) -> Self {
        Self {
            value,
            raw: RawResponse::Evm { output, logs },
        }
    }

    /// Build a Solana response.
    pub fn solana(value: T, raw: TransactionMetadata) -> Self {
        Self {
            value,
            raw: RawResponse::Svm(raw),
        }
    }

    /// Build a Tron response from the call's return data and emitted logs.
    pub fn tron(value: T, output: Bytes, logs: Vec<Log>) -> Self {
        Self {
            value,
            raw: RawResponse::Tron { output, logs },
        }
    }

    /// Borrow the typed payload.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// Consume the envelope and return the typed payload.
    pub fn into_value(self) -> T {
        self.value
    }

    /// Which VM produced this response.
    pub fn kind(&self) -> ChainKind {
        self.raw.kind()
    }

    /// Borrow the raw, VM-specific result. The uniform handle a hook reads through.
    pub fn raw(&self) -> &RawResponse {
        &self.raw
    }

    /// The transaction hash, when the backend provides one.
    ///
    /// Available on Solana (the signature). Returns [`CrossVmError::Unsupported`] on the EVM
    /// and CosmWasm mock backends, which do not expose a hash for an in-process execution.
    pub fn transaction_hash(&self) -> Result<String, CrossVmError> {
        self.raw.transaction_hash()
    }

    /// Gas / compute units consumed, when the backend reports it.
    pub fn gas_used(&self) -> Option<u128> {
        self.raw.gas_used()
    }

    /// The raw `cw-multi-test` response, or [`CrossVmError::WrongVm`] for another VM.
    pub fn raw_cosmwasm(&self) -> Result<&CwAppResponse, CrossVmError> {
        match &self.raw {
            RawResponse::CosmWasm(r) => Ok(r),
            other => Err(CrossVmError::wrong_vm(ChainKind::CosmWasm, other.kind())),
        }
    }

    /// The events emitted by a CosmWasm execution, or [`CrossVmError::WrongVm`].
    pub fn raw_cosmwasm_events(&self) -> Result<&[Event], CrossVmError> {
        self.raw.cosmwasm_events()
    }

    /// The raw EVM return data, or [`CrossVmError::WrongVm`] for another VM.
    pub fn raw_evm(&self) -> Result<&Bytes, CrossVmError> {
        match &self.raw {
            RawResponse::Evm { output, .. } => Ok(output),
            other => Err(CrossVmError::wrong_vm(ChainKind::Evm, other.kind())),
        }
    }

    /// The logs (events) emitted by an EVM call, or [`CrossVmError::WrongVm`] for another VM.
    pub fn raw_evm_logs(&self) -> Result<&[Log], CrossVmError> {
        self.raw.evm_logs()
    }

    /// The raw Tron return data, or [`CrossVmError::WrongVm`] for another VM.
    pub fn raw_tron(&self) -> Result<&Bytes, CrossVmError> {
        match &self.raw {
            RawResponse::Tron { output, .. } => Ok(output),
            other => Err(CrossVmError::wrong_vm(ChainKind::Tron, other.kind())),
        }
    }

    /// The logs (events) emitted by a Tron call, or [`CrossVmError::WrongVm`] for another VM.
    pub fn raw_tron_logs(&self) -> Result<&[Log], CrossVmError> {
        self.raw.tron_logs()
    }

    /// The program log lines from a Solana execution, or [`CrossVmError::WrongVm`] for another VM.
    pub fn raw_solana_logs(&self) -> Result<&[String], CrossVmError> {
        self.raw.solana_logs()
    }

    /// The raw `litesvm` transaction result, or [`CrossVmError::WrongVm`] for another VM.
    pub fn raw_solana(&self) -> Result<&TransactionMetadata, CrossVmError> {
        match &self.raw {
            RawResponse::Svm(m) => Ok(m),
            other => Err(CrossVmError::wrong_vm(ChainKind::Svm, other.kind())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_accessor_is_wrong_vm_not_unsupported() {
        let resp = AppResponse::evm(7u64, Bytes::new(), vec![]);
        // Right payload.
        assert_eq!(*resp.value(), 7);
        // Wrong-VM accessor: distinct from Unsupported.
        let err = resp.raw_cosmwasm().unwrap_err();
        assert!(matches!(err, CrossVmError::WrongVm { .. }));
    }

    #[test]
    fn missing_datum_is_unsupported_not_wrong_vm() {
        let resp = AppResponse::evm((), Bytes::new(), vec![]);
        // Same-VM datum the backend lacks: Unsupported, not WrongVm.
        let err = resp.transaction_hash().unwrap_err();
        assert!(matches!(err, CrossVmError::Unsupported { .. }));
    }

    #[test]
    fn evm_logs_are_carried_solana_logs_are_wrong_vm() {
        let resp = AppResponse::evm((), Bytes::new(), vec![]);
        // EVM logs accessor on an EVM response: present (empty here), not an error.
        assert!(resp.raw_evm_logs().unwrap().is_empty());
        // Solana-logs accessor on an EVM response: WrongVm.
        assert!(matches!(
            resp.raw_solana_logs(),
            Err(CrossVmError::WrongVm { .. })
        ));
    }
}
