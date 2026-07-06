//! Raw generic example: drive `harness-core` + `harness-cli` against a small math library with
//! **no domain layer**.
//!
//! This crate doubles as developer documentation for the batteries-included
//! [`harness_cli::GenericDomain`] path. Nothing here depends on any chain crate. The system under
//! test is a [`Calculator`] (an `i32` accumulator with checked add, sub, mul, and divide), checked
//! against an `i64` shadow model that predicts, for each operation, whether the `i32` calculator
//! should accept it (result in range, and no divide by zero). Every random mode (fuzz, invariant)
//! applies a stream of operations and asserts the calculator never disagrees with the model; a
//! scenario run shows a divide by zero surfacing as a legitimate rejection.
//!
//! The whole thing is wired to config and CLI through [`math_config_setup`] and the `math-cli`
//! binary (`src/bin/math_cli.rs`): `harness run math.harness.toml --profile smoke`.

use harness_core::{
    decode_json_op, CheckOutcome, DynInvariant, DynOp, HarnessError, OpDef, OpFuture, OpSetHarness,
    Prng, Verdict,
};
use serde::{Deserialize, Serialize};

/// The system under test: an `i32` accumulator whose four operations reject on overflow and on
/// divide by zero rather than panicking or wrapping.
#[derive(Debug, Default)]
pub struct Calculator {
    /// The running accumulator value.
    pub value: i32,
}

impl Calculator {
    /// Builds a fresh calculator at zero. Alias for [`Calculator::default`], kept for readability
    /// at the config-setup call site.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `n`, rejecting on overflow instead of wrapping.
    pub fn add(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_add(n).ok_or("overflow")?;
        Ok(())
    }

    /// Subtracts `n`, rejecting on overflow instead of wrapping.
    pub fn sub(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_sub(n).ok_or("overflow")?;
        Ok(())
    }

    /// Multiplies by `n`, rejecting on overflow instead of wrapping.
    pub fn mul(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_mul(n).ok_or("overflow")?;
        Ok(())
    }

    /// Divides by `n`, rejecting divide by zero (and the one overflowing division, `i32::MIN / -1`).
    pub fn div(&mut self, n: i32) -> Result<(), String> {
        if n == 0 {
            return Err("divide by zero".to_string());
        }
        // `checked_div` also rejects `i32::MIN / -1`, the one overflowing division.
        self.value = self.value.checked_div(n).ok_or("overflow")?;
        Ok(())
    }
}

/// World = the calculator plus its shadow model, plus the bug-injection switch.
#[derive(Debug, Default)]
pub struct MathWorld {
    /// The system under test.
    pub sut: Calculator,
    /// The exact running value in `i64` (used to predict overflow).
    pub model: i64,
    /// When set, `sub` actually adds; the shadow model still predicts a real subtraction, so
    /// the `MatchesModel` invariant fires. Lives in the world because dyn ops carry no harness
    /// state.
    pub buggy: bool,
}

/// Classify one applied operation against the model's prediction (`wide` is the exact i64
/// result the op should produce). Mirrors the old enum harness's `apply` tail exactly.
fn classify(
    world: &mut MathWorld,
    result: Result<(), String>,
    wide: i64,
) -> Result<Verdict, HarnessError> {
    let expected_ok = (i32::MIN as i64..=i32::MAX as i64).contains(&wide);
    match (result, expected_ok) {
        (Ok(()), true) => {
            world.model = wide;
            Ok(Verdict::Accepted)
        }
        (Ok(()), false) => Err(HarnessError::bug(format!(
            "out of range result {wide} was accepted"
        ))),
        (Err(reason), false) => Ok(Verdict::Rejected { reason }),
        (Err(e), true) => Err(HarnessError::bug(format!(
            "a valid operation was rejected: {e}"
        ))),
    }
}

/// Add the operand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Add {
    /// The operand.
    pub n: i32,
}

impl DynOp<(), MathWorld> for Add {
    fn kind(&self) -> &'static str {
        "add"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut MathWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            let wide = world.model + self.n as i64;
            let result = world.sut.add(self.n);
            classify(world, result, wide)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), MathWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Subtract the operand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sub {
    /// The operand.
    pub n: i32,
}

impl DynOp<(), MathWorld> for Sub {
    fn kind(&self) -> &'static str {
        "sub"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut MathWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            let wide = world.model - self.n as i64;
            // The injected bug: a "subtract" that actually adds. The model still predicts a real
            // subtraction, so the two diverge and the MatchesModel invariant fires.
            let result = if world.buggy {
                world.sut.add(self.n)
            } else {
                world.sut.sub(self.n)
            };
            classify(world, result, wide)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), MathWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Multiply by the operand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mul {
    /// The operand.
    pub n: i32,
}

impl DynOp<(), MathWorld> for Mul {
    fn kind(&self) -> &'static str {
        "mul"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut MathWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            let wide = world.model * self.n as i64;
            let result = world.sut.mul(self.n);
            classify(world, result, wide)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), MathWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Divide by the operand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Div {
    /// The operand.
    pub n: i32,
}

