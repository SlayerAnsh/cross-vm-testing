//! Standalone example: drive `harness-core` against a small math library with `Ctx = ()`.
//!
//! Nothing here depends on any chain crate. The system under test is a `Calculator` (an `i32`
//! accumulator with checked add, sub, mul, and divide), checked against an `i64` shadow model
//! that predicts, for each operation, whether the `i32` calculator should accept it (result in
//! range, and no divide by zero). Every random mode (fuzz, invariant) applies a stream of
//! operations and asserts the calculator never disagrees with the model; a scenario run shows a
//! divide by zero surfacing as a legitimate rejection. The final test injects a bug and confirms
//! the harness catches it.

use harness_core::{
    CheckOutcome, Expectation, Harness, HarnessError, Prng, Runner, ScenarioStep, Verdict,
};

/// The system under test: an `i32` accumulator whose four operations reject on overflow and on
/// divide by zero rather than panicking or wrapping.
#[derive(Default)]
struct Calculator {
    value: i32,
}

impl Calculator {
    fn add(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_add(n).ok_or("overflow")?;
        Ok(())
    }

    fn sub(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_sub(n).ok_or("overflow")?;
        Ok(())
    }

    fn mul(&mut self, n: i32) -> Result<(), String> {
        self.value = self.value.checked_mul(n).ok_or("overflow")?;
        Ok(())
    }

    fn div(&mut self, n: i32) -> Result<(), String> {
        if n == 0 {
            return Err("divide by zero".to_string());
        }
        // `checked_div` also rejects `i32::MIN / -1`, the one overflowing division.
        self.value = self.value.checked_div(n).ok_or("overflow")?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum Op {
    Add(i32),
    Sub(i32),
    Mul(i32),
    Div(i32),
}

#[derive(Debug, Clone, Copy)]
enum OpKind {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone)]
enum Inv {
    /// The calculator's value always equals the shadow model.
    MatchesModel,
}

/// World = the calculator plus its shadow model. `Ctx = ()`: there is no external live system.
struct World {
    sut: Calculator,
    /// The exact running value in `i64`. It always fits `i32` (every accepted op keeps it in
    /// range), so it stays equal to `sut.value`; the wider type is only used to predict overflow.
    model: i64,
}

/// The math harness. `buggy` injects a wrong subtract so the last test can prove the harness
/// actually catches a discrepancy instead of rubber stamping.
struct MathHarness {
    buggy: bool,
}

impl Harness for MathHarness {
    type Ctx = ();
    type World = World;
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

fn fresh_world() -> World {
    World {
        sut: Calculator::default(),
        model: 0,
    }
}

#[tokio::test]
async fn fuzz_correct_math_lib_holds_invariants() {
    let mut r = Runner::fuzz(MathHarness { buggy: false }, 42);
    r.setup((), fresh_world());
    // 500 random operations, invariant checked after each.
    let report = r.run(500, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(report.steps, 500);
}

#[tokio::test]
async fn invariant_correct_math_lib_holds() {
    let mut r = Runner::invariant(MathHarness { buggy: false }, 7);
    r.setup((), fresh_world());
    let report = r.run(300, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}

#[tokio::test]
async fn scenario_divide_by_zero_is_a_legitimate_rejection() {
    let mut r = Runner::scenario(MathHarness { buggy: false }, 0);
    r.setup((), fresh_world());

    // Under run_scenario's Expectation::Any a divide by zero is a legitimate rejection, not a
    // failure: 10, then 10 / 2 = 5, then 5 / 0 rejected, then 5 * 3 = 15.
    let ops = vec![Op::Add(10), Op::Div(2), Op::Div(0), Op::Mul(3)];
    let report = r.run_scenario(ops).await;
    assert!(
        report.passed(),
        "divide by zero is a rejection, not a failure"
    );

    // The same divide by zero under Expectation::Accepted fails the step (expectation mismatch).
    let steps = vec![ScenarioStep {
        expect: Expectation::Accepted,
        ..ScenarioStep::new(Op::Div(0))
    }];
    let report = r.run_steps(steps, 1).await;
    assert!(
        !report.passed(),
        "expected the accept-a-divide-by-zero expectation to fail"
    );
}

#[tokio::test]
async fn fuzz_catches_a_buggy_subtract() {
    // Same harness, bug switched on: subtract actually adds. The shadow model still predicts a
    // real subtraction, so within a few operations the MatchesModel invariant is violated and the
    // run fails. This is what proves the correct-lib tests above are not vacuous.
    let mut r = Runner::fuzz(MathHarness { buggy: true }, 42);
    r.setup((), fresh_world());
    let report = r.run(500, None, 1).await;
    assert!(!report.passed(), "a buggy subtract must be caught");
    assert!(report.failure.is_some(), "the failure should be recorded");
}
