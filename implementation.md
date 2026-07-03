# Implementation Plan: TOML-Driven Test Runs (P1 to P6)

## Context

`docs/config-runs-spec.md` (commit 6efbe26, status "proposed, not implemented") specifies a declarative run-configuration layer: define harness plus ops once in Rust, declare any number of run combinations (fuzz, invariant, endurance, scenario) in TOML, run them via a user-crate `cross-vm` CLI binary. Today every run is a hand-written Rust test behind `#[fuzz_runner]`-style macros and cargo features; changing seed, duration, op mix, or mock vs rpc requires editing Rust and recompiling.

This plan implements all spec phases P1 to P6, plus one user-approved schema extension: a per-profile per-chain target map so one run can mix mock and rpc chains (e.g. eth live on RPC, osmosis and solana mock, in the same profile) without editing `[[chain]]` blocks.

Two load-bearing constraints from the spec:
1. Stack is single-threaded and `!Send` (`Rc<WalletFactory>`, current-thread tokio). All erased futures are `Pin<Box<dyn Future + 'a>>` non-Send.
2. Recorded seeds must keep reproducing. `OpSource` draw order is pinned by `golden_seed_sequence_is_stable` (examples/integration-tests/tests/harness/mechanics.rs:707). Config weights become a NEW `OpSource` arm; existing `Generated`/`Fixed` arms untouched.

## Approved extension: per-profile targets map

- `env.targets = { eth = "rpc" }` inline table; top-level `[env].targets` allowed too; profile map merges label-wise over top-level (profile labels win, others survive).
- Target resolution, highest wins: CLI `--target-chain LABEL=mock|rpc` (repeatable) > profile `env.targets[label]` > `[[chain]].target` > CLI `--target` > profile `env.target` > top-level `[env].target` > `"mock"`.
- All precedence funnels through one pure function `cross_vm_config::resolve_chain_target(label, decl_target, env, overrides)` used by load-time validation, CLI-time resolution, and the replay artifact writer.
- Validation: `targets` labels must exist in `[[chain]]`; any chain resolving to rpc must have `rpc_url` (post interpolation), hard error otherwise (exit 3 at CLI time when flags trip it).
- Replay artifacts write each chain's resolved target as `[[chain]].target` and omit `env.targets`, so replays reproduce the exact mock/rpc split.

## Workspace changes

Root `Cargo.toml`: add member `crates/config`; workspace deps `cross-vm-config`, `serde` (derive), `toml`, `humantime`, `clap` (derive).

`crates/framework/Cargo.toml`: optional deps `cross-vm-config`, `clap`, `serde`, `serde_json`, `dotenvy`, `tracing-subscriber`, `toml`; features:
```toml
serde = ["dep:serde"]
cli = ["serde", "dep:cross-vm-config", "dep:clap", "dep:serde_json", "dep:dotenvy", "dep:tracing-subscriber", "dep:toml"]
```
Tokio needs `signal` feature under `cli` (ctrl-c).

## P1: crates/config (cross-vm-config)

Pure data crate. Deps: serde, toml, serde_json, humantime, thiserror. No framework/tokio/chain deps (unit-testable, reused by P5 macro).

Files: `src/{lib,schema,chain,interpolate,merge,duration,seed,target,validate}.rs`, `tests/fixtures/*.toml`.

Entry points: `load(path, vars)`, `from_toml_str(s, vars)`, `from_json_str(s, vars)` where `vars: &dyn Fn(&str) -> Option<String>`. Returns `RunConfig { harness: HarnessRef, chains: Vec<ChainDecl>, env: EnvSpec, profiles: BTreeMap<String, Profile>, suites }` plus defaults-strip warnings.

Five loader stages (spec section 5): parse to `toml::Value`; interpolate `${VAR}` / `${VAR:-default}` / `$${` escape (error names var and TOML path, never echoes values); merge `[defaults]` into profiles (profile wins) and profile `env` over `[env]` (targets merge label-wise, see extension); typed deserialize; structural validation.

