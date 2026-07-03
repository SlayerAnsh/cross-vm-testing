//! [`resolve_chain_target`]: the single precedence funnel for mock-vs-rpc target resolution.
//!
//! Every other layer (load-time structural validation in `validate.rs`, CLI-time resolution,
//! and the replay artifact writer) calls this one pure function rather than re-deriving the
//! precedence order, so the rule lives in exactly one place.

use crate::schema::{EnvSpec, TargetStr};
use std::collections::BTreeMap;

/// CLI-supplied target overrides. Populated by the framework at CLI-parse time; empty (the
/// `Default`) at load time, since the config crate never sees CLI flags itself.
#[derive(Debug, Clone, Default)]
pub struct TargetOverrides {
    /// Per-chain override, from a repeatable `--target-chain LABEL=mock|rpc` flag.
    pub per_chain: BTreeMap<String, TargetStr>,
    /// The blanket `--target mock|rpc` flag, if given.
    pub cli_target: Option<TargetStr>,
}

/// Resolves one chain's effective target, highest precedence first:
///
/// 1. `overrides.per_chain[label]` (CLI `--target-chain LABEL=...`)
/// 2. `env.targets[label]` (the merged profile/top-level `targets` map)
/// 3. `decl_target` (`[[chain]].target`)
/// 4. `overrides.cli_target` (CLI `--target`)
/// 5. `env.target` (the merged `[env]`/profile `env.target`)
/// 6. [`TargetStr::Mock`] (the default)
///
/// `env` must already be the merged [`EnvSpec`] for the profile in question: profile `env`
/// already shallow-merged over the top-level `[env]`, and `targets` already merged label-wise.
/// This function does no merging itself; it only resolves precedence over already-resolved
/// inputs.
pub fn resolve_chain_target(
    label: &str,
    decl_target: Option<TargetStr>,
    env: &EnvSpec,
    overrides: &TargetOverrides,
) -> TargetStr {
    if let Some(t) = overrides.per_chain.get(label) {
        return *t;
    }
    if let Some(t) = env.targets.as_ref().and_then(|targets| targets.get(label)) {
        return *t;
    }
    if let Some(t) = decl_target {
        return t;
    }
    if let Some(t) = overrides.cli_target {
        return t;
    }
    if let Some(t) = env.target {
        return t;
    }
    TargetStr::Mock
}

