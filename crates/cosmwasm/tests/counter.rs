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

    let code_id = chain
        .store_code(&deployer, counter_contract())
        .await
        .code_id;
    let contract = chain
        .instantiate(code_id, InstantiateMsg {}, &deployer, &[], "counter")
        .await
        .expect("instantiate")
        .address;

    let res: CountResponse = chain
        .query_wasm_smart(&contract, QueryMsg::GetCount {})
        .await
        .expect("query");
    assert_eq!(res.count, 0);

    let exec1 = chain
        .execute_contract(&contract, ExecuteMsg::Increment {}, &deployer, &[])
        .await
        .expect("execute 1");
    let exec2 = chain
        .execute_contract(&contract, ExecuteMsg::Increment {}, &deployer, &[])
        .await
        .expect("execute 2");
    // The mock mints a synthetic, deterministic tx hash (uppercase sha256 hex, Tendermint shape),
    // distinct per execute, so a test can read a hash on the mock exactly as on live RPC.
    let (h1, h2) = (exec1.tx_hash, exec2.tx_hash);
    assert_eq!(h1.len(), 64, "tendermint tx hash is 32-byte sha256 hex");
    assert!(h1
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
    assert_ne!(h1, h2, "repeated identical executes get distinct hashes");

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
