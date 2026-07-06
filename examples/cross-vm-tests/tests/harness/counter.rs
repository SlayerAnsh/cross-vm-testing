//! Property-testing harness example: a multi-chain `Counter`.
//!
//! `counter_setup` builds one [`MultiChainEnv`] holding the counter on three chains (`"osmosis"` /
//! `"eth"` / `"solana"`) and returns it as the live [`Ctx`] plus the primed `CounterWorld`. Each
//! test calls it and loads the result into a mode-typed runner with `r.setup(ctx, world)`. The
//! persisted `CounterWorld` keeps only bookkeeping: each chain's deployed address, a per-chain
//! shadow count, and whether any increment has happened yet. `apply` / `check` rebuild a `Counter`
//! handle on demand from the `Ctx` and the stored address (`Counter::instance(..)`), which is what
//! keeps the live env and the persisted state cleanly separated.
//!
//! The same harness drives an **rstest matrix** (`#[values]` x `#[values]` -> 3x3 = 9 cases) via a
//! [`ScenarioRunner`], and the **fuzz** / **invariant** / **endurance** modes via the
//! `#[fuzz_runner]` / `#[invariant_runner]` / `#[endurance_runner]` attribute macros.

use std::collections::HashMap;
#[cfg(feature = "endurance")]
use std::time::Duration;

use cross_vm_framework::prelude::*;
#[cfg(feature = "rpc-endurance")]
use cross_vm_solidity::chains::BASE_SEPOLIA;

use crate::support::{fund_alice, test_wallets, Counter, CounterSpec};

/// The chains every test deploys on. Two variants gated by `rpc-endurance`: without it, the three
/// mocks; with it, a live Base Sepolia chain (`"base"`) is appended. Injection in [`counter_setup`]
/// is driven by this list, so `"base"` is built only when its label is present.
#[cfg(not(feature = "rpc-endurance"))]
const LABELS: [&str; 3] = ["osmosis", "eth", "solana"];
#[cfg(feature = "rpc-endurance")]
const LABELS: [&str; 4] = ["osmosis", "eth", "solana", "base"];

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
struct CounterWorld {
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
struct Increment {
    chain: String,
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
struct IncrementOnTwoChains {
    chain_1: String,
    chain_2: String,
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
struct CountMatchesModel;

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

fn advance(ctx: &mut Ctx, blocks: u64) -> OpFuture<'_, Result<(), HarnessError>> {
    Box::pin(async move {
        ctx.advance_all(blocks).await;
        Ok(())
    })
}

/// Assemble the multi-chain counter harness.
fn counter_harness() -> OpSetHarness<Ctx, CounterWorld> {
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

/// Build the live env (counter deployed on all three chains) and the primed world. A free function
/// the tests call themselves and load into a runner with `r.setup(ctx, world)`. Deterministic, so
/// the per-case `seed` is unused; a test needing seed-varied initial state would read it.
async fn counter_setup(_seed: u64) -> Result<(Ctx, CounterWorld), HarnessError> {
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
        // Cloned handle shares the live chain's state, so funding and deploy land on it.
        let mut chain = ctx.chain(label)?;
        // A live RPC chain cannot be minted into; its key (`on_chain`) must already be funded.
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
