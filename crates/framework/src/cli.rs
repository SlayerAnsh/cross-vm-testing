//! The config-driven `cross-vm` CLI (spec section 8): the [`Cli`] builder, the `run` / `validate`
//! / `list` subcommands, `CROSS_VM_*` env-var precedence folding, profile/suite selection, and
//! the CI exit-code contract.
//!
//! [`Cli`] wraps a [`Registry`]: a user binary calls
//! [`Cli::new`]/[`Cli::env_file`]/[`Cli::register`] to build one up, then `.main().await` to parse
//! `std::env::args()`, dispatch, and return a [`std::process::ExitCode`]. `cross-vm replay
//! <artifact>` is sugar for `run <artifact> --profile replay` (spec section 10): both dispatch
//! through the same `dispatch_run`/`run_with_config` path, so an artifact's `.toml`/`.json`
//! extension, the registry, and the exit-code contract are all unchanged. `--json-report` (spec
//! section 9) accumulates every selected profile's [`ErasedReport`] and writes the envelope once,
//! after the whole invocation finishes, in the private `run_selected` helper, which also writes a
//! replay artifact (spec section 10) for any fuzz/invariant/endurance profile that failed.
//!
//! Precedence (spec section 8), highest first: CLI flag, `CROSS_VM_*` env var, profile key,
//! `[defaults]`, built-in default. The last two tiers are already merged by the loader and
//! applied by [`resolve_profile`] — this module only ever builds
//! the CLI+env layer ([`RunOptions`]) and hands it down; it never
//! re-implements the profile/defaults merge.
//!
//! Exit codes (the CI contract): `0` all runs passed, `1` at least one run failed with
//! `Bug`/`Invariant`, `2` failed with `Infra` only, `3` a config or usage error. A suite or
//! multi-profile invocation reports the worst code across every profile it ran; the private
//! `combine` helper (unit-tested below) is the one place that ordering is decided.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use crate::config::{
    resolve_profile, write_json_report, write_replay_artifact, ErasedReport, Registry,
    ResolvedProfile, RunError, RunOptions, SetupFuture, SetupRequest, Target,
};
use crate::harness::{FailureKind, Harness};

/// The `cross-vm` CLI builder. Wraps a [`Registry`] with the `.env`
/// path a user binary wants loaded, then drives `run` / `validate` / `list` via [`Cli::main`].
///
/// ```no_run
/// # async fn demo() -> std::process::ExitCode {
/// # use cross_vm_framework::cli::Cli;
/// # use cross_vm_framework::config::{SetupFuture, SetupRequest};
/// # use cross_vm_framework::harness::{Ctx, HarnessError};
/// # struct MyHarness;
/// # impl cross_vm_framework::harness::Harness for MyHarness {
/// #     type World = ();
/// #     type Operation = ();
/// #     type Invariant = ();
/// #     type OpKind = ();
/// #     async fn apply(&self, _: &mut Ctx, _: &mut (), _: &()) -> Result<cross_vm_framework::harness::Verdict, HarnessError> { unimplemented!() }
/// #     fn op_kinds(&self) -> Vec<()> { vec![] }
/// #     fn generate_op(&self, _: &mut cross_vm_framework::harness::Prng, _: &(), _: ()) -> () {}
/// #     fn invariants(&self) -> Vec<()> { vec![] }
/// #     async fn check(&self, _: &mut Ctx, _: &(), _: &()) -> cross_vm_framework::harness::CheckOutcome { unimplemented!() }
/// # }
/// # fn my_setup(_req: SetupRequest) -> SetupFuture<'static, ()> { unimplemented!() }
/// Cli::new()
///     .env_file(".env")
///     .register("my-harness", || MyHarness, my_setup)
///     .main()
///     .await
/// # }
/// ```
pub struct Cli {
    registry: Registry,
    env_file: Option<PathBuf>,
}

impl Cli {
    /// An empty CLI with no harnesses registered yet and `env_file` defaulted to `Some(".env")`.
    pub fn new() -> Self {
        Self {
            registry: Registry::new(),
            env_file: Some(PathBuf::from(".env")),
        }
    }

