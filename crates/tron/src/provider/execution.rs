//! The results of the state-mutating Tron operations: a call and a create.
//!
//! Both report their transaction hash as a `String` of unprefixed hex, the shape java-tron renders
//! a `txID` in. A Tron txID is the SHA256 of the raw transaction, not an EVM keccak hash, so it is
//! not a `B256` in any meaningful sense; the mock's `revm` engine mints one only because it shares
//! the EVM core, and it is rendered to hex at that boundary (the one conversion in this crate).

use alloy_primitives::{Bytes, Log};

use crate::provider::address::TronAddress;

/// The compute a Tron backend metered, tagged with the quantity it is denominated in.
///
/// The two variants are not the same quantity and are not comparable. Tron bills computation in
/// energy; the mock, however, *is* `revm`, so what it meters is EVM gas. Its energy shim
/// ([`crate::tvm::resources`]) sits outside `revm`'s gas loop and is never decremented by contract
/// execution, so the mock has no energy figure to report and reporting its gas as energy would be
/// a lie. Each backend states the unit it actually metered; a caller reads the variant to know
/// which it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TronCompute {
    /// EVM gas, as `revm` metered it (`ResultGas::tx_gas_used`). The mock backend only.
    Gas(u64),
    /// Tron energy, as java-tron billed it (`receipt.energy_usage_total`). The live RPC backend
    /// only.
    Energy(u64),
}

/// What a Tron operation consumed: compute (in the unit the backend metered), bandwidth, and the
/// fee it was billed.
///
/// Tron bills two independent resources, neither derivable from the other: energy for computation
/// and bandwidth for transaction bytes.
/// Source: <https://developers.tron.network/docs/resource-model>
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TronResources {
    /// Compute consumed, carrying the unit the backend metered (see [`TronCompute`]).
    pub compute: TronCompute,
    /// Bandwidth points deducted for this transaction: `receipt.net_usage` on the live RPC, and on
    /// the mock what the [resource shim](crate::tvm::resources) charged by transaction-payload
    /// length. Zero on either backend when the free allowance did not cover the transaction, since
    /// the bytes were then paid for by burning TRX instead of by deducting points (the mock does
    /// not charge that burn).
    pub bandwidth: u64,
    /// Fee billed, in sun. `None` on the mock, which burns nothing and cannot derive a fee from
    /// revm's gas: a Tron fee is priced off energy, which the mock does not meter.
    pub fee: Option<u64>,
}

/// The result of a state-mutating call: return data, emitted logs (events), the transaction hash,
/// and the resources the operation consumed.
///
/// Tron logs are EVM-shaped (`address` / `topics` / `data`); the mock surfaces `revm`'s logs
/// directly. The only Tron divergence is presentation: a log's `address` is the 20-byte form
/// without the `0x41` prefix. Source: <https://developers.tron.network/docs/event>
#[derive(Clone, Debug)]
pub struct TronExecution {
    /// ABI-encoded return data.
    pub output: Bytes,
    /// Logs (events) emitted during execution, in order.
    pub logs: Vec<Log>,
    /// The transaction hash as unprefixed hex: the real broadcast `txID` on the live RPC backend, a
    /// synthetic deterministic one on the mock (in-process, no real tx), so callers never branch on
    /// backend.
    pub tx_hash: String,
    /// What the call consumed, in the unit the backend actually metered.
    pub resources: TronResources,
}

/// The result of a create transaction: the deployed contract's address, the transaction hash, and
/// the resources the deploy consumed.
#[derive(Clone, Debug)]
pub struct TronDeploy {
    /// Address of the freshly deployed contract.
    pub address: TronAddress,
    /// The transaction hash as unprefixed hex (see the module note).
    pub tx_hash: String,
    /// What the deploy consumed, in the unit the backend actually metered.
    pub resources: TronResources,
}
