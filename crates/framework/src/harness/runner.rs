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
use super::outcome::{
    CheckOutcome, Coverage, Failure, FailureKind, HarnessError, RunReport, Verdict,
};
use super::stats::{op_label, OpOutcome, Stats};
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
    /// Minimum delay applied between every operation, before jitter. Guarantees a floor on
    /// inter-op spacing (the total delay is `base_delay + rand(0..=max_delay)`).
    pub base_delay: Duration,
    /// Maximum random jitter added on top of `base_delay` between operations.
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
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            check_every: 1,
            advance_blocks: None,
            block_jitter: 0,
        }
    }

    /// Set the minimum inter-op delay applied before jitter.
    pub fn base_delay(mut self, d: Duration) -> Self {
        self.base_delay = d;
        self
    }

    /// Set the maximum random inter-op jitter (added on top of `base_delay`).
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
    /// Opt-in per-op diagnostics; `None` unless [`with_stats`](Runner::with_stats) turned them on.
    stats: Option<Stats>,
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
            stats: None,
            _marker: PhantomData,
        }
    }

    /// Turn on opt-in per-op diagnostics ([`Stats`]): success/failure counts, `apply` timing, and an
    /// error breakdown, grouped by op variant name. Off by default; chainable like
    /// [`setup`](Runner::setup). When on, a per-op summary is logged at run end and
    /// [`stats`](Runner::stats) returns the collected data.
    pub fn with_stats(&mut self) -> &mut Self {
        self.stats = Some(Stats::default());
        self
    }

    /// The collected [`Stats`] if [`with_stats`](Runner::with_stats) was enabled, else `None`.
    pub fn stats(&self) -> Option<&Stats> {
        self.stats.as_ref()
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
            stats,
            ..
        } = self;
        let ctx = ctx.as_mut().expect(NOT_SET_UP);
        let world = world.as_mut().expect(NOT_SET_UP);
        // Fresh tallies per run: a report's stats always describe exactly this run.
        if let Some(s) = stats.as_mut() {
            *s = Stats::default();
        }
        let builder = ReportBuilder::new(*seed, M::LABEL, harness);

        // An empty kind slice can generate nothing: surface an infra failure instead of panicking
        // inside the rng (`None` is the "draw from every kind" spelling).
        if kinds.is_some_and(|ks| ks.is_empty()) {
            let report = builder.fail(
                0,
                None,
                FailureKind::Infra(
                    "empty op-kind slice (pass None to draw from every kind)".into(),
                ),
            );
            log_summary(&report, stats.as_ref());
            return report;
        }

        let source = OpSource::Generated {
            kinds,
            remaining: ops,
        };
        let report = drive(
            harness,
            ctx,
            world,
            rng,
            source,
            check_every,
            builder,
            stats.as_mut(),
        )
        .await;
        log_summary(&report, stats.as_ref());
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
            base_delay,
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
            stats,
            ..
        } = self;
        let seed = *seed;
        let ctx = ctx.as_mut().expect(NOT_SET_UP);
        let world = world.as_mut().expect(NOT_SET_UP);
        // Fresh tallies per run: a report's stats always describe exactly this run.
        if let Some(s) = stats.as_mut() {
            *s = Stats::default();
        }
        let mut builder = ReportBuilder::new(seed, Endurance::LABEL, harness);

        let report = 'run: {
            let deadline = Instant::now() + duration;
            let mut steps = 0usize;

            while Instant::now() < deadline {
                let op = harness.generate(rng, world);
                builder.history.push(op.clone());
                steps += 1;
                let do_check = check_every > 0 && steps.is_multiple_of(check_every);
                if let Err(kind) = step(
                    harness,
                    ctx,
                    world,
                    &op,
                    do_check,
                    &mut builder.coverage,
                    stats.as_mut(),
                )
                .await
                {
                    break 'run builder.fail(steps, Some(op), kind);
                }

                if let Some(base) = advance_blocks {
                    let extra = if block_jitter > 0 {
                        rng.below(block_jitter as u128 + 1) as u64
                    } else {
                        0
                    };
                    if let Err(e) = harness.advance(ctx, base + extra).await {
                        let last = builder.history.last().cloned();
                        break 'run builder.fail(steps, last, FailureKind::Infra(e.to_string()));
                    }
                }

                let jitter = if max_delay.is_zero() {
                    0
                } else {
                    rng.below(max_delay.as_millis() + 1) as u64
                };
                let ms = base_delay.as_millis() as u64 + jitter;
                if ms > 0 {
                    sleep(Duration::from_millis(ms)).await;
                }
            }

            // Final invariant sweep: catches drift accumulated since the last mid-run check.
            if let Err(kind) = sweep(harness, ctx, world, &mut builder.coverage).await {
                let last = builder.history.last().cloned();
                break 'run builder.fail(steps, last, kind);
            }

            builder.pass(steps)
        };
        log_summary(&report, stats.as_ref());
        report
    }
}

