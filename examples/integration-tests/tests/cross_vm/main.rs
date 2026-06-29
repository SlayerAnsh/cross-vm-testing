//! Cross-VM tests: multi-chain `MultiChainEnv` setup, a cross-VM counter, and wallet derivation.
//!
//! One test binary aggregating the three modules; `support` is shared with the `harness`
//! group via a relative path so it is compiled, not duplicated.

#[path = "../support/mod.rs"]
mod support;

mod counter;
mod ping_pong;
mod setup;
mod wallet;