/// Parses a raw `"mock"` | `"rpc"` string (as stored on `ChainDecl::target`) into a
/// [`TargetStr`]. Returns a descriptive error listing the valid values on any other input.
pub fn parse_target_str(s: &str) -> Result<TargetStr, String> {
    match s {
        "mock" => Ok(TargetStr::Mock),
        "rpc" => Ok(TargetStr::Rpc),
        other => Err(format!(
            "invalid target `{other}`, expected \"mock\" or \"rpc\""
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(target: Option<TargetStr>, targets: Option<&[(&str, TargetStr)]>) -> EnvSpec {
        EnvSpec {
            target,
            targets: targets.map(|pairs| pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()),
            chains: None,
            params: None,
        }
    }

    #[test]
    fn defaults_to_mock_when_nothing_set() {
        let env = env_with(None, None);
        let overrides = TargetOverrides::default();
        assert_eq!(
            resolve_chain_target("eth", None, &env, &overrides),
            TargetStr::Mock
        );
    }

    #[test]
    fn env_target_wins_over_default() {
        let env = env_with(Some(TargetStr::Rpc), None);
        let overrides = TargetOverrides::default();
        assert_eq!(
            resolve_chain_target("eth", None, &env, &overrides),
            TargetStr::Rpc
        );
    }

    #[test]
    fn cli_target_beats_env_target() {
        let env = env_with(Some(TargetStr::Mock), None);
        let overrides = TargetOverrides {
            per_chain: BTreeMap::new(),
            cli_target: Some(TargetStr::Rpc),
        };
        assert_eq!(
            resolve_chain_target("eth", None, &env, &overrides),
            TargetStr::Rpc
        );
    }

    #[test]
    fn decl_target_beats_cli_target() {
        let env = env_with(None, None);
        let overrides = TargetOverrides {
            per_chain: BTreeMap::new(),
            cli_target: Some(TargetStr::Rpc),
        };
        assert_eq!(
            resolve_chain_target("eth", Some(TargetStr::Mock), &env, &overrides),
            TargetStr::Mock
        );
    }

    #[test]
    fn env_targets_beats_decl_target() {
        let env = env_with(None, Some(&[("eth", TargetStr::Rpc)]));
        let overrides = TargetOverrides::default();
        assert_eq!(
            resolve_chain_target("eth", Some(TargetStr::Mock), &env, &overrides),
            TargetStr::Rpc
        );
    }

    #[test]
    fn per_chain_override_beats_env_targets() {
        let env = env_with(None, Some(&[("eth", TargetStr::Rpc)]));
        let mut per_chain = BTreeMap::new();
        per_chain.insert("eth".to_string(), TargetStr::Mock);
        let overrides = TargetOverrides {
            per_chain,
            cli_target: None,
        };
        assert_eq!(
            resolve_chain_target("eth", Some(TargetStr::Rpc), &env, &overrides),
            TargetStr::Mock
        );
    }

    #[test]
    fn full_precedence_matrix_each_level_wins_over_lower_levels() {
        // Set every level for "eth" simultaneously; only the highest-precedence value must win.
        let env = env_with(Some(TargetStr::Mock), Some(&[("eth", TargetStr::Mock)]));
        let mut per_chain = BTreeMap::new();
        per_chain.insert("eth".to_string(), TargetStr::Rpc);
        let overrides = TargetOverrides {
            per_chain,
            cli_target: Some(TargetStr::Mock),
        };
        assert_eq!(
            resolve_chain_target("eth", Some(TargetStr::Mock), &env, &overrides),
            TargetStr::Rpc,
            "per_chain override must win over everything else"
        );

        // Drop per_chain: env.targets should now win over decl_target, cli_target, env.target.
        let overrides = TargetOverrides {
            per_chain: BTreeMap::new(),
            cli_target: Some(TargetStr::Rpc),
        };
        let env2 = env_with(Some(TargetStr::Rpc), Some(&[("eth", TargetStr::Mock)]));
        assert_eq!(
            resolve_chain_target("eth", Some(TargetStr::Rpc), &env2, &overrides),
            TargetStr::Mock,
            "env.targets must win over decl_target/cli_target/env.target"
        );

        // Drop env.targets: decl_target should win over cli_target and env.target.
        let env3 = env_with(Some(TargetStr::Rpc), None);
        let overrides = TargetOverrides {
            per_chain: BTreeMap::new(),
            cli_target: Some(TargetStr::Rpc),
        };
        assert_eq!(
            resolve_chain_target("eth", Some(TargetStr::Mock), &env3, &overrides),
            TargetStr::Mock,
            "decl_target must win over cli_target/env.target"
        );

        // Drop decl_target: cli_target should win over env.target.
        let env4 = env_with(Some(TargetStr::Mock), None);
        let overrides = TargetOverrides {
            per_chain: BTreeMap::new(),
            cli_target: Some(TargetStr::Rpc),
        };
        assert_eq!(
            resolve_chain_target("eth", None, &env4, &overrides),
            TargetStr::Rpc,
            "cli_target must win over env.target"
        );

        // Drop everything but env.target.
        let env5 = env_with(Some(TargetStr::Rpc), None);
        let overrides = TargetOverrides::default();
        assert_eq!(
            resolve_chain_target("eth", None, &env5, &overrides),
            TargetStr::Rpc,
            "env.target must win over the mock default"
        );
    }

    #[test]
    fn parse_target_str_accepts_mock_and_rpc() {
        assert_eq!(parse_target_str("mock"), Ok(TargetStr::Mock));
        assert_eq!(parse_target_str("rpc"), Ok(TargetStr::Rpc));
    }

    #[test]
    fn parse_target_str_rejects_other_strings() {
        let err = parse_target_str("bogus").unwrap_err();
        assert!(err.contains("bogus"));
    }
}
