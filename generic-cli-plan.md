# Generic CLI and Config Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Tasks marked with the same "Parallel group" letter are independent and MUST be dispatched to parallel subagents in one message to save wall clock time.

**Goal:** Extract the chain-agnostic parts of `cross-vm-config` and `cross-vm-framework`'s `cli` feature into two new generic crates (`harness-config`, `harness-cli`) that work raw against `harness-core`, then rebuild the cross-vm config and CLI as a domain variant on top of them, plus one raw generic example (`examples/math-tests`) that doubles as developer documentation for building new variants.

**Architecture:** `harness-config` is a pure data crate (TOML/JSON loader pipeline: parse, interpolate, merge, typed deserialize, validate) parameterized by a `ConfigExt` trait that lets a domain add top-level config sections (cross-vm adds `[[chain]]`) and domain validation. `harness-cli` holds the registry (type erasure over `harness_core::Harness`), profile resolution, run driving, JSON reports, replay artifacts, and the clap CLI, parameterized by a `CliDomain` trait that lets a domain add CLI flags (cross-vm adds `--target`/`--target-chain`) and build its own setup request type. The existing `cross-vm-config` and `cross-vm-framework::{cli, config}` become thin adapters over these crates, keeping their public re-export paths stable so the example crates keep compiling.

**Tech Stack:** Rust 2021 (MSRV 1.91), serde, toml 0.8, serde_json, humantime 2, clap 4 (derive), tokio (current_thread), dotenvy, tracing/tracing-subscriber, thiserror 2, harness-core.

## Global Constraints

- MSRV `1.91`, edition `2021`, workspace lints apply to new crates: `missing_docs = "warn"` (EVERY public item needs a rustdoc comment), `unsafe_code = "deny"`. Add `[lints] workspace = true` to every new Cargo.toml.
- New crate versions: `0.1.0` via `version.workspace = true`; internal deps declared in root `[workspace.dependencies]` with both `path` and `version = "0.1.0"` (publishability rule, see root Cargo.toml comment).
- Determinism guarantee must not change: rng draw order is "weighted kind index first, then op data" (see `crates/harness/src/runner.rs:1126-1183`). Nothing in this plan touches `harness-core`; do not add code that draws from the runner's `Prng` outside the existing paths.
- CLI behavior contract stays byte-compatible for cross-vm: exit codes 0 (pass), 1 (Bug/Invariant), 2 (Infra/Setup/Serialize/Export), 3 (usage/config); JSON report `schema_version = 1`; env precedence CLI flag > `CROSS_VM_*` env > profile key > built-in default. The `examples/*/tests/cli_e2e.rs` suites pin this and must pass unchanged in Phase 3.
- Commit messages: Conventional Commits, scope by crate (`feat(harness-config): ...`, `refactor(framework): ...`). Never mention AI tooling in commits.
- Run `cargo fmt --all` before every commit. Run tests with `--features` as noted per task (the framework CLI code needs `--features cli`).
- Documentation files written in Phase 4 must not use dashes as punctuation. Rephrase with periods, commas, or parentheses.
- All async code targets tokio `current_thread` flavor. `Cli::main` asserts this; keep the assert.
- Do not edit `crates/harness` (harness-core) or `crates/harness-macros` in this plan. They are frozen inputs.

---

## Crate and File Map

New workspace members:

```
crates/harness-config/            package "harness-config"   (pure data, no tokio/clap)
  src/lib.rs                      pipeline entry points, RunConfig<X>, ConfigError
  src/value.rs                    Doc/DocMap traits (made public), toml+json impls
  src/interpolate.rs              ${VAR} interpolation (verbatim port)
  src/duration.rs                 humantime serde adapters (verbatim port)
  src/seed.rs                     SeedSpec (verbatim port)
  src/schema.rs                   HarnessRef, Profile + 4 mode structs, CommonKeys,
                                  Suite/SuitePhase/WorldSource, ExpectStr, ScenarioStepRaw
  src/ext.rs                      ConfigExt trait + NoExt
  src/merge.rs                    [defaults] + env merge, ext hook for env entries
  src/validate.rs                 generic structural validation + ext validate call
  tests/loader_pipeline.rs        generic fixture suite
  tests/fixtures/*.toml           generic fixtures (no [[chain]] blocks)

crates/harness-cli/               package "harness-cli"      (heavy: clap/tokio/dotenvy)
  src/lib.rs                      module decls + re-exports
  src/domain.rs                   SetupFuture, SetupBuildError, CliDomain,
                                  GenericDomain, BasicSetup, NoArgs
  src/erased.rs                   ErasedReport/ErasedFailure/erase_report (port)
  src/report.rs                   JsonReport/Invocation/write_json_report (port)
  src/resolve.rs                  RunOptions, ResolvedProfile, resolve_profile (port, de-chained)
  src/registry.rs                 Registry<S>, RunError, ValidationError (port, de-Ctx'd)
  src/artifact.rs                 write_replay_artifact (port, domain sections hook)
  src/cli.rs                      Cli<D>, clap tree, select_phases, env folding,
                                  exit codes, run_selected (port)
  src/test_bridge.rs              run_profile_for_test (port)

examples/math-tests/              raw generic example (Phase 3, parallel track A)
  src/lib.rs                      MathHarness (adapted from crates/harness/tests/math.rs)
  src/bin/math_cli.rs             Cli::<GenericDomain> binary
  math.harness.toml               generic config file
  tests/config_driven.rs          test_bridge-driven profiles
  tests/cli_e2e.rs                subprocess e2e over the built bin
```

Modified crates (Phase 3, parallel track B):

```
crates/config      (cross-vm-config)     becomes: ChainDecl, EnvSpec, TargetStr, target fns,
                                         CrossVmExt, re-exports of harness-config generics
crates/framework   src/config/*          becomes: build_chain (unchanged), domain.rs
                                         (CrossVmDomain + TargetArgs + chain resolution),
                                         re-exports of harness-cli types
crates/framework   src/cli.rs            becomes: `pub type Cli = harness_cli::Cli<CrossVmDomain>`
                                         + re-exports; domain-specific tests only
```

---

## Parallel Execution Map

```
Phase 0:  T0 (serial, everything depends on it)

Phase 1:  group A: T1 -> T2 -> T3 -> T4 -> T5     (harness-config, sequential chain)
          group B: T6, T7                          (harness-cli foundations, need only
                                                    harness-core; dispatch T6 and T7 and
                                                    the T1..T5 chain ALL in parallel)

Phase 2:  group C: T8, T9      (both need Phase 1 track A done; independent of each other)
          group D: T10, T11    (T10 needs T8+T9+T6; T11 needs T8+T9+T6; independent of
                                each other, dispatch together)
          group E: T12, T13    (T12 needs T10+T11+T7; T13 needs T10; independent of each
                                other, dispatch together)

Phase 3:  group F: T14         (needs Phase 2; independent of track B)
          group G: T15 -> T16 -> T17 -> T18   (cross-vm rebase, sequential)
          Dispatch T14 alongside the T15..T18 chain.

Phase 4:  group H: T19 (docs) may start as soon as Phase 2 ends and run alongside Phase 3.
          T20 (workspace verify) is strictly last.
```

Dispatch guidance: use one subagent per task; for each "dispatch together" group send the Agent calls in a single message. Each subagent gets its task text verbatim plus the Interface Contract section below.

---

## Interface Contract (single source of truth)

Every task below consumes or produces these exact signatures. If a task's code disagrees with this section, this section wins.

### harness-config

```rust
// src/value.rs (visibility changed from pub(crate) to pub, otherwise verbatim port)
pub trait Doc: Clone + Sized + serde::de::DeserializeOwned {
    type Map: DocMap<Value = Self>;
    fn as_str_mut(&mut self) -> Option<&mut String>;
    fn as_array_mut(&mut self) -> Option<&mut Vec<Self>>;
    fn as_object_mut(&mut self) -> Option<&mut Self::Map>;
    fn as_object(&self) -> Option<&Self::Map>;
    fn into_object(self) -> Option<Self::Map>;
    fn is_object(&self) -> bool;
    fn as_str(&self) -> Option<&str>;
    fn from_object(map: Self::Map) -> Self;
    fn deserialize_into<T: serde::de::DeserializeOwned>(self) -> Result<T, String>;
}
pub trait DocMap: Clone {
    type Value;
    fn new() -> Self;
    fn is_empty(&self) -> bool;
    fn contains_key(&self, key: &str) -> bool;
    fn get(&self, key: &str) -> Option<&Self::Value>;
    fn get_mut(&mut self, key: &str) -> Option<&mut Self::Value>;
    fn remove(&mut self, key: &str) -> Option<Self::Value>;
    fn insert(&mut self, key: String, value: Self::Value);
    fn iter(&self) -> Box<dyn Iterator<Item = (&String, &Self::Value)> + '_>;
    fn iter_mut(&mut self) -> Box<dyn Iterator<Item = (&String, &mut Self::Value)> + '_>;
}
// NOTE: copy the two iter signatures EXACTLY as they exist in
// crates/config/src/value.rs. If the source uses a different return shape
// (e.g. concrete iterator types), keep the source shape and update this
// contract note in the plan file. The source file is authoritative for the
// port; only the pub visibility changes.

// src/ext.rs (NEW)
/// Domain extension for the config schema: extra top-level sections plus
/// domain validation. `Self` deserializes from whatever top-level keys remain
/// after the generic loader removes `harness`, `env`, `defaults`, `profile`,
/// `suite`, and `replay`. Use `#[serde(deny_unknown_fields)]` on the
/// implementing struct so unknown top-level keys stay hard errors.
pub trait ConfigExt:
    Sized + Clone + core::fmt::Debug + serde::de::DeserializeOwned + Default + 'static
{
    /// Domain validation pass, runs after generic structural validation.
    fn validate(cfg: &RunConfig<Self>) -> Result<(), ConfigError> {
        let _ = cfg;
        Ok(())
    }
    /// Merge hook for one colliding key when a profile's `env` table is
    /// overlaid on the top-level `[env]`. Default: replace the slot.
    fn merge_env_entry<V: Doc>(key: &str, slot: &mut V, incoming: V) {
        let _ = key;
        *slot = incoming;
    }
}

/// The no-op extension: no extra top-level sections allowed.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoExt {}
impl ConfigExt for NoExt {}

// src/lib.rs
pub struct RunConfig<X: ConfigExt> {
    pub harness: HarnessRef,
    /// Top-level `[env]`, opaque to the generic layer. Always a JSON object;
    /// `{}` when the config file omits `[env]`.
    pub env: serde_json::Value,
    pub profiles: std::collections::BTreeMap<String, Profile>,
    pub suites: std::collections::BTreeMap<String, Suite>,
    pub warnings: Vec<String>,
    /// The domain's own top-level sections.
    pub ext: X,
}
pub fn from_toml_str<X: ConfigExt>(s: &str, vars: &dyn Fn(&str) -> Option<String>) -> Result<RunConfig<X>, ConfigError>;
pub fn from_json_str<X: ConfigExt>(s: &str, vars: &dyn Fn(&str) -> Option<String>) -> Result<RunConfig<X>, ConfigError>;
pub fn load<X: ConfigExt>(path: &std::path::Path, vars: &dyn Fn(&str) -> Option<String>) -> Result<RunConfig<X>, ConfigError>;

// ConfigError: the generic variants of crates/config/src/lib.rs's ConfigError
// (Io, Parse, Deserialize, MissingVar, Interpolation, InvalidCases, InvalidOps,
//  EmptySteps, EnduranceMissingBound, KindsWeightsConflict, UnknownSuiteProfile,
//  SuiteProfilesAndPhases, PhaseNeedsNotEarlier, DuplicatePhaseProfile,
//  PhaseInheritArity, PhaseWorldNotSingleSetup, SharedDonor)
// with their exact thiserror messages, PLUS one new variant:
    /// A domain extension rejected the config.
    #[error("{0}")]
    Domain(String),
// The 7 chain-specific variants (DuplicateChainLabel, MissingChainFields,
// EmptyChainKind, UnknownChainSelection, UnknownTargetLabel,
// InvalidChainTarget, MissingRpcUrl) do NOT move; cross-vm reports those
// through Domain(String) in Phase 3.

// src/schema.rs: identical public surface to crates/config/src/schema.rs
// EXCEPT: EnvSpec and TargetStr do NOT move (they stay cross-vm), and
// CommonKeys.env changes type:
    /// Per-profile `env` override, opaque to the generic layer. After the
    /// merge stage this is the fully merged env table (profile keys overlaid
    /// on the top-level `[env]`). `None` when neither the profile nor the
    /// top-level declared env... see merge stage notes in Task 3.
    pub env: Option<serde_json::Value>,
