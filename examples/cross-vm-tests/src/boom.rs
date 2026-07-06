//! A tiny, deterministically-failing harness used only by `tests/cli_e2e.rs`'s replay-artifact /
//! shrink / `replay`-subcommand end-to-end coverage.
//!
//! The vault harness (`crate::vault`) has no reachable bug: every fuzz/invariant run over it is
//! expected to pass, so it cannot exercise "the CLI writes a replay artifact on a generative
//! failure, then `cross-vm replay <artifact>` reproduces it" as a real subprocess test without
//! deliberately introducing a fake vault-contract bug. The `boom_harness` sidesteps that: `boom`
//! always fails the same way, and `Noop` always passes, so a fuzz profile mixing the two kinds
//! gives `tests/cli_e2e.rs` a small, real, deterministic failure to write an artifact for and
//! replay, without touching the vault's own (deliberately correct) contract logic.

use cross_vm_framework::config::{SetupFuture, SetupRequest};
use cross_vm_framework::prelude::*;

/// Always accepted; advances `steps` without doing anything else.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Noop {}

impl DynOp<Ctx, ()> for Noop {
    fn kind(&self) -> &'static str {
        "noop"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _world: &'a mut (),
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move { Ok(Verdict::Accepted) })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, ()>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Always a [`HarnessError::Bug`] with a fixed, state-independent message.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Boom {}

impl DynOp<Ctx, ()> for Boom {
    fn kind(&self) -> &'static str {
        "boom"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _world: &'a mut (),
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            Err(HarnessError::Bug(
                "boom: deterministic failure for replay-loop e2e coverage".to_string(),
            ))
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, ()>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// The boom harness's only invariant; it always holds (the harness's failure mode is `apply`
/// returning `Bug`, never an invariant violation).
#[derive(Clone, Debug)]
pub struct AlwaysHolds;

impl DynInvariant<Ctx, ()> for AlwaysHolds {
    fn check<'a>(&'a self, _ctx: &'a mut Ctx, _world: &'a ()) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move { CheckOutcome::Held })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, ()>> {
        Box::new(self.clone())
    }
}

fn gen_noop(_rng: &mut Prng, _world: &()) -> Box<dyn DynOp<Ctx, ()>> {
    Box::new(Noop {})
}

fn gen_boom(_rng: &mut Prng, _world: &()) -> Box<dyn DynOp<Ctx, ()>> {
    Box::new(Boom {})
}

fn advance(ctx: &mut Ctx, blocks: u64) -> OpFuture<'_, Result<(), HarnessError>> {
    Box::pin(async move {
        ctx.advance_all(blocks).await;
        Ok(())
    })
}

/// A harness with exactly one way to fail: `apply`-ing a `boom` op always returns the same
/// [`HarnessError::Bug`], regardless of anything that came before it. This makes shrink's
/// "still fails the same way" check trivially satisfiable by any sequence containing a `boom`,
/// which is exactly what a replay-artifact/shrink end-to-end test needs: a real, reproducible
/// failure with no dependency on live chain state.
pub fn boom_harness() -> OpSetHarness<Ctx, ()> {
    OpSetHarness::new()
        .register(OpDef::new("noop", gen_noop, decode_json_op::<Noop, _, _>))
        .register(OpDef::new("boom", gen_boom, decode_json_op::<Boom, _, _>))
        .invariant(Box::new(AlwaysHolds))
        .with_advance(advance)
}

/// The config-driven setup for [`boom_harness`]: no chains at all (an empty wallet roster, no
/// `MultiChainEnv` injections), since `apply` never touches `Ctx`.
pub fn boom_setup(_req: SetupRequest) -> SetupFuture<'static, ()> {
    Box::pin(async move {
        let wallets = std::rc::Rc::new(
            WalletFactory::from_roster(EmptyWallets::SPECS).map_err(HarnessError::infra)?,
        );
        let env = MultiChainEnv::new("boom-harness", wallets);
        let ctx = Ctx::new(env.start().await?);
        Ok((ctx, ()))
    })
}
