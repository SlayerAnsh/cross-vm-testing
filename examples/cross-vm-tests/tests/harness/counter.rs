//! Property-testing harness example: a multi-chain `Counter` (see `cross_vm_tests::counter`)
//! moved to the library crate (`src/counter.rs`) so the `cross-vm` bin can also register and
//! drive it. This file keeps only the test fns.

use std::collections::HashMap;
#[cfg(feature = "endurance")]
use std::time::Duration;

use cross_vm_framework::prelude::*;

#[cfg(any(feature = "fuzz", feature = "invariant", feature = "endurance"))]
use cross_vm_tests::counter::CounterWorld;
use cross_vm_tests::counter::{
    advance, counter_harness, counter_setup, IncrementOnTwoChains, LABELS,
};

// The matrix path: rstest generates one test per (chain_1, chain_2) combination -> 3x3 = 9 cases.
#[rstest::rstest]
#[tokio::test]
async fn counter_two_chain_matrix(
    #[values("osmosis", "eth", "solana")] chain_1: &str,
    #[values("osmosis", "eth", "solana")] chain_2: &str,
) {
    let op = DynOperation(Box::new(IncrementOnTwoChains {
        chain_1: chain_1.to_string(),
        chain_2: chain_2.to_string(),
    }));
    let (ctx, world) = counter_setup(0).await.expect("setup");
    let mut r = Runner::scenario(counter_harness(), 0);
    r.setup(ctx, world);
    let report = r.run_case(op).await;
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
            EnduranceConfig::new(Duration::from_millis(5000))
                .check_every(5)
                .advance_blocks(1, 1),
        )
        .await;
    assert!(report.passed(), "{:?}", report.failure);
    assert!(report.steps > 0, "endurance ran zero steps");
}

// Fan the fuzz cases out into one test each: `counter_fuzz_case_0` .. `counter_fuzz_case_7`.
// Each is its own libtest entry (parallel, individually named and filterable, reproducible by
// seed), with its own fresh setup built in the body.
#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = counter_harness(), seed = 7, cases = 8)]
async fn counter_fuzz(#[runner] mut r: FuzzRunner<OpSetHarness<Ctx, CounterWorld>>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(25, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}

// `seed = -1` picks a fresh random base seed per run (shared across the cases) and prints it, so a
// failure is reproducible by copying the printed value back as a fixed `seed`. The counter is
// correct for every seed, so this stays green while exercising the random-seed expansion.
#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = counter_harness(), seed = -1, cases = 2)]
async fn counter_fuzz_random_seed(#[runner] mut r: FuzzRunner<OpSetHarness<Ctx, CounterWorld>>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(10, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}

// `advance` must progress block height on every chain in the multi-chain env (the default
// implementation warps them all).
#[tokio::test]
async fn advance_progresses_every_chain() {
    let (mut ctx, _w) = counter_setup(0).await.expect("build env");
    let before: HashMap<String, u64> = {
        let mut m = HashMap::new();
        for label in LABELS {
            m.insert(
                label.to_string(),
                ctx.chain(label).unwrap().block_height().await,
            );
        }
        m
    };
    advance(&mut ctx, 3).await.expect("advance");
    for label in LABELS {
        let after = ctx.chain(label).unwrap().block_height().await;
        assert!(
            after > before[label],
            "{label}: block height did not advance ({} -> {after})",
            before[label]
        );
    }
}
