//! Metadata describing a Solana cluster.

use cross_vm_core::{ChainKind, ChainSpec};

use super::commitment::Commitment;

/// Metadata describing a Solana cluster.
#[derive(Debug, Clone, Copy)]
pub struct SolanaChainInfo {
    /// Cluster identifier, e.g. `"mainnet-beta"`.
    pub chain_id: &'static str,
    /// Human-readable name, e.g. `"Solana Mainnet"`.
    pub name: &'static str,
    /// Native token symbol (always `"SOL"`).
    pub native_symbol: &'static str,
    /// Default RPC endpoint.
    pub rpc_url: Option<&'static str>,
    /// Default WebSocket endpoint.
    pub ws_url: Option<&'static str>,
    /// Default commitment level.
    pub commitment: Commitment,
}

impl ChainSpec for SolanaChainInfo {
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
        ChainKind::Svm
    }
}
