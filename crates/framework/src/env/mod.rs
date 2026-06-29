//! The two-phase cross-VM environment.

mod multi_chain_env;
mod phase;
mod setup;

pub use multi_chain_env::MultiChainEnv;
pub use phase::{Running, Setup};
