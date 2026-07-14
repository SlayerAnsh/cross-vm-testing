//! Unit tests for the CosmWasm provider.

use std::rc::Rc;

use crate::chains::{LOCAL, OSMOSIS};
use crate::{CwAsset, CwChain, CwError};
use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};
use cross_vm_macros::define_wallet_roster;

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
        bob: auto @ 1,
    };
}

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

fn wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
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
async fn mock_block_carries_the_preset_chain_id() {
    let mut chain = OSMOSIS.mock(empty_wallets());
    // Not cw-multi-test's `mock_env()` default of `cosmos-testnet-14002`.
    assert_eq!(chain.app().block_info().chain_id, OSMOSIS.chain_id());
    // And advancing the clock preserves it.
    chain.advance_blocks(2, BlockTime::Increment(1)).await;
    assert_eq!(chain.app().block_info().chain_id, OSMOSIS.chain_id());
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

/// Assert `hash` carries the textual shape of a Tendermint tx hash: 32 bytes of sha256 rendered
/// as uppercase hex. Both the live RPC hash and the mock's synthetic stand-in match it.
fn assert_tendermint_hash(hash: &str) {
    assert_eq!(hash.len(), 64, "tendermint tx hash is 32-byte sha256 hex");
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
        "hash `{hash}` is not uppercase hex"
    );
}

/// Read an arbitrary bank denom's balance off a mock chain (the trait's `balance` is native-only).
fn denom_balance(chain: &CwChain, who: &cosmwasm_std::Addr, denom: &str) -> u128 {
    let p = match chain {
        CwChain::Mock(p) => p.clone(),
        CwChain::Rpc(_) => unreachable!("mock chain"),
    };
    let balance = p.app().wrap().query_balance(who, denom).unwrap();
    balance.amount.u128()
}

#[tokio::test]
async fn transfer_funds_moves_the_native_denom() {
    let mut chain: CwChain = OSMOSIS.mock(wallets()).into();
    let denom = chain.chain_info().native_denom;
    let alice = chain
        .wallet_address(TEST_WALLETS.alice)
        .await
        .expect("alice addr");
    let bob = chain
        .wallet_address(TEST_WALLETS.bob)
        .await
        .expect("bob addr");
    chain.set_balance(&alice, denom, 1_000).await.unwrap();

    let hash = chain
        .transfer_funds(&bob, denom, 400, TEST_WALLETS.alice)
        .await
        .expect("transfer");

    assert_eq!(chain.balance(&alice).await.unwrap(), 600);
    assert_eq!(chain.balance(&bob).await.unwrap(), 400);
    // The mock's synthetic hash carries the same shape as a live Tendermint one.
    assert_tendermint_hash(&hash);
}

#[tokio::test]
async fn transfer_funds_moves_a_non_native_denom() {
    // CosmWasm's bank module moves any denom the sender holds, not just the chain's native one.
    const IBC_DENOM: &str = "ibc/27394FB092D2ECCD56123C74F36E4C1F926001CEADA9CA97EA622B25F41E5EB2";

    let mut chain: CwChain = OSMOSIS.mock(wallets()).into();
    let alice = chain
        .wallet_address(TEST_WALLETS.alice)
        .await
        .expect("alice addr");
    let bob = chain
        .wallet_address(TEST_WALLETS.bob)
        .await
        .expect("bob addr");
    chain.set_balance(&alice, IBC_DENOM, 900).await.unwrap();

    chain
        .transfer_funds(&bob, IBC_DENOM, 250, TEST_WALLETS.alice)
        .await
        .expect("transfer");

    assert_eq!(denom_balance(&chain, &alice, IBC_DENOM), 650);
    assert_eq!(denom_balance(&chain, &bob, IBC_DENOM), 250);
}

