# Repo restructure: root `contracts/`, `cross-vm-common` mocks, per-VM example test crates

Date: 2026-07-03
Status: Design (approved for implementation)

## Goal

Reorganize the repository so contract sources, their reusable type bindings, and
example tests each have a clear home. Concretely:

1. Introduce a root `contracts/` directory holding every example contract source
   (CosmWasm, Solidity, Solana, Tron).
2. Provide a single reusable bindings surface, `cross_vm_common::mocks`, so contract
   types (alloy `sol!` bindings, CosmWasm message types, Solana program IDs and
   discriminators, embedded artifact bytes) are declared once and reused everywhere.
3. Add one example test crate per VM (`evm-tests`, `cosmos-tests`, `solana-tests`,
   `tvm-tests`), each demonstrating all three test styles (attribute-macro harness,
   TOML config-driven, CLI) against a single-VM setup using the same harness code.
4. Keep the existing multi-chain suite as `examples/cross-vm-tests`.

The `crates/` library workspace stays unchanged in structure. Only internal path
references (dev-deps, embed paths) are updated.

## Background

The framework already supports four VMs behind the `ChainProvider` trait: CosmWasm
(`cw-multi-test`), EVM (`revm`), Solana (`litesvm`), and Tron (`revm` fork). Tests can
be written three ways, all sharing one harness `apply` function:

* Attribute-macro harness tests, via `#[fuzz_runner]` / `#[invariant_runner]` /
  `#[endurance_runner]` from `cross-vm-macros`.
* Config-driven tests, via `#[config_runner(config = "x.cross-vm.toml", ...)]` which
  reads the TOML at macro-expansion time and fans out `#[tokio::test]` cases.
* CLI-driven tests, via a `cross-vm` binary built on `cross_vm_framework::cli::Cli`,
  exercised end to end by a `cli_e2e.rs` subprocess test.

Today everything lives under `examples/`:

* Contract sources: `examples/{cosmwasm,solidity,solana,tron}-contracts`.
* Tests: one crate, `examples/integration-tests`, mixing single and multi-chain tests.
* Contract type bindings are duplicated across `tests/support/counter.rs`,
  `tests/support/ping_pong.rs`, `src/support/vault.rs`, and
  `examples/scripts/deploy_counter/artifacts.rs`. Each re-declares `alloy::sol!`
  blocks, Solana discriminators, program IDs, and `include_bytes!` embeds.

Two problems follow. Contract sources sit inside an `examples/` tree that also holds
tests, so navigation conflates "the thing under test" with "the test of it". And the
binding duplication means adding or changing a contract touches several files with
identical boilerplate.

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Contract location | Root `contracts/{cosmwasm,solidity,solana,tron}` | Separates subjects from tests; drops redundant `-contracts` suffix. |
| Bindings home | `cross_vm_common::mocks` module in `examples/common` | User chose to colocate mocks with shared test helpers rather than a standalone crate. One dependency edge for consumers. |
| Bindings shape | One crate, feature-gated per VM | Consumers enable only the VM they need; artifact compilation is required only for enabled features. |
| Feature names | `cw`, `evm`, `solana`, `tron`, plus `cw-artifacts` | Match `cross-vm-framework`'s feature names so consumers enable identical strings on both deps. `cw-artifacts` is split out because the CosmWasm `.wasm` is Docker built and gitignored (absent in CI). |
| Module names | `mocks::{contract}::{cw,evm,svm,tron}` | Match the `cw_*` / `evm_*` / `svm_*` / `tron_*` hook prefixes used by `#[cross_vm_contract]`. |
| Multi-chain suite | Keep as `examples/cross-vm-tests` (renamed from `integration-tests`) | Per-VM folders cannot hold cross-chain tests (ping-pong bridge, multi-chain vault). |
| Per-VM harnesses | Each test crate defines its own single-VM harness in `src/`, reusing `mocks` types | No forced generalization of harnesses over chain sets. |
| Shared helpers | `wallets` and `init_tracing` live ungated in `cross-vm-common` | Funding constants must stay identical across all consumers. |

