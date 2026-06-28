# cross-vm-testing

A Rust testing suite for cross VM work spanning three execution environments: CosmWasm (via `cw-multi-test`), EVM/Solidity (via `revm`), and Solana (via `litesvm`).

Phase 1 ships one chain provider per ecosystem. A chain provider is the analogue of alloy's `Provider`, cw-orch's `CwEnv`, or test-tube's `Runner`. Each provider wraps an in process VM ("mock") behind a single shared trait, so test code and cross VM scripts read the same regardless of target chain.

## Workspace layout

```
crates/
  core/       cross-vm-core      shared ChainProvider / ChainSpec traits, ChainKind, CrossVmError
  cosmwasm/   cross-vm-cosmwasm  CwMockProvider (cw-multi-test), CwRpcProvider (stub)
  solidity/   cross-vm-solidity  EvmMockProvider (revm), EvmRpcProvider (stub)
  solana/     cross-vm-solana    SvmMockProvider (litesvm), SvmRpcProvider (stub)
```

Each VM crate carries a `chains` module with predefined chain constants used to spin up a provider quickly.

## Quickstart

```rust
use cross_vm_core::ChainProvider;
use cross_vm_cosmwasm::chains::OSMOSIS;

let mut chain = OSMOSIS.mock();          // or CwMockProvider::new(OSMOSIS)
let alice = chain.new_account("alice");  // created and funded
assert!(chain.balance(&alice).unwrap() > 0);
```

The same shape works for `cross_vm_solidity::chains::ETHEREUM` and `cross_vm_solana::chains::SOLANA_DEVNET`.

Predefined chains include `OSMOSIS`, `JUNO`, `NEUTRON`, `COSMOS_HUB` (CosmWasm); `ETHEREUM`, `ARBITRUM`, `OPTIMISM`, `BASE`, `POLYGON` (EVM); `SOLANA_MAINNET`, `SOLANA_DEVNET`, `SOLANA_TESTNET`, `SOLANA_LOCALNET` (Solana).

## Build and test

```
cargo build --workspace
cargo test  --workspace
cargo run -p cross-vm-cosmwasm --example cosmwasm_quickstart
cargo run -p cross-vm-solidity --example evm_quickstart
cargo run -p cross-vm-solana   --example solana_quickstart
```

See `SPEC.md` for the architecture, `DEVELOPER.md` for per crate details, and `CHANGELOG.md` for release notes.

## Status

Phase 1 builds the mock providers and scaffolds the RPC providers (every RPC operation returns a clear `Unimplemented` error). Live RPC, the cross VM orchestration layer, and fuzz/invariant harnesses are planned for later phases.
