//! End-to-end proof of the dyn-op registry (`opset`): the saturating u8 counter from
//! `pure_function.rs`, rebuilt as standalone op structs registered into an `OpSetHarness`
//! instead of a hand-written enum harness.

use harness_core::{
    CheckOutcome, DynInvariant, DynOp, HarnessError, OpDef, OpFuture, OpSetHarness, Prng, Verdict,
};

/// The system under test: a u8 counter with saturating add and a subtract that
/// rejects underflow.
#[derive(Default)]
struct SatCounter {
    value: u8,
}

impl SatCounter {
    fn add(&mut self, n: u8) -> Result<(), String> {
        self.value = self.value.saturating_add(n);
        Ok(())
    }
    // Exercised from Task 4 onward (the `Sub` op); allow keeps the per-task clippy gate clean.
    #[allow(dead_code)]
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

/// World = SUT + shadow model. Ctx = (): there is no external live system.
struct World {
    sut: SatCounter,
    model: i32,
    first_op: Option<&'static str>,
}

fn fresh_world() -> World {
    World {
        sut: SatCounter::default(),
        model: 0,
        first_op: None,
    }
}

/// Saturating add of `n`: one standalone operation carrying its own `apply`.
#[derive(Debug, Clone)]
struct Add {
    n: u8,
}

impl DynOp<(), World> for Add {
    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut World,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            if world.first_op.is_none() {
                world.first_op = Some("Add");
            }
            world.sut.add(self.n).map_err(HarnessError::infra)?;
            world.model = (world.model + self.n as i32).min(u8::MAX as i32);
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), World>> {
        Box::new(self.clone())
    }
}

#[tokio::test]
async fn op_runs_standalone_without_runner() {
    let mut world = fresh_world();
    let verdict = Add { n: 5 }
        .apply(&mut (), &mut world)
        .await
        .expect("apply");
    assert!(matches!(verdict, Verdict::Accepted));
    assert_eq!(world.model, 5);
    assert_eq!(world.sut.value, 5);
    assert_eq!(world.first_op, Some("Add"));
}

#[tokio::test]
async fn boxed_op_clones_and_debugs() {
    let op: Box<dyn DynOp<(), World>> = Box::new(Add { n: 7 });
    // Stats and failure dumps bucket by the leading Debug token, so it must be the struct name.
    assert!(format!("{op:?}").starts_with("Add"), "{op:?}");
    let cloned = op.clone();
    assert!(format!("{cloned:?}").starts_with("Add"), "{cloned:?}");
    let mut world = fresh_world();
    cloned.apply(&mut (), &mut world).await.expect("apply");
    assert_eq!(world.model, 7);
}

/// Every chain state matches the shadow model: the one invariant of this harness.
#[derive(Debug, Clone)]
struct MatchesModel;

impl DynInvariant<(), World> for MatchesModel {
    fn check<'a>(&'a self, _ctx: &'a mut (), world: &'a World) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            if world.sut.value as i32 == world.model {
                CheckOutcome::Held
            } else {
                CheckOutcome::violated(format!("sut {} != model {}", world.sut.value, world.model))
            }
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<(), World>> {
        Box::new(self.clone())
    }
}

#[tokio::test]
async fn invariant_checks_standalone_without_runner() {
    let mut world = fresh_world();
    let outcome = MatchesModel.check(&mut (), &world).await;
    assert!(matches!(outcome, CheckOutcome::Held));

    world.model = 9; // desync the model on purpose
    let outcome = MatchesModel.check(&mut (), &world).await;
    assert!(!matches!(outcome, CheckOutcome::Held));

    let boxed: Box<dyn DynInvariant<(), World>> = Box::new(MatchesModel);
    assert!(format!("{boxed:?}").starts_with("MatchesModel"));
    let _clone = boxed.clone();
}

/// Generator for the `"add"` kind: any `n` in `0..300` cast to u8 (wraps past 255 on
/// purpose, exercising saturation). A named fn coerces cleanly to `GenerateFn`.
fn gen_add(rng: &mut Prng, _world: &World) -> Box<dyn DynOp<(), World>> {
    Box::new(Add {
        n: rng.below(300) as u8,
    })
}

fn zero_weight(_ctx: &(), _world: &World) -> u32 {
    0
}

#[test]
fn opdef_default_weight_is_one_and_overridable() {
    let def = OpDef::new("add", gen_add);
    assert_eq!(def.name(), "add");

    let mut rng = Prng::seed_from_u64(1);
    let world = fresh_world();
    let op = def.generate(&mut rng, &world);
    assert!(format!("{op:?}").starts_with("Add"), "{op:?}");
    assert_eq!(def.weight(&(), &world), 1);

    let gated = OpDef::new("add", gen_add).with_weight(zero_weight);
    assert_eq!(gated.weight(&(), &world), 0);
}

/// Subtract `n`, expecting rejection on underflow: the op that produces both verdicts.
#[derive(Debug, Clone)]
struct Sub {
    n: u8,
}

impl DynOp<(), World> for Sub {
    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut World,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            if world.first_op.is_none() {
                world.first_op = Some("Sub");
            }
            let expected_ok = world.model >= self.n as i32;
            match (world.sut.sub(self.n), expected_ok) {
                (Ok(()), true) => {
                    world.model -= self.n as i32;
                    Ok(Verdict::Accepted)
                }
                (Ok(()), false) => Err(HarnessError::bug("underflow was accepted")),
                (Err(reason), false) => Ok(Verdict::Rejected { reason }),
                (Err(e), true) => Err(HarnessError::bug(format!("valid sub rejected: {e}"))),
            }
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), World>> {
        Box::new(self.clone())
    }
}

fn gen_sub(rng: &mut Prng, _world: &World) -> Box<dyn DynOp<(), World>> {
    Box::new(Sub {
        n: rng.below(300) as u8,
    })
}

/// Sub weighs 0 while the model is empty: an underflow-only op is meaningless on a zero
/// counter, so it is excluded until the first Add lands.
fn sub_weight(_ctx: &(), world: &World) -> u32 {
    if world.model == 0 {
        0
    } else {
        1
    }
}

/// The full registry harness. `"sub"` is registered first on purpose: the BTreeMap must
/// still yield kinds in sorted name order for seed determinism.
fn build_harness() -> OpSetHarness<(), World> {
    OpSetHarness::new()
        .register(OpDef::new("sub", gen_sub).with_weight(sub_weight))
        .register(OpDef::new("add", gen_add))
        .invariant(Box::new(MatchesModel))
}

#[test]
#[should_panic(expected = "duplicate op kind")]
fn duplicate_op_name_panics() {
    let _ = OpSetHarness::<(), World>::new()
        .register(OpDef::new("add", gen_add))
        .register(OpDef::new("add", gen_add));
}

#[test]
fn empty_registry_constructs_via_default() {
    let _h: OpSetHarness<(), World> = OpSetHarness::default();
    let _h = build_harness();
}
