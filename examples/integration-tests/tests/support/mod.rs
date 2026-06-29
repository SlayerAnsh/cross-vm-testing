//! Shared test support, split by concern: the cross-VM `Counter` and `Vault` wrappers plus
//! wallet/funding helpers. Each integration-test binary pulls this in but uses a different
//! subset, so unused items are allowed.

#![allow(dead_code, unused_imports)]

mod bridge;
mod counter;
mod ping_pong;
mod vault;
mod wallets;

pub use bridge::{parse_packets, record_hook, Bridge, BridgeLedger, PacketEvent, PacketKind};
pub use counter::{Counter, CounterSpec};
pub use ping_pong::{PingPong, PingPongSpec, StatsView};
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
