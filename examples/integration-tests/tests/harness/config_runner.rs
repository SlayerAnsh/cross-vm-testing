//! Usage test for `#[config_runner]` (spec `docs/config-runs-spec.md` section 13/P5): drives the
//! DeFi vault harness against `vault.cross-vm.toml`'s `smoke` fuzz profile through the same
//! `cargo test` surface every other harness test in this crate uses, instead of the `cross-vm`
//! CLI binary `tests/cli_e2e.rs` exercises.
//!
//! `smoke` is `mode = "fuzz"`, `cases = 8` (see `vault.cross-vm.toml`), so this one
//! `#[config_runner]`-attributed fn fans out into `vault_smoke_config_case_0` ..
//! `vault_smoke_config_case_7`, each driving one fuzz case through
//! `cross_vm_framework::config::test_bridge::run_profile_for_test` — the same
//! `Registry`/`run_one_fuzz_case` machinery `cross-vm run vault.cross-vm.toml --profile smoke`
//! drives (see `tests/cli_e2e.rs`'s `smoke` coverage for that path). `vault_config_setup` builds
//! the same three mock chains (`osmosis`/`eth`/`solana`) `vault.cross-vm.toml` declares, so this
//! passes deterministically on mocks with no network access.

use cross_vm_macros::config_runner;

use cross_vm_integration_tests::vault::{vault_config_setup, VaultHarness};

#[config_runner(
    config = "vault.cross-vm.toml",
    harness = VaultHarness,
    setup = vault_config_setup,
    profile = "smoke"
)]
async fn vault_smoke_config() {}
