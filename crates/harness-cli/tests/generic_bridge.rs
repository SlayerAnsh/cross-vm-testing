//! End-to-end smoke test for [`harness_cli::test_bridge::run_profile_for_test`] over the raw
//! [`GenericDomain`]: a trivial `Ctx = ()`, `World = i64` harness with one `add` op and an
//! `i64` model invariant, driven through the same bridge the `#[config_runner]` macro expands
//! into. Proves the generic bridge loads a config, resolves a fuzz profile, and drives one fuzz
//! case to a passing report without any chain/domain infrastructure.

use std::sync::atomic::{AtomicU64, Ordering};

use harness_cli::test_bridge::run_profile_for_test;
use harness_cli::{BasicSetup, GenericDomain, SetupFuture};
use harness_core::{
    decode_json_op, CheckOutcome, DynInvariant, DynOp, HarnessError, OpDef, OpFuture, OpSetHarness,
    Prng, Verdict,
};

/// One operation: add a non-negative delta to the running sum.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Add {
    /// The delta added to the world's running sum.
    n: i64,
}

impl DynOp<(), i64> for Add {
    fn kind(&self) -> &'static str {
        "add"
    }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut i64,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            *world += self.n;
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), i64>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// The lone invariant: the running sum only ever grows from `0`, so it never goes negative.
#[derive(Debug, Clone)]
struct NonNegative;

impl DynInvariant<(), i64> for NonNegative {
    fn check<'a>(&'a self, _ctx: &'a mut (), world: &'a i64) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            if *world >= 0 {
                CheckOutcome::Held
            } else {
                CheckOutcome::violated(format!("running sum went negative: {world}"))
            }
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<(), i64>> {
        Box::new(self.clone())
    }
}

// Non-negative delta keeps the running sum monotonically non-decreasing from 0.
fn gen_add(rng: &mut Prng, _world: &i64) -> Box<dyn DynOp<(), i64>> {
    Box::new(Add {
        n: rng.below(100) as i64,
    })
}

/// A trivial harness whose `World` is a single `i64` running sum; every op adds a non-negative
/// delta, so the `NonNegative` invariant always holds.
fn smoke_harness() -> OpSetHarness<(), i64> {
    OpSetHarness::new()
        .register(OpDef::new("add", gen_add, decode_json_op::<Add, _, _>))
        .invariant(Box::new(NonNegative))
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
kinds = ["add"]
"#;

#[tokio::test]
async fn generic_bridge_drives_one_fuzz_case() {
    let path = write_temp_config(SMOKE_CONFIG);
    run_profile_for_test::<GenericDomain, _, _, _>(
        path.to_str().unwrap(),
        smoke_harness,
        smoke_setup,
        "smoke",
        Some(0),
        Some(1),
    )
    .await;
    std::fs::remove_file(&path).ok();
}
