//! Unit tests for the Solana provider.

use std::rc::Rc;

use crate::chains::{SOLANA_DEVNET, SOLANA_LOCALNET};
use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[test]
fn predefined_chain_metadata() {
    assert_eq!(SOLANA_DEVNET.chain_id(), "devnet");
    assert_eq!(SOLANA_DEVNET.native_symbol(), "SOL");
}

#[tokio::test]
async fn new_account_is_funded() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.balance(&alice).await.unwrap(),
        crate::DEFAULT_FUNDING_LAMPORTS
    );
}

#[tokio::test]
async fn set_and_read_balance() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain.set_balance(&bob, 12_345).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 12_345);
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    assert_eq!(chain.block_height().await, 0);
    chain.advance_blocks(4, BlockTime::Increment(1)).await;
    assert_eq!(chain.block_height().await, 4);
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = SOLANA_DEVNET.rpc(empty_wallets());
    let addr = solana_address::Address::new_unique();
    assert!(chain.set_balance(&addr, 1).await.is_err());
}
