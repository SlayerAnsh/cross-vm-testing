//! Quickstart: spin up a CosmWasm mock chain and inspect accounts/blocks.
//!
//! Run with: `cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart`

use std::rc::Rc;

use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};
use cross_vm_cosmwasm::chains::OSMOSIS;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let mut chain = OSMOSIS.mock(wallets);
    println!(
        "chain: {} ({})",
        chain.chain_info().name(),
        chain.chain_info().chain_id()
    );

    let alice = chain.new_account("alice").await;
    println!("alice: {alice}");
    println!(
        "balance: {} {}",
        chain.balance(&alice).await.unwrap(),
        OSMOSIS.native_denom
    );

    chain
        .set_balance(&alice, OSMOSIS.native_denom, 5_000_000)
        .await
        .unwrap();
    println!(
        "after set_balance: {}",
        chain.balance(&alice).await.unwrap()
    );

    let h = chain.block_height().await;
    chain.advance_blocks(10, BlockTime::Increment(1)).await;
    println!("block height: {h} -> {}", chain.block_height().await);
}
