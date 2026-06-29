//! Construction sugar turning a [`CosmosChainInfo`] into a provider.

use std::rc::Rc;

use cross_vm_core::WalletFactory;

use crate::chains::CosmosChainInfo;
use crate::provider::{CwMockProvider, CwRpcProvider};

impl CosmosChainInfo {
    /// Sugar for [`CwMockProvider::new`].
    pub fn mock(self, wallets: Rc<WalletFactory>) -> CwMockProvider {
        CwMockProvider::new(self, wallets)
    }

    /// Sugar for [`CwRpcProvider::new`].
    pub fn rpc(self, wallets: Rc<WalletFactory>) -> CwRpcProvider {
        CwRpcProvider::new(self, wallets)
    }
}
