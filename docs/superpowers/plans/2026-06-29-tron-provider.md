# Tron Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Tron as a fourth ecosystem to cross-vm-testing: a standalone `crates/tron` crate with a deterministic revm-based Mock backend (Tron-accurate addresses, precompiles, and an energy/bandwidth accounting shim) and a stub-parity java-tron RPC backend, behind the shared `ChainProvider` trait.

**Architecture:** Mirror the `crates/solidity` (EVM) crate. `TronChain = Mock(TronMockProvider) | Rpc(TronRpcProvider)`. The Mock reuses a copied revm wiring and layers Tron semantics on top; the RPC backend mirrors `EvmRpcProvider`'s shape with writes returning `Unimplemented` (no in-process java-tron, no alloy-equivalent client in v1). A final phase wires `ChainKind::Tron` through the framework and the `#[cross_vm_contract]` macro.

**Tech Stack:** Rust 2021, `revm` (same version as `crates/solidity`), `alloy-primitives`, `alloy-signer-local` (secp256k1, mnemonic), `bs58`, `sha2`, `sha3`, `k256`, `thiserror`, `tokio`.

**Reference spec:** `docs/superpowers/specs/2026-06-29-tron-chain-support-design.md` (read it; the citation table there must be honored in code comments).

## Global Constraints

- Tron uses **secp256k1 ECDSA**, the same curve as Ethereum. NOT ed25519. `ecrecover` is kept.
- `type Balance = u64` (sun; 1 TRX = 1_000_000 sun). `type Address = TronAddress`. `type Account` = the secp256k1 signer (`PrivateKeySigner`).
- SLIP-44 coin type **195**; derivation path `m/44'/195'/<index>'/0/0` (EVM shape, via `cross_vm_core::bip44_account_path`).
- **No dependency on `tronic` or `tronz`.** They are inspiration only. Use generic crates (`bs58`, `sha2`, `sha3`, `k256`).
- Every Tron-specific mock implementation (base58check, CREATE/CREATE2, precompiles, resources, opcode overrides) MUST carry a code comment linking the authoritative source from the spec's "Source citations for code comments" table.
- **Verification scope:** Phases 0-4 verify with `cargo test -p cross-vm-tron`. Adding `ChainKind::Tron` (Phase 0) knowingly breaks `cross-vm-framework` and the macro-using `examples`; the **full workspace (`cargo build`) is expected red from Phase 0 until Phase 5 closes it.** Do not "fix" framework red before Phase 5.
- Follow existing crate conventions: module-level `//!` docs, `thiserror` errors, `Rc<RefCell<_>>` shared state, `async fn` provider methods, doc examples where the EVM crate has them.
- Commit after every task. Conventional Commits. No mention of tooling/assistants in messages.

---

## File Structure

```
crates/tron/
  Cargo.toml                     deps; workspace member
  src/
    lib.rs                       module decls + pub re-exports (mirror solidity/lib.rs)
    error.rs                     TronError + From<->CrossVmError (mirror solidity/error.rs)
    asset.rs                     TronAsset { Native, Trc20(TronAddress) }
    chain.rs                     TronChain enum, ChainProvider impl, ensure_asset, acquire
    wallet.rs                    WalletDeriver impl on TronChain (coin 195)
    chains/
      mod.rs
      info.rs                    TronChainInfo (ChainSpec)
      presets.rs                 MAINNET / NILE / SHASTA / LOCAL
      sugar.rs                   TronChainInfo::mock() / .rpc()
    provider/
      mod.rs                     re-exports
      address.rs                 TronAddress newtype + base58check + key->address
      mock.rs                    TronMockProvider (revm + Tron layers)
      rpc.rs                     TronRpcProvider (stub parity)
    tvm/
      mod.rs
      create.rs                  CREATE / CREATE2 derivation (pure)
      precompiles.rs             validatemultisign (0x0a) + offset precompiles
      resources.rs               ResourceTracker (energy + bandwidth shim)
```

Workspace-integration edits in Phase 5 (no new files): `crates/core/src/chain_kind.rs` (done in Phase 0), `crates/framework/{Cargo.toml,src/lib.rs,src/any_chain.rs,src/contract/{account,response,base}.rs,src/fund/{fund_target,pending}.rs,src/env/multi_chain_env.rs,src/prelude.rs}`, `crates/macros/src/lib.rs`.

---

## Phase Overview (parallelism map)

| Phase | Tasks | Parallel? | Depends on | Verify |
|-------|-------|-----------|-----------|--------|
| 0 Foundation | T0 | no (1 agent) | — | `cargo build -p cross-vm-tron` |
| 1 Pure leaves | T1A address, T1B create, T1C chains-info | **3 parallel** | T0 | `cargo test -p cross-vm-tron <mod>` |
| 2 Crypto/resource | T2A precompiles, T2B resources | **2 parallel** | T1A | `cargo test -p cross-vm-tron <mod>` |
| 3 Providers | T3A mock, T3B rpc | **2 parallel** | T1, T2 | `cargo test -p cross-vm-tron` |
| 4 Crate integration | T4 | no (1 agent) | T3 | `cargo test -p cross-vm-tron` |
| 5 Workspace integration | T5 (macro + framework can be 2 parallel, then gate) | partial | T4 | `cargo build` + `cargo test` (whole workspace) |
| 6 End-to-end | T6 | no (1 agent) | T5 | `cargo test -p cross-vm-framework tron_e2e` |

