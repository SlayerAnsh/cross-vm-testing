//! Property-style testing over a user-defined `(Ctx, World)` pair.
//!
//! A developer implements one [`Harness`] over two cleanly separated pieces:
//! - [`Ctx`](Harness::Ctx): the live system-under-test, threaded by `&mut` through every step.
//! - [`World`](Harness::World): persisted bookkeeping only (an in-memory shadow model, flags, and
//!   any identifiers learned so far).
//!
//! Plus an `Operation` enum, an `Invariant` enum, and the functions that apply an operation,
//! generate operations, and check invariants. `Ctx` is the live system-under-test, `World` is
//! persisted bookkeeping; the developer builds both and loads them into a mode-typed [`Runner`]
//! with [`Runner::setup`]. That single harness implementation then drives every run mode:
//!
//! - **Fuzz** ([`FuzzRunner`]): a short random sequence over the loaded ctx+world; the
//!   `#[fuzz_runner]` attribute fans one `#[tokio::test]` out per case, each with its own setup.
//! - **Invariant** ([`InvariantRunner`]): one long random sequence, invariants after each op.
//! - **Endurance** ([`EnduranceRunner`]): random ops at random wall-clock times for a fixed
//!   duration, with block progression and a final invariant sweep.
//! - **Scenario** ([`ScenarioRunner`]): [`Runner::run_case`] / [`Runner::run_scenario`] run a
//!   concrete operation (or sequence), so an `#[rstest] #[values(..)]` test fans out a grid at the
//!   test layer.
//!
//! Because `World` stores identifiers rather than live handles, a handle is rebuilt on demand from
//! `Ctx` plus a stored identifier; this is what lets a subject that deploys another subject be
//! tested (record the child's identifier in `World`, rebuild a handle later).
//!
//! The same `apply` is reused by every mode: no test logic is written twice. An invariant whose
//! precondition has not occurred yet returns [`CheckOutcome::Skipped`] instead of failing.

mod outcome;
mod rng;
mod runner;
mod stats;

pub use outcome::{
    CheckOutcome, Coverage, Failure, FailureKind, HarnessError, InvCoverage, RunReport, Verdict,
    Violation,
};
#[cfg(feature = "fuzz")]
pub use rng::sample_arbitrary;
pub use rng::{random_seed, sub_seed, Prng};
pub use runner::{
    Endurance, EnduranceConfig, EnduranceRunner, Expectation, Fuzz, FuzzRunner, Invariant,
    InvariantRunner, KindMix, RunMode, Runner, Scenario, ScenarioRunner, ScenarioStep, Sequential,
    DEFAULT_SHRINK_LIMIT,
};
pub use stats::{op_label, OpStat, Stats};

/// A developer-defined property-test subject. One implementation drives fuzz, invariant,
/// endurance, and rstest-matrix runs.
///
/// The live system-under-test and the bookkeeping are kept apart:
/// - [`Ctx`](Self::Ctx) is the live system-under-test, threaded by `&mut` through every step.
/// - [`World`](Self::World) holds only **persisted state**: the in-memory shadow model, flags
///   (e.g. "any counter incremented yet"), and any identifiers learned so far. It holds no live
///   handles; a handle is rebuilt on demand from [`Ctx`](Self::Ctx) plus a stored identifier.
///
/// This split is what lets a subject that creates another subject be tested: `apply` reads the new
/// identifier (from a response or a query), records it in `World`, and a later `apply`/`check`
/// rebuilds a handle for it from `Ctx`.
///
/// # Transition invariants (state before vs after an op)
///
/// A transition-style invariant compares state before and after a single operation. No special
/// associated type or hook is needed: [`step`](Runner) runs `apply` then `check` for the same op,
/// so snapshot the pre-state **inside `apply`** (it is async and holds `Ctx`, so it can query the
/// system), stash it in `World`, and diff live post-state against it in `check`, returning
/// [`Held`](CheckOutcome::Held) / [`Violated`](CheckOutcome::Violated) (or
/// [`Skipped`](CheckOutcome::Skipped) when no snapshot applies).
#[allow(async_fn_in_trait)]
pub trait Harness {
    /// The live system-under-test the run operates against, threaded by `&mut` through every
    /// step. For a chain framework this is a started multi-chain environment; for a plain
    /// function or data structure it can simply be `()`. Kept apart from
    /// [`World`](Harness::World), which holds only persisted bookkeeping.
    type Ctx;

    /// Persisted per-run state: shadow model, flags, and learned identifiers. No live handles.
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
    /// Operates against the live `ctx` (building handles as needed) and updates the persisted
    /// `world`. Returns a [`Verdict`] classifying the SUT's response (accepted vs legitimately
    /// rejected). A confirmed bug or an infrastructure failure is an `Err` (see [`HarnessError`]).
    async fn apply(
        &self,
        ctx: &mut Self::Ctx,
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
        ctx: &mut Self::Ctx,
        world: &Self::World,
        inv: &Self::Invariant,
    ) -> CheckOutcome;

    /// Advance time/blocks between endurance operations. Defaults to a no-op: a harness whose
    /// context has a notion of time (chains, simulated clocks) overrides this; a pure-function
    /// harness ignores it.
    async fn advance(&self, ctx: &mut Self::Ctx, blocks: u64) -> Result<(), HarnessError> {
        let _ = (ctx, blocks);
        Ok(())
    }
}
