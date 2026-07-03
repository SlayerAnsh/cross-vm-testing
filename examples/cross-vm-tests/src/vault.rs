//! DeFi harness example: a collateralized-debt `Vault` across CosmWasm, EVM, and Solana.
//!
//! [`vault_setup`] builds one [`MultiChainEnv`] holding the vault on three hard coded mock chains
//! and returns it as the live [`Ctx`] plus the primed [`VaultWorld`]; each test loads it with
//! `r.setup(ctx, world)`. [`vault_config_setup`] is the config driven counterpart the `cross-vm`
//! CLI registers: it honors `SetupRequest::chain_specs` when a TOML config declares `[[chain]]`
//! entries, and falls back to the same hard coded mocks when it does not (so a config with no
//! chain declarations behaves exactly like [`vault_setup`]).
//!
//! The persisted [`VaultWorld`] keeps only the per-chain deployed address and the per-chain shadow
//! model. `apply` / `check` rebuild a `Vault` handle on demand from the `Ctx` plus the stored
//! address. State-aware `generate` produces mostly-valid operations (with a slice of invalid
//! amounts); `apply` uses [`classify`] to tell a legitimate revert from a bug; invariants compare
//! chain state against the model and assert solvency / no-bad-debt. One `apply` drives invariant,
//! fuzz, and endurance runs, and an rstest matrix (in the `harness` integration test) fans out per
//! chain.

use std::collections::HashMap;

use cross_vm_framework::config::{build_chain, SetupFuture, SetupRequest, Target};
use cross_vm_framework::prelude::*;
use serde::Deserialize;

use crate::support::{fund_user, test_wallets, Vault};

/// TOML has no native 128-bit integer, and `toml::Value`'s `Deserializer` impl only overrides
/// `deserialize_*` up through `u64`/`i64` (see its `forward_to_deserialize_any!` list); a bare
/// `u128` field's derived `Deserialize` always hits serde's default `deserialize_u128`, which
/// errors `"u128 is not supported"` regardless of the value's actual magnitude. Every scenario
/// `amount` in practice fits a `u64` (see the [`VaultOp`] docs); deserialize as `u64` and widen,
/// so a `[[profile.<name>.steps]]` TOML step round-trips.
fn deserialize_u128_from_u64<'de, D>(deserializer: D) -> Result<u128, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u64::deserialize(deserializer).map(u128::from)
}

/// Chain labels the hard coded (no `[[chain]]`) path injects, in iteration order.
const LABELS: [&str; 3] = ["osmosis", "eth", "solana"];
/// The two wallet labels every scenario/fuzz/invariant run funds and drives.
const USERS: [&str; 2] = ["alice", "bob"];
/// Loan-to-value ceiling, in basis points (50%).
const LTV_BPS: u128 = 5000;

/// One vault action: deposit, withdraw, borrow, or repay a given `amount` for `user` on `chain`.
///
/// Externally tagged (serde default) so a TOML scenario step writes
/// `op = { Deposit = { chain = "eth", user = 0, amount = 1000 } }` (spec section 7.1). `amount` is
/// `u128`; TOML cannot hold a `u128` literal directly, but every scenario amount in practice fits
/// a `u64` (see [`deserialize_u128_from_u64`]), and fuzz/invariant-generated amounts are never
/// round-tripped through TOML (they are only ever serialized into the JSON failure-history
/// artifact, which handles `u128` natively — the `deserialize_with` below only narrows the TOML
/// read path, `Serialize` is untouched).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum VaultOp {
    /// Credit `amount` of collateral to `user` on `chain`.
    Deposit {
        /// The chain label (an injected `MultiChainEnv` key).
        chain: String,
        /// Index into the user roster (0 = alice, 1 = bob).
        user: usize,
        /// The amount to deposit.
        #[serde(deserialize_with = "deserialize_u128_from_u64")]
        amount: u128,
    },
    /// Withdraw `amount` of free collateral for `user` on `chain`.
    Withdraw {
        /// The chain label (an injected `MultiChainEnv` key).
        chain: String,
        /// Index into the user roster (0 = alice, 1 = bob).
        user: usize,
        /// The amount to withdraw.
        #[serde(deserialize_with = "deserialize_u128_from_u64")]
        amount: u128,
    },
    /// Borrow `amount` of debt against `user`'s collateral on `chain`.
    Borrow {
        /// The chain label (an injected `MultiChainEnv` key).
        chain: String,
        /// Index into the user roster (0 = alice, 1 = bob).
        user: usize,
        /// The amount to borrow.
        #[serde(deserialize_with = "deserialize_u128_from_u64")]
        amount: u128,
    },
    /// Repay `amount` of `user`'s debt on `chain`.
    Repay {
        /// The chain label (an injected `MultiChainEnv` key).
        chain: String,
        /// Index into the user roster (0 = alice, 1 = bob).
        user: usize,
        /// The amount to repay.
        #[serde(deserialize_with = "deserialize_u128_from_u64")]
        amount: u128,
    },
}

