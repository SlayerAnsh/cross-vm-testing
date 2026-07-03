//! CosmWasm ping-pong bindings: message types and the in-process contract factory.

use cosmwasm_std::Empty;
use cw_multi_test::{Contract, ContractWrapper};

pub use ::ping_pong::{ExecuteMsg, InstantiateMsg, QueryMsg, StatsResponse};

/// A `cw-multi-test` wrapper over the ping-pong contract's entry points.
pub fn contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(
        ::ping_pong::execute,
        ::ping_pong::instantiate,
        ::ping_pong::query,
    ))
}
