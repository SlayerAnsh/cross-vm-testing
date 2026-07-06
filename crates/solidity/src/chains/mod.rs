//! Predefined EVM chains and their metadata type.

mod info;
#[cfg(feature = "presets")]
mod presets;
mod sugar;

pub use info::EvmChainInfo;
#[cfg(feature = "presets")]
pub use presets::{ARBITRUM, BASE, BASE_SEPOLIA, ETHEREUM, LOCAL, OPTIMISM, POLYGON, SEPOLIA};
