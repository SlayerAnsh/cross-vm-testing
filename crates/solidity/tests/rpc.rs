//! Live read-only RPC tests against Ethereum Sepolia.
//!
//! These hit a real node, so they are `#[ignore]` by default and skipped by the offline
//! suite. Run them explicitly with network access:
//!
//! ```text
//! cargo test -p cross-vm-solidity --test rpc -- --ignored
//! ```

use std::rc::Rc;

use cross_vm_core::WalletFactory;
use cross_vm_solidity::chains::SEPOLIA;

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[tokio::test]
#[ignore = "requires network access to Sepolia"]
async fn live_block_number_is_nonzero() {
    let chain = SEPOLIA.rpc(empty_wallets());
    let height = chain
        .try_block_height()
        .await
        .expect("query Sepolia block number");
    assert!(height > 0, "expected a nonzero block number, got {height}");
}
