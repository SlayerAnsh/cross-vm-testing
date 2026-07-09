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
    chain.set_balance(&bob, "SOL", 12_345).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 12_345);
}

#[tokio::test]
async fn set_balance_validates_denom() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let bob = chain.new_account("bob").await;

    assert!(chain.set_balance(&bob, "BTC", 1).await.is_err());

    chain.set_balance(&bob, "sol", 7_777).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 7_777);
}

#[tokio::test]
async fn get_account_data_matches_account_bytes() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let carol = chain.new_account("carol").await;

    let account = chain
        .get_account(&carol)
        .await
        .expect("funded account exists");
    let data = chain
        .get_account_data(&carol)
        .await
        .expect("funded account exists");
    assert_eq!(data, account.data);

    // A never-seen pubkey has no account, hence no data.
    let missing = solana_address::Address::new_unique();
    assert!(chain.get_account_data(&missing).await.is_none());
}

#[tokio::test]
async fn get_account_data_slice_matches_prefix() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let carol = chain.new_account("carol").await;

    let data = chain
        .get_account_data(&carol)
        .await
        .expect("funded account exists");
    let n = data.len().min(8);

    let slice = chain
        .get_account_data_slice(&carol, 0, n)
        .await
        .expect("slice within data");
    assert_eq!(slice, data[..n]);

    // An offset past the end of the data yields no window (all-or-nothing).
    assert!(chain
        .get_account_data_slice(&carol, data.len() + 1, 1)
        .await
        .is_none());

    // A never-seen pubkey has no account, hence no slice.
    let missing = solana_address::Address::new_unique();
    assert!(chain.get_account_data_slice(&missing, 0, 1).await.is_none());
}

#[test]
fn find_program_account_is_deterministic() {
    use crate::chain::SvmChain;

    let program_id = solana_address::Address::new_unique();
    let seeds: &[&[u8]] = &[b"counter", b"alice"];

    let a = SvmChain::find_program_account(&program_id, seeds);
    let b = SvmChain::find_program_account(&program_id, seeds);
    assert_eq!(a, b, "same seeds must derive the same PDA");

    let (direct, _bump) = solana_address::Address::find_program_address(seeds, &program_id);
    assert_eq!(a, direct, "helper must match Address::find_program_address");

    // Different seeds derive a different cell.
    let other = SvmChain::find_program_account(&program_id, &[b"counter", b"bob"]);
    assert_ne!(a, other);
}

#[tokio::test]
async fn get_program_state_none_when_pda_unfunded() {
    use crate::chain::SvmChain;

    let chain: SvmChain = SOLANA_LOCALNET.mock(empty_wallets()).into();
    let program_id = solana_address::Address::new_unique();

    // The derived PDA has no account yet, so a point-read reports Ok(None).
    let state = chain
        .get_program_state(&program_id, &[b"state"], 0, 8)
        .await
        .expect("query succeeds");
    assert!(state.is_none());
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
    assert!(chain.set_balance(&addr, "SOL", 1).await.is_err());
}
