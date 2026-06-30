# Tron chain support: design

Date: 2026-06-29
Status: Design (approved for spec)

## Goal

Add Tron as a fourth ecosystem to cross-vm-testing, behind the same `ChainProvider`
trait that already fronts CosmWasm (`cw-multi-test`), EVM (`revm`), and Solana
(`litesvm`). The same test code, cross-vm contract wrappers, and property-testing
harness should drive Tron without changing shape.

## Background

Tron's TVM is an EVM fork that has diverged. Solidity contracts compile to TVM
bytecode that `revm` can execute for the common case, but Tron differs from Ethereum
on several axes that matter for faithful testing:

* Addresses are `0x41` + 20-byte hash, presented as base58check (`T...`), not raw
  20-byte hex.
* Resource model is energy plus bandwidth (energy comes from frozen TRX, bandwidth
  is a daily free allowance), not a single gas counter.
* Signatures use secp256k1 ECDSA, the same curve as Ethereum (a common point of
  confusion: Tron does NOT use ed25519 for accounts). So `ecrecover` works as-is.
  The real precompile deltas are the TRC-60 `validatemultisign` multisig precompile
  at `0x0a` and the TIP-272 offset precompiles (`ripemd160` at `0x20003`, `blake2f`
  at `0x20009`), plus TRC10 token access via Tron-specific opcodes.
* CREATE / CREATE2 address derivation differs from Ethereum (different hash input and
  a `0x41` prefix instead of `0xff`).
* Token standards: TRC20 (ERC20-shaped, same `balanceOf` selector) plus TRC10
  (native multi-asset, different interface).
* There is no in-process Rust TVM. `java-tron` is a JVM node, so there is no
  `revm` / `litesvm` equivalent to embed. A live backend means driving an external
  `java-tron` devnet or a public testnet (Nile / Shasta) over HTTP / gRPC.

This design came out of a three-way brainstorm. Two positions were explored in
depth:

* **revm + shim (in-process).** Most contract logic runs on `revm` unchanged. Wins
  for basic deploy and call. Leaks on address derivation, Tron precompiles, energy,
  and TRC10. Becomes technical debt the moment a test depends on any of those.
* **RPC-only (external node).** Honest about there being no in-process TVM, but the
  property harness needs deterministic block control (`advance_blocks`) and balance
  mutation (`set_balance`), which a live node cannot give. Endurance and seeded
  replay break on RPC.

The resolution: the repo already runs both models per ecosystem. `EvmChain` is an
enum of `Mock(revm)` and `Rpc(stub)` (`crates/solidity/src/chain.rs:30-36`). Tron
mirrors that. The Mock backend serves the deterministic harness; the RPC backend
serves real-Tron fidelity. The question then narrows to how much TVM fidelity the
Mock owes, which is answered per component below.

## Decisions

