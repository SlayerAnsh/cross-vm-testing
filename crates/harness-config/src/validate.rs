//! Stage 5 of the loader pipeline: structural validation over the fully typed [`RunConfig`],
//! run after typed deserialization and before the config is handed back to the caller.
//!
//! Only generic (domain-agnostic) checks live here: suite/phase structure and per-profile
//! mode-specific rules. Domain-specific validation (for example chain and env checks) runs
//! through [`ConfigExt::validate`], invoked at the end of [`validate`].
use crate::ext::ConfigExt;
use crate::schema::{Profile, SuitePhase, WorldSource};
use crate::{ConfigError, RunConfig};
use std::collections::HashSet;

/// Loader stage between typed deserialize and structural validation: makes `Suite.phases` the
/// single source of truth and validates the pipeline structure.
///
/// Legacy `profiles = [a, b]` is normalized into fresh, dependency-free phases and cleared. A
/// suite that sets both `profiles` and `phases` is a hard error. After normalization every phase
/// rule in Part 3.1 is checked: `needs` may only name earlier phases, phase profiles are unique
/// per suite, `world = "inherit"` needs exactly one `needs` entry, both ends of an inherit
/// handoff must be single-setup, and a donor may feed at most one inheriting phase.
pub(crate) fn normalize_suite_phases<X: ConfigExt>(
    cfg: &mut RunConfig<X>,
) -> Result<(), ConfigError> {
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
                        params: None,
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

fn validate_suite_phase_structure<X: ConfigExt>(
    suite_name: &str,
    phases: &[SuitePhase],
    cfg: &RunConfig<X>,
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

/// Runs every generic structural check against an already-parsed, merged, and typed
/// [`RunConfig`], then hands off to the domain extension's [`ConfigExt::validate`]. Returns the
/// first violation found as a hard [`ConfigError`].
pub(crate) fn validate<X: ConfigExt>(cfg: &RunConfig<X>) -> Result<(), ConfigError> {
    validate_suites(cfg)?;

    for (name, profile) in &cfg.profiles {
        validate_profile_mode_specific(name, profile)?;
    }

    X::validate(cfg)?;
    Ok(())
}

/// Every phase must name a profile that exists. Runs after [`normalize_suite_phases`], so
/// `Suite.phases` is populated (legacy `profiles` has already been folded into it).
fn validate_suites<X: ConfigExt>(cfg: &RunConfig<X>) -> Result<(), ConfigError> {
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
