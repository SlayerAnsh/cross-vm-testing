//! Cross-VM contract abstraction.
//!
//! A developer wraps a contract once and implements only the per-VM logic they need. The
//! building blocks here are deliberately small; the per-VM dispatch lives in the wrapper
//! itself (see DEVELOPER.md), keeping this framework layer free of any encoding semantics.
//!
//! - [`Account`]: a VM-agnostic address (signer or deployed contract), with typed extractors.
//! - [`ContractBase`]: the shared chain handle + deployed address, plus typed chain accessors.
//! - [`AppResponse`]: the uniform return envelope (typed payload + raw per-VM result).
//! - [`Hooks`]: per-contract before/after callbacks a wrapper fires around each method.

mod account;
mod base;
mod hooks;
mod response;

pub use account::Account;
pub use base::ContractBase;
pub use hooks::{BeforeContext, HookContext, Hooks};
pub use response::{AppResponse, RawResponse};
