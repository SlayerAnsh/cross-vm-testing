//! Config-driven CLI, harness registry, and run pipeline over `harness-core`
//! and `harness-config`. Use raw via [`GenericDomain`], or implement
//! [`CliDomain`] to add domain config sections, CLI flags, and a custom setup
//! request type; see the cross-vm framework crate for a worked example.

mod artifact;
mod cli;
mod domain;
mod erased;
mod registry;
mod report;
mod resolve;
pub mod test_bridge;
#[cfg(test)]
mod test_mock;

pub use artifact::write_replay_artifact;
pub use cli::{
    build_run_options, combine, exit_code_for, exit_code_for_run_error, overrides_json,
    select_phases, Cli, PhasePlan,
};
pub use domain::{BasicSetup, CliDomain, GenericDomain, NoArgs, SetupBuildError, SetupFuture};
pub use erased::{ErasedFailure, ErasedReport};
pub use harness_core::OpDoc;
pub use registry::{HarnessInfo, MakeSetup, Registry, RunError, ValidationError};
pub use report::{write_json_report, Invocation, JsonReport};
pub use resolve::{resolve_profile, ResolvedProfile, RunOptions};
