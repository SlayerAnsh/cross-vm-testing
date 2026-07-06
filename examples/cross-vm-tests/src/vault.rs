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
/// `amount` in practice fits a `u64` (see the op struct docs); deserialize as `u64` and widen,
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

// Each op is externally tagged by its kind name so a TOML scenario step writes
// `op = { deposit = { chain = "eth", user = 0, amount = 1000 } }` (spec section 7.1). `amount` is
// `u128`; TOML cannot hold a `u128` literal directly, but every scenario amount in practice fits
// a `u64` (see `deserialize_u128_from_u64`), and fuzz/invariant-generated amounts are never
// round-tripped through TOML (they are only ever serialized into the JSON failure-history
// artifact, which handles `u128` natively; the `deserialize_with` below only narrows the TOML
// read path, `Serialize` is untouched).

/// Credit `amount` of collateral to `user` on `chain`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Deposit {
    /// The chain label (an injected `MultiChainEnv` key).
    pub chain: String,
    /// Index into the user roster (0 = alice, 1 = bob).
    pub user: usize,
    /// The amount to deposit.
    #[serde(deserialize_with = "deserialize_u128_from_u64")]
    pub amount: u128,
}

impl DynOp<Ctx, VaultWorld> for Deposit {
    fn kind(&self) -> &'static str {
        "deposit"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut VaultWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            // Invalidate any prior deposit snapshot; re-armed at the end so the transition
            // invariant applies to exactly this op.
            w.pre = None;
            let vault = vault_handle(ctx, w, &self.chain)?;
            // Snapshot pre-state for the transition invariant (async query, stashed in World).
            let before = vault
                .collateral_of(USERS[self.user])
                .await
                .map_err(HarnessError::infra)?;
            let res = vault.deposit(USERS[self.user], self.amount).await;
            let verdict = classify(
                true,
                res,
                || w.models.get_mut(&self.chain).unwrap().collateral[self.user] += self.amount,
                "",
                "valid deposit reverted",
            )?;
            w.pre = Some(DepositSnapshot {
                chain: self.chain.clone(),
                user: self.user,
                before,
                amount: self.amount,
            });
            Ok(verdict)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Withdraw `amount` of free collateral for `user` on `chain`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Withdraw {
    /// The chain label (an injected `MultiChainEnv` key).
    pub chain: String,
    /// Index into the user roster (0 = alice, 1 = bob).
    pub user: usize,
    /// The amount to withdraw.
    #[serde(deserialize_with = "deserialize_u128_from_u64")]
    pub amount: u128,
}

impl DynOp<Ctx, VaultWorld> for Withdraw {
    fn kind(&self) -> &'static str {
        "withdraw"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut VaultWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            w.pre = None;
            let ok = w.models[&self.chain].can_withdraw(self.user, self.amount);
            let vault = vault_handle(ctx, w, &self.chain)?;
            let res = vault.withdraw(USERS[self.user], self.amount).await;
            classify(
                ok,
                res,
                || w.models.get_mut(&self.chain).unwrap().collateral[self.user] -= self.amount,
                "over-withdraw was accepted",
                "valid withdraw reverted",
            )
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Borrow `amount` of debt against `user`'s collateral on `chain`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Borrow {
    /// The chain label (an injected `MultiChainEnv` key).
    pub chain: String,
    /// Index into the user roster (0 = alice, 1 = bob).
    pub user: usize,
    /// The amount to borrow.
    #[serde(deserialize_with = "deserialize_u128_from_u64")]
    pub amount: u128,
}

impl DynOp<Ctx, VaultWorld> for Borrow {
    fn kind(&self) -> &'static str {
        "borrow"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut VaultWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            w.pre = None;
            let ok = w.models[&self.chain].can_borrow(self.user, self.amount);
            let vault = vault_handle(ctx, w, &self.chain)?;
            let res = vault.borrow(USERS[self.user], self.amount).await;
            classify(
                ok,
                res,
                || w.models.get_mut(&self.chain).unwrap().debt[self.user] += self.amount,
                "over-borrow was accepted (bad debt)",
                "valid borrow reverted",
            )
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Repay `amount` of `user`'s debt on `chain`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Repay {
    /// The chain label (an injected `MultiChainEnv` key).
    pub chain: String,
    /// Index into the user roster (0 = alice, 1 = bob).
    pub user: usize,
    /// The amount to repay.
    #[serde(deserialize_with = "deserialize_u128_from_u64")]
    pub amount: u128,
}

impl DynOp<Ctx, VaultWorld> for Repay {
    fn kind(&self) -> &'static str {
        "repay"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut VaultWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            w.pre = None;
            let ok = w.models[&self.chain].can_repay(self.user, self.amount);
            let vault = vault_handle(ctx, w, &self.chain)?;
            let res = vault.repay(USERS[self.user], self.amount).await;
            classify(
                ok,
                res,
                || w.models.get_mut(&self.chain).unwrap().debt[self.user] -= self.amount,
                "over-repay was accepted",
                "valid repay reverted",
            )
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Per-user chain collateral/debt equals the shadow model.
#[derive(Clone, Debug)]
pub struct ModelMatches;

impl DynInvariant<Ctx, VaultWorld> for ModelMatches {
    fn check<'a>(&'a self, ctx: &'a mut Ctx, w: &'a VaultWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            for label in LABELS {
                let vault = match vault_handle(ctx, w, label) {
                    Ok(v) => v,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                let model = &w.models[label];
                for (i, user) in USERS.iter().enumerate() {
                    let c = match vault.collateral_of(user).await {
                        Ok(c) => c,
                        Err(e) => return CheckOutcome::violated(e.to_string()),
                    };
                    let d = match vault.debt_of(user).await {
                        Ok(d) => d,
                        Err(e) => return CheckOutcome::violated(e.to_string()),
                    };
                    if c != model.collateral[i] || d != model.debt[i] {
                        return CheckOutcome::violated(format!(
                            "{label}/{user}: chain (c={c}, d={d}) != model (c={}, d={})",
                            model.collateral[i], model.debt[i]
                        ));
                    }
                }
            }
            CheckOutcome::Held
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }
}

/// No user's debt exceeds the LTV limit on their collateral.
#[derive(Clone, Debug)]
pub struct NoBadDebt;

impl DynInvariant<Ctx, VaultWorld> for NoBadDebt {
    fn check<'a>(&'a self, ctx: &'a mut Ctx, w: &'a VaultWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            for label in LABELS {
                let vault = match vault_handle(ctx, w, label) {
                    Ok(v) => v,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                for user in USERS {
                    let c = match vault.collateral_of(user).await {
                        Ok(c) => c,
                        Err(e) => return CheckOutcome::violated(e.to_string()),
                    };
                    let d = match vault.debt_of(user).await {
                        Ok(d) => d,
                        Err(e) => return CheckOutcome::violated(e.to_string()),
                    };
                    if d > VaultModel::max_debt(c) {
                        return CheckOutcome::violated(format!(
                            "{label}/{user}: debt {d} exceeds max {}",
                            VaultModel::max_debt(c)
                        ));
                    }
                }
            }
            CheckOutcome::Held
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }
}

/// Aggregate debt is backed by aggregate collateral.
#[derive(Clone, Debug)]
pub struct Solvency;

impl DynInvariant<Ctx, VaultWorld> for Solvency {
    fn check<'a>(&'a self, ctx: &'a mut Ctx, w: &'a VaultWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            for label in LABELS {
                let vault = match vault_handle(ctx, w, label) {
                    Ok(v) => v,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                let (mut tot_c, mut tot_d) = (0u128, 0u128);
                for user in USERS {
                    match vault.collateral_of(user).await {
                        Ok(c) => tot_c += c,
                        Err(e) => return CheckOutcome::violated(e.to_string()),
                    }
                    match vault.debt_of(user).await {
                        Ok(d) => tot_d += d,
                        Err(e) => return CheckOutcome::violated(e.to_string()),
                    }
                }
                if tot_d > VaultModel::max_debt(tot_c) {
                    return CheckOutcome::violated(format!(
                        "{label}: total debt {tot_d} exceeds max {}",
                        VaultModel::max_debt(tot_c)
                    ));
                }
            }
            CheckOutcome::Held
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }
}

/// Transition invariant: after a `deposit`, the depositor's on-chain collateral rose by exactly
/// the deposited amount. Compares live post-state against a snapshot `apply` took just before the
/// op (see [`DepositSnapshot`]); [`skipped`](CheckOutcome::skipped) when the last op was not a
/// deposit.
#[derive(Clone, Debug)]
pub struct DepositTransition;

impl DynInvariant<Ctx, VaultWorld> for DepositTransition {
    fn check<'a>(&'a self, ctx: &'a mut Ctx, w: &'a VaultWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            let Some(snap) = &w.pre else {
                return CheckOutcome::skipped("last op was not a deposit");
            };
            let vault = match vault_handle(ctx, w, &snap.chain) {
                Ok(v) => v,
                Err(e) => return CheckOutcome::violated(e.to_string()),
            };
            let post = match vault.collateral_of(USERS[snap.user]).await {
                Ok(c) => c,
                Err(e) => return CheckOutcome::violated(e.to_string()),
            };
            let expected = snap.before + snap.amount;
            if post == expected {
                CheckOutcome::Held
            } else {
                CheckOutcome::violated(format!(
                    "{}/{}: post-deposit collateral {post} != pre {} + amount {}",
                    snap.chain, USERS[snap.user], snap.before, snap.amount
                ))
            }
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, VaultWorld>> {
        Box::new(self.clone())
    }
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

/// Rebuild a `Vault` handle bound to the deployed instance on `label`.
fn vault_handle(ctx: &Ctx, world: &VaultWorld, label: &str) -> Result<Vault, HarnessError> {
    let chain = ctx.chain(label)?;
    let addr = world
        .addrs
        .get(label)
        .cloned()
        .ok_or_else(|| HarnessError::infra(format!("no vault deployed on {label}")))?;
    Ok(Vault::instance(chain, addr))
}

/// Pick a random `(chain, user)` for a generated op: the shared preamble of every generator.
fn random_target(rng: &mut Prng) -> (String, usize) {
    let chain = LABELS[rng.index(LABELS.len())].to_string();
    let user = rng.index(USERS.len());
    (chain, user)
}

fn gen_deposit(rng: &mut Prng, _w: &VaultWorld) -> Box<dyn DynOp<Ctx, VaultWorld>> {
    let (chain, user) = random_target(rng);
    Box::new(Deposit {
        chain,
        user,
        amount: rng.range(1, 1_000_000),
    })
}

fn gen_withdraw(rng: &mut Prng, w: &VaultWorld) -> Box<dyn DynOp<Ctx, VaultWorld>> {
    let (chain, user) = random_target(rng);
    // Span past free collateral so some withdraws are (correctly) rejected.
    let amount = rng.range(1, w.models[&chain].collateral[user].max(1) * 2 + 2);
    Box::new(Withdraw {
        chain,
        user,
        amount,
    })
}

fn gen_borrow(rng: &mut Prng, w: &VaultWorld) -> Box<dyn DynOp<Ctx, VaultWorld>> {
    let (chain, user) = random_target(rng);
    let amount = rng.range(
        1,
        VaultModel::max_debt(w.models[&chain].collateral[user]).max(1) + 2,
    );
    Box::new(Borrow {
        chain,
        user,
        amount,
    })
}

fn gen_repay(rng: &mut Prng, w: &VaultWorld) -> Box<dyn DynOp<Ctx, VaultWorld>> {
    let (chain, user) = random_target(rng);
    let amount = rng.range(1, w.models[&chain].debt[user].max(1) + 2);
    Box::new(Repay {
        chain,
        user,
        amount,
    })
}

// Deposit-heavy kind mix; the generators still own all per-kind data.
fn weight_deposit(_ctx: &Ctx, _w: &VaultWorld) -> u32 {
    40
}

fn weight_withdraw(_ctx: &Ctx, _w: &VaultWorld) -> u32 {
    25
}

fn weight_borrow(_ctx: &Ctx, _w: &VaultWorld) -> u32 {
    20
}

fn weight_repay(_ctx: &Ctx, _w: &VaultWorld) -> u32 {
    15
}

fn advance(ctx: &mut Ctx, blocks: u64) -> OpFuture<'_, Result<(), HarnessError>> {
    Box::pin(async move {
        ctx.advance_all(blocks).await;
        Ok(())
    })
}

/// The DeFi vault harness: drives deposit/withdraw/borrow/repay across every chain in
/// [`VaultWorld`] and checks solvency / no-bad-debt / model-match / deposit-transition invariants.
pub fn vault_harness() -> OpSetHarness<Ctx, VaultWorld> {
    OpSetHarness::new()
        .register(
            OpDef::new("deposit", gen_deposit, decode_json_op::<Deposit, _, _>)
                .with_weight(weight_deposit),
        )
        .register(
            OpDef::new("withdraw", gen_withdraw, decode_json_op::<Withdraw, _, _>)
                .with_weight(weight_withdraw),
        )
        .register(
            OpDef::new("borrow", gen_borrow, decode_json_op::<Borrow, _, _>)
                .with_weight(weight_borrow),
        )
        .register(
            OpDef::new("repay", gen_repay, decode_json_op::<Repay, _, _>).with_weight(weight_repay),
        )
        .invariant(Box::new(ModelMatches))
        .invariant(Box::new(NoBadDebt))
        .invariant(Box::new(Solvency))
        .invariant(Box::new(DepositTransition))
        .with_advance(advance)
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
    vault.setup("alice").await.map_err(HarnessError::infra)?;
    let addr = vault
        .address()
        .ok_or_else(|| HarnessError::infra(format!("{label}: setup recorded no address")))?;
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
