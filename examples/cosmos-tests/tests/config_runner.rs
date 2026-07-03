//! Config-driven style (style b): `#[config_runner]` drives the counter harness against
//! `counter.cross-vm.toml`'s `smoke` fuzz profile through the same `cargo test` surface as every
//! other harness test. `smoke` is `mode = "fuzz"`, `cases = 4`, so this fans out into
//! `counter_smoke_config_case_0` .. `counter_smoke_config_case_3`, each driven through
//! `cross_vm_framework::config::test_bridge::run_profile_for_test` (the same machinery the CLI uses).

use cross_vm_macros::config_runner;

use cosmos_tests::counter::{counter_config_setup, CounterHarness};

#[config_runner(
    config = "counter.cross-vm.toml",
    harness = CounterHarness,
    setup = counter_config_setup,
    profile = "smoke"
)]
async fn counter_smoke_config() {}
