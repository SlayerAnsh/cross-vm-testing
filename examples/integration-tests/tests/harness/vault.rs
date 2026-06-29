//! DeFi harness example: a collateralized-debt `Vault` across CosmWasm, EVM, and Solana.
//!
//! `vault_setup` builds one [`MultiChainEnv`] holding the vault on three chains and returns it as
//! the live [`Ctx`] plus the primed `VaultWorld`; each test loads it with `r.setup(ctx, world)`.
//! The persisted `VaultWorld` keeps only the per-chain deployed address and the per-chain shadow
//! model. `apply` / `check` rebuild a `Vault` handle on demand from the `Ctx` plus the stored
//! address. State-aware `generate` produces mostly-valid operations (with a slice of invalid
//! amounts); `apply` uses [`classify`] to tell a legitimate revert from a bug; invariants compare
//! chain state against the model and assert solvency / no-bad-debt. One `apply` drives invariant,
//! fuzz, and endurance runs, and an rstest matrix fans out per chain.

use std::collections::HashMap;
#[cfg(feature = "endurance")]
use std::time::Duration;

use cross_vm_framework::prelude::*;
#[cfg(feature = "endurance")]
use cross_vm_macros::endurance_runner;
#[cfg(feature = "fuzz")]
use cross_vm_macros::fuzz_runner;
#[cfg(feature = "invariant")]
use cross_vm_macros::invariant_runner;

use crate::support::{fund_user, test_wallets, Vault};

const LABELS: [&str; 3] = ["osmosis", "eth", "solana"];
const USERS: [&str; 2] = ["alice", "bob"];
const LTV_BPS: u128 = 5000;

#[derive(Clone, Debug)]
enum VaultOp {
    Deposit {
        chain: String,
        user: usize,
        amount: u128,
    },
    Withdraw {
        chain: String,
        user: usize,
        amount: u128,
    },
    Borrow {
        chain: String,
        user: usize,
        amount: u128,
    },
    Repay {
        chain: String,
        user: usize,
        amount: u128,
    },
}

#[derive(Clone, Debug)]
enum VaultInv {
    /// Per-user chain collateral/debt equals the shadow model.
    ModelMatches,
    /// No user's debt exceeds the LTV limit on their collateral.
    NoBadDebt,
    /// Aggregate debt is backed by aggregate collateral.
    Solvency,
}

/// The data-free kinds of [`VaultOp`], for per-kind fuzzing.
#[derive(Clone, Copy, Debug)]
enum VaultOpKind {
    Deposit,
    Withdraw,
    Borrow,
    Repay,
}

/// Per-chain shadow model.
struct VaultModel {
    collateral: Vec<u128>,
    debt: Vec<u128>,
}

impl VaultModel {
    fn new(users: usize) -> Self {
        Self {
            collateral: vec![0; users],
            debt: vec![0; users],
        }
    }
    fn max_debt(c: u128) -> u128 {
        c * LTV_BPS / 10000
    }
    fn required_collateral(d: u128) -> u128 {
        (d * 10000).div_ceil(LTV_BPS)
    }
    fn can_withdraw(&self, u: usize, a: u128) -> bool {
        a <= self.collateral[u] && self.collateral[u] - a >= Self::required_collateral(self.debt[u])
    }
    fn can_borrow(&self, u: usize, a: u128) -> bool {
        self.debt[u] + a <= Self::max_debt(self.collateral[u])
    }
    fn can_repay(&self, u: usize, a: u128) -> bool {
        a <= self.debt[u]
    }
}

/// Persisted state: where the vault is deployed per chain, plus the per-chain shadow model.
struct VaultWorld {
    addrs: HashMap<String, Account>,
    models: HashMap<String, VaultModel>,
}

struct VaultHarness;

