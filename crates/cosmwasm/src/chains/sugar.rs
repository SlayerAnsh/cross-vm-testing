//! Construction sugar turning a [`CosmosChainInfo`] into a provider.

use std::rc::Rc;

use cross_vm_core::WalletFactory;

use crate::chains::CosmosChainInfo;
use crate::provider::{CwMockProvider, CwRpcProvider};
use crate::transport::{BatchConfig, BatchHttpTransport, CosmosTransport};

impl CosmosChainInfo {
    /// Sugar for [`CwMockProvider::new`].
    pub fn mock(self, wallets: Rc<WalletFactory>) -> CwMockProvider {
        CwMockProvider::new(self, wallets)
    }

    /// Sugar for [`CwRpcProvider::new`] (default [`HttpTransport`](crate::transport::HttpTransport)).
    pub fn rpc(self, wallets: Rc<WalletFactory>) -> CwRpcProvider {
        CwRpcProvider::new(self, wallets)
    }

    /// Sugar for [`CwRpcProvider::new_with_transport`]: attach a caller-supplied
    /// [`CosmosTransport`] (custom HTTP stack, instrumenting wrapper, or a mock).
    pub fn rpc_with(
        self,
        wallets: Rc<WalletFactory>,
        transport: Rc<dyn CosmosTransport>,
    ) -> CwRpcProvider {
        CwRpcProvider::new_with_transport(self, wallets, transport)
    }

    /// Sugar for a [`CwRpcProvider`] riding a [`BatchHttpTransport`], which merges concurrent
    /// JSON-RPC calls into CometBFT batch requests per `cfg`. Some public RPC gateways reject
    /// batch array bodies, so batching is opt in, never the default.
    pub fn rpc_batched(self, wallets: Rc<WalletFactory>, cfg: BatchConfig) -> CwRpcProvider {
        let transport = Rc::new(BatchHttpTransport::new(self.rpc_url, self.chain_id, cfg));
        CwRpcProvider::new_with_transport(self, wallets, transport)
    }
}