Dispatch agents per phase; barrier between phases.

---

## Phase 0 — Foundation

### Task 0: Crate scaffold, ChainKind::Tron, error type

**Files:**
- Modify: `crates/core/src/chain_kind.rs` (add variant + Display arm + test)
- Modify: `Cargo.toml` (root, add `crates/tron` to `members`)
- Create: `crates/tron/Cargo.toml`
- Create: `crates/tron/src/lib.rs`
- Create: `crates/tron/src/error.rs`

**Interfaces:**
- Produces: `cross_vm_core::ChainKind::Tron`; `cross_vm_tron::TronError` with variants `Deploy/Execute/Query/Balance/Rpc/Unimplemented/Wallet(String)` and `From<TronError> for CrossVmError` / `From<CrossVmError> for TronError`.

- [ ] **Step 1: Add the failing core test**

In `crates/core/src/chain_kind.rs` add to the existing test module:
```rust
    #[test]
    fn tron_displays() {
        assert_eq!(ChainKind::Tron.to_string(), "tron");
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p cross-vm-core tron_displays`
Expected: FAIL (no variant `Tron`).

- [ ] **Step 3: Add the variant + Display arm**

In the `enum ChainKind { … }` add after `Svm,`:
```rust
    /// Tron chains driven by a revm-based mock (TVM) or a java-tron RPC backend.
    Tron,
```
In the `Display` match add:
```rust
            ChainKind::Tron => "tron",
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p cross-vm-core`
Expected: PASS.

- [ ] **Step 5: Scaffold the crate**

`crates/tron/Cargo.toml`:
```toml
[package]
name = "cross-vm-tron"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "Tron (TVM) chain provider for the cross-vm testing suite"

[dependencies]
cross-vm-core.workspace = true
revm.workspace = true
alloy-primitives.workspace = true
alloy.workspace = true
alloy-signer-local = { version = "1", features = ["mnemonic"] }
thiserror.workspace = true
tokio.workspace = true
# Tron address codec + hashing (generic crates; NOT a Tron-specific dependency).
bs58 = "0.5"
sha2 = "0.10"
sha3 = "0.10"
k256 = "0.13"

[dev-dependencies]
tokio.workspace = true
cross-vm-macros.workspace = true
```

Root `Cargo.toml`: add `"crates/tron"` to `[workspace] members`.

`crates/tron/src/lib.rs` (modules filled in later phases; declare them now as empty files or `mod` stubs as each lands — for Phase 0 only `error` exists):
```rust
//! Tron (TVM) chain provider for the cross-vm testing suite.
//!
//! Mock backend: revm with Tron-accurate addresses, precompiles, and an energy/bandwidth
//! accounting shim. RPC backend: java-tron stub parity (writes unimplemented in v1).

mod error;

pub use error::TronError;
```

- [ ] **Step 6: Implement error.rs**

`crates/tron/src/error.rs` — mirror `crates/solidity/src/error.rs` exactly, replacing `Evm`→`Tron`, doc "EVM"→"Tron", and `let kind = ChainKind::Tron;`. Keep all seven variants and both `From` impls.

- [ ] **Step 7: Verify crate builds**

Run: `cargo build -p cross-vm-tron`
Expected: PASS (warnings about unused are fine).
Run: `cargo build 2>&1 | tail -5` — EXPECTED RED on `cross-vm-framework`/examples (the known window). Confirm the errors are only "non-exhaustive ChainKind match" / missing-variant, nothing else.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/chain_kind.rs Cargo.toml crates/tron
git commit -m "feat(tron): scaffold crate, add ChainKind::Tron and TronError"
```

---

## Phase 1 — Pure leaf modules (3 parallel agents)

### Task 1A: TronAddress + base58check

**Files:**
- Create: `crates/tron/src/provider/address.rs`
- Create: `crates/tron/src/provider/mod.rs` (add `pub mod address;` + re-export; later tasks extend it)
- Modify: `crates/tron/src/lib.rs` (add `mod provider;` + `pub use provider::address::TronAddress;`)

**Interfaces:**
- Produces:
  - `pub struct TronAddress([u8; 21]);` (byte 0 = `0x41`)
  - `impl TronAddress { pub fn from_evm(a: alloy_primitives::Address) -> Self; pub fn as_evm(&self) -> alloy_primitives::Address; pub fn to_base58(&self) -> String; pub fn from_base58(s: &str) -> Result<Self, TronError>; pub fn to_hex(&self) -> String; }`
  - `impl Display for TronAddress` (base58), `impl FromStr`, `Clone, Copy, PartialEq, Eq, Hash, Debug`.
  - `pub fn address_from_pubkey(uncompressed_pubkey: &[u8]) -> TronAddress` (keccak of the 64-byte key body, low 20 bytes, `0x41` prefix).
  - `pub(crate) fn address_from_label(label: &str) -> TronAddress` (deterministic test address; keccak(label) low 20 + `0x41`).

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Known vector: 0x41 + 20 zero bytes -> "T9yD14Nj9j7xAB4dbGeiX9h8unkKHxuWwb"
    // Source: https://developers.tron.network/docs/account (base58check = addr || sha256(sha256(addr))[..4])
    #[test]
    fn base58_roundtrip_zero() {
        let a = TronAddress::from_evm(alloy_primitives::Address::ZERO);
        let s = a.to_base58();
        assert!(s.starts_with('T'));
        assert_eq!(TronAddress::from_base58(&s).unwrap(), a);
    }

    #[test]
    fn evm_roundtrip_preserves_low_20() {
        let evm = alloy_primitives::address!("dac17f958d2ee523a2206206994597c13d831ec7");
        let t = TronAddress::from_evm(evm);
        assert_eq!(t.as_evm(), evm);
        assert_eq!(&t.to_hex()[..2], "41");
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut s = TronAddress::from_evm(alloy_primitives::Address::ZERO).to_base58();
        s.pop();
        s.push('x');
        assert!(TronAddress::from_base58(&s).is_err());
    }
}
```

