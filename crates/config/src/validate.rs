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
use crate::schema::{EnvSpec, Profile, SuitePhase, WorldSource};
use crate::target::{parse_target_str, resolve_chain_target, TargetOverrides};
use crate::{ChainDecl, ConfigError, RunConfig};
use std::collections::HashSet;

/// Loader stage between typed deserialize and structural validation: makes `Suite.phases` the
/// single source of truth and validates the pipeline structure.
///
/// Legacy `profiles = [a, b]` is normalized into fresh, dependency-free phases and cleared. A
/// suite that sets both `profiles` and `phases` is a hard error. After normalization every phase
/// rule in Part 3.1 is checked: `needs` may only name earlier phases, phase profiles are unique
/// per suite, `world = "inherit"` needs exactly one `needs` entry, both ends of an inherit
/// handoff must be single-setup, and a donor may feed at most one inheriting phase.
pub fn normalize_suite_phases(cfg: &mut RunConfig) -> Result<(), ConfigError> {
    // Pass 1: normalize legacy `profiles` into `phases` (mutates suites only).
    for (suite_name, suite) in &mut cfg.suites {
        match (suite.profiles.is_empty(), suite.phases.is_empty()) {
            (false, false) => {
                return Err(ConfigError::SuiteProfilesAndPhases {
                    suite: suite_name.clone(),
                });
            }
            (false, true) => {
                suite.phases = suite
                    .profiles
                    .iter()
                    .map(|profile| SuitePhase {
                        profile: profile.clone(),
                        needs: Vec::new(),
                        world: WorldSource::Fresh,
                    })
                    .collect();
                suite.profiles.clear();
            }
            _ => {}
        }
    }

    // Pass 2: validate the normalized phases (reads suites and profiles immutably).
    for (suite_name, suite) in &cfg.suites {
        validate_suite_phase_structure(suite_name, &suite.phases, cfg)?;
    }
    Ok(())
}

/// Whether a profile builds exactly one starting world (the requirement for either end of a
/// `world = "inherit"` handoff): `invariant`, `endurance`, `scenario`, or `fuzz` with a single
/// case. A multi-case fuzz fans out into many independent worlds, so it can neither donate nor
/// consume a single inherited world.
fn is_single_setup(profile: &Profile) -> bool {
    match profile {
        Profile::Fuzz(f) => f.cases == 1,
        Profile::Invariant(_) | Profile::Endurance(_) | Profile::Scenario(_) => true,
    }
}

fn validate_suite_phase_structure(
    suite_name: &str,
    phases: &[SuitePhase],
    cfg: &RunConfig,
) -> Result<(), ConfigError> {
    // Duplicate phase profiles, and the set of profiles declared before each phase.
    let mut seen: HashSet<&str> = HashSet::new();
    for phase in phases {
        if !seen.insert(phase.profile.as_str()) {
            return Err(ConfigError::DuplicatePhaseProfile {
                suite: suite_name.to_string(),
                profile: phase.profile.clone(),
            });
        }
    }

    // `needs` may only reference an earlier phase (declaration order is execution order). This
    // also rejects self and forward references.
    let mut earlier: HashSet<&str> = HashSet::new();
    for phase in phases {
        for needed in &phase.needs {
            if !earlier.contains(needed.as_str()) {
                return Err(ConfigError::PhaseNeedsNotEarlier {
                    suite: suite_name.to_string(),
                    phase: phase.profile.clone(),
                    needed: needed.clone(),
                });
            }
        }
        earlier.insert(phase.profile.as_str());
    }

    // `world = "inherit"` arity, single-setup ends, and shared-donor uniqueness.
    let mut donor_of: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for phase in phases {
        if phase.world != WorldSource::Inherit {
            continue;
        }
        if phase.needs.len() != 1 {
            return Err(ConfigError::PhaseInheritArity {
                suite: suite_name.to_string(),
                phase: phase.profile.clone(),
                needs: phase.needs.len(),
            });
        }
        let donor = phase.needs[0].as_str();

        // The inheriting phase must be single-setup. Skip when its profile is unknown; the
        // unknown-profile check in `validate_suites` reports that first.
        if let Some(p) = cfg.profiles.get(&phase.profile) {
            if !is_single_setup(p) {
                return Err(ConfigError::PhaseWorldNotSingleSetup {
                    suite: suite_name.to_string(),
                    phase: phase.profile.clone(),
                    role: "inheriting phase".to_string(),
                });
            }
        }
        // The donor must also be single-setup.
        if let Some(p) = cfg.profiles.get(donor) {
            if !is_single_setup(p) {
                return Err(ConfigError::PhaseWorldNotSingleSetup {
                    suite: suite_name.to_string(),
                    phase: donor.to_string(),
                    role: "donor".to_string(),
                });
            }
        }

        // At most one phase may inherit from a given donor.
        if let Some(first) = donor_of.insert(donor, phase.profile.as_str()) {
            return Err(ConfigError::SharedDonor {
                suite: suite_name.to_string(),
                donor: donor.to_string(),
                first: first.to_string(),
                second: phase.profile.clone(),
            });
        }
    }
    Ok(())
}

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

/// Every phase must name a profile that exists. Runs after [`normalize_suite_phases`], so
/// `Suite.phases` is populated (legacy `profiles` has already been folded into it).
fn validate_suites(cfg: &RunConfig) -> Result<(), ConfigError> {
    for (suite_name, suite) in &cfg.suites {
        for phase in &suite.phases {
            if !cfg.profiles.contains_key(&phase.profile) {
                return Err(ConfigError::UnknownSuiteProfile {
                    suite: suite_name.clone(),
                    profile: phase.profile.clone(),
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

fn validate_rpc_urls(
    profile: &str,
    chains: &[ChainDecl],
    env: &EnvSpec,
) -> Result<(), ConfigError> {
    for decl in chains {
        let decl_target =
            match &decl.target {
                Some(s) => Some(parse_target_str(s).map_err(|message| {
                    ConfigError::InvalidChainTarget {
                        label: decl.label.clone(),
                        message,
                    }
                })?),
                None => None,
            };
        let resolved =
            resolve_chain_target(&decl.label, decl_target, env, &TargetOverrides::default());
        if resolved == crate::TargetStr::Rpc && decl.rpc_url.is_none() {
            return Err(ConfigError::MissingRpcUrl {
                profile: profile.to_string(),
                label: decl.label.clone(),
            });
        }
    }
    Ok(())
}
