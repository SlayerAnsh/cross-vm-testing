use cosmwasm_std::{Deps, DepsMut, Response, StdResult};

use crate::msg::{CountResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use crate::state::COUNTER;

pub fn instantiate(deps: DepsMut, _msg: InstantiateMsg) -> StdResult<Response> {
    COUNTER.save(deps.storage, &0u64)?;

    Ok(Response::new().add_attribute("action", "instantiate"))
}

pub fn execute(deps: DepsMut, msg: ExecuteMsg) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Increment {} => increment(deps),
        ExecuteMsg::Reset {} => reset(deps),
    }
}

fn increment(deps: DepsMut) -> StdResult<Response> {
    let mut count = COUNTER.load(deps.storage)?;
    count += 1;
    COUNTER.save(deps.storage, &count)?;

    Ok(Response::new()
        .add_attribute("action", "increment")
        .add_attribute("count", count.to_string()))
}

fn reset(deps: DepsMut) -> StdResult<Response> {
    COUNTER.save(deps.storage, &0u64)?;

    Ok(Response::new().add_attribute("action", "reset"))
}

pub fn query(deps: Deps, msg: QueryMsg) -> StdResult<CountResponse> {
    match msg {
        QueryMsg::GetCount {} => {
            let count = COUNTER.load(deps.storage)?;
            Ok(CountResponse { count })
        }
    }
}
