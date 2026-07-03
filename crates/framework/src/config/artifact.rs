//! [`write_replay_artifact`]: the replay-artifact writer (spec `docs/config-runs-spec.md` §10).
//!
//! On any failed generative run (fuzz/invariant/endurance), the CLI writes a self-contained
//! `<harness>-<profile>-<seed>-<timestamp>.replay.toml`: a **valid config file** with one
//! `[profile.replay]` scenario profile holding the (possibly shrunk) failing history, so closing
//! the failure→replay loop needs zero new machinery — `cross-vm replay <artifact>` is just
//! `load` + `run` over the same loader and registry every other config file goes through.
//!
//! **Never writes secrets.** Only the already-resolved `rpc_url` string lands in the artifact (a
//! reproduction tool, not a secret store, per spec §10); mnemonics and keys are never part of
//! [`ChainSpecData`] in the first place, so there is nothing to accidentally leak here.
//!
//! **The u128/TOML integer-range problem.** TOML integers are a signed 64-bit type; a vault-style
//! op's `u128` amount can exceed `i64::MAX` while still fitting comfortably in the `serde_json`
//! history [`crate::config::ErasedFailure`] already carries (JSON numbers there came from
//! `serde_json::to_value`, which only requires the value fit `u64`). So `toml::to_string` can fail
//! on a step's `op` value even though the JSON form is perfectly valid; on any such error this
//! writer falls back to a sibling `*.replay.json` (`serde_json::to_string_pretty` over the exact
//! same [`Artifact`] structure), which `cross_vm_config::load`'s `.json`-by-extension dispatch
//! already reads (spec §10's closing claim: this is also the proof the schema is format
//! agnostic).

use std::path::{Path, PathBuf};

use crate::harness::FailureKind;

use super::build_chain::spec_id_to_str;
use super::erased::{ErasedFailure, ErasedReport};
use super::resolve::ResolvedProfile;
use super::setup_request::{ChainSpecData, Target};

/// Writes a self-contained replay artifact for `report` (a failed fuzz/invariant/endurance run)
/// to `dir/<harness>-<profile>-<seed>-<timestamp>.replay.toml`, creating `dir` if it does not
/// already exist. Returns the path actually written.
///
/// `source` supplies `[harness]` (name + named setup); `resolved` supplies the resolved chain
/// specs (each with its target/rpc_url/per-kind fields already resolved — spec §10 permits
/// writing the resolved `rpc_url`, never a secret) and the profile's own default target/selection.
/// `report` supplies the failure itself: `report.failure` **must** be `Some` (an artifact for a
/// passing run makes no sense; callers only call this after checking `report.failure.is_some()`).
///
/// Serialization first tries `toml::to_string_pretty`; on **any** error (an out-of-range integer,
/// a non-string map key, ...) this falls back to a sibling `<...>.replay.json` written via
/// `serde_json::to_string_pretty` over the identical (private) `Artifact` structure (see the
/// module docs' "u128" section). Both branches return the path actually written, never both.
///
/// # Errors
/// Returns the underlying [`std::io::Error`] if creating `dir` or writing either file fails.
pub fn write_replay_artifact(
    dir: &Path,
    source: &cross_vm_config::RunConfig,
    resolved: &ResolvedProfile,
    report: &ErasedReport,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;

    let artifact = build_artifact(source, resolved, report);
    let stem = format!(
        "{}-{}-{}-{}",
        report.harness,
        report.profile,
        report.seed,
        unix_timestamp()
    );

    match toml::to_string_pretty(&artifact) {
        Ok(text) => {
            let path = dir.join(format!("{stem}.replay.toml"));
            std::fs::write(&path, text)?;
            Ok(path)
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                "replay artifact could not be represented as TOML (likely a u128 amount out of \
                 i64 range); falling back to a .replay.json sidecar"
            );
            let json = serde_json::to_string_pretty(&artifact).map_err(std::io::Error::other)?;
            let path = dir.join(format!("{stem}.replay.json"));
            std::fs::write(&path, json)?;
            Ok(path)
        }
    }
}

