//! Integration test: fund -> transfer -> balance through the Solana provider, using
//! [`SvmChain::transfer_funds`] (a native SOL transfer) and factory-derived wallets.

use std::rc::Rc;

use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_macros::define_wallet_roster;
use cross_vm_solana::chains::SOLANA_LOCALNET;
use cross_vm_solana::{SvmChain, SvmError};

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
        bob: auto @ 1,
    };
}

fn test_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

#[tokio::test]
async fn fund_transfer_balance() {
    let wallets = test_wallets();
    let mut chain: SvmChain = SOLANA_LOCALNET.mock(wallets).into();

    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, "SOL", 100_000_000_000)
        .await
        .unwrap(); // 100 SOL

    let alice_start = chain.balance(&alice).await.unwrap();
    let bob_start = chain.balance(&bob).await.unwrap();
    assert!(alice_start > 0);

    let amount = 1_000_000_000; // 1 SOL
    let signature = chain
        .transfer_funds(&bob, "SOL", amount, TEST_WALLETS.alice)
        .await
        .expect("transfer");

    assert!(!signature.is_empty(), "expected a transaction signature");
    assert_eq!(chain.balance(&bob).await.unwrap(), bob_start + amount);
    assert!(chain.balance(&alice).await.unwrap() <= alice_start - amount);
}

#[tokio::test]
async fn transfer_funds_rejects_unknown_denom() {
    let wallets = test_wallets();
    let mut chain: SvmChain = SOLANA_LOCALNET.mock(wallets).into();

    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, "SOL", 100_000_000_000)
        .await
        .unwrap();

    let err = chain
        .transfer_funds(&bob, "USDC", 1_000_000_000, TEST_WALLETS.alice)
        .await
        .expect_err("unknown denom");
    assert!(
        matches!(&err, SvmError::Balance(msg) if msg.contains("USDC") && msg.contains("SOL")),
        "unexpected error: {err}"
    );
    assert_eq!(chain.balance(&bob).await.unwrap(), 0);
}

#[tokio::test]
async fn transfer_funds_rejects_insufficient_funds() {
    let wallets = test_wallets();
    let mut chain: SvmChain = SOLANA_LOCALNET.mock(wallets).into();

    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain.set_balance(&alice, "SOL", 1_000_000).await.unwrap(); // 0.001 SOL

    let err = chain
        .transfer_funds(&bob, "SOL", 10_000_000_000, TEST_WALLETS.alice)
        .await
        .expect_err("insufficient funds");
    assert!(
        matches!(err, SvmError::Execute(_)),
        "unexpected error: {err}"
    );
    assert_eq!(chain.balance(&bob).await.unwrap(), 0);
}

#[tokio::test]
async fn rpc_transfer_funds_is_unimplemented() {
    let wallets = test_wallets();
    let chain: SvmChain = SOLANA_LOCALNET.rpc(wallets).into();

    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    let err = chain
        .transfer_funds(&bob, "SOL", 1_000_000_000, TEST_WALLETS.alice)
        .await
        .expect_err("rpc transfer is a deliberate gap");
    assert!(
        matches!(&err, SvmError::Unimplemented(what) if what == "solana rpc transfer_funds"),
        "unexpected error: {err}"
    );
}
