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

/// The `boom.cross-vm.toml` fixture: a tiny, deterministically-failing harness (`src/boom.rs`)
/// registered alongside `vault`, used only by the replay-artifact/shrink/`replay`-subcommand
/// tests below (the vault harness has no reachable bug to exercise them with).
fn boom_config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("boom.cross-vm.toml")
}

/// A fresh, gitignored dir under `<CARGO_MANIFEST_DIR>/tests_result/`, unique per test
/// invocation, so replay artifacts written by the `cross-vm` bin land in a stable inspectable
/// location (never a source-tree `target/` dir) and parallel `cargo test` runs of this file never
/// collide on the same `--artifacts-dir`.
fn temp_artifacts_dir(label: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests_result")
        .join(format!(
            "cross-vm-cli-e2e-artifacts-{}-{}-{label}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&dir).expect("create tests_result artifacts dir");
    dir
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
fn run_with_json_report_writes_a_schema_version_one_envelope() {
    // A dedicated temp path (process id + a nanosecond timestamp) so parallel `cargo test`
    // runs of this file never collide on the same file.
    let path = std::env::temp_dir().join(format!(
        "cross-vm-cli-e2e-json-report-{}-{}.json",
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
    assert_eq!(
        value["invocation"]["profiles"],
        serde_json::json!(["smoke"])
    );
    let profiles = value["profiles"].as_array().expect("profiles array");
    assert_eq!(
        profiles.len(),
        1,
        "one profile ran; one entry in the envelope"
    );
    assert_eq!(profiles[0]["profile"], "smoke");
    assert_eq!(profiles[0]["harness"], "vault");
    assert_eq!(profiles[0]["mode"], "fuzz");
    // `--seed 42` is the *base* seed the run is driven with; a fuzz report's own `seed` field is
    // the sub-seed of the last case (see `ErasedReport::seed`'s docs), not the base seed itself,
    // so this deliberately does not assert an exact seed value (a per-run derived, not fixed,
    // number) — only that the field is present as a number.
    assert!(profiles[0]["seed"].is_number());
    assert!(profiles[0]["steps"].as_u64().unwrap() > 0);

    std::fs::remove_file(&path).ok();
}

// -------------------------------------------------------------------------------------------
// Replay artifacts + shrink + `replay` subcommand (spec `docs/config-runs-spec.md` section 10),
// over the deterministically-failing `boom` harness (`src/boom.rs`).
// -------------------------------------------------------------------------------------------

#[test]
fn a_failing_fuzz_profile_writes_a_shrunk_replay_artifact() {
    let dir = temp_artifacts_dir("shrink-and-write");
    let out = cross_vm(&[
        "run",
        boom_config_path().to_str().unwrap(),
        "--profile",
        "fails",
        "--artifacts-dir",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(
        exit_code(&out),
        1,
        "the boom harness must fail (Bug): stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("artifacts dir exists")
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "exactly one artifact for one failing profile"
    );
    let artifact_path = entries[0].path();
    assert!(
        artifact_path.to_string_lossy().ends_with(".replay.toml"),
        "{artifact_path:?}"
    );

    let text = std::fs::read_to_string(&artifact_path).expect("read artifact");
    // `boom.cross-vm.toml`'s `fails` profile sets `shrink = true` and mixes noop/boom over 20
    // ops; boom fails the exact same way regardless of any noops before it, so the artifact's
    // history must be minimized down to the one op that actually matters.
    assert!(text.contains("shrunk = true"), "{text}");
    assert_eq!(
        text.matches("[[profile.replay.steps]]").count(),
        1,
        "shrink must minimize the history to a single step: {text}"
    );
    // The op is externally tagged by its lowercase kind name; an empty-field op serializes as a
    // sub-table keyed by the kind (`[profile.replay.steps.op.boom]`).
    assert!(text.contains("op.boom"), "{text}");

    // The artifact must be a valid config on its own: `cross-vm validate` never touches a chain.
    let validate_out = cross_vm(&["validate", artifact_path.to_str().unwrap()]);
    assert_eq!(
        exit_code(&validate_out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&validate_out.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replay_subcommand_reproduces_the_recorded_failure() {
    let dir = temp_artifacts_dir("replay-e2e");
    let first = cross_vm(&[
        "run",
        boom_config_path().to_str().unwrap(),
        "--profile",
        "fails",
        "--artifacts-dir",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(exit_code(&first), 1);

    let artifact_path = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .expect("one artifact written")
        .unwrap()
        .path();

    // `cross-vm replay <artifact>` is sugar for `run <artifact> --profile replay`: the recorded
    // Boom must still reproduce (exit code 1), since nothing about the (nonexistent) bug was
    // fixed between the original run and the replay.
    let replay_out = cross_vm(&["replay", artifact_path.to_str().unwrap()]);
    assert_eq!(
        exit_code(&replay_out),
        1,
        "stderr: {}",
        String::from_utf8_lossy(&replay_out.stderr)
    );
    // `tracing_subscriber::fmt()`'s default writer is stdout, not stderr.
    assert!(
        String::from_utf8_lossy(&replay_out.stdout).contains("boom: deterministic failure"),
        "stdout: {}",
        String::from_utf8_lossy(&replay_out.stdout)
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_passing_profile_writes_no_replay_artifact() {
    // The vault `smoke` profile deterministically passes on mocks (see
    // `run_smoke_profile_passes_on_mocks` above); reused here to exercise the "no artifact on a
    // pass" contract with the same `--artifacts-dir` wiring the failing tests above use.
    let dir = temp_artifacts_dir("no-artifact-on-pass");
    let out = cross_vm(&[
        "run",
        config_path().to_str().unwrap(),
        "--profile",
        "smoke",
        "--artifacts-dir",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(
        exit_code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(),
        "a passing run must write no replay artifact"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// -------------------------------------------------------------------------------------------
// Pipeline suites (dependency gated phases with world handoff): the `progressive` suite in
// `vault.cross-vm.toml` (`deposit-soak` -> `mixed-after-deposits`, `world = "inherit"`), plus a
// loader rejection over a temp config.
// -------------------------------------------------------------------------------------------

#[test]
fn progressive_suite_runs_pipeline_with_handoff() {
    let out = cross_vm(&[
        "run",
        config_path().to_str().unwrap(),
        "--suite",
        "progressive",
    ]);
    assert_eq!(
        exit_code(&out),
        0,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Both phases must actually have run (not skipped by gating): the combined log names each
    // phase profile by name, so this also exercises the world handoff (the second phase only
    // passes if it started from the state the first phase built, per `world = "inherit"`).
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("deposit-soak"),
        "combined output: {combined}"
    );
    assert!(
        combined.contains("mixed-after-deposits"),
        "combined output: {combined}"
    );
}

#[test]
fn staged_suite_mixes_modes_across_phases() {
    // The `staged` suite mixes modes across phases: a scenario seeds liquidity, an invariant
    // phase runs a long random mix against that state, and a single case fuzz phase digs
    // further, each inheriting the previous phase's world (`world = "inherit"`).
    let out = cross_vm(&["run", config_path().to_str().unwrap(), "--suite", "staged"]);
    assert_eq!(
        exit_code(&out),
        0,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // All three phases must have run (not skipped by gating): the combined log names each phase
    // profile, so every inheriting phase started from the state the previous phase built.
    for phase in ["seed-liquidity", "random-mix", "deep-case"] {
        assert!(
            combined.contains(phase),
            "phase {phase} missing from combined output: {combined}"
        );
    }

    // Each phase reports its own runner mode label: the scenario phase logs `mode=scenario`
    // (erase_report's scenario arm sets `ErasedReport.mode = "scenario"`, logged verbatim by
    // `log_profile_result`), the invariant phase `mode=invariant`, and the single case fuzz
    // phase `mode=fuzz`. Their presence proves the three distinct modes ran in one pipeline.
    for mode in ["scenario", "invariant", "fuzz"] {
        assert!(
            combined.contains(mode),
            "mode label {mode} missing from combined output: {combined}"
        );
    }
}

/// A dedicated temp config path (process id + a nanosecond timestamp), unique per test
/// invocation, so parallel `cargo test` runs of this file never collide on the same file.
fn temp_config_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "cross-vm-cli-e2e-config-{}-{}-{label}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn validate_rejects_inherit_on_multi_case_fuzz() {
    // The `boom` harness (registered alongside `vault`, see `boom_config_path`'s docs) is reused
    // here purely for its minimal, chain-free profile shape; `validate` never touches a chain, so
    // no setup fn ever runs. `donor` is a multi-case fuzz (`cases = 8`), which fans out into many
    // independent worlds, so it cannot donate a single inherited world (spec: `world = "inherit"`
    // requires both ends to be single-setup: `invariant`, `endurance`, `scenario`, or `fuzz` with
    // `cases == 1`).
    let config = r#"
[harness]
name = "boom"

[defaults]
seed = 1
check_every = 1

[profile.donor]
mode = "fuzz"
cases = 8
ops = 5
kinds = ["noop"]

[profile.inheritor]
mode = "fuzz"
cases = 1
ops = 5
kinds = ["noop"]

[suite.bad]

  [[suite.bad.phases]]
  profile = "donor"

  [[suite.bad.phases]]
  profile = "inheritor"
  needs = ["donor"]
  world = "inherit"
"#;
    let path = temp_config_path("inherit-multi-case-fuzz");
    std::fs::write(&path, config).expect("write temp config");

    let out = cross_vm(&["validate", path.to_str().unwrap()]);
    assert_eq!(
        exit_code(&out),
        3,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("not single-setup"),
        "combined output must name the violated rule: {combined}"
    );

    std::fs::remove_file(&path).ok();
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
