//! `cross-vm-config`: the declarative TOML/JSON run-config schema for cross-vm-testing.
//!
//! This is a pure data crate: no framework, tokio, or chain-provider dependency. It parses a
//! config document (TOML or JSON) into a typed [`RunConfig`], so it stays unit-testable with
//! plain string fixtures and is safe for a later proc-macro to reuse verbatim. Kind names stay
//! `String`, and scenario ops stay raw [`serde_json::Value`] (a format-agnostic value that keeps
//! full integer precision for JSON input, unlike `toml::Value`); this crate never sees harness
//! types.
//!
//! ## Format-agnostic loading and integer precision
//! TOML and JSON input run the same pipeline over a small `Doc` value abstraction (see the
//! private `value` module), so TOML input keeps `toml::Value` behavior exactly while JSON input
//! is processed natively as `serde_json::Value`. This matters for the `.replay.json` sidecar
//! (spec §10): a scenario `op` amount in `(i64::MAX, u64::MAX]` survives a JSON round-trip
//! losslessly, because it never passes through `toml::Value` (whose integers are signed 64-bit).
//! Integers `> u64::MAX` are **not** supported: `serde_json` (built without its
//! `arbitrary_precision` feature) already parses such a literal into an `f64` at
//! `serde_json::from_str` time, before this crate sees it, so precision is lost at parse. In
//! practice replay histories only carry `u64`-range amounts (they originate from
//! `serde_json::to_value`, which requires a value fit `u64`), so this range covers the real use.
//!
//! The full five-stage loader pipeline (parse, interpolate, merge, typed deserialize,
//! structural validate) is implemented; see [`from_toml_str`] for the stage order. The `vars`
//! closure parameter on every entry point resolves `${VAR}` references during the interpolate
//! stage.

#![warn(missing_docs)]

mod chain;
mod duration;
mod interpolate;
mod merge;
mod schema;
mod seed;
mod target;
mod validate;
mod value;

pub use chain::{missing_required_fields, ChainDecl};
pub use duration::{humantime_duration, humantime_opt};
pub use interpolate::interpolate_value;
pub use schema::{
    CommonKeys, EnduranceProfile, EnvSpec, ExpectStr, FuzzProfile, HarnessRef, InvariantProfile,
    Profile, ScenarioProfile, ScenarioStepRaw, Suite, TargetStr,
};
pub use seed::SeedSpec;
pub use target::{parse_target_str, resolve_chain_target, TargetOverrides};

use crate::value::{Doc, DocMap};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The fully parsed, typed contents of one `*.cross-vm.toml` (or `.json`) config file.
#[derive(Debug, Clone, PartialEq)]
pub struct RunConfig {
    /// `[harness]`: registry key plus named setup.
    pub harness: HarnessRef,
    /// `[[chain]]` entries; empty means the setup fn hard codes chains.
    pub chains: Vec<ChainDecl>,
    /// `[env]`: the default environment request for every profile.
    pub env: EnvSpec,
    /// `[profile.<name>]` blocks, keyed by name. Each profile's `common().env` already holds
    /// the fully merged effective environment for that profile (the merge stage shallow-merges
    /// the profile's own `env` override over the top-level `[env]` before typed deserialize).
    pub profiles: BTreeMap<String, Profile>,
    /// `[suite.<name>]` blocks, keyed by name.
    pub suites: BTreeMap<String, Suite>,
    /// Warnings collected while loading, currently only the `[defaults]` allowlist-strip:
    /// one entry per default key removed because it did not apply to a profile's mode.
    pub warnings: Vec<String>,
}