```

### harness-cli

```rust
// src/domain.rs (NEW)
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

// src/resolve.rs (port of crates/framework/src/config/resolve.rs, de-chained)
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub seed: Option<u64>,
    pub ops: Option<usize>,
    pub cases: Option<usize>,
    pub duration: Option<std::time::Duration>,
    pub stats: Option<bool>,
    pub check_every: Option<usize>,
    pub json_report: Option<String>,
    pub artifacts_dir: Option<String>,
    pub no_shrink: bool,
    pub stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
// (same as the framework's RunOptions with `target` and `target_chains` REMOVED;
//  keep every doc comment, adjusted.)

#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub name: String,
    pub profile: harness_config::Profile,
    pub seed: harness_config::SeedSpec,
    /// The merged env table for this profile: the profile's own merged `env`
    /// key when present, else the top-level `[env]`, else `{}`.
    pub env: serde_json::Value,
    pub check_every: usize,
    pub stats: bool,
    pub shrink: bool,
    pub shrink_limit: usize,
    pub artifacts_dir: String,
    pub json_report: Option<String>,
    pub world_source: harness_config::WorldSource,
    pub stash_world: bool,
    pub phase_params: Option<toml::Table>,
}
pub fn resolve_profile<X: harness_config::ConfigExt>(
    cfg: &harness_config::RunConfig<X>,
    name: &str,
    opts: &RunOptions,
) -> Result<ResolvedProfile, harness_core::HarnessError>;

// src/registry.rs (port, Registry gains a Setup type parameter, Ctx bound dropped)
pub struct Registry<S> { /* entries: BTreeMap<String, Entry<S>> */ }
/// Per-run setup-request factory the CLI supplies (captures cfg + resolved +
/// domain args; the u64 is the concrete per-case seed).
pub type MakeSetup<'a, S> = &'a dyn Fn(u64) -> Result<S, SetupBuildError>;
impl<S: 'static> Registry<S> {
    pub fn new() -> Self;
    pub fn register<H, F, SF>(&mut self, name: &str, harness: F, setup: SF)
    where
        H: harness_core::Harness + 'static,
        H::Ctx: 'static,
        H::World: 'static,
        H::Operation: serde::Serialize + serde::de::DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + serde::de::DeserializeOwned + Copy + 'static,
        F: Fn() -> H + 'static,
        SF: Fn(S) -> SetupFuture<'static, H::Ctx, H::World> + 'static;
    pub fn register_persistent<H, F, SF>(&mut self, name: &str, harness: F, setup: SF)
        /* same bounds plus H::World: serde::Serialize */;
    // keep register_with_patch / register_persistent_with_patch with the same
    // Setup-parameter treatment if they exist in the source; port verbatim
    // otherwise.
    pub fn names(&self) -> Vec<&str>;
    pub fn validate(&self, harness: &str, profile: &harness_config::Profile) -> Result<(), RunError>;
    pub async fn run(
        &self,
        harness: &str,
        resolved: &ResolvedProfile,
        opts: &RunOptions,
        make_setup: MakeSetup<'_, S>,
    ) -> Result<ErasedReport, RunError>;
}
// RunError / ValidationError port verbatim from registry.rs.

// src/artifact.rs
pub fn write_replay_artifact<X: harness_config::ConfigExt>(
    dir: &std::path::Path,
    source: &harness_config::RunConfig<X>,
    resolved: &ResolvedProfile,
    report: &ErasedReport,
    domain_sections: toml::Table,
) -> std::io::Result<std::path::PathBuf>;

// src/cli.rs
pub struct Cli<D: CliDomain> { /* registry: Registry<D::Setup>, env_file: Option<PathBuf> */ }
impl<D: CliDomain> Cli<D> {
    pub fn new() -> Self;
    pub fn env_file(self, path: impl Into<std::path::PathBuf>) -> Self;
    pub fn register<H, F, SF>(self, name: &str, harness: F, setup: SF) -> Self
        /* same bounds as Registry::register with S = D::Setup */;
    pub fn register_persistent<H, F, SF>(self, name: &str, harness: F, setup: SF) -> Self;
    pub async fn main(self) -> std::process::ExitCode;
}

// src/erased.rs and src/report.rs: identical public surface to the framework
// originals (ErasedReport, ErasedFailure, erase_report, LocalBoxFuture,
// JsonReport, Invocation, write_json_report).

// src/test_bridge.rs
pub async fn run_profile_for_test<D, H, F, SF>(
    config_path: &str,
    harness: F,
    setup: SF,
    profile: &str,
    case: Option<usize>,
    expected_cases: Option<usize>,
) where
    D: CliDomain,
    /* H, F, SF bounds identical to Registry::register with S = D::Setup */;
```

### Phase 3 cross-vm types (produced by T15/T16, consumed by nothing new)

```rust
// crates/config (cross-vm-config)
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossVmExt {
    /// The `[[chain]]` declarations.
    #[serde(rename = "chain", default)]
    pub chain: Vec<ChainDecl>,
}
impl harness_config::ConfigExt for CrossVmExt { /* validate + merge_env_entry, Task 15 */ }
pub type RunConfig = harness_config::RunConfig<CrossVmExt>;
/// Parses an opaque env value into the typed cross-vm EnvSpec.
pub fn env_spec(env: &serde_json::Value) -> Result<EnvSpec, ConfigError>;

// crates/framework
pub struct CrossVmDomain;
#[derive(clap::Args, Debug, Clone, Default)]
pub struct TargetArgs {
    #[arg(long, value_parser = parse_target)]
    pub target: Option<Target>,
    #[arg(long = "target-chain", value_parser = parse_target_chain)]
    pub target_chain: Vec<(String, Target)>,
}
impl harness_cli::CliDomain for CrossVmDomain {
    type Ext = cross_vm_config::CrossVmExt;
    type Setup = SetupRequest;          // the existing framework struct
    type Args = TargetArgs;
    const BIN_NAME: &'static str = "cross-vm";
    const ABOUT: &'static str = "Config-driven cross-VM harness runner";
    const ENV_PREFIX: &'static str = "CROSS_VM";
    /* build_setup / artifact_sections / overrides_json in Task 16 */
}
pub type Cli = harness_cli::Cli<CrossVmDomain>;
```

---

## Phase 0

### Task 0: Branch, scaffold crates, workspace wiring

**Parallel group:** none (serial; everything depends on this).

**Files:**
- Modify: `/Volumes/euclid/personal/cross-vm-testing/Cargo.toml`
- Create: `crates/harness-config/Cargo.toml`
- Create: `crates/harness-config/src/lib.rs`
- Create: `crates/harness-cli/Cargo.toml`
- Create: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: two empty compiling crates named `harness-config` and `harness-cli`, registered in the workspace and in `[workspace.dependencies]`.

- [ ] **Step 1: Create the working branch**

```bash
cd /Volumes/euclid/personal/cross-vm-testing
git checkout -b anshu/feat/generic-cli anshu/feat/harness-crate
```

- [ ] **Step 2: Add workspace members and dependency entries**

In the root `Cargo.toml`, `[workspace] members` list, insert two lines directly after the `"crates/harness-macros",` line:

```toml
    "crates/harness-config",
    "crates/harness-cli",
```

In `[workspace.dependencies]`, directly after the `harness-core-macros = ...` line, add:

```toml
harness-config = { path = "crates/harness-config", version = "0.1.0" }
harness-cli = { path = "crates/harness-cli", version = "0.1.0" }
```

- [ ] **Step 3: Write `crates/harness-config/Cargo.toml`**

```toml
[package]
name = "harness-config"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Declarative TOML/JSON run-config schema for harness-core (pure data, no runtime deps), extensible via ConfigExt"

[dependencies]
serde = { workspace = true, features = ["derive"] }
toml.workspace = true
serde_json.workspace = true
humantime.workspace = true
thiserror.workspace = true

[lints]
workspace = true
```

- [ ] **Step 4: Write `crates/harness-config/src/lib.rs`**

```rust
//! Declarative TOML/JSON run-config schema for `harness-core`: parse,
//! interpolate, merge, typed deserialize, validate. Pure data, no runtime
//! dependencies. Domain layers extend it via [`ConfigExt`]; see the cross-vm
//! crates for a worked example.
```

(Module declarations arrive in Tasks 1 through 4.)

- [ ] **Step 5: Write `crates/harness-cli/Cargo.toml`**

```toml
[package]
name = "harness-cli"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "Config-driven CLI, registry, and run pipeline for harness-core, extensible via CliDomain"
repository.workspace = true

[dependencies]
harness-core = { workspace = true, features = ["serde"] }
harness-config.workspace = true
clap.workspace = true
tokio = { workspace = true, features = ["macros", "rt", "sync", "time", "signal"] }
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
toml.workspace = true
humantime.workspace = true
dotenvy.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
rand.workspace = true

[dev-dependencies]
tokio.workspace = true

[lints]
workspace = true
```

- [ ] **Step 6: Write `crates/harness-cli/src/lib.rs`**

```rust
//! Config-driven CLI, harness registry, and run pipeline over `harness-core`
//! and `harness-config`. Use raw via [`GenericDomain`], or implement
//! [`CliDomain`] to add domain config sections, CLI flags, and a custom setup
//! request type; see the cross-vm framework crate for a worked example.
```

(Module declarations arrive in Tasks 6 through 13.)

- [ ] **Step 7: Verify both crates compile**

Run: `cargo check -p harness-config -p harness-cli`
Expected: success (both crates are empty shells).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/harness-config crates/harness-cli
git commit -m "feat: scaffold harness-config and harness-cli crates"
```

---

## Phase 1 (dispatch group A chain and group B tasks in parallel)

### Task 1: harness-config primitive modules (value, interpolate, duration, seed)

**Parallel group:** A (first in chain).

**Files:**
- Create: `crates/harness-config/src/value.rs` (from `crates/config/src/value.rs`)
- Create: `crates/harness-config/src/interpolate.rs` (from `crates/config/src/interpolate.rs`)
- Create: `crates/harness-config/src/duration.rs` (from `crates/config/src/duration.rs`)
- Create: `crates/harness-config/src/seed.rs` (from `crates/config/src/seed.rs`)
- Modify: `crates/harness-config/src/lib.rs`

**Interfaces:**
- Consumes: nothing (self-contained ports).
- Produces: `pub trait Doc`, `pub trait DocMap`, `pub fn interpolate_value`, `pub(crate) fn interpolate_doc`, `pub mod humantime_duration`, `pub mod humantime_opt`, `pub enum SeedSpec` (exact shapes in the Interface Contract).

- [ ] **Step 1: Copy the four source files byte-for-byte**

```bash
cp crates/config/src/value.rs crates/harness-config/src/value.rs
cp crates/config/src/interpolate.rs crates/harness-config/src/interpolate.rs
cp crates/config/src/duration.rs crates/harness-config/src/duration.rs
cp crates/config/src/seed.rs crates/harness-config/src/seed.rs
```

- [ ] **Step 2: Make `value.rs` public**

In `crates/harness-config/src/value.rs`, change every `pub(crate) trait` to `pub trait` and every `pub(crate) fn` to `pub fn` for the `Doc` and `DocMap` items only (the `impl` blocks need no visibility change). Since `missing_docs = "warn"` now applies to these newly public items, confirm each trait, trait method, and public fn keeps or gains a rustdoc `///` comment (the source file already documents them; keep those comments).

- [ ] **Step 3: Fix intra-crate doc links and error type paths**

In all four copied files, any `crate::ConfigError` reference still resolves once Task 4 adds `ConfigError` to `lib.rs`. Until then, to keep this task independently compilable, add a TEMPORARY minimal error to `lib.rs` in this task (Task 4 replaces it with the full enum, same name and same variants used here, so nothing breaks):

```rust
/// Errors produced by the loader pipeline. (Extended in later tasks.)
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A `${VAR}` interpolation referenced an unset variable with no default.
    #[error("config interpolation: `${{{name}}}` at `{path}` is not set and has no default")]
    MissingVar {
        /// The variable name inside `${...}`.
        name: String,
        /// Dotted path to the value that referenced it.
        path: String,
    },
    /// Malformed `${...}` syntax.
    #[error("config interpolation at `{path}`: {message}")]
    Interpolation {
        /// Dotted path to the malformed value.
        path: String,
        /// What was malformed.
        message: String,
    },
}
```

IMPORTANT: before writing this enum, open `crates/config/src/lib.rs`, find the real `MissingVar` and `Interpolation` variant definitions, and copy their exact field names and `#[error]` strings instead of the guesses above. The interpolate.rs port must construct them identically to the source crate.

- [ ] **Step 4: Declare modules in `lib.rs`**

Append to `crates/harness-config/src/lib.rs`:

