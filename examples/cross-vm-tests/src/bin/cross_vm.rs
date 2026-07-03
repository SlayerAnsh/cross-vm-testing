//! The `cross-vm` binary: registers the vault harness with the framework's config-driven CLI
//! (spec `docs/config-runs-spec.md` section 8) and drives it against a `*.cross-vm.toml` config,
//! e.g. `examples/cross-vm-tests/vault.cross-vm.toml`.
//!
//! ```sh
//! cargo run -p cross-vm-tests --bin cross-vm -- validate vault.cross-vm.toml
//! cargo run -p cross-vm-tests --bin cross-vm -- run vault.cross-vm.toml --profile smoke
//! ```
//!
//! `current_thread` is required: the erased registry layer, and every mock VM, are `!Send` by
//! design (see `cross_vm_framework::cli::Cli::main`'s docs).

use cross_vm_tests::boom::{boom_setup, BoomHarness};
use cross_vm_tests::vault::{vault_config_setup, VaultHarness};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    cross_vm_framework::cli::Cli::new()
        .env_file(".env")
        .register("vault", || VaultHarness, vault_config_setup)
        // A tiny, deterministically-failing harness registered only for `tests/cli_e2e.rs`'s
        // replay-artifact/shrink/`replay`-subcommand coverage (see `boom`'s module docs).
        .register("boom", || BoomHarness, boom_setup)
        .main()
        .await
}
