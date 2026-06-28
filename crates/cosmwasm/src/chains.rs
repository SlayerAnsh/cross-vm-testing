//! Predefined CosmWasm chains.
//!
//! Each constant is a [`CosmosChainInfo`] holding the metadata needed to spin up a
//! provider quickly. Use them as `CwMockProvider::new(OSMOSIS)` or `OSMOSIS.mock()`.

use cross_vm_core::{ChainKind, ChainSpec};

/// Metadata describing a CosmWasm chain.
///
/// All fields are `&'static str` so the constants below are usable in `const` context
/// and the bech32 prefix satisfies `cw-multi-test`'s `&'static str` requirement.
#[derive(Debug, Clone, Copy)]
pub struct CosmosChainInfo {
    /// Canonical chain id, e.g. `"osmosis-1"`.
    pub chain_id: &'static str,
    /// Human-readable name, e.g. `"Osmosis"`.
    pub name: &'static str,
    /// Bech32 address prefix, e.g. `"osmo"`.
    pub bech32_prefix: &'static str,
    /// Native fee denom, e.g. `"uosmo"`.
    pub native_denom: &'static str,
    /// Native token symbol, e.g. `"OSMO"`.
    pub native_symbol: &'static str,
    /// Indicative gas price in `native_denom` per gas unit (metadata only; the mock
    /// VM does not charge gas).
    pub gas_price: f64,
    /// Default public RPC endpoint, if known.
    pub rpc_url: Option<&'static str>,
}

impl ChainSpec for CosmosChainInfo {
    fn chain_id(&self) -> &str {
        self.chain_id
    }
    fn name(&self) -> &str {
        self.name
    }
    fn native_symbol(&self) -> &str {
        self.native_symbol
    }
    fn rpc_url(&self) -> Option<&str> {
        self.rpc_url
    }
    fn kind(&self) -> ChainKind {
        ChainKind::CosmWasm
    }
}

/// Osmosis mainnet.
pub const OSMOSIS: CosmosChainInfo = CosmosChainInfo {
    chain_id: "osmosis-1",
    name: "Osmosis",
    bech32_prefix: "osmo",
    native_denom: "uosmo",
    native_symbol: "OSMO",
    gas_price: 0.025,
    rpc_url: Some("https://rpc.osmosis.zone:443"),
};

/// Juno mainnet.
pub const JUNO: CosmosChainInfo = CosmosChainInfo {
    chain_id: "juno-1",
    name: "Juno",
    bech32_prefix: "juno",
    native_denom: "ujuno",
    native_symbol: "JUNO",
    gas_price: 0.075,
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
    rpc_url: None,
};
