//! Runner mechanics, verified end-to-end with an in-memory harness (no chains, no artifacts).
//!
//! A tiny "bank" implements [`Harness`]; a `Behavior` knob makes it behave correctly, drift its
//! shadow model, or report a bug. That covers every runner path: fuzz over a random sequence
//! (including a restricted kind set), the invariant and endurance modes, the scenario entrypoints,
//! replay, the three failure classifications (Invariant / Bug / Infra), and an
//! always-[`CheckOutcome::Skipped`] invariant (so skip handling and the skip count are exercised
//! too). Setup is built per test via `bank_env` and loaded with `r.setup(ctx, world)`.
//!
//! The bank has no chains, so its [`Ctx`] is an empty `MultiChainEnv` it never touches; the
//! `World` is pure in-memory state. This isolates the runner's control flow from any VM.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use cross_vm_framework::prelude::*;

use crate::support::empty_wallets;

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Deposit { user: usize, amount: u128 },
    Withdraw { user: usize, amount: u128 },
}

#[derive(Clone, Debug)]
enum Inv {
    /// Chain balances equal the shadow model.
    ModelMatches,
    /// Trivially-true property, present so invariant iteration covers more than one entry.
    Bounded,
    /// Never applicable: always [`CheckOutcome::Skipped`], so skip handling is exercised.
    Untriggered,
}

/// The data-free kinds of [`Op`], for per-kind fuzzing.
#[derive(Clone, Copy, Debug)]
enum OpKind {
    Deposit,
    Withdraw,
}

#[derive(Clone)]
enum Behavior {
    /// Correct implementation.
    Good,
    /// A successful withdraw decrements the chain but forgets the model (drift -> ModelMatches breaks).
    DriftModel,
    /// Every deposit reports a confirmed bug.
    BugOnDeposit,
    /// Two distinct bugs: every deposit bugs with one detail, a withdraw of exactly 13 with
    /// another. Exercises shrink's "same bug detail" preservation.
    BugOnBoth,
    /// A deposit that lands the balance exactly on the given value reports a bug. Only the full
    /// cumulative sequence triggers it, making the failing sequence irreducible (ddmin worst case).
    BugAtBalance(u128),
    /// Cycles through `pattern` (repeating past its end): `true` fails the op as
    /// [`HarnessError::Infra`], `false` applies it exactly like [`Behavior::Good`]. Indexed by
    /// the bank's own applied-op counter, so it gives full deterministic control over which ops
    /// in an endurance run come back `Infra`, independent of the (randomly generated) op content.
    /// Used to test the endurance runner's tolerated-Infra streak (continue / reset / fail).
    InfraPattern(Vec<bool>),
    /// Applies exactly like [`Behavior::Good`], but on the Nth applied op (0-indexed, `N` = the
    /// wrapped value) additionally perturbs every user's model by `+1` relative to chain,
    /// deterministically breaking `ModelMatches` from that point on regardless of which
    /// (randomly generated) op landed on that index or whether it was accepted/rejected. Used to
    /// test that a final invariant sweep still catches drift a `check_every = 0` mid-run cadence
    /// missed.
    DriftAfter(usize),
}

struct World {
    chain: Vec<u128>,
    model: Vec<u128>,
}

struct Bank {
    users: usize,
    behavior: Behavior,
    /// Records every applied op so a test can assert what ran (e.g. per-op fuzz runs one kind).
    log: Rc<RefCell<Vec<Op>>>,
    /// Applied-op counter: indexes into `Behavior::InfraPattern`'s pattern, and marks the op
    /// `Behavior::DriftAfter` perturbs the model on. Interior mutability: `apply` takes `&self`.
    op_calls: Cell<usize>,
}

impl Bank {
    fn new(users: usize, behavior: Behavior) -> (Self, Rc<RefCell<Vec<Op>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                users,
                behavior,
                log: log.clone(),
                op_calls: Cell::new(0),
            },
            log,
        )
    }
}

impl Harness for Bank {
    type Ctx = Ctx;
    type World = World;
    type Operation = Op;
    type Invariant = Inv;
    type OpKind = OpKind;

