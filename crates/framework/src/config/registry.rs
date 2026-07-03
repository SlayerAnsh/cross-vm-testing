//! [`Registry`]: the harness registry and type-erasure layer (spec section 7).
//!
//! [`Harness`] has four associated types and async fns, so it is not `dyn` compatible. The
//! bridge is one closure pair per registered harness ([`Registry::register`]), monomorphized at
//! registration:
//! - a `validate` closure that type-checks a profile's `kinds`/`weights`/scenario `op`s against
//!   `H::OpKind`/`H::Operation` without touching a chain, and
//! - a `run` closure that drives one resolved profile end to end (setup, drive, erase).
//!
//! No `dyn Harness` ever exists inside either closure; only their *signatures* are erased (both
//! are boxed as `Box<dyn Fn(..)>`, one of them returning a [`LocalBoxFuture`]). The serde bounds
//! needed to round-trip `H::Operation`/`H::OpKind` through TOML live **only** on
//! [`Registry::register`]; [`Harness`] itself is unchanged. [`ConfigHarness`] is a
//! documentation-only blanket marker for that requirement: a harness that never touches the CLI
//! compiles exactly as before, and a missing derive at a `register` call site is an ordinary
//! compile error, not a runtime surprise.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use thiserror::Error;

use crate::harness::{
    sub_seed, Endurance, EnduranceConfig, Expectation, Failure, Fuzz, Harness, Invariant, KindMix,
    RunReport, Runner, Scenario, ScenarioStep, Stats,
};

use super::erased::{erase_report, LocalBoxFuture};
use super::{ChainSpecData, ErasedReport, ResolvedProfile, RunOptions, SetupFuture, SetupRequest};

/// Blanket, documentation-only marker: a [`Harness`] whose `Operation`/`OpKind` round-trip
/// through serde. [`Registry::register`] is what actually *enforces* this (via bounds on
/// `H::Operation`/`H::OpKind`); this trait exists only so the requirement has a name to point at
/// from docs and error messages. Blanket-implemented for every qualifying `Harness`, so it needs
/// no explicit `impl` at the call site.
pub trait ConfigHarness: Harness
where
    Self::Operation: serde::Serialize + DeserializeOwned,
    Self::OpKind: serde::Serialize + DeserializeOwned + Copy,
{
}

impl<H> ConfigHarness for H
where
    H: Harness,
    H::Operation: serde::Serialize + DeserializeOwned,
    H::OpKind: serde::Serialize + DeserializeOwned + Copy,
{
}

/// A profile's `kinds`, `weights`, or scenario `op` failed to parse against a harness's
/// `OpKind`/`Operation` type. Carries serde's own error message, which already lists the valid
/// variant names for an unknown kind (spec section 7.1).
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ValidationError(String);

/// Errors from [`Registry::validate`] and [`Registry::run`].
#[derive(Debug, Error)]
pub enum RunError {
    /// No harness is registered under this name.
    #[error("unknown harness `{0}`")]
    UnknownHarness(String),
    /// A profile's `kinds`/`weights`/scenario `op` failed to type-check against the harness; see
    /// [`ValidationError`] for the underlying serde message.
    #[error(transparent)]
    Validation(#[from] ValidationError),
    /// The config-driven setup fn failed (deploy/RPC/model desync failure) — infrastructure, not
    /// a discovered bug.
    #[error("setup failed: {0}")]
    Setup(String),
    /// Serializing the failing op history to JSON failed (an out-of-range integer, a non-string
    /// map key, ...). A well-behaved op enum never hits this.
    #[error("failed to serialize failure history: {0}")]
    Serialize(String),
    /// A profile resolved to a degenerate run this layer refuses to silently no-op (e.g. a
    /// `--cases 0` override).
    #[error("{0}")]
    Invalid(String),
    /// This run mode is not implemented yet: the endurance driver lands in a later task (P3).
    /// `validate` still type-checks its `kinds`/`weights` today; only the `run` arm is
    /// unimplemented.
    #[error("{0}")]
    UnsupportedMode(String),
}

/// Stand-in "no duration bound" for an endurance profile that sets `max_ops` but not `duration`.
/// Not `Duration::MAX`: adding that to `Instant::now()` overflows the underlying clock's
/// arithmetic (panics). 50 years comfortably outlives any `max_ops`-bounded run while staying
/// far inside `Instant`'s representable range.
const EFFECTIVELY_UNBOUNDED_DURATION: std::time::Duration =
    std::time::Duration::from_secs(60 * 60 * 24 * 365 * 50);

/// A validate closure's boxed shape, factored out purely to keep [`Entry`] and
/// [`Registry::register`] readable (clippy's `type_complexity`).
type ValidateFn = Box<dyn Fn(&cross_vm_config::Profile) -> Result<(), ValidationError>>;

/// A run closure's boxed shape: drives one resolved profile end-to-end (setup, drive, erase).
/// Factored out for the same readability reason as [`ValidateFn`].
type RunFn = Box<
    dyn for<'a> Fn(
        &'a ResolvedProfile,
        &'a RunOptions,
    ) -> LocalBoxFuture<'a, Result<ErasedReport, RunError>>,
>;

/// One registered harness: a validate closure and a run closure, both monomorphized over the
/// harness type at [`Registry::register`] time. Never exposed to callers directly; [`Registry`]'s
/// own methods are the whole surface a CLI needs onto a registered harness.
struct Entry {
    /// Type-check a profile's `kinds`/`weights`/scenario `op`s against `H`'s enums, without
    /// running or touching a chain.
    validate: ValidateFn,
    /// Drive one resolved profile end-to-end (setup, drive, erase).
    run: RunFn,
}

