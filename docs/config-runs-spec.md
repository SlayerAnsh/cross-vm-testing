# Spec: TOML driven test runs

Status: proposed, not implemented. This document specifies the declarative run configuration layer, the first piece of the "broader cross VM orchestration layer above `MultiChainEnv`" that `SPEC.md` and the README list as planned.

## 1. Motivation

Today every run is a hand written Rust test function. The four modes (scenario, fuzz, invariant, endurance) are expressed through the `#[fuzz_runner]`, `#[invariant_runner]` and `#[endurance_runner]` macros plus direct `Runner` construction, gated by cargo features and driven by Makefile targets. Changing a seed, a duration, an op mix, or switching a run from mock to live RPC means editing Rust and recompiling.

The goal: define a harness and its ops once in Rust, then declare any number of run combinations in TOML. A single config file can hold a quick smoke profile, a deep fuzz profile, a nightly invariant profile, and an eight hour live endurance profile, all over the same harness. Because the harness already talks to live chains, the same mechanism doubles as an on chain scripting tool. A scenario profile with explicit steps against an RPC target is a deployment script with built in assertions and invariant checks.

Today chain construction is also hand written Rust: each setup fn names concrete presets (`OSMOSIS.mock(wallets)`, `ETHEREUM.rpc(wallets)`) and injects them into `MultiChainEnv`. Switching chain ids, RPC endpoints, or which VMs participate means editing that setup and recompiling. The same config file should declare chains as data (`[[chain]]` entries with `kind`, `chain_id`, `rpc_url`, and VM specific fields) and let a framework `build_chain` factory materialize them into `AnyChain` handles. Setup fns then focus on harness specific wiring (funding, deploy, model priming) rather than repeating chain boilerplate.

## 2. Locked design decisions

1. TOML is the primary format. The schema is serde first, so YAML and JSON inputs are one function additions later (JSON is in fact accepted from day one, see section 10).
2. Execution model: a user crate CLI binary built from a framework provided `Cli` builder plus a harness registry. A `#[config_runner]` macro bridge for `cargo test` comes later and reuses the same loader.
3. Configs can express concrete op sequences. `Operation` and `OpKind` derive `Serialize` and `Deserialize`. The requirement is opt in at the registration site only; the `Harness` trait does not change.
4. Environment variable interpolation (`${VAR}`, `${VAR:-default}`) ships in the first phase. Runtime or computed variables are a deferred final phase.
5. Chains may be declared in config via `[[chain]]` and built by a framework `build_chain` factory over the four compiled VM backends (CosmWasm, EVM, SVM, Tron). Config selects and parameterizes compiled behavior; adding a wholly new VM still requires a crate. When no `[[chain]]` entries are present, setup fns may hard code chains exactly as today (backward compatible).

## 3. Two load bearing constraints

Both come from the current code and shape everything below.

**The stack is single threaded and `!Send` by construction.** `WalletFactory` is shared as `Rc`, and the existing script binary runs on `#[tokio::main(flavor = "current_thread")]`. Every erased future in the registry is therefore a non `Send` local boxed future (`Pin<Box<dyn Future<Output = T> + 'a>>`). No `futures` crate dependency is needed, and parallel workers inside one process are off the table (see section 12).

**Recorded seeds must keep reproducing across releases.** The comment on `OpSource` in `crates/harness/src/runner.rs` (the standalone `harness-core` crate) pins the rng draw order (weighted kind index first, then op data), guarded by the golden seed tests in `mechanics.rs`. `Harness::weight` never draws from the rng, so dynamic weights change which kind is likely, never how many rng values a draw consumes.

## 4. Config file schema

### 4.1 Full example

```toml
# vault.cross-vm.toml
[harness]
name = "vault"                 # registry key, required
setup = "default"              # named setup, optional, defaults to "default"

[[chain]]                      # declare chains as data (section 4.6)
label = "osmosis"
kind = "cosmwasm"
chain_id = "osmosis-1"
name = "Osmosis"
bech32_prefix = "osmo"
native_denom = "uosmo"
native_symbol = "OSMO"
gas_price = 0.025
rpc_url = "https://rpc.osmosis.zone:443"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"
name = "Ethereum"
native_symbol = "ETH"
spec_id = "cancun"
rpc_url = "${ETH_RPC:-https://eth.llamarpc.com}"

[[chain]]
label = "solana"
kind = "svm"
chain_id = "devnet"
name = "Solana Devnet"
native_symbol = "SOL"
commitment = "finalized"
rpc_url = "https://api.devnet.solana.com"
ws_url = "wss://api.devnet.solana.com"

[env]                          # request passed to the setup fn
target = "mock"                # "mock" | "rpc", default "mock"
chains = ["osmosis", "eth", "solana"]   # label subset; omitted = all [[chain]]

[env.params]                   # free form table, the harness author defines meaning
users = 2
rpc_label = "${TARGET_CHAIN:-base}"

[defaults]                     # shallow merged under every profile
seed = 42
check_every = 1
stats = true
artifacts_dir = "target/cross-vm"

[profile.smoke]
mode = "fuzz"
cases = 8
ops = 20
kinds = ["Deposit", "Withdraw"]

[profile.deep]
mode = "fuzz"
cases = 200
ops = 50
seed = "random"
weights = { Deposit = 40, Withdraw = 25, Borrow = 20, Repay = 15 }
shrink = true

[profile.invariant-long]
mode = "invariant"
ops = 2000
check_every = 5

[profile.soak]
mode = "endurance"
duration = "${ENDURANCE_DURATION:-8h}"
max_ops = 100000               # stop on whichever bound hits first
base_delay = "500ms"
max_delay = "2s"
check_every = 25
advance_blocks = 1
block_jitter = 2
max_consecutive_infra = 5      # RPC flake tolerance, 0 fails on the first Infra
heartbeat = "60s"
env = { target = "rpc" }       # per profile env override, shallow over [env]

[profile.deploy-base]
mode = "scenario"
env = { target = "rpc", chains = ["eth"] }
export_world = "artifacts/deploy-world.json"   # later phase, see section 10

  [[profile.deploy-base.steps]]
  op = { Deposit = { chain = "eth", user = 0, amount = 1000 } }
  expect = "accepted"          # default
  delay = "2s"                 # pacing for live chains, default "0s"

  [[profile.deploy-base.steps]]
  op = { Withdraw = { chain = "eth", user = 0, amount = 2000 } }
  expect = "rejected"          # the model says this must revert
  check = false                # skip the invariant sweep after this step

[suite.nightly]
profiles = ["deep", "invariant-long", "soak"]
stop_on_failure = false
```