- [ ] **Step 2: Run, verify fail** — `cargo test -p cross-vm-tron address::tests` → FAIL (type missing).

- [ ] **Step 3: Implement**

```rust
//! Tron address: 0x41-prefixed 21-byte form, base58check display.
//!
//! Encoding per https://developers.tron.network/docs/account :
//!   addr21 = 0x41 || keccak256(pubkey)[12..32]
//!   base58check = base58( addr21 || sha256(sha256(addr21))[..4] )
//! The inner 20 bytes equal the EVM address, so revm executes on `as_evm()` while every
//! surface shows the Tron form.

use std::fmt;
use std::str::FromStr;

use alloy_primitives::{keccak256, Address};
use sha2::{Digest, Sha256};

use crate::error::TronError;

const TRON_MAINNET_PREFIX: u8 = 0x41;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TronAddress([u8; 21]);

impl TronAddress {
    pub fn from_evm(a: Address) -> Self {
        let mut b = [0u8; 21];
        b[0] = TRON_MAINNET_PREFIX;
        b[1..].copy_from_slice(a.as_slice());
        Self(b)
    }
    pub fn as_evm(&self) -> Address {
        Address::from_slice(&self.0[1..])
    }
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
    pub fn to_base58(&self) -> String {
        let checksum = double_sha256(&self.0);
        let mut buf = self.0.to_vec();
        buf.extend_from_slice(&checksum[..4]);
        bs58::encode(buf).into_string()
    }
    pub fn from_base58(s: &str) -> Result<Self, TronError> {
        let raw = bs58::decode(s)
            .into_vec()
            .map_err(|e| TronError::Wallet(format!("base58: {e}")))?;
        if raw.len() != 25 {
            return Err(TronError::Wallet(format!("address length {}", raw.len())));
        }
        let (body, check) = raw.split_at(21);
        if double_sha256(body)[..4] != check[..4] {
            return Err(TronError::Wallet("address checksum mismatch".into()));
        }
        let mut b = [0u8; 21];
        b.copy_from_slice(body);
        Ok(Self(b))
    }
}

fn double_sha256(bytes: &[u8]) -> [u8; 32] {
    let h1 = Sha256::digest(bytes);
    let h2 = Sha256::digest(h1);
    h2.into()
}

/// secp256k1 uncompressed pubkey (65 bytes incl. 0x04 tag, or 64-byte body) -> Tron address.
/// keccak256 over the 64-byte body, low 20 bytes, 0x41 prefix.
/// Source: https://developers.tron.network/docs/account
pub fn address_from_pubkey(uncompressed_pubkey: &[u8]) -> TronAddress {
    let body = if uncompressed_pubkey.len() == 65 {
        &uncompressed_pubkey[1..]
    } else {
        uncompressed_pubkey
    };
    let h = keccak256(body);
    TronAddress::from_evm(Address::from_slice(&h[12..]))
}

pub(crate) fn address_from_label(label: &str) -> TronAddress {
    let h = keccak256(label.as_bytes());
    TronAddress::from_evm(Address::from_slice(&h[12..]))
}

impl fmt::Display for TronAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base58())
    }
}
impl FromStr for TronAddress {
    type Err = TronError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_base58(s)
    }
}
```

- [ ] **Step 4: Run, verify pass** — `cargo test -p cross-vm-tron address` → PASS. If the zero-address vector string differs, trust the roundtrip assertions (they are self-consistent); the literal in the comment is informational.

- [ ] **Step 5: Commit** — `git commit -am "feat(tron): TronAddress with base58check + key derivation"`

---

