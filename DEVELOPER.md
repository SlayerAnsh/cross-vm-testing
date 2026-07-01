# Developer guide

## Prerequisites

* Rust stable, edition 2021. The MSRV is declared as `rust-version` in the workspace `Cargo.toml` (the manifest is the source of truth; CI enforces it with a dedicated `msrv` job).
* Building the integration-test contract artifacts (all git-ignored): Foundry (`forge`) for the EVM Counter, `cargo-build-sbf` for the Solana program, and the CosmWasm wasm builds. Run `make compile` (or a single ecosystem target) before `cargo test -p cross-vm-integration-tests`. The pure unit tests in each VM crate need none of this.

## Continuous integration

`.github/workflows/ci.yml` runs on push to `main`, on pull requests, and on manual dispatch. It is eight jobs. Seven run in parallel: `lint` (`cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo doc --workspace --no-deps`), `msrv` (`cargo check --workspace` on the declared `rust-version`), `deny` (`cargo deny check` against `deny.toml`: advisories, licenses, duplicate versions, sources), `features` (`cargo hack --feature-powerset` over the framework's per-VM feature gates), `solana-artifacts` (installs the Agave platform tools, runs `make compile-solana`, uploads the three `.so`), `solidity-artifacts` (installs Foundry, runs `forge fmt --check` and `make compile-solidity`, uploads the `out/` directory), and `tron-artifacts` (Node + pnpm + tronbox, runs `make compile-tron`, uploads `build/`). The eighth job, `test`, declares `needs: [solana-artifacts, solidity-artifacts, tron-artifacts]`, downloads all three artifact sets into their exact embed paths (`examples/solana-contracts/target/deploy` for the `.so` consumed by `include_bytes!`, `examples/solidity-contracts/out` and `examples/tron-contracts/build` for the `.json` consumed by `sol!`), verifies they landed, runs `cargo clippy --all-targets` (the artifact-dependent lint pass the `lint` job deliberately skips), then runs the tests in two steps: `cargo test --workspace --exclude cross-vm-cosmwasm`, then `cargo test -p cross-vm-cosmwasm --lib --test counter --test vault --test typed_handle`. A separate `live-smoke.yml` workflow (weekly cron + manual dispatch, secrets-gated, never a required check) runs the `#[ignore]`d live RPC tests.

The contract builds parallelize cheaply because the root workspace excludes `examples/` and `examples/solana-contracts` is its own Cargo workspace, so `cargo build-sbf` compiles only the three SBF programs, not the main target. The earlier layout was a single sequential job (a six-job version before that was reverted because every job reinstalled toolchains and recompiled the full workspace with no cache). To keep the parallel split from repeating that cost, `lint` and `test` share one `Swatinem/rust-cache` key (`cross-vm-main`) so the dependency graph compiles once and restores in both, the Agave install is cached on `~/.local/share/solana` keyed to the pinned version (the `SOLANA_VERSION` workflow env, one knob for the install URL and both caches), and `lint` drops the redundant `cargo build --workspace` (the `test` job compiles the workspace anyway, and clippy already surfaces build errors). `forge fmt --check` now lives in `solidity-artifacts`, so a formatting nit gates `test` but not `lint`.

The Solana artifacts are built with `cargo build-sbf` (from the Anza platform tools), not `anchor build`. The tests use raw instruction discriminators, so the Anchor IDL is unnecessary, and `cargo build-sbf` avoids Anchor's anchor-cli/anchor-lang version handshake (the prebuilt npm `anchor-cli@0.31.1` reports `0.31.0`, so `anchor build` aborted and silently produced no `.so`). The toolchain runs with `rustflags: ""` so the action's default `-D warnings` does not turn the third-party SBF program build's deprecation warnings into errors; `-D warnings` is still enforced explicitly on clippy. `make compile-solana` is followed by a check that the `.so` exist, so a no-op build fails loudly rather than at the later `include_bytes!`.

`cross-vm-cosmwasm` is split out because `tests/onchain.rs` embeds a compiled `counter.wasm` that only the Docker CosmWasm optimizer produces (not available in CI); it is a live `#[ignore]` test, and the cross-VM cosmwasm path is still exercised through `cw-multi-test`'s in-process `ContractWrapper`, which needs no wasm.

## Async

The `ChainProvider` trait and the `MultiChainEnv` are `async` (native `async fn` in traits,
no `async-trait` crate). The mock backends are not `Send`, so the futures are not `Send`;
run them on a current-thread Tokio runtime. Tests use `#[tokio::test]` (current-thread by
default) and examples use `#[tokio::main(flavor = "current_thread")]`. `tokio` is a
dev-dependency only (the library crates define `async fn`s but pull in no runtime).

## Workspace

This is a Cargo workspace with one crate per VM plus a shared core crate. Dependency trees are isolated per crate, so building or testing one VM does not pull the others.

```
crates/core       cross-vm-core       no VM dependencies
crates/cosmwasm   cross-vm-cosmwasm   cw-multi-test, cosmwasm-std
crates/solidity   cross-vm-solidity   revm (alloy for the test bindings)
crates/solana     cross-vm-solana     litesvm, granular solana-* crates
crates/tron       cross-vm-tron       revm core + TVM layers (Tron precompiles, energy/bandwidth shim, sun balances)
crates/macros     cross-vm-macros     proc-macros (syn/quote): cross_vm_contract, CwExecuteFns/CwQueryFns, define_wallet_roster, fuzz/invariant/endurance runners
crates/framework  cross-vm-framework  umbrella over core + all three VM crates
```

`cross-vm-framework` defines `MultiChainEnv`. Each VM crate also exposes a backend enum
(`CwChain`/`EvmChain`/`SvmChain`, either `Mock` or `Rpc`) that implements `ChainProvider`
for chain-level operations (accounts, balances, blocks) and delegates idiomatic contract/program
methods to the inner mock/RPC provider, plus an asset selector (`CwAsset`/`EvmAsset`/`SvmAsset`/`TronAsset`)
and an inherent `ensure_asset` used by the environment's funding phase.

## Tron crate layout

`cross-vm-tron` (`crates/tron`) is the fourth VM crate. The TVM is an EVM derivative, so the mock reuses a `revm` core (the same `revm` 41 / `alloy-primitives` pins as `cross-vm-solidity`) and layers the Tron-specific behavior on top. Modules:

```
crates/tron/
  chain.rs              TronChain backend enum (Mock | Rpc), ChainProvider impl
  wallet.rs             secp256k1 WalletDeriver, SLIP-44 coin type 195
  asset.rs              TronAsset selector + ensure_asset (funding phase)
  error.rs              TronError, converts into CrossVmError
  provider/
    address.rs          base58check TronAddress (0x41 prefix; inner 20 bytes = EVM address)
    mock.rs             TronMockProvider over the revm core
    rpc.rs              TronRpcProvider (live java-tron over TronGrid HTTP: reads, static_call, signed deploy/call)
    execution.rs        execution result / response plumbing
  tvm/
    create.rs           pure tron_create_address / tron_create2_address (tx-id formula, for tooling)
    precompiles.rs      Tron precompiles injected into revm (TIP-272 relocations + validatemultisign)
    resources.rs        energy / bandwidth accounting shim and u64 sun balances
  chains/
    info.rs             TronChainInfo (ChainSpec impl)
    presets.rs          MAINNET / NILE / SHASTA / LOCAL
    sugar.rs            .mock(wallets) / .rpc(wallets) constructors
```

Build and test. The RPC backend drives a live java-tron node over the TronGrid HTTP REST API (reads, `static_call`, and signed `deploy_create` / `call`). The standalone `cargo test -p cross-vm-tron` unit tests still need no network and no prebuilt contract artifacts (unlike the integration tests): they cover address derivation, the mock's account/balance/block surface, the injected precompiles, the resource shim, and the RPC error/offline paths. The live RPC reads and writes are exercised by the gated `tests/onchain.rs` (run with `--ignored`): one test reads the Nile testnet block height over real HTTP, and the write test deploys the Counter and runs increment/count against Nile (it needs a funded `MNEMONIC_TEST` wallet at coin type 195). Two divergences are intentional and tested as such: the mock's `CREATE` / `CREATE2` follow revm's EVM address derivation (the Tron tx-id formula lives in the pure `tron_create_address` / `tron_create2_address` reference functions in `tvm/create.rs`), and the energy/bandwidth shim is coarse account-level accounting rather than per-opcode energy costs.

## Common commands

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets

# one crate at a time
cargo test -p cross-vm-cosmwasm
cargo test -p cross-vm-solidity
cargo test -p cross-vm-solana
cargo test -p cross-vm-tron      # standalone, no network or prebuilt artifacts needed

# examples
cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart
cargo run -p cross-vm-solidity --example evm_quickstart
cargo run -p cross-vm-solana   --example solana_quickstart
```

## Tests

Each crate has unit tests (chain metadata, account creation, balance set/get, block advance, RPC write paths return an error) in `src/tests.rs`, and an integration test in `tests/`. The CosmWasm, EVM, and Solana crates also have `tests/rpc.rs`, `#[ignore]` live read tests against Osmosis testnet, Ethereum Sepolia, and Solana Devnet (for example `cargo test -p cross-vm-solana --test rpc -- --ignored`); Tron's equivalent is `tests/onchain.rs` (Nile testnet reads plus a funded deploy/call write test):

* `cross-vm-cosmwasm`: `store_code` + `instantiate` an in-test `ContractWrapper` counter, then `execute_contract`, `query_wasm_smart`.
* `cross-vm-solidity`: `deploy_create` the Solidity `Counter` from `examples/solidity-contracts` (creation bytecode from the forge artifact via `sol!`), then `call` for `increment`/`reset`, read via `static_call`.
* `cross-vm-solana`: airdrop, System Program transfer through `send_transaction`, balance assertion.
* `cross-vm-framework`: keeps only framework functionality tests. `src/tests.rs` covers `MultiChainEnv` setup, label/VM error handling, native funding, and the before/after hook mechanics; inline `#[cfg(test)]` mods in `contract/account.rs`, `contract/response.rs`, `harness/rng.rs`, and `harness/outcome.rs` cover their units. The heavy multi-chain integration tests live in their own crate (see below), so the framework build no longer drags the contract-artifact toolchain.
* `cross-vm-integration-tests` (`examples/integration-tests`): the multi-chain integration and example tests, co-located with the contract artifacts they embed. Two test binaries. `tests/harness/` holds the property-testing examples (`counter.rs`, `vault.rs`, `ping_pong.rs`, and `mechanics.rs` for the runner mechanics); `tests/cross_vm/` holds the multi-chain tests (`setup.rs`, `counter.rs`, `wallet.rs`, `ping_pong.rs`). Both share `tests/support/`, split by concern into `counter.rs` (the cross-VM `Counter` wrapper), `vault.rs` (the cross-VM `Vault` wrapper), `ping_pong.rs` and `bridge.rs` (the ping-pong relayer flow), and `wallets.rs` (`test_wallets` plus funding helpers), aggregated by `tests/support/mod.rs`. Each group has a `main.rs` that declares its modules (Cargo treats `tests/<group>/main.rs` as one test target). `tests/cross_vm/counter.rs` runs one rstest over all four VMs (`#[values(ChainKind::CosmWasm, ChainKind::Evm, ChainKind::Svm, ChainKind::Tron)]`, each case building the matching `.mock(wallets)`) driving the single `Counter` wrapper. All four VMs use the canonical contracts from `examples/`. The EVM, Solana, and Tron cases read build artifacts at compile time, all git-ignored, so run `make compile` (or the per-ecosystem `compile-solidity` / `compile-solana` / `compile-tron` targets) before `cargo test -p cross-vm-integration-tests`. A fresh checkout will not compile the tests until they exist:
  * CosmWasm: the `examples/cosmwasm-contracts/counter` crate is consumed as an rlib (no artifact build needed).
  * EVM: `sol!` parses `examples/solidity-contracts/out/Counter.sol/Counter.json` (forge build) for the ABI and creation bytecode.
  * Solana: `include_bytes!` loads `examples/solana-contracts/target/deploy/counter.so` (`cargo-build-sbf`).

## Writing a cross-VM contract wrapper

To drive one contract across CosmWasm, EVM, Solana, and Tron from a single test, declare its logical methods in a spec trait, apply `#[cross_vm_contract(StructName)]`, and write the per-VM hooks. The macro (from `cross-vm-macros`, re-exported in the prelude) generates the wrapper struct, its `new` / `instance` constructors, the `on_before` / `on_after` forwarders, and a dispatcher per method that matches the VM and calls the matching `cw_*` / `evm_*` / `svm_*` / `tron_*` hook; a hook you do not write defaults to `unimplemented!`, panicking only if that VM is actually dispatched, so a wrapper can target a subset of VMs. See `examples/integration-tests/tests/support/counter.rs` for a full example. The shape:

```rust
// Declare logical methods once. Return types are the bare Ok type (the macro wraps each in
// Result<_, CrossVmError>). A method returning AppResponse<_> fires before/after hooks; a
// query like `count -> u64` is a plain dispatch.
#[cross_vm_contract(Counter)]
pub trait CounterSpec {
    async fn setup(&self, wallet: &str);
    async fn increment(&self, wallet: &str) -> AppResponse<()>;
    async fn count(&self) -> u64;
}

// Write only the per-VM hooks. The macro generated `struct Counter { base: ContractBase }`
// and the dispatchers that call these.
impl Counter {
    async fn cw_increment(&self, wallet: &str) -> Result<AppResponse<()>, CrossVmError> {
        let chain = self.base.cosmwasm()?;            // typed handle, WrongVm on mismatch
        let addr  = self.base.cw_addr()?;             // typed deployed address
        let raw = chain.execute_contract(&addr, ExecuteMsg::Increment {}, wallet, &[]).await?;
        Ok(AppResponse::cosmwasm((), raw))            // typed payload + raw per-VM result
    }
    // evm_increment / svm_increment own their native encoding the same way
}
```

Because the dispatchers are methods of the spec trait, a call site brings it into scope: `use ...::CounterSpec;` (the inherent `new` / `instance` / `on_*` need no import). Each VM hook a method dispatches to (`cw_increment`, `evm_increment`, `svm_increment`) must exist with the same signature; a missing one surfaces as an ordinary `no method named cw_...` error.

Guidelines:

* The contract owns its chain handle (a cheap `AnyChain` clone). Methods are `&self`; `setup` records the deployed address through `ContractBase::set_address` (interior mutable).
* Recover native types inside a hook: `self.base.cosmwasm()? / evm()? / solana()?` for the chain, `signer.cw()? / evm()? / svm()?` for the account, `self.base.cw_addr()? / evm_addr()? / svm_addr()?` for the deployed address.
* Provider errors convert into `CrossVmError` through `?` (each VM error implements `From` for it).
* A VM you do not support: return `CrossVmError::unimplemented(kind, "...")` from that arm.
* Wrap a return value as `AppResponse<T>`; the caller reads `.value()` and reaches the raw result via `raw_cosmwasm` / `raw_evm` / `raw_solana` (or `transaction_hash` / `gas_used`), and the emitted events via `raw_cosmwasm_events` / `raw_evm_logs` / `raw_solana_logs`.
* The EVM `call` returns an `EvmExecution { output, logs }` (it no longer discards the logs revm produces); build the response with `AppResponse::evm((), exec.output, exec.logs)`.
* CosmWasm hooks can skip the hand-built `ExecuteMsg` / `query_wasm_smart`: derive `CwExecuteFns` / `CwQueryFns` (from `cross-vm-macros`) on the contract's `ExecuteMsg` / `QueryMsg` (under a `cross-vm` feature so the wasm build stays clean), then call `self.base.cosmwasm()?.contract(addr).increment(wallet)` / `.get_count()`. Query variants need `#[returns(T)]`; a variant marked `#[payable]` adds a trailing `funds: &[Coin]` arg. EVM already gets typed calls from `alloy::sol!`; Solana has no schema so its hooks stay hand-written.

## Transaction hooks

A wrapper can run side-logic (an indexer, a bridge relay, an event listener) before and after each transaction. Hooks live on `ContractBase`. A developer registers them with `on_before` / `on_after`; the method dispatcher fires them with `run_before` / `run_after` around the per-VM execution. An after-hook receives the uniform `AppResponse` (read-only), so it can react to the result independent of the VM.

```rust
pub async fn increment(&self, signer: &Account) -> Result<AppResponse<()>, CrossVmError> {
    self.base.run_before("increment")?;                  // fire before-hooks, abort on Err
    let resp = match self.base.kind() {
        ChainKind::CosmWasm => self.cw_increment(signer).await?,
        ChainKind::Evm      => self.evm_increment(signer).await?,
        ChainKind::Svm      => self.svm_increment(signer).await?,
    };
    self.base.run_after("increment", resp)               // fire after-hooks, return the response
}

// registration, anywhere the wrapper exposes its base:
counter.on_after(|ctx| {
    println!("{} on {:?} -> {:?}", ctx.label(), ctx.kind(), ctx.transaction_hash());
    Ok(())
});
```

Properties:

* Synchronous `FnMut`. The mock backends are synchronous and the runtime is current-thread (futures are not `Send`), so async side-effects flow through a channel or an `Rc<RefCell<_>>` buffer the closure captures and drains later.
* Both kinds return `Result<(), CrossVmError>`. The first `Err` aborts: a before-`Err` stops the transaction from running; an after-`Err` becomes the method's error.
* Registered per contract, fired in registration order. A before-hook reads `label` / `kind`; an after-hook also reads `transaction_hash` / `gas_used` off the response, plus per-VM event data.
* Events are exposed per VM because the shapes do not unify: `cosmwasm_events()` returns typed `Event` attributes, `evm_logs()` returns ABI `Log`s (address, topics, data), `solana_logs()` returns the program log lines (`msg!` / `sol_log`; Anchor `emit!` events are base64 inside them), and `tron_logs()` returns Tron's EVM-shaped logs. The matching accessor succeeds; the others return `WrongVm`. An after-hook that watches every VM matches on `ctx.kind()`.
* Re-entrancy is unsupported: a hook that re-enters the same contract's `run_before` / `run_after` panics on the `RefCell` (the registry is borrowed while firing).
* The two dispatcher lines are the shape the deferred scaffolding macro will generate.

## Adding a predefined chain

Add a constant to the relevant crate's `chains/presets.rs`. The metadata struct (`CosmosChainInfo`, `EvmChainInfo`, `SolanaChainInfo`, or `TronChainInfo`) lives in `chains/info.rs`; the `ChainSpec` impl and the `.mock()` / `.rpc()` sugar (`chains/sugar.rs`) apply automatically.

## Adding a new VM

The short version:

1. Add a crate under `crates/` and list it in the workspace `members`.
2. Define a `ChainInfo` struct implementing `cross_vm_core::ChainSpec`, with a `chains` module of constants.
3. Implement `cross_vm_core::ChainProvider` for a mock provider and an RPC provider (chain-level ops only). The mock is what the default suite runs; the RPC backend can start read-only and grow writes later (the EVM, CosmWasm, and Tron providers all sign and broadcast today).
4. Add idiomatic contract/program methods on the mock and RPC providers (and delegate from the chain enum).
5. Convert the provider error into `CrossVmError` via `From`.
6. Mirror the unit and integration test layout above.

The full file-by-file checklist, derived from the Tron addition (the most recent complete example), lives in [`docs/adding-a-vm.md`](docs/adding-a-vm.md). The framework touch points (the `AnyChain` / `Account` / `RawResponse` / funding enums, the macro hook, the prelude, CI) are enum dispatch by design, so a new VM edits a known, bounded set of files; the checklist names each one and which existing VM to crib from.

## Notes on crate versions

These ecosystems change their public APIs frequently. Pin exact majors and follow the docs of the pinned version:

* `cw-multi-test` 2.x with matching `cosmwasm-std` 2.x. Each `cw-multi-test` major hard-pins one `cosmwasm-std` major (its `cosmwasm_2_0` ... `cosmwasm_3_0` features only toggle API tiers within that single major, they do not swap the major), so the framework targets one cosmwasm major per build. The example `counter` contract crate pins the same `cosmwasm-std` 2.x so it can be wrapped in-process.
* `revm` 41.x. The builder API (`Context::mainnet().with_db(..).build_mainnet()`, then `transact` / `transact_commit`) has changed repeatedly across majors. Ethereum value types (`Address`, `U256`, `Bytes`, `keccak256`) come from `alloy-primitives`, which is the crate `revm` itself re-exports. As long as a single `alloy-primitives` major resolves for both, the types are identical at the revm boundary (currently `alloy-primitives` 1.x). If a future `revm` bump pins a different `alloy-primitives` major than the workspace, realign the workspace pin so they unify again.
* `litesvm` 0.13 with the granular `solana-*` v4 crates it re-exports. Solana split `solana-sdk` into modular crates; `Pubkey` is an alias of `solana_address::Address`. Depending on the same granular crates litesvm uses keeps types identical at the boundary.

## Regenerating the EVM counter bytecode

The EVM creation bytecode is not pasted into a constant. `alloy::sol!` reads the forge artifact JSON at compile time and generates both the call bindings and a `Counter::BYTECODE` constant from it (see `crates/solidity/tests/counter.rs` and `examples/integration-tests/tests/support/counter.rs`). To refresh it, rebuild the artifact:

```
make compile-solidity     # runs `forge build` in examples/solidity-contracts
# -> regenerates examples/solidity-contracts/out/Counter.sol/Counter.json, which sol! consumes
```
