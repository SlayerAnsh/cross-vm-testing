//! Construction sugar turning an [`EvmChainInfo`] into a provider.

use std::rc::Rc;

use cross_vm_core::WalletFactory;

use crate::chains::EvmChainInfo;
use crate::provider::{EvmMockProvider, EvmRpcProvider};

impl EvmChainInfo {
    /// Sugar for [`EvmMockProvider::new`].
    pub fn mock(self, wallets: Rc<WalletFactory>) -> EvmMockProvider {
        EvmMockProvider::new(self, wallets)
    }

    /// Sugar for [`EvmRpcProvider::new`].
    pub fn rpc(self, wallets: Rc<WalletFactory>) -> EvmRpcProvider {
        EvmRpcProvider::new(self, wallets)
    }
}