### Task 1B: CREATE / CREATE2 derivation

**Files:**
- Create: `crates/tron/src/tvm/create.rs`
- Create: `crates/tron/src/tvm/mod.rs` (`pub mod create;`)
- Modify: `crates/tron/src/lib.rs` (`mod tvm;`)

**Interfaces:**
- Produces:
  - `pub fn tron_create_address(tx_id: [u8; 32], nonce: u64) -> TronAddress`
  - `pub fn tron_create2_address(caller: TronAddress, salt: [u8; 32], init_code: &[u8]) -> TronAddress`

- [ ] **Step 1: Failing tests** — encode the formulas as self-consistent tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::address::TronAddress;
    use alloy_primitives::keccak256;

    // CREATE: 0x41 || keccak256(tx_id || nonce_be?)[12..32]
    // Source: https://github.com/tronprotocol/tips/issues/26
    #[test]
    fn create_matches_formula() {
        let tx = [7u8; 32];
        let got = tron_create_address(tx, 1);
        let mut buf = tx.to_vec();
        buf.extend_from_slice(&1u64.to_be_bytes());
        let want = TronAddress::from_evm(alloy_primitives::Address::from_slice(&keccak256(&buf)[12..]));
        assert_eq!(got, want);
    }

    // CREATE2: 0x41 || keccak256(0x41 || caller20 || salt || keccak256(init))[12..32]
    // Source: https://developers.tron.network/docs/tvm  (0x41 prefix, not 0xff)
    #[test]
    fn create2_uses_0x41_prefix() {
        let caller = TronAddress::from_evm(alloy_primitives::Address::ZERO);
        let got = tron_create2_address(caller, [0u8; 32], b"\x60\x00");
        let mut buf = vec![0x41u8];
        buf.extend_from_slice(caller.as_evm().as_slice());
        buf.extend_from_slice(&[0u8; 32]);
        buf.extend_from_slice(keccak256(b"\x60\x00").as_slice());
        let want = TronAddress::from_evm(alloy_primitives::Address::from_slice(&keccak256(&buf)[12..]));
        assert_eq!(got, want);
    }
}
```

- [ ] **Step 2: Run, verify fail** — `cargo test -p cross-vm-tron create` → FAIL.

- [ ] **Step 3: Implement**

```rust
//! TVM contract-address derivation. Differs from Ethereum: CREATE hashes the tx id and a
//! per-root-call nonce; CREATE2 prefixes 0x41 instead of 0xff.
//! Sources: https://github.com/tronprotocol/tips/issues/26 and https://developers.tron.network/docs/tvm

use alloy_primitives::{keccak256, Address};

use crate::provider::address::TronAddress;

pub fn tron_create_address(tx_id: [u8; 32], nonce: u64) -> TronAddress {
    let mut buf = tx_id.to_vec();
    buf.extend_from_slice(&nonce.to_be_bytes());
    TronAddress::from_evm(Address::from_slice(&keccak256(&buf)[12..]))
}

pub fn tron_create2_address(caller: TronAddress, salt: [u8; 32], init_code: &[u8]) -> TronAddress {
    let mut buf = Vec::with_capacity(1 + 20 + 32 + 32);
    buf.push(0x41);
    buf.extend_from_slice(caller.as_evm().as_slice());
    buf.extend_from_slice(&salt);
    buf.extend_from_slice(keccak256(init_code).as_slice());
    TronAddress::from_evm(Address::from_slice(&keccak256(&buf)[12..]))
}
```

- [ ] **Step 4: Run, verify pass** — `cargo test -p cross-vm-tron create` → PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(tron): TVM CREATE/CREATE2 address derivation"`

> NOTE for Phase 3 (T3A): these are pure derivations. Overriding revm's *internal* CREATE address to use them is a known-hard integration (see T3A Step "CREATE spike"). If the spike fails, the mock keeps revm's EVM address and documents the divergence; these functions still ship for tooling/assertions.

---

### Task 1C: TronChainInfo + presets

**Files:**
- Create: `crates/tron/src/chains/info.rs`, `crates/tron/src/chains/presets.rs`, `crates/tron/src/chains/mod.rs`
- Modify: `crates/tron/src/lib.rs` (`pub mod chains;` + `pub use chains::TronChainInfo;`)

**Interfaces:**
- Produces: `pub struct TronChainInfo { chain_id, name, native_symbol, spec_id: revm::primitives::hardfork::SpecId, rpc_url: Option<&'static str> }` impl `ChainSpec` (`kind()->ChainKind::Tron`), `numeric_id()->u64`. Consts `MAINNET, NILE, SHASTA, LOCAL`.

