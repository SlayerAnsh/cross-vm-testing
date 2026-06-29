//! End-to-end check that `ChainKind::Tron` dispatches through the `#[cross_vm_contract]` macro
//! and the framework wiring onto a Tron mock chain. Self-contained: no external contract
//! artifacts, so it runs in CI without the forge/sbf build step.

use std::rc::Rc;

use cross_vm_framework::prelude::*;

/// A trivial cross-VM contract whose per-VM hooks return a VM-distinct sentinel, so the value
/// proves which arm the generated dispatcher took.
#[cross_vm_contract(Sentinel)]
pub trait SentinelSpec {
    /// Returns a per-VM sentinel.
    async fn which(&self) -> u64;
}

impl Sentinel {
    async fn cw_which(&self) -> Result<u64, CrossVmError> {
        Ok(1)
    }
    async fn evm_which(&self) -> Result<u64, CrossVmError> {
        Ok(2)
    }
    async fn svm_which(&self) -> Result<u64, CrossVmError> {
        Ok(3)
    }
    async fn tron_which(&self) -> Result<u64, CrossVmError> {
        Ok(4)
    }
}

#[tokio::test]
async fn dispatches_to_tron_hook_on_a_tron_chain() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
    let chain = AnyChain::from(TRON_LOCAL.mock(wallets));
    assert_eq!(chain.kind(), ChainKind::Tron);
    let contract = Sentinel::new(chain);
    // The generated dispatcher matches the chain's kind; Tron must reach `tron_which`.
    assert_eq!(contract.which().await.unwrap(), 4);
}

#[tokio::test]
async fn tron_mock_funds_and_advances_deterministically() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
    let mut chain = AnyChain::from(TRON_LOCAL.mock(wallets));
    let acct = chain.new_account("alice").await;
    assert_eq!(acct.kind(), ChainKind::Tron);
    let h0 = chain.block_height().await;
    chain.advance_blocks(5).await;
    assert_eq!(chain.block_height().await, h0 + 5);
}
