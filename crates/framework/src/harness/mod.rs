//! Property-style testing on top of the cross-VM contract wrappers.
//!
//! A developer implements one [`Harness`] over two cleanly separated pieces:
//! - [`Ctx`]: the live system-under-test, a started [`MultiChainEnv`](crate::MultiChainEnv).
//! - `World`: persisted bookkeeping only (an in-memory shadow model, flags, and the addresses
//!   of contracts deployed or discovered so far).
//!
//! Plus an `Operation` enum, an `Invariant` enum, and the functions that apply an operation,
//! generate operations, and check invariants. The developer builds the live `(Ctx, World)`
//! themselves (deploy, prime the model) and loads it into a mode-typed [`Runner`] with
//! [`Runner::setup`]; that single harness implementation then drives every run mode:
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
mod runner;

pub use classify::classify;
pub use ctx::Ctx;
pub use harness_core::{
    CheckOutcome, Coverage, Failure, FailureKind, HarnessError, InvCoverage, RunReport, Verdict,
    Violation,
};
#[cfg(feature = "fuzz")]
pub use harness_core::sample_arbitrary;
pub use harness_core::{op_label, random_seed, sub_seed, OpStat, Prng, Stats};
pub use runner::{
    Endurance, EnduranceConfig, EnduranceRunner, Expectation, Fuzz, FuzzRunner, Invariant,
    InvariantRunner, KindMix, RunMode, Runner, Scenario, ScenarioRunner, ScenarioStep, Sequential,
    DEFAULT_SHRINK_LIMIT,
};

/// A developer-defined property-test subject. One implementation drives fuzz, invariant,
/// endurance, and rstest-matrix runs.
///
/// The live system-under-test and the bookkeeping are kept apart:
/// - [`Ctx`] is the started [`MultiChainEnv`](crate::MultiChainEnv) (the SUT's chains and
///   wallets), threaded by `&mut` through every step.
/// - `World` holds only **persisted state**: the in-memory shadow model, flags
///   (e.g. "any counter incremented yet"), and the addresses of contracts deployed or
///   discovered so far. It holds no chains and no contract wrappers; a wrapper is rebuilt on
///   demand from `Ctx` plus a stored address (`Counter::instance(ctx.chain(label)?, addr)`).
///
/// This split is what lets a contract that creates another contract be tested: `apply` reads the
/// child's address (from the response events or a factory query), records it in `World`, and a
/// later `apply`/`check` rebuilds a handle for it from `Ctx`.
///
/// # Transition invariants (state before vs after an op)
///
/// A transition-style invariant compares on-chain state before and after a single operation. No
/// special associated type or hook is needed: [`step`](crate::harness::Runner) runs `apply` then
/// `check` for the same op, so snapshot the pre-state **inside `apply`** (it is async and holds
/// `Ctx`, so it can query a chain), stash it in `World`, and diff live post-state against it in
/// `check`, returning [`Held`](CheckOutcome::Held) / [`Violated`](CheckOutcome::Violated) (or
/// [`Skipped`](CheckOutcome::Skipped) when no snapshot applies). Do **not** use the sync
/// [`ContractBase`](crate::contract::ContractBase) before/after hooks for this: they are `FnMut`,
/// hold no chain handle, and cannot async-query state; they are for event/relay/indexer side-logic
/// only. The vault harness example demonstrates the `World`-stash pattern.
#[allow(async_fn_in_trait)]
pub trait Harness {
    /// Persisted per-run state: shadow model, flags, and learned addresses. No chains.
    type World;

    /// One complete developer-defined action (swap, deposit, increment, ...). `Clone` for
    /// replay; `Debug` for the failure dump.
    type Operation: Clone + core::fmt::Debug;

    /// One named property that must always hold. An enum so a failure can name which broke.
    type Invariant: Clone + core::fmt::Debug;

    /// The set of operation *kinds* (an [`Operation`](Self::Operation) without its data), used to
    /// drive per-kind fuzzing and to restrict which kinds a combination run draws from. Usually a
    /// small fieldless enum mirroring the `Operation` variants.
    type OpKind: Clone + Copy + core::fmt::Debug;

    /// Apply a single operation. The one function reused by every mode.
    ///
    /// Operates against the live `ctx` (building contract handles as needed) and updates the
    /// persisted `world`. Returns a [`Verdict`] classifying the SUT's response (accepted vs
    /// legitimately rejected). A confirmed bug or an infrastructure failure is an `Err` (see
    /// [`HarnessError`] and the [`classify`] helper).
    async fn apply(
        &self,
        ctx: &mut Ctx,
        world: &mut Self::World,
        op: &Self::Operation,
    ) -> Result<Verdict, HarnessError>;

    /// Every operation kind this harness can produce. The default pool [`generate`](Self::generate)
    /// draws from, and the set a `kinds`-restricted [`Runner`] run picks from. Must be non-empty.
    fn op_kinds(&self) -> Vec<Self::OpKind>;

    /// Build a random operation of exactly `kind`, filling its data from `rng` (state-aware via
    /// `world`, e.g. an amount `<=` a user's balance). This is the single data-generation
    /// primitive: [`generate`](Self::generate) picks a kind and calls this, and per-kind fuzzing
    /// fixes the kind and calls this repeatedly.
    fn generate_op(
        &self,
        rng: &mut Prng,
        world: &Self::World,
        kind: Self::OpKind,
    ) -> Self::Operation;

    /// Produce the next operation for the mixed modes. Defaults to a uniform choice over
    /// [`op_kinds`](Self::op_kinds) followed by [`generate_op`](Self::generate_op). Override only
    /// to bias the kind distribution (e.g. `rng.weighted(..)`); reuse `generate_op` for the data
    /// so generation logic is not duplicated.
    fn generate(&self, rng: &mut Prng, world: &Self::World) -> Self::Operation {
        let kinds = self.op_kinds();
        let kind = kinds[rng.index(kinds.len())];
        self.generate_op(rng, world, kind)
    }

    /// All invariants attached to this harness.
    fn invariants(&self) -> Vec<Self::Invariant>;

    /// Check one invariant against the current (post-operation) state.
    ///
    /// Returns a [`CheckOutcome`] so an invariant can declare itself
    /// [`Skipped`](CheckOutcome::Skipped) when its precondition has not happened yet (its
    /// state would make the check vacuous or invalid) instead of being forced to pass or fail.
    async fn check(
        &self,
        ctx: &mut Ctx,
        world: &Self::World,
        inv: &Self::Invariant,
    ) -> CheckOutcome;

    /// Advance time/blocks between endurance operations. Defaults to warping every chain in the
    /// environment, so a multi-chain harness needs no override.
    async fn advance(&self, ctx: &mut Ctx, blocks: u64) -> Result<(), HarnessError> {
        ctx.advance_all(blocks).await;
        Ok(())
    }
}