/// Unix seconds since the epoch, for the artifact filename's timestamp component. Plain runtime
/// arithmetic; `unwrap_or_default` only matters on a system clock set before 1970, in which case
/// `0` is as good a timestamp as any (this is a filename disambiguator, not a source of truth).
fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `"mock"` / `"rpc"`, the artifact's own target-string spelling (mirrors `cli::target_label`,
/// duplicated here rather than shared since both are three-line private helpers and `cli`/
/// `artifact` otherwise have no reason to depend on each other).
fn target_str(t: Target) -> &'static str {
    match t {
        Target::Mock => "mock",
        Target::Rpc => "rpc",
    }
}

/// Builds the in-memory [`Artifact`] from `source`/`resolved`/`report`; the serialize step
/// (`toml` first, `serde_json` fallback) happens in [`write_replay_artifact`], not here, so both
/// branches serialize the exact same value.
fn build_artifact(
    source: &cross_vm_config::RunConfig,
    resolved: &ResolvedProfile,
    report: &ErasedReport,
) -> Artifact {
    let chains: Vec<ArtifactChain> = resolved.chain_specs.iter().map(artifact_chain).collect();
    let chain_labels: Vec<String> = resolved
        .chain_specs
        .iter()
        .map(|c| c.label.clone())
        .collect();

    let failure = report.failure.as_ref();
    let steps: Vec<ArtifactStep> = failure
        .and_then(|f| f.history.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|op| ArtifactStep { op })
        .collect();

    Artifact {
        harness: ArtifactHarness {
            name: source.harness.name.clone(),
            setup: source.harness.setup.clone(),
        },
        chains,
        env: ArtifactEnv {
            target: target_str(resolved.target).to_string(),
            chains: chain_labels,
        },
        replay: ArtifactReplayMeta {
            source_profile: report.profile.clone(),
            source_mode: report.mode.clone(),
            // The fuzz case index that failed is not threaded through `ErasedReport` today (only
            // its sub-seed is, via `report.seed`); omitted per spec §10 ("the fuzz case index if
            // applicable, else omit") rather than guessed at.
            case: None,
            failure: failure_summary(failure),
            shrunk: failure.map(|f| f.shrunk).unwrap_or(false),
            framework_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        profile: ArtifactProfileWrapper {
            replay: ArtifactReplayProfile {
                mode: "scenario",
                seed: report.seed,
                steps,
            },
        },
    }
}

/// One resolved [`ChainSpecData`] rendered into the artifact's `[[chain]]` shape: owned strings,
/// string-spelled enums (`kind`/`target`/`spec_id`/`commitment`), the resolved `rpc_url` (never a
/// secret — see the module docs). Per-kind `Option` fields serialize as absent (not `null`,
/// TOML has none) when `None`, via `skip_serializing_if`.
fn artifact_chain(spec: &ChainSpecData) -> ArtifactChain {
    ArtifactChain {
        label: spec.label.clone(),
        kind: spec.kind.to_string(),
        chain_id: spec.chain_id.clone(),
        name: spec.name.clone(),
        native_symbol: spec.native_symbol.clone(),
        rpc_url: spec.rpc_url.clone(),
        target: target_str(spec.target).to_string(),
        bech32_prefix: spec.bech32_prefix.clone(),
        native_denom: spec.native_denom.clone(),
        gas_price: spec.gas_price,
        spec_id: spec.spec_id.map(|id| spec_id_to_str(id).to_string()),
        ws_url: spec.ws_url.clone(),
        commitment: spec.commitment.map(|c| c.to_string()),
        params: spec.params.clone(),
    }
}

/// A short, human-readable summary of why the run failed, for `[replay].failure` (provenance
/// only; never parsed back). Mirrors spec §10's examples (`"invariant NoBadDebt"`, `"bug: ..."`).
fn failure_summary(failure: Option<&ErasedFailure>) -> String {
    match failure.map(|f| &f.kind) {
        Some(FailureKind::Bug(msg)) => format!("bug: {msg}"),
        Some(FailureKind::Invariant { name, .. }) => format!("invariant {name}"),
        Some(FailureKind::Infra(msg)) => format!("infra: {msg}"),
        None => "unknown".to_string(),
    }
}

// -------------------------------------------------------------------------------------------
// The artifact's own serde-`Serialize` shape (spec §10). Field order within each struct matters
// for TOML output: scalar/inline-array fields must precede nested-table fields in the same
// struct, since a TOML table's plain `key = value` lines cannot follow a `[table]`/`[[table]]`
// header for that same table. Every struct below already satisfies this (scalars first, any
// table-shaped field last), and the `toml` crate additionally reorders map/struct entries so
// this holds even if a future edit gets the declaration order wrong.
// -------------------------------------------------------------------------------------------

/// The whole artifact document: a valid `RunConfig` (`[harness]`, `[[chain]]`, `[env]`) plus
/// `[replay]` provenance (tolerated, ignored by the run schema) and one concrete
/// `[profile.replay]` scenario profile holding the (possibly shrunk) failing history.
#[derive(Debug, serde::Serialize)]
struct Artifact {
    harness: ArtifactHarness,
    #[serde(rename = "chain")]
    chains: Vec<ArtifactChain>,
    env: ArtifactEnv,
    replay: ArtifactReplayMeta,
    profile: ArtifactProfileWrapper,
}

/// `[harness]`.
#[derive(Debug, serde::Serialize)]
struct ArtifactHarness {
    name: String,
    setup: String,
}

/// One `[[chain]]` entry.
#[derive(Debug, serde::Serialize)]
struct ArtifactChain {
    label: String,
    kind: String,
    chain_id: String,
    name: String,
    native_symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc_url: Option<String>,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    bech32_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_denom: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gas_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spec_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ws_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commitment: Option<String>,
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    params: toml::Table,
}

/// `[env]`: the resolved default target plus the exact selected chain labels (spec §10:
/// `env.targets` is deliberately omitted — every chain's resolved target is already baked into
/// its own `[[chain]].target` above, so there is nothing left for a label-keyed override map to
/// say).
#[derive(Debug, serde::Serialize)]
struct ArtifactEnv {
    target: String,
    chains: Vec<String>,
}

/// `[replay]`: provenance only, tolerated but ignored by the run schema (`cross_vm_config`'s
/// loader parses and drops it).
#[derive(Debug, serde::Serialize)]
struct ArtifactReplayMeta {
    source_profile: String,
    source_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    case: Option<usize>,
    failure: String,
    shrunk: bool,
    framework_version: String,
}

/// The `[profile]` table wrapper, so the field ends up spelled `[profile.replay]` rather than a
/// top-level `[replay_profile]`.
#[derive(Debug, serde::Serialize)]
struct ArtifactProfileWrapper {
    replay: ArtifactReplayProfile,
}

/// `[profile.replay]`: `mode = "scenario"`, the concrete seed the failing run used, and the
/// (possibly shrunk) op history as ordered steps.
#[derive(Debug, serde::Serialize)]
struct ArtifactReplayProfile {
    mode: &'static str,
    seed: u64,
    steps: Vec<ArtifactStep>,
}

/// One `[[profile.replay.steps]]` entry: just `op` (see spec §7.1 — an externally tagged
/// `H::Operation` value). `expect`/`delay`/`check` are left at `ScenarioStepRaw`'s own defaults
/// (`Accepted`/zero/`true`): a replay just re-runs the recorded sequence and checks invariants.
#[derive(Debug, serde::Serialize)]
struct ArtifactStep {
    op: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ErasedFailure, ErasedReport};
    use crate::harness::{Coverage, FailureKind};
    use std::time::Duration;

    fn base_source() -> cross_vm_config::RunConfig {
        cross_vm_config::from_toml_str(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
            &|_| None,
        )
        .expect("valid fixture")
    }

    fn base_resolved(source: &cross_vm_config::RunConfig) -> ResolvedProfile {
        crate::config::resolve_profile(source, "smoke", &crate::config::RunOptions::default())
            .expect("resolves")
    }

    fn failing_report(history: serde_json::Value, seed: u64) -> ErasedReport {
        ErasedReport {
            harness: "vault".to_string(),
            profile: "deep".to_string(),
            mode: "fuzz".to_string(),
            seed,
            steps: 3,
            skipped: 0,
            coverage: Coverage::default(),
            stats: None,
            elapsed: Duration::from_millis(5),
            failure: Some(ErasedFailure {
                step: 2,
                kind: FailureKind::Invariant {
                    name: "NoBadDebt".to_string(),
                    detail: "debt exceeds max".to_string(),
                },
                op_debug: Some("Borrow { .. }".to_string()),
                history,
                shrunk: false,
            }),
        }
    }

    fn tempdir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cross-vm-artifact-test-{}-{}-{label}",
            std::process::id(),
            unix_timestamp()
        ))
    }

    #[test]
    fn writes_a_toml_artifact_that_reloads_through_the_same_loader() {
        let source = base_source();
        let resolved = base_resolved(&source);
        let history = serde_json::json!([
            {"Deposit": {"chain": "eth", "user": 0, "amount": 1000}},
        ]);
        let report = failing_report(history, 42);

        let dir = tempdir("toml-roundtrip");
        let path = write_replay_artifact(&dir, &source, &resolved, &report).expect("write");
        assert!(
            path.to_string_lossy().ends_with(".replay.toml"),
            "expected a .replay.toml path, got {path:?}"
        );
        assert!(
            path.file_name().unwrap().to_string_lossy().starts_with("vault-deep-42-"),
            "filename must be <harness>-<profile>-<seed>-<timestamp>.replay.toml, got {path:?}"
        );

        // The artifact must load through the exact same loader every other config file does.
        let reloaded = cross_vm_config::load(&path, &|_| None).expect("artifact reloads");
        assert_eq!(reloaded.harness.name, "vault");
        assert!(reloaded.profiles.contains_key("replay"));
        let cross_vm_config::Profile::Scenario(p) = &reloaded.profiles["replay"] else {
            panic!("expected the replay profile to be a scenario profile");
        };
        assert_eq!(p.steps.len(), 1);
        // [[chain]] fidelity: the one declared chain round-trips with kind/chain_id intact.
        assert_eq!(reloaded.chains.len(), 1);
        assert_eq!(reloaded.chains[0].label, "eth");
        assert_eq!(reloaded.chains[0].kind, "evm");

        // Never a secret: no mnemonic/key-shaped field exists on ChainDecl at all, so nothing to
        // assert its absence against beyond confirming the file only holds what ChainSpecData
        // carries (rpc_url, if any, resolved), already covered by the reload above succeeding
        // against `deny_unknown_fields`.

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn u128_amount_over_i64_max_falls_back_to_a_json_sidecar() {
        let source = base_source();
        let resolved = base_resolved(&source);
        // Fits u64 (so serde_json::to_value succeeds, as a real ErasedFailure.history always
        // does) but is out of TOML's signed-64-bit integer range, so toml::to_string_pretty must
        // fail on this artifact and the writer must fall back to a .replay.json sidecar.
        let huge: u64 = (i64::MAX as u64) + 1;
        let history = serde_json::json!([
            {"Deposit": {"chain": "eth", "user": 0, "amount": huge}},
        ]);
        let report = failing_report(history, 7);

        let dir = tempdir("json-sidecar");
        let path = write_replay_artifact(&dir, &source, &resolved, &report).expect("write");
        assert!(
            path.to_string_lossy().ends_with(".replay.json"),
            "expected a .replay.json sidecar for an out-of-i64-range integer, got {path:?}"
        );
        assert!(
            toml::to_string_pretty(&build_artifact(&source, &resolved, &report)).is_err(),
            "sanity: this artifact must genuinely fail TOML serialization (the fallback trigger)"
        );

        // The sidecar's own bytes (what write_replay_artifact itself produced) must hold the
        // u64-range amount exactly, with no precision loss: parsed as raw JSON.
        let raw = std::fs::read_to_string(&path).expect("sidecar file exists");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        let amount = &value["profile"]["replay"]["steps"][0]["op"]["Deposit"]["amount"];
        assert_eq!(amount.as_u64(), Some(huge), "raw JSON bytes: {raw}");

        // It must load AND round-trip the out-of-i64-range amount losslessly (spec §10): the
        // loader now processes JSON input natively as `serde_json::Value`, so the reloaded op
        // still carries the EXACT u64 integer — not a float, not a rounded value.
        let reloaded = cross_vm_config::load(&path, &|_| None).expect("sidecar reloads");
        assert!(reloaded.profiles.contains_key("replay"));
        let cross_vm_config::Profile::Scenario(p) = &reloaded.profiles["replay"] else {
            panic!("expected the replay profile to be a scenario profile");
        };
        let reloaded_op = &p.steps[0].op;
        let reloaded_amount = &reloaded_op["Deposit"]["amount"];
        assert!(
            reloaded_amount.is_u64(),
            "reloaded op amount must survive as an integer, got {reloaded_amount:?}"
        );
        assert_eq!(
            reloaded_amount.as_u64(),
            Some(huge),
            "reloaded op must carry the exact u64 amount (no float downgrade), got {reloaded_amount:?}"
        );

        // And it must typed-deserialize into a harness op whose `amount` is a `u64` field — the
        // exact step that used to fail with `invalid type: floating point, expected u64` and
        // exit `cross-vm replay` with a config error (3) instead of reproducing the failure.
        // Deserializing straight from the reloaded op mirrors what the registry's scenario site
        // (`H::Operation::deserialize(raw.op.clone())`) does at replay time.
        #[derive(serde::Deserialize)]
        enum SidecarOp {
            Deposit {
                #[allow(dead_code)]
                chain: String,
                #[allow(dead_code)]
                user: u64,
                amount: u64,
            },
        }
        let op = <SidecarOp as serde::Deserialize>::deserialize(reloaded_op.clone())
            .expect("reloaded op deserializes into a u64-amount harness op (no config error)");
        let SidecarOp::Deposit { amount, .. } = op;
        assert_eq!(amount, huge, "typed u64 op amount must equal the original");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn passing_run_history_is_empty_and_failure_summary_is_unknown() {
        let source = base_source();
        let resolved = base_resolved(&source);
        let report = ErasedReport {
            harness: "vault".to_string(),
            profile: "smoke".to_string(),
            mode: "fuzz".to_string(),
            seed: 1,
            steps: 1,
            skipped: 0,
            coverage: Coverage::default(),
            stats: None,
            elapsed: Duration::ZERO,
            failure: None,
        };
        let artifact = build_artifact(&source, &resolved, &report);
        assert_eq!(artifact.replay.failure, "unknown");
        assert!(!artifact.replay.shrunk);
        assert!(artifact.profile.replay.steps.is_empty());
    }

    #[test]
    fn bug_and_invariant_failure_summaries_match_spec_examples() {
        assert_eq!(
            failure_summary(Some(&ErasedFailure {
                step: 1,
                kind: FailureKind::Bug("over-withdraw accepted".to_string()),
                op_debug: None,
                history: serde_json::Value::Array(vec![]),
                shrunk: false,
            })),
            "bug: over-withdraw accepted"
        );
        assert_eq!(
            failure_summary(Some(&ErasedFailure {
                step: 1,
                kind: FailureKind::Invariant {
                    name: "NoBadDebt".to_string(),
                    detail: "detail dropped from the summary".to_string(),
                },
                op_debug: None,
                history: serde_json::Value::Array(vec![]),
                shrunk: false,
            })),
            "invariant NoBadDebt"
        );
    }

    #[test]
    fn no_declared_chains_never_panics_the_build() {
        // A resolved profile with zero declared chains (backward-compatible setups): the
        // artifact must still build cleanly with an empty `[[chain]]` set.
        let source = cross_vm_config::from_toml_str(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
            &|_| None,
        )
        .expect("valid fixture");
        let resolved = base_resolved(&source);
        assert!(resolved.chain_specs.is_empty());
        let report = failing_report(serde_json::json!([]), 1);
        let artifact = build_artifact(&source, &resolved, &report);
        assert!(artifact.chains.is_empty());
        assert!(artifact.env.chains.is_empty());
    }
}
