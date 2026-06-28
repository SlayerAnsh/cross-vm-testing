//! CosmWasm chain provider for the cross-vm testing suite.
//!
//! Wraps `cw-multi-test` behind the shared [`cross_vm_core::ChainProvider`] trait.
//!
//! ```no_run
//! use cross_vm_cosmwasm::chains::OSMOSIS;
//! use cross_vm_core::ChainProvider;
//!
//! let mut chain = OSMOSIS.mock();   // or: CwMockProvider::new(OSMOSIS)
//! let alice = chain.new_account("alice");
//! assert!(chain.balance(&alice).unwrap() > 0);
//! ```

pub mod chains;
pub mod provider;

pub use chains::CosmosChainInfo;
pub use provider::{CwApp, CwError, CwMockProvider, CwRpcProvider, DEFAULT_FUNDING};

impl CosmosChainInfo {
    /// Sugar for [`CwMockProvider::new`].
    pub fn mock(self) -> CwMockProvider {
        CwMockProvider::new(self)
    }

    /// Sugar for [`CwRpcProvider::new`].
    pub fn rpc(self) -> CwRpcProvider {
        CwRpcProvider::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::chains::{LOCAL, OSMOSIS};
    use cross_vm_core::{ChainProvider, ChainSpec};

    #[test]
    fn predefined_chain_metadata() {
        assert_eq!(OSMOSIS.chain_id(), "osmosis-1");
        assert_eq!(OSMOSIS.native_symbol(), "OSMO");
        assert_eq!(OSMOSIS.bech32_prefix, "osmo");
    }

    #[test]
    fn new_account_is_prefixed_and_funded() {
        let mut chain = OSMOSIS.mock();
        let alice = chain.new_account("alice");
        assert!(alice.as_str().starts_with("osmo1"));
        assert_eq!(chain.balance(&alice).unwrap(), super::DEFAULT_FUNDING);
    }

    #[test]
    fn set_and_read_balance() {
        let mut chain = LOCAL.mock();
        let bob = chain.new_account("bob");
        chain.set_balance(&bob, 42).unwrap();
        assert_eq!(chain.balance(&bob).unwrap(), 42);
    }

    #[test]
    fn blocks_advance() {
        let mut chain = LOCAL.mock();
        let h0 = chain.block_height();
        chain.advance_blocks(3);
        assert_eq!(chain.block_height(), h0 + 3);
    }

    #[test]
    fn rpc_stub_is_unimplemented() {
        let chain = OSMOSIS.rpc();
        let addr = cosmwasm_std::Addr::unchecked("osmo1xyz");
        assert!(chain.balance(&addr).is_err());
    }
}
