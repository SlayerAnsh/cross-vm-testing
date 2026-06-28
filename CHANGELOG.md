# Changelog

All notable changes to this project are documented here. The format follows Keep a Changelog, and the project adheres to Semantic Versioning.

## [Unreleased]

### Added (Phase 1: chain providers)

* `cross-vm-core`: shared `ChainProvider` and `ChainSpec` traits, `ChainKind`, and the unified `CrossVmError`.
* `cross-vm-cosmwasm`: `CwMockProvider` over `cw-multi-test` with bech32 addresses, plus the `CwRpcProvider` stub. Predefined chains `OSMOSIS`, `JUNO`, `NEUTRON`, `COSMOS_HUB`, `LOCAL`.
* `cross-vm-solidity`: `EvmMockProvider` over `revm` with an in memory database, plus the `EvmRpcProvider` stub. Predefined chains `ETHEREUM`, `ARBITRUM`, `OPTIMISM`, `BASE`, `POLYGON`, `LOCAL`.
* `cross-vm-solana`: `SvmMockProvider` over `litesvm` with keypair tracking, plus the `SvmRpcProvider` stub. Predefined clusters `SOLANA_MAINNET`, `SOLANA_DEVNET`, `SOLANA_TESTNET`, `SOLANA_LOCALNET`.
* `.mock()` and `.rpc()` construction sugar on every chain info type.
* Unit tests per crate and an end to end integration test per VM (CosmWasm counter, Solidity counter, Solana transfer).
* Quickstart examples for each VM.

### Notes

* RPC providers are scaffolding only; every operation returns `Unimplemented`.
