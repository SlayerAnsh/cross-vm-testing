//! Shared building blocks for the example test crates.
//!
//! [`mocks`] holds reusable contract bindings (ABI, creation bytecode, CosmWasm message types,
//! Solana program ids and discriminators, embedded artifacts), one module per contract, each split
//! into per-VM submodules gated behind the matching VM feature (`cw` / `evm` / `solana` / `tron`).
//! Declaring them once here keeps every test crate and deploy script off duplicated `sol!` blocks
//! and hand-copied discriminators.

pub mod mocks;
