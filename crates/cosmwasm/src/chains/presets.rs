//! Predefined CosmWasm chains.
//!
//! Use them as `CwMockProvider::new(OSMOSIS, wallets)` or `OSMOSIS.mock(wallets)`, where
//! `wallets` is the shared `Rc<WalletFactory>`.

use super::info::CosmosChainInfo;

/// The [`CosmosChainInfo::gas_adjustment`] every preset carries: 30% headroom over the node's
/// simulated figure. Same value the config layer defaults `gas_adjustment` to, so a chain built
/// from a preset and one built from a config file that omits the field behave identically.
const DEFAULT_GAS_ADJUSTMENT: f64 = 1.3;

/// Osmosis mainnet.
pub const OSMOSIS: CosmosChainInfo = CosmosChainInfo {
    chain_id: "osmosis-1",
    name: "Osmosis",
    bech32_prefix: "osmo",
    native_denom: "uosmo",
    native_symbol: "OSMO",
    gas_price: 0.025,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
    rpc_url: Some("https://rpc.osmosis.zone:443"),
};

/// Osmosis testnet (`osmo-test-5`).
pub const OSMOSIS_TESTNET: CosmosChainInfo = CosmosChainInfo {
    chain_id: "osmo-test-5",
    name: "Osmosis Testnet",
    bech32_prefix: "osmo",
    native_denom: "uosmo",
    native_symbol: "OSMO",
    gas_price: 0.025,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
    rpc_url: Some("https://rpc.testnet.osmosis.zone:443"),
};

/// Juno mainnet.
pub const JUNO: CosmosChainInfo = CosmosChainInfo {
    chain_id: "juno-1",
    name: "Juno",
    bech32_prefix: "juno",
    native_denom: "ujuno",
    native_symbol: "JUNO",
    gas_price: 0.075,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
    rpc_url: Some("https://rpc-juno.itastakers.com:443"),
};

/// Neutron mainnet.
pub const NEUTRON: CosmosChainInfo = CosmosChainInfo {
    chain_id: "neutron-1",
    name: "Neutron",
    bech32_prefix: "neutron",
    native_denom: "untrn",
    native_symbol: "NTRN",
    gas_price: 0.0053,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
    rpc_url: Some("https://rpc-kralum.neutron-1.neutron.org:443"),
};

/// Cosmos Hub mainnet.
pub const COSMOS_HUB: CosmosChainInfo = CosmosChainInfo {
    chain_id: "cosmoshub-4",
    name: "Cosmos Hub",
    bech32_prefix: "cosmos",
    native_denom: "uatom",
    native_symbol: "ATOM",
    gas_price: 0.025,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
    rpc_url: Some("https://cosmos-rpc.publicnode.com:443"),
};

/// A generic local chain for fast tests (no real RPC).
pub const LOCAL: CosmosChainInfo = CosmosChainInfo {
    chain_id: "cosmos-testing",
    name: "Local",
    bech32_prefix: "cosmwasm",
    native_denom: "ustake",
    native_symbol: "STAKE",
    gas_price: 0.0,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
    rpc_url: None,
};
