//! End-to-end tests for the `cross-vm` binary (spec `docs/config-runs-spec.md` section 8),
//! exercised as a real subprocess via `Command` so the CLI's argument parsing, exit-code mapping,
//! and seed reproducibility are checked exactly as a user would see them, not just as library
//! calls.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The `vault.cross-vm.toml` fixture, relative to `CARGO_MANIFEST_DIR` (this crate's root, not
/// the workspace root, so the test is independent of where `cargo test` is invoked from).
fn config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vault.cross-vm.toml")
}

/// The `vault.no-chains.cross-vm.toml` fixture: no `[[chain]]` declarations, exercising
/// `vault_config_setup`'s hard coded fallback path.
fn no_chains_config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vault.no-chains.cross-vm.toml")
}

/// Runs the `cross-vm` bin built by this same `cargo test` invocation (`CARGO_BIN_EXE_cross-vm`,
/// set by Cargo for every integration test in a crate that has the `[[bin]]`) with `args`.
fn cross_vm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cross-vm"))
        .args(args)
        .output()
        .expect("spawn cross-vm bin")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("process exited via a signal")
}

#[test]
fn validate_passes_on_the_vault_config() {
    let out = cross_vm(&["validate", config_path().to_str().unwrap()]);
    assert_eq!(
        exit_code(&out),
        0,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_smoke_profile_passes_on_mocks() {
    let out = cross_vm(&["run", config_path().to_str().unwrap(), "--profile", "smoke"]);
    assert_eq!(
        exit_code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_smoke_profile_with_a_fixed_seed_is_reproducible() {
    let config = config_path();
    let args = [
        "run",
        config.to_str().unwrap(),
        "--profile",
        "smoke",
        "--seed",
        "123",
    ];
    let first = cross_vm(&args);
    let second = cross_vm(&args);
    assert_eq!(exit_code(&first), 0, "first run");
    assert_eq!(exit_code(&second), 0, "second run");

    // Timestamps differ between the two runs, so compare the deterministic markers a
    // reproducible seed pins: every "fuzz case starting"/"run passed" line's `seed=`/`steps=`
    // pair, in order, plus the final summary line.
    let markers = |out: &Output| -> Vec<String> {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .filter(|tok| {
                        tok.starts_with("seed=")
                            || tok.starts_with("steps=")
                            || tok.starts_with("case=")
                            || tok.starts_with("cases=")
                            || tok.starts_with("skipped=")
                            || tok.starts_with("exit_code=")
                            || tok.starts_with("mode=")
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    assert_eq!(
        markers(&first),
        markers(&second),
        "same seed must reproduce the exact same case/seed/steps sequence"
    );
}

#[test]
fn run_deposit_then_overdraw_scenario_passes_on_mocks() {
    let out = cross_vm(&[
        "run",
        config_path().to_str().unwrap(),
        "--profile",
        "deposit-then-overdraw",
    ]);
    assert_eq!(
        exit_code(&out),
        0,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_with_target_chain_rpc_and_no_rpc_url_is_a_usage_error() {
    // `eth` declares no `rpc_url` in the fixture (every profile there defaults to mock), so
    // forcing it to `rpc` via `--target-chain` must hit the framework's "rpc target requires
    // rpc_url" validation cleanly (never an actual network call) and report the CLI's usage/config
    // exit code (spec section 8: exit code 3).
    let out = cross_vm(&[
        "run",
        config_path().to_str().unwrap(),
        "--profile",
        "smoke",
        "--target-chain",
        "eth=rpc",
    ]);
    assert_eq!(
        exit_code(&out),
        3,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_passes_on_a_config_with_no_chain_declarations() {
    // `vault.no-chains.cross-vm.toml` has no `[[chain]]` entries, so `SetupRequest::chain_specs`
    // is empty and `vault_config_setup` falls back to hard coding the three mock chains, exactly
    // like `vault_setup` (spec section 4.2's backward-compatible path).
    let out = cross_vm(&[
        "run",
        no_chains_config_path().to_str().unwrap(),
        "--profile",
        "smoke",
    ]);
    assert_eq!(
        exit_code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
