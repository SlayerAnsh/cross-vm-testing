//! Predefined Solana clusters and their metadata types.

mod commitment;
mod info;
mod presets;
mod sugar;

pub use commitment::Commitment;
pub use info::SolanaChainInfo;
pub use presets::{SOLANA_DEVNET, SOLANA_LOCALNET, SOLANA_MAINNET, SOLANA_TESTNET};
