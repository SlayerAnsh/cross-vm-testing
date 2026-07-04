//! Declarative TOML/JSON run-config schema for `harness-core`: parse,
//! interpolate, merge, typed deserialize, validate. Pure data, no runtime
//! dependencies. Domain layers extend it via [`ConfigExt`]; see the cross-vm
//! crates for a worked example.

mod duration;
mod ext;
mod interpolate;
mod merge;
mod schema;
mod seed;
mod value;

pub use duration::{humantime_duration, humantime_opt};
pub use ext::{ConfigExt, NoExt};
pub use interpolate::interpolate_value;
pub use schema::{
    CommonKeys, EnduranceProfile, ExpectStr, FuzzProfile, HarnessRef, InvariantProfile, Profile,
    ScenarioProfile, ScenarioStepRaw, Suite, SuitePhase, WorldSource,
};
pub use seed::SeedSpec;
pub use value::{Doc, DocMap};

/// The fully loaded, validated run configuration, parameterized by a domain
/// extension `X`.
#[derive(Debug, Clone)]
pub struct RunConfig<X: ConfigExt> {
    /// The `[harness]` reference block.
    pub harness: HarnessRef,
    /// Top-level `[env]`, opaque to the generic layer. Always a JSON object;
    /// `{}` when the config file omits `[env]`.
    pub env: serde_json::Value,
    /// Profiles keyed by name.
    pub profiles: std::collections::BTreeMap<String, Profile>,
    /// Suites keyed by name.
    pub suites: std::collections::BTreeMap<String, Suite>,
    /// Non-fatal warnings gathered during loading.
    pub warnings: Vec<String>,
    /// The domain's own top-level sections.
    pub ext: X,
}

/// Errors returned while loading or parsing a config document. (Extended in later tasks.)
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The raw document failed to parse as TOML or JSON, or a stage rejected the document's
    /// structure before typed deserialization.
    #[error("failed to parse config: {0}")]
    Parse(String),
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
}
