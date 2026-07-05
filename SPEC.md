# Specification

## Goal

Provide a uniform way to drive four execution environments (CosmWasm, EVM, Solana, Tron) from Rust, so the same test code and cross VM scripts work against any of them. The base is one async chain provider per ecosystem over an in process (mock) backend, with live RPC read providers alongside. On top of that sit the cross VM contract wrapper layer, label based wallets with per ecosystem signing, the `MultiChainEnv` multi chain simulation, and a VM agnostic property testing harness.

## Design

The three VMs disagree on nearly every concrete type. CosmWasm uses `Addr` and JSON messages, EVM uses a 20 byte `Address` and ABI calldata, Solana uses a 32 byte `Address` (pubkey) and Borsh instructions. A single trait built from associated types is the way to share one method vocabulary while letting each VM keep its idiomatic types. This mirrors cw-orch's `CwEnv` and test-tube's `Runner`.

The core trait is asynchronous. Every operation is an `async fn` so the one surface fits both the in-process mocks (whose bodies are synchronous and simply do not await) and the live RPC backends (which await network I/O). `chain_info` stays synchronous since it only returns local metadata. The mock backends (`revm`, `litesvm`, `cw-multi-test`) are not `Send`, so the returned futures are not `Send`; drive them on a current-thread runtime (`#[tokio::test]`, `#[tokio::main(flavor = "current_thread")]`).

Chain handles are cheap to clone and share one underlying state. Each mock provider holds its backend behind `Rc<RefCell<_>>` (the EVM already did; CosmWasm and Solana were converted), so `CwChain`, `EvmChain`, `SvmChain`, and `AnyChain` are `Clone` and the contract operations run behind `&self`. This is what lets a contract wrapper own its own handle (`Contract::new(chain)`) while a test still drives the same chain. It mirrors how cw-orch shares an `Rc<RefCell<App>>` mock environment.

### Core traits (`cross-vm-core`)

`ChainSpec` exposes the metadata common to every predefined chain:

```rust
pub trait ChainSpec {
    fn chain_id(&self) -> &str;
    fn name(&self) -> &str;
    fn native_symbol(&self) -> &str;
    fn rpc_url(&self) -> Option<&str>;
    fn kind(&self) -> ChainKind;   // CosmWasm | Evm | Svm
}
```

`ChainProvider` is the uniform **chain-level** provider surface. Associated types (`Address`, `Account`, `Balance`, `Error`) let each VM specialize while sharing account, balance, and block operations. Methods: `chain_info`, `new_account`, `balance`, `set_balance(addr, denom, amount)`, `block_height`, `advance_blocks`. `set_balance` takes a denom argument. CosmWasm mocks mint any bank denom verbatim and preserve the account's other denoms (setting an amount of 0 clears the denom). EVM, Solana, and Tron accept only their native symbol, matched case-insensitively ("ETH", "SOL", "TRX"), and amounts stay in base units (wei, lamports, sun). Every RPC backend keeps returning `Unimplemented` for `set_balance`. `advance_blocks(n, time)` advances height/slot by `n` and sets the new block timestamp per the `BlockTime` policy (`Custom` for an exact unix-seconds value, `Now` for wall-clock time, `Increment` to add seconds to the current timestamp). It forces blocks on mock backends and is a no-op on RPC backends (a live chain advances on its own). Every mock seeds its clock to `MOCK_BLOCK_TIMESTAMP` so cross-VM packet timeouts compare correctly across VMs.

Contract and program operations are **not** on `ChainProvider`. Each VM crate exposes idiomatic methods on its mock/RPC providers and chain enums:

| VM | Contract/program API |
| --- | --- |
| CosmWasm | `store_code`, `instantiate`, `execute_contract`, `query_wasm_smart` |
| EVM | `deploy_create`, `call`, `static_call` |
| Solana | `add_program`, `send_transaction`, `get_account` |

`CrossVmError` is a unified error enum. Each provider's own error converts into it (via the `Error: Into<CrossVmError>` bound), so cross VM scripts can use one `Result` type.

### Wallets and signing (`cross-vm-core`)

