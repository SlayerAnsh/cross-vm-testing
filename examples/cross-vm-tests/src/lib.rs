//! Integration & example tests for the cross-VM framework.
//!
//! The library surface (`support`, `vault`, `counter`) exists so the `cross-vm` bin
//! (`src/bin/cross_vm.rs`), which cannot see `tests/` or dev-dependencies, can register and drive
//! the vault and counter harnesses through the framework's config-driven CLI.
//! `tests/harness/vault.rs` and `tests/harness/counter.rs` re-import the same harnesses from here;
//! a shim in `tests/support/mod.rs` keeps every other existing test compiling unchanged. `boom` is
//! a second, tiny harness registered alongside `vault`, used only by `tests/cli_e2e.rs`'s
//! replay-artifact/shrink/`replay`-subcommand coverage (see its module docs for why).

pub mod boom;
pub mod counter;
pub mod support;
pub mod vault;