impl VaultHarness {
    /// Rebuild a `Vault` handle bound to the deployed instance on `label`.
    fn vault(ctx: &Ctx, world: &VaultWorld, label: &str) -> Result<Vault, HarnessError> {
        let chain = ctx.chain(label)?;
        let addr = world.addrs.get(label).cloned().ok_or_else(|| {
            HarnessError::Infra(CrossVmError::wallet(format!(
                "no vault deployed on {label}"
            )))
        })?;
        Ok(Vault::instance(chain, addr))
    }
}

impl Harness for VaultHarness {
    type World = VaultWorld;
    type Operation = VaultOp;
    type Invariant = VaultInv;
    type OpKind = VaultOpKind;

    async fn apply(
        &self,
        ctx: &mut Ctx,
        w: &mut VaultWorld,
        op: &VaultOp,
    ) -> Result<Verdict, HarnessError> {
        match op {
            VaultOp::Deposit {
                chain,
                user,
                amount,
            } => {
                let vault = Self::vault(ctx, w, chain)?;
                let res = vault.deposit(USERS[*user], *amount).await;
                classify(
                    true,
                    res,
                    || w.models.get_mut(chain).unwrap().collateral[*user] += *amount,
                    "",
                    "valid deposit reverted",
                )
            }
            VaultOp::Withdraw {
                chain,
                user,
                amount,
            } => {
                let ok = w.models[chain].can_withdraw(*user, *amount);
                let vault = Self::vault(ctx, w, chain)?;
                let res = vault.withdraw(USERS[*user], *amount).await;
                classify(
                    ok,
                    res,
                    || w.models.get_mut(chain).unwrap().collateral[*user] -= *amount,
                    "over-withdraw was accepted",
                    "valid withdraw reverted",
                )
            }
            VaultOp::Borrow {
                chain,
                user,
                amount,
            } => {
                let ok = w.models[chain].can_borrow(*user, *amount);
                let vault = Self::vault(ctx, w, chain)?;
                let res = vault.borrow(USERS[*user], *amount).await;
                classify(
                    ok,
                    res,
                    || w.models.get_mut(chain).unwrap().debt[*user] += *amount,
                    "over-borrow was accepted (bad debt)",
                    "valid borrow reverted",
                )
            }
            VaultOp::Repay {
                chain,
                user,
                amount,
            } => {
                let ok = w.models[chain].can_repay(*user, *amount);
                let vault = Self::vault(ctx, w, chain)?;
                let res = vault.repay(USERS[*user], *amount).await;
                classify(
                    ok,
                    res,
                    || w.models.get_mut(chain).unwrap().debt[*user] -= *amount,
                    "over-repay was accepted",
                    "valid repay reverted",
                )
            }
        }
    }

    fn op_kinds(&self) -> Vec<VaultOpKind> {
        vec![
            VaultOpKind::Deposit,
            VaultOpKind::Withdraw,
            VaultOpKind::Borrow,
            VaultOpKind::Repay,
        ]
    }

    fn generate_op(&self, rng: &mut Prng, w: &VaultWorld, kind: VaultOpKind) -> VaultOp {
        let chain = LABELS[rng.index(LABELS.len())].to_string();
        let user = rng.index(USERS.len());
        let model = &w.models[&chain];
        match kind {
            VaultOpKind::Deposit => VaultOp::Deposit {
                chain,
                user,
                amount: rng.range(1, 1_000_000),
            },
            VaultOpKind::Withdraw => VaultOp::Withdraw {
                // Span past free collateral so some withdraws are (correctly) rejected.
                chain,
                user,
                amount: rng.range(1, model.collateral[user].max(1) * 2 + 2),
            },
            VaultOpKind::Borrow => VaultOp::Borrow {
                chain,
                user,
                amount: rng.range(1, VaultModel::max_debt(model.collateral[user]).max(1) + 2),
            },
            VaultOpKind::Repay => VaultOp::Repay {
                chain,
                user,
                amount: rng.range(1, model.debt[user].max(1) + 2),
            },
        }
    }