Key decisions:
- Mode dispatch manual, not serde-internally-tagged: pop `mode` key, deserialize matching per-mode struct with `deny_unknown_fields`. Avoids serde tag/deny_unknown_fields conflict, gives precise error paths.
- Common keys repeated per mode struct (`flatten` incompatible with `deny_unknown_fields`); `Profile::common()` accessor.
- `[defaults]` per-mode allowlist strip between merge and deserialize, stripped keys become warnings.
- `EnvSpec { target: Option<TargetStr>, targets: Option<BTreeMap<String, TargetStr>>, chains: Option<Vec<String>>, params: Option<toml::Table> }`; `TargetStr { Mock, Rpc }`.
- Durations humantime strings only (bare int rejected with hint); `SeedSpec::{Fixed(u64), Random}` from int, negative, or `"random"`.
- Scenario step ops stay raw `toml::Value`; kind names stay strings. Config crate never sees harness types.

Validation: cases > 0, non-empty steps, endurance duration-or-max_ops, kinds/weights mutually exclusive, suite names exist, chain label uniqueness, cosmwasm requires bech32_prefix and native_denom, env.chains labels exist, targets labels exist, rpc chains have rpc_url.

Tests: interpolation (missing var, default, escape, no value echo), merge precedence including targets label-wise merge, per-mode field tables, duration/seed parsing, chain validation errors, `resolve_chain_target` full precedence matrix, JSON input parity.

## P2: run_with/KindMix, build_chain, registry, CLI (fuzz plus invariant), vault migration

Order: (a) parsing primitives, (b) runner KindMix, (c) framework config module, (d) registry/erasure, (e) CLI, (f) vault migration plus bin plus TOML.

### (a) Parsing primitives
- `crates/core/src/chain_kind.rs`: `FromStr for ChainKind` (inverse of existing Display).
- `crates/solana/src/chains/commitment.rs`: `FromStr for Commitment`.
- SpecId: 15-entry name match table in framework `build_chain.rs` (frontier..prague per spec 4.6), error lists valid names.

### (b) KindMix in crates/framework/src/harness/runner.rs
```rust
pub enum KindMix<'a, K> { Harness, Restricted(&'a [K]), Weighted(&'a [(K, u32)]) }
pub async fn run_with(&mut self, ops, mix, check_every) -> RunReport<H::Operation>
```
Existing `run(ops, kinds, check_every)` (runner.rs:247) becomes sugar. `OpSource` (runner.rs:730) gains a third `Weighted { pairs, weights, remaining }` arm using `rng.weighted`; existing two arms byte-identical. Pair order is sorted kind-name order (BTreeMap from loader). Empty/zero weights = same Infra failure as empty kinds. Golden seed test untouched; add sibling `weighted_golden_seed_sequence_is_stable` pinning the new stream from birth.

### (c) Framework config module (new, behind cli feature)
`crates/framework/src/config/{mod,setup_request,build_chain,resolve,registry,erased}.rs`, `crates/framework/src/cli.rs`.

- `Target { Mock, Rpc }` with `From<TargetStr>`; `ChainSpecData` (owned strings, typed `ChainKind`, `Option<SpecId>`, `Option<Commitment>`, `Target`); `SetupRequest { target, chains, chain_specs, params, seed }`.
- `build_chain(spec, wallets: Rc<WalletFactory>) -> Result<AnyChain, HarnessError>`: per-kind arm builds owned `*ChainInfo` then `.mock(wallets)` or `.rpc(wallets)` (sugar in `crates/<vm>/src/chains/sugar.rs`); arms feature-gated with "kind not compiled in" fallback error. `&'static str` fields via thread-local intern cache (`Box::leak` only on first sight; bounded because fuzz re-runs setup per case). Defaults: name = label, per-kind native_symbol, gas_price 0.025, spec_id cancun, commitment finalized.
- `resolve.rs`: `RunOptions` (CLI overrides incl. `target_chains: BTreeMap<String, Target>`), `resolve_profile(cfg, name, opts) -> ResolvedProfile`. Filters `[[chain]]` by merged env.chains, resolves target per chain via `resolve_chain_target`, re-asserts rpc_url, parses enums. Scalar precedence: CLI flag > `CROSS_VM_*` env > profile key > `[defaults]` > built-in.

### (d) Registry and erasure (spec section 7)
`Entry { validate, run }` closure pair monomorphized in `Registry::register<H, F, S>`; serde bounds (`H::Operation: Serialize + DeserializeOwned`, `H::OpKind: Serialize + DeserializeOwned + Copy`) live ONLY here, `Harness` trait unchanged. `ErasedReport` and `ErasedFailure` per spec. Fuzz arm: per-case `sub_seed(seed, i)`, fresh setup, first failing case ends profile. Invariant arm similar. Scenario/endurance arms return UnsupportedMode until P3. Kinds/weights parse via `H::OpKind::deserialize(toml::Value::String(name))`; unknown name = serde unknown-variant error listing valid names.

