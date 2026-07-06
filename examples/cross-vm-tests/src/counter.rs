//! Property-testing harness example: a multi-chain `Counter`.
//!
//! [`counter_setup`] builds one [`MultiChainEnv`] holding the counter on three chains (`"osmosis"` /
//! `"eth"` / `"solana"`) and returns it as the live [`Ctx`] plus the primed [`CounterWorld`]. Each
//! test calls it and loads the result into a mode-typed runner with `r.setup(ctx, world)`. The
//! persisted [`CounterWorld`] keeps only bookkeeping: each chain's deployed address, a per-chain
//! shadow count, and whether any increment has happened yet. `apply` / `check` rebuild a `Counter`
//! handle on demand from the `Ctx` and the stored address (`Counter::instance(..)`), which is what
//! keeps the live env and the persisted state cleanly separated.
//!
//! [`counter_config_setup`] is the config-driven counterpart the `cross-vm` CLI registers: it
//! honors `SetupRequest::chain_specs` when a TOML config declares `[[chain]]` entries, and falls
//! back to the same three hard coded mocks when it does not (so a config with no chain declarations
//! behaves exactly like [`counter_setup`]'s mock topology).
//!
//! The same harness drives an **rstest matrix** (`#[values]` x `#[values]` -> 3x3 = 9 cases) via a
//! [`ScenarioRunner`], and the **fuzz** / **invariant** / **endurance** modes via the
//! `#[fuzz_runner]` / `#[invariant_runner]` / `#[endurance_runner]` attribute macros.

use std::collections::HashMap;

use cross_vm_framework::config::{build_chain, SetupFuture, SetupRequest, Target};
use cross_vm_framework::prelude::*;
#[cfg(feature = "rpc-endurance")]
use cross_vm_solidity::chains::BASE_SEPOLIA;

use crate::support::{fund_alice, test_wallets, Counter, CounterSpec};

/// The chains every test deploys on. Two variants gated by `rpc-endurance`: without it, the three
/// mocks; with it, a live Base Sepolia chain (`"base"`) is appended. Injection in [`counter_setup`]
/// is driven by this list, so `"base"` is built only when its label is present.
#[cfg(not(feature = "rpc-endurance"))]
pub const LABELS: [&str; 3] = ["osmosis", "eth", "solana"];
/// The chains every test deploys on (see the non-`rpc-endurance` variant); this build appends the
/// live Base Sepolia chain (`"base"`).
#[cfg(feature = "rpc-endurance")]
pub const LABELS: [&str; 4] = ["osmosis", "eth", "solana", "base"];

/// Wallet label used to sign on `chain`: the live `"base"` chain signs with the funded `on_chain`
/// wallet (`ON_CHAIN_WALLET`), every mock chain with the in-memory `alice`.
fn signer(chain: &str) -> &'static str {
    if chain == "base" {
        "on_chain"
    } else {
        "alice"
    }
}

/// Persisted state for one run: where the counter is deployed per chain, the shadow count, and
/// the precondition flag for the invariant. No chains or contract handles live here.
pub struct CounterWorld {
    labels: Vec<String>,
    addrs: HashMap<String, Account>,
    model: HashMap<String, u64>,
    /// Set once any increment lands; the `CountMatchesModel` invariant is skipped until then.
    any_incremented: bool,
}

/// Rebuild a `Counter` handle bound to the deployed instance on `label`. The chain is cloned
/// out of the env (shared state), so the handle reads and writes the one live counter.
fn counter_handle(ctx: &Ctx, world: &CounterWorld, label: &str) -> Result<Counter, HarnessError> {
    let chain = ctx.chain(label)?;
    let addr = world
        .addrs
        .get(label)
        .cloned()
        .ok_or_else(|| HarnessError::infra(format!("no counter deployed on {label}")))?;
    Ok(Counter::instance(chain, addr))
}

/// Increment the counter on one chain. Chains are selected by string label, matching how the
/// `MultiChainEnv` keys its chains.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Increment {
    /// The chain label (an injected `MultiChainEnv` key).
    pub chain: String,
}