- [ ] **Step 1: Failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cross_vm_core::{ChainKind, ChainSpec};
    #[test]
    fn mainnet_is_tron() {
        assert_eq!(crate::chains::MAINNET.kind(), ChainKind::Tron);
        assert_eq!(crate::chains::MAINNET.native_symbol(), "TRX");
    }
}
```
- [ ] **Step 2: Run, verify fail** — `cargo test -p cross-vm-tron chains` → FAIL.
- [ ] **Step 3: Implement** — mirror `crates/solidity/src/chains/info.rs` (replace `Evm`→`Tron`, `kind()` returns `ChainKind::Tron`). `presets.rs`:
```rust
//! Predefined Tron chains. Tron contracts run on a TVM that tracks the EVM Cancun feature set
//! closely enough for the mock; spec_id selects the revm hardfork the mock executes against.
use super::info::TronChainInfo;
use revm::primitives::hardfork::SpecId;

pub const MAINNET: TronChainInfo = TronChainInfo {
    chain_id: "728126428", // 0x2b6653dc, Tron mainnet id
    name: "Tron",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: Some("https://api.trongrid.io"),
};
pub const NILE: TronChainInfo = TronChainInfo {
    chain_id: "3448148188",
    name: "Nile",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: Some("https://nile.trongrid.io"),
};
pub const SHASTA: TronChainInfo = TronChainInfo {
    chain_id: "2494104990",
    name: "Shasta",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: Some("https://api.shasta.trongrid.io"),
};
pub const LOCAL: TronChainInfo = TronChainInfo {
    chain_id: "9",
    name: "Tron Local",
    spec_id: SpecId::CANCUN,
    native_symbol: "TRX",
    rpc_url: None,
};
```
`chains/mod.rs`: `mod info; pub mod presets; pub use info::TronChainInfo; pub use presets::{MAINNET, NILE, SHASTA, LOCAL};` (sugar.rs added in T4).
- [ ] **Step 4: Run, verify pass** — `cargo test -p cross-vm-tron chains` → PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): TronChainInfo + chain presets"`

---

## Phase 2 — Crypto / resource modules (2 parallel agents)

### Task 2A: Tron precompiles

**Files:**
- Create: `crates/tron/src/tvm/precompiles.rs`
- Modify: `crates/tron/src/tvm/mod.rs` (`pub mod precompiles;`)

**Interfaces:**
- Produces:
  - `pub fn validate_multisign(content: [u8;32], sigs: &[[u8;65]]) -> Vec<Address>` (recovered signers; secp256k1 ecrecover; max 5).
  - `pub fn tron_precompiles() -> revm::precompile::Precompiles` (mainnet set + relocate ripemd160→0x20003, blake2f→0x20009, add validatemultisign→0x0a). Exact revm API resolved during the task against the pinned revm version; if the relocation API is unavailable, ship `validate_multisign` as a pure function and leave a `// TODO(tron): wire into revm precompile set` with the spec citation, deferring registry wiring to T3A.

**Citations (in code):** validatemultisign https://github.com/tronprotocol/tips/blob/master/tip-60.md ; offsets https://github.com/tronprotocol/tips/blob/master/tip-272.md

- [ ] **Step 1: Failing test** — recover a known signer:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{SigningKey, signature::hazmat::PrehashSigner, RecoveryId, Signature};
    #[test]
    fn recovers_single_signer() {
        let sk = SigningKey::from_bytes(&[1u8; 32].into()).unwrap();
        let content = [9u8; 32];
        let (sig, rec): (Signature, RecoveryId) = sk.sign_prehash(&content).unwrap();
        let mut sig65 = [0u8; 65];
        sig65[..64].copy_from_slice(&sig.to_bytes());
        sig65[64] = rec.to_byte();
        let signers = validate_multisign(content, &[sig65]);
        assert_eq!(signers.len(), 1);
    }
}
```
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** `validate_multisign` via `k256` ecrecover (keccak not needed; content is already a digest), cap at 5 sigs (cite tip-60). Build `tron_precompiles()` from `revm::precompile::Precompiles::cancun().clone()` then insert/relocate; verify the exact constructor names against the pinned revm in this task.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): validatemultisign + offset precompiles"`

---

### Task 2B: Energy / bandwidth ResourceTracker

**Files:**
- Create: `crates/tron/src/tvm/resources.rs`
- Modify: `crates/tron/src/tvm/mod.rs` (`pub mod resources;`)

**Interfaces:**
- Produces:
  - `pub struct ResourceTracker { /* per-address energy + bandwidth */ }`
  - `impl ResourceTracker { pub fn new() -> Self; pub fn freeze_for_energy(&mut self, who: TronAddress, trx_sun: u64); pub fn unfreeze(&mut self, who: TronAddress, trx_sun: u64); pub fn energy(&self, who: &TronAddress) -> u64; pub fn bandwidth(&self, who: &TronAddress) -> u64; pub fn consume_bandwidth(&mut self, who: &TronAddress, tx_bytes: usize) -> bool; }`
  - Consts: `FREE_BANDWIDTH_PER_DAY: u64 = 600`, `SUN_PER_ENERGY: u64 = 100`, `SUN_PER_BANDWIDTH: u64 = 1000`, `SUN_PER_TRX: u64 = 1_000_000`.

**Citation (in code):** https://developers.tron.network/docs/resource-model