/// The harness registry: every [`Harness`] a `cross-vm` binary can drive, keyed by its registered
/// name. Built once at startup via repeated [`register`](Registry::register) calls;
/// [`names`](Registry::names), [`validate`](Registry::validate), and [`run`](Registry::run) are
/// the whole surface the CLI (a later task) needs.
#[derive(Default)]
pub struct Registry {
    entries: BTreeMap<String, Entry>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a harness under `name`: `harness` builds a fresh `H` per run (fresh per fuzz
    /// case too, so a harness may hold per-run state), and `setup` builds the live
    /// `(Ctx, H::World)` from a [`SetupRequest`].
    ///
    /// The serde bounds here are the only place [`Harness`] gains a requirement from the config
    /// layer: `H::Operation`/`H::OpKind` must round-trip through TOML so a profile's
    /// `kinds`/`weights`/scenario `op`s can be type-checked, and a failing history can be
    /// serialized into a report or (a later task's) replay artifact. A harness that never
    /// registers here needs neither derive; [`Harness`] itself does not change.
    ///
    /// `setup`'s return type is pinned to `SetupFuture<'static, H::World>` rather than the fully
    /// generic `for<'a> Fn(SetupRequest) -> SetupFuture<'a, H::World>` [`SetupFuture`]'s own docs
    /// describe: `Fn`'s associated `Output` type cannot depend on a higher-ranked lifetime that
    /// does not appear in its arguments (`SetupRequest` is consumed by value), so that bound does
    /// not type-check (rustc E0582). Every real setup fn already needs to be `'static` (`S:
    /// 'static` below), so it can only ever borrow from its own `'static` captures; a future
    /// borrowing `'static` data can always be typed as `SetupFuture<'static, _>`, so nothing is
    /// lost in practice — a setup fn is always called and immediately `.await`ed here, never
    /// stored past that point.
    pub fn register<H, F, S>(&mut self, name: &str, harness: F, setup: S) -> &mut Self
    where
        H: Harness + 'static,
        H::Operation: serde::Serialize + DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + DeserializeOwned + Copy + 'static,
        F: Fn() -> H + 'static,
        S: Fn(SetupRequest) -> SetupFuture<'static, H::World> + 'static,
    {
        let validate: ValidateFn =
            Box::new(|profile: &cross_vm_config::Profile| validate_profile::<H>(profile));

        // `Rc`, not a borrow of the closure's own captured fields: the run closure must be
        // callable an arbitrary number of times (once per fuzz case), and each call needs to
        // move an owned handle to `harness`/`setup` into its own `async move` block so the
        // returned future's lifetime is exactly the `'a` on its `&'a ResolvedProfile`/`&'a
        // RunOptions` parameters, never tied to the (much shorter, per-call) borrow of the
        // closure's environment. Cloning an `Rc` is cheap, and everything here is already `!Send`
        // by design, so this costs nothing the design does not already pay for.
        let harness = Rc::new(harness);
        let setup = Rc::new(setup);
        let harness_name = name.to_string();

        let run: RunFn = Box::new(
            move |resolved: &ResolvedProfile,
                  opts: &RunOptions|
                  -> LocalBoxFuture<'_, Result<ErasedReport, RunError>> {
                let harness = Rc::clone(&harness);
                let setup = Rc::clone(&setup);
                let harness_name = harness_name.clone();
                Box::pin(async move {
                    run_profile::<H, F, S>(&harness, &setup, harness_name, resolved, opts).await
                })
            },
        );

        self.entries.insert(name.to_string(), Entry { validate, run });
        self
    }

    /// Every registered harness name, in sorted (`BTreeMap`) order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Type-checks `profile`'s `kinds`/`weights`/scenario `op`s against `harness`'s enums, without
    /// touching a chain. Powers `cross-vm validate` (a later task).
    pub fn validate(
        &self,
        harness: &str,
        profile: &cross_vm_config::Profile,
    ) -> Result<(), RunError> {
        let entry = self.lookup(harness)?;
        (entry.validate)(profile).map_err(RunError::from)
    }

    /// Drives `resolved` end-to-end over `harness` (setup, drive, erase). `opts` supplies the
    /// `ops`/`cases` CLI overrides `resolve_profile` deliberately leaves unfolded (see
    /// [`RunOptions`]).
    pub async fn run(
        &self,
        harness: &str,
        resolved: &ResolvedProfile,
        opts: &RunOptions,
    ) -> Result<ErasedReport, RunError> {
        let entry = self.lookup(harness)?;
        (entry.run)(resolved, opts).await
    }

    fn lookup(&self, harness: &str) -> Result<&Entry, RunError> {
        self.entries
            .get(harness)
            .ok_or_else(|| RunError::UnknownHarness(harness.to_string()))
    }
}

/// How a profile's `kinds`/`weights` map onto the [`KindMix`] a run passes to `run_with`. Owns
/// its parsed kinds so the borrow `run_with` takes on a `&[K]` / `&[(K, u32)]` outlives the call
/// (spec section 7.1: weighted pair order is the sorted kind-name / `BTreeMap` iteration order).
enum KindSelection<K> {
    /// Neither `kinds` nor `weights` was set: draw from every kind via `Harness::generate` (a
    /// harness `generate` override still applies).
    All,
    /// `kinds` was set: uniform draw over this subset.
    Restricted(Vec<K>),
    /// `weights` was set: draw over these `(kind, weight)` pairs, in sorted-kind-name order.
    Weighted(Vec<(K, u32)>),
}

