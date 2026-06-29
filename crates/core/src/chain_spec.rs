//! Metadata describing a predefined chain.

use crate::chain_kind::ChainKind;

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
