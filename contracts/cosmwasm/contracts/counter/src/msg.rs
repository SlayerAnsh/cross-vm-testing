use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InstantiateMsg {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwExecuteFns))]
pub enum ExecuteMsg {
    Increment {},
    Reset {},
    SetVersion { version: u64 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwQueryFns))]
pub enum QueryMsg {
    #[cfg_attr(feature = "cross-vm", returns(CountResponse))]
    GetCount {},
    #[cfg_attr(feature = "cross-vm", returns(VersionResponse))]
    GetVersion {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CountResponse {
    pub count: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VersionResponse {
    pub version: u64,
}

#[cfg(feature = "cross-vm")]
cross_vm_macros::cross_vm_cw_interface!(pub CounterContract, InstantiateMsg, ExecuteMsg, QueryMsg);