    async fn apply(&self, _ctx: &mut Ctx, w: &mut World, op: &Op) -> Result<Verdict, HarnessError> {
        self.log.borrow_mut().push(op.clone());
        let call_idx = self.op_calls.get();
        self.op_calls.set(call_idx + 1);

        if let Behavior::InfraPattern(pattern) = &self.behavior {
            if pattern[call_idx % pattern.len()] {
                return Err(HarnessError::infra("infra pattern hit"));
            }
            // `false`: falls through to the same application the `Good` behavior takes below,
            // since `InfraPattern` matches none of the `matches!` guards in that match.
        }
        if let Behavior::DriftAfter(n) = &self.behavior {
            if call_idx == *n {
                // Unconditional, before the op is even matched: independent of whether this op
                // is a deposit/withdraw or ends up accepted/rejected.
                for m in w.model.iter_mut() {
                    *m = m.wrapping_add(1);
                }
            }
        }
        match *op {
            Op::Deposit { user, amount } => {
                if matches!(self.behavior, Behavior::BugOnDeposit | Behavior::BugOnBoth) {
                    return Err(HarnessError::Bug("deposit exploded".into()));
                }
                w.chain[user] += amount;
                w.model[user] += amount;
                if let Behavior::BugAtBalance(bomb) = &self.behavior {
                    if w.chain[user] == *bomb {
                        return Err(HarnessError::Bug("balance bomb".into()));
                    }
                }
                Ok(Verdict::Accepted)
            }
            Op::Withdraw { user, amount } => {
                if matches!(self.behavior, Behavior::BugOnBoth) && amount == 13 {
                    return Err(HarnessError::Bug("withdraw exploded".into()));
                }
                if amount > w.chain[user] {
                    // Over-withdraw: a legitimate rejection, not a failure.
                    return Ok(Verdict::Rejected {
                        reason: "insufficient balance".into(),
                    });
                }
                w.chain[user] -= amount;
                if !matches!(self.behavior, Behavior::DriftModel) {
                    w.model[user] -= amount;
                }
                Ok(Verdict::Accepted)
            }
        }
    }

    fn op_kinds(&self) -> Vec<OpKind> {
        vec![OpKind::Deposit, OpKind::Withdraw]
    }

    fn generate_op(&self, rng: &mut Prng, w: &World, kind: OpKind) -> Op {
        let user = rng.index(self.users);
        match kind {
            OpKind::Deposit => Op::Deposit {
                user,
                amount: rng.range(1, 100),
            },
            // Span past the balance so some withdraws are rejected and some succeed.
            OpKind::Withdraw => Op::Withdraw {
                user,
                amount: rng.range(1, w.chain[user] * 2 + 2),
            },
        }
    }

    fn generate(&self, rng: &mut Prng, w: &World) -> Op {
        let kind = if rng.weighted(&[1, 1]) == 0 {
            OpKind::Deposit
        } else {
            OpKind::Withdraw
        };
        self.generate_op(rng, w, kind)
    }

    fn invariants(&self) -> Vec<Inv> {
        vec![Inv::ModelMatches, Inv::Bounded, Inv::Untriggered]
    }

    async fn advance(&self, ctx: &mut Ctx, blocks: u64) -> Result<(), HarnessError> {
        ctx.advance_all(blocks).await;
        Ok(())
    }

    async fn check(&self, _ctx: &mut Ctx, w: &World, inv: &Inv) -> CheckOutcome {
        match inv {
            Inv::ModelMatches if w.chain != w.model => {
                CheckOutcome::violated(format!("chain {:?} != model {:?}", w.chain, w.model))
            }
            Inv::Untriggered => CheckOutcome::skipped("precondition never met"),
            _ => CheckOutcome::Held,
        }
    }
}

/// Build the (chainless) env and primed world the bank runs over. Lifted out of the old
/// `Harness::setup`; each test calls it and loads the result with `r.setup(ctx, world)`.
async fn bank_env(users: usize) -> Result<(Ctx, World), HarnessError> {
    // No chains: an empty environment the bank never reads from.
    let env = MultiChainEnv::new("bank", empty_wallets()).start().await?;
    Ok((
        Ctx::new(env),
        World {
            chain: vec![1_000; users],
            model: vec![1_000; users],
        },
    ))
}

#[tokio::test]
async fn good_bank_passes_invariant_and_fuzz() {
    // Invariant run over a persisted world.
    let (bank, _log) = Bank::new(3, Behavior::Good);
    let (ctx, world) = bank_env(3).await.unwrap();
    let mut r = Runner::invariant(bank, 1);
    r.setup(ctx, world);
    let rep = r.run(300, None, 1).await;
    assert!(rep.passed(), "invariant: {:?}", rep.failure);
    assert_eq!(rep.steps, 300);

    // Fuzz a random sequence (fresh bank + env, since each runner owns its loaded state).
    let (bank, _log) = Bank::new(3, Behavior::Good);
    let (ctx, world) = bank_env(3).await.unwrap();
    let mut r = Runner::fuzz(bank, 0);
    r.setup(ctx, world);
    let rep = r.run(25, None, 1).await;
    assert!(rep.passed(), "fuzz: {:?}", rep.failure);
}

