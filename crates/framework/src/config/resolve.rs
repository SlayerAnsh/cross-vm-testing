//! [`resolve_profile`]: resolves a loaded [`RunConfig`](cross_vm_config::RunConfig) plus a
//! chosen profile name plus CLI-shaped overrides into a runnable [`ResolvedProfile`].
//!
//! This is the framework's only caller of [`cross_vm_config::resolve_chain_target`]: every
//! chain's target, and the profile's own default target, funnel through that one function so
//! the precedence order (spec section 8) lives in exactly one place.
//!
//! **CLI env folding is out of scope here.** `CROSS_VM_SEED`, `CROSS_VM_PROFILE`, and friends
//! (spec section 8's "env override" precedence tier) are the CLI layer's job (Task 7): the CLI
//! folds them into a [`RunOptions`] value *before* calling [`resolve_profile`], so this function
//! only ever sees "CLI flag, already-folded" vs. "profile key" vs. "built-in default", which
//! keeps it deterministic and unit-testable without touching the process environment.

use std::collections::BTreeMap;

use cross_vm_config::{resolve_chain_target, RunConfig, SeedSpec, TargetOverrides, TargetStr};
use cross_vm_core::{ChainKind, CrossVmError};

use crate::harness::HarnessError;

use super::setup_request::{ChainSpecData, Target};

/// CLI-shaped run overrides, applied over a profile's own keys by [`resolve_profile`].
///
/// Every field is `None` (or empty/`false`) by default, meaning "no override, fall through to
/// the profile / built-in default". Folding `CROSS_VM_*` environment variables into this value
/// is the CLI layer's job (Task 7), not this module's.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// `--seed N`: overrides the profile's [`SeedSpec`] with a fixed value.
    pub seed: Option<u64>,
    /// `--ops N`: overrides a fuzz/invariant/endurance profile's op count. Carried through for
    /// the run-driving layer (Task 6) to apply; `resolve_profile` does not consume it.
    pub ops: Option<usize>,
    /// `--cases N`: overrides a fuzz profile's case count. Carried through for the run-driving
    /// layer (Task 6) to apply; `resolve_profile` does not consume it.
    pub cases: Option<usize>,
    /// `--duration DUR`: overrides an endurance profile's wall-clock bound. Carried through for
    /// the run-driving layer (Task 6) to apply; `resolve_profile` does not consume it.
    pub duration: Option<std::time::Duration>,
    /// `--target mock|rpc`: the blanket CLI target override.
    pub target: Option<Target>,
    /// `--target-chain LABEL=mock|rpc` (repeatable): per-chain CLI target overrides.
    pub target_chains: BTreeMap<String, Target>,
    /// `--stats`: enables run statistics collection.
    pub stats: Option<bool>,
    /// `--check-every N`: overrides the invariant sweep cadence.
    pub check_every: Option<usize>,
    /// `--json-report PATH`: overrides the JSON report output path.
    pub json_report: Option<String>,
    /// `--artifacts-dir DIR`: overrides the replay-artifact/report directory.
    pub artifacts_dir: Option<String>,
    /// `--no-shrink`: force-disables auto-shrink regardless of the profile's own `shrink` key
    /// or mode default.
    pub no_shrink: bool,
    /// Cooperative cancellation flag for an endurance run, checked at the top of the endurance
    /// driver's loop (never around an in-flight `apply`). The CLI wires this to a ctrl-c signal
    /// task; `resolve_profile` never reads it (it is consumed by the registry's endurance run
    /// arm). `None` (the default) means an endurance run never stops early by signal.
    pub stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// A profile resolved into a runnable shape: selection-filtered chain specs (with `kind`/`target`
