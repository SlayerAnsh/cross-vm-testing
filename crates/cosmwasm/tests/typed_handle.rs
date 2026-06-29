//! `CwContract` handle: bind a `CwChain` + address once, then `execute` / `query` without
//! repeating the address. Drives the canonical counter contract on a mock chain.

use std::rc::Rc;

use cosmwasm_std::Empty;
use counter::{CountResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use cross_vm_core::WalletFactory;
use cross_vm_cosmwasm::chains::OSMOSIS;
use cross_vm_cosmwasm::CwChain;
use cross_vm_macros::define_wallet_roster;
use cw_multi_test::{Contract, ContractWrapper};

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
    };
}

fn counter_contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(
        counter::execute,
        counter::instantiate,
        counter::query,
    ))
}

fn wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

#[tokio::test]
async fn handle_binds_address_for_execute_and_query() {
    let chain: CwChain = OSMOSIS.mock(wallets()).into();

    let code_id = chain.store_code(counter_contract()).await.expect("store");
    let addr = chain
        .instantiate(
            code_id,
            InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
        )
        .await
        .expect("instantiate");

    let contract = chain.contract(addr);

    let res: CountResponse = contract.query(QueryMsg::GetCount {}).await.expect("query");
    assert_eq!(res.count, 0);

    contract
        .execute(ExecuteMsg::Increment {}, "alice")
        .await
        .expect("increment");

    let res: CountResponse = contract.query(QueryMsg::GetCount {}).await.expect("query");
    assert_eq!(res.count, 1);
}
