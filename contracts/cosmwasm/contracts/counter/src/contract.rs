use cosmwasm_std::{to_json_binary, Binary, Deps, DepsMut, Response, StdResult};

use crate::msg::{CountResponse, ExecuteMsg, InstantiateMsg, QueryMsg, VersionResponse};
use crate::state::{COUNTER, VERSION};

pub fn instantiate(deps: DepsMut, _msg: InstantiateMsg) -> StdResult<Response> {
    COUNTER.save(deps.storage, &0u64)?;

    Ok(Response::new().add_attribute("action", "instantiate"))
}

pub fn execute(deps: DepsMut, msg: ExecuteMsg) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Increment {} => increment(deps),
        ExecuteMsg::Reset {} => reset(deps),
        ExecuteMsg::SetVersion { version } => set_version(deps, version),
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

fn set_version(deps: DepsMut, version: u64) -> StdResult<Response> {
    VERSION.save(deps.storage, &version)?;

    Ok(Response::new()
        .add_attribute("action", "set_version")
        .add_attribute("version", version.to_string()))
}

pub fn query(deps: Deps, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::GetCount {} => {
            let count = COUNTER.load(deps.storage)?;
            to_json_binary(&CountResponse { count })
        }
        QueryMsg::GetVersion {} => {
            let version = VERSION.may_load(deps.storage)?.unwrap_or(0);
            to_json_binary(&VersionResponse { version })
        }
    }
}
