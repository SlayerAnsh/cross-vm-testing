# Implementation plan: repo restructure

Date: 2026-07-03
Spec: [2026-07-03-repo-restructure-contracts-mocks](../specs/2026-07-03-repo-restructure-contracts-mocks.md)

Each phase leaves the repo green (`cargo test --workspace` passing) and is one commit.

## Phase 0: baseline

Confirm artifacts present and suite green before touching anything:
`make compile && cargo test --workspace`.

## Phase 1: move contracts, fix all paths (atomic)

1. `git mv examples/{cosmwasm,solidity,solana,tron}-contracts contracts/{cosmwasm,solidity,solana,tron}`.
2. Move untracked outputs manually (git does not move them): preserve
   `counter/artifacts/counter.wasm` (Docker built); rebuild the rest via `make compile`.
3. Workspace `Cargo.toml`: `exclude = ["contracts", "examples"]`. Explicit `members`
   entries override the exclude (existing `examples/integration-tests` precedent).
4. Update every reference. Grep patterns:
   `cosmwasm-contracts|solidity-contracts|solana-contracts|tron-contracts`.
   * `crates/cosmwasm/Cargo.toml` dev-dep paths.
   * `crates/cosmwasm/tests/onchain.rs` wasm embed path.
   * `crates/solidity/tests/{counter,onchain}.rs`, `crates/tron/tests/onchain.rs`
     `sol!` paths (become `../../contracts/solidity/out/...`).
   * `examples/integration-tests/Cargo.toml` dep paths and all `sol!` / `include_bytes!`
     sites (repoint only; mocks consumption is Phase 3).
   * `examples/scripts/Cargo.toml` and `deploy_counter/artifacts.rs` (repoint).
   * Root `Makefile`: `-C contracts/...`, `setup-*`, `fmt` forge path.
   * `contracts/tron/scripts/prepare-contracts.sh`: `src_dir="$tron_dir/../solidity/src"`.
   * `.gitignore`: rewrite the `examples/*-contracts` lines to `contracts/...`.
   * `.github/workflows/ci.yml`: `solana-artifacts`, `solidity-artifacts`,
     `tron-artifacts`, `test` jobs (working-dirs, verify/upload/download paths,
     `cache-dependency-path: contracts/tron/pnpm-lock.yaml`). `live-smoke.yml` is clean.
5. Verify: `make compile`, `cargo test --workspace`, `(cd examples/scripts && cargo check)`.

## Phase 2: create `examples/common` (`cross-vm-common`) with mocks

New crate holding the `mocks` module only (wallets arrive in Phase 4). Feature-gated
per the spec table. Add to workspace members.

Module files: `src/lib.rs`, `src/mocks/mod.rs`, and per contract
`src/mocks/{counter,ping_pong,vault}/{mod,cw,evm,svm,tron}.rs`. Migrate binding content
verbatim from the existing duplicated sites; rename `VDISC_*` to `DISC_DEPOSIT` etc.
(namespaced by module now). Use leading-colon paths in the `cw` submodules.

Verify per feature (this is the artifact-path smoke test):
`cargo check -p cross-vm-common` then `--features cw`, `evm`, `solana`, `tron`,
`cw,evm,solana,tron`, and `cw-artifacts` where the wasm exists locally. Then
`cargo doc -p cross-vm-common --all-features` for the `missing_docs` lint.

## Phase 3: integration-tests onto common mocks, then rename

3a. Delete the duplicated `sol!` / `DISC_*` / `include_bytes!` / `cw` factory blocks in
`src/support/vault.rs`, `tests/support/{counter,ping_pong}.rs`; import from
`cross_vm_common::mocks::...`. Drop the direct contract path-deps; add
`cross-vm-common` with all four VM features. Wrappers, harnesses, TOMLs, bin, and tests
are otherwise untouched. Verify `make test-examples test-harness test-cross-vm`.

3b. `git mv examples/integration-tests examples/cross-vm-tests`; package
`cross-vm-tests`, lib `cross_vm_tests`. Update `use cross_vm_integration_tests::` sites
(bin, `tests/harness/{vault,config_runner}.rs`, `tests/support/mod.rs`), workspace
members, Makefile `-p` flags. `#[config_runner]` and `CARGO_BIN_EXE` are
manifest-relative, so they survive the rename.