- [ ] **Step 1: Failing tests** — freeze grants energy by price; bandwidth deducts by byte size, falls back below zero to `false`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::address::{TronAddress, address_from_label as lbl};
    #[test]
    fn freeze_grants_energy() {
        let mut r = ResourceTracker::new();
        let a = lbl("a");
        r.freeze_for_energy(a, 1_000_000); // 1 TRX
        assert_eq!(r.energy(&a), 1_000_000 / SUN_PER_ENERGY); // 10_000
    }
    #[test]
    fn bandwidth_free_then_exhausts() {
        let mut r = ResourceTracker::new();
        let a = lbl("a");
        assert!(r.consume_bandwidth(&a, 300));
        assert!(r.consume_bandwidth(&a, 300));
        assert!(!r.consume_bandwidth(&a, 1)); // 600 free units used
    }
}
```
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** with a `HashMap<TronAddress, (u64 energy, u64 bandwidth_used)>`; bandwidth budget = `FREE_BANDWIDTH_PER_DAY` for v1 (no daily reset modeled — note in comment). Cite the resource-model URL.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): energy/bandwidth accounting shim"`

---

## Phase 3 — Providers (2 parallel agents)

### Task 3A: TronMockProvider

**Files:**
- Create: `crates/tron/src/provider/mock.rs`
- Modify: `crates/tron/src/provider/mod.rs`

**Interfaces:**
- Consumes: `TronAddress`, `address_from_label`, `tron_precompiles`/`validate_multisign`, `ResourceTracker`, `tron_create_address`.
- Produces (mirror `EvmMockProvider`):
  - `pub type TronInner = MainnetEvm<MainnetContext<InMemoryDB>>;`
  - `pub struct TronMockProvider { evm, info: TronChainInfo, wallets, signers, resources: Rc<RefCell<ResourceTracker>> }`
  - `pub fn new(info, wallets) -> Self`
  - `async fn deploy_create(&self, bytecode: Bytes, ctor: impl AsRef<[u8]>, from: &TronAddress) -> Result<TronAddress, TronError>`
  - `async fn call(&self, to: &TronAddress, calldata, from: &TronAddress) -> Result<TronExecution, TronError>`
  - `async fn static_call(&self, to: &TronAddress, calldata) -> Result<Bytes, TronError>`
  - `pub struct TronExecution { output: Bytes, logs: Vec<Log>, tx_hash: Option<B256> }`
  - `impl ChainProvider` (Address=TronAddress, Balance=u64, Account=TronAddress).
  - Resource surface: `pub fn freeze_for_energy(&self, who: &TronAddress, trx_sun: u64)`, `pub fn energy(&self, who: &TronAddress) -> u64`, `pub fn bandwidth(&self, who: &TronAddress) -> u64`.

- [ ] **Step 1: Failing test** — deploy + read. Use the EVM crate's test contract bytecode pattern (see `crates/solidity/src/tests.rs` for a minimal storage contract) but assert via Tron addresses:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chains::LOCAL;
    use cross_vm_core::{ChainProvider, WalletFactory};
    use std::rc::Rc;

    #[tokio::test]
    async fn new_account_is_funded_and_tron_shaped() {
        let mut c = TronMockProvider::new(LOCAL, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let a = c.new_account("alice").await;
        assert!(a.to_base58().starts_with('T'));
        assert!(c.balance(&a).await.unwrap() > 0);
    }
}
```
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** by mirroring `crates/solidity/src/provider/mock.rs` with these concrete diffs:
  - `type Balance = u64`: store/read native via `U256`, convert at the boundary (`U256::from(amount)` on set; `info.balance.saturating_to::<u64>()` on read). `DEFAULT_FUNDING_SUN: u64 = 10_000 * SUN_PER_TRX` (10k TRX).
  - All `Address` params become `TronAddress`; convert to `alloy` `Address` with `.as_evm()` before building `TxEnv`, wrap results with `TronAddress::from_evm(...)`.
  - On `new`, after `build_mainnet`, install Tron precompiles from T2A (the exact revm hook is resolved here; if T2A could not wire the registry, do it now via the revm handler/`append_handler_register` equivalent for the pinned version).
  - Block-context overrides (cite https://developers.tron.network/v4.4.0/docs/vm-vs-evm): set `ctx.block` so `DIFFICULTY`/`GASLIMIT` read 0; leave GASPRICE/BASEFEE mapping as a documented approximation if revm exposes no direct hook.
  - Hold a `ResourceTracker` in an `Rc<RefCell<_>>`; expose the resource methods. `call` consumes bandwidth by encoded calldata length (approximation; cite resource-model).
- [ ] **Step 3b: CREATE spike (timeboxed)** — attempt to override revm's CREATE result address with `tron_create_address`. Two acceptable outcomes, pick one and record it in a module comment:
  - (a) Wire a custom handler so deploy returns the Tron-derived address; add a test `deploy_address_matches_tron_formula`.
  - (b) If revm's pinned API doesn't allow clean override without forking, keep revm's address, add `// DIVERGENCE(tron): mock CREATE uses revm/EVM derivation; real Tron differs — see tron_create_address(). Source: tips/issues/26` and DO NOT add the matching test. Note the divergence in the plan's Phase 6 docs task.
