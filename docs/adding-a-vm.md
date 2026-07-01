# Adding a new VM ecosystem

A file-by-file checklist for wiring a fifth VM into the framework, derived from the Tron addition (commit `43b566e`, the most recent complete example). The framework dispatches over per-VM enums by design (`ChainProvider` is not object safe), so a new VM edits a known, bounded set of files rather than implementing one trait in one place. Every row names the file, what to add, and which existing VM to crib from. Tron is usually the best reference (newest, and its design doc lives in `docs/superpowers/specs/`).

Conventions in this checklist: `<vm>` is the short lowercase name used in feature flags and hooks (like `tron`), `<Vm>` the type prefix (like `Tron`).

## 1. The provider crate (`crates/<vm>/`)

Self-contained; get this fully green (`cargo test -p cross-vm-<vm>`, clippy clean) before touching the framework.

| File | What goes there | Crib from |
| --- | --- | --- |
| `Cargo.toml` | Inherit `version/edition/rust-version/license/repository` from the workspace; depend on `cross-vm-core` | `crates/tron/Cargo.toml` |
| `src/lib.rs` | Public exports: chain enum, providers, asset, error, presets | `crates/tron/src/lib.rs` |
| `src/chain.rs` | `<Vm>Chain { Mock(..) \| Rpc(..) }` enum, `ChainProvider` impl, inherent contract ops (`deploy_create`, `call`, `static_call`, `ensure_asset`) delegating to the active backend | `crates/tron/src/chain.rs` |
| `src/wallet.rs` | `WalletDeriver` for the VM's curve and SLIP-44 coin type | `crates/tron/src/wallet.rs` (secp256k1, 195) |
| `src/asset.rs` | `<Vm>Asset` selector plus `ensure_asset` semantics for the funding phase | `crates/tron/src/asset.rs` |
| `src/error.rs` | `<Vm>Error` with `From<<Vm>Error> for CrossVmError` | `crates/tron/src/error.rs` |
| `src/provider/mock.rs` | The in-process mock backend (what the default suite runs) | `crates/tron/src/provider/mock.rs` |
| `src/provider/rpc.rs` | The live backend; read paths first, writes can land later (EVM, CosmWasm, and Tron all sign and broadcast today) | `crates/tron/src/provider/rpc.rs` |
| `src/chains/{info,presets,sugar}.rs` | `<Vm>ChainInfo` (`ChainSpec` impl), chain preset constants, `.mock(wallets)` / `.rpc(wallets)` constructors | `crates/tron/src/chains/` |
| `src/tests.rs` + `tests/` | Unit tests (metadata, accounts, balances, blocks, RPC error paths) plus an `#[ignore]`d live test (`tests/rpc.rs` or `tests/onchain.rs`) | `crates/tron/tests/onchain.rs` |
| VM-specific modules | Anything the VM needs beyond the shared shape (Tron: `provider/address.rs` base58check, `tvm/` precompiles, opcodes, resources) | `crates/tron/src/tvm/` |

Also add the crate to the root `Cargo.toml` `members` list and put any new shared dependency versions in `[workspace.dependencies]`.

## 2. Core (`crates/core/`)

| File | What goes there |
| --- | --- |
| `src/chain_kind.rs` | The new `ChainKind::<Vm>` variant with a doc line describing both backends |

Wallet sources usually need nothing new (the roster is VM-agnostic; the deriver lives in your crate).

## 3. Framework (`crates/framework/`)

Each of these is a four-arm enum or match gaining a fifth arm, gated by a new cargo feature. Grep for `ChainKind::Tron` to find every dispatch site; the compiler's non-exhaustive-match errors then walk you through the rest.

| File | What goes there |
| --- | --- |
| `Cargo.toml` | `cross-vm-<vm> = { path = "../<vm>", optional = true }`, a `<vm>` feature (`dep:` plus adding it to `default`) |
| `src/any_chain.rs` | `AnyChain::<Vm>` variant, arms in every forwarding method, a `From<<Vm>Chain>` impl (the `into_any!` macro) |
| `src/contract/account.rs` | `Account::<Vm>` variant, `.<vm>()` typed extractor, `From<<Vm>Address>` |
| `src/contract/base.rs` | Typed chain accessor and address getter (`<vm>()` / `<vm>_addr()`) |
| `src/contract/response.rs` | `RawResponse::<Vm>` variant, `AppResponse::<vm>` constructor, per-VM accessors (logs, gas, tx hash as the backend provides them) |
| `src/contract/hooks.rs` | `HookContext::<vm>_logs()` (or the VM's event shape) |
| `src/env/multi_chain_env.rs` | The typed `<vm>(label)` borrow |
| `src/fund/{fund_target,pending}.rs` | `FundTarget` impl for the VM address type, `Pending::<Vm>` variant applied at `start()` |
| `src/error.rs` | Only if the VM introduces a new env-level failure shape |
| `src/prelude.rs` | Feature-gated re-exports: chain presets, chain/asset/provider types |
| `src/lib.rs` | Feature-gated `pub use` of the provider crate; extend the `compile_error!` VM-feature list |
| `tests/<vm>_e2e.rs` | An artifact-free end-to-end test proving macro dispatch, `new_account`, funding, and block advance on the mock (see `tests/tron_e2e.rs`) |

## 4. Macros (`crates/macros/`)

| File | What goes there |
| --- | --- |
| `src/lib.rs` | The `<vm>_{method}` hook name, its dispatch arm in the generated match, and the default-`unimplemented!` entry in the generated hooks trait |

A contract that never targets the new VM keeps compiling unchanged: the generated hook defaults to `unimplemented!` and only panics if that VM is actually dispatched.

## 5. Example contracts and integration tests (`examples/`)

| File | What goes there |
| --- | --- |
| `examples/<vm>-contracts/` | The VM's build toolchain for the shared example contracts (Tron reuses the Solidity sources and compiles them with tronbox; a genuinely different VM needs its own Counter/Vault/PingPong) |
| `Makefile` | `compile-<vm>` and `setup-<vm>` targets, folded into `compile` |
| `examples/integration-tests/tests/support/{counter,vault,ping_pong}.rs` | The `<vm>_*` hooks on each wrapper (encode calls, decode responses) |
| `examples/integration-tests/tests/support/{wallets,bridge}.rs` | Funding helper arm, event-parsing arm |
| `examples/integration-tests/tests/cross_vm/counter.rs` | Add the new `ChainKind` to the rstest matrix |

## 6. CI and docs

| File | What goes there |
| --- | --- |
| `.github/workflows/ci.yml` | A `<vm>-artifacts` job (toolchain, compile, upload), the artifact download/verify steps in `test`, and `needs:` |
| `README.md` | Ecosystem row in the status matrix, quickstart mention |
| `DEVELOPER.md` | Crate table row, test inventory, this checklist's short version |
| `SPEC.md` | Backend semantics and honest v1 limits (what the mock diverges on, what the RPC backend leaves unimplemented) |
| `CHANGELOG.md` | An entry enumerating the blast radius, like the Tron entry |

## Order of work

The Tron addition landed in dependency order, each step compiling and tested before the next: design doc first, then the standalone crate (leaf modules, then providers, then the chain enum), then core + framework + macros in one step (the workspace is red in between, this is the one non-incremental hop), then example wrappers and artifacts, then CI. Budget the most time for the mock's semantic fidelity; it is what every default-suite test exercises.