### 4.2 Top level blocks

| block | required | meaning |
|---|---|---|
| `[harness]` | yes | `name` (registry key) and optional `setup` (named setup, default `"default"`) |
| `[[chain]]` | no | one chain declaration per entry; built by `build_chain` at run time (section 4.6). Omitted means the setup fn hard codes chains |
| `[env]` | no | default environment request for every profile (section 6.2) |
| `[defaults]` | no | table shallow merged under every profile, profile keys win |
| `[profile.<name>]` | at least one | one runnable configuration |
| `[suite.<name>]` | no | ordered pipeline of phases (`[[suite.<name>.phases]]`, section 4.7) plus `stop_on_failure` (bool, default false). The legacy `profiles = [...]` list is sugar for dependency free, fresh world phases (updated: see 4.7) |
| `[replay]` | no | provenance metadata written by the artifact writer, ignored by the run schema (section 10) |

There is no `single` mode. A scenario with one step is the same thing.

Unknown keys are hard errors (`deny_unknown_fields` everywhere). Typo safety is the point of a validated schema. The one exception: `[defaults]` keys that do not apply to a given profile's mode are stripped before typed deserialization against a per mode allowlist, each strip emitting a warning, so a shared `check_every` default does not explode a scenario profile.

### 4.3 Common profile keys (all modes)

| key | type | default | meaning |
|---|---|---|---|
| `mode` | `"fuzz"` \| `"invariant"` \| `"endurance"` \| `"scenario"` | required | selects the driver |
| `seed` | integer or `"random"` | 0 | negative or `"random"` picks a fresh seed per run and prints it, matching the existing macro convention |
| `check_every` | integer >= 0 | 1 | invariant cadence, 0 means never mid run |
| `stats` | bool | false | enables `Runner::with_stats()` |
| `artifacts_dir` | path string | `"target/cross-vm"` | replay artifacts and reports land here |
| `json_report` | path string | none | write the run report as JSON |
| `env` | inline table | top level `[env]` | per profile override, shallow merge |
| `shrink` | bool | true for fuzz and invariant, false for endurance | auto shrink a failing history before writing the artifact |
| `shrink_limit` | integer | 256 | shrink replay budget, today's `DEFAULT_SHRINK_LIMIT` becomes the default of a parameter |

### 4.4 Mode specific keys

Fuzz:

| key | type | default | meaning |
|---|---|---|---|
| `cases` | integer > 0 | required | fan out count; case `i` is seeded `sub_seed(seed, i)` with a fresh setup per case |
| `ops` | integer > 0 | required | sequence length per case |
| `kinds` | array of kind names | all kinds | restricted uniform draw |
| `weights` | table of kind name to integer weight | none | weighted draw (section 6.1), mutually exclusive with `kinds` |

Invariant: `ops` (required), `kinds`, `weights`, same semantics, one long run, no `cases`.

Endurance:

| key | type | default | meaning |
|---|---|---|---|
| `duration` | duration string | required unless `max_ops` is set | wall clock bound |
| `max_ops` | integer | none | op count bound, whichever bound hits first stops the run |
| `base_delay` | duration string | `"0s"` | floor between ops |
| `max_delay` | duration string | `"0s"` | jitter ceiling on top of `base_delay` |
| `advance_blocks` | integer | none | blocks advanced per op |
| `block_jitter` | integer | 0 | extra random blocks per advance |
| `max_consecutive_infra` | integer | 0 | tolerated consecutive `Infra` failures before the run fails, 0 keeps today's fail on first behavior |
| `heartbeat` | duration string | `"60s"`, `"0s"` disables | periodic info log with steps, elapsed time, coverage and stats snapshots |

Endurance also accepts `kinds` and `weights`.

Scenario:

| key | type | default | meaning |
|---|---|---|---|
| `steps` | array of tables | required, non empty | ordered concrete ops |
| `export_world` | path string | none | later phase, serialize the final `World` as JSON |

Per step:

| key | type | default | meaning |
|---|---|---|---|
| `op` | table or string (externally tagged op enum) | required | one `H::Operation` |
| `expect` | `"accepted"` \| `"rejected"` \| `"any"` | `"accepted"` | verdict assertion (section 6.3) |
| `delay` | duration string | `"0s"` | sleep before this step, live chain pacing |
| `check` | bool | true | run the invariant sweep after this step, subject to `check_every = 0` disabling all sweeps |

### 4.5 Durations and seeds

Durations are strings only, parsed with the humantime grammar (`"8h"`, `"500ms"`, `"1h 30m"`) through a `#[serde(with = ...)]` adapter. Bare integers are rejected at load with a hint to write `"500ms"`; unit ambiguity is not worth the keystrokes saved, and string durations keep the YAML and JSON paths identical.

`seed` deserializes from an integer or the string `"random"`. Any negative integer also means random, mirroring `#[fuzz_runner(seed = -1)]`. Resolution to a concrete `u64` happens at run time in the framework, which prints a "set seed = N to reproduce" line.

### 4.6 Chain declarations

Each `[[chain]]` entry declares one chain the framework builds via `build_chain` (section 6.2). The label is the `MultiChainEnv` injection key and the value used in op fields (`chain = "eth"`). Labels must be unique within a config file.

**Common fields (all kinds):**

| key | type | required | default | meaning |
|---|---|---|---|---|
| `label` | string | yes | — | injection key into `MultiChainEnv` |
| `kind` | `"cosmwasm"` \| `"evm"` \| `"svm"` \| `"tron"` | yes | — | selects the compiled VM backend |
| `chain_id` | string | yes | — | canonical chain id (e.g. `"osmosis-1"`, `"1"`, `"devnet"`) |
| `name` | string | no | `label` | human readable name |
| `native_symbol` | string | no | per kind default | token symbol (e.g. `"OSMO"`, `"ETH"`, `"SOL"`, `"TRX"`) |
| `rpc_url` | string | no | none | RPC endpoint; required when `target = "rpc"` for this chain |
| `target` | `"mock"` \| `"rpc"` | no | top level `[env].target` | per chain mock vs rpc override |
| `params` | inline table | no | none | free form metadata passed through to `ChainSpecData` |

**CosmWasm (`kind = "cosmwasm"`) additional fields:**

| key | type | required | default | meaning |
|---|---|---|---|---|
| `bech32_prefix` | string | yes | — | address prefix (e.g. `"osmo"`) |
| `native_denom` | string | yes | — | fee denom (e.g. `"uosmo"`) |
| `gas_price` | float | no | `0.025` | indicative gas price in `native_denom` per gas unit (metadata only on mock) |