```rust
mod duration;
mod interpolate;
mod seed;
mod value;

pub use duration::{humantime_duration, humantime_opt};
pub use interpolate::interpolate_value;
pub use seed::SeedSpec;
pub use value::{Doc, DocMap};
```

(Match the source crate's actual re-export item names in `crates/config/src/lib.rs:39-47`; if `duration.rs` exposes differently named modules, mirror the source.)

- [ ] **Step 5: Run the ported unit tests**

The four source files carry `#[cfg(test)]` modules that came along with the copy.

Run: `cargo test -p harness-config`
Expected: PASS (all copied unit tests green).

- [ ] **Step 6: Commit**

```bash
git add crates/harness-config
git commit -m "feat(harness-config): port value, interpolate, duration, seed modules"
```

### Task 2: harness-config generic schema

**Parallel group:** A (after Task 1).

**Files:**
- Create: `crates/harness-config/src/schema.rs` (from `crates/config/src/schema.rs`)
- Modify: `crates/harness-config/src/lib.rs`

**Interfaces:**
- Consumes: `SeedSpec`, `humantime_*` (Task 1), `Doc` (Task 1).
- Produces: `HarnessRef`, `WorldSource`, `SuitePhase`, `Suite`, `ExpectStr`, `ScenarioStepRaw`, `CommonKeys` (with `env: Option<serde_json::Value>`), `FuzzProfile`, `InvariantProfile`, `EnduranceProfile`, `ScenarioProfile`, `Profile` (with `common()` and `pub(crate) from_mode_table`).

- [ ] **Step 1: Copy schema.rs**

```bash
cp crates/config/src/schema.rs crates/harness-config/src/schema.rs
```

- [ ] **Step 2: Remove the chain-domain types**

Delete from the copy: `pub enum TargetStr` (schema.rs:36 area) and `pub struct EnvSpec` (schema.rs:46 area), including their impls, wire structs, and doc comments. They stay in cross-vm-config.

- [ ] **Step 3: Re-type the env field on CommonKeys**

In `CommonKeys` (source line ~171) replace the field:

```rust
// old
pub env: Option<EnvSpec>,
// new
/// Per-profile `env` override, opaque to the generic layer. After the merge
/// stage this holds the fully merged env table (profile keys overlaid on the
/// top-level `[env]`). Domain layers re-type it (see the cross-vm crates).
pub env: Option<serde_json::Value>,
```

Apply the same change inside the private `*Wire` deserialize structs and their `From<Wire>` impls (the wire structs mirror `CommonKeys` field-for-field; every `EnvSpec` mention in this file becomes `serde_json::Value`). Compile errors are the checklist here: after this step `grep -n "EnvSpec\|TargetStr" crates/harness-config/src/schema.rs` must print nothing.

- [ ] **Step 4: Declare the module and re-exports in lib.rs**

Append to the module block in `crates/harness-config/src/lib.rs`:

```rust
mod schema;

pub use schema::{
    CommonKeys, EnduranceProfile, ExpectStr, FuzzProfile, HarnessRef, InvariantProfile,
    Profile, ScenarioProfile, ScenarioStepRaw, Suite, SuitePhase, WorldSource,
};
```

(Mirror the exact re-export list of `crates/config/src/lib.rs:42-45` minus `EnvSpec`/`TargetStr`.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p harness-config`
Expected: PASS. Any schema unit tests copied along that construct `EnvSpec` must be edited to use `serde_json::json!({...})` values instead; any test asserting env-typed fields moves its assertion to the raw JSON shape.

- [ ] **Step 6: Commit**

```bash
git add crates/harness-config
git commit -m "feat(harness-config): generic profile schema with opaque env"
```

### Task 3: ConfigExt trait and merge stage

**Parallel group:** A (after Task 2).

**Files:**
- Create: `crates/harness-config/src/ext.rs` (new code)
- Create: `crates/harness-config/src/merge.rs` (from `crates/config/src/merge.rs`)
- Modify: `crates/harness-config/src/lib.rs`

**Interfaces:**
- Consumes: `Doc`/`DocMap` (Task 1), `RunConfig<X>` (forward declaration lands in Task 4; `ext.rs`'s `validate` default body only names the type, so declare `ext.rs` in the same commit as a stub `RunConfig` if needed, or land `ext.rs` and `lib.rs`'s `RunConfig` together in Task 4; PREFERRED: write `ext.rs` here but only add `mod ext;` to lib.rs in Task 4 alongside `RunConfig`).
- Produces: `pub trait ConfigExt`, `pub struct NoExt`, `pub(crate) fn merge<V: Doc, X: ConfigExt>(root: &mut V) -> Result<Vec<String>, ConfigError>`.

- [ ] **Step 1: Write `ext.rs` exactly as specified in the Interface Contract**

Copy the `ConfigExt` trait and `NoExt` struct verbatim from the Interface Contract section, with full rustdoc comments.

- [ ] **Step 2: Copy merge.rs and make it generic over the extension**

```bash
cp crates/config/src/merge.rs crates/harness-config/src/merge.rs
```

Edits:
1. Change the entry signature from `pub fn merge<V: Doc>(root: &mut V) -> Result<Vec<String>, ConfigError>` to `pub(crate) fn merge<V: Doc, X: ConfigExt>(root: &mut V) -> Result<Vec<String>, ConfigError>` and add `use crate::ext::ConfigExt;`. Thread the `X` parameter down to the env-merge helper (the defaults-merge helpers do not need it).
2. Replace the body of the env-merge helper (`merge_env_tables` at source line ~167) with this generic version. Keep the original function's doc comment, updated to mention the hook:

```rust
/// Overlays a profile's own `env` table on the top-level `[env]`: the result
/// starts as a clone of the top-level table, then each profile-level key is
/// merged in via [`ConfigExt::merge_env_entry`] (default: replace). The
/// merged table is written back into the profile under `env`.
fn merge_env_tables<V: Doc, X: ConfigExt>(top_env: &V, profile_env: &mut V) {
    let Some(mut incoming) = std::mem::replace(profile_env, top_env.clone()).into_object()
    else {
        // A non-table profile `env` is left for typed deserialize to reject.
        return;
    };
    let Some(base) = profile_env.as_object_mut() else {
        // A non-table top-level `[env]` is left for typed deserialize to reject.
        return;
    };
    let keys: Vec<String> = incoming.iter().map(|(k, _)| k.clone()).collect();
    for key in keys {
        let value = incoming.remove(&key).expect("key came from this map");
        match base.get_mut(&key) {
            Some(slot) => X::merge_env_entry(&key, slot, value),
            None => base.insert(key, value),
        }
    }
}
```

CAUTION: before replacing, read the original `merge_env_tables` and its call site. If the original's caller semantics differ (for example it only merges when the profile HAS an `env` key, or it also copies the top-level `[env]` into profiles that lack one), preserve the original call-site semantics exactly and only swap the per-key `targets` special case for the `X::merge_env_entry` hook. The 3 hardcoded behaviors to remove are: label-wise merge for key `"targets"`, whole-value override for `"target"`/`"chains"`/`"params"`. Both collapse into the hook (default hook = whole-value override; cross-vm reintroduces the `targets` case in its `ConfigExt` impl in Task 15).
3. `COMMON_KEYS` (source line ~35) and `mode_specific_keys` (source line ~49) port verbatim (they are generic mode vocabulary).

- [ ] **Step 3: Compile check (module not yet wired)**

Add to lib.rs module block: `mod ext;` `mod merge;` and `pub use ext::{ConfigExt, NoExt};`. If `ext.rs`'s default `validate` body fails to compile because `RunConfig` does not exist yet, add the `RunConfig<X>` struct from the Interface Contract to lib.rs NOW (Task 4 will then only add the loader fns around it).

Run: `cargo check -p harness-config`
Expected: success.

- [ ] **Step 4: Unit test the merge hook**

Append to `crates/harness-config/src/merge.rs`'s test module (create one if the copy has none):

```rust
#[cfg(test)]
mod ext_hook_tests {
    use super::*;
    use crate::ext::{ConfigExt, NoExt};

    /// An ext whose hook deep-merges the `nested` env key label-wise.
    #[derive(Debug, Clone, Default, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DeepNested {}
    impl ConfigExt for DeepNested {
        fn merge_env_entry<V: crate::Doc>(key: &str, slot: &mut V, incoming: V) {
            if key == "nested" {
                if let (Some(base), Some(inc)) = (slot.as_object_mut(), incoming.clone().into_object()) {
                    let keys: Vec<String> = inc.iter().map(|(k, _)| k.clone()).collect();
                    let mut inc = inc;
                    for k in keys {
                        let v = inc.remove(&k).expect("key came from this map");
                        base.insert(k, v);
                    }
                    return;
                }
            }
            *slot = incoming;
        }
    }

    fn doc(s: &str) -> toml::Value {
        toml::from_str(s).expect("valid toml")
    }

    #[test]
    fn default_hook_replaces_whole_value() {
        let mut root = doc(
            "[env]\n[env.nested]\na = 1\nb = 2\n\n[profile.p]\nmode = \"fuzz\"\ncases = 1\nops = 1\n[profile.p.env]\n[profile.p.env.nested]\na = 9\n",
        );
        merge::<toml::Value, NoExt>(&mut root).expect("merge succeeds");
        let merged = &root["profile"]["p"]["env"]["nested"];
        assert_eq!(merged.get("a").and_then(|v| v.as_integer()), Some(9));
        assert_eq!(merged.get("b"), None, "default hook replaces, not deep-merges");
    }

    #[test]
    fn custom_hook_deep_merges_selected_key() {
        let mut root = doc(
            "[env]\n[env.nested]\na = 1\nb = 2\n\n[profile.p]\nmode = \"fuzz\"\ncases = 1\nops = 1\n[profile.p.env]\n[profile.p.env.nested]\na = 9\n",
        );
        merge::<toml::Value, DeepNested>(&mut root).expect("merge succeeds");
        let merged = &root["profile"]["p"]["env"]["nested"];
        assert_eq!(merged.get("a").and_then(|v| v.as_integer()), Some(9));
        assert_eq!(merged.get("b").and_then(|v| v.as_integer()), Some(2));
    }
}
```

Adjust the `merge::<toml::Value, NoExt>(...)` call spelling to the actual entry fn path/name from Step 2. If the original merge only runs `merge_env_tables` for profiles that declare `env`, these fixtures already declare it, so the tests hold either way.

- [ ] **Step 5: Run tests**

Run: `cargo test -p harness-config`
Expected: PASS, including both new hook tests.

- [ ] **Step 6: Commit**

```bash
git add crates/harness-config
git commit -m "feat(harness-config): ConfigExt seam and generic merge stage"
```

### Task 4: RunConfig<X>, loader pipeline, generic validation

**Parallel group:** A (after Task 3).

**Files:**
- Create: `crates/harness-config/src/validate.rs` (from `crates/config/src/validate.rs`)
- Modify: `crates/harness-config/src/lib.rs` (full pipeline)

**Interfaces:**
- Consumes: everything from Tasks 1 through 3.
- Produces: `RunConfig<X>`, `ConfigError` (final), `from_toml_str<X>`, `from_json_str<X>`, `load<X>` (exact shapes in the Interface Contract).

- [ ] **Step 1: Port validate.rs, generic checks only**

```bash
cp crates/config/src/validate.rs crates/harness-config/src/validate.rs
```

Edits:
1. Change signatures to `pub(crate) fn normalize_suite_phases<X: ConfigExt>(cfg: &mut RunConfig<X>) -> Result<(), ConfigError>` and `pub(crate) fn validate<X: ConfigExt>(cfg: &RunConfig<X>) -> Result<(), ConfigError>`.
2. DELETE the chain-specific validators and their call sites: `validate_chain_labels_unique`, `validate_chain_kind_non_empty`, `validate_chain_fields`, `validate_env_selection`, `validate_env_targets`, `validate_rpc_urls` (they move to cross-vm in Task 15).
3. KEEP: `validate_suite_phase_structure`, `is_single_setup`, `validate_suites`, `validate_profile_mode_specific`, and everything `normalize_suite_phases` needs.
4. At the END of `validate`, after all generic checks pass, add:

```rust
    X::validate(cfg)?;
    Ok(())
```

- [ ] **Step 2: Write the final ConfigError in lib.rs**

Replace the temporary enum from Task 1 with the full generic enum: open `crates/config/src/lib.rs:76` onward, copy every variant EXCEPT `DuplicateChainLabel`, `MissingChainFields`, `EmptyChainKind`, `UnknownChainSelection`, `UnknownTargetLabel`, `InvalidChainTarget`, `MissingRpcUrl` (keep exact field names, doc comments, and `#[error]` strings), then append the new variant:

```rust
    /// A domain extension ([`ConfigExt::validate`]) rejected the config.
    #[error("{0}")]
    Domain(String),
```

- [ ] **Step 3: Write RunConfig<X> and the pipeline in lib.rs**

Add the `RunConfig<X>` struct exactly as in the Interface Contract (skip if already added in Task 3 Step 3). Then port the pipeline from `crates/config/src/lib.rs:296-428` with these exact transformations:

1. `from_toml_str`, `from_json_str`, `load`, `load_from_value` all gain `<X: ConfigExt>` and return `Result<RunConfig<X>, ConfigError>`; `load_from_value` calls `merge::merge::<V, X>(&mut value)` and `validate::normalize_suite_phases::<X>` / `validate::validate::<X>`.
2. DELETE `RawRunConfig<V>` entirely. Replace `build_run_config` with the pop-keys version:

```rust
/// Deserializes the stable-shaped parts of the document (popping each generic
/// top-level key off the root table), hands every remaining top-level key to
/// the domain extension `X`, then dispatches every profile table into its
/// per-mode struct by mode name.
fn build_run_config<V: Doc, X: ConfigExt>(value: V) -> Result<RunConfig<X>, ConfigError> {
    let mut root = value.into_object().ok_or_else(|| ConfigError::Deserialize {
        path: "<root>".to_string(),
        message: "config root must be a table".to_string(),
    })?;

    let harness: HarnessRef = match root.remove("harness") {
        Some(v) => v.deserialize_into().map_err(|message| ConfigError::Deserialize {
            path: "harness".to_string(),
            message,
        })?,
        None => {
            return Err(ConfigError::Deserialize {
                path: "<root>".to_string(),
                message: "missing required key `harness`".to_string(),
            })
        }
    };

    let env: serde_json::Value = match root.remove("env") {
        Some(v) => v.deserialize_into().map_err(|message| ConfigError::Deserialize {
            path: "env".to_string(),
            message,
        })?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };

    let profile_tables: std::collections::BTreeMap<String, V> = match root.remove("profile") {
        Some(v) => v.deserialize_into().map_err(|message| ConfigError::Deserialize {
            path: "profile".to_string(),
            message,
        })?,
        None => std::collections::BTreeMap::new(),
    };

    let suites: std::collections::BTreeMap<String, Suite> = match root.remove("suite") {
        Some(v) => v.deserialize_into().map_err(|message| ConfigError::Deserialize {
            path: "suite".to_string(),
            message,
        })?,
        None => std::collections::BTreeMap::new(),
    };

    // `[replay]` is provenance in replay artifacts; tolerated and dropped,
    // exactly as the pre-extraction loader did.
    let _ = root.remove("replay");

    // Everything left at the top level belongs to the domain extension. With
    // `NoExt` (deny_unknown_fields over an empty struct) any leftover key is a
    // hard error, preserving the old `deny_unknown_fields` behavior.
    let ext: X = V::from_object(root)
        .deserialize_into()
        .map_err(|message| ConfigError::Deserialize {
            path: "<root>".to_string(),
            message,
        })?;

    let mut profiles = std::collections::BTreeMap::new();
    for (name, mut profile_value) in profile_tables {
        // (port the mode-pop + Profile::from_mode_table dispatch loop verbatim
        //  from crates/config/src/lib.rs:389-418, unchanged)
    }

    Ok(RunConfig {
        harness,
        env,
        profiles,
        suites,
        warnings: Vec::new(),
        ext,
    })
}
```

Port the profile dispatch loop body verbatim from the source (lines 389 through 418).

- [ ] **Step 4: Run tests**

Run: `cargo test -p harness-config`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/harness-config
git commit -m "feat(harness-config): RunConfig<X> loader pipeline with ext seam"
```

### Task 5: harness-config generic fixture suite

**Parallel group:** A (after Task 4; closes track A).

**Files:**
- Create: `crates/harness-config/tests/loader_pipeline.rs`
- Create: `crates/harness-config/tests/fixtures/*.toml`

**Interfaces:**
- Consumes: the full Task 4 pipeline with `NoExt`.
- Produces: regression coverage; nothing downstream consumes it.

- [ ] **Step 1: Build generic fixtures from the cross-vm set**

Copy `crates/config/tests/fixtures/` to `crates/harness-config/tests/fixtures/`, then in EVERY copied fixture delete all `[[chain]]` blocks and every chain-domain `env` key that would matter to typed cross-vm validation (`target`, `targets`, `chains` keys may STAY, because env is opaque to the generic layer and must round-trip unknown keys; only `[[chain]]` blocks must go, since `NoExt` rejects unknown top-level keys). Delete fixtures that exist solely to trigger the 7 removed chain error variants (the `bad_*` files whose loader_pipeline assertion targets `DuplicateChainLabel`, `MissingChainFields`, `EmptyChainKind`, `UnknownChainSelection`, `UnknownTargetLabel`, `InvalidChainTarget`, or `MissingRpcUrl`).

- [ ] **Step 2: Port the pipeline test file**

Copy `crates/config/tests/loader_pipeline.rs`, then:
1. Replace every loader call with the turbofish form: `harness_config::from_toml_str::<harness_config::NoExt>(...)`, `harness_config::load::<harness_config::NoExt>(...)`.
2. Delete the test fns for the deleted fixtures.
3. Add one new test proving the ext seam rejects unknown top-level keys:

```rust
#[test]
fn unknown_top_level_key_is_rejected_by_noext() {
    let err = harness_config::from_toml_str::<harness_config::NoExt>(
        "[harness]\nname = \"h\"\n\n[[chain]]\nlabel = \"eth\"\n",
        &|_| None,
    )
    .expect_err("chain is not a generic key");
    let msg = err.to_string();
    assert!(msg.contains("chain"), "error names the offending key: {msg}");
}
```

4. Add one test proving env round-trips opaquely:

```rust
#[test]
fn env_round_trips_opaquely() {
    let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
        "[harness]\nname = \"h\"\n\n[env]\ntarget = \"mock\"\n[env.params]\nusers = 2\n\n[profile.p]\nmode = \"fuzz\"\ncases = 1\nops = 1\n",
        &|_| None,
    )
    .expect("generic layer does not interpret env");
    assert_eq!(cfg.env["target"], "mock");
    assert_eq!(cfg.env["params"]["users"], 2);
}
```

- [ ] **Step 3: Run and commit**

Run: `cargo test -p harness-config`
Expected: PASS.

```bash
git add crates/harness-config/tests
git commit -m "test(harness-config): generic loader pipeline fixture suite"
```

### Task 6: harness-cli erased report module

**Parallel group:** B (independent of track A; dispatch alongside Task 1 and Task 7).

**Files:**
- Create: `crates/harness-cli/src/erased.rs` (from `crates/framework/src/config/erased.rs`)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: `harness_core::{Coverage, FailureKind, RunReport, Stats}` only.
- Produces: `ErasedReport`, `ErasedFailure`, `pub(crate) fn erase_report`, `pub(crate) type LocalBoxFuture`.

- [ ] **Step 1: Copy and rewire imports**

```bash
cp crates/framework/src/config/erased.rs crates/harness-cli/src/erased.rs
```

Replace `use crate::harness::{...}` (or `use harness_core::...` via the framework re-export path) with direct `use harness_core::{Coverage, FailureKind, RunReport, Stats};`. No other logic changes; this file is already VM-free.

- [ ] **Step 2: Wire the module**

Append to `crates/harness-cli/src/lib.rs`:

```rust
mod erased;

pub use erased::{ErasedFailure, ErasedReport};
```

Keep `erase_report` and `LocalBoxFuture` at `pub(crate)` exactly as the source has them.

- [ ] **Step 3: Run tests and commit**

Run: `cargo test -p harness-cli`
Expected: PASS (the file's own unit tests, if any, come along).

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): port erased report types"
```

### Task 7: harness-cli JSON report module

**Parallel group:** B (independent; dispatch alongside Task 1 and Task 6). If Task 6 has not merged yet when this starts, this task still compiles only after Task 6 lands (it imports `ErasedReport`); in a shared-worktree execution run Task 6 and Task 7 in the same subagent back-to-back instead.

**Files:**
- Create: `crates/harness-cli/src/report.rs` (from `crates/framework/src/config/report.rs`)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: `ErasedReport` (Task 6), `harness_core::FailureKind` (tests).
- Produces: `JsonReport`, `Invocation`, `write_json_report` (signatures unchanged from the source file).

- [ ] **Step 1: Copy and rewire imports**

```bash
cp crates/framework/src/config/report.rs crates/harness-cli/src/report.rs
```

Edits: `use super::erased::ErasedReport;` becomes `use crate::erased::ErasedReport;`. In the test module, `use crate::config::ErasedFailure;` becomes `use crate::erased::ErasedFailure;` and `use crate::harness::FailureKind;` becomes `use harness_core::FailureKind;`.

- [ ] **Step 2: Wire the module**

Append to lib.rs:

```rust
mod report;

pub use report::{write_json_report, Invocation, JsonReport};
```

- [ ] **Step 3: Run tests and commit**

Run: `cargo test -p harness-cli`
Expected: PASS, including the two `write_json_report_*` tests.

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): port json report envelope"
```

---

## Phase 2 (needs Phase 1 complete)

### Task 8: harness-cli domain seam (CliDomain, GenericDomain)

**Parallel group:** C (dispatch alongside Task 9).

**Files:**
- Create: `crates/harness-cli/src/domain.rs` (new code)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: `harness_config::{ConfigExt, NoExt, RunConfig}`, `ResolvedProfile` (Task 9; if executing before Task 9 lands, this file's `build_setup` signature references it, so in a shared worktree run Tasks 8 and 9 in one subagent, Task 9 first).
- Produces: `SetupFuture`, `SetupBuildError`, `CliDomain`, `GenericDomain`, `NoArgs`, `BasicSetup` (exact code in the Interface Contract).

- [ ] **Step 1: Write domain.rs**

Copy every item from the Interface Contract's `src/domain.rs` block verbatim, including all rustdoc comments. Add at the top:

```rust
//! The domain seam: [`CliDomain`] lets a variant add config sections, CLI
//! flags, and its own setup request type; [`GenericDomain`] is the raw,
//! batteries-included implementation.

use crate::resolve::ResolvedProfile;
```

- [ ] **Step 2: Wire and test**

Append to lib.rs:

```rust
mod domain;

pub use domain::{BasicSetup, CliDomain, GenericDomain, NoArgs, SetupBuildError, SetupFuture};
```

Add a compile-shape unit test at the bottom of domain.rs:

```rust
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
```

Run: `cargo test -p harness-cli`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): CliDomain seam with GenericDomain default"
```

### Task 9: harness-cli profile resolution

**Parallel group:** C (dispatch alongside Task 8; see Task 8's note about ordering within one worktree).

**Files:**
- Create: `crates/harness-cli/src/resolve.rs` (from `crates/framework/src/config/resolve.rs`)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: `harness_config::{RunConfig, ConfigExt, Profile, SeedSpec, WorldSource}`, `harness_core::HarnessError`.
- Produces: `RunOptions`, `ResolvedProfile`, `resolve_profile<X>` (exact shapes in the Interface Contract).

- [ ] **Step 1: Copy and de-chain**

```bash
cp crates/framework/src/config/resolve.rs crates/harness-cli/src/resolve.rs
```

Edits, in order:
1. Imports: drop `cross_vm_core`, drop `super::setup_request::{ChainSpecData, Target}`, drop `resolve_chain_target`/`TargetOverrides`/`TargetStr` from the config import; import from `harness_config` instead of `cross_vm_config`; `use harness_core::HarnessError;`.
2. `RunOptions`: delete the `target` and `target_chains` fields and their doc comments. Everything else ports verbatim (including the `stop` field and its doc).
3. `ResolvedProfile`: delete `chain_specs`, `target`, and `params` fields; add the `env: serde_json::Value` field with the doc comment from the Interface Contract. All other fields port verbatim.
4. Delete the now-dead helpers `default_native_symbol` and `target_to_str`, and every chain-resolution block inside `resolve_profile` (the code that builds `ChainSpecData` values, resolves targets, and re-asserts `rpc_url`). That logic moves to the framework domain adapter in Task 16.
5. In `resolve_profile`, where the old code computed `params` from the merged env, compute `env` instead:

```rust
    // The loader already merged the profile's own `env` over the top-level
    // `[env]`; a profile with no `env` key falls back to the top-level table.
    let env: serde_json::Value = profile
        .common()
        .env
        .clone()
        .unwrap_or_else(|| cfg.env.clone());
```

6. Make the fn generic: `pub fn resolve_profile<X: ConfigExt>(cfg: &RunConfig<X>, name: &str, opts: &RunOptions) -> Result<ResolvedProfile, HarnessError>`.
7. Keep the unknown-profile error (with its sorted name listing), the seed folding (`opts.seed` to `SeedSpec::Fixed`, else profile seed), the `check_every`/`stats` folding, the shrink mode-default logic (`true` fuzz/invariant, `false` endurance/scenario, `no_shrink` forces false), `shrink_limit`, `artifacts_dir`, `json_report` folding, and the `world_source: Fresh` / `stash_world: false` / `phase_params: None` defaults. These are the load-bearing precedence rules; port them line-for-line.

- [ ] **Step 2: Port the file's unit tests**

The source file's `#[cfg(test)]` module tests precedence logic. Keep every test that does not touch chains/targets; for tests that construct configs with `[[chain]]`, delete the chain blocks from their TOML strings (env keys may stay, they are opaque now). Replace `cross_vm_config::from_toml_str(...)` with `harness_config::from_toml_str::<harness_config::NoExt>(...)`. Delete tests that exist solely to check chain/target resolution.

- [ ] **Step 3: Wire, run, commit**

Append to lib.rs:

```rust
mod resolve;

pub use resolve::{resolve_profile, ResolvedProfile, RunOptions};
```

Run: `cargo test -p harness-cli`
Expected: PASS.

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): generic profile resolution"
```

### Task 10: harness-cli registry

**Parallel group:** D (dispatch alongside Task 11).

**Files:**
- Create: `crates/harness-cli/src/registry.rs` (from `crates/framework/src/config/registry.rs`)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: `ResolvedProfile`/`RunOptions` (Task 9), `SetupFuture`/`SetupBuildError`/`MakeSetup` (Task 8), `erase_report`/`ErasedReport`/`LocalBoxFuture` (Task 6), the full harness-core runner surface.
- Produces: `Registry<S>`, `MakeSetup`, `RunError`, `ValidationError`, `pub(crate) run_one_fuzz_case` (exact shapes in the Interface Contract).

- [ ] **Step 1: Copy and apply the generalization edits**

```bash
cp crates/framework/src/config/registry.rs crates/harness-cli/src/registry.rs
```

Edits, in order:
1. Imports: `cross_vm_config::X` becomes `harness_config::X` throughout; `use crate::harness::Ctx;` is DELETED; harness-core items import directly (`use harness_core::{...};` mirroring the source's list at registry.rs:39-42).
2. `Registry` becomes `pub struct Registry<S>` holding `entries: BTreeMap<String, Entry<S>>`. `Entry`, `RunFn`, and the register fns gain the `S` parameter.
3. `SessionSlot<W>` (which names the concrete `Ctx`) becomes generic over both parts:

```rust
/// Session state one pipeline phase hands to the next: the live context and
/// world of a passed run, stashed for an `Inherit` successor.
type SessionSlot<C, W> = std::rc::Rc<std::cell::RefCell<Option<(C, W)>>>;
```

Every use site `SessionSlot<H::World>` becomes `SessionSlot<H::Ctx, H::World>`.
4. `MakeSetup` alias: add to this file (and re-export from lib.rs):

```rust
/// Per-run setup-request factory the CLI supplies; the `u64` is the concrete
/// per-case seed. Building can fail (e.g. a domain flag names an unknown
/// chain), so the factory returns a [`SetupBuildError`].
pub type MakeSetup<'a, S> = &'a dyn Fn(u64) -> Result<S, crate::domain::SetupBuildError>;
```

5. `RunFn` gains the factory parameter:

```rust
type RunFn<S> = Box<
    dyn for<'a> Fn(
        &'a ResolvedProfile,
        &'a RunOptions,
        MakeSetup<'a, S>,
    ) -> LocalBoxFuture<'a, Result<ErasedReport, RunError>>,
>;
```

6. The old internal `build_setup_request(resolved, seed)` helper (registry.rs:561 area) is DELETED; every call site becomes `make_setup(seed)`, mapping `Err(SetupBuildError::Usage(m))` to `RunError::Invalid(m)` and `Err(SetupBuildError::Infra(m))` to `RunError::Setup(m)` (match the actual `RunError` variant payload shapes from the source; if `RunError::Setup` wraps an error type rather than a String, wrap the message accordingly).
7. `register`/`register_persistent`/`register_with_patch`/`register_persistent_with_patch` bounds: replace `H: Harness<Ctx = crate::harness::Ctx> + 'static` with `H: Harness + 'static, H::Ctx: 'static, H::World: 'static`; the setup fn bound becomes `SF: Fn(S) -> SetupFuture<'static, H::Ctx, H::World> + 'static`.
8. `Registry::run` gains the `make_setup: MakeSetup<'_, S>` parameter and threads it into the stored `RunFn`.
9. `run_profile<H, F, SF>` (the big per-mode driver, registry.rs:747-1060) keeps its entire mode logic UNCHANGED (fuzz case loop with `sub_seed`, `KindMix` construction via `parse_kind_selection`, `EnduranceConfig` assembly, scenario `ScenarioStep` mapping, `maybe_shrink` via `shrink_with_limit`, stash/inherit via the session slot). The only changes are the setup call sites (point 6) and the type parameters.
10. `run_one_fuzz_case` stays `pub(crate)` and gains the same treatment.

- [ ] **Step 2: Port the registry unit tests**

The source file's test module (registry.rs after line ~1100) uses a mock harness with the framework `Ctx`. In the port, redefine the mock with `type Ctx = u32;` (or `()`), build `Registry::<TestSetup>::new()` where `TestSetup` is a tiny local struct `struct TestSetup { seed: u64 }`, and pass `&|seed| Ok(TestSetup { seed })` as `make_setup`. Keep every behavioral assertion (mode dispatch, seed derivation, kind selection, shrink behavior, stash/inherit) intact. Delete tests that construct `SetupRequest`/`ChainSpecData` shapes; those semantics now live in the framework adapter and stay covered there.

- [ ] **Step 3: Wire, run, commit**

Append to lib.rs:

```rust
mod registry;

pub use registry::{MakeSetup, Registry, RunError, ValidationError};
```

Run: `cargo test -p harness-cli`
Expected: PASS.

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): registry generic over setup request type"
```

### Task 11: harness-cli replay artifacts

**Parallel group:** D (dispatch alongside Task 10). Both need Tasks 6, 8, 9; they do not need each other.

**Files:**
- Create: `crates/harness-cli/src/artifact.rs` (from `crates/framework/src/config/artifact.rs`)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: `ErasedReport`/`ErasedFailure` (Task 6), `ResolvedProfile` (Task 9), `harness_config::{RunConfig, ConfigExt, WorldSource}`, `harness_core::FailureKind`.
- Produces: `write_replay_artifact<X>` with the `domain_sections: toml::Table` parameter (Interface Contract).

- [ ] **Step 1: Copy and generalize**

```bash
cp crates/framework/src/config/artifact.rs crates/harness-cli/src/artifact.rs
```

Edits:
1. Imports: `cross_vm_config::RunConfig` becomes `harness_config::RunConfig<X>`; drop `ChainSpecData`/`Target` imports; `use crate::harness::FailureKind;` becomes `use harness_core::FailureKind;`; `super::erased`/`super::resolve` become `crate::erased`/`crate::resolve`.
2. Signature becomes the Interface Contract's `write_replay_artifact<X: ConfigExt>(dir, source, resolved, report, domain_sections)`.
3. DELETE: `target_str`, `artifact_chain`, `ArtifactChain`, `ArtifactEnv`, and the `chains`/`env` fields of `Artifact` (cross-vm re-adds chains and a resolved env through `domain_sections` and the generic env embed below).
4. `Artifact` gains a generic env embed and the struct serialization gains domain-section merging. Replace the `Artifact` assembly and serialization with:

```rust
/// The whole artifact document: a valid generic `RunConfig` (`[harness]`,
/// `[env]`) plus `[replay]` provenance (tolerated, ignored by the run schema)
/// and one concrete `[profile.replay]` scenario profile holding the (possibly
/// shrunk) failing history. Domain sections (e.g. cross-vm's `[[chain]]`) are
/// merged in as extra top-level tables before serialization.
#[derive(Debug, serde::Serialize)]
struct Artifact {
    harness: ArtifactHarness,
    /// The failing profile's fully merged env table, embedded so a replay
    /// resolves the same environment. Skipped when empty.
    #[serde(skip_serializing_if = "env_is_empty")]
    env: serde_json::Value,
    replay: ArtifactReplayMeta,
    profile: ArtifactProfileWrapper,
}

/// True when the env value is an empty object (nothing worth embedding).
fn env_is_empty(env: &serde_json::Value) -> bool {
    env.as_object().map(|m| m.is_empty()).unwrap_or(false)
}
```

In `build_artifact`, set `env: resolved.env.clone()` and drop the `chains`/`chain_labels` code. `ArtifactHarness`, `ArtifactReplayMeta` (all its fields including `world_source`/`phase_params`), `ArtifactStep`, `ArtifactReplayProfile`, `ArtifactProfileWrapper`, `failure_summary`, and `unix_timestamp` port verbatim.
5. Domain-section merging in `write_replay_artifact`, replacing the direct `toml::to_string_pretty(&artifact)` call:

```rust
    // Merge the domain's extra top-level sections (e.g. `[[chain]]`) into the
    // generic artifact before serializing, so the artifact stays a loadable
    // config for the domain's own `Ext`.
    let merged_toml: Result<toml::Table, toml::ser::Error> =
        toml::Table::try_from(&artifact).map(|mut table| {
            for (key, value) in domain_sections.clone() {
                table.insert(key, value);
            }
            table
        });

    match merged_toml.and_then(|t| toml::to_string_pretty(&t)) {
        Ok(text) => { /* write <stem>.replay.toml exactly as the source did */ }
        Err(e) => {
            // JSON fallback: same value, same merge, via serde_json.
            let mut json = serde_json::to_value(&artifact).map_err(std::io::Error::other)?;
            if let Some(obj) = json.as_object_mut() {
                for (key, value) in domain_sections {
                    let jv = serde_json::to_value(value).map_err(std::io::Error::other)?;
                    obj.insert(key, jv);
                }
            }
            /* write <stem>.replay.json exactly as the source did, logging `e`
               at debug level with the source's message */
        }
    }
