//! Reusable bindings for the example contracts.
//!
//! One module per contract ([`counter`], [`ping_pong`], [`vault`]); each contains per-VM
//! submodules (`cw`, `evm`, `svm`, `tron`) gated behind the matching VM feature. Module names
//! match the `cw_*` / `evm_*` / `svm_*` / `tron_*` hook prefixes used by `#[cross_vm_contract]`.

pub mod counter;
pub mod ping_pong;
pub mod vault;
