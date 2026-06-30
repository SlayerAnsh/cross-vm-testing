//! A fundable asset on a Tron chain.

use crate::provider::address::TronAddress;

/// A fundable asset on a Tron chain.
///
/// TRC10 (Tron's native multi-asset) is out of scope for v1; only the native coin and TRC20
/// (ERC20-shaped) tokens are modeled.
#[derive(Debug, Clone)]
pub enum TronAsset {
    /// The native coin (TRX, denominated in sun).
    Native,
    /// A TRC20 token contract (ERC20-compatible `balanceOf` interface).
    Trc20(TronAddress),
}
