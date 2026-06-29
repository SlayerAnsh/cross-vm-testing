//! Metadata describing a CosmWasm chain.

use cross_vm_core::{ChainKind, ChainSpec};

/// Metadata describing a CosmWasm chain.
///
/// The string fields are `&'static str` so the constants are usable in `const` context and the
/// bech32 prefix satisfies `cw-multi-test`'s `&'static str` requirement (`gas_price` is `f64`,
/// `rpc_url` is `Option<&'static str>`).
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
