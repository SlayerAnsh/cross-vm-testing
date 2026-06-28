//! EVM/Solidity chain provider for the cross-vm testing suite.
//!
//! Wraps `revm` behind the shared [`cross_vm_core::ChainProvider`] trait.
//!
//! ```no_run
//! use cross_vm_solidity::chains::ETHEREUM;
//! use cross_vm_core::ChainProvider;
//!
//! let mut chain = ETHEREUM.mock();   // or: EvmMockProvider::new(ETHEREUM)
//! let alice = chain.new_account("alice");
//! assert!(chain.balance(&alice).unwrap() > revm::primitives::U256::ZERO);
//! ```

pub mod chains;
pub mod provider;

pub use chains::EvmChainInfo;
pub use provider::{EvmError, EvmInner, EvmMockProvider, EvmRpcProvider, DEFAULT_FUNDING_WEI};

impl EvmChainInfo {
    /// Sugar for [`EvmMockProvider::new`].
    pub fn mock(self) -> EvmMockProvider {
        EvmMockProvider::new(self)
    }

    /// Sugar for [`EvmRpcProvider::new`].
    pub fn rpc(self) -> EvmRpcProvider {
        EvmRpcProvider::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::chains::{ETHEREUM, LOCAL};
    use cross_vm_core::{ChainProvider, ChainSpec};
    use revm::primitives::U256;

    #[test]
    fn predefined_chain_metadata() {
        assert_eq!(ETHEREUM.chain_id(), "1");
        assert_eq!(ETHEREUM.numeric_id(), 1);
        assert_eq!(ETHEREUM.native_symbol(), "ETH");
    }

    #[test]
    fn new_account_is_funded() {
        let mut chain = ETHEREUM.mock();
        let alice = chain.new_account("alice");
        assert_eq!(
            chain.balance(&alice).unwrap(),
            U256::from(super::DEFAULT_FUNDING_WEI)
        );
    }

    #[test]
    fn set_and_read_balance() {
        let mut chain = LOCAL.mock();
        let bob = chain.new_account("bob");
        chain.set_balance(&bob, U256::from(42u64)).unwrap();
        assert_eq!(chain.balance(&bob).unwrap(), U256::from(42u64));
    }

    #[test]
    fn blocks_advance() {
        let mut chain = LOCAL.mock();
        let h0 = chain.block_height();
        chain.advance_blocks(5);
        assert_eq!(chain.block_height(), h0 + 5);
    }

    #[test]
    fn rpc_stub_is_unimplemented() {
        let chain = ETHEREUM.rpc();
        assert!(chain.balance(&revm::primitives::Address::ZERO).is_err());
    }
}