    // Bias the kind mix (deposit-heavy); reuse `generate_op` for the per-kind data.
    fn generate(&self, rng: &mut Prng, w: &VaultWorld) -> VaultOp {
        let kind = match rng.weighted(&[40, 25, 20, 15]) {
            0 => VaultOpKind::Deposit,
            1 => VaultOpKind::Withdraw,
            2 => VaultOpKind::Borrow,
            _ => VaultOpKind::Repay,
        };
        self.generate_op(rng, w, kind)
    }

    fn invariants(&self) -> Vec<VaultInv> {
        vec![
            VaultInv::ModelMatches,
            VaultInv::NoBadDebt,
            VaultInv::Solvency,
        ]
    }

    async fn check(&self, ctx: &mut Ctx, w: &VaultWorld, inv: &VaultInv) -> CheckOutcome {
        for label in LABELS {
            let vault = match Self::vault(ctx, w, label) {
                Ok(v) => v,
                Err(e) => return CheckOutcome::violated(e.to_string()),
            };
            let model = &w.models[label];
            let (mut tot_c, mut tot_d) = (0u128, 0u128);
            for (i, user) in USERS.iter().enumerate() {
                let c = match vault.collateral_of(user).await {
                    Ok(c) => c,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                let d = match vault.debt_of(user).await {
                    Ok(d) => d,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                tot_c += c;
                tot_d += d;
                match inv {
                    VaultInv::ModelMatches => {
                        if c != model.collateral[i] || d != model.debt[i] {
                            return CheckOutcome::violated(format!(
                                "{label}/{user}: chain (c={c}, d={d}) != model (c={}, d={})",
                                model.collateral[i], model.debt[i]
                            ));
                        }
                    }
                    VaultInv::NoBadDebt => {
                        if d > VaultModel::max_debt(c) {
                            return CheckOutcome::violated(format!(
                                "{label}/{user}: debt {d} exceeds max {}",
                                VaultModel::max_debt(c)
                            ));
                        }
                    }
                    VaultInv::Solvency => {}
                }
            }
            if let VaultInv::Solvency = inv {
                if tot_d > VaultModel::max_debt(tot_c) {
                    return CheckOutcome::violated(format!(
                        "{label}: total debt {tot_d} exceeds max {}",
                        VaultModel::max_debt(tot_c)
                    ));
                }
            }
        }
        CheckOutcome::Held
    }
}

/// Build the live env (vault deployed on all three chains, both users funded) and the primed world.
/// Each test calls it and loads the result with `r.setup(ctx, world)`. Deterministic, so the
/// per-case `seed` is unused.
async fn vault_setup(_seed: u64) -> Result<(Ctx, VaultWorld), HarnessError> {
    crate::support::init_tracing();
    let wallets = test_wallets();
    let mut env = MultiChainEnv::new("vault-harness", wallets.clone());
    env.inject("osmosis", OSMOSIS.mock(wallets.clone()));
    env.inject("eth", ETHEREUM.mock(wallets.clone()));
    env.inject("solana", SOLANA_DEVNET.mock(wallets));
    let ctx = Ctx::new(env.start().await?);

    let mut addrs = HashMap::new();
    let mut models = HashMap::new();
    for label in LABELS {
        let mut chain = ctx.chain(label)?;
        for user in USERS {
            fund_user(&mut chain, WalletLabel::wrap(user)).await;
        }
        let vault = Vault::new(chain);
        vault.setup("alice").await.map_err(HarnessError::Infra)?;
        let addr = vault.address().ok_or_else(|| {
            HarnessError::Infra(CrossVmError::wallet(format!(
                "{label}: setup recorded no address"
            )))
        })?;
        addrs.insert(label.to_string(), addr);
        models.insert(label.to_string(), VaultModel::new(USERS.len()));
    }
    Ok((ctx, VaultWorld { addrs, models }))
}

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
