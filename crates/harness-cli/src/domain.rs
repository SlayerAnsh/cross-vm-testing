//! The domain seam: [`CliDomain`] lets a variant add config sections, CLI
//! flags, and its own setup request type; [`GenericDomain`] is the raw,
//! batteries-included implementation.

use crate::resolve::ResolvedProfile;

/// A boxed, pinned future returning the `(Ctx, World)` pair a config-driven
/// setup fn builds.
pub type SetupFuture<'a, C, W> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(C, W), harness_core::HarnessError>> + 'a>,
>;

/// Why a domain could not build its setup request.
#[derive(Debug)]
pub enum SetupBuildError {
    /// User/config mistake; maps to exit code 3.
    Usage(String),
    /// Environment/infrastructure problem; maps to exit code 2.
    Infra(String),
}

/// Domain hook bundle for the CLI: config extension, setup request type,
/// extra clap flags, and naming.
pub trait CliDomain: 'static {
    /// The config schema extension this domain loads with.
    type Ext: harness_config::ConfigExt;
    /// What registered setup fns receive.
    type Setup: 'static;
    /// Extra CLI flags flattened into the `run` and `replay` subcommands.
    type Args: clap::Args + Clone + core::fmt::Debug + Default + 'static;
    /// clap command name (e.g. "cross-vm").
    const BIN_NAME: &'static str;
    /// clap about line.
    const ABOUT: &'static str;
    /// Env var prefix: `{PREFIX}_PROFILE`, `{PREFIX}_SEED`, `{PREFIX}_CASES`,
    /// `{PREFIX}_OPS` are honored (plus `PROPTEST_CASES` for cases).
    const ENV_PREFIX: &'static str;
    /// Builds the domain setup request for one run (called once per fuzz case
    /// with that case's sub-seed).
    fn build_setup(
        cfg: &harness_config::RunConfig<Self::Ext>,
        resolved: &ResolvedProfile,
        args: &Self::Args,
        seed: u64,
    ) -> Result<Self::Setup, SetupBuildError>;
    /// Extra top-level sections for replay artifacts (e.g. cross-vm's
    /// `[[chain]]` blocks). Default: none.
    fn artifact_sections(
        cfg: &harness_config::RunConfig<Self::Ext>,
        resolved: &ResolvedProfile,
        args: &Self::Args,
    ) -> toml::Table {
        let _ = (cfg, resolved, args);
        toml::Table::new()
    }
    /// Domain flags to record in the JSON report's `invocation.overrides`
    /// object. Default: none.
    fn overrides_json(args: &Self::Args) -> serde_json::Map<String, serde_json::Value> {
        let _ = args;
        serde_json::Map::new()
    }
}

/// The batteries-included domain for using harness-cli raw: no extra config
/// sections, no extra flags, setup fns receive a [`BasicSetup`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GenericDomain;

/// Zero extra CLI flags.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct NoArgs {}

/// The setup request [`GenericDomain`] hands to setup fns.
#[derive(Debug, Clone)]
pub struct BasicSetup {
    /// The resolved profile name being run.
    pub profile: String,
    /// The run seed, already concrete (per-case for fuzz).
    pub seed: u64,
    /// The merged env table, verbatim (`{}` when the config declared none).
    pub env: serde_json::Value,
}

impl CliDomain for GenericDomain {
    type Ext = harness_config::NoExt;
    type Setup = BasicSetup;
    type Args = NoArgs;
    const BIN_NAME: &'static str = "harness";
    const ABOUT: &'static str = "Config-driven harness runner";
    const ENV_PREFIX: &'static str = "HARNESS";
    fn build_setup(
        _cfg: &harness_config::RunConfig<Self::Ext>,
        resolved: &ResolvedProfile,
        _args: &Self::Args,
        seed: u64,
    ) -> Result<Self::Setup, SetupBuildError> {
        Ok(BasicSetup {
            profile: resolved.name.clone(),
            seed,
            env: resolved.env.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_domain_builds_basic_setup_from_resolved_profile() {
        let resolved = crate::resolve::ResolvedProfile {
            name: "smoke".to_string(),
            profile: sample_profile(),
            seed: harness_config::SeedSpec::Fixed(7),
            env: serde_json::json!({"users": 2}),
            check_every: 1,
            stats: false,
            shrink: true,
            shrink_limit: 256,
            artifacts_dir: "target/harness".to_string(),
            json_report: None,
            world_source: harness_config::WorldSource::Fresh,
            stash_world: false,
            phase_params: None,
        };
        let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
            "[harness]\nname = \"h\"\n[profile.smoke]\nmode = \"fuzz\"\ncases = 1\nops = 1\n",
            &|_| None,
        )
        .expect("valid config");
        let setup = GenericDomain::build_setup(&cfg, &resolved, &NoArgs::default(), 42)
            .expect("generic build_setup is infallible");
        assert_eq!(setup.profile, "smoke");
        assert_eq!(setup.seed, 42);
        assert_eq!(setup.env["users"], 2);
    }

    fn sample_profile() -> harness_config::Profile {
        let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
            "[harness]\nname = \"h\"\n[profile.p]\nmode = \"fuzz\"\ncases = 1\nops = 1\n",
            &|_| None,
        )
        .expect("valid config");
        cfg.profiles["p"].clone()
    }
}