impl<K> KindSelection<K> {
    fn as_mix(&self) -> KindMix<'_, K> {
        match self {
            KindSelection::All => KindMix::Harness,
            KindSelection::Restricted(ks) => KindMix::Restricted(ks),
            KindSelection::Weighted(pairs) => KindMix::Weighted(pairs),
        }
    }
}

/// Parses one profile's `kinds`/`weights` against `H::OpKind`. Precedence matches spec section
/// 6.1 (`weights` beats `kinds` beats the harness default); `cross-vm-config`'s structural
/// validation already rejects a profile that sets both, so this is belt and suspenders.
fn parse_kind_selection<H: Harness>(
    kinds: &Option<Vec<String>>,
    weights: &Option<BTreeMap<String, u32>>,
) -> Result<KindSelection<H::OpKind>, ValidationError>
where
    H::OpKind: DeserializeOwned,
{
    if let Some(weights) = weights {
        // `BTreeMap` iteration is sorted-key order: the pinned weighted-stream order documented
        // on `crate::harness::KindMix::Weighted`.
        let pairs = weights
            .iter()
            .map(|(name, weight)| parse_kind::<H>(name).map(|k| (k, *weight)))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(KindSelection::Weighted(pairs));
    }
    if let Some(kinds) = kinds {
        let parsed = kinds
            .iter()
            .map(|name| parse_kind::<H>(name))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(KindSelection::Restricted(parsed));
    }
    Ok(KindSelection::All)
}

/// Parses one kind name via `H::OpKind`'s derived `Deserialize`: `toml::Value` is itself a
/// `serde::Deserializer`, so a bare kind name string deserializes exactly as it would from
/// `kinds = ["Deposit"]` in TOML. An unknown name surfaces serde's own "unknown variant, expected
/// one of ..." message, which already lists the valid names.
fn parse_kind<H: Harness>(name: &str) -> Result<H::OpKind, ValidationError>
where
    H::OpKind: DeserializeOwned,
{
    H::OpKind::deserialize(toml::Value::String(name.to_string()))
        .map_err(|e| ValidationError(e.to_string()))
}

/// Type-checks one profile's `kinds`/`weights`/scenario `op`s against `H`, without running or
/// touching a chain. Powers [`Registry::validate`].
fn validate_profile<H: Harness>(profile: &cross_vm_config::Profile) -> Result<(), ValidationError>
where
    H::OpKind: DeserializeOwned,
    H::Operation: DeserializeOwned,
{
    match profile {
        cross_vm_config::Profile::Fuzz(p) => {
            parse_kind_selection::<H>(&p.kinds, &p.weights).map(|_| ())
        }
        cross_vm_config::Profile::Invariant(p) => {
            parse_kind_selection::<H>(&p.kinds, &p.weights).map(|_| ())
        }
        cross_vm_config::Profile::Endurance(p) => {
            parse_kind_selection::<H>(&p.kinds, &p.weights).map(|_| ())
        }
        cross_vm_config::Profile::Scenario(p) => {
            for step in &p.steps {
                H::Operation::deserialize(step.op.clone())
                    .map_err(|e| ValidationError(e.to_string()))?;
            }
            Ok(())
        }
    }
}

/// Resolves a profile's `SeedSpec` to a concrete `u64`, once per profile run: `Fixed(n)` is used
/// verbatim, `Random` draws a fresh seed and logs it so the run can be reproduced.
fn resolve_base_seed(seed: cross_vm_config::SeedSpec) -> u64 {
    match seed {
        cross_vm_config::SeedSpec::Fixed(n) => n,
        cross_vm_config::SeedSpec::Random => {
            let n = rand::random::<u64>();
            tracing::info!("set seed = {n} to reproduce");
            n
        }
    }
}

/// Assembles the [`SetupRequest`] a config-driven setup fn receives, for one concrete `seed`
/// (per-case for fuzz).
fn build_setup_request(resolved: &ResolvedProfile, seed: u64) -> SetupRequest {
    SetupRequest {
        target: resolved.target,
        chains: resolved
            .chain_specs
            .iter()
            .map(|c: &ChainSpecData| c.label.clone())
            .collect(),
        chain_specs: resolved.chain_specs.clone(),
        params: resolved.params.clone(),
        seed,
    }
}

