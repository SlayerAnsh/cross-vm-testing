//! Predefined CosmWasm chains and their metadata type.

mod info;
#[cfg(feature = "presets")]
mod presets;
mod sugar;

pub use info::CosmosChainInfo;
#[cfg(feature = "presets")]
pub use presets::{COSMOS_HUB, JUNO, LOCAL, NEUTRON, OSMOSIS, OSMOSIS_TESTNET};