#[tokio::test]
async fn skipped_invariant_does_not_fail_and_is_counted() {
    let (bank, _log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::invariant(bank, 1);
    r.setup(ctx, world);
    let rep = r.run(5, None, 1).await;
    assert!(rep.passed(), "{:?}", rep.failure);
    // `Untriggered` is skipped on every per-op check: one skip per op.
    assert_eq!(rep.skipped, 5, "expected one skip per op");
}

#[tokio::test]
async fn endurance_honors_wall_clock_and_passes() {
    let (bank, _log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 3);
    r.setup(ctx, world);
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_millis(40))
                .base_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(1))
                .check_every(5)
                .advance_blocks(1, 2),
        )
        .await;
    assert!(rep.passed(), "endurance: {:?}", rep.failure);
    assert!(rep.steps > 0, "endurance ran zero steps");
}

#[tokio::test]
async fn endurance_max_ops_stops_independent_of_duration() {
    // A generous wall-clock bound but a tight `max_ops`, no inter-op delay: `max_ops` must be
    // what stops the run, and it must stop at exactly that count.
    let (bank, _log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 101);
    r.setup(ctx, world);
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(5))
                .check_every(0)
                .max_ops(7),
        )
        .await;
    assert!(rep.passed(), "endurance: {:?}", rep.failure);
    assert_eq!(rep.steps, 7);
}

#[tokio::test]
async fn endurance_default_fails_on_the_first_infra() {
    // `max_consecutive_infra` defaults to 0: today's behavior (fail on the first Infra) must be
    // preserved unless a test opts into tolerance.
    let (bank, _log) = Bank::new(2, Behavior::InfraPattern(vec![true]));
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 103);
    r.setup(ctx, world);
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(5))
                .check_every(0)
                .max_ops(5),
        )
        .await;
    assert!(!rep.passed(), "expected the first Infra to fail the run");
    let f = rep.failure.unwrap();
    assert_eq!(f.step, 1);
    assert!(matches!(f.kind, FailureKind::Infra(_)), "{:?}", f.kind);
}

#[tokio::test]
async fn endurance_tolerates_infra_up_to_max_consecutive() {
    // 2 consecutive Infra ops, then success, then 1 more Infra: never more than 2 in a row, and
    // `max_consecutive_infra = 2` tolerates exactly that.
    let (bank, _log) = Bank::new(
        2,
        Behavior::InfraPattern(vec![true, true, false, true, false]),
    );
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 107);
    r.setup(ctx, world);
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(5))
                .check_every(0)
                .max_ops(5)
                .max_consecutive_infra(2),
        )
        .await;
    assert!(rep.passed(), "endurance: {:?}", rep.failure);
    assert_eq!(rep.steps, 5);
}

#[tokio::test]
async fn endurance_infra_streak_resets_on_success() {
    // Alternating Infra/success, never two Infra in a row: `max_consecutive_infra = 1` must never
    // trip even though the pattern repeats for the whole run (the streak resets to 0 every other
    // op, so it is never given the chance to exceed 1).
    let (bank, _log) = Bank::new(2, Behavior::InfraPattern(vec![true, false]));
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 109);
    r.setup(ctx, world);
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(5))
                .check_every(0)
                .max_ops(10)
                .max_consecutive_infra(1),
        )
        .await;
    assert!(rep.passed(), "endurance: {:?}", rep.failure);
    assert_eq!(rep.steps, 10);
}

#[tokio::test]
async fn endurance_fails_once_infra_streak_exceeds_max_consecutive() {
    // Every op is Infra: with `max_consecutive_infra = 2`, the 3rd consecutive Infra must fail
    // the run (the first two are tolerated).
    let (bank, _log) = Bank::new(2, Behavior::InfraPattern(vec![true]));
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 113);
    r.setup(ctx, world);
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(5))
                .check_every(0)
                .max_ops(10)
                .max_consecutive_infra(2),
        )
        .await;
    assert!(
        !rep.passed(),
        "expected the infra streak to exceed the ceiling"
    );
    let f = rep.failure.unwrap();
    assert!(matches!(f.kind, FailureKind::Infra(_)), "{:?}", f.kind);
    assert_eq!(f.step, 3, "should fail on the 3rd consecutive infra op");
}

#[tokio::test]
async fn endurance_stop_flag_ends_the_run_as_a_pass_with_final_sweep() {
    // The flag is already set before the first iteration: the loop must break immediately, but
    // the final sweep must still run (and the report is a PASS, not a failure).
    let (bank, _log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 127);
    r.setup(ctx, world);
    let stop = Arc::new(AtomicBool::new(true));
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(5))
                .check_every(0)
                .stop(stop),
        )
        .await;
    assert!(
        rep.passed(),
        "stop should end the run as a pass: {:?}",
        rep.failure
    );
    assert_eq!(rep.steps, 0, "the flag was already set before the first op");
    assert!(
        rep.coverage.iter().all(|(_, c)| c.total() > 0),
        "the final sweep must still have run: {:?}",
        rep.coverage
    );
}

