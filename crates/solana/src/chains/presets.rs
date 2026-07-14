//! Predefined Solana clusters.
//!
//! Use them as `SvmMockProvider::new(SOLANA_DEVNET, wallets)` or `SOLANA_DEVNET.mock(wallets)`,
//! where `wallets` is the shared `Rc<WalletFactory>`.

use super::commitment::Commitment;
use super::info::SolanaChainInfo;

/// Headroom every preset leaves over a simulated compute-unit consumption. Matches the
/// framework's config default, so a preset and a chain declared in TOML behave alike.
const DEFAULT_GAS_ADJUSTMENT: f64 = 1.3;

/// Solana mainnet-beta.
pub const SOLANA_MAINNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "mainnet-beta",
    name: "Solana Mainnet",
    native_symbol: "SOL",
    rpc_url: Some("https://api.mainnet-beta.solana.com"),
    ws_url: Some("wss://api.mainnet-beta.solana.com"),
    commitment: Commitment::Finalized,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};

/// Solana devnet.
pub const SOLANA_DEVNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "devnet",
    name: "Solana Devnet",
    native_symbol: "SOL",
    rpc_url: Some("https://api.devnet.solana.com"),
    ws_url: Some("wss://api.devnet.solana.com"),
    commitment: Commitment::Confirmed,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};

/// Solana testnet.
pub const SOLANA_TESTNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "testnet",
    name: "Solana Testnet",
    native_symbol: "SOL",
    rpc_url: Some("https://api.testnet.solana.com"),
    ws_url: Some("wss://api.testnet.solana.com"),
    commitment: Commitment::Confirmed,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};

/// A local validator / in-process chain (no real RPC).
pub const SOLANA_LOCALNET: SolanaChainInfo = SolanaChainInfo {
    chain_id: "localnet",
    name: "Solana Localnet",
    native_symbol: "SOL",
    rpc_url: Some("http://localhost:8899"),
    ws_url: Some("ws://localhost:8900"),
    commitment: Commitment::Confirmed,
    gas_adjustment: DEFAULT_GAS_ADJUSTMENT,
};
