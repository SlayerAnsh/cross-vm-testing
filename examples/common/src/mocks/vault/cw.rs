//! CosmWasm vault bindings: message types and the in-process contract factory.

use cosmwasm_std::Empty;
use cw_multi_test::{Contract, ContractWrapper};

pub use ::vault::{AmountResponse, ExecuteMsg, InstantiateMsg, QueryMsg};

/// A `cw-multi-test` wrapper over the vault contract's entry points.
pub fn contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(
        ::vault::execute,
        ::vault::instantiate,
        ::vault::query,
    ))
}
