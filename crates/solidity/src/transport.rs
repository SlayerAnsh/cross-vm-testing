//! Pluggable JSON-RPC transport for the live-RPC EVM provider.
//!
//! Alloy already carries the transport seam: an [`RpcClient`] is the thing a provider sends
//! requests through, and `ProviderBuilder::connect_client` builds a concrete provider around one
//! (so the inherent `fill` that `sign_transaction` relies on survives). [`EvmTransport`] is a thin
//! factory over that seam: a caller hands the provider any client (a custom HTTP stack, a websocket
//! later, an instrumenting wrapper, or a `RpcClient::mocked` asserter in tests) and the provider
//! keeps building through `connect_client`. The shipped [`HttpTransport`] is the default and
//! reproduces the exact behavior the provider had before the transport existed.

use std::future::Future;
use std::pin::Pin;

use alloy::rpc::client::{ClientBuilder, RpcClient};

use crate::error::EvmError;

/// A boxed, non-`Send` future: matches the repo's `Rc`/current-thread world and keeps
/// [`EvmTransport`] object-safe behind `Rc<dyn EvmTransport>`.
pub type EvmClientFuture<'a> = Pin<Box<dyn Future<Output = Result<RpcClient, EvmError>> + 'a>>;

/// A factory over alloy's own transport seam ([`RpcClient`]).
///
/// Async because a future websocket transport connects on first use; the shipped
/// [`HttpTransport`] resolves immediately.
pub trait EvmTransport {
    /// Build the [`RpcClient`] the provider sends requests through.
    fn rpc_client(&self) -> EvmClientFuture<'_>;
}

/// The default transport: alloy's HTTP JSON-RPC client for the chain's endpoint.
///
/// Cheap to build (just a reqwest client, no connection), so the provider builds one per request.
/// Keeps `chain_id` only for the no-url error message, preserving the text the provider surfaced
/// before the transport existed.
pub struct HttpTransport {
    url: Option<String>,
    chain_id: String,
}

impl HttpTransport {
    /// Bind the transport to a chain endpoint. `url` is `None` (or empty) for a chain declared
    /// without an `rpc_url`; the missing-url error then surfaces lazily at the first call, so
    /// `SEPOLIA.rpc(wallets)` sugar stays infallible.
    pub fn new(url: Option<String>, chain_id: String) -> Self {
        Self { url, chain_id }
    }

    /// Build the client now; the trait impl wraps this in an immediately-ready future.
    fn build(&self) -> Result<RpcClient, EvmError> {
        let url = self.url.as_deref().unwrap_or("");
        if url.is_empty() {
            return Err(EvmError::Rpc(format!(
                "chain '{}' has no rpc_url; use a chain preset with an endpoint",
                self.chain_id
            )));
        }
        let url = url
            .parse()
            .map_err(|e| EvmError::Rpc(format!("invalid rpc url: {e}")))?;
        Ok(ClientBuilder::default().http(url))
    }
}

impl EvmTransport for HttpTransport {
    fn rpc_client(&self) -> EvmClientFuture<'_> {
        Box::pin(async move { self.build() })
    }
}
