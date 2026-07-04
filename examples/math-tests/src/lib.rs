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

use harness_core::{CheckOutcome, Harness, HarnessError, Prng, Verdict};
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

/// One operation applied to the calculator. Tuple variants carry the operand; the serde shape of
/// `Op::Add(5)` is `{"Add": 5}` (a single-key object), which the scenario config mirrors as
/// `op = { Add = 5 }`. Derives `Serialize`/`Deserialize` because the CLI registry bounds require
/// operations to round-trip through config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    /// Add the operand.
    Add(i32),
    /// Subtract the operand.
    Sub(i32),
    /// Multiply by the operand.
    Mul(i32),
    /// Divide by the operand.
    Div(i32),
}

/// The classes of operation the fuzzer/invariant modes draw from. `Copy` + serde are demanded by
/// the CLI registry bounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum OpKind {
    /// Generate an [`Op::Add`].
    Add,
    /// Generate an [`Op::Sub`].
    Sub,
    /// Generate an [`Op::Mul`].
    Mul,
    /// Generate an [`Op::Div`].
    Div,
}

/// The invariants the harness checks after each step.
#[derive(Debug, Clone)]
pub enum Inv {
    /// The calculator's value always equals the shadow model.
    MatchesModel,
}

/// World = the calculator plus its shadow model. The harness `Ctx` is `()`: there is no external
/// live system to hold.
#[derive(Debug, Default)]
pub struct MathWorld {
    /// The system under test.
    pub sut: Calculator,
    /// The exact running value in `i64`. It always fits `i32` (every accepted op keeps it in
    /// range), so it stays equal to `sut.value`; the wider type is only used to predict overflow.
    pub model: i64,
}

/// The math harness. `buggy` injects a wrong subtract (subtract that actually adds) so a
/// bug-injection test can prove the harness catches a real discrepancy instead of rubber stamping.
/// `Default` gives `buggy: false`, the shape the CLI binary and config tests use.
#[derive(Debug, Default)]
pub struct MathHarness {
    /// When set, subtract actually adds; the shadow model still predicts a real subtraction, so
    /// the two diverge and the `MatchesModel` invariant fires.
    pub buggy: bool,
}

impl Harness for MathHarness {
    type Ctx = ();
    type World = MathWorld;
    type Operation = Op;
    type Invariant = Inv;
    type OpKind = OpKind;

    async fn apply(
        &self,
        _ctx: &mut Self::Ctx,
        world: &mut Self::World,
        op: &Self::Operation,
    ) -> Result<Verdict, HarnessError> {
        let current = world.model;

        // Divide by zero is handled first: the model cannot compute it, and the calculator must
        // reject it. A calculator that accepts it is a bug.
        if let Op::Div(0) = op {
            return match world.sut.div(0) {
                Err(reason) => Ok(Verdict::Rejected { reason }),
                Ok(()) => Err(HarnessError::bug("divide by zero was accepted")),
            };
        }

        // (calculator result, exact i64 result the operation should produce)
        let (result, wide) = match op {
            Op::Add(n) => (world.sut.add(*n), current + *n as i64),
            Op::Sub(n) => {
                // The injected bug: a "subtract" that actually adds. The model still predicts a
                // real subtraction, so the two diverge and the MatchesModel invariant fires.
                let res = if self.buggy {
                    world.sut.add(*n)
                } else {
                    world.sut.sub(*n)
                };
                (res, current - *n as i64)
            }
            Op::Mul(n) => (world.sut.mul(*n), current * *n as i64),
            Op::Div(n) => (world.sut.div(*n), current / *n as i64),
        };

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

    fn op_kinds(&self) -> Vec<Self::OpKind> {
        vec![OpKind::Add, OpKind::Sub, OpKind::Mul, OpKind::Div]
    }

    fn generate_op(&self, rng: &mut Prng, _world: &Self::World, kind: Self::OpKind) -> Op {
        // Operands span negatives, zero, and positives, so overflow (via repeated mul) and divide
        // by zero are both reachable.
        let n = rng.below(201) as i32 - 100;
        match kind {
            OpKind::Add => Op::Add(n),
            OpKind::Sub => Op::Sub(n),
            OpKind::Mul => Op::Mul(n),
            OpKind::Div => Op::Div(n),
        }
    }

    fn invariants(&self) -> Vec<Self::Invariant> {
        vec![Inv::MatchesModel]
    }

    async fn check(
        &self,
        _ctx: &mut Self::Ctx,
        world: &Self::World,
        inv: &Self::Invariant,
    ) -> CheckOutcome {
        match inv {
            Inv::MatchesModel if world.sut.value as i64 == world.model => CheckOutcome::Held,
            Inv::MatchesModel => CheckOutcome::violated(format!(
                "calculator {} does not equal model {}",
                world.sut.value, world.model
            )),
        }
    }
}

/// Config-driven setup: builds a fresh calculator and model world. The [`harness_cli::BasicSetup`]
/// carries the resolved env verbatim; this example reads an optional `buggy` flag from it to
/// demonstrate env-driven setup. The harness `Ctx` is `()`, so the returned pair is `((), MathWorld)`.
pub fn math_config_setup(
    req: harness_cli::BasicSetup,
) -> harness_cli::SetupFuture<'static, (), MathWorld> {
    Box::pin(async move {
        // The env `buggy` flag is surfaced here to show config-driven setup; the correctness
        // harness ignores it, so the wired-up profiles stay green. (A bug-injection demonstration
        // lives in the `#[tokio::test]` below, which constructs `MathHarness { buggy: true }`
        // directly.)
        let _buggy = req.env["buggy"].as_bool().unwrap_or(false);
        Ok(((), MathWorld::default()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the serde shape of `Op::Add(5)` so the scenario TOML (`op = { Add = 5 }`) stays in
    /// step with the enum. If the enum representation ever changes, this fails loudly.
    #[test]
    fn op_add_serializes_as_single_key_object() {
        let value = serde_json::to_value(Op::Add(5)).expect("serializable");
        assert_eq!(value, serde_json::json!({ "Add": 5 }));
    }

    /// A correct math library holds its `MatchesModel` invariant across a fuzz run.
    #[tokio::test]
    async fn fuzz_correct_math_lib_holds_invariants() {
        let mut r = harness_core::Runner::fuzz(MathHarness { buggy: false }, 42);
        r.setup((), MathWorld::default());
        let report = r.run(500, None, 1).await;
        assert!(report.passed(), "{:?}", report.failure);
        assert_eq!(report.steps, 500);
    }

    /// A buggy subtract is caught: proof the correctness assertions above are not vacuous.
    #[tokio::test]
    async fn fuzz_catches_a_buggy_subtract() {
        let mut r = harness_core::Runner::fuzz(MathHarness { buggy: true }, 42);
        r.setup((), MathWorld::default());
        let report = r.run(500, None, 1).await;
        assert!(!report.passed(), "a buggy subtract must be caught");
        assert!(report.failure.is_some(), "the failure should be recorded");
    }
}
