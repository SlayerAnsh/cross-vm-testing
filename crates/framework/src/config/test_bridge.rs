//! [`run_profile_for_test`]: the `cargo test` bridge from a `*.cross-vm.toml` config to a plain
//! `#[tokio::test]` body, used by the `#[config_runner]` proc-macro
//! (`crates/macros/src/config_runner.rs`).
//!
//! The macro reads the config file at **expansion** time only to learn how many `#[tokio::test]`
//! fns to fan out for a fuzz profile (one per case, mirroring `#[fuzz_runner]`); it never trusts
//! that count at run time. [`run_profile_for_test`] reloads the same file at **run** time (via
//! [`cross_vm_config::load`], the same P1 loader every other entry point uses) and is the actual
//! source of truth: for a fuzz case it re-asserts the freshly loaded profile's `cases` still
//! matches what the macro saw at compile time, and panics with a clear "rebuild" message if the
//! config changed since the last `cargo build` regenerated the macro's fan-out. This is the same
//! staleness class every compiled-in constant has (a `const` computed from a file needs a
//! rebuild too); the assertion just turns a silent under/over-count into a loud test failure
//! instead of a subtly wrong fan-out.
//!
//! For a fuzz case, driving the case itself reuses
//! [`crate::config::registry::run_one_fuzz_case`] — the exact function
//! [`Registry::run`](super::Registry::run)'s fuzz arm calls per case — so the seeded op sequence
//! a `#[config_runner]` test drives is byte-for-byte identical to what `cross-vm run` would drive
//! for the same case (see that function's docs for why this matters: the fuzz golden stream must
//! never depend on which caller drove it). For any other mode, this function goes through a
//! fresh, single-harness [`Registry`] exactly as `cross-vm run` does.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::harness::Harness;

use super::registry::{self, Registry};
use super::resolve::{resolve_profile, RunOptions};
use super::setup_request::{SetupFuture, SetupRequest};