/// The invariants [`VaultHarness`] checks after each op.
///
/// `pub` only because it is `Harness::Invariant` for the `pub` [`VaultHarness`] impl (an
/// associated-type leak, not a type callers construct); its variants carry no data callers need.
#[derive(Clone, Debug)]
pub enum VaultInv {
    /// Per-user chain collateral/debt equals the shadow model.
    ModelMatches,
    /// No user's debt exceeds the LTV limit on their collateral.
    NoBadDebt,
    /// Aggregate debt is backed by aggregate collateral.
    Solvency,
    /// Transition invariant: after a `Deposit`, the depositor's on-chain collateral rose by exactly
    /// the deposited amount. Compares live post-state against a snapshot `apply` took just before
    /// the op (see `DepositSnapshot`); [`Skipped`](CheckOutcome::Skipped) when the last op was not
    /// a deposit.
    DepositTransition,
}

/// A pre-op snapshot for the [`VaultInv::DepositTransition`] invariant, captured inside `apply`
/// (async, holds `Ctx`) and stashed in the `World` — the pattern for transition invariants, as
/// opposed to the sync contract hooks which cannot query chain state.
struct DepositSnapshot {
    chain: String,
    user: usize,
    /// The depositor's on-chain collateral immediately before the deposit.
    before: u128,
    /// The amount deposited.
    amount: u128,
}

