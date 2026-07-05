//! Unit tests for the CosmWasm provider.

use std::rc::Rc;

use crate::chains::{LOCAL, OSMOSIS};
use crate::{CwAsset, CwChain};
use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};

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
    chain.set_balance(&bob, "ustake", 42).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 42);
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let h0 = chain.block_height().await;
    chain.advance_blocks(3, BlockTime::Increment(1)).await;
    assert_eq!(chain.block_height().await, h0 + 3);
}

#[tokio::test]
async fn set_balance_multiple_denoms() {
    let mut chain: CwChain = LOCAL.mock(empty_wallets()).into();
    let bob = chain.new_account("bob").await;
    chain.set_balance(&bob, "ustake", 100).await.unwrap();
    chain.set_balance(&bob, "uatom", 55).await.unwrap();

    // The native denom survives minting a second denom.
    assert_eq!(chain.balance(&bob).await.unwrap(), 100);

    // The second denom is readable at the bank level. Cloning the mock provider
    // shares the same chain state (Rc), so `p` stays valid across later writes.
    let p = match &chain {
        CwChain::Mock(p) => p.clone(),
        CwChain::Rpc(_) => unreachable!("mock chain"),
    };
    let uatom = p.app().wrap().query_balance(&bob, "uatom").unwrap();
    assert_eq!(uatom.amount.u128(), 55);

    // Setting an existing denom overwrites its amount, it does not add.
    chain.set_balance(&bob, "uatom", 5).await.unwrap();
    let uatom = p.app().wrap().query_balance(&bob, "uatom").unwrap();
    assert_eq!(uatom.amount.u128(), 5);

    // Amount 0 clears the denom entry.
    chain.set_balance(&bob, "uatom", 0).await.unwrap();
    #[allow(deprecated)]
    // cosmwasm-std 2.3 deprecates query_all_balances; no non-paginated replacement.
    let all = p.app().wrap().query_all_balances(&bob).unwrap();
    assert!(all.iter().all(|c| c.denom != "uatom"));
    assert_eq!(chain.balance(&bob).await.unwrap(), 100);
}

#[tokio::test]
async fn ensure_asset_native_preserves_other_denoms() {
    let mut chain: CwChain = LOCAL.mock(empty_wallets()).into();
    let bob = chain.new_account("bob").await;
    chain.set_balance(&bob, "uatom", 10).await.unwrap();

    // new_account funded DEFAULT_FUNDING of "ustake"; asking for double forces a mint.
    chain
        .ensure_asset(
            &bob,
            CwAsset::Native("ustake".to_string()),
            2 * crate::DEFAULT_FUNDING,
        )
        .await
        .unwrap();

    let p = match &chain {
        CwChain::Mock(p) => p.clone(),
        CwChain::Rpc(_) => unreachable!("mock chain"),
    };
    assert_eq!(
        p.app()
            .wrap()
            .query_balance(&bob, "uatom")
            .unwrap()
            .amount
            .u128(),
        10
    );
    assert_eq!(
        chain.balance(&bob).await.unwrap(),
        2 * crate::DEFAULT_FUNDING
    );
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = OSMOSIS.rpc(empty_wallets());
    let addr = cosmwasm_std::Addr::unchecked("osmo1xyz");
    assert!(chain.set_balance(&addr, "uosmo", 1).await.is_err());
}
