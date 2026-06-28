//! Quickstart: spin up an EVM mock chain and inspect accounts/blocks.
//!
//! Run with: `cargo run -p cross-vm-solidity --example evm_quickstart`

use cross_vm_core::{ChainProvider, ChainSpec};
use cross_vm_solidity::chains::ETHEREUM;
use revm::primitives::U256;

fn main() {
    // Two equivalent ways to construct: `ETHEREUM.mock()` or `EvmMockProvider::new(ETHEREUM)`.
    let mut chain = ETHEREUM.mock();
    println!("chain: {} (id {})", chain.chain_info().name(), chain.chain_info().chain_id());

    let alice = chain.new_account("alice");
    println!("alice: {alice}");
    println!("balance (wei): {}", chain.balance(&alice).unwrap());

    chain.set_balance(&alice, U256::from(1_000u64)).unwrap();
    println!("after set_balance: {}", chain.balance(&alice).unwrap());

    let h = chain.block_height();
    chain.advance_blocks(10);
    println!("block height: {h} -> {}", chain.block_height());
}
