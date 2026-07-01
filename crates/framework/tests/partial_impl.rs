//! End-to-end check that a `#[cross_vm_contract]` wrapper compiles with only *some* per-VM hooks
//! implemented. The macro emits a hooks trait whose methods default to `unimplemented!()`, so a
//! contract that targets a subset of VMs need not write the rest; a skipped VM panics only if that
//! chain is actually dispatched.

use std::rc::Rc;

use cross_vm_framework::prelude::*;

/// A wrapper that implements CosmWasm and EVM only. `svm_which` / `tron_which` are intentionally
/// left to the macro's `unimplemented!()` defaults. That this crate compiles at all is the point:
/// without the default-hook trait it would fail with `no method named svm_which`.
#[cross_vm_contract(Partial)]
pub trait PartialSpec {
    /// Returns a per-VM sentinel.
    async fn which(&self) -> u64;
}

impl Partial {
    async fn cw_which(&self) -> Result<u64, CrossVmError> {
        Ok(1)
    }
    async fn evm_which(&self) -> Result<u64, CrossVmError> {
        Ok(2)
    }
    // svm_which and tron_which deliberately omitted; they fall through to the defaults.
}

#[tokio::test]
async fn implemented_vm_dispatches_normally() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
    let chain = AnyChain::from(OSMOSIS.mock(wallets));
    assert_eq!(chain.kind(), ChainKind::CosmWasm);
    let contract = Partial::new(chain);
    assert_eq!(contract.which().await.unwrap(), 1);
}

#[tokio::test]
#[should_panic(expected = "tron_which is not implemented")]
async fn skipped_vm_panics_when_dispatched() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
    let chain = AnyChain::from(TRON_LOCAL.mock(wallets));
    assert_eq!(chain.kind(), ChainKind::Tron);
    let contract = Partial::new(chain);
    // Dispatching a Tron chain reaches the unimplemented default hook.
    let _ = contract.which().await;
}
