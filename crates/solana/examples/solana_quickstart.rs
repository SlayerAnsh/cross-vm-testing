//! Quickstart: spin up a Solana mock chain, derive wallets, fund, and transfer SOL.
//!
//! Run with: `cargo run -p cross-vm-solana --example solana_quickstart`

use std::rc::Rc;

use cross_vm_core::{ChainProvider, ChainSpec, WalletFactory};
use cross_vm_macros::define_wallet_roster;
use cross_vm_solana::chains::SOLANA_DEVNET;
use cross_vm_solana::{SvmChain, SvmComputeBudget};
use solana_system_interface::instruction::transfer;

define_wallet_roster! {
    pub const DEMO_WALLETS: DemoWallets = {
        alice: auto @ 0,
        bob: auto @ 1,
    };
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // `auto` wallets generate fresh mnemonics so the example is self-contained; on a real cluster
    // use `env_mnemonic("VAR")` and load secrets from `.env` first.
    let wallets = Rc::new(WalletFactory::from_roster(DemoWallets::SPECS).unwrap());

    let mut chain: SvmChain = SOLANA_DEVNET.mock(wallets).into();
    println!(
        "cluster: {} ({})",
        chain.chain_info().name(),
        chain.chain_info().chain_id()
    );

    let alice = chain.wallet_address(DEMO_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(DEMO_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, "SOL", 100_000_000_000)
        .await
        .unwrap(); // 100 SOL
    println!(
        "alice: {alice}  balance: {}",
        chain.balance(&alice).await.unwrap()
    );
    println!(
        "bob:   {bob}  balance: {}",
        chain.balance(&bob).await.unwrap()
    );

    // The budget caps the compute units the transaction may burn (it does not cap the fee, which
    // is per signature): `Estimated` simulates the transaction and caps it at what it consumed,
    // scaled by the cluster's `gas_adjustment`.
    let ix = transfer(&alice, &bob, 1_000_000_000); // 1 SOL
    chain
        .send_transaction(vec![ix], DEMO_WALLETS.alice, SvmComputeBudget::Estimated)
        .await
        .expect("transfer");

    println!("after transfer:");
    println!("alice balance: {}", chain.balance(&alice).await.unwrap());
    println!("bob balance:   {}", chain.balance(&bob).await.unwrap());
}