**EVM (`kind = "evm"`) and Tron (`kind = "tron"`) additional fields:**

| key | type | required | default | meaning |
|---|---|---|---|---|
| `spec_id` | string | no | `"cancun"` | revm hardfork; parsed to `SpecId`. Valid values: `frontier`, `homestead`, `tangerine`, `spurious`, `byzantium`, `constantinople`, `petersburg`, `istanbul`, `muir`, `berlin`, `london`, `paris`, `shanghai`, `cancun`, `prague` |

**Solana (`kind = "svm"`) additional fields:**

| key | type | required | default | meaning |
|---|---|---|---|---|
| `ws_url` | string | no | none | websocket endpoint for subscriptions |
| `commitment` | string | no | `"finalized"` | parsed to `Commitment`. Valid values: `processed`, `confirmed`, `finalized` |

**Selection semantics.** `[[chain]]` declares the pool of available chains. `[env].chains` (and per profile `env = { chains = [...] }`) selects a label subset for a given run; omitted means all declared chains. A label in `chains` that does not match any `[[chain]].label` is a hard load error.

**Preset equivalence.** The `[[chain]]` entries in the full example (section 4.1) are equivalent to today's `OSMOSIS`, `ETHEREUM`, and `SOLANA_DEVNET` presets, but with config supplied `rpc_url` and no Rust edit to switch endpoints.

**Compiled VM boundary.** `kind` must be one of the four backends compiled into the framework. Config cannot introduce a new VM; that requires a new crate with a `ChainProvider` implementation, `WalletDeriver`, and an `AnyChain` variant.

### 4.7 Pipeline suites: `[[suite.<name>.phases]]`

A suite can express an ordered pipeline of phases instead of (or in place of) the legacy flat `profiles` list, so a later phase can require an earlier one to have passed, and optionally continue from the exact `(Ctx, World)` the earlier phase ended with, rather than paying a fresh setup again.

```toml
[suite.progressive]

  [[suite.progressive.phases]]
  profile = "deposit-soak"

  [[suite.progressive.phases]]
  profile = "mixed-after-deposits"
  needs = ["deposit-soak"]
  world = "inherit"
```

Per phase keys:

| key | type | default | meaning |
|---|---|---|---|
| `profile` | string | required | a `[profile.<name>]` in the same config file |
| `needs` | array of phase profile names | empty | earlier phases in this suite that must have passed; a failed or skipped dependency skips this phase |
| `world` | `"fresh"` \| `"inherit"` | `"fresh"` | `"fresh"` builds a new environment via the registered setup fn (today's behavior); `"inherit"` starts from the live environment and world the donor phase (the single `needs` entry) finished with |

Structural rules, checked at load time (a violation is a hard config error, `cross-vm validate` exit code 3):

| rule | detail |
|---|---|
| declaration order is execution order | `needs` may only name a phase declared earlier in the same suite; a self reference or a forward reference is rejected |
| unique phase profiles | a profile may appear as at most one phase within a suite, so `needs` entries name it unambiguously |
| `world = "inherit"` arity | requires exactly one `needs` entry, the donor |
| single setup ends | both the donor and the inheriting phase must build exactly one starting world: `invariant`, `endurance`, `scenario`, or `fuzz` with `cases == 1`. A multi case fuzz fans out into many independent worlds, so it can neither donate nor consume a single inherited world |
| one inheritor per donor | a donor may feed at most one inheriting phase; two phases inheriting from the same donor is a hard error (state forking is not implemented in this milestone; a later replay based fork is tracked separately) |

A skipped or failed dependency skips the dependent phase entirely; a skip contributes nothing to the suite's combined exit code (only an actual failure does). The legacy `profiles = [a, b]` sugar is normalized into fresh, dependency free phases (`needs = []`, `world = "fresh"`) by the loader, so a config written before this feature keeps behaving exactly as it did.

## 5. Loader pipeline: the `cross-vm-config` crate

A new pure data crate at `crates/config`, package name `cross-vm-config`. No framework dependency, no tokio, no chains. This purity is what makes YAML later a one function addition, keeps the loader unit testable with plain string fixtures, and lets the phase 5 macro bridge reuse it verbatim.

```rust
pub struct ChainDecl {
    pub label: String,
    pub kind: String,              // "cosmwasm" | "evm" | "svm" | "tron" — stays String here
    pub chain_id: String,
    pub name: Option<String>,
    pub native_symbol: Option<String>,
    pub rpc_url: Option<String>,
    pub target: Option<String>,    // "mock" | "rpc"; resolved against [env].target in framework
    pub params: Option<toml::Table>,
    // per-kind optional fields (present only when applicable)
    pub bech32_prefix: Option<String>,
    pub native_denom: Option<String>,
    pub gas_price: Option<f64>,
    pub spec_id: Option<String>,
    pub ws_url: Option<String>,
    pub commitment: Option<String>,
}

pub struct RunConfig {
    pub harness: HarnessRef,                  // name + setup
    pub chains: Vec<ChainDecl>,                 // [[chain]] entries; empty = setup hard codes chains
    pub env: EnvSpec,                         // target, chains (label selection), params: toml::Table
    pub profiles: BTreeMap<String, Profile>,  // typed, defaults merged
    pub suites: BTreeMap<String, Suite>,
}

pub fn load(path: &Path, vars: &dyn Fn(&str) -> Option<String>) -> Result<RunConfig, ConfigError>;
pub fn from_toml_str(s: &str, vars: &dyn Fn(&str) -> Option<String>) -> Result<RunConfig, ConfigError>;
```

Five ordered stages, each independently testable:

1. **Parse** to `toml::Value`, not straight to typed structs, so merging and interpolation see the raw document.
2. **Interpolate.** Walk every string value. `${VAR}` errors with the TOML path when unset, `${VAR:-default}` falls back, `$${` escapes a literal. The variable lookup is injected (the CLI passes a closure over `std::env::var` after loading `.env` via dotenvy), so loader tests are deterministic. Interpolated values are never echoed in error messages or `validate` output, since they may carry RPC keys. Errors name the variable, not its value.
3. **Merge.** Shallow key level merge of the `[defaults]` table into each `[profile.*]` table, profile wins. The profile local `env` inline table shallow merges over the top level `[env]`.
4. **Typed deserialize.** `Profile` is an internally tagged serde enum (`#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]`) with `FuzzProfile`, `InvariantProfile`, `EnduranceProfile` and `ScenarioProfile` payloads. Inapplicable `[defaults]` keys are stripped per mode with warnings before this stage.
5. **Structural validation.** `cases > 0`, non empty `steps`, `duration` or `max_ops` present, `kinds` and `weights` mutually exclusive, suite profile names exist. Chain validation (config crate, no framework types):
   * `[[chain]]` labels are unique.
   * `kind` is non empty (framework resolves to `ChainKind` at run time; unknown kind is a framework error).
   * `chain_id` is present on every `[[chain]]`.
   * Per kind required fields: `bech32_prefix` and `native_denom` when `kind = "cosmwasm"`.
   * Every label in `[env].chains` (after profile merge) matches a declared `[[chain]].label` when `[[chain]]` entries exist.
   * `spec_id` and `commitment` strings are validated at the framework layer (section 6.2), not in the config crate, since they map to revm/Solana enums.

Kind names stay as `Vec<String>` and `BTreeMap<String, u32>` at this layer, and scenario ops stay as raw `toml::Value`. The config crate never sees harness types. Registry dependent validation (harness name, kind names, op shapes) happens in the framework (section 8), because only a monomorphized entry can deserialize them.

New workspace dependencies: `serde` (derive), `toml`, `humantime`. `clap` is added too but only reached through the framework's `cli` feature.

Module layout:

```
crates/config/src/
  lib.rs          # RunConfig, load(), errors
  schema.rs       # typed structs (ChainDecl, Profile, FuzzProfile, ...)
  interpolate.rs  # ${VAR} / ${VAR:-default} over toml::Value strings
  merge.rs        # defaults into profile table merge
  duration.rs     # humantime serde adapters
  seed.rs         # SeedSpec (integer | "random" | negative integer)
  chain.rs        # ChainDecl serde, per-kind field presence validation
```

## 6. Framework additions

All framework side code lives in a new `crates/framework/src/config/` module behind a new `cli` cargo feature, plus a small `serde` feature (section 9). Library consumers who never touch the CLI pay nothing.

### 6.1 `KindMix` and `run_with`

How generated runs pick the next op kind becomes explicit:

```rust
/// How generated runs pick the next op kind.
pub enum KindMix<'a, K> {
    /// Every kind from Harness::op_kinds, static weight 1 each (the mix is then purely the
    /// harness's dynamic weights; with the default weight this is a uniform draw). Config
    /// emits this when neither kinds nor weights is set.
    Harness,
    /// A subset of kinds, static weight 1 each (uniform up to dynamic weights).
    Restricted(&'a [K]),
    /// Config supplied static weights per kind, in sorted-kind-name order (the loader hands
    /// weights as a BTreeMap). Each static weight is multiplied per draw by Harness::weight.
    Weighted(&'a [(K, u32)]),
}

impl<H: Harness, M: Sequential> Runner<H, M> {
    pub async fn run_with(
        &mut self,
        ops: usize,
        mix: KindMix<'_, H::OpKind>,
        check_every: usize,
    ) -> RunReport<H::Operation>;
    // the existing run(ops, kinds, check_every) becomes sugar over run_with
}
```

Implementation constraint: the weighted path is one new arm in `OpSource` (or a third variant `Weighted { pairs, remaining }`). The existing `Generated` and `Fixed` arms keep their exact draw sequences so the golden seed test in `mechanics.rs` passes untouched, and the weighted path gets its own golden test so it is pinned from birth. The iteration order of config supplied weight pairs is the sorted kind name order (the loader hands over a `BTreeMap`), documented so the same file always yields the same op stream.

Precedence, documented in the schema: `weights` beats `kinds`, and both compose with the harness's dynamic `weight` (effective weight is static times dynamic, so a dynamic 0 always excludes a kind). A zero total static weight or an empty pair list produces the same infra failure an empty `kinds` slice already produces; a mix whose *effective* weights are all 0 for the current state fails the run at that draw.

The endurance driver gains the same `mix` parameter through its config (section 6.4).

### 6.2 `SetupRequest` and `build_chain`: how config reaches the setup fn

Chain construction is config driven by default. When `[[chain]]` entries are present, the framework resolves them into `ChainSpecData` values, filters by the profile's `chains` label selection, and passes the result to the setup fn via `SetupRequest::chain_specs`. A generic setup injects each spec with `build_chain`; harness specific work (funding, deploy, model priming) stays in the setup fn. When no `[[chain]]` entries are present, `chain_specs` is empty and the setup fn hard codes chains exactly as today (backward compatible).

```rust
pub enum Target { Mock, Rpc }

/// Framework resolved chain spec (owned strings, parsed enums).
pub struct ChainSpecData {
    pub label: String,
    pub kind: ChainKind,           // parsed from ChainDecl.kind via FromStr
    pub chain_id: String,
    pub name: String,
    pub native_symbol: String,
    pub rpc_url: Option<String>,
    pub target: Target,            // per-chain override or profile default
    pub params: toml::Table,
    // per-kind fields (Some only when applicable)
    pub bech32_prefix: Option<String>,
    pub native_denom: Option<String>,
    pub gas_price: Option<f64>,
    pub spec_id: Option<SpecId>,       // EVM/Tron; parsed from string
    pub ws_url: Option<String>,
    pub commitment: Option<Commitment>, // SVM; parsed from string
}

pub struct SetupRequest {
    pub target: Target,                // profile default target
    pub chains: Vec<String>,           // requested label subset; empty = all declared chains
    pub chain_specs: Vec<ChainSpecData>, // resolved, selection-filtered; empty = setup hard codes chains
    pub params: toml::Table,           // [env.params] verbatim
    pub seed: u64,                     // resolved per run (per case for fuzz)
}

pub type SetupFuture<'a, W> =
    Pin<Box<dyn Future<Output = Result<(Ctx, W), HarnessError>> + 'a>>;

/// Build one AnyChain from a resolved ChainSpecData.
pub fn build_chain(
    spec: &ChainSpecData,
    wallets: Rc<WalletFactory>,
) -> Result<AnyChain, HarnessError> {
    let target = spec.target;
    match spec.kind {
        ChainKind::CosmWasm => { /* construct CosmosChainInfo from spec fields, .mock/.rpc */ }
        ChainKind::Evm => { /* construct EvmChainInfo, parse spec_id */ }
        ChainKind::Svm => { /* construct SolanaChainInfo, parse commitment */ }
        ChainKind::Tron => { /* construct TronChainInfo */ }
    }
}
```

**`build_chain` implementation notes:**

* Lives in `crates/framework/src/config/build_chain.rs` (has chain crate dependencies; the config crate stays pure).
* Constructs an owned `*ChainInfo` from `ChainSpecData` fields, then calls `.mock(wallets)` or `.rpc(wallets)` based on `spec.target`.
* `ChainKind` gains `FromStr` (today it has `Display` only). Unknown `kind` string is a hard error listing valid values.
* `SpecId` and `Commitment` parse from the string tables in section 4.6; unknown value errors with valid names listed.
* String fields on `*ChainInfo` are `&'static str` today (required by `const` presets and `MockApiBech32::new`). Config sourced strings are interned via `Box::leak(spec.chain_id.into_boxed_str())` once per declared chain per run (bounded; one intern per field per chain). Alternatively, fields move to `Cow<'static, str>` in a follow up refactor.

**Generic config driven setup example:**

```rust
async fn vault_config_setup(req: SetupRequest) -> Result<(Ctx, VaultWorld), HarnessError> {
    let wallets = test_wallets();
    let mut env = MultiChainEnv::new("vault-harness", wallets.clone());

    if req.chain_specs.is_empty() {
        // backward compatible: hard code presets when no [[chain]] declared
        let target = req.target;
        env.inject("osmosis", chain_for_target(OSMOSIS, target, wallets.clone())?);
        env.inject("eth", chain_for_target(ETHEREUM, target, wallets.clone())?);
        env.inject("solana", chain_for_target(SOLANA_DEVNET, target, wallets)?);
    } else {
        for spec in &req.chain_specs {
            env.inject(&spec.label, build_chain(spec, wallets.clone())?);
        }
    }

    let ctx = Ctx::new(env.start().await?);
    // harness specific: fund, deploy, prime model (unchanged)
    Ok((ctx, deploy_vault_world(&ctx, &req).await?))
}
```

**Selection and target resolution.** The registry's `run` closure assembles `SetupRequest` from the resolved profile:

1. Start with all `[[chain]]` entries from `RunConfig.chains`.
2. Filter to labels in `env.chains` when non empty; otherwise keep all.
3. Resolve each chain's `target`: per chain `[[chain]].target` wins, then profile `env.target`, then top level `[env].target` (default `"mock"`).
4. Parse `kind`, `spec_id`, `commitment` strings into typed enums; error on unknown values.

`SetupRequest.target` carries the profile default; `ChainSpecData.target` carries the per chain resolved value used by `build_chain`.

Multiple named setups remain supported: `[harness] setup = "lean"` selects among registered names. Named setups are for genuinely different topologies (different funding strategies, deploy sequences) rather than mock versus rpc switching, which `target` and `[[chain]]` handle declaratively.

### 6.3 Scenario driver with expectations

`Verdict` is currently consumed inside `step()` and never reaches `RunReport`, so per step `expect` cannot be checked from outside. Expectation checking therefore moves into a new scenario driver where the verdict is visible:

```rust
pub enum Expectation { Accepted, Rejected, Any }

pub struct ScenarioStep<Op> {
    pub op: Op,
    pub expect: Expectation,   // default Accepted
    pub delay: Duration,       // default ZERO
    pub check: bool,           // default true
}

impl<H: Harness> Runner<H, Scenario> {
    pub async fn run_steps(
        &mut self,
        steps: Vec<ScenarioStep<H::Operation>>,
        check_every: usize,
    ) -> RunReport<H::Operation>;
}
```

A verdict contradicting `expect` becomes `FailureKind::Bug("step N: expected rejection, operation was accepted")` (and the mirror image), reusing the existing failure, coverage and shrink machinery unchanged. `run_scenario` and `run_case` remain as sugar (`Expectation::Any`, no delay).

This is what turns scenario plus rpc into an on chain scripting tool: `expect = "accepted"` per deployment step, `delay` for pacing, and the harness invariants become post deployment health checks. The imperative pattern in `examples/scripts/deploy_counter/main.rs` becomes a config file.

### 6.4 Endurance extensions

`EnduranceConfig` gains fields (builder methods added, defaults preserve today's behavior; the struct also becomes `#[non_exhaustive]`, a pre 1.0 break noted in the changelog):

```rust
pub max_ops: Option<usize>,        // stop on op count OR duration, first wins
pub max_consecutive_infra: usize,  // 0 = fail on the first Infra (today's behavior)
pub heartbeat: Duration,           // ZERO = off; periodic info log
pub stop: Option<Arc<AtomicBool>>, // cooperative cancel, checked at loop top
```

Driver changes:

* Bound check at the top of the loop, `max_ops` or `duration`, first hit wins.
* On an `Infra` failure, increment a consecutive counter and continue while under `max_consecutive_infra`, resetting on any success and recording the skip in stats; fail when exceeded.
* Heartbeat emits a `tracing::info!` line with steps, elapsed time, a coverage snapshot and a stats snapshot.
* When `stop` flips, break out, run the final invariant sweep, and return the report as a pass annotated in the log ("stopped by signal after N ops").

The CLI installs `tokio::signal::ctrl_c` to flip the flag, so a SIGINT on an eight hour soak still yields a full report, artifacts and the correct exit code. A second ctrl-c hard exits (the standard double ctrl-c contract). Stopping is cooperative, never a `select!` abort mid `apply`, so the process global `wallet_lock` is never stranded and a live broadcast is never left half observed.

Deferred: stop file polling, resume seed continuation, staged load (section 12).

### 6.5 Pipeline handoff: the session slot

A `world = "inherit"` phase (section 4.7) is served by one session slot per registered harness, `Rc<RefCell<Option<(Ctx, H::World)>>>`, alive for the whole registry's lifetime. A donor phase whose ending world a later phase inherits stashes its final `(Ctx, World)` pair into the slot after it passes; the inheriting phase takes (moves) the pair out of the slot, leaving it empty again. Because the CLI process runs exactly one invocation and every inheritor consumes the slot by move rather than by reference, an accidental reuse (an inheriting phase whose donor never ran, or already handed its world to someone else) finds an empty slot and fails loudly with `RunError::Invalid`, never a silent reuse of stale state. A donor that fails never stashes anything, so a dependent phase gated on it is skipped by the `needs` rule (section 4.7) before it would ever reach the empty slot.

Two behaviors follow from the pair being a real, live, moved value rather than a serialized snapshot. First, shrinking an inherited phase's failure is disabled: a shrink rebuild always starts from a fresh setup, which cannot reproduce the state a donor handed over, so shrinking under a different starting world would compare unrelated runs; the raw (unshrunk) failing history is kept instead. Second, a replay artifact written for an inherited phase's failure records `world_source = "inherited"` in its `[replay]` provenance and warns in the log: a standalone `cross-vm replay` of that artifact starts from a fresh setup, exactly like every other artifact, so it may not reproduce the same failure the pipeline run saw. Both caveats lift with the forthcoming replay fork design, which rematerializes an inherited starting state by replaying the donor's accepted op history instead of requiring the live pair.

## 7. Registry and type erasure

`Harness` has four associated types and async fns, so it is not dyn compatible. The bridge is one closure pair per registered harness, monomorphized at registration. Chain construction (`build_chain`, `ChainSpecData` resolution) lives in the framework config module, not in the registry itself; the registry's `run` closure assembles `SetupRequest` (including `chain_specs`) before calling the user setup fn.

```rust
pub(crate) type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Mode agnostic outcome of one profile run: RunReport with the op type erased.
pub struct ErasedReport {
    pub harness: String,
    pub profile: String,
    pub mode: String,
    pub seed: u64,
    pub steps: usize,
    pub skipped: usize,
    pub coverage: Coverage,           // already string keyed
    pub stats: Option<Stats>,
    pub elapsed: Duration,
    pub failure: Option<ErasedFailure>,
}

pub struct ErasedFailure {
    pub step: usize,
    pub kind: FailureKind,
    pub op_debug: Option<String>,     // Debug rendering for the human log
    pub history: serde_json::Value,   // serialized ops, feeds the replay artifact
    pub shrunk: bool,
}

struct Entry {
    /// Deserialize check kinds/weights/steps against H's enums without running.
    validate: Box<dyn Fn(&Profile) -> Result<(), ValidationError>>,
    /// Run one resolved profile end to end (setup, drive, report).
    run: Box<dyn for<'a> Fn(&'a ResolvedProfile, &'a RunOptions)
                 -> LocalBoxFuture<'a, Result<ErasedReport, RunError>>>,
}

pub struct Registry { entries: BTreeMap<String, Entry> }

impl Registry {
    pub fn register<H, F, S>(&mut self, name: &str, harness: F, setup: S) -> &mut Self
    where
        H: Harness + 'static,
        H::Operation: Serialize + DeserializeOwned + 'static,
        H::OpKind: Serialize + DeserializeOwned + Copy + 'static,
        F: Fn() -> H + 'static,
        S: for<'a> Fn(SetupRequest) -> SetupFuture<'a, H::World> + 'static,
    { /* builds both closures, monomorphized over H */ }

    pub fn register_setup<H, S>(&mut self, name: &str, setup_name: &str, setup: S) -> &mut Self;
}
```

The serde bounds live here and only here. A `ConfigHarness` blanket marker trait documents the requirement (`Harness` where `Operation` and `OpKind` round trip through serde); existing harnesses that never touch the CLI compile unchanged, and the vault example adds two derive lines.

Inside the `run` closure, everything is generic over `H`, so no dyn `Harness` ever exists:

* **setup assembly**: resolve `[[chain]]` entries into `chain_specs` (section 6.2), merge profile `env`, build `SetupRequest { target, chains, chain_specs, params, seed }`.
* **fuzz**: for each case `i` in `0..cases`, compute `seed_i = sub_seed(seed, i)`, run a fresh `setup(SetupRequest { seed: seed_i, .. })`, build `Runner::fuzz(harness(), seed_i)`, optionally `with_stats`, then `run_with(ops, mix, check_every)`. The first failing case ends the profile (the log names the case, its report is kept). Case progress logs at info.
* **invariant**: one setup, `Runner::invariant`, `run_with`.
* **endurance**: one setup, `Runner::endurance`, an `EnduranceConfig` built from the profile plus the CLI's stop flag.
* **scenario**: deserialize each step's stored `toml::Value` into `H::Operation` (`toml::Value` implements `Deserializer`, so this is `H::Operation::deserialize(value)`), build the `ScenarioStep`s, `run_steps`.
* On a failure in fuzz or invariant with `shrink = true`: `Runner::scenario(..).shrink_with(history, check_every, rebuild)` where `rebuild` re invokes the stored setup fn with the same seed; the (shrunk) history is then serialized with `serde_json` into `ErasedFailure.history`.

The `validate` closure powers `cross-vm validate` and pre run checks: it parses every kind name in `kinds` and `weights` through `H::OpKind` and every scenario `op` through `H::Operation`, without touching a chain.

### 7.1 Op and kind serialization conventions

**Externally tagged enums** (serde's default, zero attributes on user enums) are the friendliest in TOML:

```toml
[[steps]]
op = { Deposit = { chain = "eth", user = 0, amount = 1000 } }

[[steps]]
op = "Ping"                # unit variants are plain strings

[[steps]]                  # or the long form as a sub table
[steps.op.Deposit]
chain = "eth"
user = 0
amount = 1000
```

Adjacently tagged would force `#[serde(tag = ..., content = ...)]` attributes on every user enum and reads worse in TOML. Users who want different casing use standard serde attributes and the config follows automatically.

**Kind names need no new machinery.** A fieldless enum with derived serde deserializes a unit variant from a bare string and serializes to one. So `kinds = ["Deposit"]` parses each string via `H::OpKind::deserialize`, and an unknown name produces serde's "unknown variant, expected one of ..." error, which already enumerates the valid names. `weights` keys parse the same way. The reverse mapping (for reports and artifact comments) is serialization to a string. No `strum`, no `kind_names()` method.

## 8. CLI

The user crate hosts a tiny binary:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    cross_vm_framework::cli::Cli::new()
        .env_file(".env")   // optional dotenvy path, default ".env"
        .register("vault", || VaultHarness, vault_config_setup)
        .register("counter", || CounterHarness, counter_config_setup)
        .main()
        .await
}
```

Subcommands (`clap` derive, behind the `cli` feature):

```
cross-vm run <config.toml> [--profile NAME]... [--suite NAME]
             [--seed N] [--ops N] [--cases N] [--duration 8h] [--target mock|rpc]
             [--stats] [--check-every N] [--json-report PATH] [--artifacts-dir DIR]
             [--no-shrink]
cross-vm validate <config.toml>    # loader + registry validation, no chains
cross-vm list <config.toml>        # profiles, suites, registered harnesses and setups
cross-vm replay <artifact.toml>    # sugar for: run <artifact.toml> --profile replay
```

Precedence, highest first: CLI flag, env override (`CROSS_VM_SEED`, `CROSS_VM_PROFILE`, the `FOUNDRY_PROFILE` and `PROPTEST_CASES` pattern), profile key, `[defaults]`, built in default.

Selection rules: with no `--profile` or `--suite` and exactly one profile in the file, that profile runs; otherwise the invocation must name one, and the error lists the available names. Multiple `--profile` flags run sequentially like a suite.

Exit codes (the CI contract): `0` all passed, `1` at least one run failed with `Bug` or `Invariant`, `2` failed with `Infra` only, `3` config or usage error. A suite reports the worst code.

Output: the human log goes through the existing tracing summary seam (the CLI installs a `tracing-subscriber` fmt layer honoring `RUST_LOG`, default `info`), plus a per profile one line result and a final table. `Cli::main` asserts the current thread runtime flavor, since the erased layer is `!Send` by design.

## 9. JSON reports

`--json-report PATH` (or the `json_report` profile key) writes:

```json
{
  "schema_version": 1,
  "invocation": { "config": "vault.cross-vm.toml", "profiles": ["deep"], "overrides": {} },
  "profiles": [ /* ErasedReport ... */ ]
}
```

This needs a new framework `serde` cargo feature adding `Serialize` to `Coverage`, `InvCoverage`, `FailureKind`, `Stats` and `OpStat`. The `cli` feature enables it.

## 10. Failure persistence and the replay loop

On any failed generative run (fuzz, invariant, endurance), the CLI writes `<artifacts_dir>/<harness>-<profile>-<seed>-<timestamp>.replay.toml`:

```toml
# Auto generated by cross-vm. Reproduce with: cross-vm replay <this-file>
[harness]
name = "vault"
setup = "default"

[[chain]]
label = "osmosis"
kind = "cosmwasm"
chain_id = "osmosis-1"
name = "Osmosis"
bech32_prefix = "osmo"
native_denom = "uosmo"
native_symbol = "OSMO"
gas_price = 0.025

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"
name = "Ethereum"
native_symbol = "ETH"
spec_id = "cancun"

[[chain]]
label = "solana"
kind = "svm"
chain_id = "devnet"
name = "Solana Devnet"
native_symbol = "SOL"
commitment = "finalized"

[env]
target = "mock"
chains = ["osmosis", "eth", "solana"]

[replay]                       # provenance, ignored by the run schema
source_profile = "deep"
source_mode = "fuzz"
case = 17
failure = "invariant NoBadDebt"
shrunk = true
framework_version = "0.1.0"

[profile.replay]
mode = "scenario"
seed = 6021349

[[profile.replay.steps]]
op = { Deposit = { chain = "eth", user = 0, amount = 731 } }

[[profile.replay.steps]]
op = { Borrow = { chain = "eth", user = 0, amount = 366 } }
```

Because the artifact **is** a valid config file with one scenario profile, the loop closes with zero new machinery: `cross-vm replay file.toml` is load plus run. The history is the shrunk sequence when shrinking succeeded, raw otherwise. Committing an artifact next to the config gives Foundry style regression persistence, and the bug issue template's "seed + mode + minimized history" ask becomes "attach the artifact".

The artifact writer emits the `[[chain]]` declarations from the source config (or the resolved `chain_specs` when the source config had none but the setup hard coded chains with known metadata). This ensures replay reproduces the exact chain set, endpoints, and VM kinds, not just the op history. Interpolated `rpc_url` values are written as resolved strings (the artifact is a reproduction tool, not a secret store; mnemonics and keys are never written).

**TOML integer limits.** TOML integers are i64 and vault ops carry `u128` amounts. Generated amounts in the examples fit i64, but the writer must handle serialization failure: when `toml::to_string` of a step fails (out of range integer, non string map key), the artifact falls back to a sidecar `*.replay.json` with identical structure, and `run` and `replay` accept `.json` config input from day one (`serde_json` deserializes the same schema, which is also the proof the schema is format agnostic). Documented guidance: prefer serde friendly field types in op enums, or a string encoding attribute for u128 fields.

**World export (later in phase 4).** `export_world = "path.json"` on a scenario profile serializes the final `World` (learned addresses, model state) via an opt in registration variant `register_persistent` requiring `H::World: Serialize`. A deploy run's addresses can then feed a follow up run; consuming them declaratively (`${world.addrs.eth}`) is the phase 6 runtime variables story, until then the setup fn can read the JSON itself through `[env.params]`.

## 11. Worked use cases

**Nightly endurance in CI.** One `soak` profile with `duration = "${ENDURANCE_DURATION:-8h}"`, `target = "rpc"`, `heartbeat = "60s"`, `max_consecutive_infra = 5`. CI runs `cross-vm run vault.cross-vm.toml --profile soak --json-report soak.json`. SIGINT or the duration bound both end with a full report; exit code 2 distinguishes flaky infrastructure from real failures; the JSON report is the archived artifact.

**Deployment scripting.** A `deploy-base` scenario profile with `target = "rpc"`, explicit steps, `expect` assertions, per step `delay`, and invariants acting as post deployment health checks. `cross-vm validate` type checks the script offline first (every op deserializes against the real `Operation` enum). This replaces the imperative `examples/scripts/deploy_counter` pattern for anything expressible as harness ops.

**Regression from a fuzz failure.** `deep` fails on case 17, the CLI shrinks the history and writes an artifact. The developer runs `cross-vm replay target/cross-vm/vault-deep-6021349-*.replay.toml`, fixes the bug, and commits the artifact as a pinned regression config.

## 12. Prior art and adopted knobs

Surveyed: Echidna (`testLimit`, `seqLen`, `shrinkLimit`, `corpusDir` YAML config), Medusa (workers, campaign JSON config), Foundry (`[fuzz]` and `[invariant]` sections, `runs`, `depth`, `fail_on_revert`, `shrink_run_limit`, dictionary weights, `FOUNDRY_PROFILE`, persisted failures), proptest (`cases`, `max_shrink_iters`, `PROPTEST_*` env overrides), cargo-fuzz and libFuzzer (corpus, `max_total_time`), k6 (stages, thresholds, graceful stop).

Adopted in this spec, ranked by value:

1. **Failure persistence plus one command replay** (Foundry persisted failures, Echidna corpusDir): section 10. The highest value per line of code in the whole system.
2. **Profile and env var override layering** (`FOUNDRY_PROFILE`, `PROPTEST_CASES`): section 8.
3. **Configurable shrink budget** (`shrink_limit`): the 256 constant becomes a parameter default.
4. **Graceful stop with a final report** (k6 gracefulStop): section 6.4.
5. **Infra failure tolerance for live runs**: `max_consecutive_infra`.

Good later additions, in rough order:

6. **Fuzz time budget** (libFuzzer `max_total_time`): a `max_time` cap on the cases loop for CI lanes.
7. **Threshold assertions over Stats** (k6 thresholds): for example `max_reject_rate = 0.8` failing a run that tested nothing; `Stats` already computes reject rates.
8. **Staged endurance load** (k6 stages): `stages = [{ duration = "10m", base_delay = "1s" }, ...]`.
9. **Value dictionaries beyond kind weights** (Foundry dictionary): needs harness cooperation; kind level weights cover the common case.

Deferred indefinitely: **parallel workers** (Medusa). The `Rc` based `!Send` single thread architecture means parallelism would be process level fan out (a `--jobs` orchestrator spawning child processes over case ranges), a separate design.

## 13. Implementation phases

Each phase is independently shippable and ends with docs (a SPEC.md pointer stays current, DEVELOPER.md usage, CHANGELOG entry).

**P1: the `cross-vm-config` crate.** New workspace member. Raw parse, interpolation with injected variable lookup, defaults and env merging, typed schema (including `ChainDecl` and `chains: Vec<ChainDecl>` on `RunConfig`), structural validation (`[[chain]]` label uniqueness, per kind required fields, selection label existence), `SeedSpec`, duration adapters, JSON input variant. Pure unit tests: golden good and bad TOML fixtures, interpolation edge cases (missing variable, default, escape), merge precedence, every mode's field table, mutual exclusion rules, chain declaration validation (duplicate labels, missing `bech32_prefix` on cosmwasm, unknown selection label). No framework changes.

**P2: `run_with(KindMix)`, registry, erasure, CLI for fuzz and invariant, and `build_chain`.** The golden seed test must pass untouched; a new weighted path golden test is added. `SetupRequest` (with `chain_specs`), `ChainSpecData`, `Target`, `build_chain` factory (owned `*ChainInfo` constructors with string interning, `ChainKind::FromStr`, `SpecId`/`Commitment` string parsing), `Registry`, `ErasedReport`, the fuzz and invariant drivers, the `Cli` builder with `run` (fuzz and invariant profiles only), `validate`, `list`, precedence and exit codes. The vault harness and its support code move from `examples/integration-tests/tests/` (now `examples/cross-vm-tests`) into that crate's `src/` lib (a bin target cannot see dev dependencies or test modules), with a `cross-vm` bin and a checked in `vault.cross-vm.toml` using `[[chain]]` declarations. The feature gated Makefile targets must keep passing unchanged. Tests: registry validation errors, an end to end CLI run over the mock vault with config defined chains, seed reproducibility across two invocations, `build_chain` round trip for each VM kind (mock and rpc), backward compatible setup with no `[[chain]]` entries.

**P3: scenario and endurance.** `Expectation`, `ScenarioStep`, `run_steps`; the scenario driver with per step deserialization, expect, delay, check. The `EnduranceConfig` extensions and the ctrl-c wiring. Tests: an expect mismatch becomes `Bug`, delay honored under a paused tokio clock, endurance stops on `max_ops`, the infra tolerance counter resets on success, the stop flag yields a passing report with a final sweep. This phase makes `target = "rpc"` scenario profiles a working deployment scripting tool.

**P4: reports and replay artifacts.** The framework `serde` feature, `--json-report`, the artifact writer (TOML with the JSON sidecar fallback, including `[[chain]]` declarations for replay fidelity), the `replay` subcommand, `shrink` and `shrink_limit` plumbing, `export_world` via `register_persistent`. Tests: artifact round trip (fail a seeded run, load the artifact, replay reproduces the same `FailureKind` and chain set), the u128 fallback path, a world export snapshot.

**P5: the `#[config_runner]` macro bridge.** An attribute generating `#[tokio::test]`s from a config file, for example `#[config_runner(config = "vault.cross-vm.toml", harness = VaultHarness, setup = vault_config_setup, profile = "smoke")]`, reusing the P1 loader at runtime inside the generated test. Fuzz profiles fan out per case like `#[fuzz_runner]`.

**P6 (optional): runtime variables.** Computed and late bound interpolation, for example `${world.addrs.eth}` consuming a P4 world export, or `${run.seed}` in paths. The P1 interpolator's value walk design leaves room for a second pass with a richer resolver.

## 14. Risk register

* **Serde requirements breaking existing harnesses.** Avoided: the bounds live on `Registry::register` only, `Harness` is unchanged, `ConfigHarness` is a blanket marker. A missing derive is a compile error at the registration site.
* **Golden seed reproducibility.** `run_with` adds a new `OpSource` arm; the existing arms keep their draw order, guarded by the mechanics golden seed test; the weighted path gets its own golden test; weight iteration order is pinned to sorted kind names.
* **TOML representation limits.** No u128, no null. Mitigated by the JSON sidecar fallback plus documented serde attribute guidance. Steps read back through `toml::Value`'s `Deserializer`, which handles every shape TOML can express.
* **Dyn erasure of async fns.** Local boxed futures, nothing `Send`, current thread runtime asserted at CLI start, matching the `Rc<WalletFactory>` reality.
* **Long run cancellation safety.** Cooperative flag checked between ops, never future abortion, so `wallet_lock` and in flight broadcasts are never severed; a second ctrl-c hard exits.
* **Duration ambiguity.** Strings only, humantime grammar, integers rejected at load with a hint.
* **Kind name drift.** Derived serde names are the single source. Renaming a variant invalidates old configs and artifacts loudly (unknown variant error listing valid names). `framework_version` in artifacts flags cross version replays; the ChaCha8 seed streams are already portable.
* **`[defaults]` holding mode specific keys.** Allowlist strip with warnings rather than `deny_unknown_fields` explosions, so typos stay visible without breaking shared defaults.
* **Secrets in config.** Interpolation exists for URLs and params, but the ".env holds secrets only" convention stands: mnemonics and keys are never interpolated into config visible values, and `validate` and log output never print interpolated strings.
* **`EnduranceConfig` field additions.** Struct literal construction breaks; acceptable pre 1.0, `#[non_exhaustive]` added, the builder is the documented path.
* **Integration test restructuring in P2.** Moving the harness and support code from `tests/` to `src/` touches module paths in existing test files; mechanical, but the feature gated Makefile targets must keep passing.
* **`&'static str` on `*ChainInfo` fields.** Config sourced strings do not satisfy `&'static str` or `MockApiBech32::new`'s prefix requirement. Mitigated by `Box::leak` interning (bounded: one per field per declared chain per run) or a follow up refactor to `Cow<'static, str>`. Documented in section 6.2.
* **`SpecId` / `Commitment` / `ChainKind` string parsing.** These are compiled Rust enums today with no `FromStr`. P2 adds parse tables (section 4.6); unknown values are hard load errors listing valid names. A typo in `spec_id = "cancn"` fails loudly.
* **Config cannot add a new VM.** `kind` must match one of the four compiled backends. A request for `kind = "move"` or `kind = "aptos"` errors at load time. Adding a VM requires a new crate, `AnyChain` variant, and `build_chain` arm. This is intentional: config parameterizes compiled behavior, it does not extend it.
