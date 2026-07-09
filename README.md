<div align="center">

# cross-vm-testing

**Write one test. Run it on CosmWasm, EVM, Solana, and Tron.**

A Rust testing suite for cross VM work. It puts four execution environments behind a single async trait, so the same test code drives an in process CosmWasm chain (`cw-multi-test`), an EVM (`revm`), Solana (`litesvm`), and Tron (a `revm` core with TVM-accurate layers) without changing shape.

![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)
![MSRV](https://img.shields.io/badge/MSRV-1.91-orange?logo=rust)
![Edition](https://img.shields.io/badge/edition-2021-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Version](https://img.shields.io/badge/version-0.1.0-lightgrey)
![VMs](https://img.shields.io/badge/VMs-CosmWasm%20%7C%20EVM%20%7C%20Solana%20%7C%20Tron-purple)

</div>

---

## Why

Cross VM work (bridges, relayers, multi chain protocols) usually means three separate test stacks with three different mental models. This suite collapses that. A chain provider here is the analogue of alloy's `Provider`, cw-orch's `CwEnv`, or test-tube's `Runner`: one shared trait, one in process VM per ecosystem behind it, identical call sites regardless of target chain.

On top of that base, three things make full cross VM tests practical:

* **One contract wrapper, three VMs.** Declare a contract's logical methods once and the `#[cross_vm_contract]` macro generates a struct that dispatches each call to the right VM. You write only the per VM glue.
* **A property testing harness.** Fuzz, invariant, endurance, and scenario runners drive any wrapper over many generated operation sequences, all seeded and reproducible.
* **Real wallets.** Mnemonics in a `.env`, a compile time roster, per ecosystem HD derivation, and a process-global broadcast lock keyed by `(chain, address)` so live nonces never collide, even across tests.

## Table of contents

* [Install](#install)
* [Quickstart](#quickstart)
* [MultiChainEnv: many chains, one simulation](#multichainenv-many-chains-one-simulation)
* [Cross VM contracts: one wrapper, four VMs](#cross-vm-contracts-one-wrapper-four-vms)
* [Two ways to write a test](#two-ways-to-write-a-test)
* [Property testing harness](#property-testing-harness)
* [The config-driven CLI](#the-config-driven-cli)
* [Wallets](#wallets)
* [Live RPC providers](#live-rpc-providers)
* [Tron (TVM)](#tron-tvm)
* [Macros at a glance](#macros-at-a-glance)
* [Workspace layout](#workspace-layout)
* [Build and test](#build-and-test)
* [Status](#status-supported--planned)

## Install

This is a Cargo workspace, not yet published to crates.io. Depend on the framework crate by path (it re-exports every VM crate and the prelude):

```toml
[dev-dependencies]
cross-vm-framework = { path = "crates/framework" }
tokio = { version = "1", features = ["macros", "rt"] }
```

Everything is `async` and the mock backends are not `Send`, so run on a current thread runtime: `#[tokio::test]` (current thread by default) or `#[tokio::main(flavor = "current_thread")]`. The library crates define `async fn`s but pull in no runtime; `tokio` is a dev dependency.

The minimum supported Rust version is declared as `rust-version` in the workspace `Cargo.toml` and enforced by CI; any stable toolchain at or above it works.

## Quickstart

```rust
use std::rc::Rc;
use cross_vm_core::{ChainProvider, ChainSpec, WalletFactory};
use cross_vm_cosmwasm::chains::OSMOSIS;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // One shared WalletFactory drives every chain. An empty roster needs no .env.
    let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
    let mut chain = OSMOSIS.mock(wallets);          // or CwMockProvider::new(OSMOSIS, wallets)
    let alice = chain.new_account("alice").await;   // created and funded
    assert!(chain.balance(&alice).await.unwrap() > 0);
}
```

The same shape works for `cross_vm_solidity::chains::ETHEREUM` and `cross_vm_solana::chains::SOLANA_DEVNET`. Runnable versions live in each crate's `examples/`:

```
cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart
cargo run -p cross-vm-solidity --example evm_quickstart
cargo run -p cross-vm-solana   --example solana_quickstart
```

Predefined chains include `OSMOSIS`, `JUNO`, `NEUTRON`, `COSMOS_HUB`, `CW_LOCAL` (CosmWasm); `ETHEREUM`, `ARBITRUM`, `OPTIMISM`, `BASE`, `POLYGON`, `EVM_LOCAL` (EVM); `SOLANA_MAINNET`, `SOLANA_DEVNET`, `SOLANA_TESTNET`, `SOLANA_LOCALNET` (Solana); and `TRON_MAINNET`, `TRON_NILE`, `TRON_SHASTA`, `TRON_LOCAL` (Tron). Each carries `.mock(wallets)` and `.rpc(wallets)` constructors (the RPC endpoint is part of the chain preset).

## MultiChainEnv: many chains, one simulation

`MultiChainEnv` models a chain simulation with two phases. During setup you inject chains and declare funding; `start()` applies the plan and enters the running phase, where only chain execution is allowed (funding and injection are gone at the type level).

```rust
use std::rc::Rc;
use cross_vm_framework::prelude::*;

let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
let mut env = MultiChainEnv::new("swap-test", wallets.clone());
env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets.clone())));
env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets.clone())));

let cw_alice  = env.cosmwasm("osmosis").unwrap().new_account("alice").await;
let evm_alice = env.evm("eth").unwrap().new_account("alice").await;
// Testing funds native balances only. The asset is a raw denom string (the bank denom
// on CosmWasm; ignored on EVM/Solana, which each have a single native coin).
env.fund("osmosis", &cw_alice, "uosmo", 1_000_000u128).unwrap();
env.fund("eth", &evm_alice, "eth", cross_vm_solidity::U256::from(5u64)).unwrap();

let mut env = env.start().await.unwrap();       // running phase
let bal = env.cosmwasm("osmosis").unwrap().balance(&cw_alice).await.unwrap();
// Accessing a label that was never injected returns Err(EnvError::UnknownChain):
assert!(env.solana("sol").is_err());
// env.fund(...);  // compile error: not available once running
```

**Funding semantics.** Native assets mock mint the shortfall. Token assets (cw20/erc20/SPL) are validated against the real on chain balance (you mint them by deploying the token during setup). On all three RPC providers, native funding validates rather than mints (a live chain cannot mint): it reads the real balance and reports a `Shortfall` if the account is underfunded. Token RPC funding paths return `Unimplemented`.

## Cross VM contracts: one wrapper, four VMs

The headline feature. To drive one contract across CosmWasm, EVM, Solana, and Tron from a single test, declare its logical methods in a spec trait and apply `#[cross_vm_contract(StructName)]`. The macro generates the wrapper struct, its `new` / `instance` constructors, the `on_before` / `on_after` hook forwarders, and a dispatcher per method that matches the chain's VM and calls the matching `cw_*` / `evm_*` / `svm_*` / `tron_*` hook. You write only the per VM hooks.

```rust
use cross_vm_framework::prelude::*;

// Declare logical methods once. Return types are the bare Ok type (the macro wraps each in
// Result<_, CrossVmError>). A method returning AppResponse<_> fires before/after hooks;
// a query like `count -> u64` is a plain dispatch.
#[cross_vm_contract(Counter)]
pub trait CounterSpec {
    async fn setup(&self, wallet: &str);
    async fn increment(&self, wallet: &str) -> AppResponse<()>;
    async fn count(&self) -> u64;
}

// Write only the per VM hooks. The macro generated `struct Counter { base: ContractBase }`
// and the dispatchers that call these.
impl Counter {
    async fn cw_increment(&self, wallet: &str) -> Result<AppResponse<()>, CrossVmError> {
        let chain = self.base.cosmwasm()?;          // typed handle, WrongVm on mismatch
        let addr  = self.base.cw_addr()?;           // typed deployed address
        let raw = chain
            .contract_as::<CounterContract>(addr)
            .increment(wallet)
            .await?;   // typed call (see below)
        Ok(AppResponse::cosmwasm((), raw))          // typed payload + raw per-VM result
    }
    // evm_increment / svm_increment own their native encoding the same way.
}
```

A call site brings the spec trait into scope (`use ...::CounterSpec;`) to reach the dispatchers; the inherent `new` / `instance` / `on_*` need no import. Recover native types inside a hook with `self.base.cosmwasm()? / evm()? / solana()?` for the chain and `self.base.cw_addr()? / evm_addr()? / svm_addr()?` for the deployed address. A VM you do not support returns `CrossVmError::unimplemented(kind, "...")` from that arm.

**Typed CosmWasm calls.** Instead of hand building `ExecuteMsg` / `query_wasm_smart`, derive `CwExecuteFns` / `CwQueryFns` on the contract's message enums (behind a `cross-vm` feature so the wasm build stays clean):

Declare a per-contract marker with `cross_vm_cw_interface!` (cw-orch's `#[interface]` analogue), then derive the handles:

```rust
#[cfg(feature = "cross-vm")]
cross_vm_macros::cross_vm_cw_interface!(pub CounterContract, InstantiateMsg, ExecuteMsg, QueryMsg);

#[derive(Serialize, Deserialize, /* ... */)]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwExecuteFns))]
pub enum ExecuteMsg {
    Increment {},
    Reset {},
}

#[derive(Serialize, Deserialize, /* ... */)]
#[cfg_attr(feature = "cross-vm", derive(cross_vm_macros::CwQueryFns))]
pub enum QueryMsg {
    #[cfg_attr(feature = "cross-vm", returns(CountResponse))]
    GetCount {},
}
```

That emits `ExecuteMsgFns` / `QueryMsgFns` traits implemented for `CwContract<I>` where `I: CwInterface<ExecuteMsg = ...>` / `QueryMsg = ...`, one typed `async fn` per variant: `chain.contract_as::<CounterContract>(addr).increment(wallet)` and `.get_count()`. Named or tuple variant fields become method args (tuple fields as positional `arg0`, `arg1`, ...); query variants need `#[returns(T)]`; a variant marked `#[payable]` gains a trailing `funds: &[Coin]` arg. Add `#[cross_vm(trait_name = "...")]` on the enum to rename the generated trait, e.g. to run alongside cw-orch's `ExecuteFns` / `QueryFns` without a name clash. For dynamic message construction (no typed `*Fns`), use the untyped `chain.contract(addr)` handle (`CwContract<()>`) and call `execute` / `query` directly. EVM gets typed calls from `alloy::sol!`; Solana has no schema, so its hooks stay hand written.

**Transaction hooks.** A wrapper can run side logic (an indexer, a bridge relay, an event listener) before and after each transaction. Register with `on_before` / `on_after`; the dispatcher fires them around the per VM execution. An after hook receives the uniform `AppResponse`, so it reacts to the result independent of the VM:

```rust
counter.on_after(|ctx| {
    println!("{} on {:?} -> {:?}", ctx.label(), ctx.kind(), ctx.transaction_hash());
    Ok(())
});
```

Hooks are synchronous `FnMut` (the runtime is current thread, so async side effects flow through a channel or an `Rc<RefCell<_>>` the closure drains later). The first `Err` aborts: a before `Err` stops the transaction; an after `Err` becomes the method's error. Events are exposed per VM (`cosmwasm_events()` / `evm_logs()` / `solana_logs()`) because the shapes do not unify.

See `examples/cross-vm-tests/tests/support/counter.rs` for the full three VM wrapper, and `DEVELOPER.md` for the complete hook reference.

## Two ways to write a test

Both run on the same chains. The difference is who drives the operations.

Use **`MultiChainEnv` directly** when the test is a fixed storyline you write out by hand: inject the chains, fund, `start()`, then run a known sequence of calls and assert the exact end state. This is the right tool for "does this specific cross VM flow work" (deploy here, call there, assert balances on a third chain). Every step and assertion is explicit and the failure points straight at the line that broke. See `examples/cross-vm-tests/tests/cross_vm/`.

Use the **`Harness` runner** when you want a property checked across *many* sequences you did not write by hand. See [Property testing harness](#property-testing-harness) below.

Rule of thumb: reach for `MultiChainEnv` first. Promote to a `Harness` once you find yourself wanting to assert the same property over many different sequences, or want fuzz, soak, or replay coverage.

A worked cross VM flow, a CosmWasm/EVM ping pong relayer driven through one `MultiChainEnv`, lives at `examples/cross-vm-tests/tests/cross_vm/ping_pong.rs` (with a narrative in `examples/PING_PONG.md`).

## Property testing harness

You implement one `Harness` (a `World` of persisted bookkeeping, an `Operation` enum, an `Invariant` enum, an `OpKind` enum of the data free operation kinds, and `apply` / `generate_op` / `check`). Generation is decomposed: `generate_op(rng, world, kind)` builds a random instance of one kind, and the runner picks each kind by weight. `weight(ctx, world, kind)` (a provided default returning 1) sets the relative draw weight per kind for the current state, so a harness can bias the mix or return 0 to exclude a kind until the world makes it meaningful. Each test builds its own `(Ctx, World)` (deploy, prime the model, set up op preconditions) and loads it into a mode typed runner with `r.setup(ctx, world)`. The runner sits on top of the env, it does not replace it.

That one harness then drives several runner types:

| Runner | What it does | Reach for it when |
| --- | --- | --- |
| `FuzzRunner::run(ops, kinds, check_every)` | One short random sequence over the loaded world, drawing from all kinds (`None`) or a restricted subset | The input space is large and you want random exploration of operation interleavings |
| `InvariantRunner::run(ops, None, check_every)` | One long persisted sequence, invariants checked along the way | A stateful sequence must keep a property true (model matches chain, no bad debt) |
| `EnduranceRunner::run(EnduranceConfig)` | Random ops at random wall clock delays (`base_delay + rand(0..=max_delay)`) with block progression, then a final sweep | Soak testing for drift, time, or block height dependent bugs (and live RPC, paced by `base_delay`) |
| `ScenarioRunner::run_case` / `run_scenario` (rstest) | One concrete op or sequence | Exhaustive coverage of a small grid (chain x chain) via `#[rstest] #[values(..)]` |
| `ScenarioRunner::replay(history)` | Re runs a recorded failing sequence deterministically | Turning a fuzz failure into a regression test |

The fuzz, invariant, and endurance runs are attribute macros that inject a seeded, mode typed runner shell into a `#[runner]` argument. You write the setup, the `run(..)` call, and the asserts in the body. `#[fuzz_runner]` fans the test out into one `#[tokio::test]` per case (parallel, individually named, filterable, reproducible by seed):

```rust
#[fuzz_runner(harness = CounterHarness, seed = 7, cases = 64)]
async fn counter_fuzz(#[runner] mut r: FuzzRunner<CounterHarness>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(25, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}
// -> counter_fuzz_case_0 .. counter_fuzz_case_63, each its own setup and one fuzz sequence
```

Case `i` is seeded by `sub_seed(seed, i)`, so a flagged case re-runs in isolation by name. `#[invariant_runner]` and `#[endurance_runner]` emit a single test each. A negative seed (`seed = -1`) picks a fresh random seed per run and prints it, so a failure stays reproducible by copying the printed value back as a fixed `seed`.

**Reproducing a failure.** Every failed run reports its seed and the exact operation history. Feed the seed back as the macro's `seed =`, or turn the history into a deterministic regression test with `ScenarioRunner::replay(history)`; both re-drive the identical sequence. When filing a bug, the seed plus mode (and the failing invariant name) are all a maintainer needs.

In the example crate the heavier runs are opt in so the default `cargo test` stays fast: the fuzz, invariant, and endurance tests sit behind the `fuzz`, `invariant`, and `endurance` cargo features, while the scenario (rstest matrix) tests and the runner mechanics self tests always run. See `examples/cross-vm-tests/tests/harness/` for a multi chain counter, a DeFi vault, and the runner mechanics.

A config file's `[suite.<name>]` can also chain profiles into a dependency gated pipeline, with a later phase continuing from an earlier one's finished state instead of a fresh setup:

```toml
[suite.progressive]

  [[suite.progressive.phases]]
  profile = "deposit-soak"

  [[suite.progressive.phases]]
  profile = "mixed-after-deposits"
  needs = ["deposit-soak"]
  world = "inherit"
```

Phases mix modes freely: a scenario phase can seed liquidity or set preconditions with a few concrete steps, hand its world to an invariant phase (a long random "auto run"), which hands off to a single case fuzz phase, each starting where the previous one stopped and optionally re-pinning state through its own `params` table:

```toml
[suite.staged]

  [[suite.staged.phases]]
  profile = "seed-liquidity"    # scenario: fixed setup steps

  [[suite.staged.phases]]
  profile = "random-mix"        # invariant: long random sweep
  needs = ["seed-liquidity"]
  world = "inherit"

  [[suite.staged.phases]]
  profile = "deep-case"         # fuzz, cases = 1: deep single case
  needs = ["random-mix"]
  world = "inherit"
```

See `docs/config-runs-spec.md` section 4.7 for the phase schema and structural rules, and `examples/cross-vm-tests/vault.cross-vm.toml`'s `progressive` and `staged` suites for the checked in, runnable versions.

## The config-driven CLI

The same `Harness` also drives from a declarative TOML (or JSON) config file, through a command line runner. A config names the harness, an `[env]` table, and a set of `[profile.*]` blocks (one per fuzz, invariant, endurance, or scenario mode), plus optional `[suite.*]` pipelines:

```toml
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 4
ops = 20

[profile.invariant-long]
mode = "invariant"
ops = 500
```

A binary registers its harness once and hands off to the shared CLI:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    cross_vm_framework::cli::Cli::new()
        .env_file(".env")
        .register("vault", || VaultHarness, vault_config_setup)
        .main()
        .await
}
```

That binary drives the whole config from the command line:

```sh
# type-check a config against the registered harness (touches no chains)
cargo run -p cross-vm-tests --bin cross-vm -- validate vault.cross-vm.toml

# run one profile against the mock chains
cargo run -p cross-vm-tests --bin cross-vm -- run vault.cross-vm.toml --profile smoke

# list registered harnesses and a config's profiles and suites
cargo run -p cross-vm-tests --bin cross-vm -- list vault.cross-vm.toml

# re-run a failing run's replay artifact
cargo run -p cross-vm-tests --bin cross-vm -- replay run-logs/replay/vault-deep-....replay.toml
```

Exit codes are a stable CI contract: `0` all runs passed, `1` a `Bug` or invariant violation, `2` an infrastructure or setup failure, `3` a config or usage error. Every failing fuzz, invariant, or endurance run writes a self contained `*.replay.toml` artifact (itself a valid config), so `replay` closes the failure to regression loop with no bespoke tooling. `--json-report <path>` emits a machine readable envelope (`schema_version = 1`). Environment overrides follow a fixed precedence: a CLI flag beats a `CROSS_VM_*` variable, which beats a profile key, which beats the built in default. See `docs/config-runs-spec.md` for the full schema.

**Three reusable layers.** The runner, the config loader, and the CLI are three standalone crates, none of which knows about chains:

```
harness-core     the Harness trait, the mode-typed Runner, rng, stats, outcomes
harness-config   the TOML/JSON config schema and load pipeline (ConfigExt seam)
harness-cli      the registry, profile resolution, run driving, JSON reports,
                 replay artifacts, and the clap CLI (CliDomain seam)
```

cross-vm is one variant on top of them. `cross-vm-config` adds the `[[chain]]` sections through `ConfigExt`, and `cross-vm-framework` adds the `--target` flags and chain-aware setup through `CliDomain`, keeping the `cross-vm` binary a thin `harness_cli::Cli<CrossVmDomain>`. You can also use the stack raw, with no domain layer at all: `examples/math-tests` registers a plain `MathHarness` against `harness_cli::GenericDomain` and drives it from `math.harness.toml` (`harness run math.harness.toml --profile smoke`), a complete config-driven runner whose binary is fifteen lines. `docs/extending-harness-cli.md` walks both the raw path and building a new variant.

## Wallets

Mnemonics are the only secret, and they live in a `.env` (gitignored). Everything else, the wallet roster (labels, account indices, how each wallet sources its key), is a compile time const built with `define_wallet_roster!`, resolved by a single shared `WalletFactory`. Each roster row picks one source:

* `env_mnemonic("VAR")`: read a BIP-39 phrase from an env var.
* `auto`: generate a fresh random mnemonic at build time (for mock chains).
* `env_private_key("VAR")`: read a raw VM native key.

The factory keeps each row's source and resolves it on demand: `auto` rows generate their mnemonic once at construction, while `env_mnemonic` / `env_private_key` rows read their variable lazily, only when that wallet first signs. So load the `.env` before signing (`dotenvy::from_path(".env")`); a missing variable fails at the signing call, not at construction, which lets a roster carry a funded on-chain wallet whose secret is absent for runs that never use it (e.g. the `on_chain` row, `env_mnemonic("ON_CHAIN_WALLET")`, used only by the live `rpc-endurance` test). Serializing concurrent broadcasts of one live account is a process-global locker (`core::wallet_lock`) keyed by `(chain, address)`, acquired only on the RPC path and held across send→confirm; mock backends take no lock, and different accounts and chains run in parallel. A global (not per-factory) lock is what makes two separate tests on the same on-chain account serialize. Per ecosystem HD derivation uses coin types 118 / 60 / 501 (CosmWasm / EVM / Solana).

Copy `.env.example` to `.env` and fill in your mnemonics. An all `auto` roster (or the empty `&[]` roster used in the quickstarts) needs no `.env` at all. See `crates/framework/examples/wallet_quickstart.rs` for a derive, sign, broadcast walkthrough.

## Live RPC providers

Alongside the mock backends, each VM crate ships an RPC provider (`CwRpcProvider`, `EvmRpcProvider`, `SvmRpcProvider`) that talks to a real node at the endpoint baked into the chain preset (`OSMOSIS_TESTNET.rpc(wallets)`, and so on). All three serve live reads; CosmWasm and EVM also sign and broadcast writes (`store_code`/`instantiate`/`execute_contract` and `deploy_create`/`call`). Solana writes are not implemented yet. CosmWasm reads block height, native balance, and smart queries; EVM reads block number, native balance, and `eth_call`; Solana reads slot, lamport balance, and `getAccountInfo`. Construction and a read flow are shown in the `*_rpc_quickstart` examples:

```
cargo run -p cross-vm-cosmwasm --example cosmwasm_rpc_quickstart
cargo run -p cross-vm-solidity --example evm_rpc_quickstart
cargo run -p cross-vm-solana   --example solana_rpc_quickstart
```

A fourth provider, `TronRpcProvider`, talks to a live java-tron node over the TronGrid HTTP REST API (`/wallet/*` endpoints, via `reqwest` plus `serde_json`). It serves live reads (`balance`, `block_height`, `static_call`) and signs and broadcasts writes (`deploy_create` and `call` build the unsigned transaction at the node, sign its `txID` locally with the wallet's secp256k1 key, then `broadcasttransaction`). Only `set_balance` returns `Unimplemented` (a live chain cannot mint). See [Tron (TVM)](#tron-tvm) for the details.

## Tron (TVM)

Tron is the fourth ecosystem, behind the same `ChainProvider` trait as the other three. It ships two backends mirroring the EVM crate: `TronChain` is either `Mock(TronMockProvider)` or `Rpc(TronRpcProvider)`. The TVM is an EVM derivative, so the mock reuses a `revm` core and adds the layers where Tron diverges from Ethereum.

The mock runs that `revm` core with TVM-accurate layers on top: a base58check `TronAddress` (the `0x41` version prefix over a secp256k1 key, whose inner 20 bytes equal the matching EVM address), the Tron precompiles injected into revm (the TIP-272 relocations, `ripemd160` at `0x20003` and `blake2f` at `0x20009`, plus `validatemultisign` at `0x0a`, all over secp256k1, not ed25519), a provider-layer energy and bandwidth accounting shim that sits outside revm's gas loop, and u64 sun balances (1 TRX = 1,000,000 sun). Wallets derive over secp256k1 at SLIP-44 coin type 195 (path `m/44'/195'/<index>'/0/0`). Tron logs are EVM-shaped, so the mock surfaces revm logs directly.

Two honest v1 limits remain in the mock. The mock's `CREATE` / `CREATE2` use revm's EVM address derivation, not Tron's tx-id-based formula. The real formula ships as the pure functions `tron_create_address` / `tron_create2_address` for tooling, because revm 41 does not allow cleanly overriding the in-VM derivation. The energy shim is coarse account-level accounting, not per-opcode costs. The RPC backend is no longer a stub: it drives a live java-tron node over TronGrid HTTP for reads and signed writes, and `call` polls `gettransactioninfobyid` after broadcast so it returns the transaction's return data and EVM-shaped logs (a reverted tx surfaces as an error). The only remaining gap is range and topic log search (`eth_getLogs`, TronGrid `/v1/contracts/{addr}/events`), which is not yet wired.

## Macros at a glance

| Macro | Kind | Purpose |
| --- | --- | --- |
| `#[cross_vm_contract(Name)]` | attribute | Turn a spec trait into a contract wrapper that dispatches each method to the matching `cw_*` / `evm_*` / `svm_*` / `tron_*` hook |
| `cross_vm_cw_interface!` | function-like | Declare a zero-sized `CwInterface` marker for one contract (scopes typed `*Fns` to `CwContract<I>`) |
| `#[derive(CwExecuteFns)]` | derive | Typed per variant `async fn` execute methods from a CosmWasm `ExecuteMsg` enum (named or tuple fields become args; `#[payable]` adds a `funds` arg) |
| `#[derive(CwQueryFns)]` | derive | Typed per variant `async fn` query methods from a `QueryMsg` enum (each variant needs `#[returns(T)]`) |
| `define_wallet_roster!` | function-like | Compile time wallet roster with typed `WalletLabel` fields |
| `#[fuzz_runner]` | attribute | Fan a fuzz test into one `#[tokio::test]` per case with a seeded `FuzzRunner` |
| `#[invariant_runner]` | attribute | One `#[tokio::test]` with a seeded `InvariantRunner` |
| `#[endurance_runner]` | attribute | One `#[tokio::test]` with a seeded `EnduranceRunner` |

Five are re-exported from `cross_vm_framework::prelude`. The three runner attribute macros live in the standalone `harness-core-macros` crate (the prelude re-exports them directly, since they know nothing about chains); the rest are defined in `cross-vm-macros`. The two `Cw*Fns` derives are applied on a contract's message enums (often in a separate crate compiled to wasm), so they are named directly as `cross_vm_macros::CwExecuteFns` / `CwQueryFns` behind a `cross-vm` feature. The generated code names framework types unqualified, so any invocation site needs `use cross_vm_framework::prelude::*;` in scope.

## Workspace layout

```
crates/
  core/           cross-vm-core         shared ChainProvider / ChainSpec traits, ChainKind, CrossVmError, FundError
  cosmwasm/       cross-vm-cosmwasm     CwMockProvider (cw-multi-test), CwRpcProvider (live reads), CwChain, CwAsset
  solidity/       cross-vm-solidity     EvmMockProvider (revm), EvmRpcProvider (live reads), EvmChain, EvmAsset
  solana/         cross-vm-solana       SvmMockProvider (litesvm), SvmRpcProvider (live reads), SvmChain, SvmAsset
  tron/           cross-vm-tron         TronMockProvider (revm + TVM layers), TronRpcProvider (live java-tron over TronGrid HTTP), TronChain, TronAsset
  harness/        harness-core          standalone, VM agnostic property testing runner: the Harness trait, mode typed Runner, rng, stats, outcome types
  harness-macros/ harness-core-macros   proc-macros for harness-core: fuzz_runner, invariant_runner, endurance_runner
  harness-config/ harness-config        generic TOML/JSON run-config schema and loader (the ConfigExt seam); pure data, no runtime deps
  harness-cli/    harness-cli           generic registry, profile resolution, run driving, JSON reports, replay artifacts, and clap CLI (the CliDomain seam)
  config/         cross-vm-config       cross-vm variant of harness-config: the [[chain]] sections, the typed EnvSpec, and chain validation
  macros/         cross-vm-macros       proc-macros: cross_vm_contract, CwExecuteFns/CwQueryFns, define_wallet_roster, config_runner
  framework/      cross-vm-framework    MultiChainEnv (umbrella over all VMs), a Ctx/classify layer over harness-core, the cross-vm CLI variant (CrossVmDomain over harness-cli), prelude
```

Dependency trees are isolated per crate, so building or testing one VM does not pull the others. Each VM crate carries a `chains` module with predefined chain constants. `harness-core`, `harness-config`, and `harness-cli` are VM agnostic and know nothing about chains: they are the reusable generic stack (see [The config-driven CLI](#the-config-driven-cli)). `cross-vm-config` and `cross-vm-framework` are the cross-vm variant built on that stack. The framework re-exports everything, adds the multi chain `MultiChainEnv`, pins `harness-core`'s generic `Ctx` to a started multi-chain environment for the property testing harness, and supplies the `CrossVmDomain` that gives the generic CLI its `[[chain]]` sections and `--target` flags.

Example crates and contract sources live outside the root workspace:

```
contracts/
  cosmwasm/   counter, ping-pong, vault CosmWasm contract crates
  solidity/   Foundry project (Counter, PingPong) -> out/ artifacts
  solana/     Anchor programs -> target/deploy/*.so
  tron/       tronbox project -> build/ artifacts
examples/
  common/         cross-vm-common   reusable contract bindings (mocks) + shared wallet/tracing helpers
  cross-vm-tests/ cross-vm-tests    multi-chain tests (cross-VM flows, ping-pong, vault, replay) + the cross-vm CLI
  evm-tests/      cosmos-tests/     solana-tests/     tvm-tests/   single-VM Counter example crates
  math-tests/     math-tests        raw harness-core + harness-cli example (no chains, no domain layer); the reference for building a new variant
  scripts/        deploy_counter    imperative deploy script over the live RPC providers
```

Each single-VM example crate exercises one `Counter` harness three ways: attribute-macro runners (`tests/harness.rs`), config-driven `#[config_runner]` fan-out (`tests/config_runner.rs`) against its `counter.cross-vm.toml`, and a CLI binary driven end to end (`tests/cli_e2e.rs`). All of them source their contract bindings from `cross-vm-common`. The `math-tests` crate is the minimal counterpart: a plain arithmetic harness driven by the generic config and CLI with no chains and no domain layer at all, doubling as the worked walkthrough in `docs/extending-harness-cli.md`.

## Build and test

```
cargo build --workspace
cargo test  --workspace
```

The integration tests embed compiled contract artifacts, so build those first with `make compile` (or a single ecosystem: `make compile-cosmwasm` / `compile-solidity` / `compile-solana`), then run `make test`. A fresh checkout will not compile the integration tests until the artifacts exist:

* CosmWasm: the `contracts/cosmwasm/contracts/*` crates are consumed as rlibs (no artifact build strictly needed for counter, but `make compile-cosmwasm` builds the wasm into `contracts/cosmwasm/artifacts/`).
* EVM: `sol!` parses the forge build JSON for the ABI and creation bytecode (`forge build`).
* Solana: `include_bytes!` loads the `cargo-build-sbf` output (`.so`).

The heavier harness modes are feature gated to keep the default `cargo test` fast. Enable them with `make test-fuzz` / `test-invariant` / `test-endurance` / `test-harness-all`, or directly:

```
cargo test -p cross-vm-tests --test harness --features "fuzz invariant endurance"
```

`make test-rpc-endurance` runs the endurance harness against a live Base Sepolia chain over RPC (needs network and a funded `ON_CHAIN_WALLET` mnemonic in `.env`; it signs the `on_chain` wallet). Other handy targets: `make test-cross-vm` (hand written flows), `make test-harness` (scenario matrices + mechanics), `make fmt`.

## Status (Supported / Planned)

| Capability | CosmWasm | EVM | Solana | Tron | Notes |
| --- | --- | --- | --- | --- | --- |
| Mock provider (in process VM) | Supported | Supported | Supported | Supported | `cw-multi-test` / `revm` / `litesvm`; Tron is a `revm` core with TVM-accurate layers |
| Live RPC reads | Supported | Supported | Supported | Supported | validated on `osmo-test-5`, Ethereum Sepolia, Solana Devnet; Tron RPC reads over TronGrid HTTP (block height, native balance, `triggerconstantcontract`), exercised against Nile |
| Live RPC writes (deploy + call) | Supported | Supported | Planned | Supported | CosmWasm/EVM/Tron sign and broadcast (CosmWasm deploy via `store_code` with compiled wasm bytes; Tron signs the `txID` and broadcasts over TronGrid HTTP); Solana writes return `Unimplemented`. `set_balance` is `Unimplemented` on every RPC backend (a live chain cannot mint). On mocks, `set_balance(addr, denom, amount)` mints any bank denom on CosmWasm and accepts only the native symbol on EVM, Solana, and Tron. |
| Wallet derivation (mnemonic to signer) | Supported | Supported | Supported | Supported | coin types 118 / 60 / 501 / 195, global (chain, address) broadcast lock on the RPC path |
| Cross VM contract wrapper (`#[cross_vm_contract]`) | Supported | Supported | Supported | Supported | typed CosmWasm/EVM calls; Solana and Tron hooks hand written |
| Property `Harness` (fuzz / invariant / endurance / matrix) | Supported (VM agnostic, runs over any injected chain) |||||
| Broader cross VM orchestration layer | Planned |||||

**Planned.** Solana live RPC writes (its mock coupled return types are being decoupled), and the broader cross VM orchestration layer above `MultiChainEnv`.

**Known Tron mock divergences.** `CREATE` / `CREATE2` follow revm's EVM address derivation rather than Tron's tx-id-based formula (the real formula is exposed as the pure functions `tron_create_address` / `tron_create2_address`), and the energy/bandwidth shim is coarse account-level accounting, not per-opcode energy costs.

## Documentation

* `SPEC.md`: architecture and design.
* `DEVELOPER.md`: per crate details, the full contract wrapper and hook reference, and how to add a VM or chain.
* `docs/extending-harness-cli.md`: the three reusable layers (harness-core, harness-config, harness-cli) and how to build a new config-driven variant.
* `docs/config-runs-spec.md`: the TOML/JSON config schema for config-driven runs, profiles, and suites.
* `docs/adding-a-vm.md`: the file-by-file checklist for a new chain ecosystem.
* `CONTRIBUTING.md`: setup, the pre-PR command list, and ground rules.
* `CHANGELOG.md`: release notes.

## License

MIT. See `LICENSE`.
