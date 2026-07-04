//! Cross-vm variant of the generic `harness-config` schema: adds `[[chain]]`
//! declarations, the typed `EnvSpec` env shape, and mock/rpc target
//! resolution on top of the generic loader. This crate is the worked example
//! for building a domain config layer; see harness-config's docs.

#![warn(missing_docs)]

mod chain;
mod schema;
mod target;
mod validate;

pub use chain::{missing_required_fields, ChainDecl};
pub use schema::{EnvSpec, TargetStr};
pub use target::{parse_target_str, resolve_chain_target, TargetOverrides};

// Generic machinery, re-exported so downstream paths stay stable.
pub use harness_config::{
    humantime_duration, humantime_opt, interpolate_value, CommonKeys, ConfigError,
    EnduranceProfile, ExpectStr, FuzzProfile, HarnessRef, InvariantProfile, Profile,
    ScenarioProfile, ScenarioStepRaw, SeedSpec, Suite, SuitePhase, WorldSource,
};

/// The cross-vm domain extension: `[[chain]]` declarations plus chain-aware
/// validation of the (otherwise opaque) env tables.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossVmExt {
    /// The `[[chain]]` declarations.
    #[serde(rename = "chain", default)]
    pub chain: Vec<ChainDecl>,
}

impl harness_config::ConfigExt for CrossVmExt {
    fn validate(cfg: &RunConfig) -> Result<(), ConfigError> {
        validate::validate_chains(cfg).map_err(ConfigError::Domain)
    }

    fn merge_env_entry<V: harness_config::Doc>(key: &str, slot: &mut V, incoming: V) {
        use harness_config::DocMap;
        // `targets` merges label-wise; every other env key replaces whole.
        if key == "targets" {
            if let Some(incoming_map) = incoming.clone().into_object() {
                if let Some(base) = slot.as_object_mut() {
                    let keys: Vec<String> = incoming_map.iter().map(|(k, _)| k.clone()).collect();
                    let mut incoming_map = incoming_map;
                    for k in keys {
                        let v = incoming_map.remove(&k).expect("key came from this map");
                        base.insert(k, v);
                    }
                    return;
                }
            }
        }
        *slot = incoming;
    }
}

/// The cross-vm run config: the generic shape carrying [`CrossVmExt`].
pub type RunConfig = harness_config::RunConfig<CrossVmExt>;

/// Parses the opaque merged env value into the typed cross-vm [`EnvSpec`].
///
/// A malformed env table (for example a non-table `targets`) fails here, which
/// is how the domain validation pass surfaces it as a hard error even though
/// the generic loader keeps `[env]` opaque.
pub fn env_spec(env: &serde_json::Value) -> Result<EnvSpec, ConfigError> {
    serde_json::from_value(env.clone()).map_err(|e| ConfigError::Deserialize {
        path: "env".to_string(),
        message: e.to_string(),
    })
}

/// Loads a cross-vm config file (TOML, or JSON by `.json` extension).
pub fn load(
    path: &std::path::Path,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    harness_config::load::<CrossVmExt>(path, vars)
}

/// Parses a TOML string as a cross-vm config.
pub fn from_toml_str(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    harness_config::from_toml_str::<CrossVmExt>(s, vars)
}

/// Parses a JSON string as a cross-vm config.
pub fn from_json_str(
    s: &str,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<RunConfig, ConfigError> {
    harness_config::from_json_str::<CrossVmExt>(s, vars)
}