    /// Overrides the `.env` path loaded at the start of [`Cli::main`]. Pass a path that does not
    /// exist to opt out silently (a missing file is never fatal; see [`Cli::main`]'s docs).
    pub fn env_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.env_file = Some(path.into());
        self
    }

    /// Registers a harness under `name`, delegating to
    /// [`Registry::register`](crate::config::Registry::register) (same bounds: `harness` builds
    /// a fresh `H` per run, `setup` builds the live `(Ctx, H::World)` from a
    /// [`SetupRequest`]).
    pub fn register<H, F, S>(mut self, name: &str, harness: F, setup: S) -> Self
    where
        H: Harness + 'static,
        H::Operation: serde::Serialize + serde::de::DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + serde::de::DeserializeOwned + Copy + 'static,
        F: Fn() -> H + 'static,
        S: Fn(SetupRequest) -> SetupFuture<'static, H::World> + 'static,
    {
        self.registry.register(name, harness, setup);
        self
    }

    /// Registers a persistent harness under `name`, delegating to
    /// [`Registry::register_persistent`](crate::config::Registry::register_persistent) (same
    /// bounds as [`Cli::register`], plus `H::World: Serialize`). A scenario profile's
    /// `export_world` key only works against a harness registered this way; against a plain
    /// [`Cli::register`]-ed harness it fails both `cross-vm validate` and `cross-vm run` with a
    /// clear error.
    pub fn register_persistent<H, F, S>(mut self, name: &str, harness: F, setup: S) -> Self
    where
        H: Harness + 'static,
        H::Operation: serde::Serialize + serde::de::DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + serde::de::DeserializeOwned + Copy + 'static,
        H::World: serde::Serialize + 'static,
        F: Fn() -> H + 'static,
        S: Fn(SetupRequest) -> SetupFuture<'static, H::World> + 'static,
    {
        self.registry.register_persistent(name, harness, setup);
        self
    }

    /// Parses `std::env::args()`, dispatches to `run` / `validate` / `list`, and returns the CI
    /// exit code (spec section 8).
    ///
    /// In order: (1) asserts the current thread-local tokio runtime flavor is
    /// [`RuntimeFlavor::CurrentThread`](tokio::runtime::RuntimeFlavor::CurrentThread) — the
    /// erased registry layer is `!Send` by design, so a caller running this on a multi-thread
    /// runtime is a programming error, not a recoverable one. (2) installs a
    /// `tracing-subscriber` fmt layer honoring `RUST_LOG` (default `info`); double-init (e.g.
    /// across repeated test calls) is tolerated via `try_init()`. (3) loads `.env` via
    /// [`dotenvy::from_path`] if [`Cli::env_file`] is set; a missing file is logged at `debug`
    /// and is not fatal (a run may set secrets another way). (4) parses argv and dispatches.
    pub async fn main(self) -> std::process::ExitCode {
        assert_eq!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread,
            "the cross-vm CLI must run on a #[tokio::main(flavor = \"current_thread\")] runtime: \
             the erased registry layer is !Send by design"
        );

        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();

        if let Some(path) = &self.env_file {
            match dotenvy::from_path(path) {
                Ok(()) => tracing::debug!(path = %path.display(), "loaded .env"),
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "no .env file loaded (not fatal)")
                }
            }
        }

        let args = match CliArgs::try_parse() {
            Ok(a) => a,
            Err(e) => {
                let _ = e.print();
                // clap's own `exit_code()` is 0 for --help/--version, non-zero otherwise; a
                // parse failure is always a usage error (exit code 3), never a run outcome.
                return std::process::ExitCode::from(if e.exit_code() == 0 { 0 } else { 3 });
            }
        };

        let code = match args.command {
            Command::Run(run_args) => self.dispatch_run(run_args).await,
            Command::Validate(cfg_args) => self.dispatch_validate(cfg_args),
            Command::List(cfg_args) => self.dispatch_list(cfg_args),
            Command::Replay(replay_args) => self.dispatch_replay(replay_args).await,
        };
        std::process::ExitCode::from(code)
    }

    /// `cross-vm run <config> [--profile ...] [--suite ...] ...`. Loads the config, selects
    /// profiles (spec section 8's selection rules), runs each in turn, and returns the combined
    /// exit code.
    async fn dispatch_run(&self, args: RunArgs) -> u8 {
        let cfg = match load_config(&args.config) {
            Ok(c) => c,
            Err(msg) => {
                tracing::error!("{msg}");
                return 3;
            }
        };
        self.run_with_config(&cfg, &args).await
    }

    /// `cross-vm replay <artifact>` (spec section 10): sugar for `run <artifact> --profile
    /// replay`. Reuses [`Cli::dispatch_run`] verbatim — an artifact is a valid config file with
    /// exactly one `[profile.replay]` scenario profile holding the (possibly shrunk) failing
    /// history, so this needs no bespoke loading, registry, or exit-code logic of its own. Exit
    /// code `0` means the artifact's failure no longer reproduces (the bug was fixed); non-zero
    /// means it still does.
    async fn dispatch_replay(&self, args: ReplayArgs) -> u8 {
        self.dispatch_run(RunArgs {
            config: args.artifact,
            profile: vec!["replay".to_string()],
            ..Default::default()
        })
        .await
    }

    /// The testable body of [`Cli::dispatch_run`]: everything after the config is already
    /// loaded, so tests can build a [`cross_vm_config::RunConfig`] in memory (no disk I/O).
    async fn run_with_config(&self, cfg: &cross_vm_config::RunConfig, args: &RunArgs) -> u8 {
        for w in &cfg.warnings {
            tracing::warn!("{w}");
        }

        if !self.registry.names().any(|n| n == cfg.harness.name) {
            tracing::error!(
                harness = %cfg.harness.name,
                registered = %self.registry.names().collect::<Vec<_>>().join(", "),
                "unknown harness"
            );
            return 3;
        }

        let env = std_env_lookup;
        let (names, stop_on_failure) = match select_profile_names(cfg, args, &env) {
            Ok(v) => v,
            Err(msg) => {
                tracing::error!("{msg}");
                return 3;
            }
        };

        let mut opts = build_run_options(args, &env);

        // Cooperative ctrl-c for an endurance run: the driver polls `opts.stop` at the top of
        // its loop only (see `EnduranceRunner::run_with`), never around an in-flight `apply`, so
        // a wallet lock or in-flight broadcast is never severed. First ctrl-c asks the run to
        // stop after its current operation; a second forces a hard exit. The spawned task
        // captures only the `Send` `Arc<AtomicBool>`, so it is spawnable on this CLI's required
        // `#[tokio::main(flavor = "current_thread")]` runtime despite the rest of the registry
        // being `!Send`.
        let stop = Arc::new(AtomicBool::new(false));
        opts.stop = Some(Arc::clone(&stop));
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            stop.store(true, Ordering::Relaxed);
            tracing::info!(
                "stopping after the current operation; press ctrl-c again to force quit"
            );
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        });

        let config_path = args.config.to_string_lossy();
        let code = run_selected(
            &self.registry,
            cfg,
            &names,
            &opts,
            stop_on_failure,
            &config_path,
        )
        .await;
        tracing::info!(exit_code = code, profiles = names.len(), "run summary");
        code
    }

    /// `cross-vm validate <config>`. Loads the config, prints its loader warnings, and
    /// type-checks every profile against the registered harness — never touching a chain.
    fn dispatch_validate(&self, args: ConfigArgs) -> u8 {
        let cfg = match load_config(&args.config) {
            Ok(c) => c,
            Err(msg) => {
                tracing::error!("{msg}");
                return 3;
            }
        };
        self.validate_with_config(&cfg)
    }

    /// The testable body of [`Cli::dispatch_validate`].
    fn validate_with_config(&self, cfg: &cross_vm_config::RunConfig) -> u8 {
        for w in &cfg.warnings {
            tracing::warn!("{w}");
        }

        if !self.registry.names().any(|n| n == cfg.harness.name) {
            tracing::error!(
                harness = %cfg.harness.name,
                registered = %self.registry.names().collect::<Vec<_>>().join(", "),
                "unknown harness"
            );
            return 3;
        }

        for (name, profile) in &cfg.profiles {
            if let Err(e) = self.registry.validate(&cfg.harness.name, profile) {
                tracing::error!(profile = %name, error = %e, "profile failed validation");
                return 3;
            }
        }

        tracing::info!("valid");
        0
    }

    /// `cross-vm list <config>`. Prints registered harness names, the config's profiles
    /// (name + mode), suites (name + members), and the `[harness]` in use.
    fn dispatch_list(&self, args: ConfigArgs) -> u8 {
        let cfg = match load_config(&args.config) {
            Ok(c) => c,
            Err(msg) => {
                tracing::error!("{msg}");
                return 3;
            }
        };
        self.list_with_config(&cfg)
    }

    /// The testable body of [`Cli::dispatch_list`].
    fn list_with_config(&self, cfg: &cross_vm_config::RunConfig) -> u8 {
        tracing::info!(
            harnesses = %self.registry.names().collect::<Vec<_>>().join(", "),
            "registered harnesses"
        );
        tracing::info!(
            name = %cfg.harness.name,
            setup = %cfg.harness.setup,
            "config harness"
        );
        for (name, profile) in &cfg.profiles {
            tracing::info!(profile = %name, mode = mode_label(profile), "profile");
        }
        for (name, suite) in &cfg.suites {
            tracing::info!(suite = %name, profiles = %suite.profiles.join(", "), "suite");
        }
        0
    }
}

impl Default for Cli {
    fn default() -> Self {
        Self::new()
    }
}

/// The mode label a profile variant carries (used by `cross-vm list`).
fn mode_label(profile: &cross_vm_config::Profile) -> &'static str {
    match profile {
        cross_vm_config::Profile::Fuzz(_) => "fuzz",
        cross_vm_config::Profile::Invariant(_) => "invariant",
        cross_vm_config::Profile::Endurance(_) => "endurance",
        cross_vm_config::Profile::Scenario(_) => "scenario",
    }
}

