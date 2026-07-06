//! Shared building blocks for the example test crates.
//!
//! [`mocks`] holds reusable contract bindings (ABI, creation bytecode, CosmWasm message types,
//! Solana program ids and discriminators, embedded artifacts), one module per contract, each split
//! into per-VM submodules gated behind the matching VM feature (`cw` / `evm` / `solana` / `tron`).
//! Declaring them once here keeps every test crate and deploy script off duplicated `sol!` blocks
//! and hand-copied discriminators.
//!
//! [`contracts`] holds the reusable contract *wrappers* and their harnesses (e.g. the `Counter`
//! wrapper + `counter_harness`), gated the same per-VM way so a single-VM crate compiles only its VM.
//!
//! [`wallets`] holds the shared wallet factory and per-VM funding helpers, and [`init_tracing`]
//! installs a libtest-friendly tracing subscriber. Both are ungated and used by every test crate.

pub mod contracts;
pub mod mocks;
pub mod wallets;

/// Install a tracing subscriber that routes through libtest's per-thread capture, so the
/// framework's per-op / per-invariant debug logs appear only under `--nocapture` / `--show-output`
/// and are never double-printed across parallel tests. Idempotent (first call wins), so every
/// harness setup can call it freely. Override the filter with `RUST_LOG`, e.g.
/// `RUST_LOG=cross_vm_framework=debug`; the default already enables framework debug logs.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let _ = fmt()
        .with_test_writer()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("cross_vm_framework=debug")),
        )
        .try_init();
}