/// resolved; `spec_id`/`commitment` carried as raw name strings, validated later by `build_chain`)
/// plus every scalar key with CLI/profile/built-in precedence already applied. Task 6 (the
/// registry/run-driving layer) consumes this directly.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    /// The profile's name, as looked up in `cfg.profiles`.
    pub name: String,
    /// The typed mode payload (fuzz/invariant/endurance/scenario), verbatim from the loader;
    /// its own defaults are already merged, but `RunOptions.ops`/`cases`/`duration` overrides
    /// are not applied here (the run-driving layer applies them against `opts` directly).
    pub profile: cross_vm_config::Profile,
    /// The resolved run seed: `RunOptions.seed` (folded to [`SeedSpec::Fixed`]) if set,
    /// otherwise the profile's own [`SeedSpec`]. Still unresolved to a concrete `u64` when
    /// [`SeedSpec::Random`]; that resolution (and the "reproduce with seed = N" line) happens
    /// per run, in the run-driving layer.
    pub seed: SeedSpec,
    /// Selection-filtered `[[chain]]` declarations, each with `kind` parsed and `target` resolved
    /// through [`resolve_chain_target`]; `spec_id`/`commitment` are carried as raw name strings and
    /// validated in `build_chain`. Empty when the config file declares no `[[chain]]` entries (the
    /// setup fn hard codes its own chains).
    pub chain_specs: Vec<ChainSpecData>,
    /// The profile's own resolved default target (used when `chain_specs` is empty).
    pub target: Target,
    /// The merged `[env.params]` table (profile override already merged over the top-level
    /// `[env]` by the loader).
    pub params: toml::Table,
    /// Invariant sweep cadence, `RunOptions.check_every` over the profile's own key.
    pub check_every: usize,
    /// Whether to collect run statistics, `RunOptions.stats` over the profile's own key.
    pub stats: bool,
    /// Whether to auto-shrink a failing history: the profile's own `shrink` key when set,
    /// otherwise the mode default (`true` for fuzz/invariant, `false` for endurance/scenario);
    /// `RunOptions.no_shrink` forces this to `false` regardless.
    pub shrink: bool,
    /// Shrink replay budget, from the profile's own key (no CLI override exists for this key).
    pub shrink_limit: usize,
    /// Replay-artifact/report directory, `RunOptions.artifacts_dir` over the profile's own key.
    pub artifacts_dir: String,
    /// JSON report output path, `RunOptions.json_report` over the profile's own key.
    pub json_report: Option<String>,
}

/// Per-kind `native_symbol` default (spec section 4.6), applied here when a `[[chain]]`
/// declaration omits (or blanks) the field.
fn default_native_symbol(kind: ChainKind) -> &'static str {
    match kind {
        ChainKind::CosmWasm => "OSMO",
        ChainKind::Evm => "ETH",
        ChainKind::Svm => "SOL",
        ChainKind::Tron => "TRX",
    }
}

/// Converts the framework's [`Target`] back into the config crate's [`TargetStr`], the shape
/// [`resolve_chain_target`] and [`TargetOverrides`] need.
fn target_to_str(t: Target) -> TargetStr {
    match t {
        Target::Mock => TargetStr::Mock,
        Target::Rpc => TargetStr::Rpc,
    }
}

