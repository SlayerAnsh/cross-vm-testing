//! Typed message handles on a multi-variant contract: the CosmWasm vault. Exercises the
//! `CwExecuteFns` derive (including a `#[payable]` `deposit` that attaches native funds) and the
//! `CwQueryFns` derive, end to end on a mock chain.

use std::rc::Rc;

use cosmwasm_std::{coins, Empty, Uint128};
use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_cosmwasm::chains::OSMOSIS;
use cross_vm_cosmwasm::CwChain;
use cross_vm_macros::define_wallet_roster;
use cw_multi_test::{Contract, ContractWrapper};
use vault::{AmountResponse, ExecuteMsgFns, QueryMsgFns};

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
    };
}

fn vault_contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(
        vault::execute,
        vault::instantiate,
        vault::query,
    ))
}

fn wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

#[tokio::test]
async fn payable_deposit_attaches_funds_then_borrow_and_query() {
    let mut chain: CwChain = OSMOSIS.mock(wallets()).into();
    let denom = chain.chain_info().native_denom;

    let alice = chain
        .wallet_address(TEST_WALLETS.alice)
        .await
        .expect("alice addr");
    chain
        .set_balance(&alice, denom, 1_000_000)
        .await
        .expect("fund alice");

    let code_id = chain.store_code(vault_contract()).await.expect("store");
    let addr = chain
        .instantiate(
            code_id,
            vault::InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "vault",
        )
        .await
        .expect("instantiate");
    let vault = chain.contract_as::<vault::VaultContract>(addr);

    let before = chain.balance(&alice).await.expect("balance before");
    vault
        .deposit("alice", Uint128::new(1000), &coins(100, denom))
        .await
        .expect("deposit");
    let after = chain.balance(&alice).await.expect("balance after");
    assert!(after < before);

    vault
        .borrow("alice", Uint128::new(500))
        .await
        .expect("borrow");

    let debt: AmountResponse = vault.debt(alice.to_string()).await.expect("debt query");
    assert_eq!(debt.amount, Uint128::new(500));
}
