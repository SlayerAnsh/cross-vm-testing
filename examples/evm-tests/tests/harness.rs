//! Attribute-macro harness style (style a): the scenario test always runs; the fuzz / invariant /
//! endurance runs are opt-in behind their features, mirroring `cross-vm-tests`'s harness tests but
//! against the single-VM EVM counter.

use cross_vm_framework::prelude::*;
#[cfg(feature = "endurance")]
use std::time::Duration;

use evm_tests::counter::{counter_setup, CounterHarness, CounterOp};

// Style (a), always-on: a concrete scenario step through a `ScenarioRunner`.
#[tokio::test]
async fn counter_scenario_increments() {
    let (ctx, world) = counter_setup(0).await.expect("setup");
    let mut r = Runner::scenario(CounterHarness, 0);
    r.setup(ctx, world);
    let report = r.run_case(CounterOp::IncrementTwice).await;
    assert!(report.passed(), "{:?}", report.failure);
}

#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = CounterHarness, seed = 7, cases = 4)]
async fn counter_fuzz(#[runner] mut r: FuzzRunner<CounterHarness>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(25, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}

#[cfg(feature = "invariant")]
#[invariant_runner(harness = CounterHarness, seed = 7)]
async fn counter_invariant_mode(#[runner] mut r: InvariantRunner<CounterHarness>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(30, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(report.steps, 30);
}

#[cfg(feature = "endurance")]
#[endurance_runner(harness = CounterHarness, seed = 1)]
async fn counter_endurance_mode(#[runner] mut r: EnduranceRunner<CounterHarness>) {
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
