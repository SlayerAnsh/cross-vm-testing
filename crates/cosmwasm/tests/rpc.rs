//! Live read-only RPC tests against Osmosis testnet (`osmo-test-5`).
//!
//! These hit a real node, so they are `#[ignore]` by default and skipped by the offline
//! suite. Run them explicitly with network access:
//!
//! ```text
//! cargo test -p cross-vm-cosmwasm --test rpc -- --ignored
//! ```

use std::rc::Rc;

use cross_vm_core::WalletFactory;
use cross_vm_cosmwasm::chains::OSMOSIS_TESTNET;

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[tokio::test]
#[ignore = "requires network access to osmo-test-5"]
async fn live_block_height_is_nonzero() {
    let chain = OSMOSIS_TESTNET.rpc(empty_wallets());
    let height = chain
        .try_block_height()
        .await
        .expect("query osmo-test-5 block height");
    assert!(height > 0, "expected a nonzero block height, got {height}");
}