Mnemonics are the only secret. A `.env` holds nothing but BIP-39 phrases (one or more, each under its own variable). The wallet roster is a compile time const built with the `define_wallet_roster!` macro: each `WalletSpec` row names a label, a `WalletSource`, and an account index. A `WalletSource` is one of `Auto` (generate a fresh random mnemonic at build time, for mock chains), `EnvMnemonic(var)` (read a BIP-39 phrase from a named process env var), or `EnvPrivateKey(var)` (read a raw VM native private key, derived directly with no HD path). `WalletFactory::from_roster(roster)` keeps each row's `WalletSource` and `WalletFactory::resolve(label)` materializes it into a `WalletDef` (`Mnemonic { phrase, index, .. }` or `PrivateKey`) on demand: `Auto` rows generate their mnemonic eagerly at construction (their derived address must stay stable within a run), while env-sourced rows read their variable lazily, only when that wallet first signs. So load the `.env` before signing (for example `dotenvy::from_path(".env")`); a missing variable fails at the signing call, not at construction, which lets a roster carry a funded on-chain wallet whose secret is absent for runs that never use it. Adding a wallet means adding a roster row, not calling a runtime registration API.

Key derivation is per ecosystem, behind the `WalletDeriver` trait (a sibling of `ChainProvider`, so providers that need no crypto are unaffected). Each VM crate implements it on its chain handle:

| VM | Coin type | Algorithm | Signer |
| --- | --- | --- | --- |
| EVM | 60 | alloy `MnemonicBuilder` | `PrivateKeySigner` |
| Cosmos | 118 | `bip39` seed + cosmrs `bip32`, bech32 prefix from `chain_info` | `CosmosSigner` (`Rc<SigningKey>` + `Addr`) |
| Solana | 501 | `bip39` seed + SLIP-10 ed25519 | `SvmSigner` (`Rc<Keypair>`) |
| Tron | 195 | secp256k1 (`m/44'/195'/<index>'/0/0`) | secp256k1 key + base58check `TronAddress` |

The factory is VM-agnostic (it stores roster `WalletSource` rows, resolved to signing material on demand, no signer types), which lets it live in `core` while the chains that hold an `Rc<WalletFactory>` live in the VM crates that depend on `core`, with no dependency cycle. Each chain derives and caches its own signer type.

Broadcasts take a wallet label, not an address. `EvmChain::deploy_create`/`call`, `CwChain::instantiate`/`execute_contract`, and `SvmChain::send_transaction` resolve the label through the factory to a signer. Serializing concurrent broadcasts of one live account (which would collide on the EVM nonce / Cosmos account sequence) is handled by a **process-global** locker (`core::wallet_lock`) keyed by `(chain kind, chain id, address)`, acquired only on the RPC path and held for the whole build, sign, broadcast, confirm sequence. It uses a `tokio::sync::Mutex` owned guard (an async mutex is mandatory: a `std` mutex held across an `.await` would deadlock the single-thread runtime) and lives in a global registry, so the same account serializes across the separate per-test runtimes where a per-factory lock could not. Mock backends take no lock (each test has an isolated in-process chain, no shared nonce); different accounts and different chains proceed in parallel. One `Rc<WalletFactory>` is shared by the whole simulation: the caller builds it with `from_roster`, passes it to `MultiChainEnv::new(label, wallets)`, and clones it into every chain it injects (`OSMOSIS.mock(wallets.clone())`), so the env and all chains resolve labels through the same factory.

### Per VM mapping

| Concern | CosmWasm (`cw-multi-test`) | EVM (`revm`) | Solana (`litesvm`) | Tron (`revm` core + TVM layers) |
| --- | --- | --- | --- | --- |
| Backend | `App` with `MockApiBech32` | `MainnetEvm` over `InMemoryDB` | `LiteSVM` | `revm` core with Tron precompiles and a resource shim |
| Address | `Addr` (bech32, chain prefix) | `Address` (20 bytes) | `Address` (pubkey) | `TronAddress` (base58check, `0x41` prefix; inner 20 bytes = EVM address) |
| Upload/deploy | `store_code` | `deploy_create` (create tx) | `add_program` | `deploy_create` (revm `CREATE`) |
| Mutate | `instantiate` / `execute_contract` | `call` (`transact_commit`) | `send_transaction` | `call` (`transact_commit`) |
| Read | `query_wasm_smart` | `static_call` (`transact`, no commit) | `get_account` | `static_call` (`transact`, no commit) |
| Balance | bank `init_balance` / `query_balance` | `AccountInfo.balance` | `airdrop` / `get_balance` | u64 sun (1 TRX = 1,000,000 sun) |
| Msg types | JSON serde (`CwSerde`) | `AsRef<[u8]>` calldata | `AsRef<[Instruction]>` | `AsRef<[u8]>` calldata (EVM-shaped) |

