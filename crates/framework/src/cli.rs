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
/// # use cross_vm_framework::harness::{
/// #     decode_json_op, Ctx, DynOp, HarnessError, OpDef, OpFuture, OpSetHarness, Prng, Verdict,
/// # };
/// # #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// # struct Noop {}
/// # impl DynOp<Ctx, ()> for Noop {
/// #     fn kind(&self) -> &'static str { "noop" }
/// #     fn apply<'a>(&'a self, _: &'a mut Ctx, _: &'a mut ()) -> OpFuture<'a, Result<Verdict, HarnessError>> {
/// #         Box::pin(async move { Ok(Verdict::Accepted) })
/// #     }
/// #     fn clone_box(&self) -> Box<dyn DynOp<Ctx, ()>> { Box::new(self.clone()) }
/// #     fn to_data(&self) -> serde_json::Value { serde_json::to_value(self).unwrap() }
/// # }
/// # fn gen_noop(_: &mut Prng, _: &()) -> Box<dyn DynOp<Ctx, ()>> { Box::new(Noop {}) }
/// # fn my_harness() -> OpSetHarness<Ctx, ()> {
/// #     OpSetHarness::new().register(OpDef::new("noop", gen_noop, decode_json_op::<Noop, _, _>))
/// # }
/// # fn my_setup(_req: SetupRequest) -> SetupFuture<'static, ()> { unimplemented!() }
/// Cli::new()
///     .env_file(".env")
///     .register("my-harness", my_harness, my_setup)
///     .main()
///     .await
/// # }
/// ```
pub type Cli = harness_cli::Cli<CrossVmDomain>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SetupFuture, SetupRequest, Target, TargetArgs};
    use crate::harness::{
        decode_json_op, CheckOutcome, Ctx, DynInvariant, DynOp, HarnessError, OpDef, OpFuture,
        OpSetHarness, Prng, Verdict,
    };
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

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct Ping {}

    impl DynOp<Ctx, u32> for Ping {
        fn kind(&self) -> &'static str {
            "ping"
        }

        fn apply<'a>(
            &'a self,
            _ctx: &'a mut Ctx,
            world: &'a mut u32,
        ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
            Box::pin(async move {
                *world += 1;
                Ok(Verdict::Accepted)
            })
        }

        fn clone_box(&self) -> Box<dyn DynOp<Ctx, u32>> {
            Box::new(self.clone())
        }

        fn to_data(&self) -> serde_json::Value {
            serde_json::to_value(self).expect("op data serializes")
        }
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct Boom {}

    impl DynOp<Ctx, u32> for Boom {
        fn kind(&self) -> &'static str {
            "boom"
        }

        fn apply<'a>(
            &'a self,
            _ctx: &'a mut Ctx,
            world: &'a mut u32,
        ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
            Box::pin(async move {
                *world += 1;
                Err(HarnessError::Bug("boom".to_string()))
            })
        }

        fn clone_box(&self) -> Box<dyn DynOp<Ctx, u32>> {
            Box::new(self.clone())
        }

        fn to_data(&self) -> serde_json::Value {
            serde_json::to_value(self).expect("op data serializes")
        }
    }

    #[derive(Debug, Clone)]
    struct AlwaysHolds;

    impl DynInvariant<Ctx, u32> for AlwaysHolds {
        fn check<'a>(&'a self, _ctx: &'a mut Ctx, _world: &'a u32) -> OpFuture<'a, CheckOutcome> {
            Box::pin(async move { CheckOutcome::Held })
        }

        fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, u32>> {
            Box::new(self.clone())
        }
    }

    fn gen_ping(_rng: &mut Prng, _world: &u32) -> Box<dyn DynOp<Ctx, u32>> {
        Box::new(Ping {})
    }

    fn gen_boom(_rng: &mut Prng, _world: &u32) -> Box<dyn DynOp<Ctx, u32>> {
        Box::new(Boom {})
    }

    fn mock_harness() -> OpSetHarness<Ctx, u32> {
        OpSetHarness::new()
            .register(OpDef::new("ping", gen_ping, decode_json_op::<Ping, _, _>))
            .register(OpDef::new("boom", gen_boom, decode_json_op::<Boom, _, _>))
            .invariant(Box::new(AlwaysHolds))
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
        let _cli: Cli = Cli::new().register("vault", mock_harness, mock_setup);
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
kinds = ["ping"]
"#,
        )
        .expect("write temp config");

        crate::config::test_bridge::run_profile_for_test(
            path.to_str().unwrap(),
            mock_harness,
            mock_setup,
            "smoke",
            None,
            None,
        )
        .await;

        std::fs::remove_file(&path).ok();
    }
}
