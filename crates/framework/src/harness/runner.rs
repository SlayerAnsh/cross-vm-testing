//! Drives a [`Harness`] over an injected, already-live `(Ctx, World)`.
//!
//! The run *shape* is encoded in the runner's type via a phantom [`RunMode`] marker, mirroring the
//! [`MultiChainEnv<S>`](crate::MultiChainEnv) phase typestate: [`FuzzRunner`], [`InvariantRunner`],
//! [`EnduranceRunner`], and [`ScenarioRunner`] each expose only the driver method that mode needs.
//!
//! Construction is two-phase. A constructor ([`Runner::fuzz`], [`Runner::endurance`], ...) builds a
//! *shell* (harness + seed + seeded rng + mode); the developer then builds the live env+world
//! however the test needs (deploy, prime the model, establish op preconditions) and loads them with
//! [`Runner::setup`] before calling the driver. Setup is developer code, written per test, not a
//! trait hook, so each operation / combination / endurance test can set up differently. The
//! `#[fuzz_runner]` / `#[invariant_runner]` / `#[endurance_runner]` attribute macros inject a seeded
//! shell as a `#[runner]` argument and fan fuzz out into one `#[tokio::test]` per case.

use std::marker::PhantomData;
use std::time::Duration;

use tokio::time::{sleep, Instant};

use super::ctx::Ctx;
use super::outcome::{CheckOutcome, Failure, FailureKind, HarnessError, RunReport};
use super::{Harness, Prng};

/// A run-mode marker. The unit type parameter `M` on [`Runner`] selects which driver method is
/// available and labels the [`RunReport`].
pub trait RunMode {
    /// The [`RunReport::mode`] label runs in this mode report under.
    const LABEL: &'static str;
}

/// Fuzz a short random sequence per case; fanned out one test per case by `#[fuzz_runner]`.
pub struct Fuzz;
/// One long persisted random sequence, invariants after each op.
pub struct Invariant;
/// Random ops at random wall-clock delays until a deadline, then a final invariant sweep.
pub struct Endurance;
/// A fixed, concrete operation or sequence (rstest-matrix and replay entrypoint).
pub struct Scenario;

impl RunMode for Fuzz {
    const LABEL: &'static str = "fuzz";
}
impl RunMode for Invariant {
    const LABEL: &'static str = "invariant";
}
impl RunMode for Endurance {
    const LABEL: &'static str = "endurance";
}
impl RunMode for Scenario {
    const LABEL: &'static str = "case";
}

/// Modes that drive one random sequence over the loaded world. [`Fuzz`] and [`Invariant`] share the
/// driver verbatim; only [`RunMode::LABEL`] differs.
pub trait Sequential: RunMode {}
impl Sequential for Fuzz {}
impl Sequential for Invariant {}

/// Knobs for [`EnduranceRunner::run`] (the only driver with more than one tuning parameter).
#[derive(Debug, Clone)]
pub struct EnduranceConfig {
    /// Wall-clock duration to run for.
    pub duration: Duration,
    /// Maximum random delay inserted between operations.
    pub max_delay: Duration,
    /// Check invariants every N applied operations (`1` = after each, `0` = never mid-run).
    pub check_every: usize,
    /// Advance this many blocks between operations.
    pub advance_blocks: Option<u64>,
    /// Add up to this many extra random blocks per advance.
    pub block_jitter: u64,
}

impl EnduranceConfig {
    /// A config running for `duration` with sensible defaults (check after each op, no delay, no
    /// block progression).
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            max_delay: Duration::ZERO,
            check_every: 1,
            advance_blocks: None,
            block_jitter: 0,
        }
    }

    /// Set the maximum random inter-op delay.
    pub fn max_delay(mut self, d: Duration) -> Self {
        self.max_delay = d;
        self
    }

    /// Set the invariant-check cadence.
    pub fn check_every(mut self, n: usize) -> Self {
        self.check_every = n;
        self
    }

    /// Set block progression: `base` blocks per op plus up to `jitter` extra random blocks.
    pub fn advance_blocks(mut self, base: u64, jitter: u64) -> Self {
        self.advance_blocks = Some(base);
        self.block_jitter = jitter;
        self
    }
}

/// Drives a [`Harness`] over an injected `(Ctx, World)`. Build a shell with a mode constructor, load
/// state with [`setup`](Runner::setup), then call the mode's driver.
pub struct Runner<H: Harness, M: RunMode = Fuzz> {
    harness: H,
    ctx: Option<Ctx>,
    world: Option<H::World>,
    seed: u64,
    rng: Prng,
    _marker: PhantomData<M>,
}