impl DynOp<Ctx, CounterWorld> for Increment {
    fn kind(&self) -> &'static str {
        "increment"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut CounterWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            inc(ctx, w, &self.chain).await?;
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, CounterWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Increment the counter on two chains in one op.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct IncrementOnTwoChains {
    /// The first chain label to increment.
    pub chain_1: String,
    /// The second chain label to increment.
    pub chain_2: String,
}

impl DynOp<Ctx, CounterWorld> for IncrementOnTwoChains {
    fn kind(&self) -> &'static str {
        "increment_on_two_chains"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut CounterWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            inc(ctx, w, &self.chain_1).await?;
            inc(ctx, w, &self.chain_2).await?;
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, CounterWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// Every chain's on-chain count equals the shadow model. Only meaningful once at least one
/// increment has been applied; before that it is skipped.
#[derive(Clone, Debug)]
pub struct CountMatchesModel;

impl DynInvariant<Ctx, CounterWorld> for CountMatchesModel {
    fn check<'a>(&'a self, ctx: &'a mut Ctx, w: &'a CounterWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            // Precondition: nothing to compare until the first increment lands.
            if !w.any_incremented {
                return CheckOutcome::skipped("no increment applied yet");
            }
            for label in &w.labels {
                let counter = match counter_handle(ctx, w, label) {
                    Ok(c) => c,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                let n = match counter.count().await {
                    Ok(n) => n,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                let m = *w.model.get(label).expect("model for label");
                if n != m {
                    return CheckOutcome::violated(format!("{label}: chain {n} != model {m}"));
                }
            }
            CheckOutcome::Held
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, CounterWorld>> {
        Box::new(self.clone())
    }
}

fn gen_increment(rng: &mut Prng, w: &CounterWorld) -> Box<dyn DynOp<Ctx, CounterWorld>> {
    let chain = w.labels[rng.index(w.labels.len())].clone();
    Box::new(Increment { chain })
}

fn gen_increment_on_two_chains(
    rng: &mut Prng,
    w: &CounterWorld,
) -> Box<dyn DynOp<Ctx, CounterWorld>> {
    let chain_1 = w.labels[rng.index(w.labels.len())].clone();
    let chain_2 = w.labels[rng.index(w.labels.len())].clone();
    Box::new(IncrementOnTwoChains { chain_1, chain_2 })
}

// Bias toward single-chain increments (two-chain drawn 1 in 4).
fn weight_increment(_ctx: &Ctx, _w: &CounterWorld) -> u32 {
    3
}

/// Advance block height on every chain in the multi-chain env by `blocks` (the runner's
/// `with_advance` hook; the default implementation warps them all).
pub fn advance(ctx: &mut Ctx, blocks: u64) -> OpFuture<'_, Result<(), HarnessError>> {
    Box::pin(async move {
        ctx.advance_all(blocks).await;
        Ok(())
    })
}

/// Assemble the multi-chain counter harness.
pub fn counter_harness() -> OpSetHarness<Ctx, CounterWorld> {
    OpSetHarness::new()
        .register(
            OpDef::new(
                "increment",
                gen_increment,
                decode_json_op::<Increment, _, _>,
            )
            .with_weight(weight_increment),
        )
        .register(OpDef::new(
            "increment_on_two_chains",
            gen_increment_on_two_chains,
            decode_json_op::<IncrementOnTwoChains, _, _>,
        ))
        .invariant(Box::new(CountMatchesModel))
        .with_advance(advance)
}

/// Increment the counter on `label` and bump its model. An increment never legitimately fails,
/// so any error is infrastructure.
async fn inc(ctx: &mut Ctx, w: &mut CounterWorld, label: &str) -> Result<(), HarnessError> {
    let counter = counter_handle(ctx, w, label)?;
    counter
        .increment(signer(label))
        .await
        .map_err(HarnessError::infra)?;
    *w.model
        .get_mut(label)
        .ok_or_else(|| HarnessError::infra(format!("unknown chain {label}")))? += 1;
    w.any_incremented = true;
    Ok(())
}

/// Fund `alice` (except on the live `"base"` chain), deploy a fresh `Counter` on `label`, and
/// prime `addrs`/`model` for it. Shared by [`counter_setup`] and [`counter_config_setup`] so the
/// two setups only differ in how the chain itself gets injected.
///
/// Inherited quirk from the `rpc-endurance` topology: a chain labeled exactly `"base"` skips
/// funding (a live RPC chain cannot be minted into) and signs with the funded `on_chain` wallet
/// instead of `alice`. A config-driven `[[chain]]` that happens to use the label `"base"` inherits
/// that behavior.
async fn deploy_and_prime(
    ctx: &Ctx,
    label: &str,
    addrs: &mut HashMap<String, Account>,
    model: &mut HashMap<String, u64>,
) -> Result<(), HarnessError> {
    let mut chain = ctx.chain(label)?;
    if label != "base" {
        fund_alice(&mut chain).await;
    }
    let counter = Counter::new(chain);
    counter
        .setup(signer(label))
        .await
        .map_err(HarnessError::infra)?;
    let addr = counter
        .address()
        .ok_or_else(|| HarnessError::infra(format!("{label}: setup recorded no address")))?;
    addrs.insert(label.to_string(), addr);
    model.insert(label.to_string(), 0u64);
    Ok(())
}

/// Build one `AnyChain` for `target` from a preset's mock/rpc constructors. Used by
/// [`counter_config_setup`]'s hard coded (no `[[chain]]`) fallback path, so that path still honors
/// `SetupRequest::target` (mock by default, rpc when a config's top level `[env].target = "rpc"`,
/// even without per chain `[[chain]]` declarations).
///
/// Duplicated privately here (rather than shared with `src/vault.rs`'s equivalent) to keep the
/// vault module untouched.
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

/// Build the live env (counter deployed on all chains in [`LABELS`]) and the primed world. A free
/// function the tests call themselves and load into a runner with `r.setup(ctx, world)`.
/// Deterministic, so the per-case `seed` is unused; a test needing seed-varied initial state would
/// read it.
///
/// Kept alongside [`counter_config_setup`] (rather than replaced by it) because the
/// `#[fuzz_runner]`/`#[invariant_runner]`/`#[endurance_runner]`-attributed test bodies call it
/// directly by name; those macros only inject the seeded `Runner` shell, the test author's body
/// still does its own setup.
pub async fn counter_setup(_seed: u64) -> Result<(Ctx, CounterWorld), HarnessError> {
    crate::support::init_tracing();
    // Load the workspace `.env` so the `on_chain` wallet's `ON_CHAIN_WALLET` is in the process env
    // when the live `"base"` chain signs. Harmless (and ignored) when absent for mock-only runs.
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));

