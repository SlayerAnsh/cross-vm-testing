//! Attribute-macro harness style (style a): the scenario test always runs; the fuzz / invariant /
//! endurance runs are opt-in behind their features, mirroring `cross-vm-tests`'s harness tests but
//! against the single-VM Tron counter.

use cross_vm_framework::prelude::*;
#[cfg(feature = "endurance")]
use std::time::Duration;

#[cfg(any(feature = "fuzz", feature = "invariant", feature = "endurance"))]
use tvm_tests::counter::CounterWorld;
use tvm_tests::counter::{counter_harness, counter_setup, IncrementTwice};

// Style (a), always-on: a concrete scenario step through a `ScenarioRunner`.
#[tokio::test]
async fn counter_scenario_increments() {
    let (ctx, world) = counter_setup(0).await.expect("setup");
    let mut r = Runner::scenario(counter_harness(), 0);
    r.setup(ctx, world);
    let report = r.run_case(DynOperation(Box::new(IncrementTwice {}))).await;
    assert!(report.passed(), "{:?}", report.failure);
}

#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = counter_harness(), seed = 7, cases = 4)]
async fn counter_fuzz(#[runner] mut r: FuzzRunner<OpSetHarness<Ctx, CounterWorld>>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(25, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}

#[cfg(feature = "invariant")]
#[invariant_runner(harness = counter_harness(), seed = 7)]
async fn counter_invariant_mode(#[runner] mut r: InvariantRunner<OpSetHarness<Ctx, CounterWorld>>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(30, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(report.steps, 30);
}

#[cfg(feature = "endurance")]
#[endurance_runner(harness = counter_harness(), seed = 1)]
async fn counter_endurance_mode(#[runner] mut r: EnduranceRunner<OpSetHarness<Ctx, CounterWorld>>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r
        .run(
            EnduranceConfig::new(Duration::from_millis(2000))
                .check_every(5)
                .advance_blocks(1, 1),
        )
        .await;
    assert!(report.passed(), "{:?}", report.failure);
    assert!(report.steps > 0, "endurance ran zero steps");
}