#[tokio::test]
async fn transfer_funds_rejects_an_underfunded_sender() {
    let mut chain: CwChain = OSMOSIS.mock(wallets()).into();
    let denom = chain.chain_info().native_denom;
    let alice = chain
        .wallet_address(TEST_WALLETS.alice)
        .await
        .expect("alice addr");
    let bob = chain
        .wallet_address(TEST_WALLETS.bob)
        .await
        .expect("bob addr");
    chain.set_balance(&alice, denom, 100).await.unwrap();

    let err = chain
        .transfer_funds(&bob, denom, 1_000, TEST_WALLETS.alice)
        .await
        .expect_err("sender holds only 100");
    assert!(
        matches!(err, CwError::Execute(_)),
        "insufficient funds is an execute failure, got {err:?}"
    );

    // The failed send left both balances untouched.
    assert_eq!(chain.balance(&alice).await.unwrap(), 100);
    assert_eq!(chain.balance(&bob).await.unwrap(), 0);
}

#[tokio::test]
async fn every_mutating_step_carries_a_distinct_tx_hash() {
    use cosmwasm_std::Empty;
    use counter::{ExecuteMsg, InstantiateMsg};
    use cw_multi_test::{Contract, ContractWrapper};

    let chain: CwChain = OSMOSIS.mock(wallets()).into();
    let code: Box<dyn Contract<Empty, Empty>> = Box::new(ContractWrapper::new(
        counter::execute,
        counter::instantiate,
        counter::query,
    ));

    let stored = chain
        .store_code(code, TEST_WALLETS.alice)
        .await
        .expect("store_code");
    let instantiated = chain
        .instantiate(
            stored.code_id,
            InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
        )
        .await
        .expect("instantiate");
    let exec = chain
        .execute_contract(
            &instantiated.address,
            ExecuteMsg::Increment {},
            TEST_WALLETS.alice,
            &[],
        )
        .await
        .expect("increment");

    let hashes = [
        stored.tx_hash.as_str(),
        instantiated.tx_hash.as_str(),
        exec.tx_hash.as_str(),
    ];
    for hash in hashes {
        assert_tendermint_hash(hash);
    }
    // Each step is its own transaction on a live chain, so the mock's stand-ins never collide.
    let unique: std::collections::HashSet<&str> = hashes.iter().copied().collect();
    assert_eq!(unique.len(), hashes.len(), "hashes collide: {hashes:?}");
}

#[tokio::test]
async fn mock_reports_no_gas_figure_rather_than_a_zero_one() {
    use cosmwasm_std::Empty;
    use counter::{ExecuteMsg, InstantiateMsg};
    use cw_multi_test::{Contract, ContractWrapper};

    let chain: CwChain = OSMOSIS.mock(wallets()).into();
    let code: Box<dyn Contract<Empty, Empty>> = Box::new(ContractWrapper::new(
        counter::execute,
        counter::instantiate,
        counter::query,
    ));

    let stored = chain
        .store_code(code, TEST_WALLETS.alice)
        .await
        .expect("store_code");
    let instantiated = chain
        .instantiate(
            stored.code_id,
            InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
        )
        .await
        .expect("instantiate");
    let exec = chain
        .execute_contract(
            &instantiated.address,
            ExecuteMsg::Increment {},
            TEST_WALLETS.alice,
            &[],
        )
        .await
        .expect("increment");

    // cw-multi-test has no gas meter, so every mock op reports the absence of a figure. A
    // `Some(CwGas { used: 0, fee: 0 })` would assert the mock measured these transactions and
    // found them free, which a caller could not tell apart from a genuinely free tx. If a future
    // change starts fabricating a zero, this fails.
    for (op, gas) in [
        ("store_code", stored.gas),
        ("instantiate", instantiated.gas),
        ("execute_contract", exec.gas),
    ] {
        assert!(
            gas.is_none(),
            "mock {op} must report no gas figure (it cannot meter), got {gas:?}"
        );
    }
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = OSMOSIS.rpc(empty_wallets());
    let addr = cosmwasm_std::Addr::unchecked("osmo1xyz");
    assert!(chain.set_balance(&addr, "uosmo", 1).await.is_err());
}

