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
* Precompiles differ: a TRC10 token system contract, ed25519 verification, and Tron
  multisig precompiles (`validatemultisign`, `batchvalidatesign`). No `ecrecover` in
  the Ethereum sense.
* CREATE / CREATE2 address derivation differs from Ethereum.
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
2. **Precompiles.** A custom precompile registry injected into `revm`:
   * ed25519 signature verification (replacing Ethereum `ecrecover` semantics),
   * the TRC10 token system contract at its fixed address,
   * `validatemultisign` and `batchvalidatesign`.
3. **CREATE / CREATE2.** TVM address-derivation formula, so a contract deployed on
   the mock receives the address real Tron would assign. This makes mock addresses
   consistent with the RPC backend and with hardcoded expectations in tests.
4. **Energy and bandwidth.** A provider-layer accounting shim, deliberately outside
   `revm`'s gas loop:
   * freeze / unfreeze TRX to obtain energy,
   * deduct bandwidth per transaction,
   * expose energy and bandwidth balances as Tron-crate methods for tests to assert.
   `revm`'s gas engine is left untouched. Determinism for fuzz, invariant, endurance,
   and replay is therefore preserved, because resource accounting does not enter the
   execution loop.

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

### TronDeriver (`wallet.rs`)

* secp256k1 (same curve as EVM), so it reuses the standard BIP-44 account path
  `bip44_account_path(195, index)` from `crates/core/src/wallet.rs:184`.
* SLIP-44 coin type 195.
* Address encoding: `keccak256(pubkey)[12..]` -> prepend `0x41` -> base58check.
* Slots into the existing compile-time wallet roster and per-wallet broadcast lock,
  same as the other ecosystems.

### Assets (`asset.rs`)

* **TRC20** mirrors the EVM ERC20 path. The `balanceOf` selector (`0x70a08231`) is
  identical, so `ensure_asset` follows `crates/solidity/src/chain.rs:170-195`
  closely.
* **TRC10** (native multi-asset) uses a different interface and is not a copy of the
  ERC20 path. v1 provides the TRC20 path; the TRC10 funding path is called out as
  additional work and may land in a follow-up rather than v1.

### Chain metadata (`chains/`)

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

## Testing

* **Mock backend.** No network. Unit tests for: base58check round-trip, CREATE /
  CREATE2 derivation against known Tron vectors, ed25519 precompile, energy
  freeze/unfreeze accounting, bandwidth deduction, TRC20 deploy and `balanceOf`.
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