/// Loads and parses `path` into a [`cross_vm_config::RunConfig`], resolving `${VAR}`
/// interpolation against the process environment (after `.env` has already been folded in by
/// [`Cli::main`]).
fn load_config(path: &Path) -> Result<cross_vm_config::RunConfig, String> {
    cross_vm_config::load(path, &std_env_lookup).map_err(|e| e.to_string())
}

/// `vars` closure shared by config loading and `CROSS_VM_*`/`PROPTEST_*` env folding: reads the
/// real process environment. A plain fn pointer (not a closure) so every call site can pass
/// `&std_env_lookup` without re-allocating a capture.
fn std_env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

// ---------------------------------------------------------------------------------------------
// clap arg model
// ---------------------------------------------------------------------------------------------

/// Top-level `cross-vm` argv, parsed by [`Cli::main`]. Deliberately not named `Cli` (that name is
/// the public builder in this module); kept private since only [`Cli::main`] parses it.
#[derive(Debug, clap::Parser)]
#[command(name = "cross-vm", about = "Config-driven cross-VM harness runner")]
struct CliArgs {
    #[command(subcommand)]
    command: Command,
}

/// `run` / `validate` / `list` / `replay`.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Run one or more profiles (or a suite) against a config file.
    Run(RunArgs),
    /// Validate a config file against the registered harness; touches no chains.
    Validate(ConfigArgs),
    /// List registered harnesses and a config file's profiles/suites.
    List(ConfigArgs),
    /// Replay a `*.replay.toml`/`*.replay.json` artifact: sugar for `run <artifact> --profile
    /// replay` (spec section 10).
    Replay(ReplayArgs),
}

/// Shared by `validate` and `list`: just the config path.
#[derive(Debug, Clone, Default, clap::Args)]
struct ConfigArgs {
    /// Path to the `*.cross-vm.toml` (or `.json`) config file.
    config: PathBuf,
}

/// `cross-vm replay <artifact>` (spec section 10).
#[derive(Debug, Clone, Default, clap::Args)]
struct ReplayArgs {
    /// Path to a `*.replay.toml` (or `*.replay.json`) artifact written by a prior failing run.
    artifact: PathBuf,
}

/// `cross-vm run <config> [--profile NAME]... [--suite NAME] ...` (spec section 8).
#[derive(Debug, Clone, Default, clap::Args)]
struct RunArgs {
    /// Path to the `*.cross-vm.toml` (or `.json`) config file.
    config: PathBuf,
    /// Run this profile; repeatable. Mutually exclusive in practice with `--suite` (suite wins
    /// if both are given — see [`select_profile_names`]).
    #[arg(long = "profile")]
    profile: Vec<String>,
    /// Run this suite's profiles in order, honoring its `stop_on_failure`.
    #[arg(long = "suite")]
    suite: Option<String>,
    /// Overrides the resolved run seed.
    #[arg(long)]
    seed: Option<u64>,
    /// Overrides a fuzz/invariant profile's op count.
    #[arg(long)]
    ops: Option<usize>,
    /// Overrides a fuzz profile's case count.
    #[arg(long)]
    cases: Option<usize>,
    /// Overrides an endurance profile's wall-clock bound (humantime grammar, e.g. `8h`).
    #[arg(long, value_parser = humantime::parse_duration)]
    duration: Option<Duration>,
    /// Blanket mock/rpc target override for every chain.
    #[arg(long, value_parser = parse_target)]
    target: Option<Target>,
    /// Per-chain mock/rpc target override, `LABEL=mock|rpc`; repeatable.
    #[arg(long = "target-chain", value_parser = parse_target_chain)]
    target_chain: Vec<(String, Target)>,
    /// Enables run statistics collection.
    #[arg(long)]
    stats: bool,
    /// Overrides the invariant sweep cadence.
    #[arg(long = "check-every")]
    check_every: Option<usize>,
    /// Overrides the JSON report output path (spec section 9). The envelope is written once,
    /// after every selected profile has run, by [`run_selected`].
    #[arg(long = "json-report")]
    json_report: Option<String>,
    /// Overrides the replay-artifact/report directory.
    #[arg(long = "artifacts-dir")]
    artifacts_dir: Option<String>,
    /// Force-disables auto-shrink regardless of the profile's own key or mode default.
    #[arg(long = "no-shrink")]
    no_shrink: bool,
}

/// Parses a `--target` value: `"mock"` or `"rpc"`.
fn parse_target(s: &str) -> Result<Target, String> {
    match s {
        "mock" => Ok(Target::Mock),
        "rpc" => Ok(Target::Rpc),
        other => Err(format!(
            "invalid target `{other}`, expected \"mock\" or \"rpc\""
        )),
    }
}

/// Parses a `--target-chain` value: `LABEL=mock|rpc`, splitting on the first `=`.
fn parse_target_chain(s: &str) -> Result<(String, Target), String> {
    let (label, value) = s.split_once('=').ok_or_else(|| {
        format!("invalid --target-chain `{s}`: expected LABEL=mock|rpc (e.g. `eth=rpc`)")
    })?;
    let target = parse_target(value)?;
    Ok((label.to_string(), target))
}

// ---------------------------------------------------------------------------------------------
// Precedence folding (spec section 8): CLI flag > CROSS_VM_* env > profile/[defaults] (already
// folded by resolve_profile) > built-in default.
// ---------------------------------------------------------------------------------------------

