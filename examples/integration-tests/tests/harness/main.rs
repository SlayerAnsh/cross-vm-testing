//! Property-testing harness tests: runner mechanics, a multi-chain counter, and a DeFi vault.
//!
//! One test binary aggregating the three harness modules; `support` is shared with the
//! `cross_vm` group via a relative path so it is compiled, not duplicated.

#[path = "../support/mod.rs"]
mod support;

mod counter;
mod mechanics;
mod ping_pong;
mod vault;