| Decision | Choice |
|----------|--------|
| Code organization | New standalone `crates/tron` crate. Copies the `revm` wiring rather than depending on `cross-vm-solidity`, for full isolation and freedom to diverge TVM semantics. |
| Backends | Both: `Mock(revm-based)` and `Rpc(java-tron)`, mirroring the `EvmChain` enum. |
| Mock fidelity | Semantics-accurate: custom Tron precompiles, TVM CREATE / CREATE2 derivation, plus an energy/bandwidth accounting layer. |
| Energy model | Provider-layer accounting shim (outside `revm`'s gas loop), not in-loop metering. Preserves determinism, avoids forking `revm`. |
| RPC scope (v1) | Stub parity with the EVM and Solana RPC backends: reads plus address/wallet derivation; writes, `set_balance`, and `advance_blocks` are `Unimplemented` / no-op. Real broadcast deferred. |

## Architecture

### Crate layout

```
crates/tron/
  src/
    lib.rs
    chain.rs          TronChain enum (Mock | Rpc), ChainProvider impl, ensure_asset
    asset.rs          TronAsset (TRC20 first; TRC10 path noted below)
    error.rs          TronError -> CrossVmError
    wallet.rs         TronDeriver (secp256k1, coin type 195, base58 encoding)
    chains/
      info.rs         TronChainInfo (ChainSpec impl)
      presets.rs      Mainnet, Nile, Shasta presets
      mod.rs
    provider/
      address.rs      TronAddress newtype + base58check encode/decode
      mock.rs         TronMockProvider (revm core + Tron precompiles + CREATE + energy shim)
      rpc.rs          TronRpcProvider (stub parity)
      mod.rs
```

The only changes outside the new crate:

* `crates/core/src/chain_kind.rs`: add `ChainKind::Tron` (Display `"tron"`).
* `crates/framework`: register the crate in the prelude / re-exports, same as the
  other VM crates.
* `crates/macros`: `#[cross_vm_contract]` gains a Tron dispatch arm.

### ChainProvider associated types

```rust
type Spec    = TronChainInfo;
type Address = TronAddress;
type Account = TronSigner;     // secp256k1, mirrors EVM signer
type Balance = u64;            // sun; 1 TRX = 1_000_000 sun
type Error   = TronError;
```

### TronChain enum

```rust
pub enum TronChain {
    Mock(TronMockProvider),   // in-process revm core; full state control
    Rpc(TronRpcProvider),     // live java-tron; phase-1 stub
}
```

`ChainProvider` is implemented on `TronChain` by delegating to the active variant,
exactly as `EvmChain` does. Contract operations (`deploy_create`, `call`,
`static_call`) are idiomatic methods on the chain handle, not trait methods.

## Components

### TronAddress (`provider/address.rs`)

* Newtype over the 21-byte Tron form (`0x41` prefix + 20-byte keccak hash).
* `Display` and `FromStr` use base58check (`T...`).
* `to_hex()` yields the `41...` hex form.
* `as_evm() -> alloy_primitives::Address` exposes the inner 20 bytes for the `revm`
  boundary; `from_evm(Address)` re-wraps with the `0x41` prefix.

This keeps the `revm`-facing representation 20 bytes (so execution is unmodified)
while every surface that a test sees is a correct Tron address. It resolves the
"address type divergence" leak identified in the brainstorm: tests never hold a raw
`alloy` `Address` for a Tron chain.

### TronMockProvider (`provider/mock.rs`)

Deterministic, in-process backend. The property harness runs against this.

1. **Execution.** `revm` over the inner 20-byte address, same core as the EVM mock.
2. **Precompiles.** A custom precompile registry injected into `revm`. `ecrecover`
   is kept (Tron is secp256k1, same as Ethereum). Added / relocated:
   * TRC-60 `validatemultisign` at `0x0a` (max 5 sigs, ECDSA recovery, 1500 energy
     per signature),
   * TIP-272 offset precompiles `ripemd160` at `0x20003` and `blake2f` at `0x20009`,
   * TRC10 token access (`tokenBalance` / `transferToken`), exposed via the
     Tron-specific opcodes `TOKENBALANCE` (0xD1) and `CALLTOKEN` (0xD0) rather than a
     fixed-address contract.
3. **CREATE / CREATE2.** TVM derivation, so a mock-deployed contract gets the address
   real Tron would assign:
   * CREATE: `0x41 || keccak256(tx_hash || nonce)[12..32]`, where nonce is a
     per-root-call sequence starting at 1 (NOT Ethereum's
     `keccak256(rlp(sender, nonce))`).
   * CREATE2: `0x41 || keccak256(0x41 || caller || salt || keccak256(init_code))[12..32]`
     (note `0x41` prefix, not EVM's `0xff`).
   This keeps mock addresses consistent with the RPC backend and with hardcoded
   expectations in tests.
4. **Energy and bandwidth.** A provider-layer accounting shim, deliberately outside
   `revm`'s gas loop:
   * freeze / unfreeze TRX (FreezeBalanceV2 / Stake 2.0) to obtain energy,
   * deduct bandwidth per transaction (bandwidth = transaction size in bytes; 600
     free units per account per day),
   * use constant resource prices (energy 100 sun/unit, bandwidth 1000 sun/unit;
     1 TRX = 1,000,000 sun) rather than the full network-pool distribution,
   * expose energy and bandwidth balances as Tron-crate methods for tests to assert.
   `revm`'s gas engine is left untouched. Determinism for fuzz, invariant, endurance,
   and replay is therefore preserved, because resource accounting does not enter the
   execution loop. This is an approximation: per-opcode energy costs (e.g. SLOAD is
   50 on TVM vs 800 on EVM) are NOT modeled; see Out of scope.
5. **Block-context opcodes.** Adjust `revm` block-context results to TVM semantics:
   `GASPRICE` / `BASEFEE` return the energy price, `DIFFICULTY` and `GASLIMIT` return
   0, `COINBASE` returns the block proposer. The Tron-specific opcodes (`TOKENBALANCE`
   0xD1, `CALLTOKEN` 0xD0, `FREEZE` 0xD5, `DELEGATERESOURCE` 0xDE) are added only as
   needed for TRC10 and resource tests; the rest are out of scope for v1.

### TronRpcProvider (`provider/rpc.rs`)

Stub-parity with `EvmRpcProvider` and the Solana RPC provider for v1.

* `new_account`: derive and return a Tron address (real derivation, no broadcast).
* `balance`, `block_height`: real reads when wired to a node; otherwise inert.
* `set_balance`: `Err(Unimplemented)` ("cannot mint on a real chain").
* `advance_blocks`: no-op ("a real chain advances on its own; tests poll instead").
* Contract broadcast: `Unimplemented` in v1.

Real `java-tron` broadcast (TransferContract, TriggerSmartContract, faucet funding,
block polling) is an explicit later phase, tracked separately. Because writes are
stubbed in v1, the endurance/replay limitations of an RPC backend do not bite yet.

Design shape for that later phase, taken from the reference libraries (as inspiration
only; not dependencies, see Reference implementations):

* Transport gRPC-first via `java-tron` protobuf (`tonic` / `prost`), with an HTTP REST
  fallback (TronGrid / FullNode). Both `tronic` and `tronz` are gRPC-first.
* View reads through `TriggerConstantContract` (returns return-data plus energy
  estimate, no state change).
* Transaction construction via a fluent builder that fills TAPOS reference block,
  expiration, and `fee_limit`, then signs and broadcasts. Skip the libraries' heavier
  features (KMS / keystore, multisig flows, ABI codegen) for v1.

### TronDeriver (`wallet.rs`)

* secp256k1 (same curve as EVM), so it reuses the standard BIP-44 account path
  `bip44_account_path(195, index)` from `crates/core/src/wallet.rs:184`.
* SLIP-44 coin type 195.
* Address encoding: `keccak256(pubkey)[12..]` -> prepend `0x41` -> base58check, where
  the checksum is the first 4 bytes of `sha256(sha256(0x41 || hash))`. Implemented
  with generic crates (`bs58`, `sha2`, `sha3`, `k256`), not a Tron dependency.
* Slots into the existing compile-time wallet roster and per-wallet broadcast lock,
  same as the other ecosystems.

### Assets (`asset.rs`)

* **TRC20** mirrors the EVM ERC20 path. The `balanceOf` selector (`0x70a08231`) is
  identical, so `ensure_asset` follows `crates/solidity/src/chain.rs:170-195`
  closely.
* **TRC10** (native multi-asset) uses a different interface and is not a copy of the
  ERC20 path. v1 provides the TRC20 path; the TRC10 funding path is called out as
  additional work and may land in a follow-up rather than v1.

### Events (`provider/mock.rs`, `provider/rpc.rs`)

Tron contract logs are EVM-shaped. In `TransactionInfo` (returned by
`gettransactioninfobyid` / gRPC `GetTransactionInfoById`, NOT the base `Transaction`)
each `Log` is `{ address, topics[], data }` with `topics[0] = keccak256("Event(types)")`,
identical to an Ethereum receipt. The single divergence: the log `address` is the
20-byte form WITHOUT the `0x41` prefix.

* **Mock.** `revm` already produces EVM logs (`address` / `topics` / `data`). Surface
  them directly; when presenting Tron-style addresses, wrap the 20 bytes back into a
  `TronAddress` (re-add `0x41`). No extra machinery: event assertions in the property
  harness work off the revm logs. Topic hashing matches `keccak256` exactly.
* **RPC (later phase).** Minimal per-tx read path is
  `GET /v1/transactions/{txid}/events` (returns decoded params, no manual ABI decode).
  Range / topic search has three tiers: `eth_getLogs` on the JSON-RPC layer
  (`fromBlock` / `toBlock` / `address` / `topics`; 5000-block range cap), the TronGrid
  Event API `GET /v1/contracts/{addr}/events` (filters `event_name` / `block_number` /
  timestamp / `only_confirmed`, `fingerprint` pagination), or a self-hosted event
  plugin (Mongo / Kafka). A vanilla FullNode has NO native log index. v1 is stub-parity
  so none of this is built yet; it is recorded here for the write-path phase.

`TronChainInfo` implements `ChainSpec` (chain_id, name, native_symbol `TRX`,
optional rpc_url, `kind() -> ChainKind::Tron`), following `EvmChainInfo`. Presets:
Mainnet, Nile, Shasta.

## Harness and macro integration

* `#[cross_vm_contract]` gains a Tron dispatch arm so a single contract wrapper can
  target Tron alongside the other VMs.
* The property runners (fuzz, invariant, endurance, scenario, replay) work unchanged
  on the Mock backend, which satisfies the full `ChainProvider` deterministically
  (including `advance_blocks` and `set_balance`).
* On the RPC backend, the same harness-level constraints that already apply to the
  EVM and Solana RPC stubs apply to Tron: endurance and seeded replay are not
  supported against a live node. In v1 this is academic because RPC writes are
  stubbed.

## Reference implementations

Two existing Rust Tron libraries were studied for the RPC and address work. Neither
is taken as a dependency; they are inspiration and a cross-check on the protocol
facts only.

* `tronic` (https://github.com/39george/tronic): clean `TronAddress` (21-byte, base58
  + hex duality), fluent transaction builders, gRPC provider with energy/bandwidth
  query methods.
* `tronz` (https://github.com/throgxyz/tronz): crate split
  (primitives / signer / provider / contract), gRPC transport with per-call timeouts,
  retries, and endpoint failover, Stake 2.0 support.

Patterns mirrored: base58check via `bs58` + `sha2`, gRPC-first transport, builder for
TAPOS / fee_limit, `TriggerConstantContract` for view reads. Patterns skipped for v1:
KMS / keystore, multisig flows, ABI codegen, governance helpers.

## Source citations for code comments

Each mock component that reproduces real Tron behavior must carry a comment linking
the authoritative source, so a future reader can verify the rule against the protocol.
Required citations:

| Code site | Cite in comment |
|-----------|-----------------|
| `TronAddress` base58check encode/decode | https://developers.tron.network/docs/account |
| Wallet derivation (coin type 195, secp256k1) | https://github.com/tronprotocol/tips/issues/102 |
| CREATE / CREATE2 derivation | https://github.com/tronprotocol/tips/issues/26 and https://developers.tron.network/docs/tvm |
| `validatemultisign` precompile (0x0a) | https://github.com/tronprotocol/tips/blob/master/tip-60.md |
| Offset precompiles (ripemd160 0x20003, blake2f 0x20009) | https://github.com/tronprotocol/tips/blob/master/tip-272.md |
| TRC10 token opcodes / access | https://developers.tron.network/docs/trc10-transfer-in-smart-contracts |
| Energy / bandwidth model and prices | https://developers.tron.network/docs/resource-model |
| Block-context opcode differences (GASPRICE/BASEFEE/DIFFICULTY/GASLIMIT) | https://developers.tron.network/v4.4.0/docs/vm-vs-evm |
| Per-opcode energy table | https://developers.tron.network/docs/opcodes |
| RPC transport / tx builder shape | https://github.com/39george/tronic and https://github.com/throgxyz/tronz |
| Event log shape (TransactionInfo.log) | https://developers.tron.network/docs/event and https://github.com/tronprotocol/java-tron/blob/develop/protocol/src/main/protos/core/Tron.proto |
| Event query APIs (per-tx, per-contract) | https://developers.tron.network/reference/get-events-by-transaction-id and https://developers.tron.network/reference/get-events-by-contract-address |
| eth_getLogs compatibility | https://developers.tron.network/reference/eth_getlogs |

Reference implementation for all of the above is `java-tron`
(https://github.com/tronprotocol/java-tron).

## Testing

* **Mock backend.** No network. Unit tests for: base58check round-trip, CREATE /
  CREATE2 derivation against known Tron vectors, `validatemultisign` precompile
  (secp256k1 ECDSA recovery), energy freeze/unfreeze accounting, bandwidth deduction,
  TRC20 deploy and `balanceOf`.
  Property-harness smoke test (a short fuzz run) to confirm determinism.
* **RPC backend.** Stub-parity tests asserting the `Unimplemented` / no-op contracts,
  mirroring the existing EVM and Solana RPC tests. No live node required in CI.
* **CI.** The Mock backend runs in the existing in-process tiers with no new
  infrastructure, matching the other VMs.

## Scope and risks (honest flags)

1. The custom precompile set is the largest single chunk of new code. Bounded, but
   real, and the correctness-sensitive part of the work.
2. TRC10 native-asset funding is more than the ERC20 `ensure_asset` copy. v1 ships
   TRC20; TRC10 may be deferred.
3. The energy/bandwidth shim is an approximation of Tron's resource model, not a
   per-opcode-accurate meter. It gives tests a resource surface to assert against
   without forking `revm`. If per-opcode energy fidelity is ever required, that is a
   separate, larger effort (in-loop metering) and is explicitly out of scope here.
4. Standalone-crate isolation duplicates the `revm` wiring from `crates/solidity`.
   Accepted cost: two wirings to maintain, in exchange for freedom to diverge TVM
   semantics without risking the EVM path.

## Out of scope (v1)

* Real `java-tron` broadcast / write path on the RPC backend.
* TRC10 native-asset funding (candidate follow-up).
* In-loop per-opcode energy metering.
* Live-node CI.

## Non-goals

This is not an attempt to embed a faithful full TVM in Rust. The Mock backend is a
semantics-accurate approximation for deterministic testing; the RPC backend is the
path to real-Tron fidelity, realized incrementally.
