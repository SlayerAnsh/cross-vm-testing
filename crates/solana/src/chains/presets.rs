//! Predefined Solana clusters.
//!
//! Use them as `SvmMockProvider::new(SOLANA_DEVNET)` or `SOLANA_DEVNET.mock()`.

use super::commitment::Commitment;
use super::info::SolanaChainInfo;

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