/// If `resolved.shrink` is set and `report` failed, greedily shrinks the failing history (spec
/// section 10 / this task) before it ever reaches [`erase_report`]: rebuilds a fresh `(Ctx,
/// World)` from `setup`/`seed` for every replay attempt via a scenario-mode [`Runner`], replaces
/// the failure's `history` with the minimized sequence (keeping every other `Failure` field —
/// `step`/`op`/`kind` — exactly as the original run reported them), and returns `(report, true)`.
/// Returns `(report, false)` unchanged when shrink is disabled or the run passed.
///
/// `seed` is the concrete seed the *failing* run was driven with: the per-case sub-seed for fuzz
/// (the case that actually failed, not the profile's base seed), the profile's own resolved seed
/// for invariant/endurance. Shrink candidates must rebuild under the exact starting state the
/// original failure surfaced under, or a "still fails" verdict would compare apples to oranges.
///
/// Shrinking is expensive (every candidate re-drives a fresh setup), so this only ever runs on an
/// actual failure with `resolved.shrink` enabled — never on a passing run or when shrink is off
/// (scenario profiles never call this at all: concrete steps are not generative).
async fn maybe_shrink<H, F, S>(
    mut report: RunReport<H::Operation>,
    make_harness: &F,
    setup: &S,
    resolved: &ResolvedProfile,
    seed: u64,
) -> (RunReport<H::Operation>, bool)
where
    H: Harness,
    F: Fn() -> H,
    S: Fn(SetupRequest) -> SetupFuture<'static, H::World>,
{
    if !resolved.shrink || report.failure.is_none() {
        return (report, false);
    }
    let failure = report.failure.take().expect("checked is_some above");
    let history = failure.history;

    let rebuild = || {
        let req = build_setup_request(resolved, seed);
        async move {
            setup(req)
                .await
                .expect("shrink rebuild: setup failed")
        }
    };

    let mut runner = Runner::<H, Scenario>::scenario(make_harness(), seed);
    let shrunk_history = runner
        .shrink_with_limit(history, resolved.check_every, resolved.shrink_limit, rebuild)
        .await;

    report.failure = Some(Failure {
        history: shrunk_history,
        ..failure
    });
    (report, true)
}

/// The generic body every registered harness's `run` closure calls into (spec section 7's `run`
/// bullet list). No `dyn Harness` exists here: `H`, `F`, `S` are all concrete at the call site.
async fn run_profile<H, F, S>(
    make_harness: &F,
    setup: &S,
    harness_name: String,
    resolved: &ResolvedProfile,
    opts: &RunOptions,
) -> Result<ErasedReport, RunError>
where
    H: Harness,
    H::Operation: serde::Serialize + DeserializeOwned,
    H::OpKind: DeserializeOwned + Copy,
    F: Fn() -> H,
    S: Fn(SetupRequest) -> SetupFuture<'static, H::World>,
{
    let started = std::time::Instant::now();
    let base_seed = resolve_base_seed(resolved.seed);

    match &resolved.profile {
        cross_vm_config::Profile::Fuzz(p) => {
            let cases = opts.cases.unwrap_or(p.cases);
            let ops = opts.ops.unwrap_or(p.ops);
            if cases == 0 {
                return Err(RunError::Invalid(
                    "fuzz profile resolved to 0 cases".to_string(),
                ));
            }
            let selection = parse_kind_selection::<H>(&p.kinds, &p.weights)?;

            // The first failing case ends the profile; if every case passes, the last case's
            // report stands in for the profile (there is no single meaningful "combined" report
            // across independent cases, and the last case is as representative as any — see the
            // task report for the alternative considered and rejected).
            let mut last: Option<(RunReport<H::Operation>, Option<Stats>, u64)> = None;
            for case in 0..cases {
                let seed_i = sub_seed(base_seed, case);
                tracing::info!(case, seed = seed_i, cases, "fuzz case starting");

                let (ctx, world) = setup(build_setup_request(resolved, seed_i))
                    .await
                    .map_err(|e| RunError::Setup(e.to_string()))?;

                let mut runner = Runner::<H, Fuzz>::fuzz(make_harness(), seed_i);
                if resolved.stats {
                    runner.with_stats();
                }
                runner.setup(ctx, world);
                let report = runner
                    .run_with(ops, selection.as_mix(), resolved.check_every)
                    .await;
                let stats = runner.stats().cloned();
                let failed = !report.passed();
                last = Some((report, stats, seed_i));
                if failed {
                    break;
                }
            }
            // `cases > 0` is checked above, so the loop always ran at least once.
            let (report, stats, failing_seed) = last.expect("fuzz loop ran at least one case");
            let (report, shrunk) =
                maybe_shrink(report, make_harness, setup, resolved, failing_seed).await;
            erase_report(
                report,
                harness_name,
                resolved.name.clone(),
                "fuzz".to_string(),
                stats,
                started.elapsed(),
                shrunk,
            )
            .map_err(|e| RunError::Serialize(e.to_string()))
        }
        cross_vm_config::Profile::Invariant(p) => {
            let ops = opts.ops.unwrap_or(p.ops);
            let selection = parse_kind_selection::<H>(&p.kinds, &p.weights)?;

            let (ctx, world) = setup(build_setup_request(resolved, base_seed))
                .await
                .map_err(|e| RunError::Setup(e.to_string()))?;

            let mut runner = Runner::<H, Invariant>::invariant(make_harness(), base_seed);
            if resolved.stats {
                runner.with_stats();
            }
            runner.setup(ctx, world);
            let report = runner
                .run_with(ops, selection.as_mix(), resolved.check_every)
                .await;
            let stats = runner.stats().cloned();

            let (report, shrunk) = maybe_shrink(report, make_harness, setup, resolved, base_seed).await;
            erase_report(
                report,
                harness_name,
                resolved.name.clone(),
                "invariant".to_string(),
                stats,
                started.elapsed(),
                shrunk,
            )
            .map_err(|e| RunError::Serialize(e.to_string()))
        }
        cross_vm_config::Profile::Endurance(p) => {
            let selection = parse_kind_selection::<H>(&p.kinds, &p.weights)?;

            let (ctx, world) = setup(build_setup_request(resolved, base_seed))
                .await
                .map_err(|e| RunError::Setup(e.to_string()))?;

            // The loader's structural validation (`validate::validate`) guarantees a profile
            // sets `duration` and/or `max_ops`; when only `max_ops` is set, a long-but-safe
            // duration (not `Duration::MAX`, which overflows `Instant` arithmetic) lets `max_ops`
            // alone govern the run.
            let duration = opts
                .duration
                .or(p.duration)
                .unwrap_or(EFFECTIVELY_UNBOUNDED_DURATION);
            let max_ops = opts.ops.or(p.max_ops);

            let mut cfg = EnduranceConfig::new(duration)
                .base_delay(p.base_delay)
                .max_delay(p.max_delay)
                .check_every(resolved.check_every)
                .max_consecutive_infra(p.max_consecutive_infra)
                .heartbeat(p.heartbeat);
            if let Some(n) = max_ops {
                cfg = cfg.max_ops(n);
            }
            if let Some(base) = p.advance_blocks {
                cfg = cfg.advance_blocks(base as u64, p.block_jitter as u64);
            }
            if let Some(flag) = opts.stop.clone() {
                cfg = cfg.stop(flag);
            }

            let mut runner = Runner::<H, Endurance>::endurance(make_harness(), base_seed);
            if resolved.stats {
                runner.with_stats();
            }
            runner.setup(ctx, world);
            let report = runner.run_with(cfg, selection.as_mix()).await;
            let stats = runner.stats().cloned();

            // Shrink defaults to `false` for endurance (spec section 4.3); `resolved.shrink`
            // already carries that mode-dependent default, so `maybe_shrink` only actually
            // shrinks when a profile opted in explicitly.
            let (report, shrunk) = maybe_shrink(report, make_harness, setup, resolved, base_seed).await;
            erase_report(
                report,
                harness_name,
                resolved.name.clone(),
                "endurance".to_string(),
                stats,
                started.elapsed(),
                shrunk,
            )
            .map_err(|e| RunError::Serialize(e.to_string()))
        }
        cross_vm_config::Profile::Scenario(p) => {
            let (ctx, world) = setup(build_setup_request(resolved, base_seed))
                .await
                .map_err(|e| RunError::Setup(e.to_string()))?;

            let steps = p
                .steps
                .iter()
                .map(|raw| {
                    let op = H::Operation::deserialize(raw.op.clone())
                        .map_err(|e| ValidationError(e.to_string()))?;
                    Ok(ScenarioStep {
                        op,
                        expect: match raw.expect {
                            cross_vm_config::ExpectStr::Accepted => Expectation::Accepted,
                            cross_vm_config::ExpectStr::Rejected => Expectation::Rejected,
                            cross_vm_config::ExpectStr::Any => Expectation::Any,
                        },
                        delay: raw.delay,
                        check: raw.check,
                    })
                })
                .collect::<Result<Vec<_>, ValidationError>>()?;

            let mut runner = Runner::<H, Scenario>::scenario(make_harness(), base_seed);
            if resolved.stats {
                runner.with_stats();
            }
            runner.setup(ctx, world);
            let report = runner.run_steps(steps, resolved.check_every).await;
            let stats = runner.stats().cloned();

            // Scenario never shrinks: its steps are concrete, not generative, so there is nothing
            // to minimize.
            erase_report(
                report,
                harness_name,
                resolved.name.clone(),
                "scenario".to_string(),
                stats,
                started.elapsed(),
                false,
            )
            .map_err(|e| RunError::Serialize(e.to_string()))
        }
    }
}

