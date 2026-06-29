//! Predefined EVM chains and their metadata type.

mod info;
mod presets;
mod sugar;

pub use info::EvmChainInfo;
pub use presets::{ARBITRUM, BASE, BASE_SEPOLIA, ETHEREUM, LOCAL, OPTIMISM, POLYGON, SEPOLIA};
