//! Shared test support, split by concern: the cross-VM `Counter` and `Vault` wrappers plus
//! wallet/funding helpers. Each integration-test binary pulls this in but uses a different
//! subset, so unused items are allowed.
//!
//! This is a SHIM, not the source of truth for `Vault`/wallets/`init_tracing` any more (P2 vault
//! migration): those moved to the library crate (`src/support/`) so the `cross-vm` bin can reach
//! them too. `bridge`/`counter`/`ping_pong` are used only by tests, never by the bin, and stay
//! declared locally here. Re-exporting the moved items keeps every existing
//! `use crate::support::{...}` in the test tree compiling unchanged.

#![allow(dead_code, unused_imports)]

mod bridge;
mod counter;
mod ping_pong;

pub use bridge::{parse_packets, record_hook, Bridge, BridgeLedger, PacketEvent, PacketKind};
pub use counter::{Counter, CounterSpec};
pub use cross_vm_tests::support::{
    empty_wallets, fund_alice, fund_evm, fund_user, init_tracing, test_wallets, Vault,
};
pub use ping_pong::{PingPong, PingPongSpec, StatsView};