// ----- scenario driver (fixed operations) -----

impl<H: Harness> Runner<H, Scenario> {
    /// Run a single concrete operation over the loaded env+world, checking invariants after it.
    pub async fn run_case(&mut self, op: H::Operation) -> RunReport<H::Operation> {
        self.run_fixed(vec![op], Scenario::LABEL, 1).await
    }

    /// Run a fixed operation sequence, checking invariants after each.
    pub async fn run_scenario(&mut self, ops: Vec<H::Operation>) -> RunReport<H::Operation> {
        self.run_fixed(ops, Scenario::LABEL, 1).await
    }

    /// Replay a recorded history deterministically (seed the runner with the recorded seed first).
    /// Reproduces a failure a [`RunReport`] reported; the canonical way to turn a fuzz failure into
    /// a regression test.
    pub async fn replay(&mut self, history: Vec<H::Operation>) -> RunReport<H::Operation> {
        self.run_fixed(history, "replay", 1).await
    }

    /// Greedily shrink a known-failing sequence to a near-minimal one that still fails **the same
    /// way**, checking invariants after every op. Shorthand for
    /// [`shrink_with`](Runner::shrink_with) with `check_every = 1`; use `shrink_with` when the
    /// original failure surfaced under a sparser check cadence.
    pub async fn shrink<F, Fut>(
        &mut self,
        failing: Vec<H::Operation>,
        rebuild: F,
    ) -> Vec<H::Operation>
    where
        F: Fn() -> Fut,
        Fut: core::future::Future<Output = (Ctx, H::World)>,
    {
        self.shrink_with(failing, 1, rebuild).await
    }

    /// Greedily shrink a known-failing sequence to a near-minimal one that still fails **the same
    /// way** under the given invariant-check cadence. A generic delta-debug pass: it drops ops
    /// (windows first, then one at a time), re-drives the runner on a fresh `(Ctx, World)` from
    /// `rebuild` for each attempt, and keeps a candidate only if it still fails the same way:
    /// a `Bug` must carry the same detail and an invariant failure the same invariant name, so
    /// shrinking never converges on a *different* bug. Emit stable,
    /// state-independent bug messages from `apply` to get the most out of this.
    ///
    /// `check_every` replays candidates under the cadence the original failure surfaced with
    /// (`1` = check after every op, `0` = never mid-run), so a cadence-dependent invariant
    /// failure is not re-judged under a stricter schedule.
    ///
    /// Replay attempts are capped at [`DEFAULT_SHRINK_LIMIT`]; on exhaustion the best sequence
    /// found so far is returned. Runs are diagnostics-free: any [`Stats`] the runner collects are
    /// parked for the duration so shrink replays neither pollute tallies nor spam summaries.
    ///
    /// `rebuild` yields a fresh live env + primed world for each replay attempt; mock chains are
    /// cheap to rebuild. It is async because in-tree env setup deploys contracts.
    ///
    /// Returns the minimized sequence. If `failing` does not actually fail, it is returned unchanged.
    ///
    /// ```ignore
    /// let min = runner.shrink_with(report.failure.unwrap().history, check_every, || async {
    ///     vault_setup(seed).await.expect("setup")
    /// }).await;
    /// ```
    pub async fn shrink_with<F, Fut>(
        &mut self,
        failing: Vec<H::Operation>,
        check_every: usize,
        rebuild: F,
    ) -> Vec<H::Operation>
    where
        F: Fn() -> Fut,
        Fut: core::future::Future<Output = (Ctx, H::World)>,
    {
        // Park stats for the whole shrink: replays are throwaway diagnostics-wise, and the caller's
        // tallies must keep describing the run they came from.
        let parked = self.stats.take();
        let minimized = self.shrink_inner(failing, check_every, &rebuild).await;
        self.stats = parked;
        minimized
    }

