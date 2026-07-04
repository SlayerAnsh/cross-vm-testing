//! [`write_replay_artifact`]: the replay-artifact writer (spec `docs/config-runs-spec.md` §10).
//!
//! On any failed generative run (fuzz/invariant/endurance), the CLI writes a self-contained
//! `<harness>-<profile>-<seed>-<timestamp>.replay.toml`: a **valid config file** with one
//! `[profile.replay]` scenario profile holding the (possibly shrunk) failing history, so closing
//! the failure→replay loop needs zero new machinery — replaying `<artifact>` is just `load` + `run`
//! over the same loader and registry every other config file goes through.
//!
//! **Domain sections.** The generic artifact carries `[harness]`, `[env]`, `[replay]`, and
//! `[profile.replay]`; a domain injects its own extra top-level sections (e.g. cross-vm's
//! `[[chain]]`) through the `domain_sections` parameter, merged in before serialization so the
//! artifact stays a loadable config for that domain's own extension type.
//!
//! **Never writes secrets.** Only already-resolved, non-secret values land in the artifact (a
//! reproduction tool, not a secret store, per spec §10); the resolved `[env]` embed and any
//! domain-injected sections carry resolved data only, so there is nothing to accidentally leak
//! here.
//!
//! **The u128/TOML integer-range problem.** TOML integers are a signed 64-bit type; a vault-style
//! op's `u128` amount can exceed `i64::MAX` while still fitting comfortably in the `serde_json`
//! history [`crate::erased::ErasedFailure`] already carries (JSON numbers there came from
//! `serde_json::to_value`, which only requires the value fit `u64`). So `toml::to_string` can fail
//! on a step's `op` value even though the JSON form is perfectly valid; on any such error this
//! writer falls back to a sibling `*.replay.json` (`serde_json::to_string_pretty` over the exact
//! same [`Artifact`] structure), which the loader's `.json`-by-extension dispatch already reads
//! (spec §10's closing claim: this is also the proof the schema is format agnostic).

use std::path::{Path, PathBuf};

use harness_core::FailureKind;

use crate::erased::{ErasedFailure, ErasedReport};
use crate::resolve::ResolvedProfile;

