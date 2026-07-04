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
//!
//! [`Registry::register_persistent`] is the opt-in variant that adds exactly one more bound,
//! `H::World: Serialize`, and nothing else: both methods funnel into the private
//! `register_inner`, which takes an `Option<WorldExportFn<H::World>>` (`None` from `register`,
//! `Some` from `register_persistent`). `WorldExportFn<W>`'s own *type* — `Rc<dyn Fn(&W, &Path) ->
//! Result<(), RunError>>` — never mentions `Serialize`, so `register_inner`'s signature (and
//! therefore `register`'s) carries no such bound; only the closure *body*
//! `register_persistent` builds (`export_world_json::<H::World>`, where the bound is locally in
//! scope) does. A scenario profile's `export_world` is enforced twice: [`Registry::validate`]
//! rejects it up front (`validate_export_world`) whenever the harness has no exporter, and the
//! scenario run arm (`run_profile`) rejects it again at run time before touching a chain, since
//! `cross-vm run` does not call `validate` on its own.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use thiserror::Error;

use crate::harness::{
    sub_seed, Ctx, Endurance, EnduranceConfig, Expectation, Failure, Fuzz, Harness, Invariant,
    KindMix, RunReport, Runner, Scenario, ScenarioStep, Stats,
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
    /// A scenario profile's `export_world` failed to write: its parent directory could not be
    /// created, the final `World` failed to serialize as JSON, or the file itself could not be
    /// written. Not a discovered SUT bug — this maps to the infra exit code (`2`), the same
    /// bucket [`RunError::Setup`]/[`RunError::Serialize`] use. A harness that cannot export at
    /// all (registered via [`Registry::register`], not [`Registry::register_persistent`]) never
    /// reaches this variant; that mismatch is [`RunError::Invalid`] instead (a config/usage
    /// error, caught by `validate` too).
    #[error("failed to export world: {0}")]
    Export(String),
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

/// A world-exporter closure's shape: JSON-serializes the final `World` to a path.
/// [`Registry::register`] never constructs one of these (it passes `None` through
/// `register_inner`); [`Registry::register_persistent`] is the only place a real one is built
/// (`export_world_json::<H::World>`). Note this *type* mentions no `Serialize` bound on `W` at
/// all — only the function `register_persistent` points it at does — which is exactly what keeps
/// `register_inner`'s (and therefore `register`'s) signature free of the bound.
type WorldExportFn<W> = Rc<dyn Fn(&W, &Path) -> Result<(), RunError>>;

/// The pipeline handoff slot: the typed `(Ctx, World)` a passing donor phase stashes for the
/// next phase to inherit. One is created per registered harness in `register_inner` and lives for
/// the registry's lifetime; a `None` slot means no donor world is currently available. It is an
/// `Rc<RefCell<..>>` because the run closure is called once per phase and each call needs a shared
/// handle to the same slot; everything here is `!Send` by design (single-threaded), so this costs
/// nothing the design does not already pay for.
type SessionSlot<W> = Rc<RefCell<Option<(Ctx, W)>>>;

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
        H: Harness<Ctx = crate::harness::Ctx> + 'static,
        H::Operation: serde::Serialize + DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + DeserializeOwned + Copy + 'static,
        F: Fn() -> H + 'static,
        S: Fn(SetupRequest) -> SetupFuture<'static, H::World> + 'static,
    {
        self.register_inner::<H, F, S>(name, harness, setup, None)
    }

    /// Like [`register`](Registry::register), plus one additional bound: `H::World: Serialize`.
    /// A scenario profile registered this way may set `export_world = "path.json"`, which
    /// serializes the final `World` (via [`Runner::into_parts`](crate::harness::Runner::into_parts))
    /// as pretty JSON after [`run_steps`](crate::harness::Runner::run_steps) completes —
    /// regardless of pass/fail, so the state the run actually reached is what lands on disk.
    /// Missing parent directories are created; the written path is logged at `info`.
    ///
    /// A harness registered with plain `register` cannot export: a profile that sets
    /// `export_world` against it fails both `validate` (`RunError::Validation`, so `cross-vm
    /// validate` catches it offline) and `run` (`RunError::Invalid`, so a direct `cross-vm run`
    /// that skipped `validate` still cannot silently ignore the key).
    ///
    /// The `World` is whatever the harness's own type holds — for the in-tree vault harness that
    /// is learned addresses and model state, never a mnemonic or key — but this method makes no
    /// guarantee about what a *given* harness's `World` contains; a harness whose `World`
    /// happens to hold something sensitive would export it verbatim.
    pub fn register_persistent<H, F, S>(&mut self, name: &str, harness: F, setup: S) -> &mut Self
    where
        H: Harness<Ctx = crate::harness::Ctx> + 'static,
        H::Operation: serde::Serialize + DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + DeserializeOwned + Copy + 'static,
        H::World: serde::Serialize + 'static,
        F: Fn() -> H + 'static,
        S: Fn(SetupRequest) -> SetupFuture<'static, H::World> + 'static,
    {
        let export: WorldExportFn<H::World> = Rc::new(export_world_json::<H::World>);
        self.register_inner::<H, F, S>(name, harness, setup, Some(export))
    }

    /// The shared body of [`register`](Registry::register) and
    /// [`register_persistent`](Registry::register_persistent): builds the validate/run closures
    /// common to both. `export` is `None` for `register` (there is no `H::World: Serialize`
    /// bound in scope here to build a real exporter from — `register`'s own generic parameters
    /// never gain one) and `Some` for `register_persistent` (a closure built where that bound
    /// *is* in scope). This is the one place the bound-isolation trick lives: `Option<
    /// WorldExportFn<H::World>>` is a perfectly valid parameter type regardless of whether
    /// `H::World: Serialize` holds, since the *type* `WorldExportFn<W>` never mentions the bound
    /// — only the value `register_persistent` constructs does.
    fn register_inner<H, F, S>(
        &mut self,
        name: &str,
        harness: F,
        setup: S,
        export: Option<WorldExportFn<H::World>>,
    ) -> &mut Self
    where
        H: Harness<Ctx = crate::harness::Ctx> + 'static,
        H::Operation: serde::Serialize + DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + DeserializeOwned + Copy + 'static,
        F: Fn() -> H + 'static,
        S: Fn(SetupRequest) -> SetupFuture<'static, H::World> + 'static,
    {
        let export_capable = export.is_some();
        let validate: ValidateFn = Box::new(move |profile: &cross_vm_config::Profile| {
            validate_profile::<H>(profile)?;
            validate_export_world(profile, export_capable)
        });

        // `Rc`, not a borrow of the closure's own captured fields: the run closure must be
        // callable an arbitrary number of times (once per fuzz case), and each call needs to
        // move an owned handle to `harness`/`setup`/`export` into its own `async move` block so
        // the returned future's lifetime is exactly the `'a` on its `&'a ResolvedProfile`/`&'a
        // RunOptions` parameters, never tied to the (much shorter, per-call) borrow of the
        // closure's environment. Cloning an `Rc` is cheap, and everything here is already `!Send`
        // by design, so this costs nothing the design does not already pay for.
        let harness = Rc::new(harness);
        let setup = Rc::new(setup);
        let harness_name = name.to_string();

        // The pipeline handoff slot: one per registered harness, alive for the registry's
        // lifetime, holding the typed `(Ctx, H::World)` a passing donor phase left behind.
        // Inside this monomorphized closure the pair is fully typed, so no erasure or
        // serialization is involved; the slot is invisible to the harness and the setup fn.
        let session: SessionSlot<H::World> = Rc::new(RefCell::new(None));

        let run: RunFn = Box::new(
            move |resolved: &ResolvedProfile,
                  opts: &RunOptions|
                  -> LocalBoxFuture<'_, Result<ErasedReport, RunError>> {
                let harness = Rc::clone(&harness);
                let setup = Rc::clone(&setup);
                let harness_name = harness_name.clone();
                let export = export.clone();
                let session = Rc::clone(&session);
                Box::pin(async move {
                    run_profile::<H, F, S>(
                        &harness,
                        &setup,
                        harness_name,
                        resolved,
                        opts,
                        export.as_ref(),
                        &session,
                    )
                    .await
                })
            },
        );

        self.entries
            .insert(name.to_string(), Entry { validate, run });
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
pub(super) enum KindSelection<K> {
    /// Neither `kinds` nor `weights` was set: draw from every kind, weighted per draw by
    /// `Harness::weight` (uniform under the default weight of 1).
    All,
    /// `kinds` was set: draw over this subset, weighted per draw by `Harness::weight`.
    Restricted(Vec<K>),
    /// `weights` was set: static per-kind weights, in sorted-kind-name order, multiplied per
    /// draw by `Harness::weight`.
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
/// 6.1 (`weights` beats `kinds`; both compose with the harness's dynamic `Harness::weight`
/// (static times dynamic)); `cross-vm-config`'s structural validation already rejects a profile
/// that sets both, so this is belt and suspenders.
pub(super) fn parse_kind_selection<H: Harness<Ctx = crate::harness::Ctx>>(
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
fn parse_kind<H: Harness<Ctx = crate::harness::Ctx>>(
    name: &str,
) -> Result<H::OpKind, ValidationError>
where
    H::OpKind: DeserializeOwned,
{
    H::OpKind::deserialize(toml::Value::String(name.to_string()))
        .map_err(|e| ValidationError(e.to_string()))
}

/// Type-checks one profile's `kinds`/`weights`/scenario `op`s against `H`, without running or
/// touching a chain. Powers [`Registry::validate`].
fn validate_profile<H: Harness<Ctx = crate::harness::Ctx>>(
    profile: &cross_vm_config::Profile,
) -> Result<(), ValidationError>
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

/// Rejects a scenario profile whose `export_world` is set against a harness that cannot export
/// (registered via [`Registry::register`], not [`Registry::register_persistent`]).
/// `export_capable` is `true` only when the registering closure built a real world-exporter
/// (i.e. the harness was registered with `register_persistent`). Every other mode's profile has
/// no `export_world` key at all, so only the `Profile::Scenario` arm is ever inspected. Powers
/// [`Registry::validate`]; the scenario run arm re-checks the same condition at run time (spec:
/// `validate` catches it offline, `run` enforces it even when a caller skips `validate`).
fn validate_export_world(
    profile: &cross_vm_config::Profile,
    export_capable: bool,
) -> Result<(), ValidationError> {
    if let cross_vm_config::Profile::Scenario(p) = profile {
        if p.export_world.is_some() && !export_capable {
            return Err(ValidationError(
                "export_world requires this harness to be registered with \
                 Registry::register_persistent (H::World: Serialize), not register"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// Serializes `world` as pretty JSON to `path`, creating any missing parent directories first.
/// Built only in [`Registry::register_persistent`] (as `export_world_json::<H::World>`), where
/// the `H::World: Serialize` bound this needs is in scope; [`Registry::register`] never
/// constructs a [`WorldExportFn`] at all, so this function is never reachable from a
/// non-persistent registration.
fn export_world_json<W: serde::Serialize>(world: &W, path: &Path) -> Result<(), RunError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                RunError::Export(format!(
                    "failed to create export_world parent dir {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    let json = serde_json::to_string_pretty(world)
        .map_err(|e| RunError::Export(format!("failed to serialize World: {e}")))?;
    std::fs::write(path, json)
        .map_err(|e| RunError::Export(format!("failed to write {}: {e}", path.display())))?;
    tracing::info!(path = %path.display(), "wrote export_world");
    Ok(())
}

/// Resolves a profile's `SeedSpec` to a concrete `u64`, once per profile run: `Fixed(n)` is used
/// verbatim, `Random` draws a fresh seed and logs it so the run can be reproduced.
pub(super) fn resolve_base_seed(seed: cross_vm_config::SeedSpec) -> u64 {
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
pub(super) fn build_setup_request(resolved: &ResolvedProfile, seed: u64) -> SetupRequest {
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
    H: Harness<Ctx = crate::harness::Ctx>,
    F: Fn() -> H,
    S: Fn(SetupRequest) -> SetupFuture<'static, H::World>,
{
    if !resolved.shrink || report.failure.is_none() {
        return (report, false);
    }
    // A shrink rebuild always starts from a *fresh* setup (see `rebuild` below), so it can never
    // reproduce the starting state an `Inherit` phase was handed by its donor. Shrinking under a
    // different starting world would compare apples to oranges, so an inherited phase is forced
    // to skip shrinking entirely, keeping the raw failing history intact.
    if resolved.world_source == cross_vm_config::WorldSource::Inherit {
        tracing::warn!(
            "shrink disabled: a shrink rebuild starts from a fresh setup and would not \
             reproduce the inherited starting state"
        );
        return (report, false);
    }
    let failure = report.failure.take().expect("checked is_some above");
    let history = failure.history;

    let rebuild = || {
        let req = build_setup_request(resolved, seed);
        async move { setup(req).await.expect("shrink rebuild: setup failed") }
    };

    let mut runner = Runner::<H, Scenario>::scenario(make_harness(), seed);
    let shrunk_history = runner
        .shrink_with_limit(
            history,
            resolved.check_every,
            resolved.shrink_limit,
            rebuild,
        )
        .await;

    report.failure = Some(Failure {
        history: shrunk_history,
        ..failure
    });
    (report, true)
}

/// Drives exactly one fuzz case: sub-seeds `base_seed` by `case`, builds a fresh `(Ctx, World)`
/// via `setup`, runs one `Runner::fuzz` over `ops` operations under `selection`, and returns the
/// resulting `(RunReport, Option<Stats>, concrete seed)`.
///
/// Factored out of [`run_profile`]'s `Profile::Fuzz` arm so that arm's loop and
/// [`crate::config::test_bridge::run_profile_for_test`] (the `#[config_runner]` bridge) both
/// call the exact same setup/seed/run sequence for a given case — this is what keeps the fuzz
/// golden stream (spec section 5's seeded RNG guarantee) identical whether a case is driven via
/// `cross-vm run`/`Registry::run` or via a `#[config_runner]`-generated `#[tokio::test]`. Neither
/// caller may reimplement any piece of this sequence itself.
pub(super) async fn run_one_fuzz_case<H, F, S>(
    make_harness: &F,
    setup: &S,
    resolved: &ResolvedProfile,
    selection: &KindSelection<H::OpKind>,
    ops: usize,
    base_seed: u64,
    case: usize,
) -> Result<(RunReport<H::Operation>, Option<Stats>, u64), RunError>
where
    H: Harness<Ctx = crate::harness::Ctx>,
    F: Fn() -> H,
    S: Fn(SetupRequest) -> SetupFuture<'static, H::World>,
{
    let seed_i = sub_seed(base_seed, case);
    tracing::info!(case, seed = seed_i, "fuzz case starting");

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
    Ok((report, stats, seed_i))
}

/// Obtains the starting `(Ctx, H::World)` for one generative or scenario phase, honoring the
/// profile's `world_source` (spec: the pipeline handoff).
///
/// `Fresh` builds a brand-new pair from `setup` (mapping any failure to [`RunError::Setup`]).
/// `Inherit` takes the pair a previous donor phase stashed in `session`, consuming it (the slot
/// is emptied); an empty slot is [`RunError::Invalid`], since a phase cannot inherit a world no
/// donor left behind (the donor did not run, did not pass, or its world was already consumed).
async fn obtain_start_state<H, S>(
    setup: &S,
    session: &SessionSlot<H::World>,
    resolved: &ResolvedProfile,
    seed: u64,
) -> Result<(Ctx, H::World), RunError>
where
    H: Harness<Ctx = crate::harness::Ctx>,
    S: Fn(SetupRequest) -> SetupFuture<'static, H::World>,
{
    match resolved.world_source {
        cross_vm_config::WorldSource::Fresh => setup(build_setup_request(resolved, seed))
            .await
            .map_err(|e| RunError::Setup(e.to_string())),
        cross_vm_config::WorldSource::Inherit => session.borrow_mut().take().ok_or_else(|| {
            RunError::Invalid(
                "phase inherits a world, but no donor world is available \
                 (the donor phase did not run, did not pass, or was already consumed)"
                    .to_string(),
            )
        }),
    }
}

/// The generic body every registered harness's `run` closure calls into (spec section 7's `run`
/// bullet list). No `dyn Harness` exists here: `H`, `F`, `S` are all concrete at the call site.
#[allow(clippy::too_many_arguments)]
async fn run_profile<H, F, S>(
    make_harness: &F,
    setup: &S,
    harness_name: String,
    resolved: &ResolvedProfile,
    opts: &RunOptions,
    export: Option<&WorldExportFn<H::World>>,
    session: &SessionSlot<H::World>,
) -> Result<ErasedReport, RunError>
where
    H: Harness<Ctx = crate::harness::Ctx>,
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

            // Pipeline handoff (inherit a donor world, or stash this world for the next phase)
            // needs a single, well-defined starting/ending world. Fuzz does a *fresh* setup per
            // case, so a multi-case fuzz phase has neither: reject it here as a run-layer backstop
            // (the config layer rejects it too). A `cases == 1` fuzz phase does have exactly one,
            // so it can take part in the handoff.
            if (resolved.world_source == cross_vm_config::WorldSource::Inherit
                || resolved.stash_world)
                && cases != 1
            {
                return Err(RunError::Invalid(
                    "pipeline handoff requires fuzz cases = 1 (fuzz does a fresh setup per case)"
                        .to_string(),
                ));
            }

            // The `cases == 1` handoff path drives the single case inline (rather than through
            // `run_one_fuzz_case`, which always does a fresh setup and cannot surface the runner
            // for stashing), so the obtained start state can be inherited and the ending world
            // stashed. The seed derivation `sub_seed(base_seed, 0)` is identical to case 0 of the
            // loop, so a stashing single-case run stays byte-identical to a plain one.
            if resolved.world_source == cross_vm_config::WorldSource::Inherit || resolved.stash_world
            {
                let seed_i = sub_seed(base_seed, 0);
                tracing::info!(case = 0, seed = seed_i, "fuzz case starting");

                let (ctx, world) =
                    obtain_start_state::<H, S>(setup, session, resolved, seed_i).await?;

                let mut runner = Runner::<H, Fuzz>::fuzz(make_harness(), seed_i);
                if resolved.stats {
                    runner.with_stats();
                }
                runner.setup(ctx, world);
                let report = runner
                    .run_with(ops, selection.as_mix(), resolved.check_every)
                    .await;
                let stats = runner.stats().cloned();

                let (report, shrunk) =
                    maybe_shrink(report, make_harness, setup, resolved, seed_i).await;

                // Hand a passing world to the next phase when the profile opted into stashing.
                if resolved.stash_world && report.passed() {
                    let (ctx, world) = runner.into_parts();
                    *session.borrow_mut() = Some((ctx, world));
                }
                return erase_report(
                    report,
                    harness_name,
                    resolved.name.clone(),
                    "fuzz".to_string(),
                    stats,
                    started.elapsed(),
                    shrunk,
                )
                .map_err(|e| RunError::Serialize(e.to_string()));
            }

            // The first failing case ends the profile; if every case passes, the last case's
            // report stands in for the profile (there is no single meaningful "combined" report
            // across independent cases, and the last case is as representative as any — see the
            // task report for the alternative considered and rejected).
            let mut last: Option<(RunReport<H::Operation>, Option<Stats>, u64)> = None;
            for case in 0..cases {
                let (report, stats, seed_i) = run_one_fuzz_case(
                    make_harness,
                    setup,
                    resolved,
                    &selection,
                    ops,
                    base_seed,
                    case,
                )
                .await?;
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

            let (ctx, world) =
                obtain_start_state::<H, S>(setup, session, resolved, base_seed).await?;

            let mut runner = Runner::<H, Invariant>::invariant(make_harness(), base_seed);
            if resolved.stats {
                runner.with_stats();
            }
            runner.setup(ctx, world);
            let report = runner
                .run_with(ops, selection.as_mix(), resolved.check_every)
                .await;
            let stats = runner.stats().cloned();

            let (report, shrunk) =
                maybe_shrink(report, make_harness, setup, resolved, base_seed).await;

            // Hand a passing world to the next phase when the profile opted into stashing.
            if resolved.stash_world && report.passed() {
                let (ctx, world) = runner.into_parts();
                *session.borrow_mut() = Some((ctx, world));
            }
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

            let (ctx, world) =
                obtain_start_state::<H, S>(setup, session, resolved, base_seed).await?;

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
            let (report, shrunk) =
                maybe_shrink(report, make_harness, setup, resolved, base_seed).await;

            // Hand a passing world to the next phase when the profile opted into stashing.
            if resolved.stash_world && report.passed() {
                let (ctx, world) = runner.into_parts();
                *session.borrow_mut() = Some((ctx, world));
            }
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
            // Enforced here too (not just `validate`): `cross-vm run` never requires a prior
            // `cross-vm validate` call, so a harness that cannot export must still fail loudly
            // before touching a chain, not silently drop the key.
            if p.export_world.is_some() && export.is_none() {
                return Err(RunError::Invalid(
                    "profile sets export_world, but this harness was registered with \
                     `register`, not `register_persistent`; export_world requires \
                     `H::World: Serialize`"
                        .to_string(),
                ));
            }

            let (ctx, world) =
                obtain_start_state::<H, S>(setup, session, resolved, base_seed).await?;

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

            // Take the final `(Ctx, World)` out of the runner exactly once: export borrows
            // `&world` first, then the pair is handed to the next phase when the profile opted
            // into stashing (a passing run only). `into_parts` is only called when at least one
            // of the two actually needs the pair, leaving the no-op path untouched.
            let stash = resolved.stash_world && report.passed();
            if p.export_world.is_some() || stash {
                let (ctx, world) = runner.into_parts();
                // Serialize the final World *after* the run, whether it passed or failed, so the
                // exported file always reflects the state the run actually reached. Only
                // reachable when `export_world` is set (checked above), which in turn only holds
                // when `export` is `Some` (checked above too).
                if let Some(path) = &p.export_world {
                    let exporter =
                        export.expect("checked above: export_world implies register_persistent");
                    (exporter.as_ref())(&world, Path::new(path))?;
                }
                if stash {
                    *session.borrow_mut() = Some((ctx, world));
                }
            }

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
        type Ctx = Ctx;
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

        fn generate_op(
            &self,
            _rng: &mut Prng,
            _world: &Self::World,
            kind: Self::OpKind,
        ) -> Self::Operation {
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
            world_source: cross_vm_config::WorldSource::Fresh,
            stash_world: false,
        }
    }

    /// [`resolved`] with `shrink`/`shrink_limit` overridden; every other field matches.
    fn resolved_with_shrink(
        profile: Profile,
        shrink: bool,
        shrink_limit: usize,
    ) -> ResolvedProfile {
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

    /// [`scenario_profile`] with `export_world` set to `path`.
    fn scenario_profile_with_export(
        steps: Vec<cross_vm_config::ScenarioStepRaw>,
        path: &str,
    ) -> Profile {
        Profile::Scenario(cross_vm_config::ScenarioProfile {
            common: common(),
            steps,
            export_world: Some(path.to_string()),
        })
    }

    /// A fresh, gitignored export path under `<CARGO_MANIFEST_DIR>/tests_result/`, unique per test
    /// invocation (process id plus a monotonic counter), so parallel test runs never collide and
    /// nothing leaks into a source-tree `target/` dir. `export_world_json` creates the parent.
    fn temp_export_path(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests_result")
            .join(format!(
                "cross-vm-registry-export-world-{}-{}-{label}.json",
                std::process::id(),
                n
            ))
    }

    /// A step whose op is a bare unit-variant name (`MockOp` has no data, so a plain TOML string
    /// deserializes into it exactly like `H::OpKind::deserialize` does for `kinds`/`weights`).
    fn mock_step(op: &str, expect: cross_vm_config::ExpectStr) -> cross_vm_config::ScenarioStepRaw {
        cross_vm_config::ScenarioStepRaw {
            op: serde_json::Value::String(op.to_string()),
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

    // ----- register_persistent + export_world (Task 12b) -----
    //
    // `MockHarness::World` is a plain `u32`, already `Serialize`, so it doubles as the
    // "persistent" harness these tests need without introducing a second mock: `apply` increments
    // `world` by 1 per applied op (both Ping/Pong are accepted), and `mock_setup` seeds it from
    // `req.seed`, so the final exported value is deterministic (`seed + accepted-op-count`).

    #[tokio::test]
    async fn register_persistent_scenario_run_exports_final_world_as_json() {
        let mut registry = Registry::new();
        registry.register_persistent("mock", || MockHarness, mock_setup);

        let path = temp_export_path("passing");
        let steps = vec![
            mock_step("Ping", cross_vm_config::ExpectStr::Accepted),
            mock_step("Pong", cross_vm_config::ExpectStr::Accepted),
        ];
        let resolved = resolved(scenario_profile_with_export(steps, path.to_str().unwrap()));

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run ok");
        assert!(report.failure.is_none(), "{:?}", report.failure);

        // `resolved()` fixes the seed at 7 (`SeedSpec::Fixed(7)`); two accepted ops each bump the
        // mock `World` (`u32`) by 1, so the exported value must be exactly 9.
        let raw = std::fs::read_to_string(&path).expect("export_world wrote the file");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(value, serde_json::json!(9));

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn register_persistent_scenario_run_exports_even_on_failure() {
        let mut registry = Registry::new();
        registry.register_persistent("mock", || MockHarness, mock_setup);

        let path = temp_export_path("failing");
        // Ping is always accepted; expecting a rejection fails the step, but export_world must
        // still write the state the run actually reached.
        let steps = vec![mock_step("Ping", cross_vm_config::ExpectStr::Rejected)];
        let resolved = resolved(scenario_profile_with_export(steps, path.to_str().unwrap()));

        let report = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .expect("run resolves to a report, not a RunError");
        assert!(report.failure.is_some());

        let raw =
            std::fs::read_to_string(&path).expect("export_world wrote the file even on failure");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(value, serde_json::json!(8), "seed 7 + one applied Ping");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn plain_register_scenario_with_export_world_fails_validate() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let steps = vec![mock_step("Ping", cross_vm_config::ExpectStr::Accepted)];
        let profile = scenario_profile_with_export(steps, "somewhere.json");

        let err = registry.validate("mock", &profile).unwrap_err();
        match err {
            RunError::Validation(e) => {
                assert!(e.to_string().contains("register_persistent"), "{e}");
            }
            other => panic!("expected RunError::Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn plain_register_scenario_with_export_world_fails_run() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        let path = temp_export_path("rejected-by-plain-register");
        let steps = vec![mock_step("Ping", cross_vm_config::ExpectStr::Accepted)];
        let resolved = resolved(scenario_profile_with_export(steps, path.to_str().unwrap()));

        let err = registry
            .run("mock", &resolved, &RunOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Invalid(_)), "{err:?}");
        assert!(
            !path.exists(),
            "a rejected run must never write the export file"
        );
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

        let report = registry
            .run("mock", &resolved, &opts)
            .await
            .expect("run ok");
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

        let report = registry
            .run("mock", &resolved, &opts)
            .await
            .expect("run ok");
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
        assert_eq!(
            shrunk_len, 1,
            "a lone Boom already reproduces; must shrink to it"
        );
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

    // ----- pipeline handoff (session slot) -----

    /// [`resolved`] with `world_source` / `stash_world` overridden; every other field matches.
    fn resolved_pipeline(
        profile: Profile,
        world_source: cross_vm_config::WorldSource,
        stash_world: bool,
    ) -> ResolvedProfile {
        ResolvedProfile {
            world_source,
            stash_world,
            ..resolved(profile)
        }
    }

    #[tokio::test]
    async fn passing_donor_stashes_and_inheritor_continues_from_it() {
        let mut registry = Registry::new();
        registry.register_persistent("mock", || MockHarness, mock_setup);

        // Donor: invariant, 3 Ping ops, seed fixed at 7 by `resolved` -> world 7 + 3 = 10.
        let donor = resolved_pipeline(
            invariant_profile(3, Some(vec!["Ping".to_string()])),
            cross_vm_config::WorldSource::Fresh,
            true,
        );
        let report = registry
            .run("mock", &donor, &RunOptions::default())
            .await
            .unwrap();
        assert!(report.failure.is_none());

        // Inheritor: scenario with one Ping, exporting the final world. Starting from the
        // stashed 10 (NOT from a fresh setup's seed), the export must be 11.
        let path = temp_export_path("inherited");
        let steps = vec![mock_step("Ping", cross_vm_config::ExpectStr::Accepted)];
        let inheritor = resolved_pipeline(
            scenario_profile_with_export(steps, path.to_str().unwrap()),
            cross_vm_config::WorldSource::Inherit,
            false,
        );
        let report = registry
            .run("mock", &inheritor, &RunOptions::default())
            .await
            .unwrap();
        assert!(report.failure.is_none());
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!(11), "10 stashed by donor, +1 Ping");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn inherit_with_empty_slot_is_invalid() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);
        let inheritor = resolved_pipeline(
            invariant_profile(1, Some(vec!["Ping".to_string()])),
            cross_vm_config::WorldSource::Inherit,
            false,
        );
        let err = registry
            .run("mock", &inheritor, &RunOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Invalid(_)), "{err:?}");
    }

    #[tokio::test]
    async fn failing_donor_stashes_nothing() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        // Donor: invariant over Boom (always a Bug). stash_world = true, but the run fails, so
        // nothing is stashed.
        let donor = resolved_pipeline(
            invariant_profile(1, Some(vec!["Boom".to_string()])),
            cross_vm_config::WorldSource::Fresh,
            true,
        );
        let report = registry
            .run("mock", &donor, &RunOptions::default())
            .await
            .unwrap();
        assert!(report.failure.is_some());

        // The inheritor must get RunError::Invalid because the slot stayed empty.
        let inheritor = resolved_pipeline(
            invariant_profile(1, Some(vec!["Ping".to_string()])),
            cross_vm_config::WorldSource::Inherit,
            false,
        );
        let err = registry
            .run("mock", &inheritor, &RunOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Invalid(_)), "{err:?}");
    }

    #[tokio::test]
    async fn multi_case_fuzz_cannot_inherit_or_stash() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        // fuzz cases=5 with world_source=Inherit -> RunError::Invalid
        let inheriting = resolved_pipeline(
            fuzz_profile(5, 1, Some(vec!["Ping".to_string()]), None),
            cross_vm_config::WorldSource::Inherit,
            false,
        );
        let err = registry
            .run("mock", &inheriting, &RunOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Invalid(_)), "{err:?}");

        // fuzz cases=5 with stash_world=true -> RunError::Invalid
        let stashing = resolved_pipeline(
            fuzz_profile(5, 1, Some(vec!["Ping".to_string()]), None),
            cross_vm_config::WorldSource::Fresh,
            true,
        );
        let err = registry
            .run("mock", &stashing, &RunOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Invalid(_)), "{err:?}");
    }

    #[tokio::test]
    async fn single_case_fuzz_participates_in_handoff() {
        let mut registry = Registry::new();
        registry.register_persistent("mock", || MockHarness, mock_setup);

        // fuzz cases=1, ops=2, kinds=["Ping"], stash_world=true. The single case does a fresh
        // setup at sub_seed(7, 0), so world = sub_seed(7, 0) as u32 + 2 accepted ops.
        let donor = resolved_pipeline(
            fuzz_profile(1, 2, Some(vec!["Ping".to_string()]), None),
            cross_vm_config::WorldSource::Fresh,
            true,
        );
        let report = registry
            .run("mock", &donor, &RunOptions::default())
            .await
            .unwrap();
        assert!(report.failure.is_none());

        // Inheritor scenario with one Ping: exported value = donor world + 1.
        let path = temp_export_path("fuzz-handoff");
        let steps = vec![mock_step("Ping", cross_vm_config::ExpectStr::Accepted)];
        let inheritor = resolved_pipeline(
            scenario_profile_with_export(steps, path.to_str().unwrap()),
            cross_vm_config::WorldSource::Inherit,
            false,
        );
        let report = registry
            .run("mock", &inheritor, &RunOptions::default())
            .await
            .unwrap();
        assert!(report.failure.is_none());
        let expected = sub_seed(7, 0) as u32 + 2 + 1;
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!(expected));
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn inherited_phase_forces_shrink_off() {
        let mut registry = Registry::new();
        registry.register("mock", || MockHarness, mock_setup);

        // Stash a world with a passing donor.
        let donor = resolved_pipeline(
            invariant_profile(3, Some(vec!["Ping".to_string()])),
            cross_vm_config::WorldSource::Fresh,
            true,
        );
        let report = registry
            .run("mock", &donor, &RunOptions::default())
            .await
            .unwrap();
        assert!(report.failure.is_none());

        // Inheritor invariant over Boom with shrink=true: a shrink rebuild would start from a
        // fresh setup, not the inherited world, so shrink is forced off. The failure stands but
        // is not marked shrunk.
        let inheritor = ResolvedProfile {
            world_source: cross_vm_config::WorldSource::Inherit,
            ..resolved_with_shrink(
                invariant_profile(3, Some(vec!["Boom".to_string()])),
                true,
                256,
            )
        };
        let report = registry
            .run("mock", &inheritor, &RunOptions::default())
            .await
            .unwrap();
        let failure = report.failure.expect("must fail");
        assert!(!failure.shrunk, "inherited phase must not shrink");
    }
}