## Target layout

```
contracts/
├── cosmwasm/{counter,ping-pong,vault}/
├── solidity/            (foundry)
├── solana/              (anchor)
└── tron/                (tronbox)
examples/
├── common/              cross-vm-common: mocks/ (feature-gated bindings) + wallets + init_tracing
├── cross-vm-tests/      multi-chain suite (was integration-tests)
├── evm-tests/  cosmos-tests/  solana-tests/  tvm-tests/
└── scripts/             consumes cross-vm-common mocks
crates/                  UNCHANGED
```

## `cross-vm-common` design

`publish = false`, `default = []` so a bare `cargo check` needs no built artifacts.

Features and their artifact requirements:

| Feature | Deps | Artifact | Built by |
|---------|------|----------|----------|
| `cw` | contract rlibs, `cosmwasm-std`, `cw-multi-test` | none | rlib path-deps |
| `evm` | `alloy` | `contracts/solidity/out/*.json` | `make compile-solidity` |
| `solana` | none | `contracts/solana/target/deploy/*.so` | `make compile-solana` |
| `tron` | `alloy` | `contracts/tron/build/*.json` | `make compile-tron` |
| `cw-artifacts` (implies `cw`) | | `contracts/cosmwasm/counter/artifacts/counter.wasm` | Docker optimizer |

Module tree: `cross_vm_common::mocks::{counter,ping_pong,vault}::{cw,evm,svm,tron}`,
each per-VM submodule gated behind its feature. Content is migrated verbatim from the
existing duplicated sites. `sol!` paths resolve relative to `CARGO_MANIFEST_DIR`
(`examples/common`), so they read `../../contracts/solidity/out/...`; `include_bytes!`
sites use explicit `concat!(env!("CARGO_MANIFEST_DIR"), ...)`.

Naming gotcha: the crate defines modules `counter` / `vault` / `ping_pong` and also
depends on extern crates of the same name. Under uniform path resolution a bare
`use counter::...` is ambiguous, so the `cw` submodules use leading-colon paths
(`use ::counter::ExecuteMsg`).

## Build-order contract

`cross-vm-common` built with a VM feature compiles only if that VM's artifacts exist;
a missing artifact is a compile-time error naming the path. Consequences:

* `cargo test --workspace` requires `make compile` (feature unification pulls all VMs,
  same as today).
* `cargo test -p evm-tests` requires only `make compile-solidity`. Per-VM artifact
  isolation holds for `-p` invocations. Dependency isolation does not (the framework
  pulls all VM crates by default), which is acceptable.

## Risks

1. `#[config_runner]` reads its TOML at macro-expansion time; the config must exist
   before its `tests/config_runner.rs` compiles. Editing `cases` requires a rebuild
   (the runtime bridge panics on drift by design).
2. `include_bytes!` / `sol!` path breakage surfaces only when the feature compiles.
   The per-feature `cargo check -p cross-vm-common --features ...` loop is the guard.
3. Untracked build outputs (`out/`, `target/`, `build/`, `generated/`) do not move with
   `git mv`. Move `counter.wasm` (Docker built, expensive) manually; rebuild the rest.
4. `contracts/tron/scripts/prepare-contracts.sh` copies Solidity sources; its source
   path must be updated to `../solidity/src`.
5. `.github/workflows/ci.yml` hardcodes contract paths in four jobs; must change in the
   move commit or CI goes red.
6. Multiple bins named `cross-vm` make bare `cargo run --bin cross-vm` ambiguous; docs
   must always use `-p`. `CARGO_BIN_EXE_cross-vm` remains per-crate and unaffected.
7. Workspace lints `missing_docs = "warn"` with CI `clippy -D warnings`; every new item
   needs rustdoc.