```

Keep the source's fallback rationale comments (the u128/TOML range note) on the new code.

- [ ] **Step 2: Port or write tests**

Port any test module, replacing chain assertions with: (a) a test that a failing report writes a `.replay.toml` whose parsed table contains `harness`, `replay`, `profile.replay.mode == "scenario"`, and the embedded `env`; (b) a test that `domain_sections` containing a `chain` array lands as a top-level `chain` key in the parsed output; (c) a test that an op history containing a `u64::MAX` integer falls back to `.replay.json`.

- [ ] **Step 3: Wire, run, commit**

Append to lib.rs:

```rust
mod artifact;

pub use artifact::write_replay_artifact;
```

Run: `cargo test -p harness-cli`
Expected: PASS.

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): replay artifacts with domain section hook"
```

### Task 12: harness-cli clap CLI

**Parallel group:** E (dispatch alongside Task 13).

**Files:**
- Create: `crates/harness-cli/src/cli.rs` (from `crates/framework/src/cli.rs`)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: everything from Tasks 6 through 11.
- Produces: `Cli<D>` (Interface Contract), plus `pub` building blocks for domain variants: `PhasePlan`, `select_phases`, `build_run_options`, `exit_code_for`, `exit_code_for_run_error`, `combine`.

- [ ] **Step 1: Copy and generalize the arg model**

