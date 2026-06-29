//! Quickstart: drive multiple VMs through one MultiChainEnv.
//!
//! Run with: `cargo run -p cross-vm-framework --example env_quickstart`

use std::rc::Rc;

use cross_vm_framework::prelude::*;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallets = Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"));
    let mut env = MultiChainEnv::new("demo", wallets.clone());

    env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets.clone())));
    env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets.clone())));

    let cw_alice = env.cosmwasm("osmosis").unwrap().new_account("alice").await;
    let evm_alice = env.evm("eth").unwrap().new_account("alice").await;

    env.fund("osmosis", &cw_alice, "uosmo", 5_000_000u128)
        .unwrap();
    env.fund(
        "eth",
        &evm_alice,
        "eth",
        cross_vm_solidity::U256::from(10u64),
    )
    .unwrap();

    let mut env = env.start().await.expect("setup");

    println!("env: {} ({} chains)", env.label(), env.len());
    println!(
        "osmosis alice uosmo: {}",
        env.cosmwasm("osmosis")
            .unwrap()
            .balance(&cw_alice)
            .await
            .unwrap()
    );
    println!(
        "eth alice wei:       {}",
        env.evm("eth").unwrap().balance(&evm_alice).await.unwrap()
    );
}