#[cfg(all(test, feature = "cli"))]
mod tests {
    use super::*;
    use crate::harness::{CheckOutcome, Ctx, FailureKind, HarnessError, Prng, Verdict};
    use cross_vm_config::{CommonKeys, FuzzProfile, InvariantProfile, Profile, SeedSpec};
    use std::collections::BTreeMap;

    // ----- a minimal mock harness: no real chain interaction, just enough to drive `run_with` -----

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    enum MockKind {
        Ping,
        Pong,
        Boom,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    enum MockOp {
        Ping,
        Pong,
        Boom,
    }

    #[derive(Debug, Clone)]
    enum MockInvariant {
        AlwaysHolds,
    }

    struct MockHarness;

    impl Harness for MockHarness {
        type World = u32;
        type Operation = MockOp;
        type Invariant = MockInvariant;
        type OpKind = MockKind;

        async fn apply(
            &self,
            _ctx: &mut Ctx,
            world: &mut Self::World,
            op: &Self::Operation,
        ) -> Result<Verdict, HarnessError> {
            *world += 1;
            match op {
                MockOp::Ping | MockOp::Pong => Ok(Verdict::Accepted),
                MockOp::Boom => Err(HarnessError::Bug("boom".to_string())),
            }
        }

        fn op_kinds(&self) -> Vec<Self::OpKind> {
            vec![MockKind::Ping, MockKind::Pong, MockKind::Boom]
        }

        fn generate_op(&self, _rng: &mut Prng, _world: &Self::World, kind: Self::OpKind) -> Self::Operation {
            match kind {
                MockKind::Ping => MockOp::Ping,
                MockKind::Pong => MockOp::Pong,
                MockKind::Boom => MockOp::Boom,
            }
        }

        fn invariants(&self) -> Vec<Self::Invariant> {
            vec![MockInvariant::AlwaysHolds]
        }

        async fn check(
            &self,
            _ctx: &mut Ctx,
            _world: &Self::World,
            _inv: &Self::Invariant,
        ) -> CheckOutcome {
            CheckOutcome::Held
        }
    }

    async fn mock_ctx() -> Ctx {
        let wallets = Rc::new(
            cross_vm_core::WalletFactory::from_roster(crate::EmptyWallets::SPECS)
                .expect("empty roster"),
        );
        let env = crate::MultiChainEnv::new("mock", wallets);
        Ctx::new(env.start().await.expect("start"))
    }

    fn mock_setup(req: SetupRequest) -> SetupFuture<'static, u32> {
        Box::pin(async move {
            let ctx = mock_ctx().await;
            Ok((ctx, req.seed as u32))
        })
    }

