//! EVM chain providers: the in-process mock and the live-RPC stub.

mod address;
mod mock;
mod rpc;

pub use mock::{EvmExecution, EvmInner, EvmMockProvider, DEFAULT_FUNDING_WEI};
pub use rpc::EvmRpcProvider;
