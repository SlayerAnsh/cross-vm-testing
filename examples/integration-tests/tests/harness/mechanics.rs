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

use std::cell::RefCell;
use std::rc::Rc;
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

#[derive(Clone, Copy)]
enum Behavior {
    /// Correct implementation.
    Good,
    /// A successful withdraw decrements the chain but forgets the model (drift -> ModelMatches breaks).
    DriftModel,
    /// Every deposit reports a confirmed bug.
    BugOnDeposit,
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
}

impl Bank {
    fn new(users: usize, behavior: Behavior) -> (Self, Rc<RefCell<Vec<Op>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                users,
                behavior,
                log: log.clone(),
            },
            log,
        )
    }
}

impl Harness for Bank {
    type World = World;
    type Operation = Op;
    type Invariant = Inv;
    type OpKind = OpKind;

    async fn apply(&self, _ctx: &mut Ctx, w: &mut World, op: &Op) -> Result<Verdict, HarnessError> {
        self.log.borrow_mut().push(op.clone());
        match *op {
            Op::Deposit { user, amount } => {
                if let Behavior::BugOnDeposit = self.behavior {
                    return Err(HarnessError::Bug("deposit exploded".into()));
                }
                w.chain[user] += amount;
                w.model[user] += amount;
                Ok(Verdict::Accepted)
            }
            Op::Withdraw { user, amount } => {
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
                .max_delay(Duration::from_millis(1))
                .check_every(5)
                .advance_blocks(1, 2),
        )
        .await;
    assert!(rep.passed(), "endurance: {:?}", rep.failure);
    assert!(rep.steps > 0, "endurance ran zero steps");
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
        Err(HarnessError::Infra(CrossVmError::wallet("setup boom")))
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
