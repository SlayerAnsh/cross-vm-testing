//! A tiny, deterministically-failing harness used only by `tests/cli_e2e.rs`'s replay-artifact /
//! shrink / `replay`-subcommand end-to-end coverage.
//!
//! The vault harness (`crate::vault`) has no reachable bug: every fuzz/invariant run over it is
//! expected to pass, so it cannot exercise "the CLI writes a replay artifact on a generative
//! failure, then `cross-vm replay <artifact>` reproduces it" as a real subprocess test without
//! deliberately introducing a fake vault-contract bug. [`BoomHarness`] sidesteps that: `Boom`
//! always fails the same way, and `Noop` always passes, so a fuzz profile mixing the two kinds
//! gives `tests/cli_e2e.rs` a small, real, deterministic failure to write an artifact for and
//! replay, without touching the vault's own (deliberately correct) contract logic.

use cross_vm_framework::config::{SetupFuture, SetupRequest};
use cross_vm_framework::prelude::*;

/// The two [`BoomOp`] kinds, for `kinds`/`weights` restriction in a config profile.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum BoomOpKind {
    /// See [`BoomOp::Noop`].
    Noop,
    /// See [`BoomOp::Boom`].
    Boom,
}

/// One [`BoomHarness`] operation: `Noop` always passes, `Boom` always fails the exact same way.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum BoomOp {
    /// Always accepted; advances `steps` without doing anything else.
    Noop,
    /// Always a [`HarnessError::Bug`] with a fixed, state-independent message.
    Boom,
}

/// [`BoomHarness`]'s only invariant; it always holds (the harness's failure mode is `apply`
/// returning `Bug`, never an invariant violation).
#[derive(Clone, Debug)]
pub enum BoomInvariant {
    /// Trivially always holds.
    AlwaysHolds,
}

/// A harness with exactly one way to fail: `apply`-ing a [`BoomOp::Boom`] always returns the same
/// [`HarnessError::Bug`], regardless of anything that came before it. This makes shrink's
/// "still fails the same way" check trivially satisfiable by any sequence containing a `Boom`,
/// which is exactly what a replay-artifact/shrink end-to-end test needs: a real, reproducible
/// failure with no dependency on live chain state.
pub struct BoomHarness;

impl Harness for BoomHarness {
    type Ctx = Ctx;
    type World = ();
    type Operation = BoomOp;
    type Invariant = BoomInvariant;
    type OpKind = BoomOpKind;

    async fn apply(
        &self,
        _ctx: &mut Ctx,
        _world: &mut (),
        op: &BoomOp,
    ) -> Result<Verdict, HarnessError> {
        match op {
            BoomOp::Noop => Ok(Verdict::Accepted),
            BoomOp::Boom => Err(HarnessError::Bug(
                "boom: deterministic failure for replay-loop e2e coverage".to_string(),
            )),
        }
    }

    fn op_kinds(&self) -> Vec<BoomOpKind> {
        vec![BoomOpKind::Noop, BoomOpKind::Boom]
    }

    fn generate_op(&self, _rng: &mut Prng, _world: &(), kind: BoomOpKind) -> BoomOp {
        match kind {
            BoomOpKind::Noop => BoomOp::Noop,
            BoomOpKind::Boom => BoomOp::Boom,
        }
    }

    fn invariants(&self) -> Vec<BoomInvariant> {
        vec![BoomInvariant::AlwaysHolds]
    }

    async fn advance(&self, ctx: &mut Ctx, blocks: u64) -> Result<(), HarnessError> {
        ctx.advance_all(blocks).await;
        Ok(())
    }

    async fn check(&self, _ctx: &mut Ctx, _world: &(), _inv: &BoomInvariant) -> CheckOutcome {
        CheckOutcome::Held
    }
}

/// The config-driven setup for [`BoomHarness`]: no chains at all (an empty wallet roster, no
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