/// The data-free kinds of [`VaultOp`], for per-kind fuzzing.
///
/// `Copy` (required by the registry's `ConfigHarness` bound) plus externally tagged serde so a
/// TOML profile can restrict/weight kinds by name (`kinds = ["Deposit", "Withdraw"]`,
/// `weights = { Deposit = 40, ... }`, spec section 4.4).
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum VaultOpKind {
    /// See [`VaultOp::Deposit`].
    Deposit,
    /// See [`VaultOp::Withdraw`].
    Withdraw,
    /// See [`VaultOp::Borrow`].
    Borrow,
    /// See [`VaultOp::Repay`].
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

/// Persisted state: where the vault is deployed per chain, the per-chain shadow model, and the most
/// recent deposit snapshot (for the transition invariant).
pub struct VaultWorld {
    addrs: HashMap<String, Account>,
    models: HashMap<String, VaultModel>,
    /// Snapshot of the last `Deposit`'s pre-state, or `None` if the last op was not a deposit.
    pre: Option<DepositSnapshot>,
}

/// The DeFi vault [`Harness`]: drives deposit/withdraw/borrow/repay across every chain in
/// [`VaultWorld`] and checks solvency / no-bad-debt / model-match invariants.
pub struct VaultHarness;

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
        // Invalidate any prior deposit snapshot; the Deposit arm re-arms it below. This makes the
        // transition invariant apply to exactly the op that just ran.
        w.pre = None;
        match op {
            VaultOp::Deposit {
                chain,
                user,
                amount,
            } => {
                let vault = Self::vault(ctx, w, chain)?;
                // Snapshot pre-state for the transition invariant (async query, stashed in World).
                let before = vault
                    .collateral_of(USERS[*user])
                    .await
                    .map_err(HarnessError::Infra)?;
                let res = vault.deposit(USERS[*user], *amount).await;
                let verdict = classify(
                    true,
                    res,
                    || w.models.get_mut(chain).unwrap().collateral[*user] += *amount,
                    "",
                    "valid deposit reverted",
                )?;
                w.pre = Some(DepositSnapshot {
                    chain: chain.clone(),
                    user: *user,
                    before,
                    amount: *amount,
                });
                Ok(verdict)
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
            VaultInv::DepositTransition,
        ]
    }

    async fn check(&self, ctx: &mut Ctx, w: &VaultWorld, inv: &VaultInv) -> CheckOutcome {
        // Transition invariant: diff live post-state against the snapshot `apply` stashed in World.
        if let VaultInv::DepositTransition = inv {
            let Some(snap) = &w.pre else {
                return CheckOutcome::skipped("last op was not a deposit");
            };
            let vault = match Self::vault(ctx, w, &snap.chain) {
                Ok(v) => v,
                Err(e) => return CheckOutcome::violated(e.to_string()),
            };
            let post = match vault.collateral_of(USERS[snap.user]).await {
                Ok(c) => c,
                Err(e) => return CheckOutcome::violated(e.to_string()),
            };
            let expected = snap.before + snap.amount;
            return if post == expected {
                CheckOutcome::Held
            } else {
                CheckOutcome::violated(format!(
                    "{}/{}: post-deposit collateral {post} != pre {} + amount {}",
                    snap.chain, USERS[snap.user], snap.before, snap.amount
                ))
            };
        }
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
                    // Handled before the per-chain loop; unreachable here.
                    VaultInv::DepositTransition => {}
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

/// Fund both users and deploy a fresh `Vault` on `label`, priming `addrs`/`models` for it.
/// Shared by [`vault_setup`] and [`vault_config_setup`] so the two setups only differ in how the
/// chain itself gets injected.
async fn deploy_and_prime(
    ctx: &Ctx,
    label: &str,
    addrs: &mut HashMap<String, Account>,
    models: &mut HashMap<String, VaultModel>,
) -> Result<(), HarnessError> {
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
    Ok(())
}

/// Build one `AnyChain` for `target` from a preset's mock/rpc constructors. Used by
/// [`vault_config_setup`]'s hard coded (no `[[chain]]`) fallback path, so that path still honors
/// `SetupRequest::target` (mock by default, rpc when a config's top level `[env].target = "rpc"`,
/// even without per chain `[[chain]]` declarations).
fn chain_for_target<M: Into<AnyChain>, R: Into<AnyChain>>(
    target: Target,
    mock: impl FnOnce() -> M,
    rpc: impl FnOnce() -> R,
) -> AnyChain {
    match target {
        Target::Mock => mock().into(),
        Target::Rpc => rpc().into(),
    }
}

/// Build the live env (vault deployed on all three hard coded chains, both users funded) and the
/// primed world. Each test calls it and loads the result with `r.setup(ctx, world)`. Deterministic,
/// so the per-case `seed` is unused.
///
/// Kept alongside [`vault_config_setup`] (rather than replaced by it) because the
/// `#[fuzz_runner]`/`#[invariant_runner]`/`#[endurance_runner]`-attributed test bodies call it
/// directly by name; those macros only inject the seeded `Runner` shell, the test author's body
/// still does its own setup (see `crates/macros/src/runner_macros.rs`).
pub async fn vault_setup(_seed: u64) -> Result<(Ctx, VaultWorld), HarnessError> {
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
        deploy_and_prime(&ctx, label, &mut addrs, &mut models).await?;
    }
    Ok((
        ctx,
        VaultWorld {
            addrs,
            models,
            pre: None,
        },
    ))
}

/// The config driven counterpart of [`vault_setup`], registered with the `cross-vm` CLI
/// (spec section 6.2). When `req.chain_specs` is empty (no `[[chain]]` in the loaded TOML), this
/// injects the same three hard coded mock/rpc presets as [`vault_setup`], honoring
/// `req.target`. Otherwise it builds one `AnyChain` per resolved [`cross_vm_framework::config::ChainSpecData`]
/// via [`build_chain`] and injects it under its declared label; funding, deploy, and model priming
/// then iterate `req.chain_specs`' labels instead of the hard coded `LABELS`.
pub fn vault_config_setup(req: SetupRequest) -> SetupFuture<'static, VaultWorld> {
    Box::pin(async move {
        crate::support::init_tracing();
        let wallets = test_wallets();
        let mut env = MultiChainEnv::new("vault-harness", wallets.clone());

        let labels: Vec<String> = if req.chain_specs.is_empty() {
            env.inject(
                "osmosis",
                chain_for_target(
                    req.target,
                    || OSMOSIS.mock(wallets.clone()),
                    || OSMOSIS.rpc(wallets.clone()),
                ),
            );
            env.inject(
                "eth",
                chain_for_target(
                    req.target,
                    || ETHEREUM.mock(wallets.clone()),
                    || ETHEREUM.rpc(wallets.clone()),
                ),
            );
            env.inject(
                "solana",
                chain_for_target(
                    req.target,
                    || SOLANA_DEVNET.mock(wallets.clone()),
                    || SOLANA_DEVNET.rpc(wallets.clone()),
                ),
            );
            LABELS.iter().map(|s| s.to_string()).collect()
        } else {
            for spec in &req.chain_specs {
                env.inject(&spec.label, build_chain(spec, wallets.clone())?);
            }
            req.chain_specs.iter().map(|s| s.label.clone()).collect()
        };

        let ctx = Ctx::new(env.start().await?);

        let mut addrs = HashMap::new();
        let mut models = HashMap::new();
        for label in &labels {
            deploy_and_prime(&ctx, label, &mut addrs, &mut models).await?;
        }
        Ok((
            ctx,
            VaultWorld {
                addrs,
                models,
                pre: None,
            },
        ))
    })
}
