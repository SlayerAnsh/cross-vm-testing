//! CLI style (style c): drives the `cross-vm` binary as a real subprocess, so argument parsing,
//! exit-code mapping, and seed reproducibility are checked exactly as a user sees them.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The `counter.cross-vm.toml` fixture, relative to this crate's root.
fn config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("counter.cross-vm.toml")
}

/// Runs the `cross-vm` bin built by this same `cargo test` invocation.
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
fn validate_passes_on_the_counter_config() {
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
fn list_reports_the_counter_harness() {
    let out = cross_vm(&["list", config_path().to_str().unwrap()]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", String::from_utf8_lossy(&out.stderr));
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
fn run_steps_scenario_passes_on_mocks() {
    let out = cross_vm(&["run", config_path().to_str().unwrap(), "--profile", "steps"]);
    assert_eq!(
        exit_code(&out),
        0,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
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

    // Compare the deterministic markers a reproducible seed pins (timestamps differ per run).
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
fn run_with_json_report_writes_a_schema_version_one_envelope() {
    let path = std::env::temp_dir().join(format!(
        "solana-tests-cli-e2e-json-report-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let out = cross_vm(&[
        "run",
        config_path().to_str().unwrap(),
        "--profile",
        "smoke",
        "--seed",
        "42",
        "--json-report",
        path.to_str().unwrap(),
    ]);
    assert_eq!(
        exit_code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let raw = std::fs::read_to_string(&path).expect("json report was written");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(value["schema_version"], 1);
    let profiles = value["profiles"].as_array().expect("profiles array");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["profile"], "smoke");
    assert_eq!(profiles[0]["harness"], "counter");
    assert_eq!(profiles[0]["mode"], "fuzz");
    assert!(profiles[0]["steps"].as_u64().unwrap() > 0);

    std::fs::remove_file(&path).ok();
}
