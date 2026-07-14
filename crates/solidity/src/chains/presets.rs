//! Predefined EVM chains.
//!
//! Use them as `EvmMockProvider::new(ETHEREUM, wallets)` or `ETHEREUM.mock(wallets)`, where
//! `wallets` is the shared `Rc<WalletFactory>`.

use super::info::EvmChainInfo;
use revm::primitives::hardfork::SpecId;

/// Ethereum mainnet.
pub const ETHEREUM: EvmChainInfo = EvmChainInfo {
    chain_id: "1",
    name: "Ethereum",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    gas_adjustment: 1.3,
    rpc_url: Some("https://eth.llamarpc.com"),
};

/// Ethereum Sepolia testnet.
pub const SEPOLIA: EvmChainInfo = EvmChainInfo {
    chain_id: "11155111",
    name: "Sepolia",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    gas_adjustment: 1.3,
    rpc_url: Some("https://ethereum-sepolia-rpc.publicnode.com"),
};

/// Base Sepolia testnet.
pub const BASE_SEPOLIA: EvmChainInfo = EvmChainInfo {
    chain_id: "84532",
    name: "Base Sepolia",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    gas_adjustment: 1.3,
    rpc_url: Some("https://sepolia.base.org"),
};

/// Arbitrum One.
pub const ARBITRUM: EvmChainInfo = EvmChainInfo {
    chain_id: "42161",
    name: "Arbitrum One",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    gas_adjustment: 1.3,
    rpc_url: Some("https://arb1.arbitrum.io/rpc"),
};

/// OP Mainnet (Optimism).
pub const OPTIMISM: EvmChainInfo = EvmChainInfo {
    chain_id: "10",
    name: "OP Mainnet",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    gas_adjustment: 1.3,
    rpc_url: Some("https://mainnet.optimism.io"),
};

/// Base.
pub const BASE: EvmChainInfo = EvmChainInfo {
    chain_id: "8453",
    name: "Base",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    gas_adjustment: 1.3,
    rpc_url: Some("https://mainnet.base.org"),
};

/// Polygon PoS.
pub const POLYGON: EvmChainInfo = EvmChainInfo {
    chain_id: "137",
    name: "Polygon",
    spec_id: SpecId::CANCUN,
    native_symbol: "POL",
    gas_adjustment: 1.3,
    rpc_url: Some("https://polygon-rpc.com"),
};

/// A generic local chain for fast tests (no real RPC).
pub const LOCAL: EvmChainInfo = EvmChainInfo {
    chain_id: "31337",
    name: "Local",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    gas_adjustment: 1.3,
    rpc_url: None,
};
