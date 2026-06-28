//! Predefined Solana clusters.
//!
//! Each constant is a [`SolanaChainInfo`]. Use them as `SvmMockProvider::new(SOLANA_DEVNET)`
//! or `SOLANA_DEVNET.mock()`.

use cross_vm_core::{ChainKind, ChainSpec};

/// Commitment level requested from a (future) RPC endpoint. Metadata only for the mock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commitment {
    /// Processed by the node, not yet voted on.
    Processed,
    /// Confirmed by a supermajority.
    Confirmed,
    /// Finalized (rooted).
    Finalized,
}

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

/// Solana mainnet-beta.
pub const SOLANA_MAINNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "mainnet-beta",
    name: "Solana Mainnet",
    native_symbol: "SOL",
    rpc_url: Some("https://api.mainnet-beta.solana.com"),
    ws_url: Some("wss://api.mainnet-beta.solana.com"),
    commitment: Commitment::Finalized,
};

/// Solana devnet.
pub const SOLANA_DEVNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "devnet",
    name: "Solana Devnet",
    native_symbol: "SOL",
    rpc_url: Some("https://api.devnet.solana.com"),
    ws_url: Some("wss://api.devnet.solana.com"),
    commitment: Commitment::Confirmed,
};

/// Solana testnet.
pub const SOLANA_TESTNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "testnet",
    name: "Solana Testnet",
    native_symbol: "SOL",
    rpc_url: Some("https://api.testnet.solana.com"),
    ws_url: Some("wss://api.testnet.solana.com"),
    commitment: Commitment::Confirmed,
};

/// A local validator / in-process chain (no real RPC).
pub const SOLANA_LOCALNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "localnet",
    name: "Solana Localnet",
    native_symbol: "SOL",
    rpc_url: Some("http://localhost:8899"),
    ws_url: Some("ws://localhost:8900"),
    commitment: Commitment::Confirmed,
};
