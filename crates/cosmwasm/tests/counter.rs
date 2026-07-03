//! Integration test: full store_code -> instantiate -> execute -> query path through the
//! CosmWasm provider, driving the canonical counter contract from
//! `contracts/cosmwasm/counter`. The example crate is consumed as an rlib and wrapped
//! in-process via `ContractWrapper`, so no external wasm artifact is required.

use std::rc::Rc;

use cosmwasm_std::Empty;
use counter::{CountResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_cosmwasm::chains::OSMOSIS;
use cross_vm_cosmwasm::CwMockProvider;
use cw_multi_test::{Contract, ContractWrapper};

fn counter_contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(
        counter::execute,
        counter::instantiate,
        counter::query,
    ))
}

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[tokio::test]
async fn deploy_increment_query() {
    let mut chain: CwMockProvider = OSMOSIS.mock(empty_wallets());
    let deployer = chain.new_account("deployer").await;

    let code_id = chain.store_code(counter_contract()).await;
    let contract = chain
        .instantiate(code_id, InstantiateMsg {}, &deployer, &[], "counter")
        .await
        .expect("instantiate");

    let res: CountResponse = chain
        .query_wasm_smart(&contract, QueryMsg::GetCount {})
        .await
        .expect("query");
    assert_eq!(res.count, 0);

    chain
        .execute_contract(&contract, ExecuteMsg::Increment {}, &deployer, &[])
        .await
        .expect("execute 1");
    chain
        .execute_contract(&contract, ExecuteMsg::Increment {}, &deployer, &[])
        .await
        .expect("execute 2");

    let res: CountResponse = chain
        .query_wasm_smart(&contract, QueryMsg::GetCount {})
        .await
        .expect("query");
    assert_eq!(res.count, 2);

    chain
        .execute_contract(&contract, ExecuteMsg::Reset {}, &deployer, &[])
        .await
        .expect("reset");

    let res: CountResponse = chain
        .query_wasm_smart(&contract, QueryMsg::GetCount {})
        .await
        .expect("query");
    assert_eq!(res.count, 0);
}
