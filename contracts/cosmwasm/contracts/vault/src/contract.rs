//! Collateralized-debt vault logic (CosmWasm port).
//!
//! Mirrors the EVM and Solana vaults: a pure accounting ledger keyed by `info.sender`. A user
//! deposits collateral, borrows debt up to an LTV fraction of it, repays, and withdraws
//! collateral not locked by debt. Violations return an error, which `cw-multi-test` surfaces as
//! a failed execute (the rejection path a property test exercises). Identical LTV math across
//! all three VMs lets one shadow model validate every chain.

use cosmwasm_std::{
    to_json_binary, Addr, Binary, Deps, DepsMut, Response, StdError, StdResult, Uint128,
};

use crate::msg::{AmountResponse, ExecuteMsg, InstantiateMsg, QueryMsg, VersionResponse};
use crate::state::{COLLATERAL, DEBT, VERSION};

/// Loan-to-value, in basis points (5000 = 50%): max debt is `collateral * LTV_BPS / 10000`.
const LTV_BPS: u128 = 5000;

/// Maximum debt a given collateral can support (floored).
fn max_debt(collateral: Uint128) -> Uint128 {
    collateral.multiply_ratio(LTV_BPS, 10000u128)
}

/// Collateral that must remain locked to back `debt`. Inverse of [`max_debt`], rounded up
/// (`multiply_ratio` floors) so a borrower can never withdraw into bad debt.
fn required_collateral(debt: Uint128) -> Uint128 {
    debt.multiply_ratio(10000u128, LTV_BPS)
        + if (debt.u128() * 10000) % LTV_BPS == 0 {
            Uint128::zero()
        } else {
            Uint128::one()
        }
}

/// No global state to set up: ledgers are created lazily per sender on first write.
pub fn instantiate(_deps: DepsMut, _msg: InstantiateMsg) -> StdResult<Response> {
    Ok(Response::new().add_attribute("action", "instantiate"))
}

/// Dispatch an execute message. `sender` is `info.sender`, threaded in from the entry point.
pub fn execute(deps: DepsMut, sender: Addr, msg: ExecuteMsg) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Deposit { amount } => deposit(deps, sender, amount),
        ExecuteMsg::Withdraw { amount } => withdraw(deps, sender, amount),
        ExecuteMsg::Borrow { amount } => borrow(deps, sender, amount),
        ExecuteMsg::Repay { amount } => repay(deps, sender, amount),
        ExecuteMsg::SetVersion { version } => set_version(deps, version),
    }
}

/// Read a ledger entry, treating an absent key as zero.
fn load(map: cw_storage_plus::Map<&Addr, Uint128>, deps: &Deps, who: &Addr) -> StdResult<Uint128> {
    Ok(map.may_load(deps.storage, who)?.unwrap_or_default())
}

/// Credit `amount` of collateral to `sender`.
fn deposit(deps: DepsMut, sender: Addr, amount: Uint128) -> StdResult<Response> {
    let c = COLLATERAL
        .may_load(deps.storage, &sender)?
        .unwrap_or_default();
    COLLATERAL.save(deps.storage, &sender, &(c + amount))?;
    Ok(Response::new()
        .add_attribute("action", "deposit")
        .add_attribute("amount", amount.to_string()))
}

/// Withdraw collateral not locked by outstanding debt.
fn withdraw(deps: DepsMut, sender: Addr, amount: Uint128) -> StdResult<Response> {
    let c = COLLATERAL
        .may_load(deps.storage, &sender)?
        .unwrap_or_default();
    let d = DEBT.may_load(deps.storage, &sender)?.unwrap_or_default();
    if amount > c {
        return Err(StdError::generic_err("amount exceeds collateral"));
    }
    if c - amount < required_collateral(d) {
        return Err(StdError::generic_err("insufficient free collateral"));
    }
    COLLATERAL.save(deps.storage, &sender, &(c - amount))?;
    Ok(Response::new()
        .add_attribute("action", "withdraw")
        .add_attribute("amount", amount.to_string()))
}

/// Borrow against collateral, up to the LTV limit.
fn borrow(deps: DepsMut, sender: Addr, amount: Uint128) -> StdResult<Response> {
    let c = COLLATERAL
        .may_load(deps.storage, &sender)?
        .unwrap_or_default();
    let d = DEBT.may_load(deps.storage, &sender)?.unwrap_or_default();
    let new_debt = d + amount;
    if new_debt > max_debt(c) {
        return Err(StdError::generic_err("exceeds max debt"));
    }
    DEBT.save(deps.storage, &sender, &new_debt)?;
    Ok(Response::new()
        .add_attribute("action", "borrow")
        .add_attribute("amount", amount.to_string()))
}

/// Repay outstanding debt.
fn repay(deps: DepsMut, sender: Addr, amount: Uint128) -> StdResult<Response> {
    let d = DEBT.may_load(deps.storage, &sender)?.unwrap_or_default();
    if amount > d {
        return Err(StdError::generic_err("repay exceeds debt"));
    }
    DEBT.save(deps.storage, &sender, &(d - amount))?;
    Ok(Response::new()
        .add_attribute("action", "repay")
        .add_attribute("amount", amount.to_string()))
}

/// Store the contract version.
fn set_version(deps: DepsMut, version: u64) -> StdResult<Response> {
    VERSION.save(deps.storage, &version)?;
    Ok(Response::new()
        .add_attribute("action", "set_version")
        .add_attribute("version", version.to_string()))
}

/// Read a user's collateral or debt, or the stored version.
pub fn query(deps: Deps, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Collateral { who } => {
            let who = deps.api.addr_validate(&who)?;
            to_json_binary(&AmountResponse {
                amount: load(COLLATERAL, &deps, &who)?,
            })
        }
        QueryMsg::Debt { who } => {
            let who = deps.api.addr_validate(&who)?;
            to_json_binary(&AmountResponse {
                amount: load(DEBT, &deps, &who)?,
            })
        }
        QueryMsg::GetVersion {} => {
            let version = VERSION.may_load(deps.storage)?.unwrap_or(0);
            to_json_binary(&VersionResponse { version })
        }
    }
}