/// Resolves `cfg`'s profile `name` against `opts` into a [`ResolvedProfile`].
///
/// Errors (all [`HarnessError::Infra`]) when: `name` does not match any profile (lists the
/// available names), a chain's `kind`/`target` string fails to parse, or a chain resolves to the
/// `rpc` target with no `rpc_url` (re-asserted here since interpolation and target resolution both
/// happen after the config crate's own load-time validation). A bad `spec_id`/`commitment` string
/// is not caught here: it is validated later, in `build_chain`'s VM-crate-gated arms.
pub fn resolve_profile(
    cfg: &RunConfig,
    name: &str,
    opts: &RunOptions,
) -> Result<ResolvedProfile, HarnessError> {
    let profile = cfg.profiles.get(name).cloned().ok_or_else(|| {
        let mut names: Vec<&str> = cfg.profiles.keys().map(String::as_str).collect();
        names.sort_unstable();
        let available = if names.is_empty() {
            "<none>".to_string()
        } else {
            names.join(", ")
        };
        HarnessError::infra(CrossVmError::Other {
            kind: ChainKind::Evm,
            reason: format!("unknown profile \"{name}\": available profiles: {available}"),
        })
    })?;

    let common = profile.common();
    let merged_env = common.env.clone().unwrap_or_else(|| cfg.env.clone());

    let overrides = TargetOverrides {
        per_chain: opts
            .target_chains
            .iter()
            .map(|(label, target)| (label.clone(), target_to_str(*target)))
            .collect(),
        cli_target: opts.target.map(target_to_str),
    };

    // Chain selection: `env.chains` (non-empty) filters `cfg.chains` to that label subset;
    // omitted or empty means every declared chain.
    let selected = cfg.chains.iter().filter(|decl| match &merged_env.chains {
        Some(labels) if !labels.is_empty() => labels.contains(&decl.label),
        _ => true,
    });

    let mut chain_specs = Vec::new();
    for decl in selected {
        let kind: ChainKind = decl.kind.parse().map_err(|e| {
            HarnessError::infra(CrossVmError::Other {
                kind: ChainKind::Evm,
                reason: format!("chain `{}`: {e}", decl.label),
            })
        })?;

        let decl_target = decl
            .target
            .as_deref()
            .map(cross_vm_config::parse_target_str)
            .transpose()
            .map_err(|e| {
                HarnessError::infra(CrossVmError::Other {
                    kind,
                    reason: format!("chain `{}`: {e}", decl.label),
                })
            })?;

        let target_str = resolve_chain_target(&decl.label, decl_target, &merged_env, &overrides);
        let target = Target::from(target_str);

        // `spec_id`/`commitment` are carried through as raw NAME strings; they are validated and
        // parsed into their VM-crate enums inside `build_chain`'s per-kind `#[cfg]`-gated arms, so
        // this module never references a VM-specific crate (that keeps `--features cli` composable
        // with any subset of {cw,evm,solana,tron}).
        let spec_id = decl.spec_id.clone();
        let commitment = decl.commitment.clone();

        let name_field = decl.name.clone().unwrap_or_else(|| decl.label.clone());
        let native_symbol = decl
            .native_symbol
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_native_symbol(kind).to_string());

        let rpc_url = decl.rpc_url.clone();
        if target == Target::Rpc && rpc_url.is_none() {
            return Err(HarnessError::infra(CrossVmError::Other {
                kind,
                reason: format!(
                    "chain `{}` resolves to target `rpc` but has no rpc_url",
                    decl.label
                ),
            }));
        }

        chain_specs.push(ChainSpecData {
            label: decl.label.clone(),
            kind,
            chain_id: decl.chain_id.clone(),
            name: name_field,
            native_symbol,
            rpc_url,
            target,
            params: decl.params.clone().unwrap_or_default(),
            bech32_prefix: decl.bech32_prefix.clone(),
            native_denom: decl.native_denom.clone(),
            gas_price: decl.gas_price,
            spec_id,
            ws_url: decl.ws_url.clone(),
            commitment,
        });
    }

    // Scalar precedence, highest first: RunOptions (CLI, already flag>env folded by the CLI
    // layer) > profile key (defaults already merged by the loader) > built-in default.
    let seed = opts.seed.map(SeedSpec::Fixed).unwrap_or(common.seed);
    let check_every = opts.check_every.unwrap_or(common.check_every);
    let stats = opts.stats.unwrap_or(common.stats);

    // Mode default per spec section 4.3: true for fuzz/invariant, false for endurance; scenario
    // is not generative (concrete steps), so it follows endurance's false default.
    let mode_shrink_default = matches!(
        profile,
        cross_vm_config::Profile::Fuzz(_) | cross_vm_config::Profile::Invariant(_)
    );
    let shrink = if opts.no_shrink {
        false
    } else {
        common.shrink.unwrap_or(mode_shrink_default)
    };

    let shrink_limit = common.shrink_limit;
    let artifacts_dir = opts
        .artifacts_dir
        .clone()
        .unwrap_or_else(|| common.artifacts_dir.clone());
    let json_report = opts
        .json_report
        .clone()
        .or_else(|| common.json_report.clone());

    // The profile's own default target: same precedence funnel, but with no specific chain
    // label in scope, so `overrides.per_chain` (label-keyed) cannot apply and `decl_target` is
    // `None`; only `overrides.cli_target` / `env.target` / the `Mock` default participate. This
    // mirrors `SetupRequest.target` (spec section 6.2): the fallback a setup fn uses when
    // `chain_specs` is empty and it hard codes its own chains.
    let default_overrides = TargetOverrides {
        per_chain: BTreeMap::new(),
        cli_target: overrides.cli_target,
    };
    let target = Target::from(resolve_chain_target(
        "",
        None,
        &merged_env,
        &default_overrides,
    ));

    let params = merged_env.params.clone().unwrap_or_default();

    Ok(ResolvedProfile {
        name: name.to_string(),
        profile,
        seed,
        chain_specs,
        target,
        params,
        check_every,
        stats,
        shrink,
        shrink_limit,
        artifacts_dir,
        json_report,
    })
}

