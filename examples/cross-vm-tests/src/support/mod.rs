//! Shared support the `cross-vm` bin (and the vault harness) depend on: the cross-VM `Vault`
//! contract wrapper and wallet/funding helpers. The `counter`/`ping-pong`/`bridge` support (used
//! only by tests, never by the bin) stays in `tests/support/` and is re-exported through that
//! module's shim, not duplicated here.

pub mod vault;

pub use vault::Vault;

// The `Counter` wrapper + `CounterSpec` live once in `cross-vm-common` (shared with the single-VM
// example crates); re-export them so `src/counter.rs` (and the counter test file) can reach them
// through `crate::support` unchanged. Only these two names: `cross-vm-common`'s counter module also
// exports its own single-VM `CounterWorld`/`counter_harness`, which this crate must not shadow.
pub use cross_vm_common::contracts::counter::{Counter, CounterSpec};

// Wallet/funding helpers and the tracing initializer now live in `cross-vm-common` (shared by all
// example test crates); re-exported here so existing `support::` call-sites compile unchanged.
pub use cross_vm_common::init_tracing;
pub use cross_vm_common::wallets::{empty_wallets, fund_alice, fund_evm, fund_user, test_wallets};
