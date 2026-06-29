# cross-vm-testing

A Rust testing suite for cross VM work spanning three execution environments: CosmWasm (via `cw-multi-test`), EVM/Solidity (via `revm`), and Solana (via `litesvm`).

Phase 1 ships one chain provider per ecosystem. A chain provider is the analogue of alloy's `Provider`, cw-orch's `CwEnv`, or test-tube's `Runner`. Each provider wraps an in process VM ("mock") behind a single shared trait, so test code and cross VM scripts read the same regardless of target chain.

## Workspace layout

```
crates/
  core/       cross-vm-core      shared ChainProvider / ChainSpec traits, ChainKind, CrossVmError, FundError
  cosmwasm/   cross-vm-cosmwasm  CwMockProvider (cw-multi-test), CwRpcProvider (live reads), CwChain, CwAsset
  solidity/   cross-vm-solidity  EvmMockProvider (revm), EvmRpcProvider (live reads), EvmChain, EvmAsset
  solana/     cross-vm-solana    SvmMockProvider (litesvm), SvmRpcProvider (live reads), SvmChain, SvmAsset
  framework/  cross-vm-framework MultiChainEnv (umbrella over all VMs), prelude
```

Each VM crate carries a `chains` module with predefined chain constants used to spin up a provider quickly. The `cross-vm-framework` crate re-exports everything and adds the multi chain `MultiChainEnv`.

## MultiChainEnv: many chains, one simulation

`MultiChainEnv` models a chain simulation with two phases. During setup you inject chains and declare funding; `start()` applies the plan and enters the running phase, where only chain execution is allowed (funding and injection are gone at the type level).

```rust
use std::rc::Rc;
use cross_vm_framework::prelude::*;

// The suite is async; run on a current-thread runtime (#[tokio::test] / #[tokio::main]).
// One WalletFactory is shared by the env and every chain. An empty roster needs no .env.
let wallets = Rc::new(WalletFactory::from_roster(&[]).unwrap());
let mut env = MultiChainEnv::new("swap-test", wallets.clone());
env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets.clone())));
env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets.clone())));

let cw_alice = env.cosmwasm("osmosis").unwrap().new_account("alice").await;
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

All provider and env operations are `async`. Because the mock backends (`revm`,
`litesvm`, `cw-multi-test`) are not `Send`, run on a current-thread runtime:
`#[tokio::test]` and `#[tokio::main(flavor = "current_thread")]`.

Funding semantics: native assets mock mint the shortfall; token assets (cw20/erc20/SPL) are validated against the real on chain balance (you mint them by deploying the token during setup). On all three RPC providers, native funding validates rather than mints (a live chain cannot mint): it reads the real balance and reports a `Shortfall` if the account is underfunded. Token RPC funding paths (cw20/erc20/SPL) return `Unimplemented`.

## Two ways to write a test: MultiChainEnv directly, or the Harness runner

Both run on the same chains. The difference is who drives the operations.

Use **`MultiChainEnv` directly** when the test is a fixed storyline you write out by hand: inject the chains, fund, `start()`, then run a known sequence of calls and assert the exact end state. This is the right tool for "does this specific cross VM flow work" (deploy here, call there, assert balances on a third chain). Every step and assertion is explicit and the failure points straight at the line that broke. See `examples/integration-tests/tests/cross_vm/`.

Use the **`Harness` runner** when you want a property checked across *many* sequences you did not write by hand. You implement one `Harness` (a `World` of persisted bookkeeping, an `Operation` enum, an `Invariant` enum, an `OpKind` enum of the data-free operation kinds, and `apply` / `generate_op` / `check`). Generation is decomposed: `generate_op(rng, world, kind)` builds a random instance of one kind, and `generate` (a provided default, override only to bias the kind mix) picks a kind and calls it. The harness no longer builds the environment itself: each test builds its own `(Ctx, World)` (deploy, prime the model, set up op preconditions) and loads it into a mode-typed runner with `r.setup(ctx, world)`. The runner sits on top of the env, it does not replace it. That one harness then drives several runner types:

| Runner | What it does | Reach for it when |
| --- | --- | --- |
| `FuzzRunner::run(ops, kinds, check_every)` | One short random sequence over the loaded world, drawing from all kinds (`None`) or a restricted subset | The input space is large and you want random exploration of operation interleavings |
| `InvariantRunner::run(ops, None, check_every)` | One long persisted sequence, invariants checked along the way | A stateful sequence must keep a property true (model matches chain, no bad debt) |
| `EnduranceRunner::run(EnduranceConfig)` | Random ops at random wall clock delays (`base_delay + rand(0..=max_delay)`) with block progression, then a final sweep | Soak testing for drift, time, or block height dependent bugs (and live RPC, paced by `base_delay`) |
| `ScenarioRunner::run_case` / `run_scenario` (rstest) | One concrete op or sequence | Exhaustive coverage of a small grid (for example chain x chain) via `#[rstest] #[values(..)]` |
| `ScenarioRunner::replay(history)` | Re runs a recorded failing sequence deterministically | Turning a fuzz failure into a regression test |

The runner contributes deterministic seeding (read it back with `r.seed()` to vary setup per case), the op count, and the random op stream. It does not mutate operation fields itself: what gets randomized is whatever your `generate_op` writes (fill fields with `rng.range(..)`, or derive `Arbitrary` and call `sample_arbitrary` for full field fuzzing). Invariants whose precondition has not happened yet return `CheckOutcome::Skipped` instead of failing.

The fuzz, invariant, and endurance runs are written as attribute macros that inject a seeded, mode-typed runner shell into a `#[runner]` argument. You write the setup, the `run(..)` call, and the asserts in the body. `#[fuzz_runner]` fans the test out into one `#[tokio::test]` per case (parallel, individually named, filterable, reproducible by seed):

```rust
#[fuzz_runner(harness = CounterHarness, seed = 7, cases = 64)]
async fn counter_fuzz(#[runner] mut r: FuzzRunner<CounterHarness>) {
    let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(25, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}
// -> tests counter_fuzz_case_0 .. counter_fuzz_case_63, each its own setup and one fuzz sequence
```

The case count is a compile-time choice (Rust freezes its test list before running), and case `i` is seeded by `sub_seed(seed, i)`, so a flagged case re-runs in isolation by name. `#[invariant_runner]` and `#[endurance_runner]` emit a single test each. A negative seed (`seed = -1`) picks a fresh random seed per run and prints it, so a failure stays reproducible by copying the printed value back as a fixed `seed`.

In the example crate the heavier runs are opt-in so the default `cargo test` stays fast: the fuzz, invariant, and endurance harness tests sit behind the `fuzz`, `invariant`, and `endurance` cargo features respectively, while the scenario (rstest matrix) tests and the runner-mechanics self-tests always run. Enable a category with `cargo test -p cross-vm-integration-tests --test harness --features fuzz` (or the `make test-fuzz` / `test-invariant` / `test-endurance` / `test-harness-all` targets).

Rule of thumb: reach for `MultiChainEnv` first. Promote to a `Harness` once you find yourself wanting to assert the same property over many different sequences, or want fuzz, soak, or replay coverage. See `examples/integration-tests/tests/harness/` for a multi chain counter, a DeFi vault, and the runner mechanics.

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

The same shape works for `cross_vm_solidity::chains::ETHEREUM` and `cross_vm_solana::chains::SOLANA_DEVNET`. Runnable versions live in each crate's `examples/` (`cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart`).

Predefined chains include `OSMOSIS`, `JUNO`, `NEUTRON`, `COSMOS_HUB` (CosmWasm); `ETHEREUM`, `ARBITRUM`, `OPTIMISM`, `BASE`, `POLYGON` (EVM); `SOLANA_MAINNET`, `SOLANA_DEVNET`, `SOLANA_TESTNET`, `SOLANA_LOCALNET` (Solana).

## Wallets

Mnemonics are the only secret, and they live in a `.env` (gitignored). Everything else, the
wallet roster (labels, account indices, how each wallet sources its key), is a compile-time const
built with `define_wallet_roster!`, resolved by a single shared `WalletFactory`. Each roster row
picks one source: `env_mnemonic("VAR")` (read a BIP-39 phrase from an env var), `auto` (generate a
fresh random mnemonic at build time, for mock chains), or `env_private_key("VAR")` (read a raw
VM-native key). The factory keeps each row's source and resolves it on demand: `auto` rows generate
their mnemonic once at construction, while `env_mnemonic` / `env_private_key` rows read their
variable lazily, only when that wallet first signs. So load the `.env` before signing (e.g.
`dotenvy::from_path(".env")`); a missing variable errors at the signing call, not at construction,
which lets a roster carry a funded on-chain wallet whose secret is absent for runs that never use
it (e.g. the `on_chain` row used only by the live `rpc-endurance` test).

