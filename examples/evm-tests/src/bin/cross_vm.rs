//! The `cross-vm` binary for the EVM example crate: registers the counter harness with the
//! framework's config-driven CLI and drives it against `counter.cross-vm.toml`.
//!
//! ```sh
//! cargo run -p evm-tests -- validate counter.cross-vm.toml
//! cargo run -p evm-tests -- run counter.cross-vm.toml --profile smoke
//! ```
//!
//! `current_thread` is required: the erased registry layer, and every mock VM, are `!Send` by
//! design (see `cross_vm_framework::cli::Cli::main`'s docs).

use evm_tests::counter::{counter_config_setup, CounterHarness};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    cross_vm_framework::cli::Cli::new()
        .env_file(".env")
        .register("counter", || CounterHarness, counter_config_setup)
        .main()
        .await
}
