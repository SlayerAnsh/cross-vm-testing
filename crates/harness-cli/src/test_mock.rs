//! Shared test mock: the OpSet counterpart of the old enum `MockHarness`. `Ctx = u32`,
//! `World = u32`; every op increments the world, `boom` then fails with a fixed `Bug`.

use harness_core::{
    decode_json_op, CheckOutcome, DynInvariant, DynOp, HarnessError, OpDef, OpFuture, OpSetHarness,
    Prng, Verdict,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Ping {}

impl DynOp<u32, u32> for Ping {
    fn kind(&self) -> &'static str {
        "ping"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut u32,
        world: &'a mut u32,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            *world += 1;
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<u32, u32>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Boom {}

impl DynOp<u32, u32> for Boom {
    fn kind(&self) -> &'static str {
        "boom"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut u32,
        world: &'a mut u32,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            *world += 1;
            Err(HarnessError::Bug("boom".to_string()))
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<u32, u32>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AlwaysHolds;

impl DynInvariant<u32, u32> for AlwaysHolds {
    fn check<'a>(&'a self, _ctx: &'a mut u32, _world: &'a u32) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move { CheckOutcome::Held })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<u32, u32>> {
        Box::new(self.clone())
    }
}

fn gen_ping(_rng: &mut Prng, _world: &u32) -> Box<dyn DynOp<u32, u32>> {
    Box::new(Ping {})
}

fn gen_boom(_rng: &mut Prng, _world: &u32) -> Box<dyn DynOp<u32, u32>> {
    Box::new(Boom {})
}

/// A fresh mock harness with kinds `boom`, `ping` (sorted registry order) and one invariant.
pub(crate) fn mock_harness() -> OpSetHarness<u32, u32> {
    OpSetHarness::new()
        .register(OpDef::new("ping", gen_ping, decode_json_op::<Ping, _, _>))
        .register(OpDef::new("boom", gen_boom, decode_json_op::<Boom, _, _>))
        .invariant(Box::new(AlwaysHolds))
}