/// `Runner<H, Fuzz>`: a short random sequence per case, fanned out by `#[fuzz_runner]`.
pub type FuzzRunner<H> = Runner<H, Fuzz>;
/// `Runner<H, Invariant>`: one long persisted random sequence.
pub type InvariantRunner<H> = Runner<H, Invariant>;
/// `Runner<H, Endurance>`: timed random ops with block progression and a final sweep.
pub type EnduranceRunner<H> = Runner<H, Endurance>;
/// `Runner<H, Scenario>`: a fixed operation or sequence (rstest matrix, replay).
pub type ScenarioRunner<H> = Runner<H, Scenario>;

impl<H: Harness, M: RunMode> Runner<H, M> {
    /// Build a shell: harness + seed + seeded rng, no env/world yet. Each public mode constructor
    /// delegates here.
    fn shell(harness: H, seed: u64) -> Self {
        Self {
            harness,
            ctx: None,
            world: None,
            seed,
            rng: Prng::seed_from_u64(seed),
            _marker: PhantomData,
        }
    }

    /// Load the live env and primed world. The one call a macro-driven test must add; returns
    /// `&mut Self` so it can chain into a driver (`runner.setup(ctx, world).run(..)`).
    pub fn setup(&mut self, ctx: Ctx, world: H::World) -> &mut Self {
        self.ctx = Some(ctx);
        self.world = Some(world);
        self
    }

    /// The base seed this runner's rng was seeded with. Read it in a test body so setup can vary
    /// its initial data per fuzz case (`build_world(r.seed())`).
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The operation rng, for setups that want randomized initial state off the same stream.
    pub fn rng(&mut self) -> &mut Prng {
        &mut self.rng
    }

    /// Borrow the underlying harness.
    pub fn harness(&self) -> &H {
        &self.harness
    }

    /// Borrow the loaded world. Panics if [`setup`](Runner::setup) has not been called.
    pub fn world(&self) -> &H::World {
        self.world.as_ref().expect(NOT_SET_UP)
    }

    /// Mutably borrow the loaded env, e.g. to poke a chain directly. Panics if not set up.
    pub fn ctx_mut(&mut self) -> &mut Ctx {
        self.ctx.as_mut().expect(NOT_SET_UP)
    }

    /// Recover the live env + world after a run (e.g. to hand off or chain a second run). Panics if
    /// [`setup`](Runner::setup) has not been called.
    pub fn into_parts(self) -> (Ctx, H::World) {
        (self.ctx.expect(NOT_SET_UP), self.world.expect(NOT_SET_UP))
    }
}

impl<H: Harness> Runner<H, Fuzz> {
    /// A fuzz shell seeded by `seed`. Load state with [`setup`](Runner::setup), then `run`.
    pub fn fuzz(harness: H, seed: u64) -> Self {
        Self::shell(harness, seed)
    }
}

impl<H: Harness> Runner<H, Invariant> {
    /// An invariant shell seeded by `seed`. Load state with [`setup`](Runner::setup), then `run`.
    pub fn invariant(harness: H, seed: u64) -> Self {
        Self::shell(harness, seed)
    }
}

impl<H: Harness> Runner<H, Endurance> {
    /// An endurance shell seeded by `seed`. Load state with [`setup`](Runner::setup), then `run`.
    pub fn endurance(harness: H, seed: u64) -> Self {
        Self::shell(harness, seed)
    }
}

impl<H: Harness> Runner<H, Scenario> {
    /// A scenario shell seeded by `seed`. Load state with [`setup`](Runner::setup), then
    /// [`run_case`](Runner::run_case) / [`run_scenario`](Runner::run_scenario).
    pub fn scenario(harness: H, seed: u64) -> Self {
        Self::shell(harness, seed)
    }
}

// ----- random-sequence driver (Fuzz + Invariant) -----

impl<H: Harness, M: Sequential> Runner<H, M> {
    /// Drive one random sequence of `ops` operations over the loaded env+world, drawing from
    /// `kinds` (or every kind via [`Harness::generate`] when `None`) and checking invariants per
    /// `check_every` (`0` = never mid-run). The report is labeled by the mode ([`RunMode::LABEL`]).
    pub async fn run(
        &mut self,
        ops: usize,
        kinds: Option<&[H::OpKind]>,
        check_every: usize,
    ) -> RunReport<H::Operation> {
        let Self {
            harness,
            ctx,
            world,
            seed,
            rng,
            ..
        } = self;
        let ctx = ctx.as_mut().expect(NOT_SET_UP);
        let world = world.as_mut().expect(NOT_SET_UP);

        let report = 'run: {
            let mut history = Vec::new();
            let mut skipped = 0usize;
            for i in 0..ops {
                let op = match kinds {
                    None => harness.generate(rng, world),
                    Some(ks) => {
                        let kind = ks[rng.index(ks.len())];
                        harness.generate_op(rng, world, kind)
                    }
                };
                history.push(op.clone());
                let do_check = check_every > 0 && (i + 1).is_multiple_of(check_every);
                match step(harness, ctx, world, &op, do_check).await {
                    Ok(n) => skipped += n,
                    Err(kind) => {
                        break 'run RunReport {
                            seed: *seed,
                            mode: M::LABEL,
                            steps: i + 1,
                            skipped,
                            failure: Some(Failure {
                                step: i + 1,
                                op: Some(op),
                                history,
                                kind,
                            }),
                        }
                    }
                }
            }
            RunReport {
                seed: *seed,
                mode: M::LABEL,
                steps: history.len(),
                skipped,
                failure: None,
            }
        };
        log_summary(&report);
        report
    }
}

