//! Outcome type for the testing environment's funding phase.

use thiserror::Error;

/// Outcome of an "ensure this account holds at least N of an asset" operation.
///
/// Used by the testing environment's funding phase. Amounts are carried as strings so
/// the type is VM agnostic (CosmWasm `u128`, EVM `U256`, Solana `u64`). Returned by each
/// VM's `ensure_asset` implementation; the environment turns it into its own error.
#[derive(Debug, Error)]
pub enum FundError {
    /// The account holds less than the required amount of the asset.
    #[error("{asset}: required {required}, found {actual}")]
    Shortfall {
        /// Human-readable asset label (denom, "native", token address).
        asset: String,
        /// Required amount, as a decimal string.
        required: String,
        /// Actual on-chain amount, as a decimal string.
        actual: String,
    },
    /// Funding is not available on this backend (for example live RPC).
    #[error("funding unimplemented: {0}")]
    Unimplemented(String),
    /// An underlying provider call (query/mint) failed.
    #[error("funding provider error: {0}")]
    Provider(String),
}
