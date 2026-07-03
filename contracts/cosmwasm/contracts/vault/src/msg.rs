//! Message schema for the vault: instantiate, the four ledger operations, and the read queries.

use cosmwasm_std::Uint128;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InstantiateMsg {}

/// Ledger operations, all signed by `info.sender`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwExecuteFns))]
pub enum ExecuteMsg {
    // `deposit` attaches native funds (the typed handle gains a `funds: &[Coin]` arg).
    #[cfg_attr(feature = "cross-vm", payable)]
    Deposit {
        amount: Uint128,
    },
    Withdraw {
        amount: Uint128,
    },
    Borrow {
        amount: Uint128,
    },
    Repay {
        amount: Uint128,
    },
}

/// Read a user's collateral or debt by address.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwQueryFns))]
pub enum QueryMsg {
    #[cfg_attr(feature = "cross-vm", returns(AmountResponse))]
    Collateral { who: String },
    #[cfg_attr(feature = "cross-vm", returns(AmountResponse))]
    Debt { who: String },
}

/// A single `Uint128` amount, returned by both queries.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AmountResponse {
    pub amount: Uint128,
}
