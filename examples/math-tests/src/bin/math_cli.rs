//! The raw generic CLI: `harness run math.harness.toml --profile smoke`.
//!
//! Registers the single `math` harness against [`harness_cli::GenericDomain`] (no domain layer,
//! no extra flags) and hands off to the shared CLI. This is the whole binary; everything else is
//! config.

use math_tests::{math_config_setup, MathHarness};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    harness_cli::Cli::<harness_cli::GenericDomain>::new()
        .register("math", MathHarness::default, math_config_setup)
        .main()
        .await
}
