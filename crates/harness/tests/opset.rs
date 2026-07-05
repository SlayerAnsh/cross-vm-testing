//! End-to-end proof of the dyn-op registry (`opset`): the saturating u8 counter from
//! `pure_function.rs`, rebuilt as standalone op structs registered into an `OpSetHarness`
//! instead of a hand-written enum harness.

use harness_core::{DynOp, HarnessError, OpFuture, Verdict};

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