#[cfg(all(test, feature = "cli"))]
mod tests {
    use super::*;

    fn load(toml: &str) -> RunConfig {
        cross_vm_config::from_toml_str(toml, &|_| None).expect("valid fixture")
    }

    const BASE: &str = r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#;

    #[test]
    fn unknown_profile_lists_available_names() {
        let cfg = load(BASE);
        let err = resolve_profile(&cfg, "nope", &RunOptions::default()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nope"));
        assert!(msg.contains("smoke"));
    }

    #[test]
    fn no_chain_declarations_yields_empty_chain_specs() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let resolved = resolve_profile(&cfg, "smoke", &RunOptions::default()).unwrap();
        assert!(resolved.chain_specs.is_empty());
    }

    #[test]
    fn cli_target_chain_beats_env_targets_beats_decl_target() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"
target = "mock"
rpc_url = "http://localhost:8545"

[env]
targets = { eth = "mock" }

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let mut opts = RunOptions::default();
        opts.target_chains.insert("eth".to_string(), Target::Rpc);
        let resolved = resolve_profile(&cfg, "smoke", &opts).unwrap();
        assert_eq!(resolved.chain_specs.len(), 1);
        assert_eq!(resolved.chain_specs[0].target, Target::Rpc);
    }

    #[test]
    fn rpc_target_without_rpc_url_errors() {
        // No `target`/`rpc_url` on the declaration, so this loads fine (the config crate's own
        // load-time check resolves targets with no CLI overrides, i.e. stays `mock`). Forcing
        // `rpc` via a CLI override at `resolve_profile` time is exactly the case the framework
        // must re-assert `rpc_url` for, since the config crate never sees CLI flags.
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let opts = RunOptions {
            target: Some(Target::Rpc),
            ..Default::default()
        };
        let err = resolve_profile(&cfg, "smoke", &opts).unwrap_err();
        assert!(err.to_string().contains("rpc_url"));
    }

    #[test]
    fn cli_seed_overrides_profile_seed() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
seed = 7
"#,
        );
        let resolved = resolve_profile(&cfg, "smoke", &RunOptions::default()).unwrap();
        assert_eq!(resolved.seed, SeedSpec::Fixed(7));

        let opts = RunOptions {
            seed: Some(42),
            ..Default::default()
        };
        let resolved = resolve_profile(&cfg, "smoke", &opts).unwrap();
        assert_eq!(resolved.seed, SeedSpec::Fixed(42));
    }

    #[test]
    fn chain_selection_filters_by_env_chains() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[[chain]]
label = "osmosis"
kind = "cosmwasm"
chain_id = "osmosis-1"
bech32_prefix = "osmo"
native_denom = "uosmo"

[env]
chains = ["eth"]

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let resolved = resolve_profile(&cfg, "smoke", &RunOptions::default()).unwrap();
        assert_eq!(resolved.chain_specs.len(), 1);
        assert_eq!(resolved.chain_specs[0].label, "eth");
    }
}
