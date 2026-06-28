//! Integration test: full deploy -> execute -> query path through the CosmWasm
//! provider, using an in-test counter contract (no external wasm needed).

use cosmwasm_std::{
    to_json_binary, Binary, Deps, DepsMut, Empty, Env, MessageInfo, Response, StdError, StdResult,
};
use cross_vm_core::ChainProvider;
use cross_vm_cosmwasm::chains::OSMOSIS;
use cw_multi_test::{Contract, ContractWrapper};
use serde::{Deserialize, Serialize};
use serde_json::json;

const COUNT_KEY: &[u8] = b"count";

#[derive(Serialize, Deserialize)]
struct InstantiateMsg {
    count: i32,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExecuteMsg {
    Increment {},
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QueryMsg {
    Count {},
}

#[derive(Serialize, Deserialize)]
struct CountResponse {
    count: i32,
}

fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> StdResult<Response> {
    deps.storage.set(COUNT_KEY, &msg.count.to_be_bytes());
    Ok(Response::new())
}

fn execute(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: ExecuteMsg,
) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Increment {} => {
            let cur = read_count(deps.storage)?;
            deps.storage.set(COUNT_KEY, &(cur + 1).to_be_bytes());
            Ok(Response::new().add_attribute("action", "increment"))
        }
    }
}

fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Count {} => to_json_binary(&CountResponse {
            count: read_count(deps.storage)?,
        }),
    }
}

fn read_count(storage: &dyn cosmwasm_std::Storage) -> StdResult<i32> {
    let bytes = storage
        .get(COUNT_KEY)
        .ok_or_else(|| StdError::msg("count not set"))?;
    let arr: [u8; 4] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| StdError::msg("bad count encoding"))?;
    Ok(i32::from_be_bytes(arr))
}

fn counter_contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(execute, instantiate, query))
}

#[test]
fn deploy_increment_query() {
    let mut chain = OSMOSIS.mock();
    let deployer = chain.new_account("deployer");

    // Deploy with initial count = 5.
    let contract = chain
        .deploy(counter_contract(), json!({ "count": 5 }), &deployer)
        .expect("deploy");

    // Initial query.
    let res = chain.query(&contract, json!({ "count": {} })).expect("query");
    assert_eq!(res["count"], 5);

    // Increment twice.
    chain
        .execute(&contract, json!({ "increment": {} }), &deployer)
        .expect("execute 1");
    chain
        .execute(&contract, json!({ "increment": {} }), &deployer)
        .expect("execute 2");

    // Final query.
    let res = chain.query(&contract, json!({ "count": {} })).expect("query");
    assert_eq!(res["count"], 7);
}