#[tokio::test(start_paused = true)]
async fn endurance_delay_advances_the_paused_clock() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::endurance(bank, 131);
    r.setup(ctx, world);
    let start = tokio::time::Instant::now();
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(30))
                .base_delay(Duration::from_millis(500))
                .check_every(0)
                .max_ops(3),
        )
        .await;
    assert!(rep.passed(), "{:?}", rep.failure);
    assert_eq!(rep.steps, 3);
    assert!(
        start.elapsed() >= Duration::from_millis(1500),
        "virtual clock should have advanced by base_delay * steps, elapsed = {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn endurance_check_every_zero_disables_mid_run_sweep_but_final_sweep_still_runs() {
    // `DriftAfter(0)` perturbs the model on the very first applied op, deterministically breaking
    // `ModelMatches` from then on regardless of which op the endurance draw landed on. With
    // `check_every = 0` no mid-run sweep can catch it, but the final sweep must.
    let (bank, _log) = Bank::new(2, Behavior::DriftAfter(0));
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::endurance(bank, 137);
    r.setup(ctx, world);
    let rep = r
        .run(
            EnduranceConfig::new(Duration::from_secs(5))
                .check_every(0)
                .max_ops(5),
        )
        .await;
    assert!(
        !rep.passed(),
        "the final sweep must catch the drift the mid-run cadence missed"
    );
    let f = rep.failure.unwrap();
    assert_eq!(f.step, 5, "the sweep only runs after every op has applied");
    assert!(
        matches!(f.kind, FailureKind::Invariant { .. }),
        "{:?}",
        f.kind
    );
}

#[tokio::test]
async fn fuzz_restricted_to_single_kind_runs_only_that_kind() {
    let (bank, log) = Bank::new(2, Behavior::Good);
    // Restrict the sequence to one kind: 30 randomized deposits over the loaded world.
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::fuzz(bank, 4);
    r.setup(ctx, world);
    let rep = r.run(30, Some(&[OpKind::Deposit]), 1).await;
    assert!(rep.passed(), "{:?}", rep.failure);
    assert_eq!(rep.steps, 30);
    let log = log.borrow();
    assert_eq!(log.len(), 30);
    assert!(
        log.iter().all(|op| matches!(op, Op::Deposit { .. })),
        "only deposits should have run, got {log:?}"
    );
}

#[tokio::test]
async fn fuzz_restricts_to_given_kinds() {
    let (bank, log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::fuzz(bank, 2);
    r.setup(ctx, world);
    let rep = r.run(20, Some(&[OpKind::Deposit]), 1).await;
    assert!(rep.passed(), "{:?}", rep.failure);
    assert!(
        log.borrow()
            .iter()
            .all(|op| matches!(op, Op::Deposit { .. })),
        "only deposits should have run"
    );
}

#[tokio::test]
async fn drift_model_caught_as_invariant_failure() {
    let (bank, _log) = Bank::new(1, Behavior::DriftModel);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    // Deposit (model+chain move together), then a successful withdraw (chain drops, model does not).
    let rep = r
        .run_scenario(vec![
            Op::Deposit {
                user: 0,
                amount: 50,
            },
            Op::Withdraw {
                user: 0,
                amount: 20,
            },
        ])
        .await;
    assert!(!rep.passed());
    let f = rep.failure.unwrap();
    assert_eq!(f.step, 2, "should fail after the withdraw");
    assert!(
        matches!(f.kind, FailureKind::Invariant { .. }),
        "{:?}",
        f.kind
    );
}

#[tokio::test]
async fn bug_reported_by_apply_is_caught() {
    let (bank, _log) = Bank::new(1, Behavior::BugOnDeposit);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let rep = r
        .run_case(Op::Deposit {
            user: 0,
            amount: 10,
        })
        .await;
    let f = rep.failure.unwrap();
    assert!(matches!(f.kind, FailureKind::Bug(_)), "{:?}", f.kind);
}

#[tokio::test]
async fn setup_failure_surfaces_as_infra_err() {
    // Setup now runs in the test body before the runner drives anything, so a build failure is the
    // setup fn's `Err` (the caller `.expect()`s or `?`s it) rather than a step-zero RunReport.
    async fn failing_env() -> Result<(Ctx, World), HarnessError> {
        Err(HarnessError::infra("setup boom"))
    }
    match failing_env().await {
        Ok(_) => panic!("expected setup to fail"),
        Err(e) => assert!(matches!(e, HarnessError::Infra(_)), "{e:?}"),
    }
}

#[tokio::test]
async fn run_case_on_good_bank_passes() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let rep = r
        .run_case(Op::Deposit {
            user: 0,
            amount: 10,
        })
        .await;
    assert!(rep.passed(), "{:?}", rep.failure);
    assert_eq!(rep.steps, 1);
}

