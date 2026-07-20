//! Construction sugar turning an [`EvmChainInfo`] into a provider.

use std::rc::Rc;

use cross_vm_core::WalletFactory;

use crate::chains::EvmChainInfo;
use crate::provider::{EvmMockProvider, EvmRpcProvider};
use crate::transport::EvmTransport;

impl EvmChainInfo {
    /// Sugar for [`EvmMockProvider::new`].
    pub fn mock(self, wallets: Rc<WalletFactory>) -> EvmMockProvider {
        EvmMockProvider::new(self, wallets)
    }

    /// Sugar for [`EvmRpcProvider::new`] (default [`HttpTransport`](crate::transport::HttpTransport)).
    pub fn rpc(self, wallets: Rc<WalletFactory>) -> EvmRpcProvider {
        EvmRpcProvider::new(self, wallets)
    }

    /// Sugar for [`EvmRpcProvider::new_with_transport`]: attach a caller-supplied
    /// [`EvmTransport`] (custom HTTP stack, websocket, instrumenting wrapper, or a mock).
    pub fn rpc_with(
        self,
        wallets: Rc<WalletFactory>,
        transport: Rc<dyn EvmTransport>,
    ) -> EvmRpcProvider {
        EvmRpcProvider::new_with_transport(self, wallets, transport)
    }
}