#[tokio::test]
async fn query_wasm_raw_reads_item_storage() {
    use cosmwasm_std::Empty;
    use counter::{ExecuteMsg, InstantiateMsg};
    use cw_multi_test::{Contract, ContractWrapper};

    let mut chain = OSMOSIS.mock(empty_wallets());
    let deployer = chain.new_account("deployer").await;

    let code: Box<dyn Contract<Empty, Empty>> = Box::new(ContractWrapper::new(
        counter::execute,
        counter::instantiate,
        counter::query,
    ));
    let code_id = chain.store_code(&deployer, code).await.code_id;
    let contract = chain
        .instantiate(code_id, InstantiateMsg {}, &deployer, &[], "counter")
        .await
        .expect("instantiate")
        .address;

    // The counter contract keeps its count in a cw-storage-plus `Item::new("counter")`, which
    // lands under the raw storage key `b"counter"`, JSON-encoded (`0u64` -> b"0").
    let raw = chain
        .query_wasm_raw(&contract, b"counter")
        .await
        .expect("raw query")
        .expect("counter key present after instantiate");
    assert_eq!(
        serde_json::from_slice::<u64>(&raw).expect("raw bytes parse as u64"),
        0
    );

    // Two increments later, the same raw key reflects the new value.
    for _ in 0..2 {
        chain
            .execute_contract(&contract, ExecuteMsg::Increment {}, &deployer, &[])
            .await
            .expect("increment");
    }
    let raw = chain
        .query_wasm_raw(&contract, b"counter")
        .await
        .expect("raw query")
        .expect("counter key present");
    assert_eq!(
        serde_json::from_slice::<u64>(&raw).expect("raw bytes parse as u64"),
        2
    );

    // A key the contract never writes comes back as `None`.
    assert!(chain
        .query_wasm_raw(&contract, b"missing")
        .await
        .expect("raw query")
        .is_none());
}

#[tokio::test]
async fn get_contract_states_dumps_all_storage() {
    use cosmwasm_std::Empty;
    use counter::{ExecuteMsg, InstantiateMsg};
    use cw_multi_test::{Contract, ContractWrapper};

    let mut chain = OSMOSIS.mock(empty_wallets());
    let deployer = chain.new_account("deployer").await;

    let code: Box<dyn Contract<Empty, Empty>> = Box::new(ContractWrapper::new(
        counter::execute,
        counter::instantiate,
        counter::query,
    ));
    let code_id = chain.store_code(&deployer, code).await.code_id;
    let contract = chain
        .instantiate(code_id, InstantiateMsg {}, &deployer, &[], "counter")
        .await
        .expect("instantiate")
        .address;

    // The full dump carries the counter's `Item::new("counter")` entry under raw key `b"counter"`,
    // JSON-encoded (`0u64` -> b"0"). Contract-info-only keys are harmless: we only assert the
    // counter pair is present among whatever the dump returns.
    let states = chain
        .get_contract_states(&contract)
        .await
        .expect("dump states");
    let counter = states
        .iter()
        .find(|(k, _)| k.as_slice() == b"counter")
        .expect("counter key present after instantiate");
    assert_eq!(
        serde_json::from_slice::<u64>(&counter.1).expect("raw bytes parse as u64"),
        0
    );

    // Two increments later, the dump reflects the new value under the same key.
    for _ in 0..2 {
        chain
            .execute_contract(&contract, ExecuteMsg::Increment {}, &deployer, &[])
            .await
            .expect("increment");
    }
    let states = chain
        .get_contract_states(&contract)
        .await
        .expect("dump states");
    let counter = states
        .iter()
        .find(|(k, _)| k.as_slice() == b"counter")
        .expect("counter key present");
    assert_eq!(
        serde_json::from_slice::<u64>(&counter.1).expect("raw bytes parse as u64"),
        2
    );
}