Notes on specific choices:

* The EVM mock holds the `revm` instance in a `RefCell` so read-only `static_call` (which `revm` implements through a `&mut` static call) can run behind `&self`. Static calls use `transact` (no commit) so they leave no state behind. Nonce checking is disabled and transactions are sent as legacy (no chain id) to keep a test harness free of nonce and EIP-155 bookkeeping.
* The Solana mock signs with the wallet's keypair, supplied by the factory (the chain resolves a label to an `SvmSigner` and hands the mock the `Keypair`). `new_account` still mints a funded throwaway pubkey for balance and read scenarios, but it no longer retains keys, since sending now goes through wallet labels. Block height is tracked alongside `warp_to_slot`.
* The CosmWasm mock configures `MockApiBech32` with the chain's bech32 prefix, so generated addresses are realistic (for example `osmo1...`).

### Tron (`revm` core with TVM layers)

Tron is the fourth ecosystem, behind the same `ChainProvider` trait. `TronChain` is a backend enum (`Mock(TronMockProvider)` or `Rpc(TronRpcProvider)`), mirroring the EVM crate, because the TVM is an EVM derivative: the mock reuses a `revm` core and adds the layers where Tron diverges from Ethereum.

* Address model. `TronAddress` is base58check with the `0x41` version prefix over a secp256k1 key (Tron uses secp256k1, not ed25519). The inner 20 bytes are exactly the matching EVM address, so the same key yields a Tron address and an EVM address that share their raw bytes.
* Precompiles. The Tron precompile set is injected into revm: the TIP-272 relocations (`ripemd160` at `0x20003`, `blake2f` at `0x20009`) plus `validatemultisign` at `0x0a`, all secp256k1-based.
* Resource model. Energy and bandwidth are tracked by a provider-layer accounting shim that sits outside revm's gas loop, and balances are u64 sun (1 TRX = 1,000,000 sun). The shim is coarse account-level accounting; per-opcode energy costs are not modeled.
* Events. Tron logs are EVM-shaped (address, topics, data), so the mock surfaces revm logs directly.
* Wallets. secp256k1 at SLIP-44 coin type 195, path `m/44'/195'/<index>'/0/0`.

Known divergences (honest v1 limits):

* `CREATE` / `CREATE2` use revm's EVM address derivation, not Tron's tx-id-based formula. The real formula is available as the pure functions `tron_create_address` / `tron_create2_address` for tooling, but revm 41 does not allow cleanly overriding the in-VM derivation, so a contract address minted inside the mock follows the EVM rule.
* The RPC backend (`TronRpcProvider`) drives a live java-tron node over the TronGrid HTTP REST API (`/wallet/*` endpoints, via `reqwest`). Real reads (`balance`, `block_height`, `static_call`) and signed writes (`deploy_create` and `call` build the unsigned transaction at the node, sign its `txID` locally, then broadcast) are in place; only `set_balance` returns `Unimplemented`, since a live chain cannot mint. After broadcast, `call` polls `gettransactioninfobyid` until the transaction is mined (erroring on a poll timeout or an on-chain failure), then returns its return data and EVM-shaped logs. Range and topic log search (`eth_getLogs`, TronGrid `/v1/contracts/{addr}/events`) remains a later enhancement.

### Cross-VM contract layer (`cross-vm-framework`)

The `contract` module lets a developer wrap a contract once and run one test across all four VMs (for example an rstest over `#[values(ChainKind::CosmWasm, ChainKind::Evm, ChainKind::Svm, ChainKind::Tron)]` that builds the matching `.mock(wallets)` per case). The framework stays free of any message encoding; the developer owns the per-VM encoding in native typed code. Pieces:

