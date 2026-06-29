//! Solana chain providers: the in-process mock and the live-RPC stub.

mod mock;
mod rpc;

pub use mock::{SvmMockProvider, DEFAULT_FUNDING_LAMPORTS};
pub use rpc::SvmRpcProvider;
