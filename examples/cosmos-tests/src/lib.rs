//! Single-VM (CosmWasm) example tests for the cross-VM framework.
//!
//! One [`counter`] harness, driven three ways: the attribute-macro runners (`tests/harness.rs`),
//! the config-driven `#[config_runner]` fan-out (`tests/config_runner.rs`), and the `cross-vm` CLI
//! (`src/bin/cross_vm.rs`, exercised end to end by `tests/cli_e2e.rs`). The harness and its setups
//! live here in the library so the `cross-vm` bin, which cannot see `tests/` or dev-dependencies,
//! can register and drive them.

pub mod counter;