impl DynOp<(), MathWorld> for Div {
    fn kind(&self) -> &'static str {
        "div"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut MathWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            // Divide by zero is handled first: the model cannot compute it, and the calculator
            // must reject it. A calculator that accepts it is a bug.
            if self.n == 0 {
                return match world.sut.div(0) {
                    Err(reason) => Ok(Verdict::Rejected { reason }),
                    Ok(()) => Err(HarnessError::bug("divide by zero was accepted")),
                };
            }
            let wide = world.model / self.n as i64;
            let result = world.sut.div(self.n);
            classify(world, result, wide)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), MathWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// The calculator's value always equals the shadow model.
#[derive(Debug, Clone)]
pub struct MatchesModel;

impl DynInvariant<(), MathWorld> for MatchesModel {
    fn check<'a>(&'a self, _ctx: &'a mut (), world: &'a MathWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            if world.sut.value as i64 == world.model {
                CheckOutcome::Held
            } else {
                CheckOutcome::violated(format!(
                    "calculator {} does not equal model {}",
                    world.sut.value, world.model
                ))
            }
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<(), MathWorld>> {
        Box::new(self.clone())
    }
}

// Operands span negatives, zero, and positives, so overflow (via repeated mul) and divide by zero
// are both reachable.
fn operand(rng: &mut Prng) -> i32 {
    rng.below(201) as i32 - 100
}

fn gen_add(rng: &mut Prng, _w: &MathWorld) -> Box<dyn DynOp<(), MathWorld>> {
    Box::new(Add { n: operand(rng) })
}

fn gen_sub(rng: &mut Prng, _w: &MathWorld) -> Box<dyn DynOp<(), MathWorld>> {
    Box::new(Sub { n: operand(rng) })
}

fn gen_mul(rng: &mut Prng, _w: &MathWorld) -> Box<dyn DynOp<(), MathWorld>> {
    Box::new(Mul { n: operand(rng) })
}

fn gen_div(rng: &mut Prng, _w: &MathWorld) -> Box<dyn DynOp<(), MathWorld>> {
    Box::new(Div { n: operand(rng) })
}

/// Assemble the math harness.
pub fn math_harness() -> OpSetHarness<(), MathWorld> {
    OpSetHarness::new()
        .register(OpDef::new("add", gen_add, decode_json_op::<Add, _, _>))
        .register(OpDef::new("sub", gen_sub, decode_json_op::<Sub, _, _>))
        .register(OpDef::new("mul", gen_mul, decode_json_op::<Mul, _, _>))
        .register(OpDef::new("div", gen_div, decode_json_op::<Div, _, _>))
        .invariant(Box::new(MatchesModel))
}

/// Config-driven setup: builds a fresh calculator and model world. The [`harness_cli::BasicSetup`]
/// carries the resolved env verbatim; this example reads an optional `buggy` flag from it to
/// demonstrate env-driven setup. The harness `Ctx` is `()`, so the returned pair is `((), MathWorld)`.
pub fn math_config_setup(
    req: harness_cli::BasicSetup,
) -> harness_cli::SetupFuture<'static, (), MathWorld> {
    Box::pin(async move {
        // The env `buggy` flag is surfaced here to show config-driven setup; the correctness
        // profiles keep it off, so they stay green. (A bug-injection demonstration lives in the
        // `#[tokio::test]` below, which builds a `MathWorld { buggy: true, .. }` directly.)
        let _buggy = req.env["buggy"].as_bool().unwrap_or(false);
        Ok(((), MathWorld::default()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the externally tagged, lowercase op shape so the scenario TOML (`op = { add = { n = 5 } }`)
    /// stays in step with the harness. Decodes and re-encodes through the `ConfigOps` codec.
    #[test]
    fn op_add_encodes_externally_tagged_lowercase() {
        use harness_core::ConfigOps;
        let h = math_harness();
        let op = h
            .decode_op(&serde_json::json!({ "add": { "n": 5 } }))
            .expect("decodes");
        assert_eq!(h.encode_op(&op), serde_json::json!({ "add": { "n": 5 } }));
    }

    /// A correct math library holds its `MatchesModel` invariant across a fuzz run.
    #[tokio::test]
    async fn fuzz_correct_math_lib_holds_invariants() {
        let mut r = harness_core::Runner::fuzz(math_harness(), 42);
        r.setup((), MathWorld::default());
        let report = r.run(500, None, 1).await;
        assert!(report.passed(), "{:?}", report.failure);
        assert_eq!(report.steps, 500);
    }

    /// A buggy subtract is caught: proof the correctness assertions above are not vacuous.
    #[tokio::test]
    async fn fuzz_catches_a_buggy_subtract() {
        let mut r = harness_core::Runner::fuzz(math_harness(), 42);
        r.setup(
            (),
            MathWorld {
                buggy: true,
                ..Default::default()
            },
        );
        let report = r.run(500, None, 1).await;
        assert!(!report.passed(), "a buggy subtract must be caught");
        assert!(report.failure.is_some(), "the failure should be recorded");
    }
}