/// Errors returned while loading or parsing a config document.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading the config file from disk failed.
    #[error("failed to read config file `{path}`: {source}")]
    Io {
        /// The path that failed to read.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The raw document failed to parse as TOML or JSON.
    #[error("failed to parse config: {0}")]
    Parse(String),
    /// A parsed value failed typed deserialization into the config schema.
    #[error("failed to deserialize `{path}`: {message}")]
    Deserialize {
        /// A dotted path locating the offending value (e.g. `profile.smoke`).
        path: String,
        /// The underlying deserialization error message.
        message: String,
    },
    /// A `${VAR}` reference had no value and no `:-default` fallback. Never carries the
    /// surrounding string value, since it may hold an RPC secret.
    #[error(
        "undefined variable `{var}` referenced at `{path}` (set the environment variable, or add a `:-default` fallback)"
    )]
    MissingVar {
        /// The variable name, exactly as written inside `${...}`.
        var: String,
        /// The TOML path of the string value that referenced it (e.g. `chain[1].rpc_url`).
        path: String,
    },
    /// A `${...}` interpolation expression was malformed (e.g. an unterminated `${`).
    #[error("invalid interpolation expression at `{path}`: {message}")]
    Interpolation {
        /// The TOML path of the offending string value.
        path: String,
        /// A description of what was malformed; never echoes the value's contents.
        message: String,
    },
    /// Two or more `[[chain]]` entries share the same `label`.
    #[error("duplicate chain label `{label}`")]
    DuplicateChainLabel {
        /// The label that appears more than once.
        label: String,
    },
    /// A `[[chain]]` entry is missing field(s) required for its `kind`.
    #[error("chain `{label}` (kind `{kind}`) is missing required field(s): {}", fields.join(", "))]
    MissingChainFields {
        /// The chain's label.
        label: String,
        /// The chain's `kind`.
        kind: String,
        /// The names of the missing required fields.
        fields: Vec<String>,
    },
    /// A `[[chain]]` entry set `kind` to an empty string. The framework resolves `kind` to a
    /// `ChainKind` at run time; an unrecognized non-empty kind is a framework error, but an
    /// empty kind can never resolve to anything, so this crate hard-errors on it directly.
    #[error("chain `{label}`: `kind` must not be empty")]
    EmptyChainKind {
        /// The chain's label.
        label: String,
    },
    /// A fuzz profile's `cases` was not greater than zero.
    #[error("profile `{profile}`: `cases` must be greater than 0")]
    InvalidCases {
        /// The offending profile's name.
        profile: String,
    },
    /// A fuzz or invariant profile's `ops` was not greater than zero.
    #[error("profile `{profile}`: `ops` must be greater than 0")]
    InvalidOps {
        /// The offending profile's name.
        profile: String,
    },
    /// A scenario profile's `steps` was empty.
    #[error("profile `{profile}`: `steps` must be non-empty")]
    EmptySteps {
        /// The offending profile's name.
        profile: String,
    },
    /// An endurance profile set neither `duration` nor `max_ops`.
    #[error("profile `{profile}`: endurance requires `duration` or `max_ops` (or both)")]
    EnduranceMissingBound {
        /// The offending profile's name.
        profile: String,
    },
    /// A profile set both `kinds` and `weights`, which are mutually exclusive.
    #[error("profile `{profile}`: `kinds` and `weights` are mutually exclusive")]
    KindsWeightsConflict {
        /// The offending profile's name.
        profile: String,
    },
    /// A `[suite.<name>]` named a profile that does not exist.
    #[error("suite `{suite}` references unknown profile `{profile}`")]
    UnknownSuiteProfile {
        /// The suite's name.
        suite: String,
        /// The profile name it referenced that does not exist.
        profile: String,
    },
    /// `env.chains` named a label with no matching `[[chain]]` entry.
    #[error("profile `{profile}`: env.chains references unknown chain label `{label}`")]
    UnknownChainSelection {
        /// The profile whose effective `env.chains` referenced the label.
        profile: String,
        /// The unmatched label.
        label: String,
    },
    /// `env.targets` named a label with no matching `[[chain]]` entry.
    #[error("profile `{profile}`: env.targets references unknown chain label `{label}`")]
    UnknownTargetLabel {
        /// The profile whose effective `env.targets` referenced the label.
        profile: String,
        /// The unmatched label.
        label: String,
    },
    /// A `[[chain]].target` value was neither `"mock"` nor `"rpc"`.
    #[error("chain `{label}`: invalid `target` value: {message}")]
    InvalidChainTarget {
        /// The chain's label.
        label: String,
        /// A description of the invalid value, produced by [`crate::parse_target_str`]. It
        /// does embed the raw offending string (e.g. `` invalid target `bogus`, expected
        /// "mock" or "rpc" ``); that is fine here since a chain's `target` field is a fixed
        /// enum-like string (`"mock"`/`"rpc"`), never a secret, unlike an interpolated value.
        message: String,
    },
    /// A chain resolved to the `rpc` target (for some profile's effective environment) but has
    /// no `rpc_url`.
    #[error("profile `{profile}`: chain `{label}` resolves to target `rpc` but has no `rpc_url`")]
    MissingRpcUrl {
        /// The profile under which this chain resolves to `rpc`.
        profile: String,
        /// The chain's label.
        label: String,
    },
}