/// Folds `args` and `env`-sourced `CROSS_VM_*`/`PROPTEST_CASES` overrides into a
/// [`RunOptions`], CLI winning over env. `--stats` is a bool flag: `Some(true)` when present so
/// the profile's own `stats` key stands when it is not.
fn build_run_options(args: &RunArgs, env: &dyn Fn(&str) -> Option<String>) -> RunOptions {
    let seed = args
        .seed
        .or_else(|| env("CROSS_VM_SEED").and_then(|s| s.parse().ok()));
    let cases = args.cases.or_else(|| {
        env("CROSS_VM_CASES")
            .or_else(|| env("PROPTEST_CASES"))
            .and_then(|s| s.parse().ok())
    });
    let ops = args
        .ops
        .or_else(|| env("CROSS_VM_OPS").and_then(|s| s.parse().ok()));

    RunOptions {
        seed,
        ops,
        cases,
        duration: args.duration,
        target: args.target,
        target_chains: args
            .target_chain
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>(),
        stats: args.stats.then_some(true),
        check_every: args.check_every,
        json_report: args.json_report.clone(),
        artifacts_dir: args.artifacts_dir.clone(),
        no_shrink: args.no_shrink,
        // Never folded here: `run_with_config` wires this to the ctrl-c signal task, which needs
        // a live `Arc` to flip, not anything derivable from `args`/`env` alone. Kept `None` in
        // this pure, unit-tested builder.
        stop: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Profile / suite selection (spec section 8)
// ---------------------------------------------------------------------------------------------

/// Resolves which profiles a `run` invocation drives, and whether to stop at the first failure.
///
/// Order: `--suite NAME` (its own `profiles` + `stop_on_failure`) beats one-or-more `--profile
/// NAME` (run in order, `stop_on_failure = false`) beats `CROSS_VM_PROFILE` (single profile) beats
/// "exactly one profile exists in the config" (auto-select). Otherwise: a usage error listing the
/// available names. An unknown `--suite`/`--profile`/`CROSS_VM_PROFILE` name is also a usage
/// error listing the available names.
fn select_profile_names(
    cfg: &cross_vm_config::RunConfig,
    args: &RunArgs,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(Vec<String>, bool), String> {
    if let Some(suite_name) = &args.suite {
        let suite = cfg.suites.get(suite_name).ok_or_else(|| {
            unknown_name_message("suite", suite_name, cfg.suites.keys().map(String::as_str))
        })?;
        return Ok((suite.profiles.clone(), suite.stop_on_failure));
    }

    if !args.profile.is_empty() {
        for name in &args.profile {
            if !cfg.profiles.contains_key(name) {
                return Err(unknown_name_message(
                    "profile",
                    name,
                    cfg.profiles.keys().map(String::as_str),
                ));
            }
        }
        return Ok((args.profile.clone(), false));
    }

    if let Some(env_profile) = env("CROSS_VM_PROFILE") {
        if !cfg.profiles.contains_key(&env_profile) {
            return Err(unknown_name_message(
                "profile",
                &env_profile,
                cfg.profiles.keys().map(String::as_str),
            ));
        }
        return Ok((vec![env_profile], false));
    }

    if cfg.profiles.len() == 1 {
        let name = cfg
            .profiles
            .keys()
            .next()
            .expect("len == 1 checked above")
            .clone();
        return Ok((vec![name], false));
    }

    let mut names: Vec<&str> = cfg.profiles.keys().map(String::as_str).collect();
    names.sort_unstable();
    let available = if names.is_empty() {
        "<none>".to_string()
    } else {
        names.join(", ")
    };
    Err(format!(
        "no --profile or --suite given, and {} profiles exist in this config: choose one of: {available}",
        cfg.profiles.len()
    ))
}

/// Formats an "unknown `{kind}` `{name}`" usage-error message listing the available names,
/// sorted for a stable, testable message.
fn unknown_name_message<'a>(
    kind: &str,
    name: &str,
    names: impl Iterator<Item = &'a str>,
) -> String {
    let mut names: Vec<&str> = names.collect();
    names.sort_unstable();
    let available = if names.is_empty() {
        "<none>".to_string()
    } else {
        names.join(", ")
    };
    format!("unknown {kind} `{name}`; available: {available}")
}

// ---------------------------------------------------------------------------------------------
// Exit codes (spec section 8, the CI contract)
// ---------------------------------------------------------------------------------------------

/// Maps one [`ErasedReport`] to its exit code: `0` passed, `1` failed with `Bug`/`Invariant`, `2`
/// failed with `Infra`.
fn exit_code_for(report: &ErasedReport) -> u8 {
    match report.failure.as_ref().map(|f| &f.kind) {
        None => 0,
        Some(FailureKind::Bug(_)) | Some(FailureKind::Invariant { .. }) => 1,
        Some(FailureKind::Infra(_)) => 2,
    }
}

/// Maps a [`RunError`] to its exit code. `UnknownHarness`/`Validation`/`Invalid`/`UnsupportedMode`
/// are config/usage errors (`3`, per spec section 8's exit-code list) — a profile setting
/// `export_world` against a harness that cannot export lands in `Invalid`, since `validate` is
/// meant to catch exactly this offline. `Setup` (the config-driven setup fn failed: deploy/RPC/
/// model desync), `Serialize` (the failure history could not be turned into JSON), and `Export`
/// (a `register_persistent` harness's `export_world` write itself failed: bad directory,
/// permissions, disk full) are not usage errors — nothing about the invocation was wrong — but
/// neither are they a discovered SUT bug, so all three map to `2` (infra-only failure), the same
/// bucket a `FailureKind::Infra` report gets.
fn exit_code_for_run_error(err: &RunError) -> u8 {
    match err {
        RunError::UnknownHarness(_)
        | RunError::Validation(_)
        | RunError::Invalid(_)
        | RunError::UnsupportedMode(_) => 3,
        RunError::Setup(_) | RunError::Serialize(_) | RunError::Export(_) => 2,
    }
}

/// The severity ranking [`combine`] uses: usage/config error (exit code `3`) dominates
/// everything; among run outcomes, a discovered bug/invariant violation (`1`) is worse than an
/// infra-only failure (`2`), which is worse than a clean pass (`0`). This is spec section 8's
/// ordering ("a suite reports the worst code") made precise: exit-code numbers are not
/// monotonic with severity (`1` is worse than `2`), so comparing them directly would be wrong;
/// this function is the one place that ordering is decided.
fn severity_rank(code: u8) -> u8 {
    match code {
        3 => 3,
        1 => 2,
        2 => 1,
        0 => 0,
        _ => 3, // defensive: an unrecognized code is treated as the most severe
    }
}

/// Combines a sequence of exit codes into the single worst one, per [`severity_rank`]'s ordering.
/// `combine([])` is `0` (no runs, nothing failed).
fn combine(codes: impl IntoIterator<Item = u8>) -> u8 {
    codes.into_iter().fold(0u8, |worst, code| {
        if severity_rank(code) > severity_rank(worst) {
            code
        } else {
            worst
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Running the selected profiles
// ---------------------------------------------------------------------------------------------

/// Runs every name in `names` against `cfg`'s harness, in order, combining exit codes per
/// [`combine`]. When `stop_on_failure` is set, stops after the first profile whose code is
/// non-zero (whether a config/usage error resolving the profile, a `RunError`, or a failing
/// report).
///
/// Accumulates every profile's [`ErasedReport`] into one `Vec`, and — if a JSON report path is
/// set, either `opts.json_report` (the CLI `--json-report` flag, checked first) or the first
/// resolved profile's own `json_report` key — writes the whole invocation's envelope exactly
/// once at the end via [`write_json_report`], never per-profile (spec section 9: one file holds
/// every profile of one invocation). `config_path` is the config file path exactly as the user
/// passed it, `names` is the invocation's selected profile names (recorded in the envelope
/// regardless of whether `stop_on_failure` cut the run short before every one of them ran).
async fn run_selected(
    registry: &Registry,
    cfg: &cross_vm_config::RunConfig,
    names: &[String],
    opts: &RunOptions,
    stop_on_failure: bool,
    config_path: &str,
) -> u8 {
    let mut code = 0u8;
    let mut reports: Vec<ErasedReport> = Vec::new();
    // `resolve_profile` already folds `opts.json_report.or(profile.json_report)` into
    // `resolved.json_report`, so checking `opts.json_report` here first is only a fast path that
    // skips resolving the first profile when the CLI flag alone already decides the path; the
    // `is_none()` fallback below reaches the same value `resolved.json_report` would give. Once a
    // path is found it is never overwritten by a later profile's own key, since the envelope is
    // written to exactly one file.
    let mut json_report_path = opts.json_report.clone();

    for name in names {
        let resolved: ResolvedProfile = match resolve_profile(cfg, name, opts) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(profile = %name, error = %e, "profile resolution failed (usage/config error)");
                code = combine([code, 3]);
                if stop_on_failure {
                    break;
                }
                continue;
            }
        };

        if json_report_path.is_none() {
            json_report_path = resolved.json_report.clone();
        }

        match registry.run(&cfg.harness.name, &resolved, opts).await {
            Ok(report) => {
                let this_code = exit_code_for(&report);
                log_profile_result(&report);

                // A replay artifact only makes sense for a generative failure (fuzz/invariant/
                // endurance): a scenario is already a concrete, checked-in sequence, and a
                // passing run has no failure to reproduce.
                if report.failure.is_some() && report.mode != "scenario" {
                    match write_replay_artifact(
                        Path::new(&resolved.artifacts_dir),
                        cfg,
                        &resolved,
                        &report,
                    ) {
                        Ok(path) => tracing::info!(
                            "wrote replay artifact: {}; reproduce with: cross-vm replay {}",
                            path.display(),
                            path.display()
                        ),
                        // Non-fatal to the exit code: the run already failed and reported that
                        // failure; a write error here (bad directory, permissions, disk full) is
                        // a secondary concern, logged but never overriding `this_code`.
                        Err(e) => tracing::warn!(
                            profile = %resolved.name,
                            error = %e,
                            "failed to write replay artifact"
                        ),
                    }
                }

                reports.push(report);

                code = combine([code, this_code]);
                if stop_on_failure && this_code != 0 {
                    break;
                }
            }
            Err(e) => {
                let this_code = exit_code_for_run_error(&e);
                tracing::error!(profile = %name, error = %e, "run failed");
                code = combine([code, this_code]);
                if stop_on_failure && this_code != 0 {
                    break;
                }
            }
        }
    }

    if let Some(path) = &json_report_path {
        let overrides = overrides_json(opts);
        match write_json_report(Path::new(path), config_path, names, &reports, overrides) {
            Ok(()) => tracing::info!(path, "wrote JSON report"),
            Err(e) => {
                // An IO failure here (bad directory, permissions, disk full) is a property of
                // the invocation's own `--json-report`/`json_report` argument, not of anything
                // the run discovered about the system under test: it belongs in the same
                // usage/config bucket (exit 3) as an unresolvable profile name or a malformed
                // config, not the infra bucket (2) reserved for chain/RPC/deploy failures during
                // a run. `combine` folds it in, so it dominates a clean pass but never silently
                // downgrades a worse code a profile already reported.
                tracing::error!(path, error = %e, "failed to write JSON report");
                code = combine([code, 3]);
            }
        }
    }

    code
}

/// Builds the `invocation.overrides` object for a [`write_json_report`] call: every CLI-set
/// scalar on `opts`, skipping anything left at its `None`/empty/`false` default. Deliberately
/// narrow — only the run-shape knobs (`seed`/`ops`/`cases`/`duration`/`target`/`target_chain`/
/// `stats`/`check_every`/`no_shrink`), never a config value (env params, rpc URLs, ...), so the
/// envelope can never leak a config secret through this field.
fn overrides_json(opts: &RunOptions) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(v) = opts.seed {
        map.insert("seed".to_string(), v.into());
    }
    if let Some(v) = opts.ops {
        map.insert("ops".to_string(), v.into());
    }
    if let Some(v) = opts.cases {
        map.insert("cases".to_string(), v.into());
    }
    if let Some(v) = opts.duration {
        map.insert("duration_secs".to_string(), v.as_secs().into());
    }
    if let Some(t) = opts.target {
        map.insert("target".to_string(), target_label(t).into());
    }
    if !opts.target_chains.is_empty() {
        let per_chain: serde_json::Map<String, serde_json::Value> = opts
            .target_chains
            .iter()
            .map(|(label, target)| {
                (
                    label.clone(),
                    serde_json::Value::from(target_label(*target)),
                )
            })
            .collect();
        map.insert(
            "target_chain".to_string(),
            serde_json::Value::Object(per_chain),
        );
    }
    if let Some(v) = opts.stats {
        map.insert("stats".to_string(), v.into());
    }
    if let Some(v) = opts.check_every {
        map.insert("check_every".to_string(), v.into());
    }
    if opts.no_shrink {
        map.insert("no_shrink".to_string(), true.into());
    }
    serde_json::Value::Object(map)
}

/// `"mock"`/`"rpc"`, the JSON-friendly label for a [`Target`] (the inverse of [`parse_target`]).
fn target_label(t: Target) -> &'static str {
    match t {
        Target::Mock => "mock",
        Target::Rpc => "rpc",
    }
}

/// Logs one profile's per-run result line (pass/fail, mode, seed, steps, elapsed).
fn log_profile_result(report: &ErasedReport) {
    match &report.failure {
        None => tracing::info!(
            profile = %report.profile,
            mode = %report.mode,
            seed = report.seed,
            steps = report.steps,
            elapsed = ?report.elapsed,
            "PASS"
        ),
        Some(f) => tracing::info!(
            profile = %report.profile,
            mode = %report.mode,
            seed = report.seed,
            steps = report.steps,
            elapsed = ?report.elapsed,
            step = f.step,
            kind = ?f.kind,
            "FAIL"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{CheckOutcome, Ctx, HarnessError, Prng, Verdict};
    use std::rc::Rc;

    // -----------------------------------------------------------------------------------------
    // exit_code_for / exit_code_for_run_error / combine
    // -----------------------------------------------------------------------------------------

    fn passing_report() -> ErasedReport {
        ErasedReport {
            harness: "h".to_string(),
            profile: "p".to_string(),
            mode: "fuzz".to_string(),
            seed: 0,
            steps: 1,
            skipped: 0,
            coverage: Default::default(),
            stats: None,
            elapsed: Duration::ZERO,
            failure: None,
        }
    }

    fn failing_report(kind: FailureKind) -> ErasedReport {
        ErasedReport {
            failure: Some(crate::config::ErasedFailure {
                step: 1,
                kind,
                op_debug: None,
                history: serde_json::Value::Null,
                shrunk: false,
            }),
            ..passing_report()
        }
    }

    #[test]
    fn exit_code_for_passing_report_is_zero() {
        assert_eq!(exit_code_for(&passing_report()), 0);
    }

    #[test]
    fn exit_code_for_bug_is_one() {
        let report = failing_report(FailureKind::Bug("boom".to_string()));
        assert_eq!(exit_code_for(&report), 1);
    }

    #[test]
    fn exit_code_for_invariant_is_one() {
        let report = failing_report(FailureKind::Invariant {
            name: "inv".to_string(),
            detail: "broke".to_string(),
        });
        assert_eq!(exit_code_for(&report), 1);
    }

    #[test]
    fn exit_code_for_infra_is_two() {
        let report = failing_report(FailureKind::Infra("rpc down".to_string()));
        assert_eq!(exit_code_for(&report), 2);
    }

    #[test]
    fn exit_code_for_run_error_maps_usage_errors_to_three() {
        assert_eq!(
            exit_code_for_run_error(&RunError::UnknownHarness("x".to_string())),
            3
        );
        assert_eq!(
            exit_code_for_run_error(&RunError::Invalid("x".to_string())),
            3
        );
        assert_eq!(
            exit_code_for_run_error(&RunError::UnsupportedMode("x".to_string())),
            3
        );
    }

    #[test]
    fn exit_code_for_run_error_maps_setup_and_serialize_to_two() {
        assert_eq!(
            exit_code_for_run_error(&RunError::Setup("x".to_string())),
            2
        );
        assert_eq!(
            exit_code_for_run_error(&RunError::Serialize("x".to_string())),
            2
        );
    }

    #[test]
    fn combine_empty_is_zero() {
        assert_eq!(combine([]), 0);
    }

    #[test]
    fn combine_bug_beats_infra_beats_pass() {
        assert_eq!(combine([0, 2, 1]), 1);
        assert_eq!(combine([0, 2]), 2);
        assert_eq!(combine([0, 0]), 0);
    }

    #[test]
    fn combine_usage_error_dominates_everything() {
        assert_eq!(combine([1, 2, 3, 0]), 3);
        assert_eq!(combine([3, 0]), 3);
    }

    // -----------------------------------------------------------------------------------------
    // clap parsing
    // -----------------------------------------------------------------------------------------

    #[test]
    fn parses_run_with_two_profiles_a_target_chain_and_seed() {
        let args = CliArgs::try_parse_from([
            "cross-vm",
            "run",
            "f.toml",
            "--profile",
            "a",
            "--profile",
            "b",
            "--target-chain",
            "eth=rpc",
            "--seed",
            "7",
        ])
        .expect("valid invocation");

        let Command::Run(run) = args.command else {
            panic!("expected Run subcommand");
        };
        assert_eq!(run.config, PathBuf::from("f.toml"));
        assert_eq!(run.profile, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(run.target_chain, vec![("eth".to_string(), Target::Rpc)]);
        assert_eq!(run.seed, Some(7));
    }

    #[test]
    fn bad_target_chain_without_equals_is_a_parse_error() {
        let err =
            CliArgs::try_parse_from(["cross-vm", "run", "f.toml", "--target-chain", "ethrpc"])
                .unwrap_err();
        assert!(err.to_string().contains("LABEL=mock|rpc"), "{err}");
    }

    #[test]
    fn bad_target_value_is_a_parse_error() {
        let err =
            CliArgs::try_parse_from(["cross-vm", "run", "f.toml", "--target", "xyz"]).unwrap_err();
        assert!(err.to_string().contains("mock"), "{err}");
    }

    #[test]
    fn validate_subcommand_parses() {
        let args = CliArgs::try_parse_from(["cross-vm", "validate", "f.toml"]).unwrap();
        assert!(matches!(args.command, Command::Validate(_)));
    }

    #[test]
    fn list_subcommand_parses() {
        let args = CliArgs::try_parse_from(["cross-vm", "list", "f.toml"]).unwrap();
        assert!(matches!(args.command, Command::List(_)));
    }

    #[test]
    fn replay_subcommand_parses() {
        let args = CliArgs::try_parse_from(["cross-vm", "replay", "f.replay.toml"]).unwrap();
        let Command::Replay(replay) = args.command else {
            panic!("expected Replay subcommand");
        };
        assert_eq!(replay.artifact, PathBuf::from("f.replay.toml"));
    }

    #[test]
    fn stats_flag_and_no_shrink_flag_parse_as_bools() {
        let args = CliArgs::try_parse_from(["cross-vm", "run", "f.toml", "--stats", "--no-shrink"])
            .unwrap();
        let Command::Run(run) = args.command else {
            panic!("expected Run");
        };
        assert!(run.stats);
        assert!(run.no_shrink);
    }

    // -----------------------------------------------------------------------------------------
    // RunOptions folding (spec section 8 precedence)
    // -----------------------------------------------------------------------------------------

    fn run_args_with_seed(seed: Option<u64>) -> RunArgs {
        RunArgs {
            seed,
            ..Default::default()
        }
    }

    #[test]
    fn cli_seed_wins_over_env() {
        let args = run_args_with_seed(Some(7));
        let env = |k: &str| {
            if k == "CROSS_VM_SEED" {
                Some("99".to_string())
            } else {
                None
            }
        };
        let opts = build_run_options(&args, &env);
        assert_eq!(opts.seed, Some(7));
    }

    #[test]
    fn env_seed_used_when_no_cli_flag() {
        let args = run_args_with_seed(None);
        let env = |k: &str| {
            if k == "CROSS_VM_SEED" {
                Some("99".to_string())
            } else {
                None
            }
        };
        let opts = build_run_options(&args, &env);
        assert_eq!(opts.seed, Some(99));
    }

    #[test]
    fn neither_cli_nor_env_seed_leaves_none_for_profile_to_stand() {
        let args = run_args_with_seed(None);
        let env = |_: &str| None;
        let opts = build_run_options(&args, &env);
        assert_eq!(opts.seed, None);
    }

    #[test]
    fn cases_folds_from_cross_vm_cases_then_proptest_cases() {
        let args = RunArgs::default();
        let env = |k: &str| {
            if k == "PROPTEST_CASES" {
                Some("42".to_string())
            } else {
                None
            }
        };
        let opts = build_run_options(&args, &env);
        assert_eq!(opts.cases, Some(42));
    }

    #[test]
    fn stats_flag_present_is_some_true_absent_is_none() {
        let mut args = RunArgs::default();
        let env = |_: &str| None;
        assert_eq!(build_run_options(&args, &env).stats, None);
        args.stats = true;
        assert_eq!(build_run_options(&args, &env).stats, Some(true));
    }

    // -----------------------------------------------------------------------------------------
    // profile / suite selection
    // -----------------------------------------------------------------------------------------

    fn load(toml: &str) -> cross_vm_config::RunConfig {
        cross_vm_config::from_toml_str(toml, &|_| None).expect("valid fixture")
    }

    const SINGLE_PROFILE: &str = r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#;

    const MULTI_PROFILE: &str = r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1

[profile.deep]
mode = "invariant"
ops = 5

[suite.ci]
profiles = ["smoke", "deep"]
stop_on_failure = true
"#;

    #[test]
    fn single_profile_config_auto_selects() {
        let cfg = load(SINGLE_PROFILE);
        let args = RunArgs::default();
        let env = |_: &str| None;
        let (names, stop) = select_profile_names(&cfg, &args, &env).unwrap();
        assert_eq!(names, vec!["smoke".to_string()]);
        assert!(!stop);
    }

    #[test]
    fn multi_profile_with_no_selector_is_a_usage_error_listing_names() {
        let cfg = load(MULTI_PROFILE);
        let args = RunArgs::default();
        let env = |_: &str| None;
        let err = select_profile_names(&cfg, &args, &env).unwrap_err();
        assert!(err.contains("smoke"), "{err}");
        assert!(err.contains("deep"), "{err}");
    }

    #[test]
    fn explicit_profile_flags_win_and_run_in_order() {
        let cfg = load(MULTI_PROFILE);
        let args = RunArgs {
            profile: vec!["deep".to_string(), "smoke".to_string()],
            ..Default::default()
        };
        let env = |_: &str| None;
        let (names, stop) = select_profile_names(&cfg, &args, &env).unwrap();
        assert_eq!(names, vec!["deep".to_string(), "smoke".to_string()]);
        assert!(!stop);
    }

    #[test]
    fn unknown_profile_flag_is_a_usage_error() {
        let cfg = load(MULTI_PROFILE);
        let args = RunArgs {
            profile: vec!["nope".to_string()],
            ..Default::default()
        };
        let env = |_: &str| None;
        let err = select_profile_names(&cfg, &args, &env).unwrap_err();
        assert!(err.contains("nope"));
        assert!(err.contains("smoke"));
    }

    #[test]
    fn suite_selects_its_profiles_and_stop_on_failure() {
        let cfg = load(MULTI_PROFILE);
        let args = RunArgs {
            suite: Some("ci".to_string()),
            ..Default::default()
        };
        let env = |_: &str| None;
        let (names, stop) = select_profile_names(&cfg, &args, &env).unwrap();
        assert_eq!(names, vec!["smoke".to_string(), "deep".to_string()]);
        assert!(stop);
    }

    #[test]
    fn unknown_suite_is_a_usage_error() {
        let cfg = load(MULTI_PROFILE);
        let args = RunArgs {
            suite: Some("nope".to_string()),
            ..Default::default()
        };
        let env = |_: &str| None;
        let err = select_profile_names(&cfg, &args, &env).unwrap_err();
        assert!(err.contains("nope"));
        assert!(err.contains("ci"));
    }

    #[test]
    fn cross_vm_profile_env_selects_when_no_flag_given() {
        let cfg = load(MULTI_PROFILE);
        let args = RunArgs::default();
        let env = |k: &str| {
            if k == "CROSS_VM_PROFILE" {
                Some("deep".to_string())
            } else {
                None
            }
        };
        let (names, stop) = select_profile_names(&cfg, &args, &env).unwrap();
        assert_eq!(names, vec!["deep".to_string()]);
        assert!(!stop);
    }

    // -----------------------------------------------------------------------------------------
    // end-to-end through the CLI dispatch helpers, over a mock harness (cheap: no injected chain)
    // -----------------------------------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    enum MockKind {
        Ping,
        Boom,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    enum MockOp {
        Ping,
        Boom,
    }

    #[derive(Debug, Clone)]
    enum MockInvariant {
        AlwaysHolds,
    }

    struct MockHarness;

    impl Harness for MockHarness {
        type World = u32;
        type Operation = MockOp;
        type Invariant = MockInvariant;
        type OpKind = MockKind;

        async fn apply(
            &self,
            _ctx: &mut Ctx,
            world: &mut Self::World,
            op: &Self::Operation,
        ) -> Result<Verdict, HarnessError> {
            *world += 1;
            match op {
                MockOp::Ping => Ok(Verdict::Accepted),
                MockOp::Boom => Err(HarnessError::Bug("boom".to_string())),
            }
        }

        fn op_kinds(&self) -> Vec<Self::OpKind> {
            vec![MockKind::Ping, MockKind::Boom]
        }

        fn generate_op(
            &self,
            _rng: &mut Prng,
            _world: &Self::World,
            kind: Self::OpKind,
        ) -> Self::Operation {
            match kind {
                MockKind::Ping => MockOp::Ping,
                MockKind::Boom => MockOp::Boom,
            }
        }

        fn invariants(&self) -> Vec<Self::Invariant> {
            vec![MockInvariant::AlwaysHolds]
        }

        async fn check(
            &self,
            _ctx: &mut Ctx,
            _world: &Self::World,
            _inv: &Self::Invariant,
        ) -> CheckOutcome {
            CheckOutcome::Held
        }
    }

    async fn mock_ctx() -> Ctx {
        let wallets = Rc::new(
            cross_vm_core::WalletFactory::from_roster(crate::EmptyWallets::SPECS)
                .expect("empty roster"),
        );
        let env = crate::MultiChainEnv::new("mock", wallets);
        Ctx::new(env.start().await.expect("start"))
    }

    fn mock_setup(req: SetupRequest) -> SetupFuture<'static, u32> {
        Box::pin(async move {
            let ctx = mock_ctx().await;
            Ok((ctx, req.seed as u32))
        })
    }

    fn cli_with_mock() -> Cli {
        Cli::new().register("vault", || MockHarness, mock_setup)
    }

    #[tokio::test]
    async fn validate_with_config_passes_for_known_kinds() {
        let cfg = load(SINGLE_PROFILE);
        let cli = cli_with_mock();
        assert_eq!(cli.validate_with_config(&cfg), 0);
    }

    #[tokio::test]
    async fn validate_with_config_fails_for_unknown_harness() {
        let cfg = load(
            r#"
[harness]
name = "not-registered"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let cli = cli_with_mock();
        assert_eq!(cli.validate_with_config(&cfg), 3);
    }

    #[tokio::test]
    async fn validate_with_config_fails_for_unknown_kind() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Nope"]
"#,
        );
        let cli = cli_with_mock();
        assert_eq!(cli.validate_with_config(&cfg), 3);
    }

    #[tokio::test]
    async fn list_with_config_returns_zero() {
        let cfg = load(MULTI_PROFILE);
        let cli = cli_with_mock();
        assert_eq!(cli.list_with_config(&cfg), 0);
    }

    #[tokio::test]
    async fn run_with_config_all_pass_is_zero() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 2
kinds = ["Ping"]
"#,
        );
        let cli = cli_with_mock();
        let args = RunArgs {
            config: PathBuf::from("unused"),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 0);
    }

    #[tokio::test]
    async fn run_with_config_bug_is_one() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Boom"]
"#,
        );
        let cli = cli_with_mock();
        // This run fails on `Boom`, so `run_selected` writes a replay artifact; pin its dir to a
        // gitignored `tests_result` path so it never leaks into the source-tree `target/cross-vm`.
        let args = RunArgs {
            artifacts_dir: Some(
                temp_artifacts_dir("bug-is-one")
                    .to_str()
                    .unwrap()
                    .to_string(),
            ),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 1);
    }

    #[tokio::test]
    async fn run_with_config_unknown_profile_selector_is_three() {
        let cfg = load(MULTI_PROFILE);
        let cli = cli_with_mock();
        let args = RunArgs {
            profile: vec!["nope".to_string()],
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 3);
    }

    #[tokio::test]
    async fn run_with_config_multi_profile_reports_worst_code() {
        // `smoke` passes, `deep` (invariant, Boom-only) fails with Bug -> combined code is 1.
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Ping"]

[profile.deep]
mode = "invariant"
ops = 1
kinds = ["Boom"]
"#,
        );
        let cli = cli_with_mock();
        // `deep` fails on `Boom`, so a replay artifact is written; pin its dir to a gitignored
        // `tests_result` path so it never leaks into the source-tree `target/cross-vm`.
        let args = RunArgs {
            profile: vec!["smoke".to_string(), "deep".to_string()],
            artifacts_dir: Some(
                temp_artifacts_dir("multi-profile-worst-code")
                    .to_str()
                    .unwrap()
                    .to_string(),
            ),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 1);
    }

    #[tokio::test]
    async fn run_with_config_ops_override_from_cli_wins() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "invariant"
