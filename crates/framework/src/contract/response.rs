//! The uniform return envelope for cross-VM contract executions.
//!
//! A per-VM contract hook produces its native execution result; it wraps that, plus an
//! optional typed payload `T`, into an [`AppResponse<T>`]. The caller reads the typed payload
//! with [`AppResponse::value`] and reaches the raw per-VM result through the accessors.
//!
//! VM-specific accessors return [`CrossVmError::WrongVm`] when the caller uses one (e.g.
//! `raw_evm`) on a response from a different VM. Wrong path, right data elsewhere.

use cross_vm_core::{ChainKind, CrossVmError};
#[cfg(feature = "cw")]
use cross_vm_cosmwasm::{CwAppResponse, CwGas, Event};
#[cfg(feature = "solana")]
use cross_vm_solana::TransactionMetadata;
// `Bytes`/`Log` are alloy-primitives types shared by the EVM and Tron variants. Source them
// from whichever provider crate is compiled in (they are the same underlying types).
#[cfg(feature = "evm")]
use cross_vm_solidity::{Bytes, EvmGas, Log, B256};
#[cfg(all(feature = "tron", not(feature = "evm")))]
use cross_vm_tron::{Bytes, Log};
#[cfg(feature = "tron")]
use cross_vm_tron::{TronCompute, TronResources};

/// Render a 32-byte EVM transaction hash as a `0x`-prefixed hex string.
#[cfg(feature = "evm")]
fn hex_hash(h: B256) -> String {
    format!("{h:#x}")
}

/// What an operation consumed and paid, in the unit the backend actually metered.
///
/// A single scalar cannot describe cost across VMs: Tron bills compute and bandwidth as two
/// independent resources, and Solana's fee is priced per signature, not derived from its compute
/// units. The `unit` field keeps the figure self-describing so numbers denominated in different
/// quantities are never silently compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    /// Native execution units consumed.
    pub units: u128,
    /// Which unit `units` is denominated in.
    pub unit: CostUnit,
    /// Bandwidth consumed. Tron only (it bills transaction bytes as a resource independent of
    /// compute); `None` elsewhere.
    pub bandwidth: Option<u64>,
    /// Fee paid, in base units of the chain's native denom, where the backend reports or can
    /// derive one.
    pub fee: Option<u128>,
}

/// The quantity [`Cost::units`] is denominated in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostUnit {
    /// EVM gas. Also CosmWasm gas, and what the Tron mock meters (its engine is `revm`).
    Gas,
    /// Solana compute units.
    ComputeUnits,
    /// Tron energy, billed by the live RPC backend only. The Tron mock never reports this: it
    /// meters EVM gas, which is not the same quantity.
    Energy,
}

impl core::fmt::Display for CostUnit {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            CostUnit::Gas => "gas",
            CostUnit::ComputeUnits => "compute units",
            CostUnit::Energy => "energy",
        };
        f.write_str(s)
    }
}

/// Renders as `<units> <unit>`, then Tron's bandwidth and the fee when each is present:
/// `21000 gas`, `21000 gas, 268 bandwidth`, `150 compute units, fee 5000 (base units)`.
impl core::fmt::Display for Cost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} {}", self.units, self.unit)?;
        if let Some(bandwidth) = self.bandwidth {
            write!(f, ", {bandwidth} bandwidth")?;
        }
        if let Some(fee) = self.fee {
            // `Cost` does not carry the native denom (wei, lamports, sun, uatom), so the fee is
            // qualified as base units rather than rendered bare: an unqualified number reads as a
            // headline currency amount, and 2_100_000 sun is 2.1 TRX, not 2.1 million of anything.
            write!(f, ", fee {fee} (base units)")?;
        }
        Ok(())
    }
}

#[cfg(feature = "evm")]
impl From<EvmGas> for Cost {
    fn from(gas: EvmGas) -> Self {
        Cost {
            units: u128::from(gas.used),
            unit: CostUnit::Gas,
            bandwidth: None,
            fee: gas.fee,
        }
    }
}

#[cfg(feature = "cw")]
impl From<CwGas> for Cost {
    fn from(gas: CwGas) -> Self {
        Cost {
            units: u128::from(gas.used),
            unit: CostUnit::Gas,
            bandwidth: None,
            fee: Some(gas.fee),
        }
    }
}

