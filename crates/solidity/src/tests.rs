//! Unit tests for the EVM provider.

use std::rc::Rc;

use crate::chains::{ETHEREUM, LOCAL};
use alloy_primitives::{Bytes, U256};
use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[test]
fn predefined_chain_metadata() {
    assert_eq!(ETHEREUM.chain_id(), "1");
    assert_eq!(ETHEREUM.numeric_id(), 1);
    assert_eq!(ETHEREUM.native_symbol(), "ETH");
}

#[tokio::test]
async fn new_account_is_funded() {
    let mut chain = ETHEREUM.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.balance(&alice).await.unwrap(),
        U256::from(crate::DEFAULT_FUNDING_WEI)
    );
}

#[tokio::test]
async fn set_and_read_balance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain
        .set_balance(&bob, "ETH", U256::from(42u64))
        .await
        .unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(42u64));
}

#[tokio::test]
async fn set_balance_validates_denom() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;

    // Unknown denom is rejected.
    assert!(chain
        .set_balance(&bob, "BTC", U256::from(1u64))
        .await
        .is_err());

    // The native symbol is accepted case-insensitively.
    chain
        .set_balance(&bob, "eth", U256::from(7u64))
        .await
        .unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(7u64));
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let h0 = chain.block_height().await;
    chain.advance_blocks(5, BlockTime::Increment(1)).await;
    assert_eq!(chain.block_height().await, h0 + 5);
}

#[tokio::test]
async fn reads_storage_slot_written_by_constructor() {
    // Initcode whose constructor writes 42 into storage slot 0, then returns an empty runtime:
    //   PUSH1 0x2a, PUSH1 0x00, SSTORE, PUSH1 0x00, PUSH1 0x00, RETURN.
    let initcode = Bytes::from(vec![
        0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x60, 0x00, 0xf3,
    ]);
    let mut chain = LOCAL.mock(empty_wallets());
    let deployer = chain.new_account("deployer").await;
    let addr = chain
        .deploy_create(initcode, [], &deployer)
        .await
        .expect("storage-writing deploy succeeds");
    // The constructor wrote 42 at slot 0; an untouched slot reads as zero.
    assert_eq!(
        chain.get_storage_at(&addr, U256::ZERO).await.unwrap(),
        U256::from(42u64)
    );
    assert_eq!(
        chain.get_storage_at(&addr, U256::from(1u64)).await.unwrap(),
        U256::ZERO
    );
}

#[tokio::test]
async fn get_storage_at_plumbs_through_chain() {
    // Exercise the `EvmChain` enum dispatch: an unset slot reads as zero.
    let mut chain = crate::EvmChain::from(LOCAL.mock(empty_wallets()));
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.get_storage_at(&alice, U256::ZERO).await.unwrap(),
        U256::ZERO
    );
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = ETHEREUM.rpc(empty_wallets());
    assert!(chain
        .set_balance(&alloy_primitives::Address::ZERO, "ETH", U256::from(1u64))
        .await
        .is_err());
}