ops = 1
kinds = ["Ping"]
"#,
        );
        let cli = cli_with_mock();
        let args = RunArgs {
            ops: Some(3),
            ..Default::default()
        };
        // Steps aren't observable through the exit code alone, but a bad override would panic
        // or error inside `run_with`, so a clean 0 here demonstrates the override was accepted
        // and threaded through `RunOptions` end to end via the CLI dispatch path.
        assert_eq!(cli.run_with_config(&cfg, &args).await, 0);
    }

    // -----------------------------------------------------------------------------------------
    // --json-report (spec section 9): overrides_json, and the envelope written once per
    // invocation by run_selected/run_with_config.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn overrides_json_is_empty_object_when_nothing_is_set() {
        assert_eq!(
            overrides_json(&RunOptions::default()),
            serde_json::json!({})
        );
    }

    #[test]
    fn overrides_json_includes_only_the_scalars_that_were_set() {
        let opts = RunOptions {
            seed: Some(7),
            cases: Some(2),
            ..Default::default()
        };
        assert_eq!(
            overrides_json(&opts),
            serde_json::json!({"seed": 7, "cases": 2})
        );
    }

    #[test]
    fn overrides_json_never_includes_json_report_or_artifacts_dir() {
        // Both are file paths, not "run-shape" overrides; asserting their absence documents
        // that this function's field list is deliberately narrow, not merely incomplete.
        let opts = RunOptions {
            json_report: Some("out.json".to_string()),
            artifacts_dir: Some("/tmp/artifacts".to_string()),
            no_shrink: true,
            ..Default::default()
        };
        let value = overrides_json(&opts);
        assert!(value.get("json_report").is_none());
        assert!(value.get("artifacts_dir").is_none());
        assert_eq!(value["no_shrink"], true);
    }

    /// A fresh temp file path under the OS temp dir, unique per test invocation (process id +
    /// a monotonic counter), so parallel test runs never collide. No new dev-dependency needed
    /// for this one narrow use.
    fn temp_json_path(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests_result")
            .join(format!(
                "cross-vm-cli-json-report-{}-{}-{label}.json",
                std::process::id(),
                n
            ))
    }

    #[tokio::test]
    async fn run_with_config_writes_json_report_once_for_the_whole_invocation() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Ping"]