### (e) CLI (crates/framework/src/cli.rs)
`Cli::new().env_file(".env").register(name, harness_fn, setup).main()`. Clap subcommands `run`, `validate`, `list` (replay in P4). `run` flags per spec 8 plus repeatable `--target-chain LABEL=mock|rpc`. Behavior: dotenvy, tracing-subscriber fmt honoring RUST_LOG (default info), assert current-thread flavor, profile selection rules (single profile auto-selected, else error listing names, multiple `--profile` sequential), exit codes 0/1/2/3, suite reports worst.

### (f) Vault migration (examples/integration-tests)
Bins cannot see tests/. Move with shim so existing tests compile untouched:
- `tests/harness/vault.rs` harness types plus `vault_setup` move to `src/vault.rs`; add `vault_config_setup(req: SetupRequest)` (empty chain_specs = hard-code today's three mock chains; else `env.inject(label, build_chain(spec, wallets)?)`, then existing fund/deploy code).
- `tests/support/{vault,wallets}.rs` (and whatever they pull in) move to `src/support/`; `tests/support/mod.rs` becomes shim re-exporting from the lib crate.
- `tests/harness/vault.rs` keeps only test fns using existing runner macros, importing from lib. Feature gates unchanged.
- `#[derive(Serialize, Deserialize)]` on VaultOp and VaultKind (externally tagged, matches spec 7.1 TOML shape).
- New bin `src/bin/cross_vm.rs` (spec 8 main, registers "vault"); Cargo.toml gains `[lib]`, `[[bin]] name = "cross-vm"`, promotes needed deps from dev-deps.
- New checked-in `examples/integration-tests/vault.cross-vm.toml`: three `[[chain]]` (osmosis/eth/solana mirroring presets), smoke/deep fuzz, invariant-long, plus a mixed profile example: `env = { targets = { eth = "rpc" } }`.

Tests: both golden tests green; registry validation errors; build_chain per kind x mock/rpc construction; resolve precedence incl. `--target-chain` > `env.targets` > `[[chain]].target`; CLI e2e (validate exits 0, run smoke passes on mocks, same seed twice identical, `--target-chain eth=rpc` without url exits 3, no-`[[chain]]` config runs hard-coded path); `make test-harness-all` green; framework builds without `cli` feature with zero serde in tree.

## P3: scenario driver and endurance extensions

`crates/framework/src/harness/runner.rs`:
- Refactor private `step()` to expose verdict (no rng change, golden safe).
- `Expectation { Accepted, Rejected, Any }`; `ScenarioStep<Op> { op, expect, delay, check }`; `Runner<H, Scenario>::run_steps(steps, check_every)`. Sleep delay, apply, verdict-vs-expect mismatch = `FailureKind::Bug("step N: expected rejection, operation was accepted")`. `run_case`/`run_scenario` become sugar.
- `EnduranceConfig` (runner.rs:64): `#[non_exhaustive]`, new fields `max_ops: Option<usize>`, `max_consecutive_infra: usize` (default 0), `heartbeat: Duration` (default 60s, zero off), `stop: Option<Arc<AtomicBool>>`, builder methods. CHANGELOG notes struct-literal break.
- Endurance driver (runner.rs:310): bound check max_ops/deadline first-wins plus stop check at loop top; Infra consecutive counter (continue under limit, reset on success, record skip); heartbeat tracing; stop flag = break, final sweep, passing report. Add endurance `run_with(cfg, mix)`; `Harness` mix keeps today's exact `harness.generate` stream.
- `registry.rs`: fill scenario arm (deserialize each step `toml::Value` into `H::Operation`, run_steps) and endurance arm (build config plus CLI stop flag).
- `cli.rs`: ctrl_c task flips shared AtomicBool; second ctrl-c hard exit. Cooperative only, never select!-abort around apply (wallet_lock safety).

Tests: expect mismatch = Bug with exact message; delay under start_paused; max_ops stop; infra counter tolerate/fail/reset; stop flag passing report with final sweep; check=false skips sweep; scenario profile in vault.cross-vm.toml (deposit then over-withdraw expect rejected) passes on mock; make test-endurance green.

## P4: JSON reports and replay artifacts

- `outcome.rs`/`stats.rs`: `#[cfg_attr(feature = "serde", derive(serde::Serialize))]` on Coverage, InvCoverage, FailureKind, Stats, OpStat.
- New `config/report.rs`: `write_json_report` with `schema_version: 1` envelope (spec 9).
- New `config/artifact.rs`: `write_replay_artifact(dir, source, resolved, report)` writes `<harness>-<profile>-<seed>-<timestamp>.replay.toml`, a valid config with one `[profile.replay]` scenario (shrunk history), `[[chain]]` decls with resolved targets (extension note above), `[replay]` provenance, resolved rpc_url strings, never secrets. On toml serialization failure (u128) falls back to sibling `.replay.json`, identical structure; `run`/`replay` accept `.json` from day one.
- `cli.rs`: `replay <artifact>` = sugar for `run <file> --profile replay`; on generative failure shrink via `Runner::scenario(..).shrink_with` (add `shrink_with_limit` for configurable `shrink_limit`, default `DEFAULT_SHRINK_LIMIT` 256) then write artifact.
- `registry.rs`: `register_persistent` (adds `H::World: Serialize`), `export_world` on scenario profiles.

Tests: artifact round trip (seeded failing mock run, artifact written, replay reproduces same FailureKind and chain set); u128 amount > i64::MAX forces .json sidecar and replays; JSON report snapshot on stable fields; world export write/reload.

## P5: #[config_runner] macro bridge

- New `crates/macros/src/config_runner.rs`, registered in macros lib.rs, arg parsing mirrors `runner_macros.rs`.
- `#[config_runner(config = "vault.cross-vm.toml", harness = VaultHarness, setup = vault_config_setup, profile = "smoke")]` expands to `#[tokio::test]`(s) calling a thin framework helper `config::test_bridge::run_profile_for_test<H, F, S>(path, harness, setup, profile, case)` reusing P1 loader and P2 registry internals.
- Fuzz fan-out: macro reads TOML at expansion time (CARGO_MANIFEST_DIR-relative) only to learn `cases`, one test per case; bridge asserts runtime cases match expansion count ("config changed since compile, rebuild").
- Usage test in example harness tests behind fuzz feature.

## P6 (optional): runtime variables

- `interpolate.rs`: generalize to `VarResolver` trait with `defer(name)` for `world.*`/`run.*` namespaces (left intact at load); existing closure callers wrap via adapter, P1 API stable.
- `resolve.rs`: second interpolation pass before each profile run; `run.seed` from resolved seed, `world.*` from an `import_world` JSON (P4 export).

## Risks

- Golden seed: every runner.rs change verified with `make test-harness`; Weighted arm golden test lands in same commit as the arm.
- Serde bounds isolated to Registry::register; verify framework builds without cli feature, no serde in tree.
- Box::leak bounded by thread-local intern cache; Cow refactor noted as follow-up.
- u128 vs TOML i64: JSON sidecar; document in DEVELOPER.md.
- EnduranceConfig break: non_exhaustive plus builders, CHANGELOG; grep for struct literals first.
- P2 vault move is highest regression risk: own commit, all make targets green before adding bin, shim keeps other test paths unchanged.
- Workspace enforces missing_docs warn: rustdoc every new pub item; update SPEC.md pointer, DEVELOPER.md, CHANGELOG.md each phase.

## Verification per phase

```sh
# P1
cargo test -p cross-vm-config && cargo clippy -p cross-vm-config -- -D warnings
# P2
cargo test -p cross-vm-framework --features cli
make test-harness-all && make test-cross-vm && make test-examples
cargo run -p cross-vm-integration-tests --bin cross-vm -- validate examples/integration-tests/vault.cross-vm.toml
cargo run -p cross-vm-integration-tests --bin cross-vm -- run examples/integration-tests/vault.cross-vm.toml --profile smoke
cargo run -p cross-vm-integration-tests --bin cross-vm -- run ... --profile smoke --target-chain eth=rpc   # expect exit 3
cargo build -p cross-vm-framework --no-default-features --features cw,evm,solana,tron   # cli off, no serde
# P3
make test-endurance && make test-fuzz && make test-invariant
# P4
cargo run ... -- run vault.cross-vm.toml --profile deep --json-report /tmp/r.json  # then replay written artifact
# P5
cargo test -p cross-vm-integration-tests --features fuzz config_runner
# always
cargo clippy --workspace --all-features -- -D warnings && cargo doc --workspace --no-deps
```
