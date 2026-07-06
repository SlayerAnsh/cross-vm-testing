//! Single-VM (Tron/TVM) `Counter` adapter: re-exports the shared `Counter` wrapper + harness builder from
//! `cross-vm-common` and pins them to this crate's chain (the mock/rpc `TRON_NILE` preset under
//! `"tron"`).
//!
//! Everything VM-agnostic (the `Counter` wrapper, `counter_harness`, ops, world) now lives once in
//! [`cross_vm_common::contracts::counter`]; only the preset + label differ per crate, so this file
//! is just the two setup wrappers the harness/config/CLI entry points call.

pub use cross_vm_common::contracts::counter::{
    counter_harness, Counter, CounterWorld, Increment, IncrementTwice,
};

use cross_vm_common::contracts::counter::{config_setup_with, setup_on};
use cross_vm_framework::config::{SetupFuture, SetupRequest, Target};
use cross_vm_framework::prelude::*;

/// The chain label this single-VM harness deploys and operates on when no `[[chain]]` is declared.
const LABEL: &str = "tron";

/// Build the live env (counter deployed on one mock Tron chain) and the primed world. The
/// attribute-macro tests call this directly; deterministic, so `seed` is unused.
pub async fn counter_setup(_seed: u64) -> Result<(Ctx, CounterWorld), HarnessError> {
    setup_on(LABEL, |w| TRON_NILE.mock(w).into()).await
}

/// The config-driven counterpart of [`counter_setup`], registered with the `cross-vm` CLI. Falls
/// back to the mock (or rpc) `TRON_NILE` preset under `"tron"` when the TOML declares no `[[chain]]`.
pub fn counter_config_setup(req: SetupRequest) -> SetupFuture<'static, CounterWorld> {
    config_setup_with(req, LABEL, |target, w| match target {
        Target::Mock => TRON_NILE.mock(w).into(),
        Target::Rpc => TRON_NILE.rpc(w).into(),
    })
}
