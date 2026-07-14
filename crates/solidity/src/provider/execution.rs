//! The results of the state-mutating EVM operations, shared by both provider backends.

use alloy_primitives::{Address, Bytes, Log, B256};

/// What a state-mutating EVM transaction cost.
///
/// Unlike CosmWasm's `Option<CwGas>`, this is never optional: both EVM backends meter gas (the
/// mock is `revm`, which cannot execute without a gas loop). Only the *fee* can be missing, so
/// the `Option` sits on that field alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvmGas {
    /// Gas the transaction was billed for: the receipt's `gas_used` on the live RPC backend, the
    /// figure `revm` metered on the mock. Already net of the refund on both.
    pub used: u64,
    /// Fee paid, in wei: `used * effective_gas_price` from the receipt on the live RPC backend.
    /// `None` on the mock, which has no gas price: this repo carries no EVM gas-price config, and
    /// `revm` prices the transaction at zero, so any fee it could report would be a fabrication
    /// rather than a measurement.
    pub fee: Option<u128>,
}

/// The result of a state-mutating EVM call: the return data, the logs (events) emitted during
/// execution, the transaction hash, and what the transaction cost.
///
/// `output` is empty on the live RPC backend: a broadcast transaction yields a receipt, not return
/// data. Read a value back with a `static_call` instead.
#[derive(Clone, Debug, Default)]
pub struct EvmExecution {
    /// ABI-encoded return data.
    pub output: Bytes,
    /// Logs (events) emitted during execution, in order.
    pub logs: Vec<Log>,
    /// The transaction hash: the real broadcast hash on the live RPC backend, a synthetic
    /// deterministic one on the mock (which executes in-process and signs no transaction), so
    /// callers never branch on backend.
    pub tx_hash: B256,
    /// Gas burned and, where derivable, the fee paid.
    pub gas: EvmGas,
}

impl From<cross_vm_revm_common::Execution> for EvmExecution {
    fn from(e: cross_vm_revm_common::Execution) -> Self {
        Self {
            output: e.output,
            logs: e.logs,
            tx_hash: e.tx_hash,
            gas: EvmGas {
                used: e.gas_used,
                fee: None,
            },
        }
    }
}

/// The result of an EVM create transaction: the deployed contract address, the transaction hash,
/// and what the transaction cost. Every field is sourced exactly as [`EvmExecution`]'s counterpart
/// is.
#[derive(Clone, Debug, Default)]
pub struct EvmDeploy {
    /// Address of the freshly deployed contract.
    pub address: Address,
    /// The transaction hash (see [`EvmExecution::tx_hash`]).
    pub tx_hash: B256,
    /// Gas burned by the create transaction and, where derivable, the fee paid (see [`EvmGas`]).
    pub gas: EvmGas,
}

impl From<cross_vm_revm_common::Deployment> for EvmDeploy {
    fn from(d: cross_vm_revm_common::Deployment) -> Self {
        Self {
            address: d.address,
            tx_hash: d.tx_hash,
            gas: EvmGas {
                used: d.gas_used,
                fee: None,
            },
        }
    }
}
