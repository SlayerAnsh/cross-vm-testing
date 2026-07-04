//! [`resolve_profile`]: resolves a loaded [`RunConfig`](harness_config::RunConfig) plus a chosen
//! profile name plus CLI-shaped overrides into a runnable [`ResolvedProfile`].
//!
//! **CLI env folding is out of scope here.** Environment overrides (the "env override" precedence
//! tier) are the CLI layer's job: the CLI folds them into a [`RunOptions`] value *before* calling
//! [`resolve_profile`], so this function only ever sees "CLI flag, already-folded" vs. "profile
//! key" vs. "built-in default", which keeps it deterministic and unit-testable without touching
//! the process environment.

use harness_config::{ConfigExt, Profile, RunConfig, SeedSpec, WorldSource};
use harness_core::HarnessError;

/// CLI-shaped run overrides, applied over a profile's own keys by [`resolve_profile`].
///
/// Every field is `None` (or empty/`false`) by default, meaning "no override, fall through to
/// the profile / built-in default". Folding environment variables into this value is the CLI
/// layer's job, not this module's.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// `--seed N`: overrides the profile's [`SeedSpec`] with a fixed value.
    pub seed: Option<u64>,
    /// `--ops N`: overrides a fuzz/invariant/endurance profile's op count. Carried through for
    /// the run-driving layer to apply; `resolve_profile` does not consume it.
    pub ops: Option<usize>,
    /// `--cases N`: overrides a fuzz profile's case count. Carried through for the run-driving
    /// layer to apply; `resolve_profile` does not consume it.
    pub cases: Option<usize>,
    /// `--duration DUR`: overrides an endurance profile's wall-clock bound. Carried through for
    /// the run-driving layer to apply; `resolve_profile` does not consume it.
    pub duration: Option<std::time::Duration>,
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

/// A profile resolved into a runnable shape: every scalar key with CLI/profile/built-in
/// precedence already applied. The run-driving layer consumes this directly.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    /// The profile's name, as looked up in `cfg.profiles`.
    pub name: String,
    /// The typed mode payload (fuzz/invariant/endurance/scenario), verbatim from the loader;
    /// its own defaults are already merged, but `RunOptions.ops`/`cases`/`duration` overrides
    /// are not applied here (the run-driving layer applies them against `opts` directly).
    pub profile: harness_config::Profile,
    /// The resolved run seed: `RunOptions.seed` (folded to [`SeedSpec::Fixed`]) if set,
    /// otherwise the profile's own [`SeedSpec`]. Still unresolved to a concrete `u64` when
    /// [`SeedSpec::Random`]; that resolution (and the "reproduce with seed = N" line) happens
    /// per run, in the run-driving layer.
    pub seed: SeedSpec,
    /// The merged env table for this profile: the profile's own merged `env`
    /// key when present, else the top-level `[env]`, else `{}`.
    pub env: serde_json::Value,
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
    /// Where this run's starting `(Ctx, World)` comes from. `Fresh` (the default) calls the
    /// registered setup fn; `Inherit` takes the pair a donor phase stashed. Set only by the
    /// CLI's pipeline driver; `resolve_profile` always emits `Fresh`.
    pub world_source: harness_config::WorldSource,
    /// Whether a later phase inherits from this run: when true and the run passes, the final
    /// `(Ctx, World)` is stashed in the harness's session slot instead of being dropped.
    pub stash_world: bool,
    /// Per-phase params for the registered world patch fn. Set only by the CLI's pipeline
    /// driver; `resolve_profile` always emits `None`.
    pub phase_params: Option<toml::Table>,
}

/// Resolves `cfg`'s profile `name` against `opts` into a [`ResolvedProfile`].
///
/// Errors with [`HarnessError::Infra`] when `name` does not match any profile (the message lists
/// the available profile names, sorted).
pub fn resolve_profile<X: ConfigExt>(
    cfg: &RunConfig<X>,
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
        HarnessError::infra(format!(
            "unknown profile \"{name}\": available profiles: {available}"
        ))
    })?;

    let common = profile.common();

    // The loader already merged the profile's own `env` over the top-level
    // `[env]`; a profile with no `env` key falls back to the top-level table.
    let env: serde_json::Value = profile
        .common()
        .env
        .clone()
        .unwrap_or_else(|| cfg.env.clone());

    // Scalar precedence, highest first: RunOptions (CLI, already flag>env folded by the CLI
    // layer) > profile key (defaults already merged by the loader) > built-in default.
    let seed = opts.seed.map(SeedSpec::Fixed).unwrap_or(common.seed);
    let check_every = opts.check_every.unwrap_or(common.check_every);
    let stats = opts.stats.unwrap_or(common.stats);

    // Mode default per spec section 4.3: true for fuzz/invariant, false for endurance; scenario
    // is not generative (concrete steps), so it follows endurance's false default.
    let mode_shrink_default = matches!(profile, Profile::Fuzz(_) | Profile::Invariant(_));
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

    Ok(ResolvedProfile {
        name: name.to_string(),
        profile,
        seed,
        env,
        check_every,
        stats,
        shrink,
        shrink_limit,
        artifacts_dir,
        json_report,
        world_source: WorldSource::Fresh,
        stash_world: false,
        phase_params: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(toml: &str) -> RunConfig<harness_config::NoExt> {
        harness_config::from_toml_str::<harness_config::NoExt>(toml, &|_| None)
            .expect("valid fixture")
    }

    const BASE: &str = r#"
[harness]
name = "vault"

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
}
