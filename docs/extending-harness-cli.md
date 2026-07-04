# Extending the generic config and CLI layers

The config-driven runner is split into three reusable, chain-agnostic crates. This guide explains the split, walks a raw example that uses the generic layer with no domain code at all (`examples/math-tests`), shows the two seams a variant plugs into (using the cross-vm variant as the worked example), and lists the behavior contract any variant inherits for free.

If you only want to use the cross-vm runner, read `docs/config-runs-spec.md` instead. That spec describes the cross-vm variant; this guide describes the generic machinery underneath it and how to build a new variant on the same machinery.

## 1. The three layers

**`harness-core`** (`crates/harness`) is the property-testing engine. It defines the `Harness` trait (a developer's `Operation`, `Invariant`, `OpKind`, plus `apply`/`generate_op`/`check`), the mode-typed `Runner` and its fuzz/invariant/endurance/scenario drivers, the seedable `Prng`, per-op `Stats`, and the outcome types (`Verdict`, `HarnessError`, `CheckOutcome`, `RunReport`). It names no chain type and no config type. A harness is just a Rust impl; nothing here knows about TOML or a CLI.

**`harness-config`** (`crates/harness-config`) is the declarative schema. It parses TOML or JSON into a validated `RunConfig<X>` through a five-stage pipeline (parse, interpolate `${VAR}`, merge `[defaults]` and per-profile `env`, typed deserialize, structural validate). It is pure data with no runtime dependencies (serde, toml, serde_json, thiserror only), and it does not depend on `harness-core`. Its `[env]` table is deliberately opaque (a `serde_json::Value`), and its one extension point is the `ConfigExt` trait, which lets a domain claim the leftover top-level keys and add a domain validation pass.

**`harness-cli`** (`crates/harness-cli`) ties the two together. It holds the harness registry, the clap-based CLI (`run`/`replay`/`list`/`validate`), the run pipeline that resolves a profile and drives it through `harness-core`, the JSON report envelope, and the replay-artifact writer. It depends on both `harness-core` and `harness-config`. Its extension point is the `CliDomain` trait, which pairs a `ConfigExt` with a setup-request type, extra clap flags, an env-var prefix, and the artifact/overrides hooks. Used raw through `GenericDomain`, it is a complete runner with no domain code.

Dependency edges (an arrow means "depends on"):

```
                 harness-core           (Harness trait, Runner, Prng, outcomes)
                    ^      ^
                    |      |
   harness-config   |      |            (schema + ConfigExt seam; no harness-core dep)
        ^    ^      |      |
        |    |      |      |
        |    +--- harness-cli           (registry, CLI, run pipeline, JSON report, replay)
        |               ^
        |               |
 cross-vm-config        |               (CrossVmExt: a ConfigExt impl)
        ^               |
        |               |
        +------ cross-vm-framework       (CrossVmDomain: a CliDomain impl)
```

The generic layer is `harness-core` + `harness-config` + `harness-cli`. The cross-vm variant adds exactly two crates on top: `cross-vm-config` (the `ConfigExt` impl) and the domain module in `cross-vm-framework` (the `CliDomain` impl). Building your own variant means writing the same two pieces.

## 2. Using the generic layer raw

`examples/math-tests` drives `harness-core` and `harness-cli` against a small math library with no domain layer at all. The system under test is a `Calculator` (an `i32` accumulator with checked add, sub, mul, divide) checked against an `i64` shadow model. It is the whole generic path in one small crate.

### The harness impl (`src/lib.rs`)

The harness picks its five associated types and implements the operation/generation/check trio. Because there is no live external system, `Ctx` is `()`:

```rust
impl Harness for MathHarness {
    type Ctx = ();
    type World = MathWorld;
    type Operation = Op;
    type Invariant = Inv;
    type OpKind = OpKind;

    async fn apply(
        &self,
        _ctx: &mut Self::Ctx,
        world: &mut Self::World,
        op: &Self::Operation,
    ) -> Result<Verdict, HarnessError> {
        // ... applies op to world.sut, compares against world.model,
        //     returns Verdict::Accepted / Verdict::Rejected, or HarnessError::bug(..)
    }

    fn op_kinds(&self) -> Vec<Self::OpKind> {
        vec![OpKind::Add, OpKind::Sub, OpKind::Mul, OpKind::Div]
    }

    fn generate_op(&self, rng: &mut Prng, _world: &Self::World, kind: Self::OpKind) -> Op {
        let n = rng.below(201) as i32 - 100;
        match kind {
            OpKind::Add => Op::Add(n),
            OpKind::Sub => Op::Sub(n),
            OpKind::Mul => Op::Mul(n),
            OpKind::Div => Op::Div(n),
        }
    }

    fn invariants(&self) -> Vec<Self::Invariant> {
        vec![Inv::MatchesModel]
    }

    async fn check(
        &self,
        _ctx: &mut Self::Ctx,
        world: &Self::World,
        inv: &Self::Invariant,
    ) -> CheckOutcome {
        match inv {
            Inv::MatchesModel if world.sut.value as i64 == world.model => CheckOutcome::Held,
            Inv::MatchesModel => CheckOutcome::violated(format!(
                "calculator {} does not equal model {}",
                world.sut.value, world.model
            )),
        }
    }
}
```

`Op` and `OpKind` derive `Serialize`/`Deserialize` because the CLI registry requires operations to round-trip through config (a scenario step's `op = { Add = 5 }` is the serde shape of `Op::Add(5)`).

### The setup fn shape

A config-driven setup fn takes the domain's setup request and returns a pinned future producing the `(Ctx, World)` pair. For `GenericDomain`, the request type is `BasicSetup` (the resolved profile name, the concrete seed, and the merged `[env]` table verbatim):

```rust
pub fn math_config_setup(
    req: harness_cli::BasicSetup,
) -> harness_cli::SetupFuture<'static, (), MathWorld> {
    Box::pin(async move {
        let _buggy = req.env["buggy"].as_bool().unwrap_or(false);
        Ok(((), MathWorld::default()))
    })
}
```

The `Ctx` is `()`, so the returned pair is `((), MathWorld)`. Reading `req.env["buggy"]` shows how env-driven setup reaches the opaque `[env]` table.

### The config file (`math.harness.toml`)

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

### The binary (`src/bin/math_cli.rs`)

The whole CLI is a registry of one harness against `GenericDomain`. Everything else is config:

```rust
use math_tests::{math_config_setup, MathHarness};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    harness_cli::Cli::<harness_cli::GenericDomain>::new()
        .register("math", MathHarness::default, math_config_setup)
        .main()
        .await
}
```

Run it with `harness run math.harness.toml --profile smoke`.

### The test bridge (`tests/config_driven.rs`)

For the `cargo test` path, `harness_cli::test_bridge::run_profile_for_test` reloads the config at run time and drives a named profile through the same registry the CLI uses, panicking on any failure. This is the hand-written equivalent of what a `#[config_runner]` macro would emit:

```rust
use harness_cli::GenericDomain;
use math_tests::{math_config_setup, MathHarness};

#[tokio::test]
async fn smoke_profile_case_0() {
    harness_cli::test_bridge::run_profile_for_test::<GenericDomain, _, _, _>(
        concat!(env!("CARGO_MANIFEST_DIR"), "/math.harness.toml"),
        MathHarness::default,
        math_config_setup,
        "smoke",
        Some(0),
        Some(4),
    )
    .await;
}
```

The two `Some(..)` arguments are the fuzz case index and the total case count; a non-fuzz profile (like `steps`) passes `None` for both and runs whole.

## 3. Building a variant

A variant plugs into two seams. `ConfigExt` (in `harness-config`) claims extra config sections and adds domain validation. `CliDomain` (in `harness-cli`) supplies the setup-request type, extra flags, an env prefix, and the artifact/overrides hooks. The cross-vm variant is the worked example for both.

### Seam one: `ConfigExt`

`ConfigExt` deserializes from whatever top-level keys remain after the generic loader removes `harness`, `env`, `defaults`, `profile`, `suite`, and `replay`:

```rust
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
```

Three things to notice. The struct itself is what deserializes from the leftover keys, so put `#[serde(deny_unknown_fields)]` on it to keep unknown top-level keys as hard errors. `validate` runs after generic structural validation and is where a domain re-checks its own sections. `merge_env_entry` decides, per colliding key, how a profile's `env` overlay combines with the top-level `[env]` (the default replaces the whole slot).

The cross-vm implementation (`crates/config/src/lib.rs`) adds `[[chain]]` declarations, validates them, and merges the `targets` env key label-wise instead of wholesale:

```rust
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossVmExt {
    /// The `[[chain]]` declarations.
    #[serde(rename = "chain", default)]
    pub chain: Vec<ChainDecl>,
}

impl harness_config::ConfigExt for CrossVmExt {
    fn validate(cfg: &RunConfig) -> Result<(), ConfigError> {
        validate::validate_chains(cfg).map_err(ConfigError::Domain)
    }

    fn merge_env_entry<V: harness_config::Doc>(key: &str, slot: &mut V, incoming: V) {
        use harness_config::DocMap;
        // `targets` merges label-wise; every other env key replaces whole.
        if key == "targets" {
            if let Some(incoming_map) = incoming.clone().into_object() {
                if let Some(base) = slot.as_object_mut() {
                    let keys: Vec<String> = incoming_map.iter().map(|(k, _)| k.clone()).collect();
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
```

The generic loader keeps `[env]` opaque, so the domain re-types it when it needs the typed shape. Cross-vm does this in `env_spec`, turning the merged `serde_json::Value` into a typed `EnvSpec` and surfacing a malformed env table as a hard error.

### Seam two: `CliDomain`

`CliDomain` (in `crates/harness-cli/src/domain.rs`) bundles the config extension, the setup-request type, extra clap flags, and the naming/prefix constants:

```rust
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
```

`build_setup` is the required method: it turns a resolved profile plus the domain's flags into the setup request that registered setup fns receive (it is called once per fuzz case with that case's sub-seed). `SetupBuildError::Usage` maps to exit code 3 and `SetupBuildError::Infra` to exit code 2. The two hooks are optional. `artifact_sections` injects extra top-level sections into a replay artifact so the artifact stays a loadable config for the domain's own extension type. `overrides_json` records domain flags in the JSON report's `invocation.overrides`.

`GenericDomain` is the raw impl: `Ext = NoExt`, `Setup = BasicSetup`, `Args = NoArgs`, prefix `HARNESS`, and both hooks left at their no-op defaults. The cross-vm impl (`crates/framework/src/config/domain.rs`) fills in all of them:

```rust
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
        cfg: &RunConfig,
        resolved: &ResolvedProfile,
        args: &TargetArgs,
        seed: u64,
    ) -> Result<SetupRequest, SetupBuildError> {
        resolve_chains(cfg, resolved, args).map(|(chain_specs, target, chains, params)| {
            SetupRequest {
                target,
                chains,
                chain_specs,
                params,
                seed,
            }
        })
    }

    fn artifact_sections(
        cfg: &RunConfig,
        resolved: &ResolvedProfile,
        args: &TargetArgs,
    ) -> toml::Table {
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
        if !args.target_chain.is_empty() {
            let per_chain: serde_json::Map<String, serde_json::Value> = args
                .target_chain
                .iter()
                .map(|(label, target)| {
                    (label.clone(), serde_json::Value::from(target_label(*target)))
                })
                .collect();
            map.insert("target_chain".into(), serde_json::Value::Object(per_chain));
        }
        map
    }
}
```

`build_setup` re-types the opaque env into `EnvSpec`, resolves each `[[chain]]` declaration's target through the mock-vs-rpc precedence funnel, and builds the chain-aware `SetupRequest`. `artifact_sections` re-renders the resolved `[[chain]]` and `[env]` tables (best-effort: a run that got far enough to fail already resolved its chains once, so it falls back to no sections rather than failing the artifact write). `overrides_json` records only the two target flags; the generic scalar overrides (`seed`, `cases`, `ops`) are recorded by `harness-cli` itself.

## 4. Behavior contract you inherit

Any variant built on `harness-cli` inherits the following behavior with no extra code. All of it is defined in the generic layer, so it is identical for `GenericDomain`, `CrossVmDomain`, and any variant you write.

### Exit codes

The CLI maps a run outcome to a process exit code (`crates/harness-cli/src/cli.rs`):

| Code | Meaning |
| ---- | ------- |
| 0 | Every selected profile passed. |
| 1 | A run failed with a discovered bug (`FailureKind::Bug`) or an invariant violation (`FailureKind::Invariant`). |
| 2 | A run failed with `FailureKind::Infra`, or a `RunError::Setup`/`Serialize`/`Export` (a setup fn failed, a report could not be serialized, or an `export_world` write failed). |
| 3 | A usage or config error: `RunError::UnknownHarness`/`Validation`/`Invalid`/`UnsupportedMode`, or a `SetupBuildError::Usage`. |

A suite reports the worst code across its profiles, but not by numeric maximum: severity ranks usage/config (3) above a discovered bug (1) above an infra failure (2) above a clean pass (0). The single `combine` function owns that ordering, since exit-code numbers are not monotonic with severity.

### Environment variable precedence

Every scalar override resolves highest-first: a CLI flag beats a `{PREFIX}_*` env var, which beats a profile key (with `[defaults]` already merged), which beats the built-in default. `{PREFIX}` is the domain's `ENV_PREFIX` (`HARNESS` for the generic runner, `CROSS_VM` for cross-vm). The honored env vars are `{PREFIX}_PROFILE`, `{PREFIX}_SEED`, `{PREFIX}_CASES`, and `{PREFIX}_OPS` (plus `PROPTEST_CASES` as an alias for cases). The env fold happens once in the CLI layer, so the pure profile-resolution code only ever sees "CLI flag, already folded" versus "profile key" versus "default".

### JSON report envelope

`--json-report` (or the `json_report` profile key) writes one file per invocation, not one per profile (`crates/harness-cli/src/report.rs`). The envelope is:

```json
{
  "schema_version": 1,
  "invocation": {
    "config": "<config path as passed>",
    "profiles": ["<selected profile names>"],
    "overrides": { "seed": 7, "cases": 2 }
  },
  "profiles": [ /* one entry per selected profile, in run order */ ]
}
```

`schema_version` is always `1` today; a future incompatible change bumps it. `invocation.overrides` records only the CLI-set scalar overrides (plus any domain flags from `overrides_json`), never a copy of the config, so it cannot leak a config secret. A suite of three profiles produces one file with a three-element `profiles` array.

### Replay artifact shape

On any failed generative run (fuzz, invariant, endurance), the CLI writes a self-contained `<harness>-<profile>-<seed>-<timestamp>.replay.toml` (`crates/harness-cli/src/artifact.rs`). The artifact is a valid config file: a `[harness]` block, the resolved `[env]` embedded verbatim, a `[replay]` provenance block, and a single `[profile.replay]` scenario profile holding the (possibly shrunk) failing history. A domain injects its own extra top-level sections through `artifact_sections` (cross-vm adds `[[chain]]`), merged in before serialization so the artifact stays loadable for that domain's extension type. Replaying it is just `load` plus `run` over the same loader and registry every other config file goes through, so it reloads identically. Field ordering within the file is not part of the contract. Only already-resolved, non-secret values land in the artifact; it is a reproduction tool, not a secret store. If a step's `op` holds an integer outside TOML's signed 64-bit range, the writer falls back to a sibling `*.replay.json` (which the loader reads by extension) carrying the identical structure.

### Determinism

Recorded seeds must keep reproducing across releases. The rng draw order is pinned: on each generative step the runner draws the weighted kind index first, then the op data (`crates/harness/src/runner.rs`). Do not perturb this order. `Harness::weight` never draws from the rng, so dynamic weights change which kind is likely, never how many rng values a draw consumes. A fixed seed's operation stream is byte-identical across releases, guarded by the golden-seed tests in the example crates.

## 5. Checklist for a new variant

1. **Ext struct.** Define a struct for your extra top-level sections, derive `Deserialize`/`Clone`/`Debug`/`Default`, and put `#[serde(deny_unknown_fields)]` on it so unknown top-level keys stay hard errors. Implement `ConfigExt` for it.
2. **Domain validation.** Implement `ConfigExt::validate` to re-check your sections after generic structural validation. Return `ConfigError::Domain(msg)` with a rendered, human-readable message. Add `merge_env_entry` only if some env key needs non-replace merge semantics.
3. **Setup type.** Define the setup-request type your setup fns receive (`type Setup`), carrying whatever a setup fn needs (the seed, resolved env, resolved domain data).
4. **Args struct.** Define a `clap::Args` struct for your extra flags (derive `Clone`/`Debug`/`Default`). Use `NoArgs`-shaped emptiness if you need none.
5. **Env prefix.** Pick an `ENV_PREFIX`. It selects the `{PREFIX}_PROFILE`/`_SEED`/`_CASES`/`_OPS` env vars, so choose one that will not collide with an unrelated tool's env vars.
6. **Artifact sections.** Implement `artifact_sections` only if a replay must round-trip domain-specific data (like cross-vm's `[[chain]]` blocks). If your setup fn reconstructs everything from `[harness]` and `[env]` alone, leave the default no-op. Implement `overrides_json` if your flags should appear in the JSON report's `invocation.overrides`.

Register your harnesses on `Cli::<YourDomain>::new()` in a binary, and add a test bridge with `run_profile_for_test::<YourDomain, _, _, _>` for the `cargo test` path. Everything in section 4 (exit codes, env precedence, JSON envelope, replay artifacts, determinism) then works without further code.