    async fn shrink_inner<F, Fut>(
        &mut self,
        failing: Vec<H::Operation>,
        check_every: usize,
        rebuild: &F,
    ) -> Vec<H::Operation>
    where
        F: Fn() -> Fut,
        Fut: core::future::Future<Output = (Ctx, H::World)>,
    {
        // Establish the reference failure the shrink must preserve.
        let (ctx, world) = rebuild().await;
        self.setup(ctx, world);
        let ref_kind = match self
            .run_fixed(failing.clone(), Scenario::LABEL, check_every)
            .await
            .failure
        {
            Some(f) => f.kind,
            None => return failing, // Doesn't fail: nothing to shrink.
        };

        let mut current = failing;
        let mut budget = DEFAULT_SHRINK_LIMIT;

        // Chunked pass: remove contiguous windows, largest first (classic ddmin coarse phase).
        let mut size = current.len() / 2;
        while size >= 1 && current.len() > 1 {
            let mut start = 0;
            while start < current.len() {
                let end = (start + size).min(current.len());
                let candidate: Vec<H::Operation> = current[..start]
                    .iter()
                    .chain(&current[end..])
                    .cloned()
                    .collect();
                if !candidate.is_empty() {
                    let Some(fails) = self
                        .try_candidate(&candidate, &ref_kind, check_every, rebuild, &mut budget)
                        .await
                    else {
                        return current; // Budget exhausted: best so far.
                    };
                    if fails {
                        current = candidate;
                        // Keep `start`; the window now spans the ops that followed the removed block.
                        continue;
                    }
                }
                start += size;
            }
            size /= 2;
        }

        // Single-op pass: drop one op at a time, retrying the same index after a successful drop.
        let mut i = 0;
        while i < current.len() && current.len() > 1 {
            let mut candidate = current.clone();
            candidate.remove(i);
            let Some(fails) = self
                .try_candidate(&candidate, &ref_kind, check_every, rebuild, &mut budget)
                .await
            else {
                return current; // Budget exhausted: best so far.
            };
            if fails {
                current = candidate;
                continue;
            }
            i += 1;
        }
        current
    }

    /// Run one shrink candidate against the budget: `None` when the budget is exhausted, else
    /// `Some(fails_the_same_way)`. Each attempt re-drives on a fresh env from `rebuild`.
    async fn try_candidate<F, Fut>(
        &mut self,
        candidate: &[H::Operation],
        ref_kind: &FailureKind,
        check_every: usize,
        rebuild: &F,
        budget: &mut usize,
    ) -> Option<bool>
    where
        F: Fn() -> Fut,
        Fut: core::future::Future<Output = (Ctx, H::World)>,
    {
        if *budget == 0 {
            tracing::warn!(
                limit = DEFAULT_SHRINK_LIMIT,
                "shrink replay budget exhausted; returning best sequence found so far"
            );
            return None;
        }
        *budget -= 1;
        let (ctx, world) = rebuild().await;
        self.setup(ctx, world);
        let fails = match self
            .run_fixed(candidate.to_vec(), Scenario::LABEL, check_every)
            .await
            .failure
        {
            Some(f) => same_failure(&f.kind, ref_kind),
            None => false,
        };
        Some(fails)
    }

