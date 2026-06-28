//! Solana (SVM) chain provider for the cross-vm testing suite.
//!
//! Wraps `litesvm` behind the shared [`cross_vm_core::ChainProvider`] trait.
//!
//! ```no_run
//! use cross_vm_solana::chains::SOLANA_DEVNET;
//! use cross_vm_core::ChainProvider;
//!
//! let mut chain = SOLANA_DEVNET.mock();   // or: SvmMockProvider::new(SOLANA_DEVNET)
//! let alice = chain.new_account("alice");
//! assert!(chain.balance(&alice).unwrap() > 0);
//! ```

pub mod chains;
pub mod provider;

pub use chains::{Commitment, SolanaChainInfo};
pub use provider::{SvmError, SvmMockProvider, SvmRpcProvider, DEFAULT_FUNDING_LAMPORTS};

impl SolanaChainInfo {
    /// Sugar for [`SvmMockProvider::new`].
    pub fn mock(self) -> SvmMockProvider {
        SvmMockProvider::new(self)
    }

    /// Sugar for [`SvmRpcProvider::new`].
    pub fn rpc(self) -> SvmRpcProvider {
        SvmRpcProvider::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::chains::{SOLANA_DEVNET, SOLANA_LOCALNET};
    use cross_vm_core::{ChainProvider, ChainSpec};

    #[test]
    fn predefined_chain_metadata() {
        assert_eq!(SOLANA_DEVNET.chain_id(), "devnet");
        assert_eq!(SOLANA_DEVNET.native_symbol(), "SOL");
    }

    #[test]
    fn new_account_is_funded() {
        let mut chain = SOLANA_LOCALNET.mock();
        let alice = chain.new_account("alice");
        assert_eq!(
            chain.balance(&alice).unwrap(),
            super::DEFAULT_FUNDING_LAMPORTS
        );
    }

    #[test]
    fn set_and_read_balance() {
        let mut chain = SOLANA_LOCALNET.mock();
        let bob = chain.new_account("bob");
        chain.set_balance(&bob, 12_345).unwrap();
        assert_eq!(chain.balance(&bob).unwrap(), 12_345);
    }

    #[test]
    fn blocks_advance() {
        let mut chain = SOLANA_LOCALNET.mock();
        assert_eq!(chain.block_height(), 0);
        chain.advance_blocks(4);
        assert_eq!(chain.block_height(), 4);
    }

    #[test]
    fn rpc_stub_is_unimplemented() {
        let chain = SOLANA_DEVNET.rpc();
        let addr = solana_address::Address::new_unique();
        assert!(chain.balance(&addr).is_err());
    }
}
