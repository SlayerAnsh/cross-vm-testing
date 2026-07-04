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
mod validate;
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

use std::collections::BTreeMap;
use std::path::Path;

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
    /// A `[suite.<name>]` set both the legacy `profiles` list and `phases`. They are mutually
    /// exclusive: use `phases` for pipelines, or `profiles` for the flat legacy form.
    #[error("suite `{suite}`: set either `profiles` or `phases`, not both")]
    SuiteProfilesAndPhases {
        /// The suite's name.
        suite: String,
    },
    /// A phase's `needs` entry did not name an earlier phase in the same suite. Declaration order
    /// is execution order, so a dependency must appear before the phase that needs it (this also
    /// rejects self references and forward references).
    #[error("suite `{suite}`: phase `{phase}` needs `{needed}`, which is not an earlier phase in the same suite (needs must name a phase declared before it)")]
    PhaseNeedsNotEarlier {
        /// The suite's name.
        suite: String,
        /// The phase profile whose `needs` is invalid.
        phase: String,
        /// The offending `needs` entry.
        needed: String,
    },
    /// Two phases in one suite ran the same profile. Each phase profile must be unique within a
    /// suite so `needs` entries name it unambiguously.
    #[error("suite `{suite}`: phase profile `{profile}` is declared more than once (phase profiles must be unique within a suite)")]
    DuplicatePhaseProfile {
        /// The suite's name.
        suite: String,
        /// The profile name declared more than once.
        profile: String,
    },
    /// A phase set `world = "inherit"` but did not have exactly one `needs` entry. An inheriting
    /// phase starts from exactly one donor, so it must name exactly one dependency.
    #[error("suite `{suite}`: phase `{phase}` sets `world = \"inherit\"` but has {needs} `needs` entries (inherit requires exactly one, the donor)")]
    PhaseInheritArity {
        /// The suite's name.
        suite: String,
        /// The inheriting phase profile.
        phase: String,
        /// How many `needs` entries the phase declared.
        needs: usize,
    },
    /// A phase involved in a `world = "inherit"` handoff is not single-setup. Only single-setup
    /// modes (`invariant`, `endurance`, `scenario`, or `fuzz` with `cases == 1`) build exactly
    /// one world, which a handoff can donate or consume.
    #[error("suite `{suite}`: phase `{phase}` ({role}) is not single-setup (world inherit needs `invariant`, `endurance`, `scenario`, or `fuzz` with `cases == 1`)")]
    PhaseWorldNotSingleSetup {
        /// The suite's name.
        suite: String,
        /// The offending phase profile.
        phase: String,
        /// Whether this phase is the `donor` or the `inheriting phase`.
        role: String,
    },
    /// Two phases declared `world = "inherit"` against the same donor. State forking is not
    /// implemented: a donor can feed exactly one inheriting phase (replay fork is milestone 2).
    #[error("suite `{suite}`: donor `{donor}` is inherited by both phase `{first}` and phase `{second}`, but a donor can feed only one inheriting phase (state forking is not implemented; replay fork is milestone 2)")]
    SharedDonor {
        /// The suite's name.
        suite: String,
        /// The shared donor phase profile.
        donor: String,
        /// The first inheriting phase.
        first: String,
        /// The second inheriting phase.
        second: String,
    },
    /// A domain extension ([`ConfigExt::validate`]) rejected the config.
    #[error("{0}")]
    Domain(String),
}

/// Parses a TOML document string into a [`RunConfig`], running the full five-stage pipeline:
/// parse, interpolate (`vars`), merge (`[defaults]` and per-profile `env`), typed deserialize,
/// structural validate.
pub fn from_toml_str<X: ConfigExt>(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig<X>, ConfigError> {
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
pub fn from_json_str<X: ConfigExt>(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig<X>, ConfigError> {
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
fn load_from_value<V: Doc, X: ConfigExt>(
    mut value: V,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig<X>, ConfigError> {
    interpolate::interpolate_doc(&mut value, vars)?;
    let warnings = merge::merge::<V, X>(&mut value)?;
    let mut cfg = build_run_config::<V, X>(value)?;
    validate::normalize_suite_phases::<X>(&mut cfg)?;
    validate::validate::<X>(&cfg)?;
    cfg.warnings = warnings;
    Ok(cfg)
}

/// Reads `path` and parses it as TOML, or as JSON when the extension is `.json`.
pub fn load<X: ConfigExt>(
    path: &Path,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig<X>, ConfigError> {
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

/// Deserializes the stable-shaped parts of the document (popping each generic
/// top-level key off the root table), hands every remaining top-level key to
/// the domain extension `X`, then dispatches every profile table into its
/// per-mode struct by mode name.
fn build_run_config<V: Doc, X: ConfigExt>(value: V) -> Result<RunConfig<X>, ConfigError> {
    let mut root = value
        .into_object()
        .ok_or_else(|| ConfigError::Deserialize {
            path: "<root>".to_string(),
            message: "config root must be a table".to_string(),
        })?;

    let harness: HarnessRef = match root.remove("harness") {
        Some(v) => v
            .deserialize_into()
            .map_err(|message| ConfigError::Deserialize {
                path: "harness".to_string(),
                message,
            })?,
        None => {
            return Err(ConfigError::Deserialize {
                path: "<root>".to_string(),
                message: "missing required key `harness`".to_string(),
            })
        }
    };

    let env: serde_json::Value = match root.remove("env") {
        Some(v) => v
            .deserialize_into()
            .map_err(|message| ConfigError::Deserialize {
                path: "env".to_string(),
                message,
            })?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };

    let profile_tables: BTreeMap<String, V> = match root.remove("profile") {
        Some(v) => v
            .deserialize_into()
            .map_err(|message| ConfigError::Deserialize {
                path: "profile".to_string(),
                message,
            })?,
        None => BTreeMap::new(),
    };

    let suites: BTreeMap<String, Suite> = match root.remove("suite") {
        Some(v) => v
            .deserialize_into()
            .map_err(|message| ConfigError::Deserialize {
                path: "suite".to_string(),
                message,
            })?,
        None => BTreeMap::new(),
    };

    // `[replay]` is provenance in replay artifacts; tolerated and dropped,
    // exactly as the pre-extraction loader did.
    let _ = root.remove("replay");

    // Everything left at the top level belongs to the domain extension. With
    // `NoExt` (deny_unknown_fields over an empty struct) any leftover key is a
    // hard error, preserving the old `deny_unknown_fields` behavior.
    let ext: X =
        V::from_object(root)
            .deserialize_into()
            .map_err(|message| ConfigError::Deserialize {
                path: "<root>".to_string(),
                message,
            })?;

    let mut profiles = BTreeMap::new();
    for (name, mut profile_value) in profile_tables {
        let mode = {
            let table = profile_value
                .as_object_mut()
                .ok_or_else(|| ConfigError::Deserialize {
                    path: format!("profile.{name}"),
                    message: "a profile must be a table".to_string(),
                })?;
            let mode_value = table
                .remove("mode")
                .ok_or_else(|| ConfigError::Deserialize {
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
        harness,
        env,
        profiles,
        suites,
        warnings: Vec::new(),
        ext,
    })
}
