//! EVM chain providers: the in-process mock and the live-RPC stub, plus the shared execution types.

mod address;
mod execution;
mod mock;
mod rpc;

pub use execution::{EvmDeploy, EvmExecution, EvmGas};
pub use mock::{EvmInner, EvmMockProvider, DEFAULT_FUNDING_WEI};
pub use rpc::EvmRpcProvider;
