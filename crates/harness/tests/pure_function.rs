//! End-to-end proof that harness-core drives a plain function with `Ctx = ()`:
//! a saturating u8 counter (the SUT) checked against an i32 shadow model.

use harness_core::{CheckOutcome, EnduranceConfig, Harness, HarnessError, Prng, Runner, Verdict};
use std::time::Duration;

/// The system under test: a u8 counter with saturating add and a (deliberately correct)
/// subtract that rejects underflow.
#[derive(Default)]
struct SatCounter {
    value: u8,
}

impl SatCounter {
    fn add(&mut self, n: u8) -> Result<(), String> {
        self.value = self.value.saturating_add(n);
        Ok(())
    }
    fn sub(&mut self, n: u8) -> Result<(), String> {
        match self.value.checked_sub(n) {
            Some(v) => {
                self.value = v;
                Ok(())
            }
            None => Err("underflow".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
enum Op {
    Add(u8),
    Sub(u8),
}

#[derive(Debug, Clone, Copy)]
enum OpKind {
    Add,
    Sub,
}

#[derive(Debug, Clone)]
enum Inv {
    MatchesModel,
    NeverExceedsMax,
}

/// World = SUT + shadow model. Ctx = (): there is no external live system.
struct World {
    sut: SatCounter,
    model: i32,
    first_op: Option<&'static str>,
}

struct CounterHarness;

impl Harness for CounterHarness {
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
        if world.first_op.is_none() {
            world.first_op = Some(match op {
                Op::Add(_) => "Add",
                Op::Sub(_) => "Sub",
            });
        }
        match op {
            Op::Add(n) => {
                world.sut.add(*n).map_err(HarnessError::infra)?;
                world.model = (world.model + *n as i32).min(u8::MAX as i32);
                Ok(Verdict::Accepted)
            }
            Op::Sub(n) => {
                let expected_ok = world.model >= *n as i32;
                match (world.sut.sub(*n), expected_ok) {
                    (Ok(()), true) => {
                        world.model -= *n as i32;
                        Ok(Verdict::Accepted)
                    }
                    (Ok(()), false) => Err(HarnessError::bug("underflow was accepted")),
                    (Err(reason), false) => Ok(Verdict::Rejected { reason }),
                    (Err(e), true) => Err(HarnessError::bug(format!("valid sub rejected: {e}"))),
                }
            }
        }
    }

    fn op_kinds(&self) -> Vec<Self::OpKind> {
        vec![OpKind::Add, OpKind::Sub]
    }

    // Sub weighs 0 while the model is empty: an underflow-only op is meaningless on a
    // zero counter, so it is excluded until the first Add lands.
    fn weight(&self, _ctx: &(), world: &Self::World, kind: Self::OpKind) -> u32 {
        match kind {
            OpKind::Add => 1,
            OpKind::Sub => {
                if world.model == 0 {
                    0
                } else {
                    1
                }
            }
        }
    }

    fn generate_op(&self, rng: &mut Prng, _world: &Self::World, kind: Self::OpKind) -> Op {
        let n = rng.below(300) as u8; // wraps past 255 on purpose: exercises saturation
        match kind {
            OpKind::Add => Op::Add(n),
            OpKind::Sub => Op::Sub(n),
        }
    }

    fn invariants(&self) -> Vec<Self::Invariant> {
        vec![Inv::MatchesModel, Inv::NeverExceedsMax]
    }

    async fn check(
        &self,
        _ctx: &mut Self::Ctx,
        world: &Self::World,
        inv: &Self::Invariant,
    ) -> CheckOutcome {
        match inv {
            Inv::MatchesModel if world.sut.value as i32 == world.model => CheckOutcome::Held,
            Inv::MatchesModel => {
                CheckOutcome::violated(format!("sut {} != model {}", world.sut.value, world.model))
            }
            Inv::NeverExceedsMax => CheckOutcome::Held, // u8 cannot exceed its own max
        }
    }
}

fn fresh_world() -> World {
    World {
        sut: SatCounter::default(),
        model: 0,
        first_op: None,
    }
}

#[tokio::test]
async fn fuzz_pure_function_with_unit_ctx() {
    let mut r = Runner::fuzz(CounterHarness, 42);
    r.setup((), fresh_world());
    let report = r.run(200, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(report.steps, 200);
}

#[tokio::test]
async fn endurance_pure_function_with_unit_ctx() {
    let mut r = Runner::endurance(CounterHarness, 7);
    r.setup((), fresh_world());
    let cfg = EnduranceConfig::new(Duration::from_millis(200)).max_ops(500);
    let report = r.run(cfg).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert!(report.steps > 0);
}

#[tokio::test]
async fn scenario_verdicts_and_expectations_with_unit_ctx() {
    let mut r = Runner::scenario(CounterHarness, 0);
    r.setup((), fresh_world());

    // A legitimate rejection (underflow) is not a failure under run_scenario's Expectation::Any.
    let ops = vec![Op::Add(1), Op::Add(2), Op::Sub(200), Op::Add(3)];
    let report = r.run_scenario(ops).await;
    assert!(
        report.passed(),
        "Sub(200) is a legitimate rejection, not a failure"
    );

    // The same op under Expectation::Accepted fails as a Bug (expectation mismatch).
    use harness_core::{Expectation, ScenarioStep};
    let steps = vec![ScenarioStep {
        expect: Expectation::Accepted,
        ..ScenarioStep::new(Op::Sub(200))
    }];
    let report = r.run_steps(steps, 1).await;
    assert!(
        !report.passed(),
        "expected the expectation mismatch to fail"
    );
}

#[tokio::test]
async fn zero_weight_gates_sub_until_first_add() {
    let mut r = Runner::fuzz(CounterHarness, 3);
    r.setup((), fresh_world());
    let report = r.run(50, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    // The model starts at 0, so Sub's weight is 0 at the first draw: op 1 must be an Add.
    // (Reach the world through the runner after the run.)
    assert_eq!(r.world().first_op, Some("Add"));
}
