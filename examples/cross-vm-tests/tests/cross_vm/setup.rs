//! Cross-VM integration: one MultiChainEnv holding a CosmWasm, an EVM, and a Solana chain.
//! Seed state during setup (accounts + native funding), `start()`, then drive an
//! execution on Solana and assert balances across all three chains.

use std::rc::Rc;

use cross_vm_framework::prelude::*;
use solana_system_interface::instruction::transfer;

use crate::support::test_wallets;

#[tokio::test]
async fn three_chains_in_one_env() {
    let wallets = test_wallets();
    let mut env = MultiChainEnv::new("cross-vm", wallets.clone());

    env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets.clone())));
    env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets.clone())));
    env.inject("sol", AnyChain::from(SOLANA_DEVNET.mock(wallets)));

    let cw_alice = env.cosmwasm("osmosis").unwrap().new_account("alice").await;
    let evm_alice = env.evm("eth").unwrap().new_account("alice").await;

    env.fund("osmosis", &cw_alice, "uosmo", 5_000_000u128)
        .unwrap();
    env.fund(
        "eth",
        &evm_alice,
        "eth",
        cross_vm_solidity::U256::from(7u64),
    )
    .unwrap();

    let mut env = env.start().await.expect("start should succeed");

    assert!(
        env.cosmwasm("osmosis")
            .unwrap()
            .balance(&cw_alice)
            .await
            .unwrap()
            >= 5_000_000
    );

    assert!(
        env.evm("eth").unwrap().balance(&evm_alice).await.unwrap()
            >= cross_vm_solidity::U256::from(7u64)
    );

    let sol_alice = env
        .solana("sol")
        .unwrap()
        .wallet_address(TEST_WALLETS.alice)
        .await
        .unwrap();
    let sol_bob = env
        .solana("sol")
        .unwrap()
        .wallet_address(TEST_WALLETS.bob)
        .await
        .unwrap();
    env.solana("sol")
        .unwrap()
        .set_balance(&sol_alice, "SOL", 2_000_000_000u64)
        .await
        .unwrap();

    let bob_start = env.solana("sol").unwrap().balance(&sol_bob).await.unwrap();
    let amount = 500_000_000u64;
    let ix = transfer(&sol_alice, &sol_bob, amount);
    env.solana("sol")
        .unwrap()
        .send_transaction(vec![ix], TEST_WALLETS.alice, SvmComputeBudget::Estimated)
        .await
        .expect("solana transfer");
    assert_eq!(
        env.solana("sol").unwrap().balance(&sol_bob).await.unwrap(),
        bob_start + amount
    );
}

#[tokio::test]
async fn native_funding_is_minted_on_mock() {
    let wallets = Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"));
    let mut env = MultiChainEnv::new("shortfall", wallets.clone());
    env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets)));
    let alice = env.cosmwasm("osmosis").unwrap().new_account("alice").await;
    env.fund("osmosis", &alice, "uosmo", 1_000u128).unwrap();
    let mut env = env.start().await.expect("native funding should succeed");
    assert!(
        env.cosmwasm("osmosis")
            .unwrap()
            .balance(&alice)
            .await
            .unwrap()
            >= 1_000
    );
}
