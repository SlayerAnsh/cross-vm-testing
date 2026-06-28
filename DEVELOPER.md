# Developer guide

## Prerequisites

* Rust stable (developed against 1.96, edition 2021).
* For regenerating the EVM test bytecode only: Foundry (`forge`). The committed bytecode means tests run without it.

## Workspace

This is a Cargo workspace with one crate per VM plus a shared core crate. Dependency trees are isolated per crate, so building or testing one VM does not pull the others.

```
crates/core       cross-vm-core       no VM dependencies
crates/cosmwasm   cross-vm-cosmwasm   cw-multi-test, cosmwasm-std
crates/solidity   cross-vm-solidity   revm (alloy for the test bindings)
crates/solana     cross-vm-solana     litesvm, granular solana-* crates
```

## Common commands

```
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets

# one crate at a time
cargo test -p cross-vm-cosmwasm
cargo test -p cross-vm-solidity
cargo test -p cross-vm-solana

# examples
cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart
cargo run -p cross-vm-solidity --example evm_quickstart
cargo run -p cross-vm-solana   --example solana_quickstart
```

## Tests

Each crate has unit tests (chain metadata, account creation, balance set/get, block advance, RPC stub returns an error) in `src/lib.rs`, and an integration test in `tests/`:

* `cross-vm-cosmwasm`: deploys an in test `ContractWrapper` counter, then instantiate, increment, query.
* `cross-vm-solidity`: deploys a Solidity `Counter` (committed creation bytecode), then `setNumber`, `increment`, read via static call. ABI encoding uses alloy's `sol!`.
* `cross-vm-solana`: airdrop, System Program transfer through `execute`, balance assertion.

## Adding a predefined chain

Add a constant to the relevant `chains.rs` (`CosmosChainInfo`, `EvmChainInfo`, or `SolanaChainInfo`). The `ChainSpec` impl and the `.mock()` / `.rpc()` sugar apply automatically.

## Adding a new VM

1. Add a crate under `crates/` and list it in the workspace `members`.
2. Define a `ChainInfo` struct implementing `cross_vm_core::ChainSpec`, with a `chains` module of constants.
3. Implement `cross_vm_core::ChainProvider` for a mock provider and an RPC stub.
4. Convert the provider error into `CrossVmError` via `From`.
5. Mirror the unit and integration test layout above.

## Notes on crate versions

These ecosystems change their public APIs frequently. Pin exact majors and follow the docs of the pinned version:

* `cw-multi-test` 3.x with matching `cosmwasm-std` 3.x. Note `Coin.amount` is `Uint256` in cosmwasm-std 3.
* `revm` 41.x. The builder API (`Context::mainnet().with_db(..).build_mainnet()`, then `transact` / `transact_commit`) has changed repeatedly across majors. Use `revm::primitives` for `Address`, `U256`, and `Bytes` so types match the EVM exactly.
* `litesvm` 0.13 with the granular `solana-*` v4 crates it re-exports. Solana split `solana-sdk` into modular crates; `Pubkey` is an alias of `solana_address::Address`. Depending on the same granular crates litesvm uses keeps types identical at the boundary.

## Regenerating the EVM counter bytecode

```
forge build           # from a project containing the Counter.sol shown in tests/counter.rs
# copy out/Counter.sol/Counter.json -> .bytecode.object into COUNTER_CREATION_BYTECODE
```
