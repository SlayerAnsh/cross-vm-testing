//! Single-VM (Tron/TVM) `Counter` harness, driven three ways from one place.
//!
//! [`Counter`] is a thin wrapper over the Tron counter contract (deploy, increment, read). The
//! [`CounterHarness`] drives it: `apply` performs an op and advances a shadow model, `check`
//! asserts the on-chain count matches. [`counter_setup`] builds the live env for the attribute-macro
//! tests; [`counter_config_setup`] is the config-driven counterpart the `cross-vm` CLI registers,
//! honoring a `[[chain]]` declaration and falling back to the mock `TRON_NILE` preset otherwise.

use cross_vm_common::mocks::counter::tron as counter_tron;
use cross_vm_common::wallets::{fund_user, test_wallets};
use cross_vm_framework::config::{build_chain, SetupFuture, SetupRequest, Target};
use cross_vm_framework::prelude::*;
use cross_vm_tron::Bytes;
use serde::{Deserialize, Serialize};

/// The chain label this single-VM harness deploys and operates on when no `[[chain]]` is declared.
const LABEL: &str = "tron";
/// The wallet that signs deploys and increments.
const SIGNER: &str = "alice";

/// A minimal Tron `Counter` handle: deploy, increment, read the count.
pub struct Counter {
    base: ContractBase,
}

impl Counter {
    /// A counter not yet deployed on `chain`. Call [`Counter::setup`] to deploy.
    pub fn new(chain: AnyChain) -> Self {
        Self {
            base: ContractBase::new(chain),
        }
    }

    /// A counter attached to an already-deployed instance at `address`. Lets the harness rebuild a
    /// handle from a stored address.
    pub fn instance(chain: AnyChain, address: Account) -> Self {
        Self {
            base: ContractBase::with_address(chain, address),
        }
    }

    /// The deployed instance address, if [`Counter::setup`] (or [`Counter::instance`]) bound one.
    pub fn address(&self) -> Option<Account> {
        self.base.address()
    }

    /// Deploy the counter (TVM-native bytecode), signed by `wallet`.
    pub async fn setup(&self, wallet: &str) -> Result<(), CrossVmError> {
        let chain = self.base.tron()?;
        let addr = chain
            .deploy_create(
                counter_tron::Counter::BYTECODE.clone(),
                Bytes::new(),
                WalletLabel::wrap(wallet),
            )
            .await?;
        self.base.set_address(Account::Tron(addr));
        Ok(())
    }

    /// Increment the count by one, signed by `wallet`.
    pub async fn increment(&self, wallet: &str) -> Result<(), CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.tron()?;
        let addr = self.base.tron_addr()?;
        let calldata = Bytes::from(counter_tron::Counter::incrementCall {}.abi_encode());
        chain.call(&addr, calldata, WalletLabel::wrap(wallet)).await?;
        Ok(())
    }

    /// Read the current count.
    pub async fn count(&self) -> Result<u64, CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.tron()?;
        let addr = self.base.tron_addr()?;
        let out = chain
            .static_call(
                &addr,
                Bytes::from(counter_tron::Counter::countCall {}.abi_encode()),
            )
            .await?;
        let n =
            counter_tron::Counter::countCall::abi_decode_returns(&out).map_err(|e| {
                CrossVmError::Query {
                    kind: ChainKind::Tron,
                    reason: e.to_string(),
                }
            })?;
        Ok(n.saturating_to::<u64>())
    }
}

/// One counter action. Externally tagged (serde default) so a TOML scenario step writes the unit
/// variant as a bare string, e.g. `op = "Increment"`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CounterOp {
    /// Increment the count by one.
    Increment,
    /// Increment the count twice (exercises multi-call ops and a +2 model step).
    IncrementTwice,
}

/// The data-free kinds of [`CounterOp`], for per-kind fuzzing (`kinds = [...]` / `weights = {...}`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum CounterOpKind {
    /// See [`CounterOp::Increment`].
    Increment,
    /// See [`CounterOp::IncrementTwice`].
    IncrementTwice,
}

/// The invariants [`CounterHarness`] checks after each op.
///
/// `pub` only because it is `Harness::Invariant` for the `pub` [`CounterHarness`]; its variants
/// carry no data callers need.
#[derive(Clone, Debug)]
pub enum CounterInv {
    /// The on-chain count equals the shadow model (skipped until the first increment lands).
    CountMatchesModel,
}

/// Persisted state: where the counter is deployed, the shadow count, and the invariant precondition.
pub struct CounterWorld {
    label: String,
    addr: Account,
    model: u64,
    any_incremented: bool,
}

/// The single-VM counter [`Harness`]: increments the counter and checks the on-chain count against
/// a shadow model. One `apply` drives the scenario, fuzz, invariant, and endurance modes.
pub struct CounterHarness;

impl CounterHarness {
    /// Rebuild a `Counter` handle bound to the deployed instance.
    fn counter(ctx: &Ctx, world: &CounterWorld) -> Result<Counter, HarnessError> {
        let chain = ctx.chain(&world.label)?;
        Ok(Counter::instance(chain, world.addr.clone()))
    }
}

