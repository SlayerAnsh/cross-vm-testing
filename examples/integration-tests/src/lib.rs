//! Integration & example tests for the cross-VM framework.
//!
//! The library surface (`support`, `vault`) exists so the `cross-vm` bin (`src/bin/cross_vm.rs`),
//! which cannot see `tests/` or dev-dependencies, can register and drive the vault harness through
//! the framework's config-driven CLI. `tests/harness/vault.rs` re-imports the same harness from
//! here; a shim in `tests/support/mod.rs` keeps every other existing test compiling unchanged.

pub mod support;
pub mod vault;
