//! Construction sugar turning a [`TronChainInfo`] into a provider.

use std::rc::Rc;

use cross_vm_core::WalletFactory;

use crate::chains::TronChainInfo;
use crate::provider::{TronMockProvider, TronRpcProvider};

impl TronChainInfo {
    /// Sugar for [`TronMockProvider::new`].
    pub fn mock(self, wallets: Rc<WalletFactory>) -> TronMockProvider {
        TronMockProvider::new(self, wallets)
    }

    /// Sugar for [`TronRpcProvider::new`].
    pub fn rpc(self, wallets: Rc<WalletFactory>) -> TronRpcProvider {
        TronRpcProvider::new(self, wallets)
    }
}
