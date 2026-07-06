//! Config-driven chain construction and the cross-vm CLI domain adapter (the `cli` feature).
//!
//! This module is a thin cross-vm layer over the generic [`harness_cli`] machinery. The generic
//! registry, profile resolution, type-erased outcome, JSON report, and replay-artifact writer all
//! live in `harness-cli` now and are re-exported here so downstream paths (for example
//! `cross_vm_framework::config::Registry`) stay stable. The cross-vm specifics are:
//!
//! - `setup_request`: [`ChainSpecData`] and [`SetupRequest`], the types a config-driven setup fn
//!   receives, plus the [`SetupFuture`] alias (over the generic shape, with the framework's `Ctx`
//!   fixed) a registered setup fn returns.
//! - `build_chain`: [`build_chain()`] materializes one resolved [`ChainSpecData`] into an
//!   [`crate::AnyChain`], and [`parse_spec_id()`] parses the revm hardfork short names.
//! - `domain`: [`CrossVmDomain`], the [`harness_cli::CliDomain`] implementation that adds the
//!   `--target`/`--target-chain` flags, rebuilds the chain-aware [`SetupRequest`] from the opaque
//!   env plus the `[[chain]]` declarations, and renders the replay artifact's `[[chain]]`/`[env]`
//!   sections.

mod build_chain;
mod domain;
mod setup_request;

pub use build_chain::build_chain;
// `parse_spec_id` names `revm`'s `SpecId`, which feature-gates behind `evm`/`tron`; re-export it
// only when one of those VMs is compiled in so a `cli,cw`/`cli,solana` build stays clean.
#[cfg(any(feature = "evm", feature = "tron"))]
pub use build_chain::parse_spec_id;
pub use domain::{CrossVmDomain, TargetArgs};
pub use setup_request::{ChainSpecData, SetupFuture, SetupRequest, Target};

// Generic machinery, re-exported from harness-cli so downstream paths stay stable
// (`cross_vm_framework::config::Registry`, etc.).
pub use harness_cli::{
    resolve_profile, write_json_report, write_replay_artifact, ErasedFailure, ErasedReport,
    Invocation, JsonReport, ResolvedProfile, RunError, RunOptions, ValidationError,
};

/// The cross-vm registry: harness-cli's registry fixed to [`SetupRequest`].
pub type Registry = harness_cli::Registry<SetupRequest>;

/// The `cargo test` bridge, fixed to the cross-vm domain (what `#[config_runner]` expands to;
/// keep this path stable).
pub mod test_bridge {
    /// See [`harness_cli::test_bridge::run_profile_for_test`]. Fixed to the cross-vm domain
    /// ([`CrossVmDomain`](super::CrossVmDomain)) so a `#[config_runner]`-generated `#[tokio::test]`
    /// loads a `*.cross-vm.toml` config and builds each run's [`SetupRequest`](super::SetupRequest)
    /// through the cross-vm chain resolution.
    pub async fn run_profile_for_test<H, F, S>(
        config_path: &str,
        harness: F,
        setup: S,
        profile: &str,
        case: Option<usize>,
        expected_cases: Option<usize>,
    ) where
        H: crate::harness::ConfigOps + crate::harness::Harness<Ctx = crate::harness::Ctx> + 'static,
        H::World: 'static,
        F: Fn() -> H + 'static,
        S: Fn(super::SetupRequest) -> super::SetupFuture<'static, H::World> + 'static,
    {
        harness_cli::test_bridge::run_profile_for_test::<super::CrossVmDomain, H, F, S>(
            config_path,
            harness,
            setup,
            profile,
            case,
            expected_cases,
        )
        .await
    }
}