#[tokio::test]
async fn run_steps_expect_rejected_but_accepted_fails_with_exact_message() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let steps = vec![ScenarioStep {
        expect: Expectation::Rejected,
        ..ScenarioStep::new(Op::Deposit {
            user: 0,
            amount: 10,
        })
    }];
    let rep = r.run_steps(steps, 1).await;
    let f = rep.failure.expect("must fail");
    assert_eq!(f.step, 1);
    match f.kind {
        FailureKind::Bug(msg) => assert_eq!(
            msg, "step 1: expected rejection, operation was accepted",
            "exact mismatch message required"
        ),
        other => panic!("expected Bug, got {other:?}"),
    }
}

#[tokio::test]
async fn run_steps_expect_accepted_but_rejected_fails_with_exact_message() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    // Balance starts at 1_000; withdrawing 5_000 is a legitimate rejection.
    let steps = vec![ScenarioStep::new(Op::Withdraw {
        user: 0,
        amount: 5_000,
    })];
    let rep = r.run_steps(steps, 1).await;
    let f = rep.failure.expect("must fail");
    assert_eq!(f.step, 1);
    match f.kind {
        FailureKind::Bug(msg) => assert_eq!(
            msg, "step 1: expected acceptance, operation was rejected",
            "exact mismatch message required"
        ),
        other => panic!("expected Bug, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn run_steps_delay_advances_the_paused_clock() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let start = tokio::time::Instant::now();
    let steps = vec![ScenarioStep {
        delay: Duration::from_millis(500),
        ..ScenarioStep::new(Op::Deposit {
            user: 0,
            amount: 10,
        })
    }];
    let rep = r.run_steps(steps, 1).await;
    assert!(rep.passed(), "{:?}", rep.failure);
    assert!(
        start.elapsed() >= Duration::from_millis(500),
        "virtual clock should have advanced by the step delay, elapsed = {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn run_steps_check_false_skips_the_sweep_after_that_step() {
    // DriftModel: a successful withdraw decrements chain but not the model. A checked sweep after
    // the withdraw would catch the drift; `check = false` on that step must skip it.
    let (bank, _log) = Bank::new(1, Behavior::DriftModel);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let steps = vec![
        ScenarioStep::new(Op::Deposit {
            user: 0,
            amount: 50,
        }),
        ScenarioStep {
            check: false,
            ..ScenarioStep::new(Op::Withdraw {
                user: 0,
                amount: 20,
            })
        },
    ];
    let rep = r.run_steps(steps, 1).await;
    assert!(
        rep.passed(),
        "drift after an unchecked step must not surface: {:?}",
        rep.failure
    );
}

#[tokio::test]
async fn run_steps_check_every_zero_disables_all_sweeps() {
    let (bank, _log) = Bank::new(1, Behavior::DriftModel);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let steps = vec![
        ScenarioStep::new(Op::Deposit {
            user: 0,
            amount: 50,
        }),
        ScenarioStep::new(Op::Withdraw {
            user: 0,
            amount: 20,
        }),
    ];
    let rep = r.run_steps(steps, 0).await;
    assert!(
        rep.passed(),
        "check_every = 0 must disable every sweep, even on check = true steps: {:?}",
        rep.failure
    );
}

#[tokio::test]
async fn coverage_reports_per_invariant_tallies() {
    // A checked run: `Untriggered` is always skipped (held 0), the others hold every check.
    let (bank, _log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::invariant(bank, 1);
    r.setup(ctx, world);
    let rep = r.run(5, None, 1).await;
    assert!(rep.passed(), "{:?}", rep.failure);

    let tally = |name: &str| -> InvCoverage {
        *rep.coverage
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("no coverage for {name}"))
            .1
    };
    // Always-Skipped invariant: never held, one skip per op.
    let untriggered = tally("Untriggered");
    assert_eq!(untriggered.held, 0, "Untriggered never holds");
    assert_eq!(untriggered.skipped, 5, "one skip per op");
    assert_eq!(untriggered.violated, 0);
    // A real invariant held on every op.
    assert_eq!(tally("ModelMatches").held, 5);
    assert!(
        rep.coverage.uncovered().next().is_none(),
        "every invariant ran at least once"
    );
    assert_eq!(
        rep.skipped,
        rep.coverage.total_skipped(),
        "aggregate matches"
    );

    // An unchecked run (`check_every = 0`): no invariant ever runs, so all are uncovered even though
    // they were seeded from `invariants()`.
    let (bank, _log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::fuzz(bank, 1);
    r.setup(ctx, world);
    let rep = r.run(3, None, 0).await;
    assert!(rep.passed(), "{:?}", rep.failure);
    let uncovered: Vec<&str> = rep.coverage.uncovered().collect();
    assert!(
        uncovered.contains(&"ModelMatches"),
        "ModelMatches never ran; uncovered = {uncovered:?}"
    );
    assert_eq!(uncovered.len(), 3, "no invariant ran under check_every = 0");
}

#[tokio::test]
async fn stats_flag_an_op_kind_that_always_reverts() {
    // Ten over-withdraws on a 1_000 balance: every one is a legitimate rejection, so a stats-enabled
    // run shows that kind at 100% rejected — the "generated ops tested almost nothing" signal.
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.with_stats();
    r.setup(ctx, world);
    let ops: Vec<Op> = (0..10)
        .map(|_| Op::Withdraw {
            user: 0,
            amount: 5_000,
        })
        .collect();
    let rep = r.run_scenario(ops).await;
    assert!(rep.passed(), "{:?}", rep.failure);

    let stats = r.stats().expect("stats were enabled");
    let w = stats.get("Withdraw").expect("withdraw stats recorded");
    assert_eq!(w.count, 10);
    assert_eq!(w.rejected, 10, "every over-withdraw is rejected");
    assert_eq!(w.accepted, 0);
    assert!((w.reject_rate() - 1.0).abs() < 1e-9, "100% rejected");
}

#[tokio::test]
async fn stats_are_off_by_default() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let _ = r.run_case(Op::Deposit { user: 0, amount: 1 }).await;
    assert!(r.stats().is_none(), "stats must be opt-in");
}

#[tokio::test]
async fn shrink_reduces_to_the_single_triggering_op() {
    // Only a Deposit triggers a bug (BugOnDeposit); the withdraws are harmless filler. Shrink must
    // strip the sequence down to that one op and keep failing the same way.
    let (bank, _log) = Bank::new(1, Behavior::BugOnDeposit);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);

    let mut failing: Vec<Op> = (0..7)
        .map(|_| Op::Withdraw {
            user: 0,
            amount: 10,
        })
        .collect();
    failing.insert(3, Op::Deposit { user: 0, amount: 5 });

    let minimized = r
        .shrink(failing, || async { bank_env(1).await.unwrap() })
        .await;
    assert_eq!(minimized.len(), 1, "shrunk to a single op: {minimized:?}");
    assert!(matches!(minimized[0], Op::Deposit { .. }));

    // The minimized sequence still fails, and fails the same way (a Bug).
    let (ctx, world) = bank_env(1).await.unwrap();
    r.setup(ctx, world);
    let rep = r.run_scenario(minimized).await;
    assert!(matches!(
        rep.failure.expect("still fails").kind,
        FailureKind::Bug(_)
    ));
}

#[tokio::test]
async fn run_and_shrink_puts_minimized_history_in_report() {
    let (bank, _log) = Bank::new(1, Behavior::BugOnDeposit);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);

    let mut ops: Vec<Op> = (0..5)
        .map(|_| Op::Withdraw {
            user: 0,
            amount: 10,
        })
        .collect();
    ops.push(Op::Deposit { user: 0, amount: 1 });

    let rep = r
        .run_and_shrink(ops, || async { bank_env(1).await.unwrap() })
        .await;
    let f = rep.failure.expect("still fails");
    assert_eq!(f.history.len(), 1, "history minimized: {:?}", f.history);
    assert!(matches!(f.history[0], Op::Deposit { .. }));
}