    // ----- fixture builders -----

    fn common() -> CommonKeys {
        CommonKeys {
            seed: SeedSpec::Fixed(0),
            check_every: 1,
            stats: false,
            artifacts_dir: "target/cross-vm".to_string(),
            json_report: None,
            env: None,
            shrink: None,
            shrink_limit: 256,
        }
    }

    fn fuzz_profile(
        cases: usize,
        ops: usize,
        kinds: Option<Vec<String>>,
        weights: Option<BTreeMap<String, u32>>,
    ) -> Profile {
        Profile::Fuzz(FuzzProfile {
            common: common(),
            cases,
            ops,
            kinds,
            weights,
        })
    }

    fn invariant_profile(ops: usize, kinds: Option<Vec<String>>) -> Profile {
        Profile::Invariant(InvariantProfile {
            common: common(),
            ops,
            kinds,
            weights: None,
        })
    }

    fn resolved(profile: Profile) -> ResolvedProfile {
        ResolvedProfile {
            name: "test".to_string(),
            profile,
            seed: SeedSpec::Fixed(7),
            chain_specs: Vec::new(),
            target: super::super::Target::Mock,
            params: toml::Table::new(),
            check_every: 1,
            stats: false,
            shrink: false,
            shrink_limit: 256,
            artifacts_dir: "target/cross-vm".to_string(),
            json_report: None,
        }
    }

    /// [`resolved`] with `shrink`/`shrink_limit` overridden; every other field matches.
    fn resolved_with_shrink(profile: Profile, shrink: bool, shrink_limit: usize) -> ResolvedProfile {
        ResolvedProfile {
            shrink,
            shrink_limit,
            ..resolved(profile)
        }
    }

    // ----- validate -----

    #[test]
    fn validate_accepts_known_kind() {
        let profile = fuzz_profile(1, 1, Some(vec!["Ping".to_string()]), None);
        assert!(validate_profile::<MockHarness>(&profile).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_kind_listing_valid_names() {
        let profile = fuzz_profile(1, 1, Some(vec!["Nope".to_string()]), None);
        let err = validate_profile::<MockHarness>(&profile).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Ping"), "message was: {msg}");
        assert!(msg.contains("Pong"), "message was: {msg}");
    }

    #[test]
    fn validate_rejects_unknown_weight_key() {
        let mut weights = BTreeMap::new();
        weights.insert("Nope".to_string(), 5);
        let profile = fuzz_profile(1, 1, None, Some(weights));
        let err = validate_profile::<MockHarness>(&profile).unwrap_err();
        assert!(err.to_string().contains("Ping"));
    }

    #[test]
    fn validate_via_registry_reports_unknown_harness() {
        let registry = Registry::new();
        let profile = fuzz_profile(1, 1, None, None);
        let err = registry.validate("nope", &profile).unwrap_err();
        assert!(matches!(err, RunError::UnknownHarness(name) if name == "nope"));
    }

    // ----- KindMix-from-config mapping -----

    #[test]
    fn kind_selection_weighted_is_sorted_by_kind_name() {
        let mut weights = BTreeMap::new();
        weights.insert("Pong".to_string(), 1);
        weights.insert("Ping".to_string(), 2);
        let selection = parse_kind_selection::<MockHarness>(&None, &Some(weights)).unwrap();
        match selection {
            KindSelection::Weighted(pairs) => {
                assert_eq!(pairs, vec![(MockKind::Ping, 2), (MockKind::Pong, 1)]);
            }
            _ => panic!("expected Weighted"),
        }
    }

    #[test]
    fn kind_selection_kinds_is_restricted() {
        let selection =
            parse_kind_selection::<MockHarness>(&Some(vec!["Pong".to_string()]), &None).unwrap();
        match selection {
            KindSelection::Restricted(ks) => assert_eq!(ks, vec![MockKind::Pong]),
            _ => panic!("expected Restricted"),
        }
    }

    #[test]
    fn kind_selection_neither_is_all() {
        let selection = parse_kind_selection::<MockHarness>(&None, &None).unwrap();
        assert!(matches!(selection, KindSelection::All));
    }

    // ----- registry bookkeeping -----

    #[test]
    fn names_lists_every_registered_harness_sorted() {
        let mut registry = Registry::new();
        registry.register("zeta", || MockHarness, mock_setup);
        registry.register("alpha", || MockHarness, mock_setup);
        let names: Vec<&str> = registry.names().collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[tokio::test]
    async fn run_unknown_harness_errors() {
        let registry = Registry::new();
        let resolved = resolved(fuzz_profile(1, 1, None, None));
        let err = registry
            .run("nope", &resolved, &RunOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::UnknownHarness(name) if name == "nope"));
    }

    // ----- full fuzz/invariant runs over the mock harness (cheap: `Ctx` needs no injected chain) -----

    #[tokio::test]
    async fn fuzz_run_all_passed_reports_last_case() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = fuzz_profile(
            3,
            2,
            Some(vec!["Ping".to_string(), "Pong".to_string()]),
            None,
        );
        let resolved = resolved(profile);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        assert_eq!(report.mode, "fuzz");
        assert!(report.failure.is_none());
        assert_eq!(report.steps, 2);
    }

    #[tokio::test]
    async fn fuzz_run_stops_at_first_failure_and_serializes_history() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = fuzz_profile(5, 3, Some(vec!["Boom".to_string()]), None);
        let resolved = resolved(profile);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run resolves to a report, not a RunError");
        assert_eq!(report.mode, "fuzz");
        let failure = report.failure.expect("must fail");
        assert!(matches!(failure.kind, FailureKind::Bug(ref m) if m == "boom"));
        assert_eq!(failure.step, 1);
        assert!(failure.op_debug.as_deref() == Some("Boom"));
        assert!(failure.history.is_array());
        assert!(!failure.shrunk);
    }

