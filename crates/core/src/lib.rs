//! Core abstractions shared by every chain provider in the cross-vm testing suite.
//!
//! The suite spans three execution environments (CosmWasm, EVM, Solana). Each one
//! ships a *chain provider*: the analogue of alloy's `Provider`, cw-orch's `CwEnv`,
//! or test-tube's `Runner`. Every provider wraps an in-process VM ("mock") today and
//! a live RPC connection later, behind the single [`ChainProvider`] trait defined here.
//!
//! Because the three VMs disagree on almost every concrete type (`Addr` vs `Address`
//! vs `Pubkey`, bech32 messages vs ABI calldata vs Borsh instructions), the trait is
//! built from associated types. Each VM keeps its idiomatic types while sharing one
//! method vocabulary, so cross-vm scripts read the same regardless of target.

use thiserror::Error;

/// Which execution environment a provider targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainKind {
    /// CosmWasm chains driven by `cw-multi-test`.
    CosmWasm,
    /// EVM chains driven by `revm`.
    Evm,
    /// Solana (SVM) chains driven by `litesvm`.
    Svm,
}

impl core::fmt::Display for ChainKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            ChainKind::CosmWasm => "cosmwasm",
            ChainKind::Evm => "evm",
            ChainKind::Svm => "svm",
        };
        f.write_str(s)
    }
}

/// Metadata describing a predefined chain.
///
/// Each VM crate defines its own concrete struct (`CosmosChainInfo`, `EvmChainInfo`,
/// `SolanaChainInfo`) carrying VM-specific fields, and implements this trait to expose
/// the fields common to all of them. Predefined constants (`OSMOSIS`, `ETHEREUM`, ...)
/// live in each crate's `chains` module.
pub trait ChainSpec {
    /// Canonical chain identifier (e.g. `"osmosis-1"`, `"1"`, `"mainnet-beta"`).
    fn chain_id(&self) -> &str;
    /// Human-readable name (e.g. `"Osmosis"`, `"Ethereum"`).
    fn name(&self) -> &str;
    /// Native token symbol (e.g. `"OSMO"`, `"ETH"`, `"SOL"`).
    fn native_symbol(&self) -> &str;
    /// Default RPC endpoint, when one is known.
    fn rpc_url(&self) -> Option<&str>;
    /// Which VM this chain runs.
    fn kind(&self) -> ChainKind;
}

/// The uniform provider vocabulary implemented by every VM.
///
/// Associated types let each VM keep its own concrete `Address`/`Msg`/`Response` while
/// sharing method names. The trait is synchronous: all three in-process VMs run
/// synchronously, and the future live-RPC path wraps async internally rather than
/// forcing `async` on every caller.
pub trait ChainProvider {
    /// Concrete [`ChainSpec`] for this provider's chain.
    type Spec: ChainSpec;
    /// Account/address type (`Addr`, `Address`, `Pubkey`).
    type Address;
    /// Signing identity, when distinct from the address.
    type Account;
    /// Deployable code payload (a `ContractWrapper`, EVM bytecode, program bytes).
    type Code;
    /// Message used at deploy/instantiation time.
    type InitMsg;
    /// Message used to mutate a deployed contract/program.
    type ExecMsg;
    /// Message used to read from a deployed contract/program.
    type QueryMsg;
    /// Handle to a deployed contract/program (code id + address, address, program id).
    type ContractRef;
    /// Result of an `execute`.
    type Response;
    /// Result of a `query`.
    type QueryResponse;
    /// Native-balance representation.
    type Balance;
    /// Provider error type. Must convert into [`CrossVmError`] for cross-vm scripts.
    type Error: Into<CrossVmError>;

    /// Metadata for the chain this provider targets.
    fn chain_info(&self) -> &Self::Spec;

    /// Create a fresh account. Mock providers also fund it with a default balance.
    fn new_account(&mut self, label: &str) -> Self::Address;

    /// Read an account's native balance.
    fn balance(&self, addr: &Self::Address) -> Result<Self::Balance, Self::Error>;

    /// Overwrite an account's native balance (mock-only convenience).
    fn set_balance(&mut self, addr: &Self::Address, amount: Self::Balance)
        -> Result<(), Self::Error>;

    /// Current block height / slot.
    fn block_height(&self) -> u64;

    /// Advance the chain by `n` blocks/slots.
    fn advance_blocks(&mut self, n: u64);

    /// Deploy code and return a handle to the deployed instance.
    fn deploy(
        &mut self,
        code: Self::Code,
        init: Self::InitMsg,
        sender: &Self::Address,
    ) -> Result<Self::ContractRef, Self::Error>;

    /// Execute a state-mutating call against a deployed instance.
    fn execute(
        &mut self,
        contract: &Self::ContractRef,
        msg: Self::ExecMsg,
        sender: &Self::Address,
    ) -> Result<Self::Response, Self::Error>;

    /// Run a read-only query against a deployed instance.
    fn query(
        &self,
        contract: &Self::ContractRef,
        msg: Self::QueryMsg,
    ) -> Result<Self::QueryResponse, Self::Error>;
}

/// Unified error type so cross-vm scripts can use one `Result` across all VMs.
///
/// Each provider's own error converts into this via [`ChainProvider::Error`]'s
/// `Into<CrossVmError>` bound.
#[derive(Debug, Error)]
pub enum CrossVmError {
    /// A feature is scaffolded but not yet implemented (e.g. live RPC in phase 1).
    #[error("{kind} provider: {what} is not implemented yet")]
    Unimplemented {
        /// VM the unimplemented feature belongs to.
        kind: ChainKind,
        /// Short description of the missing feature.
        what: String,
    },

    /// Deploying code failed.
    #[error("{kind} deploy failed: {reason}")]
    Deploy {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// Executing a call failed.
    #[error("{kind} execute failed: {reason}")]
    Execute {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// A query failed.
    #[error("{kind} query failed: {reason}")]
    Query {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// A balance read/write failed.
    #[error("{kind} balance op failed: {reason}")]
    Balance {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// Anything else, kept as a message.
    #[error("{kind} error: {reason}")]
    Other {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Message.
        reason: String,
    },
}

impl CrossVmError {
    /// Helper to build an [`CrossVmError::Unimplemented`].
    pub fn unimplemented(kind: ChainKind, what: impl Into<String>) -> Self {
        CrossVmError::Unimplemented {
            kind,
            what: what.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_kind_displays() {
        assert_eq!(ChainKind::CosmWasm.to_string(), "cosmwasm");
        assert_eq!(ChainKind::Evm.to_string(), "evm");
        assert_eq!(ChainKind::Svm.to_string(), "svm");
    }

    #[test]
    fn unimplemented_message_is_clear() {
        let e = CrossVmError::unimplemented(ChainKind::Evm, "live RPC execute");
        assert!(e.to_string().contains("not implemented"));
        assert!(e.to_string().contains("live RPC execute"));
    }
}
