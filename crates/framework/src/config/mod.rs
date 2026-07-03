//! Config-driven chain construction and profile resolution (the `cli` feature).
//!
//! Bridges the pure [`cross_vm_config`] schema (`RunConfig`, `ChainDecl`, `Profile`, ...) into
//! framework-native, chain-provider-aware types:
//!
//! - `setup_request`: [`ChainSpecData`] and [`SetupRequest`], the types a config-driven setup
//!   fn receives, plus the [`SetupFuture`] alias a registered setup fn returns.
//! - `build_chain`: [`build_chain()`] materializes one resolved [`ChainSpecData`] into an
//!   [`crate::AnyChain`], and [`parse_spec_id()`] parses the 15 revm hardfork short names
//!   (spec section 4.6).
//! - `resolve`: [`resolve_profile()`] resolves a loaded `RunConfig` plus a chosen profile name
//!   plus CLI-shaped overrides ([`RunOptions`]) into a runnable [`ResolvedProfile`], calling
//!   [`cross_vm_config::resolve_chain_target`] as the single target precedence funnel.
//!
//! No registry, type erasure, or CLI argument parsing lives here; those are later tasks that
//! build on top of this module.

mod build_chain;
mod resolve;
mod setup_request;

pub use build_chain::{build_chain, parse_spec_id};
pub use resolve::{resolve_profile, ResolvedProfile, RunOptions};
pub use setup_request::{ChainSpecData, SetupFuture, SetupRequest, Target};
