//! Predefined CosmWasm chains and their metadata type.

mod info;
mod presets;
mod sugar;

pub use info::CosmosChainInfo;
pub use presets::{COSMOS_HUB, JUNO, LOCAL, NEUTRON, OSMOSIS, OSMOSIS_TESTNET};
