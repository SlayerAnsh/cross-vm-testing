//! The `--json-report` envelope (spec `docs/config-runs-spec.md` section 9): [`JsonReport`],
//! [`Invocation`], and [`write_json_report`], the single write-once-per-invocation entry point
//! [`crate::cli`] calls after every selected profile has run.
//!
//! The envelope holds **all** profiles of one invocation, not one file per profile: a suite of
//! three profiles produces one JSON file with a three-element `profiles` array. This mirrors the
//! CLI's own "combined exit code for the whole invocation" contract (spec section 8) — the JSON
//! report is that same invocation's machine-readable twin.

use std::io;
use std::path::Path;

use super::erased::ErasedReport;

/// The `--json-report` (or `json_report` profile key) output envelope: `schema_version` pins the
/// shape for forward compatibility (spec section 9 only ever specifies `1`), `invocation`
/// records what was asked for, and `profiles` holds every [`ErasedReport`] the invocation
/// produced, in the order those profiles ran.
#[derive(serde::Serialize)]
pub struct JsonReport<'a> {
    /// Always `1` today; a future incompatible envelope change bumps this.
    pub schema_version: u32,
    /// What was asked for: the config path, the selected profile names, and any CLI overrides.
    pub invocation: Invocation<'a>,
    /// One entry per selected profile, in run order. Empty if every profile failed to resolve
    /// before it could run (a config/usage error, not a run outcome).
    pub profiles: &'a [ErasedReport],
}

/// The `invocation` block of a [`JsonReport`]: what this run of the `cross-vm` binary was asked
/// to do, not what happened (that is `profiles`).
#[derive(serde::Serialize)]
pub struct Invocation<'a> {
    /// The config file path exactly as passed on the command line (not canonicalized).
    pub config: &'a str,
    /// The profile names the invocation selected (`--profile`/`--suite`/env/auto-select
    /// resolution, spec section 8), regardless of whether every one of them finished running
    /// (a `stop_on_failure` suite can select more names than it ends up running).
    pub profiles: &'a [String],
    /// The CLI-set scalar overrides for this invocation (e.g. `{"seed": 7, "cases": 2}`), or an
    /// empty object if none were set. Deliberately excludes the resolved config's own fields
    /// (env values, params, rpc URLs, ...): this is a record of what the CLI flags overrode, not
    /// a copy of the config, so it can never leak a config secret.
    pub overrides: serde_json::Value,
}

/// Writes the run-report envelope for one `cross-vm` invocation as pretty JSON to `path`,
/// creating `path`'s parent directories if they do not already exist.
///
/// Called once per invocation, after every selected profile has run (or the invocation stopped
/// early on `stop_on_failure`) — never once per profile. `config` is the config file path as the
/// user passed it, `profiles` is the invocation's selected profile names (spec section 8's
/// selection rules), `reports` is every [`ErasedReport`] produced so far, and `overrides` is the
/// small JSON object of CLI-set scalars built by the caller (never a copy of the config).
///
/// # Errors
/// Returns the underlying [`io::Error`] if creating `path`'s parent directories or writing the
/// file fails. Serialization itself cannot fail: every type reachable from [`JsonReport`]
/// derives `Serialize` over plain data (no maps with non-string keys, no user-controlled
/// float `NaN`/`Infinity`).
pub fn write_json_report(
    path: &Path,
    config: &str,
    profiles: &[String],
    reports: &[ErasedReport],
    overrides: serde_json::Value,
) -> io::Result<()> {
    let report = JsonReport {
        schema_version: 1,
        invocation: Invocation {
            config,
            profiles,
            overrides,
        },
        profiles: reports,
    };

    // `serde_json::to_string_pretty` only fails on a `Serialize` impl that errors (e.g. a map
    // with a non-string key) or writer I/O; neither applies to an in-memory `String` target over
    // this crate's plain-data types, so an error here would be a logic bug, not a runtime
    // condition callers need to branch on. Map it into `io::Error` anyway rather than panicking,
    // since a caller running unattended (CI) should get a clean exit code, not a panic.
    let json = serde_json::to_string_pretty(&report).map_err(io::Error::other)?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ErasedFailure;
    use crate::harness::FailureKind;

    fn report(profile: &str, seed: u64) -> ErasedReport {
        ErasedReport {
            harness: "vault".to_string(),
            profile: profile.to_string(),
            mode: "fuzz".to_string(),
            seed,
            steps: 3,
            skipped: 0,
            coverage: Default::default(),
            stats: None,
            elapsed: std::time::Duration::from_millis(5),
            failure: None,
        }
    }

    fn failing_report(profile: &str) -> ErasedReport {
        ErasedReport {
            failure: Some(ErasedFailure {
                step: 2,
                kind: FailureKind::Bug("over-withdraw accepted".to_string()),
                op_debug: Some("Withdraw { .. }".to_string()),
                history: serde_json::Value::Array(vec![]),
                shrunk: false,
            }),
            ..report(profile, 42)
        }
    }

    #[test]
    fn write_json_report_round_trips_schema_version_and_invocation() {
        let dir = tempfile_dir();
        let path = dir.join("nested").join("out.json");
        let reports = vec![report("smoke", 7), failing_report("deep")];
        let profiles = vec!["smoke".to_string(), "deep".to_string()];
        let overrides = serde_json::json!({"seed": 7, "cases": 2});

        write_json_report(&path, "vault.cross-vm.toml", &profiles, &reports, overrides.clone())
            .expect("write succeeds, creating the nested parent dir");

        let raw = std::fs::read_to_string(&path).expect("file was written");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["invocation"]["config"], "vault.cross-vm.toml");
        assert_eq!(value["invocation"]["profiles"], serde_json::json!(["smoke", "deep"]));
        assert_eq!(value["invocation"]["overrides"], overrides);

        let profiles_out = value["profiles"].as_array().expect("profiles array");
        assert_eq!(profiles_out.len(), 2);
        assert_eq!(profiles_out[0]["mode"], "fuzz");
        assert_eq!(profiles_out[0]["harness"], "vault");
        assert_eq!(profiles_out[0]["seed"], 7);
        assert_eq!(profiles_out[0]["steps"], 3);
        assert_eq!(profiles_out[1]["seed"], 42);
        assert_eq!(profiles_out[1]["failure"]["kind"]["Bug"], "over-withdraw accepted");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_json_report_defaults_overrides_to_an_empty_object_when_none_set() {
        let dir = tempfile_dir();
        let path = dir.join("out.json");
        write_json_report(&path, "cfg.toml", &[], &[], serde_json::json!({})).expect("write");

        let raw = std::fs::read_to_string(&path).expect("file was written");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(value["invocation"]["overrides"], serde_json::json!({}));
        assert_eq!(value["profiles"], serde_json::json!([]));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fresh temp directory under the target dir's `tmp/`, named for the test process id so
    /// parallel test runs never collide. No external crate needed for this one narrow use.
    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cross-vm-json-report-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}
