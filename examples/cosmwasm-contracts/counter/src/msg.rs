use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InstantiateMsg {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwExecuteFns))]
pub enum ExecuteMsg {
    Increment {},
    Reset {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwQueryFns))]
pub enum QueryMsg {
    #[cfg_attr(feature = "cross-vm", returns(CountResponse))]
    GetCount {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CountResponse {
    pub count: u64,
}
