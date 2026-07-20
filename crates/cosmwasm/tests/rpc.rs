//! Live read-only RPC tests against Osmosis testnet (`osmo-test-5`).
//!
//! These hit a real node, so they are `#[ignore]` by default and skipped by the offline
//! suite. Run them explicitly with network access:
//!
//! ```text
//! cargo test -p cross-vm-cosmwasm --test rpc -- --ignored
//! ```

use std::rc::Rc;
use std::time::Duration;

use cross_vm_core::WalletFactory;
use cross_vm_cosmwasm::chains::OSMOSIS_TESTNET;
use cross_vm_cosmwasm::BatchConfig;

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

#[tokio::test]
#[ignore = "requires network access to osmo-test-5"]
async fn live_batched_transport_coalesces_concurrent_reads() {
    // A `BatchHttpTransport` with a tick interval wide enough that concurrent calls pile on
    // before the leader's first tick, so they merge into one CometBFT JSON-RPC batch POST. The
    // point is to prove that array-body batching works against a real node: several reads
    // issued at once must all come back correctly routed by JSON-RPC id.
    let chain = OSMOSIS_TESTNET.rpc_batched(
        empty_wallets(),
        BatchConfig {
            interval: Duration::from_millis(20),
            ..BatchConfig::default()
        },
    );

    // Five concurrent status reads: awaited together, they enqueue before the first tick and
    // ride a single batch request. Distinct JSON-RPC ids exercise id-based response routing.
    let (h1, h2, h3, h4, h5) = tokio::join!(
        chain.try_block_height(),
        chain.try_block_height(),
        chain.try_block_height(),
        chain.try_block_height(),
        chain.try_block_height(),
    );

    let heights = [
        h1.expect("batched height 1"),
        h2.expect("batched height 2"),
        h3.expect("batched height 3"),
        h4.expect("batched height 4"),
        h5.expect("batched height 5"),
    ];

    for height in heights {
        assert!(height > 0, "expected a nonzero block height, got {height}");
    }

    // All reads hit the same node within one tick interval, so the tip should barely move; a
    // few blocks of slack covers a boundary crossing without letting a misrouted response pass.
    let min = heights.iter().min().copied().unwrap();
    let max = heights.iter().max().copied().unwrap();
    assert!(
        max - min <= 5,
        "batched heights drifted more than expected: {heights:?}"
    );
}
