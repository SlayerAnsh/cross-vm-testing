//! Config-driven chain construction and profile resolution (the `cli` feature).
//!
//! Bridges the pure [`cross_vm_config`] schema (`RunConfig`, `ChainDecl`, `Profile`, ...) into
//! framework-native, chain-provider-aware types:
//!
//! - `setup_request`: [`ChainSpecData`] and [`SetupRequest`], the types a config-driven setup
//!   fn receives, plus the [`SetupFuture`] alias a registered setup fn returns.
//! - `build_chain`: [`build_chain()`] materializes one resolved [`ChainSpecData`] into an
//!   [`crate::AnyChain`], and [`parse_spec_id()`] parses the 15 revm hardfork short names
//!   (spec section 4.6).
//! - `resolve`: [`resolve_profile()`] resolves a loaded `RunConfig` plus a chosen profile name
//!   plus CLI-shaped overrides ([`RunOptions`]) into a runnable [`ResolvedProfile`], calling
//!   [`cross_vm_config::resolve_chain_target`] as the single target precedence funnel.
//! - `erased`: [`ErasedReport`]/[`ErasedFailure`], the mode-agnostic outcome of one profile run.
//! - `registry`: [`Registry`], the harness registry and type-erasure bridge (spec section 7).
//! - `report`: [`write_json_report`], the `--json-report` envelope writer (spec section 9).
//! - `artifact`: [`write_replay_artifact`], the replay-artifact writer (spec section 10).
//! - `test_bridge`: [`test_bridge::run_profile_for_test`], the `cargo test` bridge the
//!   `#[config_runner]` proc-macro (`cross-vm-macros`) expands into.
//!
//! No CLI argument parsing lives here; that is a later task that builds on top of this module.

mod artifact;
mod build_chain;
mod erased;
mod registry;
mod report;
mod resolve;
mod setup_request;
pub mod test_bridge;

pub use artifact::write_replay_artifact;
pub use build_chain::build_chain;
// `parse_spec_id` names `revm`'s `SpecId`, which now feature-gates behind `evm`/`tron`; re-export
// it only when one of those VMs is compiled in so a `cli,cw`/`cli,solana` build stays clean.
#[cfg(any(feature = "evm", feature = "tron"))]
pub use build_chain::parse_spec_id;
pub use erased::{ErasedFailure, ErasedReport};
pub use registry::{ConfigHarness, Registry, RunError, ValidationError};
pub use report::{write_json_report, Invocation, JsonReport};
pub use resolve::{resolve_profile, ResolvedProfile, RunOptions};
pub use setup_request::{ChainSpecData, SetupFuture, SetupRequest, Target};
