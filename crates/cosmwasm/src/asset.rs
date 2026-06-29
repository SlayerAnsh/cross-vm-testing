//! A fundable asset on a CosmWasm chain.

use cosmwasm_std::Addr;

/// A fundable asset on a CosmWasm chain.
#[derive(Debug, Clone)]
pub enum CwAsset {
    /// A native bank denom (for example `"uosmo"`).
    Native(String),
    /// A cw20 token contract.
    Cw20(Addr),
}
