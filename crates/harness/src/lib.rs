//! Property-style testing over a user-defined `(Ctx, World)` pair.
//!
//! A developer describes one system under test over two cleanly separated pieces:
//! - [`Ctx`](Harness::Ctx): the live system-under-test, threaded by `&mut` through every step.
//! - [`World`](Harness::World): persisted bookkeeping only (an in-memory shadow model, flags, and
//!   any identifiers learned so far).
//!
//! Operations are standalone structs: each implements [`DynOp`] (its data plus its own `apply`)
//! and registers into an [`OpSetHarness`] through one [`OpDef`] (kind name, generator, decoder,
//! optional weight); invariants follow the same shape via [`DynInvariant`]. The assembled
//! `OpSetHarness` is the value the developer builds and loads into a mode-typed [`Runner`] with
//! [`Runner::setup`]. That single harness then drives every run mode:
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
//!
//! [`OpSetHarness`] implements the [`Harness`] trait (its `Operation` is a boxed [`DynOp`], its
//! `OpKind` is the `&'static str` kind name), which is the runner seam every mode, shrinking, and
//! replay drive through. Most developers never implement [`Harness`] directly; advanced users who
//! need a hand-written runner contract still can. See `tests/opset.rs` for a complete worked
//! example, and [`ConfigOps`] for the codec that carries a harness through config and CLI runs.

mod opset;
mod outcome;
mod rng;
mod runner;
mod stats;

#[cfg(feature = "macros")]
pub use harness_core_macros::{endurance_runner, fuzz_runner, invariant_runner};
pub use opset::{
    decode_json_op, AdvanceFn, ConfigOps, DecodeFn, DynInvariant, DynOp, DynOperation, GenerateFn,
    OpDef, OpFuture, OpSetHarness, WeightFn,
};
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

/// The runner seam for a property-test subject: one value drives fuzz, invariant, endurance, and
/// rstest-matrix runs. Most developers never implement this trait directly; they assemble an
/// [`OpSetHarness`] from [`DynOp`] structs, which implements it. Implement it by hand only to pin
/// a custom runner contract.
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
    /// replay; `Debug` for the failure dump. For [`OpSetHarness`] this is [`DynOperation`], a
    /// boxed [`DynOp`] whose `Debug` leads with the registered kind name.
    type Operation: Clone + core::fmt::Debug;

    /// One named property that must always hold, so a failure can name which broke. For
    /// [`OpSetHarness`] this is a boxed [`DynInvariant`].
    type Invariant: Clone + core::fmt::Debug;

    /// The set of operation *kinds* (an [`Operation`](Self::Operation) without its data), used to
    /// drive per-kind fuzzing and to restrict which kinds a combination run draws from. For
    /// [`OpSetHarness`] this is the `&'static str` kind name each [`OpDef`] is registered under.
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

    /// Every operation kind this harness can produce. The default candidate pool every random
    /// mode draws from (weighted by [`weight`](Self::weight)), and the set a `kinds`-restricted
    /// [`Runner`] run picks from. Must be non-empty.
    fn op_kinds(&self) -> Vec<Self::OpKind>;

    /// Build a random operation of exactly `kind`, filling its data from `rng` (state-aware via
    /// `world`, e.g. an amount `<=` a user's balance). This is the single data-generation
    /// primitive: every random mode picks a kind (weighted by [`weight`](Self::weight)) and
    /// calls this, and per-kind fuzzing fixes the kind and calls this repeatedly.
    fn generate_op(
        &self,
        rng: &mut Prng,
        world: &Self::World,
        kind: Self::OpKind,
    ) -> Self::Operation;

    /// Relative selection weight of `kind` for the current live state. The default is `1` for
    /// every kind (a uniform mix). Return `0` to exclude a kind from the draw while the state
    /// makes it meaningless (e.g. `Withdraw` before any deposit exists); it may become nonzero
    /// again later in the same run. Called freshly before every random draw, so the mix follows
    /// the `World` as it evolves.
    ///
    /// When a run also carries config-supplied static weights (`KindMix::Weighted`, the config
    /// `weights` table), the effective weight is `static * dynamic` (saturating); either side
    /// returning `0` excludes the kind. If every candidate kind's effective weight is `0` at a
    /// draw, the run fails with an `Infra` failure at that step.
    ///
    /// Must be deterministic in `(ctx, world, kind)` and must not mutate anything: it runs on
    /// the seed-pinned generation path, so a nondeterministic weight breaks same-seed
    /// reproduction. It receives no rng by design.
    fn weight(&self, ctx: &Self::Ctx, world: &Self::World, kind: Self::OpKind) -> u32 {
        let _ = (ctx, world, kind);
        1
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
