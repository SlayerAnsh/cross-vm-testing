//! Construction sugar turning a [`SolanaChainInfo`] into a provider.

use std::rc::Rc;

use cross_vm_core::WalletFactory;

use crate::chains::SolanaChainInfo;
use crate::provider::{SvmMockProvider, SvmRpcProvider};

impl SolanaChainInfo {
    /// Sugar for [`SvmMockProvider::new`].
    pub fn mock(self, wallets: Rc<WalletFactory>) -> SvmMockProvider {
        SvmMockProvider::new(self, wallets)
    }

    /// Sugar for [`SvmRpcProvider::new`].
    pub fn rpc(self, wallets: Rc<WalletFactory>) -> SvmRpcProvider {
        SvmRpcProvider::new(self, wallets)
    }
}
