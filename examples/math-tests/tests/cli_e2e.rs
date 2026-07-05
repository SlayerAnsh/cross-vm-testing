//! CLI style: drives the `math-cli` binary as a real subprocess, so argument parsing, exit-code
//! mapping, and the JSON report envelope are checked exactly as a user sees them.

use std::process::{Command, Output};

/// Runs the `math-cli` bin built by this same `cargo test` invocation, from the crate root.
fn math_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_math-cli"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn math-cli bin")
}

/// Extracts the process exit code, failing loudly if it exited via a signal.
fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("process exited via a signal")
}

/// A `run` of the `smoke` profile exits 0 and writes a `schema_version = 1` JSON report envelope.
#[test]
fn run_smoke_writes_a_schema_version_one_report() {
    let path = std::env::temp_dir().join(format!(
        "math-tests-cli-e2e-json-report-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let out = math_cli(&[
        "run",
        "math.harness.toml",
        "--profile",
        "smoke",
        "--json-report",
        path.to_str().unwrap(),
    ]);
    assert_eq!(
        exit_code(&out),
        0,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let raw = std::fs::read_to_string(&path).expect("json report was written");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(value["schema_version"], 1);

    std::fs::remove_file(&path).ok();
}

/// `list` reports the registered harness and exits 0.
#[test]
fn list_exits_zero() {
    let out = math_cli(&["list", "math.harness.toml"]);
    assert_eq!(
        exit_code(&out),
        0,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An unknown profile is a usage error: exit code 3.
#[test]
fn run_unknown_profile_exits_three() {
    let out = math_cli(&["run", "math.harness.toml", "--profile", "does-not-exist"]);
    assert_eq!(
        exit_code(&out),
        3,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
