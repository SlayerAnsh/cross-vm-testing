//! Wallet derivation: determinism within a run and per-VM coin-type distinctness.

use cross_vm_framework::prelude::*;

use crate::support::test_wallets;

#[tokio::test]
async fn derivation_is_deterministic_and_vm_distinct() {
    // `alice` is an `auto` wallet: its mnemonic is generated once at factory build and shared by
    // every chain, so addresses are stable within this run (just not a fixed known vector).
    let f = test_wallets();
    let cw: CwChain = OSMOSIS.mock(f.clone()).into();
    let evm: EvmChain = ETHEREUM.mock(f.clone()).into();
    let svm: SvmChain = SOLANA_DEVNET.mock(f).into();

    let cw_a = cw.wallet_address(TEST_WALLETS.alice).await.unwrap();
    assert_eq!(cw_a, cw.wallet_address(TEST_WALLETS.alice).await.unwrap());

    let evm_a = evm.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let svm_a = svm.wallet_address(TEST_WALLETS.alice).await.unwrap();

    assert!(cw_a.to_string().starts_with("osmo"));
    assert_ne!(cw_a.to_string(), evm_a.to_string());
    assert_ne!(evm_a.to_string(), svm_a.to_string());

    assert_ne!(cw_a, cw.wallet_address(TEST_WALLETS.bob).await.unwrap());
}