#[tokio::test]
async fn stats_describe_only_the_final_shrunk_redrive() {
    // Shrink replays a failing sequence dozens of times; the runner's stats must describe only the
    // final minimized re-drive, not the cumulative tally of every shrink candidate.
    let (bank, _log) = Bank::new(1, Behavior::BugOnDeposit);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.with_stats();
    r.setup(ctx, world);

    let mut ops: Vec<Op> = (0..5)
        .map(|_| Op::Withdraw {
            user: 0,
            amount: 10,
        })
        .collect();
    ops.push(Op::Deposit { user: 0, amount: 1 });

    let rep = r
        .run_and_shrink(ops, || async { bank_env(1).await.unwrap() })
        .await;
    assert_eq!(rep.failure.expect("still fails").history.len(), 1);

    let stats = r.stats().expect("stats stay enabled across a shrink");
    // The final re-drive ran exactly one op: the minimized Deposit. No Withdraw bucket, no
    // accumulation from the original run or the shrink candidates.
    let d = stats.get("Deposit").expect("deposit stats recorded");
    assert_eq!(d.count, 1, "one op in the final re-drive");
    assert_eq!(d.bug, 1);
    assert!(
        stats.get("Withdraw").is_none(),
        "no stats from pre-shrink runs or shrink candidates"
    );
}

