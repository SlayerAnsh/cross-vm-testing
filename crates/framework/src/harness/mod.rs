//! Property-style testing on top of the cross-VM contract wrappers.
//!
//! The trait, runner, rng, stats, and outcome types live in the standalone [`harness_core`]
//! crate and are re-exported here unchanged; this module adds the cross-VM pieces: [`Ctx`]
//! (the started [`MultiChainEnv`](crate::MultiChainEnv) every cross-VM harness runs against)
//! and [`classify`] (the response classifier that knows a revert from an infra failure).
//!
//! A cross-VM harness sets `type Ctx = cross_vm_framework::harness::Ctx;` and, when it uses
//! endurance block progression, overrides [`Harness::advance`] with
//! `ctx.advance_all(blocks).await` (the generic default is a no-op).
//!
//! A developer implements one [`Harness`] over two cleanly separated pieces: [`Ctx`], the live
//! system-under-test (a started [`MultiChainEnv`](crate::MultiChainEnv)), and `World`, persisted
//! bookkeeping only (an in-memory shadow model, flags, and the addresses of contracts deployed or
//! discovered so far). The developer builds the live `(Ctx, World)` themselves (deploy, prime the
//! model) and loads it into a mode-typed [`Runner`] with [`Runner::setup`]; that single harness
//! implementation then drives every run mode:
//!
//! - **Fuzz** ([`FuzzRunner`]): a short random sequence over the loaded env+world; the
//!   `#[fuzz_runner]` attribute fans one `#[tokio::test]` out per case, each with its own setup.
//! - **Invariant** ([`InvariantRunner`]): one long random sequence, invariants after each op.
//! - **Endurance** ([`EnduranceRunner`]): random ops at random wall-clock times for a fixed
//!   duration, with block progression and a final invariant sweep.
//! - **Scenario** ([`ScenarioRunner`]): [`Runner::run_case`] / [`Runner::run_scenario`] run a
//!   concrete operation (or sequence), so an `#[rstest] #[values(..)]` test fans out a grid at the
//!   test layer (e.g. a 3x3 chain matrix).
//!
//! The harness never touches per-VM code; `apply` calls the developer's contract wrappers
//! (e.g. `counter.increment(wallet).await`), so adding a VM costs the harness nothing. Because
//! `World` stores addresses rather than live handles, a wrapper is rebuilt on demand from `Ctx`
//! (`Counter::instance(ctx.chain(label)?, addr)`); this is what lets a contract that deploys
//! another contract be tested (record the child's address in `World`, rebuild a handle later).
//!
//! The same `apply` is reused by every mode: no test logic is written twice. An invariant whose
//! precondition has not occurred yet returns [`CheckOutcome::Skipped`] instead of failing.

mod classify;
mod ctx;

pub use classify::classify;
pub use ctx::Ctx;
#[cfg(feature = "fuzz")]
pub use harness_core::sample_arbitrary;
pub use harness_core::{
    op_label, random_seed, sub_seed, CheckOutcome, Coverage, Endurance, EnduranceConfig,
    EnduranceRunner, Expectation, Failure, FailureKind, Fuzz, FuzzRunner, Harness, HarnessError,
    InvCoverage, Invariant, InvariantRunner, KindMix, OpStat, Prng, RunMode, RunReport, Runner,
    Scenario, ScenarioRunner, ScenarioStep, Sequential, Stats, Verdict, Violation,
    DEFAULT_SHRINK_LIMIT,
};
