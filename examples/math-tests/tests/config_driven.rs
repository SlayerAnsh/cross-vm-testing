//! Drives profiles through the generic test bridge (the `cargo test` path).
//!
//! [`harness_cli::test_bridge::run_profile_for_test`] reloads `math.harness.toml` at run time and drives the
//! named profile through the same registry the CLI uses, panicking on any failure. This is the
//! hand-written equivalent of what the `#[config_runner]` macro would emit.

use harness_cli::GenericDomain;
use math_tests::{math_config_setup, math_harness};

/// Fuzz case 0 of the `smoke` profile (4 cases declared in the config).
#[tokio::test]
async fn smoke_profile_case_0() {
    harness_cli::test_bridge::run_profile_for_test::<GenericDomain, _, _, _>(
        concat!(env!("CARGO_MANIFEST_DIR"), "/math.harness.toml"),
        math_harness,
        math_config_setup,
        "smoke",
        Some(0),
        Some(4),
    )
    .await;
}

/// The whole `steps` scenario profile (no case index: non-fuzz modes run the profile whole).
#[tokio::test]
async fn scenario_profile_runs() {
    harness_cli::test_bridge::run_profile_for_test::<GenericDomain, _, _, _>(
        concat!(env!("CARGO_MANIFEST_DIR"), "/math.harness.toml"),
        math_harness,
        math_config_setup,
        "steps",
        None,
        None,
    )
    .await;
}
