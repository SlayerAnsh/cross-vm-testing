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
//!   (e.g. a backend that omits a gas figure). Right path, missing data.

use cross_vm_core::{ChainKind, CrossVmError};
#[cfg(feature = "cw")]
use cross_vm_cosmwasm::{CwAppResponse, Event};
#[cfg(feature = "solana")]
use cross_vm_solana::TransactionMetadata;
// `Bytes`/`Log`/`B256` are alloy-primitives types shared by the EVM and Tron variants. Source them
// from whichever provider crate is compiled in (they are the same underlying types).
#[cfg(feature = "evm")]
use cross_vm_solidity::{Bytes, Log, B256};
#[cfg(all(feature = "tron", not(feature = "evm")))]
use cross_vm_tron::{Bytes, Log, B256};

/// Return a stored per-VM tx hash as `Ok`, or [`CrossVmError::Unsupported`] when the backend
/// (an in-process mock) carries none. Shared by the EVM, Tron, and CosmWasm hash paths.
#[cfg(any(feature = "cw", feature = "evm", feature = "tron"))]
fn hash_or_unsupported(tx_hash: &Option<String>, kind: ChainKind) -> Result<String, CrossVmError> {
    tx_hash
        .clone()
        .ok_or_else(|| CrossVmError::unsupported(kind, "transaction hash"))
}

/// Render a 32-byte EVM/Tron transaction hash as a `0x`-prefixed hex string.
#[cfg(any(feature = "evm", feature = "tron"))]
fn hex_hash(h: B256) -> String {
    format!("{h:#x}")
}

/// The raw, VM-specific result of an execution.
pub enum RawResponse {
    /// A `cw-multi-test` execution response, plus the broadcast tx hash on the live RPC backend.
    #[cfg(feature = "cw")]
    CosmWasm {
        /// The raw `cw-multi-test` execution response.
        response: CwAppResponse,
        /// The broadcast transaction hash. `Some` on live RPC; `None` on the in-process mock.
        tx_hash: Option<String>,
    },
    /// The return data and emitted logs of an EVM call.
    #[cfg(feature = "evm")]
    Evm {
        /// ABI-encoded return data.
        output: Bytes,
        /// Logs (events) emitted during execution.
        logs: Vec<Log>,
        /// The broadcast transaction hash. `Some` on the live RPC backend; `None` on the mock.
        tx_hash: Option<String>,
    },
    /// A `litesvm` transaction result.
    #[cfg(feature = "solana")]
    Svm(TransactionMetadata),
    /// The return data and emitted logs of a Tron call.
    #[cfg(feature = "tron")]
    Tron {
        /// ABI-encoded return data.
        output: Bytes,
        /// Logs (events) emitted during execution.
        logs: Vec<Log>,
        /// The broadcast transaction hash. `Some` on the live RPC backend; `None` on the mock.
        tx_hash: Option<String>,
    },
}

impl RawResponse {
    /// Which VM produced this result.
    pub fn kind(&self) -> ChainKind {
        match self {
            #[cfg(feature = "cw")]
            RawResponse::CosmWasm { .. } => ChainKind::CosmWasm,
            #[cfg(feature = "evm")]
            RawResponse::Evm { .. } => ChainKind::Evm,
            #[cfg(feature = "solana")]
            RawResponse::Svm(_) => ChainKind::Svm,
            #[cfg(feature = "tron")]
            RawResponse::Tron { .. } => ChainKind::Tron,
        }
    }

    /// The transaction hash.
    ///
    /// The real broadcast hash on Solana (the signature) and on the live RPC backends for EVM,
    /// Tron, and CosmWasm. The in-process mock backends carry a synthetic, deterministic hash
    /// instead (they never broadcast), so the same test reads a hash on either backend; see the
    /// per-provider execution types. Returns [`CrossVmError::Unsupported`] only if a backend
    /// explicitly omits the hash.
    pub fn transaction_hash(&self) -> Result<String, CrossVmError> {
        match self {
            #[cfg(feature = "solana")]
            RawResponse::Svm(m) => Ok(m.signature.to_string()),
            #[cfg(feature = "cw")]
            RawResponse::CosmWasm { tx_hash, .. } => {
                hash_or_unsupported(tx_hash, ChainKind::CosmWasm)
            }
            #[cfg(feature = "evm")]
            RawResponse::Evm { tx_hash, .. } => hash_or_unsupported(tx_hash, ChainKind::Evm),
            #[cfg(feature = "tron")]
            RawResponse::Tron { tx_hash, .. } => hash_or_unsupported(tx_hash, ChainKind::Tron),
        }
    }