- [ ] **Step 4: Run, verify pass** — `cargo test -p cross-vm-tron provider::mock` → PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): revm-based TronMockProvider with Tron layers"`

---

### Task 3B: TronRpcProvider (stub parity)

**Files:**
- Create: `crates/tron/src/provider/rpc.rs`
- Modify: `crates/tron/src/provider/mod.rs`

**Interfaces:**
- Produces: `pub struct TronRpcProvider { info, rpc_url, wallets, signers }`, `pub fn new(info, wallets)`, `impl ChainProvider` with `new_account` deriving a `TronAddress` (via `address_from_label` for v1), `balance`/`block_height` returning `0`/inert (real reads deferred), `set_balance`/`advance_blocks` → `Unimplemented`/no-op, and inherent `deploy_create`/`call`/`static_call` returning `TronError::Unimplemented`.

- [ ] **Step 1: Failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chains::NILE;
    use cross_vm_core::{ChainProvider, WalletFactory};
    use std::rc::Rc;
    #[tokio::test]
    async fn set_balance_unimplemented() {
        let mut c = TronRpcProvider::new(NILE, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let a = c.new_account("x").await;
        assert!(c.set_balance(&a, 1).await.is_err());
    }
}
```
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — mirror `crates/solidity/src/provider/rpc.rs` STRUCTURE, but every write path returns `Err(TronError::Unimplemented("tron rpc <op>".into()))`. Module doc cites the event/read endpoints from the spec (`/v1/transactions/{txid}/events`, `eth_getLogs`) as the future write-path design, marked deferred.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): TronRpcProvider stub-parity backend"`

---

## Phase 4 — Crate integration (1 agent)

### Task 4: TronChain enum, wallet, asset, sugar, exports

**Files:**
- Create: `crates/tron/src/chain.rs`, `crates/tron/src/asset.rs`, `crates/tron/src/wallet.rs`, `crates/tron/src/chains/sugar.rs`
- Modify: `crates/tron/src/lib.rs`, `crates/tron/src/chains/mod.rs`

**Interfaces:**
- Consumes: `TronMockProvider`, `TronRpcProvider`, `TronChainInfo`, `TronAddress`, `TronExecution`.
- Produces:
  - `pub enum TronChain { Mock(TronMockProvider), Rpc(TronRpcProvider) }` + `From` impls, `impl ChainProvider`, inherent `deploy_create/call/static_call/ensure_asset/acquire/wallet_address` (mirror `EvmChain`).
  - `pub enum TronAsset { Native, Trc20(TronAddress) }`
  - `impl WalletDeriver for TronChain { type Signer = PrivateKeySigner; const COIN_TYPE = 195; … signer_address derives via address_from_pubkey(signer.credential().verifying_key()...) }`
  - `impl TronChainInfo { pub fn mock(self, wallets) -> TronMockProvider; pub fn rpc(self, wallets) -> TronRpcProvider; }`
  - `lib.rs` re-exports mirroring solidity: `pub use { TronAddress, TronAsset, TronChain, TronChainInfo, TronError, TronExecution, TronMockProvider, TronRpcProvider, chains }`.

- [ ] **Step 1: Failing test** — end-to-end on the crate:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chains::LOCAL;
    use cross_vm_core::{ChainProvider, WalletFactory};
    use std::rc::Rc;
    #[tokio::test]
    async fn mock_chain_funds_account() {
        let mut chain = TronChain::from(LOCAL.mock(Rc::new(WalletFactory::from_roster(&[]).unwrap())));
        let a = chain.new_account("alice").await;
        assert!(chain.balance(&a).await.unwrap() > 0);
    }
}
```
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** each file mirroring its `crates/solidity` sibling (`chain.rs`, `asset.rs` is `TronAsset`, `wallet.rs` with coin 195 + Tron address from the signer's pubkey, `chains/sugar.rs`). For `signer_address`: derive the uncompressed pubkey from `PrivateKeySigner` and call `address_from_pubkey` (cite account doc).
  - `ensure_asset`: Native mints on Mock (`set_balance`), validates on Rpc; `Trc20` mirrors EVM ERC20 `balanceOf` (selector `0x70a08231`) on Mock, `Unimplemented` on Rpc. TRC10 is out of scope (note in comment).
- [ ] **Step 4: Verify whole crate** — `cargo test -p cross-vm-tron` → PASS. `cargo clippy -p cross-vm-tron` clean.
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): TronChain, wallet (coin 195), TronAsset, sugar, exports"`

---

## Phase 5 — Workspace + framework + macro integration (closes the red window)

> This phase has two independent edit groups (macro, framework) that may run as 2 parallel agents, followed by a single compile gate. Each `ChainKind`/enum match site below MUST gain a `Tron` arm.

### Task 5: Wire ChainKind::Tron through framework and macro