* `Account`: a VM-agnostic address (a signer, or a deployed contract address). Per-VM hooks recover the native type with `cw()` / `evm()` / `svm()`, which return `CrossVmError::WrongVm` on a mismatch. `AnyChain::new_account` returns one.
* `ContractBase`: the shared chain handle plus the deployed address (behind a `RefCell`, so a `&self` `setup` can record it). Provides typed chain accessors (`cosmwasm()`, `evm()`, `solana()`) and address getters (`cw_addr()`, `evm_addr()`, `svm_addr()`).
* `AppResponse<T>`: the uniform return envelope, carrying a typed payload `T` plus the raw per-VM result. Common accessors (`transaction_hash`, `gas_used`) are fallible. VM-specific accessors error on a VM mismatch: the raw result (`raw_cosmwasm`, `raw_evm`, `raw_solana`) and the emitted events, whose shapes do not unify (`raw_cosmwasm_events` returns typed `Event`s, `raw_evm_logs` returns ABI `Log`s, `raw_solana_logs` returns program log lines). The EVM raw result carries both the return data and the logs (`RawResponse::Evm { output, logs }`), since revm reports them together.
* `Hooks`: per-contract before/after callbacks on `ContractBase`. A wrapper registers them (`on_before` / `on_after`) and fires them (`run_before` / `run_after`) around the per-VM execution. An after-hook observes the uniform `AppResponse` (and the per-VM event accessors above), so side-logic (indexer, bridge, listener) reacts to a transaction, matching on `kind()` only where the event shapes differ. Hooks are synchronous `FnMut`; both kinds can return `Err` to abort (before stops the tx, after fails the method).

A contract wrapper holds a `ContractBase` and writes one dispatcher per logical method that matches `kind()` and calls the matching `cw_*` / `evm_*` / `svm_*` / `tron_*` hook (see `examples/cross-vm-tests/tests/support/counter.rs`). Design decisions behind this shape:

1. Keep the `AnyChain` enum rather than a trait object: contract methods are generic and async, so they are not object safe; an enum is the only single, sized, runtime-selected type that can hold any backend and still expose generic methods.
2. One wrapper with per-VM hooks, not three separate VM traits: the developer owns each VM's native encoding, and an unsupported VM falls through to a `CrossVmError::Unimplemented` arm rather than a missing impl.
3. The contract owns its chain handle (`Contract::new(chain)` / `Contract::instance(chain, addr)`), so methods drop the chain parameter and the deployed address lives beside the chain.
4. Owning the handle forces cheap-clone shared state (`Rc<RefCell<_>>`), which also makes the contract API `&self`.
5. `AppResponse<T>` keeps two failure modes distinct: `WrongVm` (wrong accessor) versus `Unsupported` (right VM, the backend lacks the datum, for example a transaction hash on `cw-multi-test`).
6. The scaffolding macro that would generate the hooks plus dispatcher is deferred until the hand-written pattern is proven. The macro would also emit the `run_before` / `run_after` transaction-hook calls that bracket the dispatch.
7. Transaction hooks fire at the framework convergence point (`AppResponse`), not in the per-VM provider methods. Those have three incompatible signatures and no shared response; the dispatcher is the one seam where every VM collapses into a single envelope a hook can read.

The example wrapper covers all three VMs: an in-process CosmWasm counter (`ContractWrapper`), a Solidity `Counter` (committed creation bytecode, `alloy::sol!`), and an Anchor counter loaded at its `declare_id!` (built by `make compile-solana`, instructions built from the 8-byte discriminators and the PDA seeds).

### Property-testing harness (`cross-vm-framework`)

The `harness` module drives a contract wrapper over many generated operation sequences. It is VM agnostic: it runs over whatever chain the test injects, so the same property is checked on CosmWasm, EVM, Solana, or Tron. A developer implements one `Harness` trait, with associated types `World` (persisted bookkeeping / a model), `Operation`, `Invariant`, and `OpKind` (the data free operation kinds), plus `apply` (run one operation against the env and model), `check` (evaluate one invariant), and `generate_op(rng, world, kind)` (build a random instance of one kind). The runner picks each kind by weight and calls `generate_op`. A provided `weight(ctx, world, kind)` returns 1 by default (a uniform mix); a harness overrides it to bias the mix dynamically, and a weight of 0 excludes a kind for as long as the current state makes it meaningless. Config supplied static weights multiply the dynamic weight.

The harness itself does not build the environment. Each test builds its own `(Ctx, World)` (deploy, prime the model, set up preconditions) and loads it into a mode typed runner with `r.setup(ctx, world)`. One `Runner<H, Mode>` exposes only the driver its mode needs, via the `RunMode` typestate (`Fuzz`, `Invariant`, `Endurance`, `Scenario`):

