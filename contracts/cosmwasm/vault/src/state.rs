use cosmwasm_std::{Addr, Uint128};
use cw_storage_plus::Map;

/// Per-user collateral and debt ledgers.
pub const COLLATERAL: Map<&Addr, Uint128> = Map::new("collateral");
pub const DEBT: Map<&Addr, Uint128> = Map::new("debt");
