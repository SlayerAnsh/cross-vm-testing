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

/// The ceiling a mutating Tron operation runs under, tagged with the quantity it caps.
///
/// Like [`TronCompute`], the two exact variants are not the same quantity and are not
/// interchangeable, and for the same reason. java-tron's only caller-settable knob is `fee_limit`,
/// a number of *sun*: the node divides it by the current energy price to get the energy the
/// transaction may burn, so it is an energy ceiling denominated in TRX. The mock is `revm`, which
/// budgets a transaction in *EVM gas* and has no energy and no price to buy energy with. Handing
/// a backend the other's unit is an error, not a silently ignored cap and not a fabricated
/// conversion.
/// Source: <https://developers.tron.network/docs/set-feelimit>
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TronLimit {
    /// java-tron's `fee_limit`, in sun: the most TRX the transaction may spend on energy, passed
    /// to the node verbatim. The live RPC backend only.
    ///
    /// It binds even a sender whose staked energy would cover the whole transaction and who
    /// therefore burns nothing, because the node caps the transaction's energy at
    /// `min(available energy, fee_limit / energy price)`. It does NOT bound the bandwidth fee,
    /// which java-tron charges outside it.
    Fee(u64),
    /// An EVM gas budget, the only limit `revm` understands. The mock backend only.
    Gas(u64),
    /// Derive the limit from what the operation is forecast to consume, scaled by the chain's
    /// [`gas_adjustment`](crate::TronChainInfo::gas_adjustment).
    ///
    /// Each backend resolves it in the unit it can actually meter: EVM gas on the mock, and on
    /// the live RPC the node's forecast energy priced into a sun `fee_limit` at the chain's
    /// current energy price. Costs the extra round trips the forecast needs.
    Estimated,
}

/// The energy-payment policy a create writes into the contract it deploys: who pays the energy
/// when someone later calls that contract.
///
/// This is NOT a cap on the create transaction (that is [`TronLimit`], which a deploy takes
/// separately). These are two fields of java-tron's `DeployContract` that persist as properties of
/// the deployed contract and bill every FUTURE call to it, which is why they are not part of the
/// per-transaction limit. They come as a pair because either alone is meaningless:
/// `origin_energy_limit` caps what the contract's owner pays, so at
/// `consume_user_resource_percent: 100` the owner pays none of it and the limit never binds.
///
/// The mock ignores this: `revm` bills one payer and has no energy to apportion.
/// Source: <https://developers.tron.network/docs/energy-consumption-mechanism>
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TronEnergyPolicy {
    /// Percentage of a call's energy the CALLER pays, `0..=100`; the contract's owner pays the
    /// rest. java-tron rejects a deploy outside that range.
    pub consume_user_resource_percent: u8,
    /// Ceiling on the energy the contract's OWNER will pay for one call to it. Never binds when
    /// `consume_user_resource_percent` is 100.
    pub origin_energy_limit: u64,
}

/// Scale a forecast by the chain's `gas_adjustment`, rounding up.
///
/// The estimate is the floor of what the operation costs, measured against the state the estimate
/// saw; the adjustment is the headroom for the state having moved by the time the operation runs.
/// Rounding up matters at the bottom of the range, where truncation would hand back a limit under
/// the estimate itself. Saturates rather than wrapping (an f64 -> u64 cast in Rust is saturating).
pub(crate) fn with_headroom(units: u64, gas_adjustment: f64) -> u64 {
    (units as f64 * gas_adjustment).ceil() as u64
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headroom_rounds_up_and_never_lands_under_the_estimate() {
        // The adjustment is headroom, so a fractional result rounds up: truncation would put the
        // limit under the very estimate it was derived from.
        assert_eq!(with_headroom(10_000, 1.3), 13_000);
        assert_eq!(with_headroom(1, 1.3), 2);
        assert_eq!(with_headroom(3, 1.1), 4);
        // An adjustment of exactly 1.0 is the estimate itself (config permits it).
        assert_eq!(with_headroom(21_000, 1.0), 21_000);
        assert_eq!(with_headroom(0, 1.3), 0);
    }

    #[test]
    fn headroom_saturates_instead_of_wrapping() {
        assert_eq!(with_headroom(u64::MAX, 2.0), u64::MAX);
    }
}
