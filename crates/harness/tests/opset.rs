//! End-to-end proof of the dyn-op registry (`opset`): the saturating u8 counter from
//! `pure_function.rs`, rebuilt as standalone op structs registered into an `OpSetHarness`
//! instead of a hand-written enum harness.

use harness_core::{
    decode_json_op, CheckOutcome, DynInvariant, DynOp, DynOperation, Harness, HarnessError, OpDef,
    OpFuture, OpSetHarness, Prng, Runner, Verdict,
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Add {
    n: u8,
}

impl DynOp<(), World> for Add {
    fn kind(&self) -> &'static str {
        "add"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut World,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            if world.first_op.is_none() {
                world.first_op = Some("add");
            }
            world.sut.add(self.n).map_err(HarnessError::infra)?;
            world.model = (world.model + self.n as i32).min(u8::MAX as i32);
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), World>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
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
    assert_eq!(world.first_op, Some("add"));
}

#[tokio::test]
async fn boxed_op_clones_and_debugs() {
    // What the runner shows: an op flows inside `DynOperation`, whose Debug leads with the
    // registered kind name. Stats and failure dumps bucket by that leading token.
    let op = DynOperation(Box::new(Add { n: 7 }));
    assert!(format!("{op:?}").starts_with("add"), "{op:?}");
    let cloned = op.clone();
    assert!(format!("{cloned:?}").starts_with("add"), "{cloned:?}");
    let mut world = fresh_world();
    cloned.0.apply(&mut (), &mut world).await.expect("apply");
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
    let def = OpDef::new("add", gen_add, decode_json_op::<Add, _, _>);
    assert_eq!(def.name(), "add");

    let mut rng = Prng::seed_from_u64(1);
    let world = fresh_world();
    let op = def.generate(&mut rng, &world);
    assert!(format!("{op:?}").starts_with("Add"), "{op:?}");
    assert_eq!(def.weight(&(), &world), 1);

    let gated = OpDef::new("add", gen_add, decode_json_op::<Add, _, _>).with_weight(zero_weight);
    assert_eq!(gated.weight(&(), &world), 0);
}

/// Subtract `n`, expecting rejection on underflow: the op that produces both verdicts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Sub {
    n: u8,
}

impl DynOp<(), World> for Sub {
    fn kind(&self) -> &'static str {
        "sub"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut World,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            if world.first_op.is_none() {
                world.first_op = Some("sub");
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

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
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
        .register(OpDef::new("sub", gen_sub, decode_json_op::<Sub, _, _>).with_weight(sub_weight))
        .register(OpDef::new("add", gen_add, decode_json_op::<Add, _, _>))
        .invariant(Box::new(MatchesModel))
}

#[test]
#[should_panic(expected = "duplicate op kind")]
fn duplicate_op_name_panics() {
    let _ = OpSetHarness::<(), World>::new()
        .register(OpDef::new("add", gen_add, decode_json_op::<Add, _, _>))
        .register(OpDef::new("add", gen_add, decode_json_op::<Add, _, _>));
}

#[test]
fn empty_registry_constructs_via_default() {
    let _h: OpSetHarness<(), World> = OpSetHarness::default();
    let _h = build_harness();
}

#[test]
#[should_panic(expected = "no op kinds registered")]
fn empty_registry_panics_on_use() {
    // Construction is fine (the builder starts empty), but using a registry with no ops is a
    // construction bug: the runner calls op_kinds at the start of every run, so it throws there.
    let empty: OpSetHarness<(), World> = OpSetHarness::new();
    let _ = empty.op_kinds();
}

#[test]
fn op_kinds_are_sorted_by_name() {
    // build_harness registers "sub" before "add"; the BTreeMap must still sort.
    assert_eq!(build_harness().op_kinds(), vec!["add", "sub"]);
}

#[tokio::test]
async fn fuzz_opset_counter() {
    let mut r = Runner::fuzz(build_harness(), 42);
    r.setup((), fresh_world());
    let report = r.run(200, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(report.steps, 200);
}

#[tokio::test]
async fn invariant_mode_opset_counter() {
    let mut r = Runner::invariant(build_harness(), 7);
    r.setup((), fresh_world());
    let report = r.run(30, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(report.steps, 30);
}

#[tokio::test]
async fn scenario_run_with_boxed_ops() {
    let mut r = Runner::scenario(build_harness(), 0);
    r.setup((), fresh_world());
    // Sub(200) on a model of 3 is a legitimate rejection, not a failure.
    let ops: Vec<DynOperation<(), World>> = vec![
        DynOperation(Box::new(Add { n: 1 })),
        DynOperation(Box::new(Add { n: 2 })),
        DynOperation(Box::new(Sub { n: 200 })),
        DynOperation(Box::new(Add { n: 3 })),
    ];
    let report = r.run_scenario(ops).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(r.world().model, 6);
}

#[tokio::test]
async fn zero_weight_gates_sub_until_first_add() {
    let mut r = Runner::fuzz(build_harness(), 3);
    r.setup((), fresh_world());
    let report = r.run(50, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    // The model starts at 0, so "sub" weighs 0 at the first draw: op 1 must be an add.
    assert_eq!(r.world().first_op, Some("add"));
}

#[tokio::test]
async fn restricted_run_draws_only_named_kind() {
    let mut r = Runner::fuzz(build_harness(), 11);
    r.setup((), fresh_world());
    let report = r.run(20, Some(&["add"]), 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(r.world().first_op, Some("add"));
}

fn bump<'a>(ctx: &'a mut u64, blocks: u64) -> OpFuture<'a, Result<(), HarnessError>> {
    Box::pin(async move {
        *ctx += blocks;
        Ok(())
    })
}

#[tokio::test]
async fn advance_hook_runs_when_set_and_defaults_to_noop() {
    let with_hook: OpSetHarness<u64, World> = OpSetHarness::new().with_advance(bump);
    let mut ctx = 0u64;
    with_hook.advance(&mut ctx, 3).await.expect("advance");
    assert_eq!(ctx, 3);

    let without: OpSetHarness<u64, World> = OpSetHarness::new();
    let mut ctx = 0u64;
    without.advance(&mut ctx, 3).await.expect("advance");
    assert_eq!(ctx, 0);
}

#[test]
fn config_ops_roundtrip_encode_decode() {
    use harness_core::ConfigOps;
    let h = build_harness();
    // decode a single-key table
    let op = h
        .decode_op(&serde_json::json!({ "add": { "n": 5 } }))
        .expect("decodes");
    assert!(format!("{op:?}").starts_with("add"), "{op:?}");
    // encode round-trips to the same externally tagged shape
    assert_eq!(h.encode_op(&op), serde_json::json!({ "add": { "n": 5 } }));
}

#[test]
fn config_ops_decodes_bare_string_as_empty_data() {
    use harness_core::ConfigOps;
    let h = build_harness();
    // `sub` has a field, so an empty-data decode must fail with serde's message
    let err = h
        .decode_op(&serde_json::Value::String("sub".to_string()))
        .expect_err("sub requires a field");
    assert!(err.contains("sub"), "{err}");
}

#[test]
fn config_ops_rejects_unknown_kind_listing_available() {
    use harness_core::ConfigOps;
    let h = build_harness();
    let err = h
        .decode_op(&serde_json::json!({ "nope": {} }))
        .expect_err("unknown kind");
    assert!(
        err.contains("nope") && err.contains("add") && err.contains("sub"),
        "{err}"
    );
    let err = h.parse_kind("nope").expect_err("unknown kind");
    assert!(err.contains("add") && err.contains("sub"), "{err}");
    assert_eq!(h.parse_kind("add").expect("known"), "add");
}