impl Harness for CounterHarness {
    type World = CounterWorld;
    type Operation = CounterOp;
    type Invariant = CounterInv;
    type OpKind = CounterOpKind;

    async fn apply(
        &self,
        ctx: &mut Ctx,
        w: &mut CounterWorld,
        op: &CounterOp,
    ) -> Result<Verdict, HarnessError> {
        let times = match op {
            CounterOp::Increment => 1,
            CounterOp::IncrementTwice => 2,
        };
        let counter = Self::counter(ctx, w)?;
        for _ in 0..times {
            counter
                .increment(SIGNER)
                .await
                .map_err(HarnessError::Infra)?;
            w.model += 1;
        }
        w.any_incremented = true;
        Ok(Verdict::Accepted)
    }

    fn op_kinds(&self) -> Vec<CounterOpKind> {
        vec![CounterOpKind::Increment, CounterOpKind::IncrementTwice]
    }

    fn generate_op(&self, _rng: &mut Prng, _w: &CounterWorld, kind: CounterOpKind) -> CounterOp {
        match kind {
            CounterOpKind::Increment => CounterOp::Increment,
            CounterOpKind::IncrementTwice => CounterOp::IncrementTwice,
        }
    }

    // Bias toward single increments (25% double).
    fn generate(&self, rng: &mut Prng, w: &CounterWorld) -> CounterOp {
        let kind = if rng.chance(0.25) {
            CounterOpKind::IncrementTwice
        } else {
            CounterOpKind::Increment
        };
        self.generate_op(rng, w, kind)
    }

    fn invariants(&self) -> Vec<CounterInv> {
        vec![CounterInv::CountMatchesModel]
    }

    async fn check(&self, ctx: &mut Ctx, w: &CounterWorld, inv: &CounterInv) -> CheckOutcome {
        match inv {
            CounterInv::CountMatchesModel => {
                if !w.any_incremented {
                    return CheckOutcome::skipped("no increment applied yet");
                }
                let counter = match Self::counter(ctx, w) {
                    Ok(c) => c,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                match counter.count().await {
                    Ok(n) if n == w.model => CheckOutcome::Held,
                    Ok(n) => CheckOutcome::violated(format!("chain {n} != model {}", w.model)),
                    Err(e) => CheckOutcome::violated(e.to_string()),
                }
            }
        }
    }
}

/// Fund the signer and deploy a fresh `Counter` on `chain` under `label`, returning the primed
/// world.
async fn deploy_and_prime(
    ctx: &Ctx,
    label: &str,
) -> Result<CounterWorld, HarnessError> {
    let mut chain = ctx.chain(label)?;
    fund_user(&mut chain, WalletLabel::wrap(SIGNER)).await;
    let counter = Counter::new(chain);
    counter.setup(SIGNER).await.map_err(HarnessError::Infra)?;
    let addr = counter.address().ok_or_else(|| {
        HarnessError::Infra(CrossVmError::wallet(format!(
            "{label}: setup recorded no address"
        )))
    })?;
    Ok(CounterWorld {
        label: label.to_string(),
        addr,
        model: 0,
        any_incremented: false,
    })
}

/// Build the live env (counter deployed on one mock Tron chain) and the primed world. The
/// attribute-macro tests call this directly; deterministic, so `seed` is unused.
pub async fn counter_setup(_seed: u64) -> Result<(Ctx, CounterWorld), HarnessError> {
    cross_vm_common::init_tracing();
    let wallets = test_wallets();
    let mut env = MultiChainEnv::new("counter-harness", wallets.clone());
    env.inject(LABEL, TRON_NILE.mock(wallets.clone()));
    let ctx = Ctx::new(env.start().await?);
    let world = deploy_and_prime(&ctx, LABEL).await?;
    Ok((ctx, world))
}

/// The config-driven counterpart of [`counter_setup`], registered with the `cross-vm` CLI. When the
/// loaded TOML declares no `[[chain]]`, this injects the mock (or rpc) `TRON_NILE` preset under
/// `"tron"`, honoring `req.target`. Otherwise it builds the first declared chain via [`build_chain`]
/// and operates on it (a single-VM harness uses exactly one chain).
pub fn counter_config_setup(req: SetupRequest) -> SetupFuture<'static, CounterWorld> {
    Box::pin(async move {
        cross_vm_common::init_tracing();
        let wallets = test_wallets();
        let mut env = MultiChainEnv::new("counter-harness", wallets.clone());

        let label = if let Some(spec) = req.chain_specs.first() {
            env.inject(&spec.label, build_chain(spec, wallets.clone())?);
            spec.label.clone()
        } else {
            let chain: AnyChain = match req.target {
                Target::Mock => TRON_NILE.mock(wallets.clone()).into(),
                Target::Rpc => TRON_NILE.rpc(wallets.clone()).into(),
            };
            env.inject(LABEL, chain);
            LABEL.to_string()
        };

        let ctx = Ctx::new(env.start().await?);
        let world = deploy_and_prime(&ctx, &label).await?;
        Ok((ctx, world))
    })
}