Copy `.env.example` to `.env` and fill in your mnemonics. An all-`auto` roster (or the empty
`&[]` roster used in the quickstarts) needs no `.env` at all. See
`crates/framework/examples/wallet_quickstart.rs` for a derive-sign-broadcast walkthrough.

## Live RPC providers

Alongside the mock backends, each VM crate ships an RPC provider (`CwRpcProvider`,
`EvmRpcProvider`, `SvmRpcProvider`) that talks to a real node over a URL you supply. They serve
the live read paths today (and EVM write paths). Construction and a read flow are shown in the
`*_rpc_quickstart` examples:

```
cargo run -p cross-vm-cosmwasm --example cosmwasm_rpc_quickstart
cargo run -p cross-vm-solidity --example evm_rpc_quickstart
cargo run -p cross-vm-solana   --example solana_rpc_quickstart
```

## Build and test

```
cargo build --workspace
cargo test  --workspace
cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart
cargo run -p cross-vm-solidity --example evm_quickstart
cargo run -p cross-vm-solana   --example solana_quickstart
cargo run -p cross-vm-framework --example env_quickstart
cargo run -p cross-vm-framework --example wallet_quickstart
```

The integration tests embed compiled contract artifacts, so build those first with `make compile`
(or a single ecosystem: `make compile-cosmwasm` / `compile-solidity` / `compile-solana`), then run
`make test`. The heavier harness modes are feature-gated to keep the default `cargo test` fast:
enable them with `make test-fuzz` / `test-invariant` / `test-endurance` / `test-harness-all` (or
`cargo test -p cross-vm-integration-tests --test harness --features "fuzz invariant endurance"`).
`make test-rpc-endurance` runs the endurance harness against a live Base Sepolia chain over RPC
(needs network and a funded `ON_CHAIN_WALLET` mnemonic in `.env`; it signs the `on_chain` wallet).

A worked cross-VM flow, a CosmWasm/EVM ping-pong relayer driven through one `MultiChainEnv`, lives
at `examples/integration-tests/tests/cross_vm/ping_pong.rs`.

See `SPEC.md` for the architecture, `DEVELOPER.md` for per crate details, and `CHANGELOG.md` for release notes.

## Status (Supported / Planned)

| Capability | CosmWasm | EVM | Solana | Notes |
| --- | --- | --- | --- | --- |
| Mock provider (in-process VM) | Supported | Supported | Supported | `cw-multi-test` / `revm` / `litesvm` |
| Live RPC reads | Supported | Supported | Supported | validated on `osmo-test-5`, Ethereum Sepolia, Solana Devnet |
| Live RPC writes (deploy + call) | Planned | Supported | Planned | Cosmos/Solana return `Unimplemented`; signer already plumbed through |
| Wallet derivation (mnemonic to signer) | Supported | Supported | Supported | coin types 118 / 60 / 501, global (chain, address) broadcast lock on the RPC path |
| Property `Harness` (fuzz / invariant / endurance / matrix) | Supported (VM-agnostic, runs over any injected chain) ||||
| Broader cross-VM orchestration layer | Planned ||||

**Live RPC reads.** CosmWasm reads block height, native balance, and smart queries; EVM reads
block number, native balance, and `eth_call`; Solana reads slot, lamport balance, and
`getAccountInfo`.

**Wallets.** Mnemonics load from a `.env` (the only secret); the roster is a compile-time const
and a per-ecosystem `WalletDeriver` turns a mnemonic plus HD path into that VM's signer. Serializing
concurrent broadcasts of one live account (which would collide on its nonce or account sequence) is
a process-global locker (`core::wallet_lock`) keyed by `(chain, address)`, acquired only on the RPC
path and held across send→confirm; mock backends take no lock, and different accounts and chains run
in parallel. A global (not per-factory) lock is what makes two separate tests on the same on-chain
account serialize.

**Planned.** Cosmos and Solana live-RPC writes (their mock-coupled return types are being
decoupled) and the broader cross-VM orchestration layer above `MultiChainEnv`.
