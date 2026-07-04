//! End-to-end smoke test for [`harness_cli::test_bridge::run_profile_for_test`] over the raw
//! [`GenericDomain`]: a trivial `Ctx = ()`, `World = i64` harness with one `Add(i64)` op and an
//! `i64` model invariant, driven through the same bridge the `#[config_runner]` macro expands
//! into. Proves the generic bridge loads a config, resolves a fuzz profile, and drives one fuzz
//! case to a passing report without any chain/domain infrastructure.

use std::sync::atomic::{AtomicU64, Ordering};

use harness_cli::test_bridge::run_profile_for_test;
use harness_cli::{BasicSetup, GenericDomain, SetupFuture};
use harness_core::{CheckOutcome, Harness, HarnessError, Prng, Verdict};

/// One operation: add a non-negative delta to the running sum.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum Op {
    /// Add `n` to the world's running sum.
    Add(i64),
}

/// The kind tag for [`Op`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum OpKind {
    /// Selects [`Op::Add`].
    Add,
}

/// The lone invariant: the running sum only ever grows from `0`, so it never goes negative.
#[derive(Debug, Clone)]
enum Inv {
    /// `world >= 0` (the i64 model invariant).
    NonNegative,
}

/// A trivial harness whose `World` is a single `i64` running sum; every op adds a non-negative
/// delta, so the `NonNegative` invariant always holds.
struct SmokeHarness;

impl Harness for SmokeHarness {
    type Ctx = ();
    type World = i64;
    type Operation = Op;
    type Invariant = Inv;
    type OpKind = OpKind;

    async fn apply(
        &self,
        _ctx: &mut Self::Ctx,
        world: &mut Self::World,
        op: &Self::Operation,
    ) -> Result<Verdict, HarnessError> {
        match op {
            Op::Add(n) => {
                *world += *n;
                Ok(Verdict::Accepted)
            }
        }
    }

    fn op_kinds(&self) -> Vec<Self::OpKind> {
        vec![OpKind::Add]
    }

    fn generate_op(
        &self,
        rng: &mut Prng,
        _world: &Self::World,
        kind: Self::OpKind,
    ) -> Self::Operation {
        match kind {
            // Non-negative delta keeps the running sum monotonically non-decreasing from 0.
            OpKind::Add => Op::Add(rng.below(100) as i64),
        }
    }

    fn invariants(&self) -> Vec<Self::Invariant> {
        vec![Inv::NonNegative]
    }

    async fn check(
        &self,
        _ctx: &mut Self::Ctx,
        world: &Self::World,
        inv: &Self::Invariant,
    ) -> CheckOutcome {
        match inv {
            Inv::NonNegative if *world >= 0 => CheckOutcome::Held,
            Inv::NonNegative => {
                CheckOutcome::violated(format!("running sum went negative: {world}"))
            }
        }
    }
}

/// Builds the harness's `(Ctx, World)`: a unit `Ctx` (no live system) and a `World` starting at 0.
fn smoke_setup(_s: BasicSetup) -> SetupFuture<'static, (), i64> {
    Box::pin(async move { Ok(((), 0i64)) })
}

/// Writes a fresh temp config, unique per invocation (process id plus a monotonic counter), so
/// parallel test runs never collide.
fn write_temp_config(body: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "harness-cli-generic-bridge-{}-{n}.toml",
        std::process::id()
    ));
    std::fs::write(&path, body).expect("write temp config");
    path
}

const SMOKE_CONFIG: &str = r#"
[harness]
name = "smoke"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 5
kinds = ["Add"]
"#;

#[tokio::test]
async fn generic_bridge_drives_one_fuzz_case() {
    let path = write_temp_config(SMOKE_CONFIG);
    run_profile_for_test::<GenericDomain, _, _, _>(
        path.to_str().unwrap(),
        || SmokeHarness,
        smoke_setup,
        "smoke",
        Some(0),
        Some(1),
    )
    .await;
    std::fs::remove_file(&path).ok();
}