    /// Run a fixed sequence and, if it fails, auto-shrink it: the returned report's
    /// [`Failure::history`] is the minimized sequence. A pass-through when the run passes.
    /// Shorthand for [`run_and_shrink_with`](Runner::run_and_shrink_with) with `check_every = 1`.
    pub async fn run_and_shrink<F, Fut>(
        &mut self,
        ops: Vec<H::Operation>,
        rebuild: F,
    ) -> RunReport<H::Operation>
    where
        F: Fn() -> Fut,
        Fut: core::future::Future<Output = (Ctx, H::World)>,
    {
        self.run_and_shrink_with(ops, 1, rebuild).await
    }

    /// [`run_and_shrink`](Runner::run_and_shrink) under an explicit invariant-check cadence: the
    /// run, every shrink replay, and the final re-drive all check per `check_every`, so the
    /// minimized history reproduces under the same schedule the failure surfaced with.
    pub async fn run_and_shrink_with<F, Fut>(
        &mut self,
        ops: Vec<H::Operation>,
        check_every: usize,
        rebuild: F,
    ) -> RunReport<H::Operation>
    where
        F: Fn() -> Fut,
        Fut: core::future::Future<Output = (Ctx, H::World)>,
    {
        let report = self.run_fixed(ops, Scenario::LABEL, check_every).await;
        if report.passed() {
            return report;
        }
        let failing = report.failure.as_ref().unwrap().history.clone();
        let minimized = self.shrink_with(failing, check_every, &rebuild).await;
        // Re-drive the minimized sequence once on a fresh env for a clean, minimal report.
        let (ctx, world) = rebuild().await;
        self.setup(ctx, world);
        let mut final_report = self
            .run_fixed(minimized.clone(), Scenario::LABEL, check_every)
            .await;
        if let Some(f) = final_report.failure.as_mut() {
            f.history = minimized;
        }
        final_report
    }

    async fn run_fixed(
        &mut self,
        ops: Vec<H::Operation>,
        mode: &'static str,
        check_every: usize,
    ) -> RunReport<H::Operation> {
        let Self {
            harness,
            ctx,
            world,
            seed,
            rng,
            stats,
            ..
        } = self;
        let ctx = ctx.as_mut().expect(NOT_SET_UP);
        let world = world.as_mut().expect(NOT_SET_UP);
        // Fresh tallies per run: a report's stats always describe exactly this run.
        if let Some(s) = stats.as_mut() {
            *s = Stats::default();
        }
        let builder = ReportBuilder::new(*seed, mode, harness);
        let report = drive(
            harness,
            ctx,
            world,
            rng,
            OpSource::Fixed(ops.into_iter()),
            check_every,
            builder,
            stats.as_mut(),
        )
        .await;
        log_summary(&report, stats.as_ref());
        report
    }
}

// ----- shared driver machinery -----

/// Accumulates the pieces every driver builds a [`RunReport`] from (seed, mode label, coverage,
/// history), with exactly two exits: [`pass`](ReportBuilder::pass) and
/// [`fail`](ReportBuilder::fail). The single construction path keeps the `skipped ==
/// coverage.total_skipped()` and `Failure::step == steps` invariants in one place instead of at
/// every driver's every exit.
struct ReportBuilder<Op> {
    seed: u64,
    mode: &'static str,
    coverage: Coverage,
    history: Vec<Op>,
}

impl<Op: Clone> ReportBuilder<Op> {
    /// A builder with coverage pre-seeded from the harness's invariants (so a never-checked
    /// invariant still shows an all-zero total) and an empty history.
    fn new<H: Harness>(seed: u64, mode: &'static str, harness: &H) -> Self {
        Self {
            seed,
            mode,
            coverage: Coverage::seed(harness.invariants().iter().map(|i| format!("{i:?}"))),
            history: Vec::new(),
        }
    }

    /// The passing report after `steps` applied operations.
    fn pass(self, steps: usize) -> RunReport<Op> {
        RunReport {
            seed: self.seed,
            mode: self.mode,
            steps,
            skipped: self.coverage.total_skipped(),
            coverage: self.coverage,
            failure: None,
        }
    }