/// A raw, mostly-typed view of the document used to drive [`build_run_config`]. Profiles are
/// left as raw tables so `mode` can be popped and dispatched manually; everything else already
/// has a stable shape that plain derived `Deserialize` handles.
///
/// `deny_unknown_fields`: the merge stage consumes and removes `[defaults]` before this struct
/// ever sees the document, so a top-level typo is still a hard error here.
///
/// `replay` is the one deliberate exception: spec section 4.2 lists a top-level `[replay]`
/// block as valid, provenance data written by `cross-vm replay` into P4 replay artifacts, and
/// explicitly "ignored by the run schema". It is parsed here (as an untyped table) purely so
/// `deny_unknown_fields` doesn't reject it, then dropped in [`build_run_config`]; it never
/// reaches [`RunConfig`].
///
/// Generic over the document value type `V` ([`toml::Value`] or [`serde_json::Value`]) so
/// profile tables keep the source format's native number representation until they are dispatched
/// into a per-mode struct; this is what lets a JSON scenario `op` keep `u64`-range precision.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(bound(deserialize = "V: serde::de::DeserializeOwned"))]
struct RawRunConfig<V> {
    harness: HarnessRef,
    #[serde(rename = "chain", default)]
    chain: Vec<ChainDecl>,
    #[serde(default)]
    env: EnvSpec,
    #[serde(rename = "profile", default)]
    profile: BTreeMap<String, V>,
    #[serde(rename = "suite", default)]
    suite: BTreeMap<String, Suite>,
    /// Tolerated but ignored; see the struct doc comment.
    #[serde(default)]
    replay: Option<V>,
}

/// Parses a TOML document string into a [`RunConfig`], running the full five-stage pipeline:
/// parse, interpolate (`vars`), merge (`[defaults]` and per-profile `env`), typed deserialize,
/// structural validate.
pub fn from_toml_str(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    let value: toml::Value = toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
    load_from_value(value, vars)
}

/// Parses a JSON document string into a [`RunConfig`], running the same pipeline as
/// [`from_toml_str`].
///
/// The schema is format agnostic, but JSON input is processed **natively** as
/// [`serde_json::Value`] (not downgraded through [`toml::Value`]) so integer precision survives:
/// a scenario `op` amount in `(i64::MAX, u64::MAX]` round-trips exactly, which is what the
/// `.replay.json` sidecar relies on. Equivalent TOML and JSON documents still produce equal
/// [`RunConfig`]s.
pub fn from_json_str(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    let value: serde_json::Value =
        serde_json::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
    load_from_value(value, vars)
}

/// Runs stages 2 through 5 of the loader pipeline over an already-parsed document: interpolate,
/// merge (collecting defaults-strip warnings), typed deserialize, structural validate.
///
/// Generic over the document value type so TOML ([`toml::Value`]) and JSON
/// ([`serde_json::Value`]) share one implementation; the existing `toml::Value`-based unit tests
/// pin the TOML path's behavior exactly.
fn load_from_value<V: Doc>(
    mut value: V,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    interpolate::interpolate_doc(&mut value, vars)?;
    let warnings = merge::merge(&mut value)?;
    let mut cfg = build_run_config(value)?;
    validate::validate(&cfg)?;
    cfg.warnings = warnings;
    Ok(cfg)
}

/// Reads `path` and parses it as TOML, or as JSON when the extension is `.json`.
pub fn load(path: &Path, vars: &dyn Fn(&str) -> Option<String>) -> Result<RunConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        from_json_str(&contents, vars)
    } else {
        from_toml_str(&contents, vars)
    }
}

/// Deserializes the stable-shaped parts of the document, then dispatches every profile table
/// into its per-mode struct by mode name.
fn build_run_config<V: Doc>(value: V) -> Result<RunConfig, ConfigError> {
    let raw: RawRunConfig<V> =
        value
            .deserialize_into()
            .map_err(|message| ConfigError::Deserialize {
                path: "<root>".to_string(),
                message,
            })?;
    // `[replay]` is parsed only so `deny_unknown_fields` tolerates it; it is not part of the
    // run schema and is intentionally dropped here.
    let _ = raw.replay;

    let mut profiles = BTreeMap::new();
    for (name, mut profile_value) in raw.profile {
        let mode = {
            let table =
                profile_value
                    .as_object_mut()
                    .ok_or_else(|| ConfigError::Deserialize {
                        path: format!("profile.{name}"),
                        message: "a profile must be a table".to_string(),
                    })?;
            let mode_value = table.remove("mode").ok_or_else(|| ConfigError::Deserialize {
                path: format!("profile.{name}"),
                message: "missing required key `mode`".to_string(),
            })?;
            mode_value
                .as_str()
                .ok_or_else(|| ConfigError::Deserialize {
                    path: format!("profile.{name}.mode"),
                    message: "`mode` must be a string".to_string(),
                })?
                .to_string()
        };
        let profile = Profile::from_mode_table(&mode, profile_value).map_err(|message| {
            ConfigError::Deserialize {
                path: format!("profile.{name}"),
                message,
            }
        })?;
        profiles.insert(name, profile);
    }

    Ok(RunConfig {
        harness: raw.harness,
        chains: raw.chain,
        env: raw.env,
        profiles,
        suites: raw.suite,
        warnings: Vec::new(),
    })
}