/// Loads `config_path`, resolves `profile`, and runs it as a `#[tokio::test]` body: panics on
/// any failure encountered (setup, resolve, or a discovered bug), so a failing run fails the
/// test. Reuses the P1 loader ([`cross_vm_config::load`]) and the P2 registry
/// ([`Registry`]/[`registry::run_one_fuzz_case`]) end to end; no run logic is reimplemented here.
///
/// `harness` builds a fresh `H` per run (matching [`Registry::register`]'s `F: Fn() -> H`
/// bound); `setup` builds the live `(Ctx, H::World)` from a [`SetupRequest`]. Only the plain
/// [`Registry::register`] bound is used here (never `register_persistent`): a config-driven test
/// that needs `export_world` should go through the `cross-vm` CLI (Task 12b), not this bridge.
///
/// `case`: for a fuzz profile, drives only that case index (seed = `sub_seed(base, case)`, via
/// [`registry::run_one_fuzz_case`]); `None` runs the whole profile through a fresh [`Registry`]
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
///   includes the [`crate::harness::FailureKind`] and the failing op's `Debug` rendering.
pub async fn run_profile_for_test<H, F, S>(
    config_path: &str,
    harness: F,
    setup: S,
    profile: &str,
    case: Option<usize>,
    expected_cases: Option<usize>,
) where
    H: Harness + 'static,
    H::Operation: Serialize + DeserializeOwned + 'static,
    H::OpKind: Serialize + DeserializeOwned + Copy + 'static,
    F: Fn() -> H + 'static,
    S: Fn(SetupRequest) -> SetupFuture<'static, H::World> + 'static,
{
    let cfg = cross_vm_config::load(Path::new(config_path), &|k| std::env::var(k).ok())
        .unwrap_or_else(|e| panic!("run_profile_for_test: failed to load `{config_path}`: {e}"));
    let resolved = resolve_profile(&cfg, profile, &RunOptions::default()).unwrap_or_else(|e| {
        panic!("run_profile_for_test: failed to resolve profile `{profile}`: {e}")
    });

    match case {
        Some(i) => {
            let cross_vm_config::Profile::Fuzz(p) = &resolved.profile else {
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
            let selection = registry::parse_kind_selection::<H>(&p.kinds, &p.weights)
                .unwrap_or_else(|e| panic!("run_profile_for_test: {e}"));
            let base_seed = registry::resolve_base_seed(resolved.seed);

            let (report, _stats, seed_i) = registry::run_one_fuzz_case(
                &harness, &setup, &resolved, &selection, ops, base_seed, i,
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
                .run(profile, &resolved, &RunOptions::default())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{CheckOutcome, Ctx, HarnessError, Prng, Verdict};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ----- a minimal mock harness, identical in shape to registry.rs's/cli.rs's own mocks: no
    // real chain interaction, just enough to drive `run_one_fuzz_case`/`Registry::run`. -----

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    enum MockKind {
        Ping,
        Boom,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    enum MockOp {
        Ping,
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
                MockOp::Ping => Ok(Verdict::Accepted),
                MockOp::Boom => Err(HarnessError::Bug("boom".to_string())),
            }
        }

        fn op_kinds(&self) -> Vec<Self::OpKind> {
            vec![MockKind::Ping, MockKind::Boom]
        }

        fn generate_op(
            &self,
            _rng: &mut Prng,
            _world: &Self::World,
            kind: Self::OpKind,
        ) -> Self::Operation {
            match kind {
                MockKind::Ping => MockOp::Ping,
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

    /// A fresh temp config path, unique per test invocation (process id plus a monotonic
    /// counter), so parallel test runs never collide. Mirrors `registry.rs`'s
    /// `temp_export_path` fixture helper.
    fn write_temp_config(label: &str, body: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cross-vm-test-bridge-{}-{}-{label}.toml",
            std::process::id(),
            n
        ));
        std::fs::write(&path, body).expect("write temp config");
        path
    }

    const FUZZ_PASSING: &str = r#"
[harness]
name = "mock"

[profile.smoke]
mode = "fuzz"
cases = 3
ops = 2
kinds = ["Ping"]
"#;

    const FUZZ_FAILING: &str = r#"
[harness]
name = "mock"

[profile.smoke]
mode = "fuzz"
cases = 3
ops = 1
kinds = ["Boom"]
"#;

    const INVARIANT_PASSING: &str = r#"
[harness]
name = "mock"

[profile.inv]
mode = "invariant"
ops = 3
kinds = ["Ping"]
"#;

    #[tokio::test]
    async fn fuzz_case_that_passes_does_not_panic() {
        let path = write_temp_config("passing", FUZZ_PASSING);
        run_profile_for_test(
            path.to_str().unwrap(),
            || MockHarness,
            mock_setup,
            "smoke",
            Some(0),
            Some(3),
        )
        .await;
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    #[should_panic(expected = "boom")]
    async fn fuzz_case_that_fails_panics_with_failure_detail() {
        let path = write_temp_config("failing", FUZZ_FAILING);
        run_profile_for_test(
            path.to_str().unwrap(),
            || MockHarness,
            mock_setup,
            "smoke",
            Some(0),
            Some(3),
        )
        .await;
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    #[should_panic(expected = "config changed since compile, rebuild")]
    async fn expected_cases_mismatch_panics_with_rebuild_message() {
        let path = write_temp_config("mismatch", FUZZ_PASSING);
        // The macro "saw" 8 cases at compile time; the config on disk now has 3.
        run_profile_for_test(
            path.to_str().unwrap(),
            || MockHarness,
            mock_setup,
            "smoke",
            Some(0),
            Some(8),
        )
        .await;
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn no_case_index_runs_the_whole_profile() {
        let path = write_temp_config("invariant", INVARIANT_PASSING);
        run_profile_for_test(
            path.to_str().unwrap(),
            || MockHarness,
            mock_setup,
            "inv",
            None,
            None,
        )
        .await;
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    #[should_panic(expected = "is not a fuzz profile")]
    async fn case_index_against_a_non_fuzz_profile_panics() {
        let path = write_temp_config("non-fuzz-case", INVARIANT_PASSING);
        run_profile_for_test(
            path.to_str().unwrap(),
            || MockHarness,
            mock_setup,
            "inv",
            Some(0),
            None,
        )
        .await;
        std::fs::remove_file(&path).ok();
    }
}