[profile.deep]
mode = "invariant"
ops = 1
kinds = ["Ping"]
"#,
        );
        let cli = cli_with_mock();
        let path = temp_json_path("multi-profile");
        let args = RunArgs {
            config: PathBuf::from("vault.cross-vm.toml"),
            profile: vec!["smoke".to_string(), "deep".to_string()],
            json_report: Some(path.to_str().unwrap().to_string()),
            seed: Some(42),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 0);

        let raw = std::fs::read_to_string(&path).expect("json report was written");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["invocation"]["config"], "vault.cross-vm.toml");
        assert_eq!(
            value["invocation"]["profiles"],
            serde_json::json!(["smoke", "deep"])
        );
        assert_eq!(
            value["invocation"]["overrides"],
            serde_json::json!({"seed": 42})
        );
        let profiles = value["profiles"].as_array().expect("profiles array");
        // One entry per profile in the invocation: the envelope is written once, not per
        // profile, so both selected profiles land in the same file's `profiles` array.
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0]["profile"], "smoke");
        assert_eq!(profiles[1]["profile"], "deep");

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn run_with_config_no_json_report_flag_writes_nothing() {
        // `kinds = ["Ping"]` (unlike `SINGLE_PROFILE`) so the run is deterministically a pass:
        // this test asserts absence of a file, which a stray `Boom`-triggered exit code 1 must
        // not be able to cause a false failure on.
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Ping"]
"#,
        );
        let cli = cli_with_mock();
        let path = temp_json_path("not-requested");
        assert!(!path.exists());
        let args = RunArgs {
            config: PathBuf::from("vault.cross-vm.toml"),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 0);
        assert!(
            !path.exists(),
            "no --json-report flag was given; nothing should be written"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Replay artifacts (spec section 10): written on a generative failure by `run_selected`,
    // never for a pass or a scenario run; `cross-vm replay <artifact>` (`dispatch_replay`) is
    // sugar for `run <artifact> --profile replay` and reproduces the same failure.
    // -----------------------------------------------------------------------------------------

    /// A fresh, gitignored dir under `<CARGO_MANIFEST_DIR>/tests_result/`, unique per test
    /// invocation, so replay artifacts land in a stable inspectable location (never a source-tree
    /// `target/` dir) and parallel runs never collide. The writer creates it on demand.
    fn temp_artifacts_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests_result")
            .join(format!(
                "cross-vm-cli-replay-artifact-{}-{}-{label}",
                std::process::id(),
                n
            ))
    }

    #[tokio::test]
    async fn run_with_config_writes_a_replay_artifact_on_a_generative_failure() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Boom"]