```bash
cp crates/framework/src/cli.rs crates/harness-cli/src/cli.rs
```

Arg-model edits:
1. All `cross_vm_config::` paths become `harness_config::`; `super::config::...` imports become `crate::...`.
2. The clap structs gain the domain flag parameter. Exact new shapes:

```rust
#[derive(clap::Parser, Debug)]
struct CliArgs<A: clap::Args> {
    #[command(subcommand)]
    command: Command<A>,
}

#[derive(clap::Subcommand, Debug)]
enum Command<A: clap::Args> {
    /// Run profiles or a suite from a config file.
    Run(RunArgs<A>),
    /// Type-check a config against the registered harnesses without running.
    Validate(ConfigArgs),
    /// List registered harnesses and the config's profiles and suites.
    List(ConfigArgs),
    /// Re-run a replay artifact (sugar for `run <artifact> --profile replay`).
    Replay(ReplayArgs<A>),
}

#[derive(clap::Args, Debug)]
struct RunArgs<A: clap::Args> {
    // ... every generic flag from the source's RunArgs, verbatim, EXCEPT
    // `target` and `target_chain`, which are DELETED here ...
    /// Domain-specific flags (e.g. cross-vm's --target/--target-chain).
    #[command(flatten)]
    domain: A,
}

#[derive(clap::Args, Debug)]
struct ReplayArgs<A: clap::Args> {
    /// Path to a replay artifact written by a failed run.
    artifact: std::path::PathBuf,
    /// Domain-specific flags.
    #[command(flatten)]
    domain: A,
}
```

