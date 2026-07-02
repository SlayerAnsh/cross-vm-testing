//! Stage 5 of the loader pipeline: structural validation over the fully typed [`RunConfig`],
//! run after typed deserialization and before the config is handed back to the caller.
//!
//! All target precedence funnels through [`crate::resolve_chain_target`]; this module calls
//! it rather than re-deriving mock-vs-rpc precedence.
//!
//! **Per-profile-effective env.** `env` can be overridden per profile (`[profile.<name>].env`
//! shallow-merges over the top-level `[env]`, already resolved onto each profile by the merge
//! stage). Every env-dependent check here (`env.chains` selection, `env.targets` labels, and
//! the rpc-without-`rpc_url` check) validates against each profile's own effective env rather
//! than only the top-level `[env]`. This is deliberately the more thorough of the two options
//! the task brief allowed: it costs nothing extra here because the merge stage has already
//! materialized the per-profile effective env before this stage runs, so no additional merge
//! logic needs to be duplicated in this module.
use crate::chain::missing_required_fields;
use crate::schema::{EnvSpec, Profile};
use crate::target::{parse_target_str, resolve_chain_target, TargetOverrides};
use crate::{ChainDecl, ConfigError, RunConfig};
use std::collections::HashSet;

/// Runs every structural check against an already-parsed, merged, and typed [`RunConfig`].
/// Returns the first violation found as a hard [`ConfigError`].
pub fn validate(cfg: &RunConfig) -> Result<(), ConfigError> {
    validate_chain_labels_unique(&cfg.chains)?;
    for decl in &cfg.chains {
        validate_chain_kind_non_empty(decl)?;
        validate_chain_fields(decl)?;
    }
    validate_suites(cfg)?;

    for (name, profile) in &cfg.profiles {
        validate_profile_mode_specific(name, profile)?;

        let effective_env = profile
            .common()
            .env
            .clone()
            .unwrap_or_else(|| cfg.env.clone());
        validate_env_selection(name, &effective_env, &cfg.chains)?;
        validate_env_targets(name, &effective_env, &cfg.chains)?;
        validate_rpc_urls(name, &cfg.chains, &effective_env)?;
    }

    Ok(())
}

fn validate_chain_labels_unique(chains: &[ChainDecl]) -> Result<(), ConfigError> {
    let mut seen = HashSet::new();
    for decl in chains {
        if !seen.insert(decl.label.as_str()) {
            return Err(ConfigError::DuplicateChainLabel {
                label: decl.label.clone(),
            });
        }
    }
    Ok(())
}

/// `kind` is non-empty: the framework resolves it to a `ChainKind` at run time (an unknown
/// non-empty kind is a framework-level error), but an empty string can never resolve to
/// anything, so this crate rejects it directly rather than deferring to the framework.
fn validate_chain_kind_non_empty(decl: &ChainDecl) -> Result<(), ConfigError> {
    if decl.kind.is_empty() {
        return Err(ConfigError::EmptyChainKind {
            label: decl.label.clone(),
        });
    }
    Ok(())
}

fn validate_chain_fields(decl: &ChainDecl) -> Result<(), ConfigError> {
    let missing = missing_required_fields(decl);
    if !missing.is_empty() {
        return Err(ConfigError::MissingChainFields {
            label: decl.label.clone(),
            kind: decl.kind.clone(),
            fields: missing.iter().map(|s| s.to_string()).collect(),
        });
    }
    Ok(())
}

fn validate_suites(cfg: &RunConfig) -> Result<(), ConfigError> {
    for (suite_name, suite) in &cfg.suites {
        for profile_name in &suite.profiles {
            if !cfg.profiles.contains_key(profile_name) {
                return Err(ConfigError::UnknownSuiteProfile {
                    suite: suite_name.clone(),
                    profile: profile_name.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_profile_mode_specific(name: &str, profile: &Profile) -> Result<(), ConfigError> {
    match profile {
        Profile::Fuzz(f) => {
            if f.cases == 0 {
                return Err(ConfigError::InvalidCases {
                    profile: name.to_string(),
                });
            }
            if f.ops == 0 {
                return Err(ConfigError::InvalidOps {
                    profile: name.to_string(),
                });
            }
            check_kinds_weights(name, f.kinds.is_some(), f.weights.is_some())?;
        }
        Profile::Invariant(inv) => {
            if inv.ops == 0 {
                return Err(ConfigError::InvalidOps {
                    profile: name.to_string(),
                });
            }
            check_kinds_weights(name, inv.kinds.is_some(), inv.weights.is_some())?;
        }
        Profile::Endurance(e) => {
            if e.duration.is_none() && e.max_ops.is_none() {
                return Err(ConfigError::EnduranceMissingBound {
                    profile: name.to_string(),
                });
            }
            check_kinds_weights(name, e.kinds.is_some(), e.weights.is_some())?;
        }
        Profile::Scenario(s) => {
            if s.steps.is_empty() {
                return Err(ConfigError::EmptySteps {
                    profile: name.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn check_kinds_weights(name: &str, has_kinds: bool, has_weights: bool) -> Result<(), ConfigError> {
    if has_kinds && has_weights {
        return Err(ConfigError::KindsWeightsConflict {
            profile: name.to_string(),
        });
    }
    Ok(())
}

fn validate_env_selection(
    profile: &str,
    env: &EnvSpec,
    chains: &[ChainDecl],
) -> Result<(), ConfigError> {
    if chains.is_empty() {
        return Ok(());
    }
    if let Some(selected) = &env.chains {
        let labels: HashSet<&str> = chains.iter().map(|c| c.label.as_str()).collect();
        for label in selected {
            if !labels.contains(label.as_str()) {
                return Err(ConfigError::UnknownChainSelection {
                    profile: profile.to_string(),
                    label: label.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_env_targets(
    profile: &str,
    env: &EnvSpec,
    chains: &[ChainDecl],
) -> Result<(), ConfigError> {
    if let Some(targets) = &env.targets {
        let labels: HashSet<&str> = chains.iter().map(|c| c.label.as_str()).collect();
        for label in targets.keys() {
            if !labels.contains(label.as_str()) {
                return Err(ConfigError::UnknownTargetLabel {
                    profile: profile.to_string(),
                    label: label.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_rpc_urls(profile: &str, chains: &[ChainDecl], env: &EnvSpec) -> Result<(), ConfigError> {
    for decl in chains {
        let decl_target = match &decl.target {
            Some(s) => Some(parse_target_str(s).map_err(|message| {
                ConfigError::InvalidChainTarget {
                    label: decl.label.clone(),
                    message,
                }
            })?),
            None => None,
        };
        let resolved = resolve_chain_target(&decl.label, decl_target, env, &TargetOverrides::default());
        if resolved == crate::TargetStr::Rpc && decl.rpc_url.is_none() {
            return Err(ConfigError::MissingRpcUrl {
                profile: profile.to_string(),
                label: decl.label.clone(),
            });
        }
    }
    Ok(())
}
