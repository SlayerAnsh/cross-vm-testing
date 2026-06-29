//! A fundable asset on an EVM chain.

use alloy_primitives::Address;

/// A fundable asset on an EVM chain.
#[derive(Debug, Clone)]
pub enum EvmAsset {
    /// The native coin (ETH and equivalents).
    Native,
    /// An ERC-20 token contract.
    Erc20(Address),
}
