//! Reusable cross-VM contract *wrappers* (deploy / call / read) plus their property-testing
//! harnesses, one module per contract.
//!
//! Where [`mocks`](crate::mocks) holds the raw bindings (ABI, bytecode, message types,
//! discriminators), this module holds the logic that drives them: the [`counter`] module defines a
//! single `Counter` wrapper (via `#[cross_vm_contract]`) whose per-VM hooks are gated behind the
//! matching VM feature, plus the VM-agnostic `CounterHarness` scaffolding. Declaring the wrapper and
//! harness once here keeps every single-VM test crate (and the multi-chain `cross-vm-tests`) off a
//! hand-copied `Counter` + `Harness` impl.

pub mod counter;