**Files:**
- Modify: `crates/macros/src/lib.rs` (dispatch arms ~220-231; add a `#tron` method slot to the generated struct)
- Modify: `crates/framework/Cargo.toml` (+`cross-vm-tron`), `crates/framework/src/lib.rs` (+`pub use cross_vm_tron;`), `crates/framework/src/prelude.rs`
- Modify: `crates/framework/src/any_chain.rs` (`AnyChain::Tron`, arms at 30-32, 42-44, 54-56, 66-68, From macro at ~96-106)
- Modify: `crates/framework/src/contract/account.rs` (`Account::Tron`, arms 33, 57)
- Modify: `crates/framework/src/contract/response.rs` (`RawResponse::Tron`, arms 39, 99, 203)
- Modify: `crates/framework/src/contract/base.rs` (arms 96, 120)
- Modify: `crates/framework/src/fund/fund_target.rs` (61), `crates/framework/src/fund/pending.rs` (103)
- Modify: `crates/framework/src/env/multi_chain_env.rs` (104)

**Interfaces:**
- Consumes: all of `cross_vm_tron`'s public surface.
- Produces: a workspace where `cargo build` and `cargo test` pass with Tron as a first-class chain.

- [ ] **Step 1: Macro group** — in `crates/macros/src/lib.rs` add `ChainKind::Tron => self.#tron(#(#arg_names),*).await{?}` arms mirroring `#evm`, and extend the generated struct/builder to carry a `#tron` method binding the same way `#evm` is bound. Build a macro-using example to confirm generation: `cargo build -p <example-crate>`.
- [ ] **Step 2: Framework group** — add the `Tron` variant + arm to each enum/match listed above, mirroring the `Evm` arm exactly (swap `EvmChain`→`TronChain`, `cross_vm_solidity`→`cross_vm_tron`, `Address`/`U256` types→`TronAddress`/`u64`). Add the framework `Cargo.toml` dep and `lib.rs`/`prelude.rs` re-exports.
- [ ] **Step 3: Compile gate** — `cargo build` (whole workspace) → PASS. Resolve any remaining non-exhaustive matches the compiler points to (there may be sites beyond the grep list).
- [ ] **Step 4: Test gate** — `cargo test` (whole workspace) → PASS. `cargo clippy --workspace` clean.
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): integrate ChainKind::Tron into framework + macro"`

---

## Phase 6 — End-to-end + docs (1 agent)

### Task 6: MultiChainEnv example, harness smoke, README/CHANGELOG

**Files:**
- Create: `crates/tron/examples/tron_quickstart.rs` (mirror `crates/solidity` quickstart if present)
- Create/Modify: a framework integration test `crates/framework/tests/tron_e2e.rs`
- Modify: `README.md` (add Tron to the supported-VMs lines + a Tron note), `CHANGELOG.md`, `SPEC.md` (Tron section), `DEVELOPER.md` (Tron crate layout)

**Interfaces:**
- Consumes: the full stack via `cross_vm_framework::prelude`.

- [ ] **Step 1: Failing e2e test** — drive a Tron mock through `MultiChainEnv` (or the property harness `Fuzz` mode for a few seeded ops) and assert determinism (same seed → same final state hash). Mirror an existing framework test.
- [ ] **Step 2: Run, verify fail** (test references not-yet-written example helpers, or asserts behavior).
- [ ] **Step 3: Implement** the example + test; if the T3A CREATE spike took fallback (b), document the divergence in SPEC.md and README status table.
- [ ] **Step 4: Verify** — `cargo test -p cross-vm-framework tron_e2e` → PASS. `cargo test` (workspace) green.
- [ ] **Step 5: Commit** — `git commit -am "feat(tron): end-to-end example, harness smoke test, docs"`

---

## Self-Review

- **Spec coverage:** standalone crate (T0), both backends (T3A/T3B/T4), semantics-accurate mock — precompiles (T2A), CREATE (T1B+T3A spike), energy shim (T2B), block-context opcodes (T3A) — RPC stub parity (T3B), wallet coin 195 (T4), TRC20 ensure_asset (T4), events (mock logs via T3A `TronExecution`, RPC read paths documented T3B), citations (every Tron-specific task), framework+macro (T5), determinism smoke (T6). TRC10 explicitly out of scope (noted T4). Covered.
- **Placeholder scan:** novel code (address, create, resources, precompile recover) is given in full; mirror-tasks reference exact sibling files + concrete diffs. The two genuinely unknown-until-pinned items (revm precompile-registry API, revm CREATE override) are scoped as in-task spikes with explicit fallbacks, not silent TODOs.
- **Type consistency:** `TronAddress` (Address), `u64` (Balance, sun), `TronExecution` (output/logs/tx_hash), `PrivateKeySigner` (Signer), `TronChainInfo.spec_id: SpecId`, coin 195 — consistent across T1A→T4→T5.
- **Risk flagged:** T3A CREATE override is the one item that may degrade to documented-divergence; the plan makes that an explicit, testable decision rather than a hidden assumption.