    /// The failing report: failed at step `steps` on `op` (if one was in flight) with `kind`,
    /// carrying the full replayable history.
    fn fail(self, steps: usize, op: Option<Op>, kind: FailureKind) -> RunReport<Op> {
        RunReport {
            seed: self.seed,
            mode: self.mode,
            steps,
            skipped: self.coverage.total_skipped(),
            coverage: self.coverage,
            failure: Some(Failure {
                step: steps,
                op,
                history: self.history,
                kind,
            }),
        }
    }
}

/// Where a driver's next operation comes from: freshly generated (fuzz / invariant) or a fixed
/// list (scenario / replay / shrink candidates). Generated draws preserve the exact rng order the
/// modes have always used (kind index first when restricted, then op data), which is what keeps
/// recorded seeds reproducing across releases (pinned by the golden-seed test in mechanics.rs).
enum OpSource<'a, H: Harness> {
    Generated {
        kinds: Option<&'a [H::OpKind]>,
        remaining: usize,
    },
    Fixed(std::vec::IntoIter<H::Operation>),
}

impl<H: Harness> OpSource<'_, H> {
    fn next(&mut self, harness: &H, rng: &mut Prng, world: &H::World) -> Option<H::Operation> {
        match self {
            OpSource::Generated { kinds, remaining } => {
                if *remaining == 0 {
                    return None;
                }
                *remaining -= 1;
                Some(match kinds {
                    None => harness.generate(rng, world),
                    Some(ks) => {
                        let kind = ks[rng.index(ks.len())];
                        harness.generate_op(rng, world, kind)
                    }
                })
            }
            OpSource::Fixed(iter) => iter.next(),
        }
    }
}

/// The shared sequence driver behind the Fuzz/Invariant `run` and the Scenario `run_fixed`:
/// pull ops from `source`, apply each via [`step`] under the `check_every` cadence, and exit
/// through the builder on the first failure. Endurance keeps its own loop (wall-clock deadline,
/// inter-op delays, block advance) but shares [`ReportBuilder`], [`step`], and [`sweep`].
#[allow(clippy::too_many_arguments)]
async fn drive<H: Harness>(
    harness: &H,
    ctx: &mut Ctx,
    world: &mut H::World,
    rng: &mut Prng,
    mut source: OpSource<'_, H>,
    check_every: usize,
    mut builder: ReportBuilder<H::Operation>,
    mut stats: Option<&mut Stats>,
) -> RunReport<H::Operation> {
    let mut steps = 0usize;
    while let Some(op) = source.next(harness, rng, world) {
        builder.history.push(op.clone());
        steps += 1;
        let do_check = check_every > 0 && steps.is_multiple_of(check_every);
        if let Err(kind) = step(
            harness,
            ctx,
            world,
            &op,
            do_check,
            &mut builder.coverage,
            stats.as_deref_mut(),
        )
        .await
        {
            return builder.fail(steps, Some(op), kind);
        }
    }
    builder.pass(steps)
}

/// Check every invariant against the current state, recording each outcome into `coverage`.
/// The first violation is the error. Shared by [`step`]'s per-op checking and the endurance
/// final sweep.
async fn sweep<H: Harness>(
    harness: &H,
    ctx: &mut Ctx,
    world: &mut H::World,
    coverage: &mut Coverage,
) -> Result<(), FailureKind> {
    for inv in harness.invariants() {
        let name = format!("{inv:?}");
        match harness.check(ctx, world, &inv).await {
            CheckOutcome::Held => {
                coverage.record_held(&name);
                tracing::debug!(invariant = ?inv, "invariant held");
            }
            CheckOutcome::Skipped(why) => {
                coverage.record_skipped(&name);
                tracing::debug!(invariant = ?inv, reason = %why, "invariant skipped");
            }
            CheckOutcome::Violated(v) => {
                coverage.record_violated(&name);
                tracing::debug!(invariant = ?inv, detail = %v.detail, "invariant violated");
                return Err(FailureKind::Invariant {
                    name,
                    detail: v.detail,
                });
            }
        }
    }
    Ok(())
}

// ----- shared per-operation step -----