#[cfg(feature = "tron")]
impl From<TronResources> for Cost {
    fn from(resources: TronResources) -> Self {
        // Each Tron backend states the unit it actually metered: the mock is `revm`, so its
        // figure is EVM gas; the live RPC bills energy. Mapping the mock's gas to `Energy`
        // would mislabel one quantity as another.
        let (units, unit) = match resources.compute {
            TronCompute::Gas(gas) => (u128::from(gas), CostUnit::Gas),
            TronCompute::Energy(energy) => (u128::from(energy), CostUnit::Energy),
        };
        Cost {
            units,
            unit,
            bandwidth: Some(resources.bandwidth),
            fee: resources.fee.map(u128::from),
        }
    }
}

/// The raw, VM-specific result of an execution.
pub enum RawResponse {
    /// A `cw-multi-test` execution response, plus the transaction hash.
    #[cfg(feature = "cw")]
    CosmWasm {
        /// The raw `cw-multi-test` execution response.
        response: CwAppResponse,
        /// The transaction hash: real on the live RPC backend, synthetic on the mock.
        tx_hash: String,
        /// What the transaction cost: `Some` on the live RPC backend, `None` on the mock, which
        /// has no gas meter (unmeasured, not free).
        cost: Option<Cost>,
    },
    /// The return data and emitted logs of an EVM call.
    #[cfg(feature = "evm")]
    Evm {
        /// ABI-encoded return data.
        output: Bytes,
        /// Logs (events) emitted during execution.
        logs: Vec<Log>,
        /// The transaction hash as `0x`-prefixed hex: real on the live RPC backend, synthetic
        /// on the mock.
        tx_hash: String,
        /// What the transaction cost. Its `fee` is `None` on the mock, which has no gas price
        /// to derive one from.
        cost: Cost,
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
        /// The transaction hash as unprefixed hex: real on the live RPC backend, synthetic on
        /// the mock.
        tx_hash: String,
        /// What the transaction cost, in the unit the backend actually metered: EVM gas on the
        /// mock (whose engine is `revm`), energy on the live RPC.
        cost: Cost,
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
    /// The real broadcast hash on the live RPC backends (Solana's base58 signature, hex for
    /// EVM/Tron/CosmWasm). The in-process mock backends mint a synthetic, deterministic hash
    /// instead (they never broadcast), so the same test reads a hash on either backend; see the
    /// per-provider execution types.
    pub fn transaction_hash(&self) -> String {
        match self {
            #[cfg(feature = "solana")]
            RawResponse::Svm(m) => m.signature.to_string(),
            #[cfg(feature = "cw")]
            RawResponse::CosmWasm { tx_hash, .. } => tx_hash.clone(),
            #[cfg(feature = "evm")]
            RawResponse::Evm { tx_hash, .. } => tx_hash.clone(),
            #[cfg(feature = "tron")]
            RawResponse::Tron { tx_hash, .. } => tx_hash.clone(),
        }
    }

    /// What the operation consumed and paid, when the backend meters it.
    ///
    /// `None` means the backend cannot meter (the CosmWasm mock has no gas meter), not that the
    /// operation was free.
    pub fn cost(&self) -> Option<Cost> {
        match self {
            #[cfg(feature = "cw")]
            RawResponse::CosmWasm { cost, .. } => *cost,
            #[cfg(feature = "evm")]
            RawResponse::Evm { cost, .. } => Some(*cost),
            #[cfg(feature = "solana")]
            RawResponse::Svm(m) => Some(Cost {
                units: u128::from(m.compute_units_consumed),
                unit: CostUnit::ComputeUnits,
                bandwidth: None,
                fee: Some(u128::from(m.fee)),
            }),
            #[cfg(feature = "tron")]
            RawResponse::Tron { cost, .. } => Some(*cost),
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
    /// a synthetic, deterministic one on the in-process mock. `gas` is `Some` on live RPC and
    /// `None` on the mock, which has no gas meter.
    #[cfg(feature = "cw")]
    pub fn cosmwasm(value: T, raw: CwAppResponse, tx_hash: String, gas: Option<CwGas>) -> Self {
        Self {
            value,
            raw: RawResponse::CosmWasm {
                response: raw,
                tx_hash,
                cost: gas.map(Cost::from),
            },
        }
    }

    /// Build an EVM response from the call's return data, emitted logs, transaction hash
    /// (real on the live RPC backend, synthetic on the mock), and what it cost. `gas` is never
    /// optional (both EVM backends meter); its `fee` is `None` on the mock, which has no gas
    /// price to derive one from. The 32-byte hash is rendered to `0x`-prefixed hex here, once.
    #[cfg(feature = "evm")]
    pub fn evm(value: T, output: Bytes, logs: Vec<Log>, tx_hash: B256, gas: EvmGas) -> Self {
        Self {
            value,
            raw: RawResponse::Evm {
                output,
                logs,
                tx_hash: hex_hash(tx_hash),
                cost: gas.into(),
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

    /// Build a Tron response from the call's return data, emitted logs, transaction hash
    /// as unprefixed hex (real broadcast `txID` on the live RPC backend, synthetic on the mock),
    /// and the resources it consumed, in the unit the backend actually metered.
    #[cfg(feature = "tron")]
    pub fn tron(
        value: T,
        output: Bytes,
        logs: Vec<Log>,
        tx_hash: String,
        resources: TronResources,
    ) -> Self {
        Self {
            value,
            raw: RawResponse::Tron {
                output,
                logs,
                tx_hash,
                cost: resources.into(),
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

    /// The transaction hash: real on the live RPC backends, synthetic and deterministic on the
    /// in-process mocks. See [`RawResponse::transaction_hash`].
    pub fn transaction_hash(&self) -> String {
        self.raw.transaction_hash()
    }

    /// What the operation consumed and paid, when the backend meters it. `None` means the
    /// backend cannot meter, not that the operation was free. See [`RawResponse::cost`].
    pub fn cost(&self) -> Option<Cost> {
        self.raw.cost()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_unit_renders_the_chain_vocabulary_not_the_variant_name() {
        assert_eq!(CostUnit::Gas.to_string(), "gas");
        assert_eq!(CostUnit::ComputeUnits.to_string(), "compute units");
        assert_eq!(CostUnit::Energy.to_string(), "energy");
    }

    #[test]
    fn cost_renders_gas_without_a_fee() {
        // EVM/Tron mock shape: metered, but no gas price to derive a fee from.
        let cost = Cost {
            units: 21_000,
            unit: CostUnit::Gas,
            bandwidth: None,
            fee: None,
        };
        assert_eq!(cost.to_string(), "21000 gas");
    }

    #[test]
    fn cost_renders_gas_with_a_fee() {
        // EVM/CosmWasm RPC shape.
        let cost = Cost {
            units: 21_000,
            unit: CostUnit::Gas,
            bandwidth: None,
            fee: Some(42_000),
        };
        assert_eq!(cost.to_string(), "21000 gas, fee 42000 (base units)");
    }

    #[test]
    fn cost_renders_compute_units_with_a_fee() {
        // Solana shape: compute units metered, fee priced per signature.
        let cost = Cost {
            units: 150,
            unit: CostUnit::ComputeUnits,
            bandwidth: None,
            fee: Some(5_000),
        };
        assert_eq!(cost.to_string(), "150 compute units, fee 5000 (base units)");
    }

    #[test]
    fn cost_renders_bandwidth_without_a_fee() {
        // Tron mock shape: gas (its engine is revm) plus bandwidth, no fee.
        let cost = Cost {
            units: 21_000,
            unit: CostUnit::Gas,
            bandwidth: Some(268),
            fee: None,
        };
        assert_eq!(cost.to_string(), "21000 gas, 268 bandwidth");
    }

    #[test]
    fn cost_renders_energy_with_bandwidth_and_a_fee() {
        // Tron RPC shape: every field populated.
        let cost = Cost {
            units: 64_000,
            unit: CostUnit::Energy,
            bandwidth: Some(345),
            fee: Some(2_100_000),
        };
        assert_eq!(
            cost.to_string(),
            "64000 energy, 345 bandwidth, fee 2100000 (base units)"
        );
    }

    /// An empty EVM response, costed the way the mock backend costs one: metered gas, no fee.
    #[cfg(feature = "evm")]
    fn evm_resp<T>(value: T) -> AppResponse<T> {
        let gas = EvmGas {
            used: 21_000,
            fee: None,
        };
        AppResponse::evm(value, Bytes::new(), vec![], B256::ZERO, gas)
    }

    #[cfg(feature = "cw")]
    fn cw_raw() -> CwAppResponse {
        CwAppResponse {
            events: vec![],
            data: None,
            msg_responses: vec![],
        }
    }

    #[cfg(all(feature = "evm", feature = "cw"))]
    #[test]
    fn wrong_accessor_is_wrong_vm() {
        let resp = evm_resp(7u64);
        // Right payload.
        assert_eq!(*resp.value(), 7);
        // Wrong-VM accessor.
        let err = resp.raw_cosmwasm().unwrap_err();
        assert!(matches!(err, CrossVmError::WrongVm { .. }));
    }

    #[cfg(feature = "evm")]
    #[test]
    fn evm_hash_is_hex_rendered() {
        // The 32-byte hash is rendered as `0x`-prefixed hex at the constructor boundary.
        let h = B256::with_last_byte(0xAB);
        let resp = AppResponse::evm((), Bytes::new(), vec![], h, EvmGas::default());
        assert_eq!(resp.transaction_hash(), format!("{h:#x}"));
    }

    #[cfg(feature = "cw")]
    #[test]
    fn cosmwasm_hash_is_carried() {
        // The hash surfaces through the envelope verbatim.
        let resp = AppResponse::cosmwasm((), cw_raw(), "ABCD1234".to_string(), None);
        assert_eq!(resp.transaction_hash(), "ABCD1234");
    }

    #[cfg(all(feature = "evm", feature = "solana"))]
    #[test]
    fn evm_logs_are_carried_solana_logs_are_wrong_vm() {
        let resp = evm_resp(());
        // EVM logs accessor on an EVM response: present (empty here), not an error.
        assert!(resp.raw_evm_logs().unwrap().is_empty());
        // Solana-logs accessor on an EVM response: WrongVm.
        assert!(matches!(
            resp.raw_solana_logs(),
            Err(CrossVmError::WrongVm { .. })
        ));
    }

    #[cfg(feature = "evm")]
    #[test]
    fn evm_cost_reports_gas_and_carries_fee() {
        // Mock-shaped: gas metered, no fee (the mock has no gas price).
        assert_eq!(
            evm_resp(()).cost(),
            Some(Cost {
                units: 21_000,
                unit: CostUnit::Gas,
                bandwidth: None,
                fee: None,
            })
        );
        // RPC-shaped: the receipt's fee rides along, in wei.
        let gas = EvmGas {
            used: 21_000,
            fee: Some(42_000),
        };
        let resp = AppResponse::evm((), Bytes::new(), vec![], B256::ZERO, gas);
        assert_eq!(resp.cost().unwrap().fee, Some(42_000));
    }

    #[cfg(feature = "cw")]
    #[test]
    fn cosmwasm_mock_cost_is_none_not_zero() {
        // The mock backend produces `gas: None` (cw-multi-test has no gas meter). The envelope
        // must surface that as absence; a fabricated `Some` with zero units would claim the
        // transaction was metered as free.
        let resp = AppResponse::cosmwasm((), cw_raw(), "AB".to_string(), None);
        assert_eq!(resp.cost(), None);
    }

    #[cfg(feature = "cw")]
    #[test]
    fn cosmwasm_rpc_gas_maps_to_gas_cost() {
        let gas = CwGas {
            used: 123_456,
            fee: 5_000,
        };
        let resp = AppResponse::cosmwasm((), cw_raw(), "AB".to_string(), Some(gas));
        assert_eq!(
            resp.cost(),
            Some(Cost {
                units: 123_456,
                unit: CostUnit::Gas,
                bandwidth: None,
                fee: Some(5_000),
            })
        );
    }

    #[cfg(feature = "tron")]
    #[test]
    fn tron_mock_gas_stays_gas_not_energy() {
        // The Tron mock is `revm`, so it meters EVM gas; the envelope must not relabel that as
        // energy, which is a different quantity the mock never measures.
        let resources = TronResources {
            compute: TronCompute::Gas(21_000),
            bandwidth: 268,
            fee: None,
        };
        let resp = AppResponse::tron((), Bytes::new(), vec![], "ab".to_string(), resources);
        assert_eq!(
            resp.cost(),
            Some(Cost {
                units: 21_000,
                unit: CostUnit::Gas,
                bandwidth: Some(268),
                fee: None,
            })
        );
    }

    #[cfg(feature = "tron")]
    #[test]
    fn tron_rpc_energy_maps_to_energy() {
        let resources = TronResources {
            compute: TronCompute::Energy(64_000),
            bandwidth: 345,
            fee: Some(2_100_000),
        };
        let resp = AppResponse::tron((), Bytes::new(), vec![], "ab".to_string(), resources);
        assert_eq!(
            resp.cost(),
            Some(Cost {
                units: 64_000,
                unit: CostUnit::Energy,
                bandwidth: Some(345),
                fee: Some(2_100_000),
            })
        );
    }

    #[cfg(feature = "solana")]
    #[test]
    fn solana_cost_reads_compute_units_and_fee() {
        // Both figures come off litesvm's metadata: the compute units and the lamport fee.
        let meta = TransactionMetadata {
            compute_units_consumed: 150,
            fee: 5_000,
            ..Default::default()
        };
        let resp = AppResponse::solana((), meta);
        assert_eq!(
            resp.cost(),
            Some(Cost {
                units: 150,
                unit: CostUnit::ComputeUnits,
                bandwidth: None,
                fee: Some(5_000),
            })
        );
    }
}