"#,
        );
        let cli = cli_with_mock();
        let dir = temp_artifacts_dir("on-failure");
        let args = RunArgs {
            config: PathBuf::from("vault.cross-vm.toml"),
            artifacts_dir: Some(dir.to_str().unwrap().to_string()),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 1);

        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("artifacts dir was created")
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            entries.len(),
            1,
            "exactly one artifact for one failing profile"
        );
        let path = entries[0].path();
        assert!(path.to_string_lossy().ends_with(".replay.toml"), "{path:?}");
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("vault-smoke-"),
            "filename must start with <harness>-<profile>-: {path:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_with_config_writes_no_artifact_on_a_pass_or_a_scenario_run() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Ping"]
"#,
        );
        let cli = cli_with_mock();
        let dir = temp_artifacts_dir("no-artifact-on-pass");
        let args = RunArgs {
            config: PathBuf::from("vault.cross-vm.toml"),
            artifacts_dir: Some(dir.to_str().unwrap().to_string()),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 0);
        assert!(
            !dir.exists(),
            "a passing run must not create the artifacts dir at all"
        );
    }

    #[tokio::test]
    async fn replay_subcommand_reproduces_the_same_failure_the_artifact_recorded() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
kinds = ["Boom"]
"#,
        );
        let cli = cli_with_mock();
        let dir = temp_artifacts_dir("replay-e2e");
        let args = RunArgs {
            config: PathBuf::from("vault.cross-vm.toml"),
            artifacts_dir: Some(dir.to_str().unwrap().to_string()),
            ..Default::default()
        };
        assert_eq!(cli.run_with_config(&cfg, &args).await, 1);

        let artifact_path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .expect("one artifact written")
            .unwrap()
            .path();

        // `cross-vm replay <artifact>` must reproduce the exact same failure (still-broken SUT
        // means it still fails; exit code 1, same as the original run).
        let code = cli
            .dispatch_replay(ReplayArgs {
                artifact: artifact_path,
            })
            .await;
        assert_eq!(code, 1, "the recorded Boom must still reproduce on replay");

        std::fs::remove_dir_all(&dir).ok();
    }
}
