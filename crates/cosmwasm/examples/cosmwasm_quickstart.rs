//! Quickstart: spin up a CosmWasm mock chain and inspect accounts/blocks.
//!
//! Run with: `cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart`

use cross_vm_core::{ChainProvider, ChainSpec};
use cross_vm_cosmwasm::chains::OSMOSIS;

fn main() {
    // Two equivalent ways to construct: `OSMOSIS.mock()` or `CwMockProvider::new(OSMOSIS)`.
    let mut chain = OSMOSIS.mock();
    println!("chain: {} ({})", chain.chain_info().name(), chain.chain_info().chain_id());

    let alice = chain.new_account("alice");
    println!("alice: {alice}");
    println!("balance: {} {}", chain.balance(&alice).unwrap(), OSMOSIS.native_denom);

    chain.set_balance(&alice, 5_000_000).unwrap();
    println!("after set_balance: {}", chain.balance(&alice).unwrap());

    let h = chain.block_height();
    chain.advance_blocks(10);
    println!("block height: {h} -> {}", chain.block_height());
}