#[tokio::test]
async fn shrink_never_converges_on_a_different_bug() {
    // Two distinct bugs: the Deposit bugs with "deposit exploded", a Withdraw of 13 with
    // "withdraw exploded". The reference failure is the Deposit's (it runs first). A shrink that
    // compared bugs by discriminant only could drop the Deposit and return the Withdraw's bug;
    // detail comparison must pin the minimized sequence to the original bug.
    let (bank, _log) = Bank::new(1, Behavior::BugOnBoth);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);

    let failing = vec![
        Op::Deposit { user: 0, amount: 5 },
        Op::Withdraw {
            user: 0,
            amount: 13,
        },
    ];
    let minimized = r
        .shrink(failing, || async { bank_env(1).await.unwrap() })
        .await;
    assert_eq!(minimized, vec![Op::Deposit { user: 0, amount: 5 }]);

    let (ctx, world) = bank_env(1).await.unwrap();
    r.setup(ctx, world);
    let f = r
        .run_scenario(minimized)
        .await
        .failure
        .expect("still fails");
    assert!(
        matches!(f.kind, FailureKind::Bug(ref d) if d == "deposit exploded"),
        "must reproduce the original bug, got {:?}",
        f.kind
    );
}

#[tokio::test]
async fn shrink_with_honors_the_original_check_cadence() {
    // Under DriftModel the invariant breaks after the withdraw, but with `check_every = 2` a
    // single-op candidate is never checked, so neither op can be dropped: the minimized sequence
    // must keep both. Under `check_every = 1` the same input shrinks to the withdraw alone.
    let failing = vec![
        Op::Deposit {
            user: 0,
            amount: 50,
        },
        Op::Withdraw {
            user: 0,
            amount: 20,
        },
    ];

    let (bank, _log) = Bank::new(1, Behavior::DriftModel);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let minimized = r
        .shrink_with(failing.clone(), 2, || async { bank_env(1).await.unwrap() })
        .await;
    assert_eq!(
        minimized.len(),
        2,
        "cadence-2 failure needs both ops: {minimized:?}"
    );

    let (bank, _log) = Bank::new(1, Behavior::DriftModel);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let minimized = r
        .shrink(failing, || async { bank_env(1).await.unwrap() })
        .await;
    assert_eq!(
        minimized,
        vec![Op::Withdraw {
            user: 0,
            amount: 20,
        }],
        "per-op checking shrinks to the drifting withdraw alone"
    );

    // run_and_shrink_with drives the whole pipeline under the same cadence.
    let (bank, _log) = Bank::new(1, Behavior::DriftModel);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);
    let ops = vec![
        Op::Deposit {
            user: 0,
            amount: 50,
        },
        Op::Withdraw {
            user: 0,
            amount: 20,
        },
    ];
    let rep = r
        .run_and_shrink_with(ops, 2, || async { bank_env(1).await.unwrap() })
        .await;
    assert_eq!(rep.failure.expect("still fails").history.len(), 2);
}

#[tokio::test]
async fn empty_kind_slice_is_an_infra_failure_not_a_panic() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::fuzz(bank, 0);
    r.setup(ctx, world);
    let no_kinds: &[OpKind] = &[];
    let rep = r.run(10, Some(no_kinds), 1).await;
    assert_eq!(rep.steps, 0, "nothing can run without a kind to draw");
    let f = rep.failure.expect("reported as a failure");
    assert!(matches!(f.kind, FailureKind::Infra(_)), "{:?}", f.kind);
}

#[tokio::test]
async fn shrink_replay_budget_caps_rebuilds_and_returns_best_so_far() {
    // 128 one-token deposits, bug only when the cumulative balance hits exactly 1_000 + 128:
    // no strict subsequence reproduces, so ddmin exhausts its worst case (~380 attempts for
    // n = 128) and must stop at the budget instead, returning the (irreducible) input unchanged.
    let (bank, _log) = Bank::new(1, Behavior::BugAtBalance(1_000 + 128));
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::scenario(bank, 0);
    r.setup(ctx, world);

    let failing: Vec<Op> = (0..128)
        .map(|_| Op::Deposit { user: 0, amount: 1 })
        .collect();

    let rebuilds = Rc::new(RefCell::new(0usize));
    let counter = rebuilds.clone();
    let minimized = r
        .shrink(failing.clone(), move || {
            let counter = counter.clone();
            async move {
                *counter.borrow_mut() += 1;
                bank_env(1).await.unwrap()
            }
        })
        .await;

    assert_eq!(
        minimized, failing,
        "irreducible sequence returned unchanged"
    );
    // One rebuild establishes the reference failure; candidates are capped by the budget. Without
    // the cap this ddmin worst case costs ~380 replays.
    assert!(
        *rebuilds.borrow() <= 1 + DEFAULT_SHRINK_LIMIT,
        "rebuilds capped at 1 + {DEFAULT_SHRINK_LIMIT}, got {}",
        rebuilds.borrow()
    );
    assert!(
        *rebuilds.borrow() > DEFAULT_SHRINK_LIMIT / 2,
        "sanity: the worst case actually exercised the budget, got {}",
        rebuilds.borrow()
    );
}