/// Writes a self-contained replay artifact for `report` (a failed fuzz/invariant/endurance run)
/// to `dir/<harness>-<profile>-<seed>-<timestamp>.replay.toml`, creating `dir` if it does not
/// already exist. Returns the path actually written.
///
/// `source` supplies `[harness]` (name + named setup); `resolved` supplies the merged `[env]`
/// table embedded verbatim so a replay resolves the same environment. `report` supplies the
/// failure itself: `report.failure` **must** be `Some` (an artifact for a passing run makes no
/// sense; callers only call this after checking `report.failure.is_some()`). `domain_sections`
/// are the domain's extra top-level tables (e.g. cross-vm's `[[chain]]`), merged in as top-level
/// keys before serialization.
///
/// Serialization first tries `toml::to_string_pretty`; on **any** error (an out-of-range integer,
/// a non-string map key, ...) this falls back to a sibling `<...>.replay.json` written via
/// `serde_json::to_string_pretty` over the identical (private) `Artifact` structure plus the same
/// merged domain sections (see the module docs' "u128" section). Both branches return the path
/// actually written, never both.
///
/// # Errors
/// Returns the underlying [`std::io::Error`] if creating `dir` or writing either file fails.
pub fn write_replay_artifact<X: harness_config::ConfigExt>(
    dir: &Path,
    source: &harness_config::RunConfig<X>,
    resolved: &ResolvedProfile,
    report: &ErasedReport,
    domain_sections: toml::Table,
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

    // Merge the domain's extra top-level sections (e.g. `[[chain]]`) into the
    // generic artifact before serializing, so the artifact stays a loadable
    // config for the domain's own `Ext`.
    let merged_toml: Result<toml::Table, toml::ser::Error> =
        toml::Table::try_from(&artifact).map(|mut table| {
            for (key, value) in domain_sections.clone() {
                table.insert(key, value);
            }
            table
        });

    match merged_toml.and_then(|t| toml::to_string_pretty(&t)) {
        Ok(text) => {
            let path = dir.join(format!("{stem}.replay.toml"));
            std::fs::write(&path, text)?;
            Ok(path)
        }
        Err(e) => {
            // JSON fallback (spec §10): a step's `u128` amount can exceed `i64::MAX` while still
            // fitting `u64`, so `toml` refuses it even though the JSON form is valid. Same value,
            // same merge, via `serde_json`.
            tracing::debug!(
                error = %e,
                "replay artifact could not be represented as TOML (likely a u128 amount out of \
                 i64 range); falling back to a .replay.json sidecar"
            );
            let mut json = serde_json::to_value(&artifact).map_err(std::io::Error::other)?;
            if let Some(obj) = json.as_object_mut() {
                for (key, value) in domain_sections {
                    let jv = serde_json::to_value(value).map_err(std::io::Error::other)?;
                    obj.insert(key, jv);
                }
            }
            let json = serde_json::to_string_pretty(&json).map_err(std::io::Error::other)?;
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

/// Builds the in-memory [`Artifact`] from `source`/`resolved`/`report`; the serialize step
/// (`toml` first, `serde_json` fallback) happens in [`write_replay_artifact`], not here, so both
/// branches serialize the exact same value.
fn build_artifact<X: harness_config::ConfigExt>(
    source: &harness_config::RunConfig<X>,
    resolved: &ResolvedProfile,
    report: &ErasedReport,
) -> Artifact {
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
        env: resolved.env.clone(),
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
            // Provenance only: an inherited phase's starting world came from an earlier phase, so
            // replaying this artifact standalone starts from a fresh setup instead. Absent (never
            // `null`, TOML has none) for a `fresh` phase.
            world_source: match resolved.world_source {
                harness_config::WorldSource::Inherit => Some("inherited"),
                harness_config::WorldSource::Fresh => None,
            },
            phase_params: resolved.phase_params.clone(),
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

/// The whole artifact document: a valid generic `RunConfig` (`[harness]`,
/// `[env]`) plus `[replay]` provenance (tolerated, ignored by the run schema)
/// and one concrete `[profile.replay]` scenario profile holding the (possibly
/// shrunk) failing history. Domain sections (e.g. cross-vm's `[[chain]]`) are
/// merged in as extra top-level tables before serialization.
#[derive(Debug, serde::Serialize)]
struct Artifact {
    harness: ArtifactHarness,
    /// The failing profile's fully merged env table, embedded so a replay
    /// resolves the same environment. Skipped when empty.
    #[serde(skip_serializing_if = "env_is_empty")]
    env: serde_json::Value,
    replay: ArtifactReplayMeta,
    profile: ArtifactProfileWrapper,
}

/// True when the env value is an empty object (nothing worth embedding).
fn env_is_empty(env: &serde_json::Value) -> bool {
    env.as_object().map(|m| m.is_empty()).unwrap_or(false)
}

/// `[harness]`.
#[derive(Debug, serde::Serialize)]
struct ArtifactHarness {
    name: String,
    setup: String,
}

/// `[replay]`: provenance only, tolerated but ignored by the run schema (the loader parses and
/// drops it).
#[derive(Debug, serde::Serialize)]
struct ArtifactReplayMeta {
    source_profile: String,
    source_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    case: Option<usize>,
    failure: String,
    shrunk: bool,
    framework_version: String,
    /// `"inherited"` when this artifact was written for a suite phase whose starting world was
    /// inherited from an earlier phase, else absent. Provenance only, never parsed back: it warns
    /// a reader that a standalone replay starts from a fresh setup, not the inherited starting
    /// state.
    #[serde(skip_serializing_if = "Option::is_none")]
    world_source: Option<&'static str>,
    /// The exact per-phase `params` table this run's world was patched with, when the phase set
    /// one, else absent. Provenance only (never parsed back): it records the precise world
    /// mutation a replayed failure ran under. Written last so its `[replay.phase_params]` sub-table
    /// follows the scalar `[replay]` keys (a TOML table's plain keys cannot follow a nested-table
    /// header).
    #[serde(skip_serializing_if = "Option::is_none")]
    phase_params: Option<toml::Table>,
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
    use crate::erased::{ErasedFailure, ErasedReport};
    use crate::resolve::{resolve_profile, RunOptions};
    use harness_config::{NoExt, RunConfig};
    use harness_core::{Coverage, FailureKind};
    use std::time::Duration;

    fn base_source() -> RunConfig<NoExt> {
        harness_config::from_toml_str::<NoExt>(
            r#"
[harness]
name = "vault"

[env]
network = "localnet"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
            &|_| None,
        )
        .expect("valid fixture")
    }

    fn base_resolved(source: &RunConfig<NoExt>) -> ResolvedProfile {
        resolve_profile(source, "smoke", &RunOptions::default()).expect("resolves")
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

    /// A fresh, gitignored dir under `<CARGO_MANIFEST_DIR>/tests_result/`, unique per test
    /// invocation, so artifacts land in a stable inspectable location (never a source-tree
    /// `target/` dir) and parallel runs never collide. `write_replay_artifact` creates it.
    fn tempdir(label: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests_result")
            .join(format!(
                "harness-artifact-test-{}-{}-{label}",
                std::process::id(),
                unix_timestamp()
            ))
    }

    #[test]
    fn failing_report_writes_a_toml_artifact_with_the_generic_sections() {
        let source = base_source();
        let resolved = base_resolved(&source);
        let history = serde_json::json!([
            {"Deposit": {"chain": "eth", "user": 0, "amount": 1000}},
        ]);
        let report = failing_report(history, 42);

        let dir = tempdir("toml-sections");
        let path = write_replay_artifact(&dir, &source, &resolved, &report, toml::Table::new())
            .expect("write");
        assert!(
            path.to_string_lossy().ends_with(".replay.toml"),
            "expected a .replay.toml path, got {path:?}"
        );
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("vault-deep-42-"),
            "filename must be <harness>-<profile>-<seed>-<timestamp>.replay.toml, got {path:?}"
        );

        let text = std::fs::read_to_string(&path).expect("artifact file exists");
        let table: toml::Table = toml::from_str(&text).expect("valid TOML");

        // `[harness]` and `[replay]` provenance are present.
        assert_eq!(table["harness"]["name"].as_str(), Some("vault"));
        assert!(
            table.contains_key("replay"),
            "missing [replay] table: {text}"
        );
        // The concrete `[profile.replay]` scenario profile.
        assert_eq!(
            table["profile"]["replay"]["mode"].as_str(),
            Some("scenario"),
            "profile.replay.mode must be \"scenario\": {text}"
        );
        // The resolved `[env]` embed round-trips.
        assert_eq!(
            table["env"]["network"].as_str(),
            Some("localnet"),
            "the resolved [env] must be embedded: {text}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn domain_sections_land_as_top_level_keys() {
        let source = base_source();
        let resolved = base_resolved(&source);
        let report = failing_report(serde_json::json!([]), 3);

        // A domain (e.g. cross-vm) injects a `[[chain]]` array as an extra top-level section.
        let mut domain_sections = toml::Table::new();
        let mut chain = toml::Table::new();
        chain.insert("label".to_string(), toml::Value::String("eth".to_string()));
        chain.insert("kind".to_string(), toml::Value::String("evm".to_string()));
        domain_sections.insert(
            "chain".to_string(),
            toml::Value::Array(vec![toml::Value::Table(chain)]),
        );

        let dir = tempdir("domain-sections");
        let path = write_replay_artifact(&dir, &source, &resolved, &report, domain_sections)
            .expect("write");

        let text = std::fs::read_to_string(&path).expect("artifact file exists");
        let table: toml::Table = toml::from_str(&text).expect("valid TOML");

        let chains = table["chain"].as_array().expect("top-level chain array");
        assert_eq!(chains.len(), 1, "one injected chain: {text}");
        assert_eq!(chains[0]["label"].as_str(), Some("eth"));
        assert_eq!(chains[0]["kind"].as_str(), Some("evm"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn u64_max_amount_falls_back_to_a_json_sidecar() {
        let source = base_source();
        let resolved = base_resolved(&source);
        // Fits u64 (so serde_json::to_value succeeds, as a real ErasedFailure.history always
        // does) but is out of TOML's signed-64-bit integer range, so toml::to_string_pretty must
        // fail on this artifact and the writer must fall back to a .replay.json sidecar.
        let huge: u64 = u64::MAX;
        let history = serde_json::json!([
            {"Deposit": {"chain": "eth", "user": 0, "amount": huge}},
        ]);
        let report = failing_report(history, 7);

        let dir = tempdir("json-sidecar");
        let path = write_replay_artifact(&dir, &source, &resolved, &report, toml::Table::new())
            .expect("write");
        assert!(
            path.to_string_lossy().ends_with(".replay.json"),
            "expected a .replay.json sidecar for an out-of-i64-range integer, got {path:?}"
        );

        // The sidecar's own bytes (what write_replay_artifact itself produced) must hold the
        // u64-range amount exactly, with no precision loss: parsed as raw JSON.
        let raw = std::fs::read_to_string(&path).expect("sidecar file exists");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        let amount = &value["profile"]["replay"]["steps"][0]["op"]["Deposit"]["amount"];
        assert_eq!(amount.as_u64(), Some(huge), "raw JSON bytes: {raw}");

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
}