    /// Gas / compute units consumed, when the backend reports it.
    pub fn gas_used(&self) -> Option<u128> {
        match self {
            // Solana reports compute units; the EVM, CosmWasm, and Tron mock paths do not surface
            // a gas figure.
            #[cfg(feature = "solana")]
            RawResponse::Svm(m) => Some(m.compute_units_consumed as u128),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// The events emitted by a CosmWasm execution, or [`CrossVmError::WrongVm`] for another VM.
    ///
    /// CosmWasm events are typed key/value attributes. See [`evm_logs`](Self::evm_logs) and
    /// [`solana_logs`](Self::solana_logs) for the other VMs' (differently shaped) event data.
    #[cfg(feature = "cw")]
    #[allow(unreachable_patterns)]
    pub fn cosmwasm_events(&self) -> Result<&[Event], CrossVmError> {
        match self {
            RawResponse::CosmWasm { response, .. } => Ok(&response.events),
            other => Err(CrossVmError::wrong_vm(ChainKind::CosmWasm, other.kind())),
        }
    }

    /// The logs (events) emitted by an EVM call, or [`CrossVmError::WrongVm`] for another VM.
    ///
    /// Each [`Log`] carries an emitting address, indexed topics, and data. Solidity `event`s are
    /// ABI-encoded into this shape.
    #[cfg(feature = "evm")]
    #[allow(unreachable_patterns)]
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
    #[cfg(feature = "tron")]
    #[allow(unreachable_patterns)]
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
    #[cfg(feature = "solana")]
    #[allow(unreachable_patterns)]
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
    /// Build a CosmWasm response. `tx_hash` is the broadcast hash on the live RPC backend and
    /// `None` on the in-process mock.
    #[cfg(feature = "cw")]
    pub fn cosmwasm(value: T, raw: CwAppResponse, tx_hash: Option<String>) -> Self {
        Self {
            value,
            raw: RawResponse::CosmWasm {
                response: raw,
                tx_hash,
            },
        }
    }

    /// Build an EVM response from the call's return data, emitted logs, and (on the live RPC
    /// backend) the broadcast transaction hash. `tx_hash` is `None` on the mock.
    #[cfg(feature = "evm")]
    pub fn evm(value: T, output: Bytes, logs: Vec<Log>, tx_hash: Option<B256>) -> Self {
        Self {
            value,
            raw: RawResponse::Evm {
                output,
                logs,
                tx_hash: tx_hash.map(hex_hash),
            },
        }
    }

    /// Build a Solana response.
    #[cfg(feature = "solana")]
    pub fn solana(value: T, raw: TransactionMetadata) -> Self {
        Self {
            value,
            raw: RawResponse::Svm(raw),
        }
    }

    /// Build a Tron response from the call's return data, emitted logs, and (on the live RPC
    /// backend) the broadcast transaction hash. `tx_hash` is `None` on the mock.
    #[cfg(feature = "tron")]
    pub fn tron(value: T, output: Bytes, logs: Vec<Log>, tx_hash: Option<B256>) -> Self {
        Self {
            value,
            raw: RawResponse::Tron {
                output,
                logs,
                tx_hash: tx_hash.map(hex_hash),
            },
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
    #[cfg(feature = "cw")]
    #[allow(unreachable_patterns)]
    pub fn raw_cosmwasm(&self) -> Result<&CwAppResponse, CrossVmError> {
        match &self.raw {
            RawResponse::CosmWasm { response, .. } => Ok(response),
            other => Err(CrossVmError::wrong_vm(ChainKind::CosmWasm, other.kind())),
        }
    }

    /// The events emitted by a CosmWasm execution, or [`CrossVmError::WrongVm`].
    #[cfg(feature = "cw")]
    pub fn raw_cosmwasm_events(&self) -> Result<&[Event], CrossVmError> {
        self.raw.cosmwasm_events()
    }

    /// The raw EVM return data, or [`CrossVmError::WrongVm`] for another VM.
    #[cfg(feature = "evm")]
    #[allow(unreachable_patterns)]
    pub fn raw_evm(&self) -> Result<&Bytes, CrossVmError> {
        match &self.raw {
            RawResponse::Evm { output, .. } => Ok(output),
            other => Err(CrossVmError::wrong_vm(ChainKind::Evm, other.kind())),
        }
    }

    /// The logs (events) emitted by an EVM call, or [`CrossVmError::WrongVm`] for another VM.
    #[cfg(feature = "evm")]
    pub fn raw_evm_logs(&self) -> Result<&[Log], CrossVmError> {
        self.raw.evm_logs()
    }

    /// The raw Tron return data, or [`CrossVmError::WrongVm`] for another VM.
    #[cfg(feature = "tron")]
    #[allow(unreachable_patterns)]
    pub fn raw_tron(&self) -> Result<&Bytes, CrossVmError> {
        match &self.raw {
            RawResponse::Tron { output, .. } => Ok(output),
            other => Err(CrossVmError::wrong_vm(ChainKind::Tron, other.kind())),
        }
    }

    /// The logs (events) emitted by a Tron call, or [`CrossVmError::WrongVm`] for another VM.
    #[cfg(feature = "tron")]
    pub fn raw_tron_logs(&self) -> Result<&[Log], CrossVmError> {
        self.raw.tron_logs()
    }

    /// The program log lines from a Solana execution, or [`CrossVmError::WrongVm`] for another VM.
    #[cfg(feature = "solana")]
    pub fn raw_solana_logs(&self) -> Result<&[String], CrossVmError> {
        self.raw.solana_logs()
    }

    /// The raw `litesvm` transaction result, or [`CrossVmError::WrongVm`] for another VM.
    #[cfg(feature = "solana")]
    #[allow(unreachable_patterns)]
    pub fn raw_solana(&self) -> Result<&TransactionMetadata, CrossVmError> {
        match &self.raw {
            RawResponse::Svm(m) => Ok(m),
            other => Err(CrossVmError::wrong_vm(ChainKind::Svm, other.kind())),
        }
    }
}

#[cfg(all(test, feature = "evm", feature = "cw"))]
mod tests {
    use super::*;

    #[test]
    fn wrong_accessor_is_wrong_vm_not_unsupported() {
        let resp = AppResponse::evm(7u64, Bytes::new(), vec![], None);
        // Right payload.
        assert_eq!(*resp.value(), 7);
        // Wrong-VM accessor: distinct from Unsupported.
        let err = resp.raw_cosmwasm().unwrap_err();
        assert!(matches!(err, CrossVmError::WrongVm { .. }));
    }

    #[test]
    fn missing_datum_is_unsupported_not_wrong_vm() {
        // Mock-shaped EVM response: no hash present.
        let resp = AppResponse::evm((), Bytes::new(), vec![], None);
        // Same-VM datum the backend lacks: Unsupported, not WrongVm.
        let err = resp.transaction_hash().unwrap_err();
        assert!(matches!(err, CrossVmError::Unsupported { .. }));
    }

    #[test]
    fn evm_hash_is_carried_and_hex_rendered() {
        // RPC-shaped EVM response: a hash is present and rendered as `0x`-prefixed hex.
        let h = B256::with_last_byte(0xAB);
        let resp = AppResponse::evm((), Bytes::new(), vec![], Some(h));
        assert_eq!(resp.transaction_hash().unwrap(), format!("{h:#x}"));
    }

    #[test]
    fn cosmwasm_hash_is_carried_when_present() {
        // RPC-shaped CosmWasm response: the broadcast hash surfaces through the envelope.
        let raw = CwAppResponse {
            events: vec![],
            data: None,
            msg_responses: vec![],
        };
        let resp = AppResponse::cosmwasm((), raw, Some("ABCD1234".to_string()));
        assert_eq!(resp.transaction_hash().unwrap(), "ABCD1234");
    }

    #[test]
    fn absent_cosmwasm_hash_is_unsupported() {
        // A response built without a hash reads as Unsupported, not WrongVm. (Real backends now
        // supply a hash; this covers the explicit `None` path.)
        let raw = CwAppResponse {
            events: vec![],
            data: None,
            msg_responses: vec![],
        };
        let resp = AppResponse::cosmwasm((), raw, None);
        assert!(matches!(
            resp.transaction_hash(),
            Err(CrossVmError::Unsupported { .. })
        ));
    }

    #[test]
    fn evm_logs_are_carried_solana_logs_are_wrong_vm() {
        let resp = AppResponse::evm((), Bytes::new(), vec![], None);
        // EVM logs accessor on an EVM response: present (empty here), not an error.
        assert!(resp.raw_evm_logs().unwrap().is_empty());
        // Solana-logs accessor on an EVM response: WrongVm.
        assert!(matches!(
            resp.raw_solana_logs(),
            Err(CrossVmError::WrongVm { .. })
        ));
    }
}
