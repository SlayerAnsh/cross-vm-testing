//! DeFi harness example: property tests over the [`VaultHarness`] moved to the library crate
//! (`src/vault.rs`, P2 vault migration) so the `cross-vm` bin can also register and drive it.
//!
//! This file keeps only the test fns: the runner-macro tests (which call `vault_setup` directly
//! in their body — the macros only inject the seeded `Runner` shell, see
//! `crates/macros/src/runner_macros.rs`) plus the rstest matrix and the two plain `#[tokio::test]`s.

#[cfg(feature = "endurance")]
use std::time::Duration;

use cross_vm_framework::prelude::*;
#[cfg(feature = "endurance")]
use cross_vm_macros::endurance_runner;
#[cfg(feature = "fuzz")]
use cross_vm_macros::fuzz_runner;
#[cfg(feature = "invariant")]
use cross_vm_macros::invariant_runner;

#[cfg(feature = "fuzz")]
use cross_vm_tests::vault::VaultOpKind;
use cross_vm_tests::vault::{vault_setup, VaultHarness, VaultOp};

#[cfg(feature = "invariant")]
#[invariant_runner(harness = VaultHarness, seed = 42)]
async fn vault_invariant_mode(#[runner] mut r: InvariantRunner<VaultHarness>) {
    let (ctx, world) = vault_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(120, None, 1).await;
    assert!(report.passed(), "{:#?}", report.failure);
    assert_eq!(report.steps, 120);
}

// Combination fuzz over all kinds: one short random sequence per case, fanned out per case.
#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = VaultHarness, seed = 7, cases = 4)]
async fn vault_fuzz_combination(#[runner] mut r: FuzzRunner<VaultHarness>) {
    let (ctx, world) = vault_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(20, None, 1).await;
    assert!(report.passed(), "{:#?}", report.failure);
}

// Per-op fuzz: hammer a single operation kind with many randomized inputs, one fresh world per case.
#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = VaultHarness, seed = 11, cases = 50)]
async fn vault_fuzz_single_deposit(#[runner] mut r: FuzzRunner<VaultHarness>) {
    let (ctx, world) = vault_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(1, Some(&[VaultOpKind::Deposit]), 1).await;
    assert!(report.passed(), "{:#?}", report.failure);
    assert_eq!(report.steps, 1);
}

// Combination fuzz restricted to a subset of kinds: only deposits and withdraws participate.
#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = VaultHarness, seed = 5, cases = 3)]
async fn vault_fuzz_combination_subset(#[runner] mut r: FuzzRunner<VaultHarness>) {
    let (ctx, world) = vault_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r
        .run(15, Some(&[VaultOpKind::Deposit, VaultOpKind::Withdraw]), 1)
        .await;
    assert!(report.passed(), "{:#?}", report.failure);
}

#[cfg(feature = "endurance")]
#[endurance_runner(harness = VaultHarness, seed = 3)]
async fn vault_endurance_mode(#[runner] mut r: EnduranceRunner<VaultHarness>) {
    let (ctx, world) = vault_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r
        .run(
            EnduranceConfig::new(Duration::from_millis(60))
                .check_every(10)
                .advance_blocks(1, 1),
        )
        .await;
    assert!(report.passed(), "{:#?}", report.failure);
    assert!(report.steps > 0);
}

// rstest matrix: a single deposit->withdraw cycle on each chain, fanned out per chain label.
#[rstest::rstest]
#[tokio::test]
async fn vault_per_chain_matrix(#[values("osmosis", "eth", "solana")] chain: &str) {
    let (ctx, world) = vault_setup(0).await.expect("setup");
    let mut r = Runner::scenario(VaultHarness, 0);
    r.setup(ctx, world);
    let report = r
        .run_scenario(vec![
            VaultOp::Deposit {
                chain: chain.into(),
                user: 0,
                amount: 1_000,
            },
            VaultOp::Borrow {
                chain: chain.into(),
                user: 0,
                amount: 400,
            },
            VaultOp::Repay {
                chain: chain.into(),
                user: 0,
                amount: 400,
            },
            VaultOp::Withdraw {
                chain: chain.into(),
                user: 0,
                amount: 1_000,
            },
        ])
        .await;
    assert!(report.passed(), "{:#?}", report.failure);
}

// A legitimate revert (withdraw more than deposited) is Verdict::Rejected, not a failure.
#[tokio::test]
async fn over_withdraw_is_rejected_not_a_failure() {
    let (ctx, world) = vault_setup(0).await.expect("setup");
    let mut r = Runner::scenario(VaultHarness, 0);
    r.setup(ctx, world);
    let report = r
        .run_scenario(vec![
            VaultOp::Deposit {
                chain: "eth".into(),
                user: 0,
                amount: 100,
            },
            VaultOp::Withdraw {
                chain: "eth".into(),
                user: 0,
                amount: 1_000,
            },
        ])
        .await;
    assert!(report.passed(), "{:#?}", report.failure);
}