`parse_target` and `parse_target_chain` are DELETED (they move to the framework in Task 16). The clap `name`/`about` attributes on `CliArgs` are removed; naming happens at parse time (Step 3).

- [ ] **Step 2: Generalize the builder and dispatch**

1. `pub struct Cli<D: CliDomain> { registry: Registry<D::Setup>, env_file: Option<PathBuf> }`; `new`/`env_file`/`register`/`register_persistent` keep their shapes with the bounds from the Interface Contract.
2. `main`'s parse step becomes:

```rust
        let cmd = <CliArgs<D::Args> as clap::CommandFactory>::command()
            .name(D::BIN_NAME)
            .about(D::ABOUT);
        let args = match cmd.try_get_matches() {
            Ok(matches) => {
                match <CliArgs<D::Args> as clap::FromArgMatches>::from_arg_matches(&matches) {
                    Ok(args) => args,
                    Err(e) => return exit_for_clap_error(e),
                }
            }
            Err(e) => return exit_for_clap_error(e),
        };
```

where `exit_for_clap_error` reproduces the source's behavior: `--help`/`--version` (clap `ErrorKind::DisplayHelp`/`DisplayVersion`) print and exit 0, anything else prints and exits 3. Read the source's existing `try_parse` handling and mirror it exactly through this two-step API.
3. Keep verbatim: the current-thread runtime assert, tracing init, dotenvy loading, ctrl-c task wiring `opts.stop`.
4. `load_config` becomes `harness_config::load::<D::Ext>(path, &std_env_lookup)`.
5. Env folding: `build_run_options` and `select_phases` take the prefix. Replace the hardcoded strings: `"CROSS_VM_SEED"` becomes `&format!("{}_SEED", D::ENV_PREFIX)` (same for `_CASES`, `_OPS`, `_PROFILE`); `PROPTEST_CASES` stays literal. To keep the pure fns testable, give them the prefix as a parameter:

```rust
fn build_run_options<A: clap::Args>(
    args: &RunArgs<A>,
    env: &dyn Fn(&str) -> Option<String>,
    prefix: &str,
) -> RunOptions
```

and the same for `select_phases`. Port their bodies verbatim otherwise, minus the target fields.
6. `run_selected` gains the domain wiring. New signature and the three insertion points:

```rust
async fn run_selected<D: CliDomain>(
    registry: &Registry<D::Setup>,
    cfg: &harness_config::RunConfig<D::Ext>,
    phases: Vec<PhasePlan>,
    opts: RunOptions,
    stop_on_failure: bool,
    config_path: &str,
    domain_args: &D::Args,
) -> u8
```

(a) Where the old code called `registry.run(&harness, &resolved, &opts)`, build the factory first:

```rust
        let make_setup = |seed: u64| D::build_setup(cfg, &resolved, domain_args, seed);
        let outcome = registry.run(&harness_name, &resolved, &opts, &make_setup).await;
```

(b) Where the old code called `write_replay_artifact(dir, cfg, &resolved, &report)`, pass the domain sections:

```rust
        let sections = D::artifact_sections(cfg, &resolved, domain_args);
        let written = write_replay_artifact(dir, cfg, &resolved, &report, sections);
```

(c) Where the old code built `overrides_json(&opts)`, merge in the domain's flags:

```rust
        let mut overrides = overrides_json(&opts);
        if let serde_json::Value::Object(map) = &mut overrides {
            map.extend(D::overrides_json(domain_args));
        }
```

7. Everything else ports verbatim: `PhasePlan`/`fresh_phase`, phase dependency gating, world stash/inherit wiring, exit-code fns (`exit_code_for`, `exit_code_for_run_error`, `severity_rank`, `combine`), `log_profile_result`, `target_label` DELETED, `validate_with_config`/`list_with_config` (these two only touch generic types).
8. Make these building blocks `pub` with rustdoc (domain variants and their tests reuse them): `PhasePlan`, `select_phases`, `build_run_options`, `exit_code_for`, `exit_code_for_run_error`, `combine`, `overrides_json`.

- [ ] **Step 3: Port the test suite**

The source test module (cli.rs:1023-2182) splits cleanly:
- PORT with mechanical adaptation (mock harness gets `type Ctx = u32;`, `Cli` becomes `Cli<GenericDomain>`, config strings lose `[[chain]]` blocks, `CROSS_VM_` env keys become `HARNESS_` or pass the prefix parameter): exit-code tests (`exit_code_*`, `combine_*`), arg-parse tests minus target ones, env-folding tests (`cli_seed_wins_over_env`, `env_seed_used_when_no_cli_flag`, `neither_cli_nor_env_seed...`, `cases_folds_from_...` renamed for the generic prefix, `stats_flag_present_...`), selection tests (`single_profile_config_auto_selects` through `cross_vm_profile_env_selects_when_no_flag_given`, renamed), overrides tests, run tests (`run_with_config_*`), JSON report tests, replay artifact tests, pipeline tests (`pipeline_suite_*`, `independent_phases_*`, `legacy_profiles_key_*`).
- DO NOT PORT (stay framework-only, Task 17): `parses_run_with_two_profiles_a_target_chain_and_seed`, `bad_target_chain_without_equals_is_a_parse_error`, `bad_target_value_is_a_parse_error`.
The mock setup fn becomes `fn mock_setup(req: BasicSetup) -> SetupFuture<'static, u32, u32>` (adjust the world type to whatever the source mock used).

- [ ] **Step 4: Wire, run, commit**

Append to lib.rs:

```rust
mod cli;

pub use cli::{
    build_run_options, combine, exit_code_for, exit_code_for_run_error, overrides_json,
    select_phases, Cli, PhasePlan,
};
```

Run: `cargo test -p harness-cli`
Expected: PASS (the ported suite is the bulk of harness-cli's coverage).

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): generic clap CLI over CliDomain"
```

### Task 13: harness-cli test bridge

**Parallel group:** E (dispatch alongside Task 12; needs only Task 10).

**Files:**
- Create: `crates/harness-cli/src/test_bridge.rs` (from `crates/framework/src/config/test_bridge.rs`)
- Modify: `crates/harness-cli/src/lib.rs`

**Interfaces:**
- Consumes: `Registry<S>`/`run_one_fuzz_case` (Task 10), `resolve_profile` (Task 9), `CliDomain` (Task 8).
- Produces: `run_profile_for_test<D, H, F, SF>` (Interface Contract).

- [ ] **Step 1: Copy and generalize**

```bash
cp crates/framework/src/config/test_bridge.rs crates/harness-cli/src/test_bridge.rs
```

Edits: imports as in prior tasks; fn gains the `D: CliDomain` parameter and the register/setup bounds from the Interface Contract; config loads via `harness_config::load::<D::Ext>`; where the old code built setup requests, use `let make_setup = |seed: u64| D::build_setup(&cfg, &resolved, &D::Args::default(), seed);` (test runs use default domain args; document this on the fn). Panic messages port verbatim.

- [ ] **Step 2: Wire, add a smoke test, run, commit**

Append to lib.rs: `mod test_bridge;` and `pub use test_bridge::run_profile_for_test;` (mirror the source's module visibility: if the framework exposes it as `config::test_bridge::run_profile_for_test`, expose here as `pub mod test_bridge;` instead, keeping the fn `pub` inside).

Add an integration smoke test `crates/harness-cli/tests/generic_bridge.rs` using a trivial inline harness (`Ctx = ()`, `World = i64`, one `Add(i64)` op with an `i64` model invariant, modeled on `crates/harness/tests/pure_function.rs`) plus an inline config file written to a temp dir with one `[profile.smoke] mode = "fuzz" cases = 1 ops = 5`, driven through `run_profile_for_test::<GenericDomain, _, _, _>`.

Run: `cargo test -p harness-cli`
Expected: PASS.

```bash
git add crates/harness-cli
git commit -m "feat(harness-cli): generic cargo-test bridge"
```

---

## Phase 3 (dispatch Task 14 alongside the Task 15 chain)

### Task 14: Raw generic example crate (examples/math-tests)

**Parallel group:** F (independent of Tasks 15 through 18).

**Files:**
- Modify: `/Volumes/euclid/personal/cross-vm-testing/Cargo.toml` (add `"examples/math-tests"` to members, after the other example members)
- Create: `examples/math-tests/Cargo.toml`
- Create: `examples/math-tests/src/lib.rs`
- Create: `examples/math-tests/src/bin/math_cli.rs`
- Create: `examples/math-tests/math.harness.toml`
- Create: `examples/math-tests/tests/config_driven.rs`
- Create: `examples/math-tests/tests/cli_e2e.rs`

**Interfaces:**
- Consumes: `harness_core::Harness`, `harness_cli::{Cli, GenericDomain, BasicSetup, SetupFuture, run_profile_for_test}`.
- Produces: the developer-facing example; Phase 4 docs link to it.

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "math-tests"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Raw harness-core + harness-cli example: a math harness driven by generic config and CLI, no domain layer"
publish = false

[dependencies]
harness-core = { workspace = true, features = ["serde"] }
harness-cli.workspace = true
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
tokio.workspace = true

[[bin]]
name = "math-cli"
path = "src/bin/math_cli.rs"

[dev-dependencies]
serde_json.workspace = true

[lints]
workspace = true
```

- [ ] **Step 2: src/lib.rs, the harness**

Adapt the `MathHarness` from `crates/harness/tests/math.rs` (Calculator vs `i64` model): copy the harness, ops, and invariants; derive `serde::Serialize` and `serde::Deserialize` on the `Op` type and `OpKind` type (the registry bounds require it; keep `Clone`/`Debug`/`Copy` as the trait demands). Export a setup fn with the generic shape:

```rust
/// Config-driven setup: builds a fresh calculator and model world. The
/// [`BasicSetup`] carries the resolved env verbatim; this example reads an
/// optional `buggy` flag from it to demonstrate env-driven setup.
pub fn math_config_setup(
    req: harness_cli::BasicSetup,
) -> harness_cli::SetupFuture<'static, Calculator, MathWorld> {
    Box::pin(async move {
        let buggy = req.env["buggy"].as_bool().unwrap_or(false);
        let _ = buggy; // wire into the harness state exactly as math.rs's MathHarness { buggy } does
        Ok((Calculator::new(), MathWorld::default()))
    })
}
```

Match the actual `Ctx`/`World` type names from `crates/harness/tests/math.rs` when copying (if the test file uses `Ctx = ()` with the calculator inside `World`, keep that split; the snippet above adjusts to whatever the source defines). Every public item gets a rustdoc comment.

- [ ] **Step 3: The binary**

```rust
//! The raw generic CLI: `harness run math.harness.toml --profile smoke`.

use math_tests::{math_config_setup, MathHarness};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    harness_cli::Cli::<harness_cli::GenericDomain>::new()
        .register("math", || MathHarness::default(), math_config_setup)
        .main()
        .await
}
```

(If `MathHarness` carries a `buggy` field, `Default` gives `buggy: false`; add `#[derive(Default)]`.)

- [ ] **Step 4: math.harness.toml**

```toml
[harness]
name = "math"

[env]
buggy = false

[defaults]
seed = 42
check_every = 1

[profile.smoke]
mode = "fuzz"
cases = 4
ops = 20

[profile.invariants]
mode = "invariant"
ops = 200

[profile.steps]
mode = "scenario"

  [[profile.steps.steps]]
  op = { Add = 5 }

  [[profile.steps.steps]]
  op = { Add = -3 }

[suite.all]
profiles = ["smoke", "invariants", "steps"]
stop_on_failure = false
```

Adjust the scenario `op` JSON shape to the actual serde shape of the example's `Op` enum (write a tiny unit test asserting `serde_json::to_value(Op::Add(5))` and mirror that shape here).

- [ ] **Step 5: tests/config_driven.rs**

```rust
//! Drives profiles through the generic test bridge (the `cargo test` path).

use harness_cli::GenericDomain;
use math_tests::{math_config_setup, MathHarness};

#[tokio::test]
async fn smoke_profile_case_0() {
    harness_cli::run_profile_for_test::<GenericDomain, _, _, _>(
        concat!(env!("CARGO_MANIFEST_DIR"), "/math.harness.toml"),
        || MathHarness::default(),
        math_config_setup,
        "smoke",
        Some(0),
        Some(4),
    )
    .await;
}

#[tokio::test]
async fn scenario_profile_runs() {
    harness_cli::run_profile_for_test::<GenericDomain, _, _, _>(
        concat!(env!("CARGO_MANIFEST_DIR"), "/math.harness.toml"),
        || MathHarness::default(),
        math_config_setup,
        "steps",
        None,
        None,
    )
    .await;
}
```

