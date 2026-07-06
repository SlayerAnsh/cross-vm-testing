//! Predefined Solana clusters and their metadata types.

mod commitment;
mod info;
#[cfg(feature = "presets")]
mod presets;
mod sugar;

pub use commitment::Commitment;
pub use info::SolanaChainInfo;
#[cfg(feature = "presets")]
pub use presets::{SOLANA_DEVNET, SOLANA_LOCALNET, SOLANA_MAINNET, SOLANA_TESTNET};
