//! The cross-vm CLI: [`harness_cli`]'s generic [`Cli`](harness_cli::Cli) fixed to
//! [`CrossVmDomain`](crate::config::CrossVmDomain).
//!
//! Everything that used to live here (the `run` / `validate` / `list` subcommands, the
//! `CROSS_VM_*` env-var precedence folding, profile/suite selection, and the CI exit-code
//! contract) is now the domain-agnostic machinery in `harness-cli`; this module only pins the
//! generic builder to the cross-vm domain, so a registered setup fn receives the chain-aware
//! [`SetupRequest`](crate::config::SetupRequest). The domain-specific `--target`/`--target-chain`
//! flags and their parsers live in [`crate::config`]'s `domain` module.

use crate::config::CrossVmDomain;

/// The `cross-vm` CLI builder: [`harness_cli::Cli`] fixed to the cross-vm domain. See that type
/// for the full API (`new`, `env_file`, `register`, `register_persistent`, `main`); a registered
/// setup fn receives the cross-vm [`SetupRequest`](crate::config::SetupRequest) and returns a
/// [`SetupFuture`](crate::config::SetupFuture).
///
/// ```no_run
/// # async fn demo() -> std::process::ExitCode {
/// # use cross_vm_framework::cli::Cli;
/// # use cross_vm_framework::config::{SetupFuture, SetupRequest};
/// # use cross_vm_framework::harness::{Ctx, HarnessError};
/// # struct MyHarness;
/// # impl cross_vm_framework::harness::Harness for MyHarness {
/// #     type Ctx = Ctx;
/// #     type World = ();
/// #     type Operation = ();
/// #     type Invariant = ();
/// #     type OpKind = ();
/// #     async fn apply(&self, _: &mut Ctx, _: &mut (), _: &()) -> Result<cross_vm_framework::harness::Verdict, HarnessError> { unimplemented!() }
/// #     fn op_kinds(&self) -> Vec<()> { vec![] }
/// #     fn generate_op(&self, _: &mut cross_vm_framework::harness::Prng, _: &(), _: ()) -> () {}
/// #     fn invariants(&self) -> Vec<()> { vec![] }
/// #     async fn check(&self, _: &mut Ctx, _: &(), _: &()) -> cross_vm_framework::harness::CheckOutcome { unimplemented!() }
/// # }
/// # fn my_setup(_req: SetupRequest) -> SetupFuture<'static, ()> { unimplemented!() }
/// Cli::new()
///     .env_file(".env")
///     .register("my-harness", || MyHarness, my_setup)
///     .main()
///     .await
/// # }
/// ```
pub type Cli = harness_cli::Cli<CrossVmDomain>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SetupFuture, SetupRequest, Target, TargetArgs};
    use crate::harness::{CheckOutcome, Ctx, Harness, HarnessError, Prng, Verdict};
    use clap::Parser;
    use std::path::PathBuf;
    use std::rc::Rc;

    // -----------------------------------------------------------------------------------------
    // Domain-specific CLI flag parsing (`--target` / `--target-chain`).
    //
    // Only the cross-vm target flags are exercised here; the generic `run`/`validate`/`list`
    // argument surface (profile/suite/seed/ops/...) is parsed and tested inside `harness-cli`.
    // These flags are flattened into the generic `RunArgs` at the domain seam, so parsing them
    // through a tiny wrapper over `TargetArgs` mirrors exactly how the live CLI sees them.
    // -----------------------------------------------------------------------------------------

    /// A minimal clap command flattening the domain's [`TargetArgs`], so the `--target` /
    /// `--target-chain` value parsers can be driven directly in a test.
    #[derive(Debug, clap::Parser)]
    struct TargetArgsWrapper {
        #[command(flatten)]
        target: TargetArgs,
    }

    #[test]
    fn parses_a_target_chain_and_a_blanket_target() {
        let parsed = TargetArgsWrapper::try_parse_from([
            "cross-vm",
            "--target-chain",
            "eth=rpc",
            "--target",
            "mock",
        ])
        .expect("valid target flags");
        assert_eq!(
            parsed.target.target_chain,
            vec![("eth".to_string(), Target::Rpc)]
        );
        assert_eq!(parsed.target.target, Some(Target::Mock));
    }

    #[test]
    fn bad_target_chain_without_equals_is_a_parse_error() {
        let err = TargetArgsWrapper::try_parse_from(["cross-vm", "--target-chain", "ethrpc"])
            .unwrap_err();
        assert!(err.to_string().contains("LABEL=mock|rpc"), "{err}");
    }

    #[test]
    fn bad_target_value_is_a_parse_error() {
        let err = TargetArgsWrapper::try_parse_from(["cross-vm", "--target", "xyz"]).unwrap_err();
        assert!(err.to_string().contains("mock"), "{err}");
    }

    // -----------------------------------------------------------------------------------------
    // End-to-end smoke over a trivial cross-vm harness: pins the whole adapter stack (the
    // `CrossVmDomain` setup builder, the framework `Ctx`, and the registry run pipeline) that
    // this module's `Cli` alias fixes into place. The generic run/exit-code logic itself is
    // tested in `harness-cli`; here we only prove the cross-vm seam wires together.
    // -----------------------------------------------------------------------------------------

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

    #[test]
    fn cli_builds_with_a_mock_cross_vm_harness() {
        // Compile-time pin of the alias' `register` bound: a cross-vm setup fn returning the
        // framework `SetupFuture` (over `Ctx`) satisfies the generic
        // `Fn(D::Setup) -> harness_cli::SetupFuture<'static, H::Ctx, H::World>` bound with no
        // changes. This is the exact shape every `examples/*/src/bin/cross_vm.rs` relies on.
        let _cli: Cli = Cli::new().register("vault", || MockHarness, mock_setup);
    }

    /// A fresh, gitignored config-file path under `<CARGO_MANIFEST_DIR>/tests_result/`, unique per
    /// test invocation, so nothing leaks into a source-tree `target/` dir and parallel runs never
    /// collide.
    fn temp_config_path(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests_result")
            .join(format!(
                "cross-vm-cli-e2e-{}-{}-{label}.cross-vm.toml",
                std::process::id(),
                n
            ))
    }

    #[tokio::test]
    async fn mock_profile_runs_through_the_cross_vm_adapter_stack() {
        // Drive a trivial fuzz profile through the same cross-vm config bridge a
        // `#[config_runner]` test uses: it loads the config with the `CrossVmExt` schema, builds
        // each run's `SetupRequest` via `CrossVmDomain::build_setup`, and runs it against the
        // framework `Ctx`. `run_profile_for_test` panics on any failure, so reaching the end is
        // the pass assertion.
        let path = temp_config_path("mock-smoke");
        std::fs::create_dir_all(path.parent().unwrap()).expect("create tests_result dir");
        std::fs::write(
            &path,
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 2
kinds = ["Ping"]
"#,
        )
        .expect("write temp config");

        crate::config::test_bridge::run_profile_for_test(
            path.to_str().unwrap(),
            || MockHarness,
            mock_setup,
            "smoke",
            None,
            None,
        )
        .await;

        std::fs::remove_file(&path).ok();
    }
}