(Adjust the call shape to Task 13's final signature.)

- [ ] **Step 6: tests/cli_e2e.rs**

Model on `examples/evm-tests/tests/cli_e2e.rs`: spawn `env!("CARGO_BIN_EXE_math-cli")` as a subprocess with `["run", "math.harness.toml", "--profile", "smoke", "--json-report", <tmp>]` (cwd = `CARGO_MANIFEST_DIR`), assert exit code 0 and `schema_version == 1` in the report; a second test runs `["list", "math.harness.toml"]` and asserts exit 0; a third runs an unknown profile and asserts exit 3.

- [ ] **Step 7: Run everything and commit**

Run: `cargo test -p math-tests`
Expected: PASS (unit, bridge, and e2e).

Run the binary once manually to eyeball output:
`cargo run -p math-tests --bin math-cli -- run examples/math-tests/math.harness.toml --profile smoke` (path relative to repo root; adjust cwd)
Expected: exit 0, per-profile pass line in the log.

```bash
git add Cargo.toml examples/math-tests
git commit -m "feat(examples): math-tests, raw generic config and CLI example"
```

### Task 15: Rebase cross-vm-config onto harness-config

**Parallel group:** G (first in chain; dispatch the chain alongside Task 14).

**Files:**
- Modify: `crates/config/Cargo.toml` (add `harness-config.workspace = true`)
- Modify: `crates/config/src/lib.rs` (becomes the variant layer)
- Delete: `crates/config/src/value.rs`, `interpolate.rs`, `duration.rs`, `seed.rs`, `merge.rs` (moved in Phase 1)
- Modify: `crates/config/src/schema.rs` (shrinks to `EnvSpec` + `TargetStr` only)
- Modify: `crates/config/src/validate.rs` (shrinks to chain validators, called from `CrossVmExt::validate`)
- Keep: `crates/config/src/chain.rs`, `crates/config/src/target.rs` (unchanged)
- Modify: `crates/config/tests/loader_pipeline.rs`, `crates/config/tests/schema.rs`

**Interfaces:**
- Consumes: the full harness-config surface.
- Produces: `CrossVmExt`, `pub type RunConfig = harness_config::RunConfig<CrossVmExt>`, `pub fn env_spec(...)`, re-exports keeping `cross_vm_config::{Profile, CommonKeys, Suite, SuitePhase, WorldSource, ExpectStr, ScenarioStepRaw, HarnessRef, SeedSpec, ConfigError, load, from_toml_str, from_json_str, interpolate_value, humantime_duration, humantime_opt}` valid paths.

- [ ] **Step 1: Rewrite lib.rs as the variant layer**

New structure:

```rust
//! Cross-vm variant of the generic `harness-config` schema: adds `[[chain]]`
//! declarations, the typed `EnvSpec` env shape, and mock/rpc target
//! resolution on top of the generic loader. This crate is the worked example
//! for building a domain config layer; see harness-config's docs.

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
        validate::validate_chains(cfg).map_err(|e| ConfigError::Domain(e))
    }
    fn merge_env_entry<V: harness_config::Doc>(key: &str, slot: &mut V, incoming: V) {
        // `targets` merges label-wise; every other env key replaces whole.
        if key == "targets" {
            if let Some(incoming_map) = incoming.clone().into_object() {
                if let Some(base) = slot.as_object_mut() {
                    let keys: Vec<String> =
                        incoming_map.iter().map(|(k, _)| k.clone()).collect();
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
```

(Adjust the `ConfigError::Deserialize` field names to the real enum. Add `serde_json` to crates/config's deps if not present; it is.)

- [ ] **Step 2: Shrink schema.rs and validate.rs**

`schema.rs` keeps ONLY `EnvSpec` and `TargetStr` (with their wire/serde impls) and now derives/keeps `serde::Deserialize` compatibility with the opaque JSON shape (`serde_json::from_value` path). Everything else in the file is deleted (it lives in harness-config).

`validate.rs` keeps ONLY the six chain validators, refactored into one entry:

```rust
/// All cross-vm domain checks, run by `CrossVmExt::validate` after generic
/// structural validation: unique chain labels, non-empty kinds, per-kind
/// required fields, env selections and target labels naming declared chains,
/// and rpc-target chains carrying an rpc_url.
pub(crate) fn validate_chains(cfg: &crate::RunConfig) -> Result<(), String> { ... }
```

Each old validator body ports with these mechanical changes: `cfg.chains` becomes `cfg.ext.chain`; env access goes through `crate::env_spec(&cfg.env)` once at the top (a malformed env table is itself a validation failure: `return Err(format!("env: {e}"))`); per-profile env checks parse `profile.common().env` the same way, skipping `None`. Old typed errors (`ConfigError::DuplicateChainLabel { ... }` etc.) become `Err(String)` carrying the SAME rendered message text the old `#[error]` attribute produced (copy each `#[error("...")]` format string; this keeps CLI stderr output stable).

- [ ] **Step 3: Update the two test files**

`tests/loader_pipeline.rs`: loader calls now hit the wrappers (no turbofish needed). The 13 `bad_*` assertions: the 6 generic ones keep matching their `ConfigError` variants; the 7 chain-specific ones become

```rust
    let err = load(...).expect_err("...");
    assert!(matches!(err, ConfigError::Domain(_)));
    assert!(err.to_string().contains("<distinctive fragment of the old message>"));
```

`tests/schema.rs`: tests of generic profile deserialization keep passing through the re-exports; tests asserting `CommonKeys.env` as typed `EnvSpec` change to parse via `env_spec(profile.common().env.as_ref().unwrap())` first. Tests of `EnvSpec`/`TargetStr`/`ChainDecl` themselves are untouched.

- [ ] **Step 4: Run, fix, commit**

Run: `cargo test -p cross-vm-config`
Expected: PASS.

```bash
git add crates/config
git commit -m "refactor(config): rebase cross-vm-config onto harness-config"
```

### Task 16: Framework domain adapter (CrossVmDomain)

**Parallel group:** G (after Task 15).

**Files:**
- Modify: `crates/framework/Cargo.toml` (`cli` feature adds `dep:harness-cli`; keep the existing optional deps that remain used)
- Create: `crates/framework/src/config/domain.rs` (new: `CrossVmDomain`, `TargetArgs`, chain resolution moved from resolve.rs)
- Modify: `crates/framework/src/config/setup_request.rs` (SetupRequest/ChainSpecData/Target stay; `SetupFuture` becomes an alias over harness-cli's)
- Delete: `crates/framework/src/config/{registry.rs, resolve.rs, erased.rs, report.rs, artifact.rs, test_bridge.rs}` (replaced by re-exports plus a thin test_bridge wrapper)
- Modify: `crates/framework/src/config/mod.rs`
- Keep: `crates/framework/src/config/build_chain.rs` (unchanged)

**Interfaces:**
- Consumes: harness-cli's full surface, `cross_vm_config::{CrossVmExt, env_spec, resolve_chain_target, TargetOverrides, ChainDecl}`.
- Produces: `CrossVmDomain`, `TargetArgs`, and a `crate::config` module whose public re-export surface matches the old one: `write_replay_artifact, build_chain, parse_spec_id, ErasedFailure, ErasedReport, ConfigHarness (if it exists as a public marker), Registry, RunError, ValidationError, write_json_report, Invocation, JsonReport, resolve_profile, ResolvedProfile, RunOptions, ChainSpecData, SetupFuture, SetupRequest, Target, test_bridge::run_profile_for_test`.

- [ ] **Step 1: Cargo feature rewire**

In `crates/framework/Cargo.toml`: add `harness-cli = { workspace = true, optional = true }`; the `cli` feature becomes:

```toml
cli = [
    "serde",
    "dep:harness-cli",
    "dep:cross-vm-config",
    "dep:clap",
    "dep:serde_json",
    "dep:toml",
]
```

Drop `dep:dotenvy`, `dep:tracing-subscriber`, `dep:humantime`, `tokio/signal` from the feature ONLY IF nothing else under `#[cfg(feature = "cli")]` in the framework still uses them after Task 17; verify with `cargo check -p cross-vm-framework --features cli` and keep any that are still referenced.

- [ ] **Step 2: setup_request.rs keeps the domain types**

`Target`, `ChainSpecData`, `SetupRequest` stay exactly as they are. Replace the `SetupFuture` alias:

```rust
/// A boxed, pinned future returning the `(Ctx, World)` pair a config-driven
/// setup fn builds. Alias over the generic harness-cli shape with the
/// framework's [`Ctx`] fixed.
pub type SetupFuture<'a, W> = harness_cli::SetupFuture<'a, Ctx, W>;
```

The `From<cross_vm_config::TargetStr> for Target` impl stays.

- [ ] **Step 3: Write config/domain.rs**

Move here, from the old `resolve.rs` and `cli.rs`, the chain-domain logic, adapted:

```rust
//! The cross-vm [`CliDomain`]: adds `--target`/`--target-chain` flags and
//! builds the chain-aware [`SetupRequest`] from the opaque env plus the
//! `[[chain]]` declarations.

use harness_cli::{CliDomain, ResolvedProfile, SetupBuildError};

use super::setup_request::{ChainSpecData, SetupRequest, Target};

/// Cross-vm CLI flags added to `run`/`replay`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct TargetArgs {
    /// Blanket mock/rpc target override.
    #[arg(long, value_parser = parse_target)]
    pub target: Option<Target>,
    /// Per-chain target override, `LABEL=mock|rpc`, repeatable.
    #[arg(long = "target-chain", value_parser = parse_target_chain)]
    pub target_chain: Vec<(String, Target)>,
}

/// The cross-vm domain: cross-vm config extension, chain-aware setup
/// requests, target flags, `CROSS_VM_*` env vars.
pub struct CrossVmDomain;

impl CliDomain for CrossVmDomain {
    type Ext = cross_vm_config::CrossVmExt;
    type Setup = SetupRequest;
    type Args = TargetArgs;
    const BIN_NAME: &'static str = "cross-vm";
    const ABOUT: &'static str = "Config-driven cross-VM harness runner";
    const ENV_PREFIX: &'static str = "CROSS_VM";

    fn build_setup(
        cfg: &cross_vm_config::RunConfig,
        resolved: &ResolvedProfile,
        args: &TargetArgs,
        seed: u64,
    ) -> Result<SetupRequest, SetupBuildError> {
        resolve_chains(cfg, resolved, args).map(|(chain_specs, target, chains, params)| {
            SetupRequest { target, chains, chain_specs, params, seed }
        })
    }

    fn artifact_sections(
        cfg: &cross_vm_config::RunConfig,
        resolved: &ResolvedProfile,
        args: &TargetArgs,
    ) -> toml::Table {
        // Best effort: an artifact for a run that got this far resolved its
        // chains once already, so re-resolution cannot fail here in practice;
        // fall back to no sections rather than failing the artifact write.
        match resolve_chains(cfg, resolved, args) {
            Ok((chain_specs, target, chains, _params)) => {
                render_artifact_sections(&chain_specs, target, &chains)
            }
            Err(_) => toml::Table::new(),
        }
    }

    fn overrides_json(args: &TargetArgs) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        if let Some(t) = args.target {
            map.insert("target".into(), target_label(t).into());
        }
        // (mirror the OLD overrides_json's exact key set for target flags; if
        // the old fn never recorded --target-chain, do not record it here.)
        map
    }
}
```

`resolve_chains` is the chain-resolution block DELETED from resolve.rs in Task 9, moved here verbatim with these input adaptations: chains come from `cfg.ext.chain`; the env comes from `cross_vm_config::env_spec(&resolved.env)` (a parse failure is `SetupBuildError::Usage(msg)`); the `TargetOverrides` value is built from `args` exactly as the old CLI built it from `RunArgs.target`/`RunArgs.target_chain`; the "rpc target requires rpc_url" re-assertion returns `SetupBuildError::Usage`. It returns `(Vec<ChainSpecData>, Target, Vec<String>, toml::Table)` where the `toml::Table` is `[env.params]` extracted from the parsed `EnvSpec` (the old `params` logic from resolve.rs, moved verbatim). `default_native_symbol` and `target_to_str` move here too. `parse_target`, `parse_target_chain`, `target_label` move here from the old cli.rs verbatim.

`render_artifact_sections` reconstructs the OLD artifact chain/env sections: build a `toml::Table` with key `"chain"` = array of tables shaped exactly like the old `ArtifactChain` struct (label, kind string, chain_id, name, native_symbol, rpc_url, target string, per-kind Option fields skipped when None, params) and key `"env"` = table with `target` string and `chains` label array, matching the old `ArtifactEnv`. Copy the old `artifact_chain` fn body (deleted from harness-cli's artifact.rs in Task 11) as the per-chain renderer, targeting `toml::Table` instead of a serde struct (or keep the old serde structs here privately and `toml::Table::try_from` them; the latter is less error-prone: move `ArtifactChain`, `ArtifactEnv`, `artifact_chain`, `target_str` into this file as private items).

- [ ] **Step 4: Rewrite config/mod.rs as the re-export shim**

```rust
// Domain-owned modules.
pub mod build_chain;   // adjust to the real module layout (mod + pub use)
mod domain;
mod setup_request;

pub use build_chain::build_chain;
#[cfg(any(feature = "evm", feature = "tron"))]
pub use build_chain::parse_spec_id;
pub use domain::{CrossVmDomain, TargetArgs};
pub use setup_request::{ChainSpecData, SetupFuture, SetupRequest, Target};

// Generic machinery, re-exported from harness-cli so downstream paths stay
// stable (`cross_vm_framework::config::Registry`, etc.).
pub use harness_cli::{
    resolve_profile, write_json_report, write_replay_artifact, ErasedFailure, ErasedReport,
    Invocation, JsonReport, ResolvedProfile, RunError, RunOptions, ValidationError,
};

/// The cross-vm registry: harness-cli's registry fixed to [`SetupRequest`].
pub type Registry = harness_cli::Registry<SetupRequest>;

/// The `cargo test` bridge, fixed to the cross-vm domain (what
/// `#[config_runner]` expands to; keep this path stable).
pub mod test_bridge {
    /// See [`harness_cli::test_bridge::run_profile_for_test`].
    pub async fn run_profile_for_test<H, F, S>(
        config_path: &str,
        harness: F,
        setup: S,
        profile: &str,
        case: Option<usize>,
        expected_cases: Option<usize>,
    ) where
        H: crate::harness::Harness + 'static,
        H::Ctx: 'static,
        H::World: 'static,
        H::Operation: serde::Serialize + serde::de::DeserializeOwned + 'static,
        H::OpKind: serde::Serialize + serde::de::DeserializeOwned + Copy + 'static,
        F: Fn() -> H + 'static,
        S: Fn(super::SetupRequest) -> super::SetupFuture<'static, H::World> + 'static,
    {
        harness_cli::test_bridge::run_profile_for_test::<super::CrossVmDomain, H, F, S>(
            config_path, harness, setup, profile, case, expected_cases,
        )
        .await
    }
}
```

Adjust generic-argument plumbing to Task 13's real signature (if the generic fn's `SF` bound is expressed differently, mirror it). Note the old `ConfigHarness` marker: if `config/mod.rs` previously re-exported `ConfigHarness` from registry.rs and anything downstream uses it, re-export the harness-cli equivalent or keep a deprecated alias.

Delete `registry.rs`, `resolve.rs`, `erased.rs`, `report.rs`, `artifact.rs`, `test_bridge.rs` from `crates/framework/src/config/`. BEFORE deleting each, sweep its `#[cfg(test)]` module for cross-vm-specific tests (any test constructing `ChainSpecData`, `SetupRequest`, `Target`, or `[[chain]]` TOML) and move those into `domain.rs`'s test module or a new `crates/framework/tests/config_domain.rs`, adapted to call the moved fns (`resolve_chains` etc.).

- [ ] **Step 5: Compile check**

Run: `cargo check -p cross-vm-framework --features cli`
Expected: errors ONLY in `src/cli.rs` (fixed by Task 17). If config/ modules still error, fix before moving on.

- [ ] **Step 6: Commit**

```bash
git add crates/framework
git commit -m "refactor(framework): cross-vm domain adapter over harness-cli"
```

### Task 17: Framework CLI rewire

**Parallel group:** G (after Task 16).

**Files:**
- Modify: `crates/framework/src/cli.rs` (2183 lines shrink to a shim plus domain tests)
- Modify: `crates/framework/src/lib.rs` (module docs if they describe the old layout)

**Interfaces:**
- Consumes: `harness_cli::Cli`, `CrossVmDomain` (Task 16).
- Produces: `cross_vm_framework::cli::Cli` with the same builder API (`new`, `env_file`, `register`, `register_persistent`, `main`), so `examples/*/src/bin/cross_vm.rs` compiles unchanged.

- [ ] **Step 1: Replace the implementation**

New `crates/framework/src/cli.rs` top:

```rust
//! The cross-vm CLI: harness-cli's generic CLI fixed to [`CrossVmDomain`].
//! Keep the doc example from the old file's header here, updated only in its
//! import lines if needed.

use crate::config::CrossVmDomain;

/// The cross-vm CLI builder. See [`harness_cli::Cli`] for the full API;
/// registered setup fns receive the cross-vm [`crate::config::SetupRequest`].
pub type Cli = harness_cli::Cli<CrossVmDomain>;
```

Delete everything else EXCEPT: `parse_target`/`parse_target_chain`/`target_label` if Task 16 did not already move them (they must live in exactly one place; prefer `config/domain.rs`), and the test module portions below. Keep the old file's module-level doc example (the `Cli::new().register(...)` doctest) updated to compile against the alias.

CHECK: the old `Cli::register` was an inherent method on the old struct; with a type alias, `Cli::new()` resolves to `harness_cli::Cli::<CrossVmDomain>::new()` and method calls resolve fine. The one incompatibility to watch: old setup fn bound was `Fn(SetupRequest) -> SetupFuture<'static, H::World>` with the framework's 2-parameter alias; the generic bound is `Fn(D::Setup) -> harness_cli::SetupFuture<'static, H::Ctx, H::World>`. These are the same type when `H::Ctx = Ctx`, because the framework alias IS the generic alias with Ctx fixed (Task 16 Step 2). Existing example setup fns therefore satisfy the bound with zero changes. Verify by compiling an example (Step 3).

- [ ] **Step 2: Prune the test suite**

From the old test module keep ONLY the domain-specific tests, rehomed per Task 12 Step 3's list: `parses_run_with_two_profiles_a_target_chain_and_seed` (now parses `TargetArgs` through the generic `RunArgs` flatten; construct the clap command via `harness_cli` building blocks or test `TargetArgs` parsing directly with a tiny `#[derive(Parser)]` wrapper), `bad_target_chain_without_equals_is_a_parse_error`, `bad_target_value_is_a_parse_error`. Everything else was ported to harness-cli in Task 12 and is DELETED here. If any framework-level integration tests exercised `run_with_config` with the mock `Ctx`, keep one end-to-end smoke test that registers a trivial cross-vm harness and runs a mock profile through `Cli` (this pins the whole adapter stack).

- [ ] **Step 3: Verify the framework and examples compile and pass**

Run: `cargo test -p cross-vm-framework --features cli`
Expected: PASS.

Run: `cargo check -p evm-tests`
Expected: success with NO changes to the example crate.

- [ ] **Step 4: Commit**

```bash
git add crates/framework
git commit -m "refactor(framework): cli as harness-cli variant, domain tests only"
```

### Task 18: Full example matrix green

**Parallel group:** G (after Task 17; closes track B).

**Files:**
- Modify: only if a test failure demands it (report any change made).

- [ ] **Step 1: Run every example suite**

```bash
cargo test -p evm-tests
cargo test -p cosmos-tests
cargo test -p solana-tests
cargo test -p tvm-tests
cargo test -p cross-vm-tests
cargo test -p examples-common 2>/dev/null || cargo test -p common 2>/dev/null || true
```

(Resolve the actual `examples/common` package name from its Cargo.toml before running.)

Expected: ALL PASS, especially every `cli_e2e.rs` (exit codes, JSON `schema_version`, replay artifacts). These pin the behavior contract; a failure here means a Phase 2/3 port broke compatibility. Debug the port, do not adjust the e2e assertions.

- [ ] **Step 2: Grep for leftovers**

```bash
grep -rn "cross_vm_config::RawRunConfig\|crate::config::registry\|config::resolve::" crates/framework/src examples/*/src examples/*/tests
```

Expected: no hits (all paths go through the new shims).

- [ ] **Step 3: Commit (only if changes were needed)**

```bash
git add -A
git commit -m "test(examples): green matrix on generic cli rebase"
```

---

## Phase 4

### Task 19: Documentation

**Parallel group:** H (may start once Phase 2 ends; runs alongside Phase 3).

**Files:**
- Create: `docs/extending-harness-cli.md`
- Modify: `CHANGELOG.md`, `DEVELOPER.md`
- Modify: `docs/config-runs-spec.md` (one pointer paragraph, not a rewrite)

**Constraints reminder:** no dashes as punctuation in these files. Rephrase with periods, commas, or parentheses.

- [ ] **Step 1: Write docs/extending-harness-cli.md**

Structure (write real prose, not placeholders; pull code snippets from the actual implementation, verified to compile by doc-testing or by copying from committed files):
1. "The three layers": harness-core (trait + runner), harness-config (declarative schema), harness-cli (registry + CLI). One paragraph each plus the crate dependency diagram in a fenced code block.
2. "Using the generic layer raw": walk through `examples/math-tests` end to end (harness impl, setup fn shape, `math.harness.toml`, the binary, the test bridge). Copy the real files' contents.
3. "Building a variant": the two seams. `ConfigExt` (what deserializes from leftover top level keys, `validate`, `merge_env_entry`) and `CliDomain` (Setup type, Args, `build_setup`, `artifact_sections`, `overrides_json`, env prefix). For each seam show the cross-vm implementation (`CrossVmExt`, `CrossVmDomain`) as the worked example, copied from the real files.
4. "Behavior contract you inherit": exit codes table, env var precedence, JSON report envelope, replay artifact shape, determinism rules (do not perturb rng draw order).
5. "Checklist for a new variant": ext struct with deny_unknown_fields, domain validation returns rendered messages, setup type, clap Args struct, env prefix choice, artifact sections if replay must round-trip domain data.

- [ ] **Step 2: CHANGELOG.md and DEVELOPER.md**

CHANGELOG: add an Unreleased entry describing the two new crates, the cross-vm rebase, and the new example, with the "no behavior change to the cross-vm CLI surface" note. DEVELOPER.md: update the crate map section (it exists; find the crate list and add harness-config, harness-cli, examples/math-tests with one line each) and link the new extending guide.

- [ ] **Step 3: docs/config-runs-spec.md pointer**

Add one paragraph at the top noting the generic implementation now lives in harness-config and harness-cli, that this spec describes the cross-vm variant built on them, and linking `docs/extending-harness-cli.md`.

- [ ] **Step 4: Commit**

```bash
git add docs CHANGELOG.md DEVELOPER.md
git commit -m "docs: extending guide for generic config and cli layers"
```

### Task 20: Workspace verification (strictly last)

**Files:** none (fix-forward only; any fix gets its own focused commit).

- [ ] **Step 1: Format and lint**

```bash
cargo fmt --all
git diff --exit-code   # expect empty; if not, commit "style: cargo fmt"
cargo clippy --workspace --all-targets --features cli -p cross-vm-framework 2>/dev/null; cargo clippy --workspace --all-targets
```

Use the repo's Makefile lint target instead if one exists (`make lint`); check `Makefile` first and prefer its invocations.

- [ ] **Step 2: Full test sweep**

```bash
cargo test --workspace
cargo test -p cross-vm-framework --features cli
cargo test -p harness-core --all-features
```

Expected: ALL PASS.

- [ ] **Step 3: Docs build (missing_docs is a warn, keep it clean)**

```bash
cargo doc --no-deps -p harness-config -p harness-cli 2>&1 | grep -i "warning" || echo "docs clean"
```

Expected: "docs clean" (no missing-docs warnings in the new crates).

- [ ] **Step 4: Final commit if anything changed**

```bash
git add -A
git commit -m "chore: post-rebase lint and fmt sweep"
```

Do NOT push. Present the branch for review (superpowers:finishing-a-development-branch).

---

## Execution Notes for the Orchestrator

- Subagents implementing port tasks MUST read the source file they are porting in full before editing; the plan's edit lists are anchors, the source file is the authority for everything the plan marks "verbatim".
- When a plan snippet conflicts with a compile error, the fix that preserves the source file's behavior wins; record the deviation in the task's commit body.
- Tasks 6/7, 8/9, 10/11, 12/13 pairs and the Phase 1 A/B and Phase 3 F/G splits are the parallelism budget. Anything else runs sequentially.
- Every task ends with its listed test command green BEFORE its commit step. No task commits red.