## Phase 4: move wallets/init_tracing into `cross-vm-common`

Move `wallets.rs` and `init_tracing` out of cross-vm-tests into `cross-vm-common`
(ungated). The cross-vm-tests `src/support/mod.rs` re-exports
(`pub use cross_vm_common::wallets::*;`) so no call-site churn. Verify
`cargo test -p cross-vm-tests`.

## Phase 5: per-VM test crates, one commit each

Order (fastest artifact loop first): `evm-tests`, `cosmos-tests`, `solana-tests`,
`tvm-tests`. Each crate:

* `publish = false`; features `fuzz` / `invariant` / `endurance`.
* Deps: `cross-vm-framework` (with `cli`), its VM crate, `cross-vm-common` (its mocks
  feature), `tokio`, `serde`. Dev-deps: `cross-vm-macros` (direct, for rust-analyzer),
  `rstest`, `serde_json`.
* `src/counter.rs`: single-VM Counter wrapper using
  `cross_vm_common::mocks::counter::<vm>` types; `CounterOp` / `CounterOpKind` /
  `CounterInvariant` (serde derived); `CounterHarness: Harness`; `counter_setup` (one
  mock chain, funded via `cross_vm_common::wallets`); `counter_config_setup` honoring
  `chain_specs` via `build_chain` (same shape as today's `src/vault.rs`, minus the
  multi-chain loop).
* `counter.cross-vm.toml` at crate root: one `[[chain]]`, `[profile.smoke]` (fuzz,
  `cases = 4`), `[profile.steps]` (scenario).
* `src/bin/cross_vm.rs`: `#[tokio::main(flavor = "current_thread")]` registering the
  harness on `Cli`.
* `tests/harness.rs`: always-run scenario/rstest test plus feature-gated
  `#[fuzz_runner]` / `#[invariant_runner]` / `#[endurance_runner]` blocks.
* `tests/config_runner.rs`: `#[config_runner(...)]` gated behind `fuzz`.
* `tests/cli_e2e.rs`: subprocess via `env!("CARGO_BIN_EXE_cross-vm")`: validate, list,
  run twice with the same seed (reproducibility), exit codes, `--json-report`.

Per-VM specifics: cosmos-tests uses mocks `cw` (`cw::contract()` + `*MsgFns`, no
artifacts); solana-tests uses mocks `solana` plus `solana-instruction` /
`solana-system-interface`; tvm-tests uses mocks `tron` (`Bytes` re-exported by
`cross-vm-tron`, no solidity crate dep). Add a Makefile `test-examples-<vm>` target per
crate plus an aggregate `test-examples-all`.

Verify per crate: `cargo test -p <crate>`, `--features "fuzz invariant endurance"`,
`cargo run -p <crate> --bin cross-vm -- run <dir>/counter.cross-vm.toml --profile smoke`.

## Phase 6: `examples/scripts` onto common mocks

Delete `deploy_counter/artifacts.rs`; depend on `cross-vm-common` with features
`["evm", "cw-artifacts"]` (`COUNTER_WASM` plus EVM counter bindings). Verify
`(cd examples/scripts && cargo build)` without regenerating the pinned lockfile.

## Phase 7: docs and final verification

Update README (layout, commands), DEVELOPER (layout tree, the mocks feature/artifact
table, the feature-unification note, a per-VM crate walkthrough), SPEC and CONTRIBUTING
path fixes, `docs/config-runs-spec.md` path references, `examples/PING_PONG.md`, and add
one CHANGELOG entry (do not rewrite history). Per repo docs convention, do not use
dashes as punctuation.

Final gate:

```sh
make compile
for f in "" cw evm solana tron "cw,evm,solana,tron"; do
  cargo check -p cross-vm-common ${f:+--features "$f"}
done
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
make test-harness-all
cargo test -p evm-tests --features "fuzz invariant endurance"   # repeat per VM crate
cargo run -p cross-vm-tests --bin cross-vm -- run examples/cross-vm-tests/vault.cross-vm.toml --profile smoke
cargo run -p evm-tests --bin cross-vm -- run examples/evm-tests/counter.cross-vm.toml --profile smoke
(cd examples/scripts && cargo build)
cargo clean && make compile-solidity && cargo test -p evm-tests   # per-VM isolation
```
