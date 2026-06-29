//! Unit tests for the CosmWasm provider.

use std::rc::Rc;

use crate::chains::{LOCAL, OSMOSIS};
use cross_vm_core::{ChainProvider, ChainSpec, WalletFactory};

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[test]
fn predefined_chain_metadata() {
    assert_eq!(OSMOSIS.chain_id(), "osmosis-1");
    assert_eq!(OSMOSIS.native_denom, "uosmo");
}

#[tokio::test]
async fn new_account_is_funded() {
    let mut chain = OSMOSIS.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert!(chain.balance(&alice).await.unwrap() >= crate::DEFAULT_FUNDING);
}

#[tokio::test]
async fn set_and_read_balance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain.set_balance(&bob, 42).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 42);
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let h0 = chain.block_height().await;
    chain.advance_blocks(3).await;
    assert_eq!(chain.block_height().await, h0 + 3);
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = OSMOSIS.rpc(empty_wallets());
    let addr = cosmwasm_std::Addr::unchecked("osmo1xyz");
    assert!(chain.set_balance(&addr, 1).await.is_err());
}