* `FuzzRunner` runs one short random sequence per case, drawing from all kinds or a restricted subset.
* `InvariantRunner` runs one long persisted sequence, checking invariants along the way.
* `EnduranceRunner` runs random ops at random wall clock delays with block progression, then a final sweep.
* `ScenarioRunner` runs one concrete op or sequence (rstest matrices), and `replay(history)` re runs a recorded failing sequence deterministically.

The fuzz, invariant, and endurance runs are attribute macros (`#[fuzz_runner]`, `#[invariant_runner]`, `#[endurance_runner]`) that inject a seeded, mode typed runner shell into a `#[runner]` argument; the developer writes setup, the `run(..)` call, and the asserts in the body. `#[fuzz_runner]` fans out into one `#[tokio::test]` per case (case `i` seeded by `sub_seed(seed, i)`, so a flagged case re-runs by name); the others emit one test each. A negative seed picks a fresh random seed per run and prints it for reproducibility. Invariants whose precondition has not happened yet return `CheckOutcome::Skipped` rather than failing.

The config driven CLI (`docs/config-runs-spec.md`) layers a pipeline shape over the same harness: `[suite.<name>]` can declare `[[suite.<name>.phases]]`, an ordered list of profiles where a later phase names an earlier one in `needs` (skipped unless the dependency passed) and, with `world = "inherit"`, continues from the exact `(Ctx, World)` that donor phase ended with rather than a fresh setup.

```toml
[[suite.progressive.phases]]
profile = "mixed-after-deposits"
needs = ["deposit-soak"]
world = "inherit"
```

See `docs/config-runs-spec.md` section 4.7 for the full phase schema and its structural rules.

### Predefined chains

Each VM crate defines its own `ChainInfo` struct (with VM specific fields) implementing `ChainSpec`, plus constants in its `chains` module. The two construction styles are equivalent:

```rust
let chain = OSMOSIS.mock(wallets);             // sugar
let chain = CwMockProvider::new(OSMOSIS, wallets);
```

Both `.mock(wallets)` and `.rpc(wallets)` take the shared `Rc<WalletFactory>`; the RPC endpoint comes from the chain preset, not a separate argument. All four RPC providers serve live read paths. The CosmWasm provider (`OSMOSIS_TESTNET.rpc(wallets)`) goes over Tendermint RPC via `cosmrs`: block height, native balance, and `query_wasm_smart` (ABCI queries). The EVM provider (`SEPOLIA.rpc(wallets)`) goes over JSON-RPC via the alloy HTTP provider: block number, native balance, and `static_call` (`eth_call`). The Solana provider (`SOLANA_DEVNET.rpc(wallets)`) goes over JSON-RPC via a thin `reqwest` client: slot, lamport balance, and `get_account` (`getAccountInfo`). EVM and CosmWasm RPC write paths now sign with the wallet signer and broadcast (`deploy_create`/`call`; `store_code_wasm`/`instantiate`/`execute_contract`, where RPC deploy takes compiled wasm bytes because the trait-object `store_code` is `cw-multi-test` only), each acquiring the global `(chain, address)` broadcast lock first. Solana RPC writes remain compiling stubs that return `Unimplemented` (signer plumbed through, return types decoupled in a follow-up). The Tron provider (`TRON_NILE.rpc(wallets)`) goes over TronGrid HTTP via `reqwest`: block height, native balance, `static_call` (`triggerconstantcontract`), and signed `deploy_create` / `call` (`deploycontract` / `triggersmartcontract`) that sign the `txID` and broadcast (see the Tron section). Tron presets are `TRON_MAINNET`, `TRON_NILE`, `TRON_SHASTA`, and `TRON_LOCAL`. `set_balance` stays `Unimplemented` on every RPC backend since a live chain cannot mint.

## Out of scope (later phases)

The Solana RPC write paths (signed `add_program`/`send_transaction`, blocked on decoupling their mock-backend return types); the mock's tx-id-based `CREATE` / `CREATE2` derivation (Tron); the cross VM orchestration layer that runs one script across all four (its first piece, declarative TOML driven test runs, is now specified in [docs/config-runs-spec.md](docs/config-runs-spec.md)); gas/compute reporting; fork from live.
