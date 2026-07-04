//! Config-driven CLI, harness registry, and run pipeline over `harness-core`
//! and `harness-config`. Use raw via [`GenericDomain`], or implement
//! [`CliDomain`] to add domain config sections, CLI flags, and a custom setup
//! request type; see the cross-vm framework crate for a worked example.

mod domain;
mod erased;
mod registry;
mod report;
mod resolve;

pub use domain::{BasicSetup, CliDomain, GenericDomain, NoArgs, SetupBuildError, SetupFuture};
pub use erased::{ErasedFailure, ErasedReport};
pub use registry::{MakeSetup, Registry, RunError, ValidationError};
pub use report::{write_json_report, Invocation, JsonReport};
pub use resolve::{resolve_profile, ResolvedProfile, RunOptions};
