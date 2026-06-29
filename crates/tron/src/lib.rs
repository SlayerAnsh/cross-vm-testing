//! Tron (TVM) chain provider for the cross-vm testing suite.
//!
//! Two backends behind the shared [`cross_vm_core::ChainProvider`] trait:
//!
//! * **Mock** ([`TronMockProvider`]): in-process `revm` with Tron-accurate addresses,
//!   precompiles, CREATE/CREATE2 derivation, and an energy/bandwidth accounting shim. This is
//!   the deterministic target for the property-testing harness.
//! * **RPC** ([`TronRpcProvider`]): java-tron stub parity for v1. Reads and address derivation
//!   work; writes return [`TronError::Unimplemented`] (there is no in-process TVM and no
//!   alloy-equivalent java-tron client yet).
//!
//! Design and protocol-fact citations live in
//! `docs/superpowers/specs/2026-06-29-tron-chain-support-design.md`.

mod error;

pub use error::TronError;
