//! Cross-vm domain validation: the chain-specific checks the generic loader cannot make.
//!
//! Run by [`crate::CrossVmExt::validate`] after the generic structural validation pass. All
//! target precedence funnels through [`crate::resolve_chain_target`]; this module calls it
//! rather than re-deriving mock-vs-rpc precedence.
//!
//! **Per-profile-effective env.** `env` can be overridden per profile (`[profile.<name>].env`
//! shallow-merges over the top-level `[env]`, already resolved onto each profile by the generic
//! merge stage). Every env-dependent check here (`env.chains` selection, `env.targets` labels,
//! and the rpc-without-`rpc_url` check) validates against each profile's own effective env. The
//! generic layer keeps `[env]` opaque, so this module re-types it through [`crate::env_spec`]; a
//! malformed env table (for example a non-table `targets`) is itself a validation failure.
use crate::chain::missing_required_fields;
use crate::schema::EnvSpec;
use crate::target::{parse_target_str, resolve_chain_target, TargetOverrides};
use crate::{ChainDecl, RunConfig, TargetStr};
use std::collections::HashSet;

/// All cross-vm domain checks, run by [`crate::CrossVmExt::validate`] after generic structural
/// validation: unique chain labels, non-empty kinds, per-kind required fields, env selections
/// and target labels naming declared chains, and rpc-target chains carrying an rpc_url.
///
/// Returns the first violation as an error string; the caller wraps it in
/// [`crate::ConfigError::Domain`]. The message text of each check matches the pre-rebase typed
/// `ConfigError` variant exactly, so CLI stderr stays stable.
pub(crate) fn validate_chains(cfg: &RunConfig) -> Result<(), String> {
    let chains = &cfg.ext.chain;

    // The top-level env, re-typed once. A malformed env table is a hard validation failure here
    // (the generic layer accepted it opaquely).
    let top_env = crate::env_spec(&cfg.env).map_err(|e| format!("env: {e}"))?;

    validate_chain_labels_unique(chains)?;
    for decl in chains {
        validate_chain_kind_non_empty(decl)?;
        validate_chain_fields(decl)?;
        validate_gas_adjustment(decl)?;
    }

    for (name, profile) in &cfg.profiles {
        // The profile's effective env: its own merged override if present, else the top-level env.
        let effective_env = match &profile.common().env {
            Some(v) => crate::env_spec(v).map_err(|e| format!("env: {e}"))?,
            None => top_env.clone(),
        };
        validate_env_selection(name, &effective_env, chains)?;
        validate_env_targets(name, &effective_env, chains)?;
        validate_rpc_urls(name, chains, &effective_env)?;
    }

    Ok(())
}

fn validate_chain_labels_unique(chains: &[ChainDecl]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for decl in chains {
        if !seen.insert(decl.label.as_str()) {
            return Err(format!("duplicate chain label `{}`", decl.label));
        }
    }
    Ok(())
}

/// `kind` is non-empty: the framework resolves it to a `ChainKind` at run time (an unknown
/// non-empty kind is a framework-level error), but an empty string can never resolve to
/// anything, so this crate rejects it directly rather than deferring to the framework.
fn validate_chain_kind_non_empty(decl: &ChainDecl) -> Result<(), String> {
    if decl.kind.is_empty() {
        return Err(format!("chain `{}`: `kind` must not be empty", decl.label));
    }
    Ok(())
}

fn validate_chain_fields(decl: &ChainDecl) -> Result<(), String> {
    let missing = missing_required_fields(decl);
    if !missing.is_empty() {
        return Err(format!(
            "chain `{}` (kind `{}`) is missing required field(s): {}",
            decl.label,
            decl.kind,
            missing.join(", ")
        ));
    }
    Ok(())
}

/// `gas_adjustment` is finite and at least `1.0`.
///
/// The field scales an estimate up into the limit an op submits, so a value below `1.0` sets the
/// limit *under* the estimate, which is a guaranteed out-of-gas failure rather than a tunable
/// trade-off. That is rejected outright: a caller who deliberately wants a limit below the
/// estimate (say, to exercise the out-of-gas path) states it with an exact limit, which says so
/// directly instead of dressing it up as an estimate. `1.0` itself is allowed, meaning "take the
/// estimate as-is, with no headroom".
///
/// Non-finite values are rejected for the same reason they would be for any multiplier: `NaN`
/// compares false against every bound and would silently cast to `0`, and an infinite adjustment
/// saturates to the widest possible limit. Neither is a coherent request.
///
/// There is deliberately no upper bound. An adjustment far above `1.0` over-provisions, which is
/// wasteful but never broken (on EVM and Tron the unused headroom is not even charged), and
/// picking a ceiling would be inventing a policy nobody asked for.
fn validate_gas_adjustment(decl: &ChainDecl) -> Result<(), String> {
    match decl.gas_adjustment {
        Some(v) if !v.is_finite() || v < 1.0 => Err(format!(
            "chain `{}`: `gas_adjustment` must be a finite number >= 1.0 (got {v}); \
             below 1.0 the gas limit lands under the estimate, which always runs out of gas",
            decl.label
        )),
        _ => Ok(()),
    }
}

fn validate_env_selection(
    profile: &str,
    env: &EnvSpec,
    chains: &[ChainDecl],
) -> Result<(), String> {
    if chains.is_empty() {
        return Ok(());
    }
    if let Some(selected) = &env.chains {
        let labels: HashSet<&str> = chains.iter().map(|c| c.label.as_str()).collect();
        for label in selected {
            if !labels.contains(label.as_str()) {
                return Err(format!(
                    "profile `{profile}`: env.chains references unknown chain label `{label}`"
                ));
            }
        }
    }
    Ok(())
}

fn validate_env_targets(profile: &str, env: &EnvSpec, chains: &[ChainDecl]) -> Result<(), String> {
    if let Some(targets) = &env.targets {
        let labels: HashSet<&str> = chains.iter().map(|c| c.label.as_str()).collect();
        for label in targets.keys() {
            if !labels.contains(label.as_str()) {
                return Err(format!(
                    "profile `{profile}`: env.targets references unknown chain label `{label}`"
                ));
            }
        }
    }
    Ok(())
}

fn validate_rpc_urls(profile: &str, chains: &[ChainDecl], env: &EnvSpec) -> Result<(), String> {
    for decl in chains {
        let decl_target = match &decl.target {
            Some(s) => Some(parse_target_str(s).map_err(|message| {
                format!("chain `{}`: invalid `target` value: {message}", decl.label)
            })?),
            None => None,
        };
        let resolved =
            resolve_chain_target(&decl.label, decl_target, env, &TargetOverrides::default());
        if resolved == TargetStr::Rpc && decl.rpc_url.is_none() {
            return Err(format!(
                "profile `{profile}`: chain `{}` resolves to target `rpc` but has no `rpc_url`",
                decl.label
            ));
        }
    }
    Ok(())
}
