//! Predefined Tron chains.
//!
//! Use as `MAINNET.mock(wallets)` / `NILE.rpc(wallets)` once the construction sugar lands.
//! `spec_id` selects the `revm` hardfork the mock executes against; the TVM tracks the EVM
//! Cancun feature set closely enough for in-process testing.

use super::info::TronChainInfo;
use revm::primitives::hardfork::SpecId;

/// Headroom every preset applies to a forecast, matching the framework's `DEFAULT_GAS_ADJUSTMENT`
/// so a preset-built chain and a config-built one size an `Estimated` limit identically. A chain
/// that needs another value declares one in config.
const DEFAULT_GAS_ADJUSTMENT: f64 = 1.3;

/// Tron mainnet (chain id `0x2b6653dc`).
pub const MAINNET: TronChainInfo = TronChainInfo {
    chain_id: "728126428",
    name: "Tron",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: Some("https://api.trongrid.io"),
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};

/// Nile testnet (Tron staging).
pub const NILE: TronChainInfo = TronChainInfo {
    chain_id: "3448148188",
    name: "Nile",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: Some("https://nile.trongrid.io"),
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};

/// Shasta public testnet.
pub const SHASTA: TronChainInfo = TronChainInfo {
    chain_id: "2494104990",
    name: "Shasta",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: Some("https://api.shasta.trongrid.io"),
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};

/// A generic local chain for fast tests (no real RPC).
pub const LOCAL: TronChainInfo = TronChainInfo {
    chain_id: "9",
    name: "Tron Local",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: None,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};
