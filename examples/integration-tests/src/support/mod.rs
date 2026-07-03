//! Shared support the `cross-vm` bin (and the vault harness) depend on: the cross-VM `Vault`
//! contract wrapper and wallet/funding helpers. The `counter`/`ping-pong`/`bridge` support (used
//! only by tests, never by the bin) stays in `tests/support/` and is re-exported through that
//! module's shim, not duplicated here.

pub mod vault;
pub mod wallets;

pub use vault::Vault;
pub use wallets::{empty_wallets, fund_alice, fund_evm, fund_user, test_wallets};

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