#[tokio::test]
async fn replay_reproduces_the_same_failure() {
    // Drive a failing invariant run, then replay its recorded history on a fresh harness.
    let (bank, _log) = Bank::new(2, Behavior::DriftModel);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::invariant(bank, 9);
    r.setup(ctx, world);
    let rep = r.run(200, None, 1).await;
    assert!(!rep.passed(), "expected drift to break the invariant");
    let f = rep.failure.unwrap();

    let (bank2, _log2) = Bank::new(2, Behavior::DriftModel);
    let (ctx2, world2) = bank_env(2).await.unwrap();
    let mut r2 = Runner::scenario(bank2, rep.seed);
    r2.setup(ctx2, world2);
    let rep2 = r2.replay(f.history.clone()).await;
    assert!(!rep2.passed(), "replay should reproduce the failure");
    let f2 = rep2.failure.unwrap();
    assert_eq!(f.step, f2.step, "replay failed at a different step");
    assert_eq!(f.history.len(), f2.history.len());
}
/// Golden-seed pin: the exact op sequence `seed = 42` produces. The runner's RNG draw order
/// (kind choice, then op data, in generation order) is a compatibility surface: if this test
/// changes, every recorded seed in every bug report and regression test stops reproducing.
/// A refactor must keep this sequence identical; only an intentional, CHANGELOG-documented
/// generation change may update the literal.
#[tokio::test]
async fn golden_seed_sequence_is_stable() {
    let (bank, log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::fuzz(bank, 42);
    r.setup(ctx, world);
    let rep = r.run(6, None, 1).await;
    assert!(rep.passed(), "{:?}", rep.failure);
    assert_eq!(
        *log.borrow(),
        vec![
            Op::Withdraw {
                user: 1,
                amount: 301
            },
            Op::Deposit {
                user: 1,
                amount: 95
            },
            Op::Withdraw {
                user: 1,
                amount: 915
            },
            Op::Deposit {
                user: 0,
                amount: 32
            },
            Op::Withdraw {
                user: 0,
                amount: 1569
            },
            Op::Withdraw {
                user: 1,
                amount: 159
            },
        ],
        "seed 42 must reproduce the recorded sequence exactly"
    );
}

/// Golden-seed pin for [`KindMix::Weighted`]: the exact op sequence `seed = 42` produces under a
/// 3:1 Deposit:Withdraw mix. Same compatibility surface as [`golden_seed_sequence_is_stable`]: the
/// draw order (weighted kind index, then op data) is pinned in `runner.rs`'s `OpSource::Weighted`;
/// only an intentional, CHANGELOG-documented generation change may update this literal.
#[tokio::test]
async fn weighted_golden_seed_sequence_is_stable() {
    let (bank, log) = Bank::new(2, Behavior::Good);
    let (ctx, world) = bank_env(2).await.unwrap();
    let mut r = Runner::fuzz(bank, 42);
    r.setup(ctx, world);
    let mix = KindMix::Weighted(&[(OpKind::Deposit, 3), (OpKind::Withdraw, 1)]);
    let rep = r.run_with(6, mix, 1).await;
    assert!(rep.passed(), "{:?}", rep.failure);
    assert_eq!(
        *log.borrow(),
        vec![
            Op::Deposit {
                user: 0,
                amount: 80
            },
            Op::Withdraw {
                user: 0,
                amount: 1282
            },
            Op::Deposit { user: 0, amount: 8 },
            Op::Deposit {
                user: 1,
                amount: 84
            },
            Op::Withdraw {
                user: 1,
                amount: 1463
            },
            Op::Deposit {
                user: 0,
                amount: 43
            },
        ],
        "seed 42 under a 3:1 Deposit:Withdraw mix must reproduce the recorded sequence exactly"
    );
}

#[tokio::test]
async fn weighted_empty_pairs_is_an_infra_failure() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::fuzz(bank, 0);
    r.setup(ctx, world);
    let rep = r.run_with(10, KindMix::Weighted(&[]), 1).await;
    assert_eq!(rep.steps, 0, "nothing can run without a kind to draw");
    let f = rep.failure.expect("reported as a failure");
    assert!(matches!(f.kind, FailureKind::Infra(_)), "{:?}", f.kind);
}

#[tokio::test]
async fn weighted_all_zero_weights_is_an_infra_failure() {
    let (bank, _log) = Bank::new(1, Behavior::Good);
    let (ctx, world) = bank_env(1).await.unwrap();
    let mut r = Runner::fuzz(bank, 0);
    r.setup(ctx, world);
    let mix = KindMix::Weighted(&[(OpKind::Deposit, 0), (OpKind::Withdraw, 0)]);
    let rep = r.run_with(10, mix, 1).await;
    assert_eq!(rep.steps, 0, "nothing can run without a kind to draw");
    let f = rep.failure.expect("reported as a failure");
    assert!(matches!(f.kind, FailureKind::Infra(_)), "{:?}", f.kind);
}
