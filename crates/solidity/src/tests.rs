//! Unit tests for the EVM provider.

use std::rc::Rc;

use crate::chains::{ETHEREUM, LOCAL};
use alloy_primitives::U256;
use cross_vm_core::{ChainProvider, ChainSpec, WalletFactory};

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
    chain.set_balance(&bob, U256::from(42u64)).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(42u64));
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let h0 = chain.block_height().await;
    chain.advance_blocks(5).await;
    assert_eq!(chain.block_height().await, h0 + 5);
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = ETHEREUM.rpc(empty_wallets());
    assert!(chain
        .set_balance(&alloy_primitives::Address::ZERO, U256::from(1u64))
        .await
        .is_err());
}
