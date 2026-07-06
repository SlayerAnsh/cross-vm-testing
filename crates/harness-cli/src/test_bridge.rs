//! [`run_profile_for_test`]: the `cargo test` bridge from a config file to a plain
//! `#[tokio::test]` body, used by the `#[config_runner]` proc-macro
//! (`crates/macros/src/config_runner.rs`).
//!
//! The macro reads the config file at **expansion** time only to learn how many `#[tokio::test]`
//! fns to fan out for a fuzz profile (one per case, mirroring `#[fuzz_runner]`); it never trusts
//! that count at run time. [`run_profile_for_test`] reloads the same file at **run** time (via
//! [`harness_config::load`], the same loader every other entry point uses) and is the actual
//! source of truth: for a fuzz case it re-asserts the freshly loaded profile's `cases` still
//! matches what the macro saw at compile time, and panics with a clear "rebuild" message if the
//! config changed since the last `cargo build` regenerated the macro's fan-out. This is the same
//! staleness class every compiled-in constant has (a `const` computed from a file needs a
//! rebuild too); the assertion just turns a silent under/over-count into a loud test failure
//! instead of a subtly wrong fan-out.
//!
//! For a fuzz case, driving the case itself reuses `crate::registry::run_one_fuzz_case` — the
//! exact function [`Registry::run`]'s fuzz arm calls per case — so the seeded op sequence a
//! `#[config_runner]` test drives is byte-for-byte identical to what a CLI `run` would drive for
//! the same case (see that function's docs for why this matters: the fuzz golden stream must
//! never depend on which caller drove it). For any other mode, this function goes through a
//! fresh, single-harness [`Registry`] exactly as a CLI `run` does.

use std::path::Path;

use harness_core::ConfigOps;

use crate::domain::{CliDomain, SetupFuture};
use crate::registry::{self, Registry};
use crate::resolve::{resolve_profile, RunOptions};

/// Loads `config_path`, resolves `profile`, and runs it as a `#[tokio::test]` body: panics on
/// any failure encountered (setup, resolve, or a discovered bug), so a failing run fails the
/// test. Reuses the [`harness_config::load`] loader and the [`Registry`]
/// ([`Registry`]/`registry::run_one_fuzz_case`) end to end; no run logic is reimplemented here.
///
/// Generic over a [`CliDomain`] `D`: the config loads with `D`'s schema extension
/// ([`CliDomain::Ext`]) and each run's setup value is built via [`CliDomain::build_setup`].
/// **Config-driven tests always use `D::Args::default()`**: [`run_profile_for_test`] has no CLI
/// flag surface, so the default domain args stand in for the flags a live CLI `run` would parse.
///
/// `harness` builds a fresh `H` per run (matching [`Registry::register`]'s `F: Fn() -> H`
/// bound); `setup` builds the live `(H::Ctx, H::World)` from the domain setup value `D::Setup`.
/// Only the plain [`Registry::register`] bound is used here (never `register_persistent`): a
/// config-driven test that needs `export_world` should go through the CLI, not this bridge.
///
/// `case`: for a fuzz profile, drives only that case index (seed = `sub_seed(base, case)`, via
/// `registry::run_one_fuzz_case`); `None` runs the whole profile through a fresh [`Registry`]
/// (the shape every non-fuzz mode — invariant/scenario/endurance — always uses, and a fuzz
/// profile could too, though the `#[config_runner]` macro never emits that combination).
///
/// `expected_cases`: the fuzz profile's `cases` count the `#[config_runner]` macro read at
/// **compile** time. When `Some` and the freshly loaded profile's `cases` no longer matches,
/// this panics with "config changed since compile, rebuild" rather than silently running a stale
/// case count. Pass `None` to skip the check (e.g. from a hand-written call site that has no
/// compile-time count to compare against).
///
/// # Panics
/// - The config fails to load, or `profile` fails to resolve.
/// - `case` is `Some` but the resolved profile is not a fuzz profile.
/// - `expected_cases` is `Some` and no longer matches the resolved profile's `cases`.
/// - The driven run (whole profile or single case) reports a `failure`: the panic message
///   includes the [`harness_core::FailureKind`] and the failing op's `Debug` rendering.
pub async fn run_profile_for_test<D, H, F, SF>(
    config_path: &str,
    harness: F,
    setup: SF,
    profile: &str,
    case: Option<usize>,
    expected_cases: Option<usize>,
) where
    D: CliDomain,
    H: ConfigOps + 'static,
    H::Ctx: 'static,
    H::World: 'static,
    F: Fn() -> H + 'static,
    SF: Fn(D::Setup) -> SetupFuture<'static, H::Ctx, H::World> + 'static,
{
    let cfg = harness_config::load::<D::Ext>(Path::new(config_path), &|k| std::env::var(k).ok())
        .unwrap_or_else(|e| panic!("run_profile_for_test: failed to load `{config_path}`: {e}"));
    let resolved = resolve_profile(&cfg, profile, &RunOptions::default()).unwrap_or_else(|e| {
        panic!("run_profile_for_test: failed to resolve profile `{profile}`: {e}")
    });

    // Config-driven tests run with default domain args: `run_profile_for_test` has no CLI flag
    // surface, so `D::Args::default()` stands in for the flags a live CLI `run` would parse.
    let make_setup = |seed: u64| D::build_setup(&cfg, &resolved, &D::Args::default(), seed);

    match case {
        Some(i) => {
            let harness_config::Profile::Fuzz(p) = &resolved.profile else {
                panic!(
                    "run_profile_for_test: profile `{profile}` is not a fuzz profile, but a \
                     case index ({i}) was given"
                );
            };

            if let Some(expected) = expected_cases {
                if p.cases != expected {
                    panic!(
                        "config changed since compile, rebuild: profile `{profile}` now has \
                         {actual} cases, but the #[config_runner] expansion fanned out \
                         {expected} test(s)",
                        actual = p.cases
                    );
                }
            }

            let ops = p.ops;
            let codec = harness();
            let selection = registry::parse_kind_selection(&codec, &p.kinds, &p.weights)
                .unwrap_or_else(|e| panic!("run_profile_for_test: {e}"));
            let base_seed = registry::resolve_base_seed(resolved.seed);

            let (report, _stats, seed_i) = registry::run_one_fuzz_case(
                &harness,
                &setup,
                &make_setup,
                &resolved,
                &selection,
                ops,
                base_seed,
                i,
            )
            .await
            .unwrap_or_else(|e| panic!("run_profile_for_test: {e}"));

            if let Some(failure) = report.failure {
                panic!(
                    "profile `{profile}` case {i} (seed {seed_i}) failed: {:?}\n  op: {:?}",
                    failure.kind, failure.op
                );
            }
        }
        None => {
            let mut registry = Registry::new();
            registry.register(profile, harness, setup);
            let report = registry
                .run(profile, &resolved, &RunOptions::default(), &make_setup)
                .await
                .unwrap_or_else(|e| panic!("run_profile_for_test: {e}"));

            if let Some(failure) = report.failure {
                panic!(
                    "profile `{profile}` failed: {:?}\n  op: {:?}",
                    failure.kind, failure.op_debug
                );
            }
        }
    }
}