    #[tokio::test]
    async fn invariant_run_reports_mode_and_steps() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = invariant_profile(4, Some(vec!["Ping".to_string()]));
        let resolved = resolved(profile);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        assert_eq!(report.mode, "invariant");
        assert!(report.failure.is_none());
        assert_eq!(report.steps, 4);
    }

    // ----- scenario runs over the mock harness -----

    fn scenario_profile(steps: Vec<cross_vm_config::ScenarioStepRaw>) -> Profile {
        Profile::Scenario(cross_vm_config::ScenarioProfile {
            common: common(),
            steps,
            export_world: None,
        })
    }

    /// A step whose op is a bare unit-variant name (`MockOp` has no data, so a plain TOML string
    /// deserializes into it exactly like `H::OpKind::deserialize` does for `kinds`/`weights`).
    fn mock_step(op: &str, expect: cross_vm_config::ExpectStr) -> cross_vm_config::ScenarioStepRaw {
        cross_vm_config::ScenarioStepRaw {
            op: toml::Value::String(op.to_string()),
            expect,
            delay: std::time::Duration::ZERO,
            check: true,
        }
    }

    #[tokio::test]
    async fn scenario_run_drives_to_erased_report_with_scenario_mode() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let steps = vec![
            mock_step("Ping", cross_vm_config::ExpectStr::Accepted),
            mock_step("Pong", cross_vm_config::ExpectStr::Accepted),
        ];
        let resolved = resolved(scenario_profile(steps));

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        assert_eq!(report.mode, "scenario");
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert_eq!(report.steps, 2);
    }

    #[tokio::test]
    async fn scenario_run_expect_mismatch_fails_with_exact_message() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        // Ping is always accepted; expecting a rejection must fail with the exact message.
        let steps = vec![mock_step("Ping", cross_vm_config::ExpectStr::Rejected)];
        let resolved = resolved(scenario_profile(steps));

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run resolves to a report, not a RunError");
        let failure = report.failure.expect("must fail");
        assert!(
            matches!(
                failure.kind,
                FailureKind::Bug(ref m) if m == "step 1: expected rejection, operation was accepted"
            ),
            "{:?}",
            failure.kind
        );
    }

    #[tokio::test]
    async fn scenario_run_rejects_an_op_that_fails_to_deserialize() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let steps = vec![mock_step("NotAKind", cross_vm_config::ExpectStr::Accepted)];
        let resolved = resolved(scenario_profile(steps));

        let err = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Validation(_)), "{err:?}");
    }

    #[tokio::test]
    async fn ops_and_cases_overrides_from_run_options_win() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = fuzz_profile(1, 1, Some(vec!["Ping".to_string()]), None);
        let resolved = resolved(profile);
        let opts = RunOptions {
            ops: Some(5),
            cases: Some(2),
            ..Default::default()
        };

        let report = registry.run("mock", &resolved, &opts).await.expect("run ok");
        assert_eq!(report.steps, 5);
    }

    #[test]
    fn unsupported_mode_error_still_formats_for_a_future_mode() {
        // Every mode the schema knows about (fuzz/invariant/endurance/scenario) is implemented
        // today; `UnsupportedMode` stays on `RunError` as a forward-compat exit for a future
        // mode, and `Cli`'s exit-code mapping still matches on it (see `cli.rs`'s
        // `exit_code_for_run_error_maps_usage_errors_to_three`). This just pins the `Display`.
        let err = RunError::UnsupportedMode("not yet supported".to_string());
        assert_eq!(err.to_string(), "not yet supported");
    }

    // ----- endurance runs over the mock harness -----

    fn endurance_profile(
        duration: Option<std::time::Duration>,
        max_ops: Option<usize>,
        kinds: Option<Vec<String>>,
    ) -> Profile {
        Profile::Endurance(cross_vm_config::EnduranceProfile {
            common: common(),
            duration,
            max_ops,
            base_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
            advance_blocks: None,
            block_jitter: 0,
            max_consecutive_infra: 0,
            heartbeat: std::time::Duration::from_secs(60),
            kinds,
            weights: None,
        })
    }

    #[tokio::test]
    async fn endurance_run_over_mock_harness_reports_endurance_mode() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = endurance_profile(
            Some(std::time::Duration::from_millis(20)),
            None,
            Some(vec!["Ping".to_string()]),
        );
        let resolved = resolved(profile);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        assert_eq!(report.mode, "endurance");
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert!(report.steps > 0, "endurance ran zero steps");
    }

    #[tokio::test]
    async fn endurance_max_ops_bounds_the_run_independent_of_duration() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        // A generous duration but a tight max_ops, no delay: max_ops must be what stops the run,
        // and it must stop at exactly that count.
        let profile = endurance_profile(
            Some(std::time::Duration::from_secs(5)),
            Some(3),
            Some(vec!["Ping".to_string()]),
        );
        let resolved = resolved(profile);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        assert_eq!(report.steps, 3);
    }

    #[tokio::test]
    async fn endurance_run_options_ops_override_wins_as_max_ops() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = endurance_profile(
            Some(std::time::Duration::from_secs(5)),
            Some(100),
            Some(vec!["Ping".to_string()]),
        );
        let resolved = resolved(profile);
        let opts = RunOptions {
            ops: Some(2),
            ..Default::default()
        };

        let report = registry.run("mock", &resolved, &opts).await.expect("run ok");
        assert_eq!(report.steps, 2);
    }

    #[tokio::test]
    async fn endurance_missing_duration_with_max_ops_still_stops_at_max_ops() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        // No duration at all: the registry's defensive fallback must not panic or hang, and
        // max_ops alone must govern.
        let profile = endurance_profile(None, Some(4), Some(vec!["Ping".to_string()]));
        let resolved = resolved(profile);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        assert_eq!(report.steps, 4);
    }

    // ----- shrink-on-failure integration (Task 12) -----
    //
    // `MockHarness::Boom` fails the exact same way ("boom", state-independent) no matter what
    // Pings preceded it, so any failing history containing a Boom must shrink down to exactly
    // `[Boom]` — the single-op pass drops every other op and each drop still reproduces the same
    // `FailureKind::Bug("boom")`. This makes the minimized length ("1") a deterministic assertion
    // independent of the seeded rng stream, unlike asserting an exact *raw* history length would
    // be.

    #[tokio::test]
    async fn fuzz_shrink_true_minimizes_the_failing_history_and_marks_it_shrunk() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = fuzz_profile(
            1,
            20,
            Some(vec!["Ping".to_string(), "Boom".to_string()]),
            None,
        );

        let no_shrink = resolved(profile.clone());
        let report = registry
            .run("mock", &no_shrink, &RunOptions::default())
            .await
            .expect("run ok");
        let failure = report.failure.expect("must fail");
        assert!(!failure.shrunk);
        let raw_len = failure.history.as_array().expect("array").len();

        let shrink = resolved_with_shrink(profile, true, 256);
        let report = registry
            .run("mock", &shrink, &RunOptions::default())
            .await
            .expect("run ok");
        let failure = report.failure.expect("must fail");
        assert!(failure.shrunk);
        assert!(matches!(failure.kind, FailureKind::Bug(ref m) if m == "boom"));
        let shrunk_len = failure.history.as_array().expect("array").len();
        assert_eq!(shrunk_len, 1, "a lone Boom already reproduces; must shrink to it");
        assert!(shrunk_len <= raw_len);
    }

    #[tokio::test]
    async fn fuzz_shrink_false_leaves_the_raw_history_unshrunk() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = fuzz_profile(
            1,
            20,
            Some(vec!["Ping".to_string(), "Boom".to_string()]),
            None,
        );
        let resolved = resolved(profile);
        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        let failure = report.failure.expect("must fail");
        assert!(!failure.shrunk);
    }

    #[tokio::test]
    async fn invariant_shrink_true_minimizes_the_failing_history() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = invariant_profile(20, Some(vec!["Ping".to_string(), "Boom".to_string()]));
        let resolved = resolved_with_shrink(profile, true, 256);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        let failure = report.failure.expect("must fail");
        assert!(failure.shrunk);
        assert_eq!(failure.history.as_array().expect("array").len(), 1);
    }

    #[tokio::test]
    async fn tiny_shrink_limit_still_returns_a_reproducing_sequence() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = fuzz_profile(
            1,
            20,
            Some(vec!["Ping".to_string(), "Boom".to_string()]),
            None,
        );
        // A budget of 1 replay attempt: shrink_inner must still return *some* sequence (possibly
        // not fully minimized) that reproduces the exact same failure, never panic or hang.
        let resolved = resolved_with_shrink(profile, true, 1);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        let failure = report.failure.expect("must fail");
        assert!(failure.shrunk);
        assert!(matches!(failure.kind, FailureKind::Bug(ref m) if m == "boom"));
        assert!(!failure.history.as_array().expect("array").is_empty());
    }

    #[tokio::test]
    async fn endurance_shrink_defaults_to_false_and_never_shrinks() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = endurance_profile(
            Some(std::time::Duration::from_secs(5)),
            None,
            Some(vec!["Ping".to_string(), "Boom".to_string()]),
        );
        // `resolved()` hardcodes `shrink: false`, matching endurance's mode default (spec section
        // 4.3: `shrink` defaults to `false` for endurance, unlike fuzz/invariant).
        let resolved = resolved(profile);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        let failure = report.failure.expect("must fail");
        assert!(!failure.shrunk);
    }

    #[tokio::test]
    async fn endurance_shrink_true_shrinks_when_explicitly_enabled() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let profile = endurance_profile(
            Some(std::time::Duration::from_secs(5)),
            None,
            Some(vec!["Ping".to_string(), "Boom".to_string()]),
        );
        let resolved = resolved_with_shrink(profile, true, 256);

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        let failure = report.failure.expect("must fail");
        assert!(failure.shrunk);
        assert_eq!(failure.history.as_array().expect("array").len(), 1);
    }
}
