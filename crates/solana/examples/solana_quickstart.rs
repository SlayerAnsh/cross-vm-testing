//! Quickstart: spin up a Solana mock chain, fund accounts, and transfer SOL.
//!
//! Run with: `cargo run -p cross-vm-solana --example solana_quickstart`

use cross_vm_core::{ChainProvider, ChainSpec};
use cross_vm_solana::chains::SOLANA_DEVNET;
use solana_system_interface::instruction::transfer;

fn main() {
    // Two equivalent ways to construct: `SOLANA_DEVNET.mock()` or `SvmMockProvider::new(..)`.
    let mut chain = SOLANA_DEVNET.mock();
    println!("cluster: {} ({})", chain.chain_info().name(), chain.chain_info().chain_id());

    let alice = chain.new_account("alice");
    let bob = chain.new_account("bob");
    println!("alice: {alice}  balance: {}", chain.balance(&alice).unwrap());
    println!("bob:   {bob}  balance: {}", chain.balance(&bob).unwrap());

    let ix = transfer(&alice, &bob, 1_000_000_000); // 1 SOL
    chain
        .execute(&solana_system_interface::program::ID, vec![ix], &alice)
        .expect("transfer");

    println!("after transfer:");
    println!("alice balance: {}", chain.balance(&alice).unwrap());
    println!("bob balance:   {}", chain.balance(&bob).unwrap());
}
