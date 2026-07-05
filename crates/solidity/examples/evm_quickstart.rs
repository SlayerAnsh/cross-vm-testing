//! Quickstart: spin up an EVM mock chain and inspect accounts/blocks.
//!
//! Run with: `cargo run -p cross-vm-solidity --example evm_quickstart`

use std::rc::Rc;

use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};
use cross_vm_solidity::chains::ETHEREUM;
use revm::primitives::U256;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let mut chain = ETHEREUM.mock(wallets);
    println!(
        "chain: {} (id {})",
        chain.chain_info().name(),
        chain.chain_info().chain_id()
    );

    let alice = chain.new_account("alice").await;
    println!("alice: {alice}");
    println!("balance (wei): {}", chain.balance(&alice).await.unwrap());

    chain
        .set_balance(&alice, "ETH", U256::from(1_000u64))
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