// ----- endurance driver -----

impl<H: Harness> Runner<H, Endurance> {
    /// Apply random operations at random delays until `cfg.duration` elapses, optionally advancing
    /// blocks between ops, then run a final invariant sweep that catches drift since the last
    /// mid-run check.
    pub async fn run(&mut self, cfg: EnduranceConfig) -> RunReport<H::Operation> {
        let EnduranceConfig {
            duration,
            max_delay,
            check_every,
            advance_blocks,
            block_jitter,
        } = cfg;
        let Self {
            harness,
            ctx,
            world,
            seed,
            rng,
            ..
        } = self;
        let seed = *seed;
        let ctx = ctx.as_mut().expect(NOT_SET_UP);
        let world = world.as_mut().expect(NOT_SET_UP);

        let report = 'run: {
            let mut history = Vec::new();
            let deadline = Instant::now() + duration;
            let mut steps = 0usize;
            let mut skipped = 0usize;

            while Instant::now() < deadline {
                let op = harness.generate(rng, world);
                history.push(op.clone());
                steps += 1;
                let do_check = check_every > 0 && steps.is_multiple_of(check_every);
                match step(harness, ctx, world, &op, do_check).await {
                    Ok(n) => skipped += n,
                    Err(kind) => {
                        break 'run RunReport {
                            seed,
                            mode: Endurance::LABEL,
                            steps,
                            skipped,
                            failure: Some(Failure {
                                step: steps,
                                op: Some(op),
                                history,
                                kind,
                            }),
                        }
                    }
                }

                if let Some(base) = advance_blocks {
                    let extra = if block_jitter > 0 {
                        rng.below(block_jitter as u128 + 1) as u64
                    } else {
                        0
                    };
                    if let Err(e) = harness.advance(ctx, base + extra).await {
                        break 'run infra_report(
                            seed,
                            Endurance::LABEL,
                            steps,
                            skipped,
                            history,
                            e,
                        );
                    }
                }

                if !max_delay.is_zero() {
                    let ms = rng.below(max_delay.as_millis() + 1) as u64;
                    if ms > 0 {
                        sleep(Duration::from_millis(ms)).await;
                    }
                }
            }

            // Final invariant sweep: catches drift accumulated since the last mid-run check.
            for inv in harness.invariants() {
                match harness.check(ctx, world, &inv).await {
                    CheckOutcome::Held => {}
                    CheckOutcome::Skipped(_) => skipped += 1,
                    CheckOutcome::Violated(v) => {
                        break 'run RunReport {
                            seed,
                            mode: Endurance::LABEL,
                            steps,
                            skipped,
                            failure: Some(Failure {
                                step: steps,
                                op: history.last().cloned(),
                                history,
                                kind: FailureKind::Invariant {
                                    name: format!("{inv:?}"),
                                    detail: v.detail,
                                },
                            }),
                        }
                    }
                }
            }

            RunReport {
                seed,
                mode: Endurance::LABEL,
                steps,
                skipped,
                failure: None,
            }
        };
        log_summary(&report);
        report
    }
}

// ----- scenario driver (fixed operations) -----

impl<H: Harness> Runner<H, Scenario> {
    /// Run a single concrete operation over the loaded env+world, checking invariants after it.
    pub async fn run_case(&mut self, op: H::Operation) -> RunReport<H::Operation> {
        self.run_fixed(vec![op], Scenario::LABEL).await
    }

    /// Run a fixed operation sequence, checking invariants after each.
    pub async fn run_scenario(&mut self, ops: Vec<H::Operation>) -> RunReport<H::Operation> {
        self.run_fixed(ops, Scenario::LABEL).await
    }