/// Apply one operation and, if `check`, verify every invariant against the resulting state. Records
/// each invariant outcome into `coverage` (keyed by its `Debug` name) and, when `stats` is `Some`,
/// times the `apply` call and records its outcome. Maps a [`HarnessError`] or a
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
    coverage: &mut Coverage,
    mut stats: Option<&mut Stats>,
) -> Result<(), FailureKind> {
    tracing::debug!(op = ?op, check, "apply op");
    let started = std::time::Instant::now();
    let result = harness.apply(ctx, world, op).await;
    let elapsed = started.elapsed();
    match result {
        Ok(verdict) => {
            if let Some(s) = stats.as_mut() {
                let outcome = match &verdict {
                    Verdict::Accepted => OpOutcome::Accepted,
                    Verdict::Rejected { reason } => OpOutcome::Rejected(reason),
                };
                s.record(&op_label(op), elapsed, outcome);
            }
            tracing::debug!(?verdict, op = ?op, "op applied");
        }
        Err(HarnessError::Bug(m)) => {
            if let Some(s) = stats.as_mut() {
                s.record(&op_label(op), elapsed, OpOutcome::Bug(&m));
            }
            tracing::debug!(op = ?op, detail = %m, "op surfaced a bug");
            return Err(FailureKind::Bug(m));
        }
        Err(HarnessError::Infra(e)) => {
            let msg = e.to_string();
            if let Some(s) = stats.as_mut() {
                s.record(&op_label(op), elapsed, OpOutcome::Infra(&msg));
            }
            tracing::debug!(op = ?op, error = %e, "op infra error");
            return Err(FailureKind::Infra(msg));
        }
    }
    if check {
        sweep(harness, ctx, world, coverage).await?;
    }
    Ok(())
}

/// Emit a one-line end-of-run summary (op count, skips, pass/fail) at `info` level, plus a
/// per-invariant coverage line that flags any invariant that never ran, and (when `stats` is
/// enabled) a per-op stats block. Sits above the per-op `debug` logs so a run's totals show under
/// `RUST_LOG=cross_vm_framework=info` without the per-operation spam. Called once at every driver
/// exit via a labeled-block funnel.
fn log_summary<Op>(report: &RunReport<Op>, stats: Option<&Stats>) {
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
    // Coverage: warn if any invariant never fired, else confirm all did.
    let uncovered: Vec<&str> = report.coverage.uncovered().collect();
    if uncovered.is_empty() {
        tracing::info!(
            invariants = report.coverage.iter().count(),
            "invariant coverage: all invariants fired"
        );
    } else {
        tracing::warn!(?uncovered, "invariant coverage: some invariants never ran");
    }
    if let Some(s) = stats {
        if !s.is_empty() {
            s.log_summary();
        }
    }
}

/// Whether two failures are "the same" for shrinking. `Bug`s must carry the same detail string —
/// two distinct bugs both surfacing as [`FailureKind::Bug`] must not be conflated, so harnesses
/// should emit stable, state-independent bug messages. Invariant failures compare by invariant
/// name only (details embed state and vary per replay), and any two `Infra` failures match
/// (transport errors are inherently noisy).
fn same_failure(a: &FailureKind, b: &FailureKind) -> bool {
    match (a, b) {
        (FailureKind::Bug(d1), FailureKind::Bug(d2)) => d1 == d2,
        (FailureKind::Infra(_), FailureKind::Infra(_)) => true,
        (FailureKind::Invariant { name: n1, .. }, FailureKind::Invariant { name: n2, .. }) => {
            n1 == n2
        }
        _ => false,
    }
}

/// Replay-attempt budget for one shrink call (the analog of Foundry's `shrink_run_limit`): each
/// candidate costs a fresh env build plus a full replay, and a nothing-removable sequence costs
/// O(n log n) attempts, so a cap keeps pathological shrinks bounded. On exhaustion the best
/// sequence found so far is returned.
pub const DEFAULT_SHRINK_LIMIT: usize = 256;

const NOT_SET_UP: &str = "Runner::setup(ctx, world) must be called before running";