    let wallets = test_wallets();
    let mut env = MultiChainEnv::new("counter-harness", wallets.clone());
    // Inject a chain per label. The live `"base"` chain is in `LABELS` only under `rpc-endurance`,
    // so it is built only then; the others are always in-process mocks.
    for label in LABELS {
        let chain: AnyChain = match label {
            "osmosis" => OSMOSIS.mock(wallets.clone()).into(),
            "eth" => ETHEREUM.mock(wallets.clone()).into(),
            "solana" => SOLANA_DEVNET.mock(wallets.clone()).into(),
            #[cfg(feature = "rpc-endurance")]
            "base" => BASE_SEPOLIA.rpc(wallets.clone()).into(),
            other => panic!("counter_setup: unknown chain label {other:?}"),
        };
        env.inject(label, chain);
    }
    let ctx = Ctx::new(env.start().await?);

    let mut addrs = HashMap::new();
    let mut model = HashMap::new();
    for label in LABELS {
        deploy_and_prime(&ctx, label, &mut addrs, &mut model).await?;
    }
    Ok((
        ctx,
        CounterWorld {
            labels: LABELS.iter().map(|s| s.to_string()).collect(),
            addrs,
            model,
            any_incremented: false,
        },
    ))
}

/// The config driven counterpart of [`counter_setup`], registered with the `cross-vm` CLI. When
/// `req.chain_specs` is empty (no `[[chain]]` in the loaded TOML), this injects the same three hard
/// coded osmosis/eth/solana mock/rpc presets, honoring `req.target`. Otherwise it builds one
/// `AnyChain` per resolved [`cross_vm_framework::config::ChainSpecData`] via [`build_chain`] and
/// injects it under its declared label; funding, deploy, and model priming then iterate
/// `req.chain_specs`' labels instead of the hard coded list.
pub fn counter_config_setup(req: SetupRequest) -> SetupFuture<'static, CounterWorld> {
    Box::pin(async move {
        crate::support::init_tracing();
        let wallets = test_wallets();
        let mut env = MultiChainEnv::new("counter-harness", wallets.clone());

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
            ["osmosis", "eth", "solana"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            for spec in &req.chain_specs {
                env.inject(&spec.label, build_chain(spec, wallets.clone())?);
            }
            req.chain_specs.iter().map(|s| s.label.clone()).collect()
        };

        let ctx = Ctx::new(env.start().await?);
        let mut addrs = HashMap::new();
        let mut model = HashMap::new();
        for label in &labels {
            deploy_and_prime(&ctx, label, &mut addrs, &mut model).await?;
        }
        Ok((
            ctx,
            CounterWorld {
                labels,
                addrs,
                model,
                any_incremented: false,
            },
        ))
    })
}