    /// Replay a recorded history deterministically (seed the runner with the recorded seed first).
    /// Reproduces a failure a [`RunReport`] reported; the canonical way to turn a fuzz failure into
    /// a regression test.
    pub async fn replay(&mut self, history: Vec<H::Operation>) -> RunReport<H::Operation> {
        self.run_fixed(history, "replay").await
    }

    async fn run_fixed(
        &mut self,
        ops: Vec<H::Operation>,
        mode: &'static str,
    ) -> RunReport<H::Operation> {
        let Self {
            harness,
            ctx,
            world,
            seed,
            ..
        } = self;
        let seed = *seed;
        let ctx = ctx.as_mut().expect(NOT_SET_UP);
        let world = world.as_mut().expect(NOT_SET_UP);

        let report = 'run: {
            let mut history = Vec::new();
            let mut skipped = 0usize;
            for (i, op) in ops.into_iter().enumerate() {
                history.push(op.clone());
                match step(harness, ctx, world, &op, true).await {
                    Ok(n) => skipped += n,
                    Err(kind) => {
                        break 'run RunReport {
                            seed,
                            mode,
                            steps: i + 1,
                            skipped,
                            failure: Some(Failure {
                                step: i + 1,
                                op: Some(op),
                                history,
                                kind,
                            }),
                        }
                    }
                }
            }
            RunReport {
                seed,
                mode,
                steps: history.len(),
                skipped,
                failure: None,
            }
        };
        log_summary(&report);
        report
    }
}

// ----- shared per-operation step -----

/// Apply one operation and, if `check`, verify every invariant against the resulting state. Returns
/// the number of invariants that [`CheckOutcome::Skipped`] this step; maps a [`HarnessError`] or a
/// [`CheckOutcome::Violated`] into a [`FailureKind`].
///
/// A free function (not a method) so it borrows the harness disjointly from the runner's `ctx` and
/// `world` fields.
async fn step<H: Harness>(
    harness: &H,
    ctx: &mut Ctx,
    world: &mut H::World,
    op: &H::Operation,
    check: bool,
) -> Result<usize, FailureKind> {
    tracing::debug!(op = ?op, check, "apply op");
    match harness.apply(ctx, world, op).await {
        Ok(verdict) => tracing::debug!(?verdict, op = ?op, "op applied"),
        Err(HarnessError::Bug(m)) => {
            tracing::debug!(op = ?op, detail = %m, "op surfaced a bug");
            return Err(FailureKind::Bug(m));
        }
        Err(HarnessError::Infra(e)) => {
            tracing::debug!(op = ?op, error = %e, "op infra error");
            return Err(FailureKind::Infra(e.to_string()));
        }
    }
    let mut skipped = 0usize;
    if check {
        for inv in harness.invariants() {
            match harness.check(ctx, world, &inv).await {
                CheckOutcome::Held => tracing::debug!(invariant = ?inv, "invariant held"),
                CheckOutcome::Skipped(why) => {
                    tracing::debug!(invariant = ?inv, reason = %why, "invariant skipped");
                    skipped += 1;
                }
                CheckOutcome::Violated(v) => {
                    tracing::debug!(invariant = ?inv, detail = %v.detail, "invariant violated");
                    return Err(FailureKind::Invariant {
                        name: format!("{inv:?}"),
                        detail: v.detail,
                    });
                }
            }
        }
    }
    Ok(skipped)
}

/// Emit a one-line end-of-run summary (op count, skips, pass/fail) at `info` level. Sits above the
/// per-op `debug` logs so a run's totals show under `RUST_LOG=cross_vm_framework=info` without the
/// per-operation spam. Called once at every driver exit via a labeled-block funnel.
fn log_summary<Op>(report: &RunReport<Op>) {
    match &report.failure {
        None => tracing::info!(
            mode = report.mode,
            seed = report.seed,
            steps = report.steps,
            skipped = report.skipped,
            "run passed"
        ),
        Some(f) => tracing::warn!(
            mode = report.mode,
            seed = report.seed,
            steps = report.steps,
            skipped = report.skipped,
            failed_step = f.step,
            kind = ?f.kind,
            "run failed"
        ),
    }
}

/// Build a [`RunReport`] for an infrastructure failure raised mid-run (endurance `advance`).
fn infra_report<Op>(
    seed: u64,
    mode: &'static str,
    steps: usize,
    skipped: usize,
    history: Vec<Op>,
    e: HarnessError,
) -> RunReport<Op>
where
    Op: Clone,
{
    RunReport {
        seed,
        mode,
        steps,
        skipped,
        failure: Some(Failure {
            step: steps,
            op: history.last().cloned(),
            history,
            kind: FailureKind::Infra(e.to_string()),
        }),
    }
}

const NOT_SET_UP: &str = "Runner::setup(ctx, world) must be called before running";
