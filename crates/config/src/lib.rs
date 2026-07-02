//! `cross-vm-config`: the declarative TOML/JSON run-config schema for cross-vm-testing.
//!
//! This is a pure data crate: no framework, tokio, or chain-provider dependency. It parses a
//! config document (TOML or JSON) into a typed [`RunConfig`], so it stays unit-testable with
//! plain string fixtures and is safe for a later proc-macro to reuse verbatim. Kind names stay
//! `String`, and scenario ops stay raw [`toml::Value`]; this crate never sees harness types.
//!
//! This first pass implements only parsing and typed deserialization: environment variable
//! interpolation, `[defaults]` merging, and structural validation land in a later task. The
//! `vars` closure parameter on every entry point is accepted now (for a stable signature) but
//! unused until then.

#![warn(missing_docs)]

mod chain;
mod duration;
mod schema;
mod seed;

pub use chain::{missing_required_fields, ChainDecl};
pub use duration::{humantime_duration, humantime_opt};
pub use schema::{
    CommonKeys, EnduranceProfile, EnvSpec, ExpectStr, FuzzProfile, HarnessRef, InvariantProfile,
    Profile, ScenarioProfile, ScenarioStepRaw, Suite, TargetStr,
};
pub use seed::SeedSpec;

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
    /// `[profile.<name>]` blocks, keyed by name.
    pub profiles: BTreeMap<String, Profile>,
    /// `[suite.<name>]` blocks, keyed by name.
    pub suites: BTreeMap<String, Suite>,
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
}

/// A raw, mostly-typed view of the document used to drive [`build_run_config`]. Profiles are
/// left as raw tables so `mode` can be popped and dispatched manually; everything else already
/// has a stable shape that plain derived `Deserialize` handles.
#[derive(Deserialize)]
struct RawRunConfig {
    harness: HarnessRef,
    #[serde(rename = "chain", default)]
    chain: Vec<ChainDecl>,
    #[serde(default)]
    env: EnvSpec,
    #[serde(rename = "profile", default)]
    profile: BTreeMap<String, toml::Table>,
    #[serde(rename = "suite", default)]
    suite: BTreeMap<String, Suite>,
}

/// Parses a TOML document string into a [`RunConfig`].
///
/// This stage only parses and typed-deserializes: no interpolation, no `[defaults]` merging,
/// no structural validation. The `vars` closure is accepted for a stable signature but unused
/// until interpolation lands in a later task.
pub fn from_toml_str(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    let _ = vars;
    let value: toml::Value = toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
    build_run_config(value)
}

/// Parses a JSON document string into a [`RunConfig`].
///
/// The schema is format agnostic: the JSON value is converted to the same [`toml::Value`]
/// representation `from_toml_str` uses, so both inputs share one dispatch path and produce
/// equal [`RunConfig`]s for equivalent documents. See [`from_toml_str`] for what this stage
/// does and does not do yet.
pub fn from_json_str(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    let _ = vars;
    let json_value: serde_json::Value =
        serde_json::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let value = json_to_toml_value(json_value)?;
    build_run_config(value)
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
fn build_run_config(value: toml::Value) -> Result<RunConfig, ConfigError> {
    let raw = RawRunConfig::deserialize(value).map_err(|e| ConfigError::Deserialize {
        path: "<root>".to_string(),
        message: e.to_string(),
    })?;

    let mut profiles = BTreeMap::new();
    for (name, mut table) in raw.profile {
        let mode_value = table.remove("mode").ok_or_else(|| ConfigError::Deserialize {
            path: format!("profile.{name}"),
            message: "missing required key `mode`".to_string(),
        })?;
        let mode = mode_value
            .as_str()
            .ok_or_else(|| ConfigError::Deserialize {
                path: format!("profile.{name}.mode"),
                message: "`mode` must be a string".to_string(),
            })?
            .to_string();
        let profile =
            Profile::from_mode_table(&mode, table).map_err(|message| ConfigError::Deserialize {
                path: format!("profile.{name}"),
                message,
            })?;
        profiles.insert(name, profile);
    }

    Ok(RunConfig {
        harness: raw.harness,
        chains: raw.chain,
        env: raw.env,
        profiles,
        suites: raw.suite,
    })
}

/// Converts a `serde_json::Value` into an equivalent `toml::Value`, so JSON input can reuse
/// the exact same typed-deserialize path as TOML input. JSON `null` has no TOML equivalent and
/// is rejected; every config field this crate defines is either required or `Option`, so a
/// valid document never needs to express `null`.
fn json_to_toml_value(json: serde_json::Value) -> Result<toml::Value, ConfigError> {
    Ok(match json {
        serde_json::Value::Null => {
            return Err(ConfigError::Parse(
                "JSON `null` has no TOML equivalent; omit the key instead".to_string(),
            ))
        }
        serde_json::Value::Bool(b) => toml::Value::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                return Err(ConfigError::Parse(format!(
                    "number `{n}` is out of range for TOML"
                )));
            }
        }
        serde_json::Value::String(s) => toml::Value::String(s),
        serde_json::Value::Array(items) => {
            let converted = items
                .into_iter()
                .map(json_to_toml_value)
                .collect::<Result<Vec<_>, _>>()?;
            toml::Value::Array(converted)
        }
        serde_json::Value::Object(map) => {
            let mut table = toml::value::Table::new();
            for (k, v) in map {
                table.insert(k, json_to_toml_value(v)?);
            }
            toml::Value::Table(table)
        }
    })
}
