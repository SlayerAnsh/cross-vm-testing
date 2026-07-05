# Multi Native Balance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `denom` argument to `ChainProvider::set_balance` so Cosmos mocks can mint any bank denom (a Cosmos chain holds any number of native denoms), while EVM, Solana, and Tron validate the denom against their single native token symbol. RPC backends stay `Unimplemented`.

**Architecture:** One breaking signature change on the `ChainProvider` trait in `crates/core`, propagated mechanically through every provider (mock and RPC), every `*Chain` wrapper enum, and every call site. Behavior then lands per VM: CosmWasm merges the denom into the account's existing coin list (cw-multi-test's `init_balance` overwrites the whole coin vector, so a read, merge, write cycle is required to avoid wiping other denoms), and the three single token VMs reject any denom that is not their native symbol (case-insensitive).

**Tech Stack:** Rust workspace. `cw-multi-test` (Cosmos mock), `revm` via `cross-vm-revm-common` (EVM and Tron mocks), `litesvm` (Solana mock), `tokio` current-thread tests.

## Global Constraints

- The workspace must compile after every task: `cargo check --workspace --all-targets`.
- Denom rules, fixed by this plan:
  - CosmWasm: `denom` is passed to the bank verbatim (`"uosmo"`, `"uatom"`, `"ibc/..."`, anything). Amount unit is the denom's base unit.
  - EVM: `denom` must equal `chain_info().native_symbol` case-insensitively (`"ETH"`, or `"POL"` on the Polygon preset). Amount stays wei.
  - Solana: `denom` must equal `"SOL"` (the preset's `native_symbol`) case-insensitively. Amount stays lamports.
  - Tron: `denom` must equal `"TRX"` case-insensitively. Amount stays sun.
  - Every RPC provider keeps returning `Unimplemented` (a live chain cannot mint), regardless of denom.
- Denom mismatch on EVM, Solana, and Tron returns the crate's existing `Balance(String)` error variant. No new error variants (YAGNI).
- `cross-vm-revm-common`'s low level `pub fn set_balance(&self, addr: Address, amount: U256)` is NOT part of the `ChainProvider` trait and must NOT change. Denom validation happens in the provider layer above it.
- Argument order is `set_balance(addr, denom, amount)` everywhere.
- Commit messages: Conventional Commits, no tool attribution lines, no co-author trailers.
- Line numbers below are hints from the time of writing. Always locate the code by searching for the quoted snippet, not by line number.
- Doc files (README, SPEC, CHANGELOG) must not use dashes as punctuation. Use periods, commas, or parentheses.

---

### Task 1: Trait signature change plus mechanical propagation

Change the trait, every implementor, and every call site in one commit (a Rust trait change cannot compile piecemeal). No behavior change in this task: CosmWasm mock threads the caller's denom into the bank call it already makes, the other three mocks ignore the denom with a `_denom` parameter (validation lands in Tasks 3, 4, 5), and RPC providers keep erroring.

**Files:**

- Modify: `crates/core/src/chain_provider.rs` (trait method, ~line 44)
- Modify: `crates/cosmwasm/src/provider/mock.rs` (`new_account` ~151, `set_balance` ~168)
- Modify: `crates/cosmwasm/src/provider/rpc.rs` (`set_balance` ~438)
- Modify: `crates/cosmwasm/src/chain.rs` (wrapper `set_balance` ~310)
- Modify: `crates/cosmwasm/src/tests.rs` (~29, ~45)
- Modify: `crates/cosmwasm/tests/vault.rs` (~43)
- Modify: `crates/cosmwasm/examples/cosmwasm_quickstart.rs` (~28)
- Modify: `crates/solana/src/provider/mock.rs` (`set_balance` ~150)
- Modify: `crates/solana/src/provider/rpc.rs` (`set_balance` ~231)
- Modify: `crates/solana/src/chain.rs` (`ensure_asset` ~180, wrapper `set_balance` ~238)
- Modify: `crates/solana/src/tests.rs` (~32, ~48)
- Modify: `crates/solana/tests/transfer.rs` (~30)
- Modify: `crates/solana/examples/solana_quickstart.rs` (~35)
- Modify: `crates/solidity/src/provider/mock.rs` (`new_account` ~154, `set_balance` ~165)
- Modify: `crates/solidity/src/provider/rpc.rs` (`set_balance` ~249)
- Modify: `crates/solidity/src/chain.rs` (`ensure_asset` ~192, wrapper `set_balance` ~255)
- Modify: `crates/solidity/src/tests.rs` (~34, ~50)
- Modify: `crates/solidity/examples/evm_quickstart.rs` (~26)
- Modify: `crates/tron/src/provider/mock.rs` (`new_account` ~214, `set_balance` ~226, test ~264)
- Modify: `crates/tron/src/provider/rpc.rs` (`set_balance` ~405, test ~427)
- Modify: `crates/tron/src/chain.rs` (`ensure_asset` ~201, wrapper `set_balance` ~264)
- Modify: `crates/framework/examples/wallet_quickstart.rs` (~40)
- Modify: `examples/common/src/wallets.rs` (~27, ~41, ~50, ~54, ~58)
- Modify: `examples/cross-vm-tests/tests/cross_vm/setup.rs` (~64)

**Interfaces:**

- Produces: `async fn set_balance(&mut self, addr: &Self::Address, denom: &str, amount: Self::Balance) -> Result<(), Self::Error>` on `ChainProvider`, implemented by `CwMockProvider`, `CwRpcProvider`, `CwChain`, `SvmMockProvider`, `SvmRpcProvider`, `SvmChain`, `EvmMockProvider`, `EvmRpcProvider`, `EvmChain`, `TronMockProvider`, `TronRpcProvider`, `TronChain`. Every later task depends on this exact signature.

- [ ] **Step 1: Change the trait method in `crates/core/src/chain_provider.rs`**

Replace:

```rust
    /// Overwrite an account's native balance (mock-only convenience).
    async fn set_balance(
        &mut self,
        addr: &Self::Address,
        amount: Self::Balance,
    ) -> Result<(), Self::Error>;
```

with:

```rust
    /// Overwrite an account's balance in `denom` (mock-only convenience).
    ///
    /// CosmWasm mocks mint any bank denom verbatim (`"uosmo"`, `"uatom"`, `"ibc/..."`),
    /// leaving the account's other denoms untouched. EVM, Solana, and Tron have exactly
    /// one native token, so `denom` must equal the chain's native symbol,
    /// case-insensitively (`"ETH"`, `"SOL"`, `"TRX"`); `amount` stays in base units
    /// (wei, lamports, sun). RPC backends return `Unimplemented` (a live chain cannot mint).
    async fn set_balance(
        &mut self,
        addr: &Self::Address,
        denom: &str,
        amount: Self::Balance,
    ) -> Result<(), Self::Error>;
```

- [ ] **Step 2: Run `cargo check --workspace --all-targets` and use the error list as the checklist of remaining sites**

Expected: FAIL with `method 'set_balance' has 3 parameters but the declaration in trait ... has 4` and `this method takes 2 arguments but 3 were supplied` style errors across the files listed above.

- [ ] **Step 3: Update the CosmWasm provider crate**

In `crates/cosmwasm/src/provider/mock.rs`, replace `new_account` and `set_balance`:

```rust
    async fn new_account(&mut self, label: &str) -> Addr {
        let addr = label.into_bech32_with_prefix(self.info.bech32_prefix);
        // Best-effort default funding; ignore the (infallible in practice) result.
        let denom = self.info.native_denom;
        let _ = self.set_balance(&addr, denom, DEFAULT_FUNDING).await;
        addr
    }
```

```rust
    async fn set_balance(&mut self, addr: &Addr, denom: &str, amount: u128) -> Result<(), CwError> {
        let addr = addr.clone();
        self.app
            .borrow_mut()
            .init_modules(|router, _api, storage| {
                router
                    .bank
                    .init_balance(storage, &addr, coins(amount, denom))
            })
            .map_err(|e| CwError::Balance(e.to_string()))
    }
```

(The `let denom = self.info.native_denom;` line that used to open `set_balance` is gone; the parameter replaces it. Multi denom merge semantics land in Task 2.)

In `crates/cosmwasm/src/provider/rpc.rs`:

```rust
    async fn set_balance(&mut self, _addr: &Addr, _denom: &str, _amount: u128) -> Result<(), CwError> {
```

(keep the existing body returning `CwError::Unimplemented("rpc set_balance".into())` and any comment above it).

In `crates/cosmwasm/src/chain.rs`, the wrapper:

```rust
    async fn set_balance(&mut self, addr: &Addr, denom: &str, amount: u128) -> Result<(), CwError> {
        match self {
            CwChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            CwChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }
```

CosmWasm call sites:

- `crates/cosmwasm/src/tests.rs` (`LOCAL` preset, native denom `"ustake"`):
  `chain.set_balance(&bob, 42)` becomes `chain.set_balance(&bob, "ustake", 42)`.
- `crates/cosmwasm/src/tests.rs` RPC test (`OSMOSIS` preset):
  `chain.set_balance(&addr, 1)` becomes `chain.set_balance(&addr, "uosmo", 1)`.
- `crates/cosmwasm/tests/vault.rs`: a `let denom = chain.chain_info().native_denom;` binding already exists a few lines up; `.set_balance(&alice, 1_000_000)` becomes `.set_balance(&alice, denom, 1_000_000)`.
- `crates/cosmwasm/examples/cosmwasm_quickstart.rs`: `chain.set_balance(&alice, 5_000_000)` becomes `chain.set_balance(&alice, OSMOSIS.native_denom, 5_000_000)` (`OSMOSIS` is already in scope in that file).

- [ ] **Step 4: Update the Solana crate**

In `crates/solana/src/provider/mock.rs` (denom ignored until Task 4; `new_account` uses `airdrop`, not `set_balance`, so it is unchanged):

```rust
    async fn set_balance(&mut self, addr: &Address, _denom: &str, amount: u64) -> Result<(), SvmError> {
```

(body unchanged).

In `crates/solana/src/provider/rpc.rs`:

```rust
    async fn set_balance(&mut self, _addr: &Address, _denom: &str, _amount: u64) -> Result<(), SvmError> {
```

(body unchanged, still `Unimplemented`).

In `crates/solana/src/chain.rs`, the `ensure_asset` native arm:

```rust
                if current < amount {
                    let denom = p.chain_info().native_symbol;
                    p.set_balance(who, denom, amount)
                        .await
                        .map_err(|e| FundError::Provider(e.to_string()))?;
                }
```

and the wrapper:

```rust
    async fn set_balance(&mut self, addr: &Address, denom: &str, amount: u64) -> Result<(), SvmError> {
        match self {
            SvmChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            SvmChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }
```

Solana call sites, all gain `"SOL"` as the second argument:

- `crates/solana/src/tests.rs`: `chain.set_balance(&bob, "SOL", 12_345)` and `chain.set_balance(&addr, "SOL", 1)`.
- `crates/solana/tests/transfer.rs`: `chain.set_balance(&alice, "SOL", 100_000_000_000)`.
- `crates/solana/examples/solana_quickstart.rs`: `chain.set_balance(&alice, "SOL", 100_000_000_000)`.

- [ ] **Step 5: Update the Solidity (EVM) crate**

In `crates/solidity/src/provider/mock.rs`:

```rust
    async fn new_account(&mut self, label: &str) -> Address {
        let addr = address_from_label(label);
        let denom = self.info.native_symbol;
        let _ = self
            .set_balance(&addr, denom, U256::from(DEFAULT_FUNDING_WEI))
            .await;
        addr
    }
```

```rust
    async fn set_balance(&mut self, addr: &Address, _denom: &str, amount: U256) -> Result<(), EvmError> {
        self.core.set_balance(*addr, amount);
        Ok(())
    }
```

In `crates/solidity/src/provider/rpc.rs`:

```rust
    async fn set_balance(&mut self, _addr: &Address, _denom: &str, _amount: U256) -> Result<(), EvmError> {
```

(body unchanged).

In `crates/solidity/src/chain.rs`, the `ensure_asset` native arm gets the same shape as Solana:

```rust
                if current < amount {
                    let denom = p.chain_info().native_symbol;
                    p.set_balance(who, denom, amount)
                        .await
                        .map_err(|e| FundError::Provider(e.to_string()))?;
                }
```

and the wrapper:

```rust
    async fn set_balance(&mut self, addr: &Address, denom: &str, amount: U256) -> Result<(), EvmError> {
        match self {
            EvmChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            EvmChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }
```

EVM call sites, all gain `"ETH"`:

- `crates/solidity/src/tests.rs`: `chain.set_balance(&bob, "ETH", U256::from(42u64))` and the RPC test `.set_balance(&alloy_primitives::Address::ZERO, "ETH", U256::from(1u64))`.
- `crates/solidity/examples/evm_quickstart.rs`: `.set_balance(&alice, "ETH", U256::from(1_000u64))`.

- [ ] **Step 6: Update the Tron crate**

In `crates/tron/src/provider/mock.rs`, `new_account` (search for `DEFAULT_FUNDING_SUN`):

```rust
        let denom = self.chain_info().native_symbol;
        let _ = self.set_balance(&addr, denom, DEFAULT_FUNDING_SUN).await;
```

and `set_balance`:

```rust
    async fn set_balance(&mut self, addr: &TronAddress, _denom: &str, amount: u64) -> Result<(), TronError> {
```

(body unchanged; it forwards to `self.core.set_balance(addr.as_evm(), U256::from(amount))`).

The in-file test `set_and_read_balance`: `c.set_balance(&a, "TRX", 42 * SUN_PER_TRX)`.

In `crates/tron/src/provider/rpc.rs`:

```rust
    async fn set_balance(&mut self, _addr: &TronAddress, _denom: &str, _amount: u64) -> Result<(), TronError> {
```

(body unchanged, still `Unimplemented`), and its test `set_balance_unimplemented`: `c.set_balance(&a, "TRX", 1)`.

In `crates/tron/src/chain.rs`, the mock `ensure_asset` native arm (the second `TronAsset::Native` arm in the function, the one that mints):

```rust
                if current < amount {
                    let denom = p.chain_info().native_symbol;
                    p.set_balance(who, denom, amount)
                        .await
                        .map_err(|e| FundError::Provider(e.to_string()))?;
                }
```

and the wrapper:

```rust
    async fn set_balance(&mut self, addr: &TronAddress, denom: &str, amount: u64) -> Result<(), TronError> {
        match self {
            TronChain::Mock(p) => p.set_balance(addr, denom, amount).await,
            TronChain::Rpc(p) => p.set_balance(addr, denom, amount).await,
        }
    }
```

- [ ] **Step 7: Update the remaining example and framework call sites**

- `crates/framework/examples/wallet_quickstart.rs`: `.set_balance(&alice, "SOL", 10_000_000_000)`.
- `examples/cross-vm-tests/tests/cross_vm/setup.rs`: `.set_balance(&sol_alice, "SOL", 2_000_000_000u64)`.
- `examples/common/src/wallets.rs`, four arms plus `fund_evm`:

```rust
/// Fund an arbitrary wallet label on an EVM chain with gas money (100 ETH).
pub async fn fund_evm(chain: &mut AnyChain, label: WalletLabel<'_>) {
    if let AnyChain::Evm(c) = chain {
        let a = c.wallet_address(label).await.unwrap();
        c.set_balance(
            &a,
            "ETH",
            cross_vm_solidity::U256::from(10u64).pow(cross_vm_solidity::U256::from(20)),
        )
        .await
        .unwrap();
    }
}
```

and in `fund_user`:

```rust
        AnyChain::Evm(c) => {
            let a = c.wallet_address(label).await.unwrap();
            c.set_balance(
                &a,
                "ETH",
                cross_vm_solidity::U256::from(10u64).pow(cross_vm_solidity::U256::from(20)),
            )
            .await
            .unwrap();
        }
        AnyChain::Svm(c) => {
            let a = c.wallet_address(label).await.unwrap();
            c.set_balance(&a, "SOL", 100_000_000_000).await.unwrap(); // 100 SOL
        }
        AnyChain::CosmWasm(c) => {
            let a = c.wallet_address(label).await.unwrap();
            let denom = c.chain_info().native_denom;
            let _ = c.set_balance(&a, denom, 1_000_000_000_000).await;
        }
        AnyChain::Tron(c) => {
            let a = c.wallet_address(label).await.unwrap();
            c.set_balance(&a, "TRX", 100_000_000_000_000).await.unwrap(); // 100M TRX in sun
        }
```

- [ ] **Step 8: Verify the workspace compiles and behavior is unchanged**

Run: `cargo check --workspace --all-targets`
Expected: clean (0 errors, 0 warnings; unused import or unused variable warnings mean a site was half updated).

Run: `cargo test -p cross-vm-cosmwasm -p cross-vm-solana -p cross-vm-solidity -p cross-vm-tron -p cross-vm-core`
Expected: PASS (all existing tests, same behavior as before the change).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(core)!: add denom arg to ChainProvider::set_balance

Signature-only propagation across all providers, wrappers, and call
sites. Cosmos mock threads the caller's denom into the bank; EVM,
Solana, and Tron ignore it until per-VM validation lands. RPC backends
stay Unimplemented."
```

---

### Task 2: CosmWasm multi denom mint with merge semantics

cw-multi-test's `BankKeeper::init_balance` replaces the account's entire coin vector. After Task 1, `set_balance(&bob, "uatom", 55)` therefore wipes bob's `"ustake"`. This task makes `set_balance` read the account's current coins, merge the one denom, and write the full list back. It also reroutes `CwChain::ensure_asset`'s native arm through `set_balance`, which fixes the same wipe bug that arm has today.

**Files:**

- Modify: `crates/cosmwasm/src/provider/mock.rs` (`set_balance`, imports)
- Modify: `crates/cosmwasm/src/chain.rs` (`ensure_asset` native arm, imports)
- Test: `crates/cosmwasm/src/tests.rs`

**Interfaces:**

- Consumes: `set_balance(addr, denom, amount)` from Task 1.
- Produces: merge semantics on `CwMockProvider::set_balance` (other denoms preserved, same denom overwritten, amount 0 removes the denom). Task 6 documents this.

- [ ] **Step 1: Write the failing tests**

In `crates/cosmwasm/src/tests.rs`, extend the imports:

```rust
use crate::{CwAsset, CwChain};
```

and add:

```rust
#[tokio::test]
async fn set_balance_multiple_denoms() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain.set_balance(&bob, "ustake", 100).await.unwrap();
    chain.set_balance(&bob, "uatom", 55).await.unwrap();

    // The native denom survives minting a second denom.
    assert_eq!(chain.balance(&bob).await.unwrap(), 100);

    // The second denom is readable at the bank level. Cloning the mock provider
    // shares the same chain state (Rc), so `p` stays valid across later writes.
    let p = match &chain {
        CwChain::Mock(p) => p.clone(),
        CwChain::Rpc(_) => unreachable!("mock chain"),
    };
    let uatom = p.app().wrap().query_balance(&bob, "uatom").unwrap();
    assert_eq!(uatom.amount.u128(), 55);

    // Setting an existing denom overwrites its amount, it does not add.
    chain.set_balance(&bob, "uatom", 5).await.unwrap();
    let uatom = p.app().wrap().query_balance(&bob, "uatom").unwrap();
    assert_eq!(uatom.amount.u128(), 5);

    // Amount 0 clears the denom entry.
    chain.set_balance(&bob, "uatom", 0).await.unwrap();
    let all = p.app().wrap().query_all_balances(&bob).unwrap();
    assert!(all.iter().all(|c| c.denom != "uatom"));
    assert_eq!(chain.balance(&bob).await.unwrap(), 100);
}

#[tokio::test]
async fn ensure_asset_native_preserves_other_denoms() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain.set_balance(&bob, "uatom", 10).await.unwrap();

    // new_account funded DEFAULT_FUNDING of "ustake"; asking for double forces a mint.
    chain
        .ensure_asset(
            &bob,
            CwAsset::Native("ustake".to_string()),
            2 * crate::DEFAULT_FUNDING,
        )
        .await
        .unwrap();

    let p = match &chain {
        CwChain::Mock(p) => p.clone(),
        CwChain::Rpc(_) => unreachable!("mock chain"),
    };
    assert_eq!(
        p.app().wrap().query_balance(&bob, "uatom").unwrap().amount.u128(),
        10
    );
    assert_eq!(chain.balance(&bob).await.unwrap(), 2 * crate::DEFAULT_FUNDING);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cross-vm-cosmwasm set_balance_multiple_denoms ensure_asset_native_preserves_other_denoms`
Expected: FAIL. `set_balance_multiple_denoms` fails at `assert_eq!(chain.balance(&bob).await.unwrap(), 100)` (actual 0, the uatom write wiped ustake). `ensure_asset_native_preserves_other_denoms` fails at the uatom assertion (actual 0).

- [ ] **Step 3: Implement merge semantics in `crates/cosmwasm/src/provider/mock.rs`**

Update the import to add `coin` and `Uint128` (and drop `coins`, which becomes unused):

```rust
use cosmwasm_std::{coin, Addr, Coin, Empty, Uint128};
```

Replace `set_balance`:

```rust
    async fn set_balance(&mut self, addr: &Addr, denom: &str, amount: u128) -> Result<(), CwError> {
        let addr = addr.clone();
        // `BankKeeper::init_balance` replaces the account's whole coin vector, so read,
        // merge the one denom, and write the full list back to preserve other denoms.
        let mut balances = self
            .app
            .borrow()
            .wrap()
            .query_all_balances(&addr)
            .map_err(|e| CwError::Balance(e.to_string()))?;
        match balances.iter_mut().find(|c| c.denom == denom) {
            Some(entry) => entry.amount = Uint128::new(amount),
            None => balances.push(coin(amount, denom)),
        }
        balances.retain(|c| !c.amount.is_zero());
        self.app
            .borrow_mut()
            .init_modules(|router, _api, storage| {
                router.bank.init_balance(storage, &addr, balances)
            })
            .map_err(|e| CwError::Balance(e.to_string()))
    }
```

- [ ] **Step 4: Reroute `CwChain::ensure_asset`'s native arm in `crates/cosmwasm/src/chain.rs`**

Replace the `CwAsset::Native(denom)` arm (the one that calls `init_modules` inline):

```rust
            CwAsset::Native(denom) => {
                let current = p
                    .app()
                    .wrap()
                    .query_balance(who, &denom)
                    .map_err(|e| FundError::Provider(e.to_string()))?
                    .amount
                    .to_string()
                    .parse::<u128>()
                    .map_err(|e| FundError::Provider(e.to_string()))?;
                if current < amount {
                    p.set_balance(who, &denom, amount)
                        .await
                        .map_err(|e| FundError::Provider(e.to_string()))?;
                }
                Ok(())
            }
```

Then remove now unused imports from `chain.rs` (`coins`, and `ChainProvider` only if it was solely used here, which it is not, so in practice just `coins`). Let the compiler's unused import warnings be the guide.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p cross-vm-cosmwasm`
Expected: PASS (the two new tests plus every existing test).

- [ ] **Step 6: Commit**

```bash
git add crates/cosmwasm
git commit -m "feat(cosmwasm): multi-denom set_balance with merge semantics

Read query_all_balances, replace the one denom, write the full list
back (init_balance overwrites the whole coin vector). Amount 0 clears
the denom. ensure_asset's native arm now routes through set_balance,
fixing its wipe of other denoms."
```

---

### Task 3: EVM denom validation

**Files:**

- Modify: `crates/solidity/src/provider/mock.rs` (`set_balance`)
- Test: `crates/solidity/src/tests.rs`

**Interfaces:**

- Consumes: `set_balance(addr, denom, amount)` from Task 1, `EvmError::Balance(String)`.
- Produces: denom mismatch returns `Err(EvmError::Balance(..))`; matching is `eq_ignore_ascii_case` against `chain_info().native_symbol`.

- [ ] **Step 1: Write the failing test**

In `crates/solidity/src/tests.rs`, add:

```rust
#[tokio::test]
async fn set_balance_validates_denom() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;

    // Unknown denom is rejected.
    assert!(chain.set_balance(&bob, "BTC", U256::from(1u64)).await.is_err());

    // The native symbol is accepted case-insensitively.
    chain.set_balance(&bob, "eth", U256::from(7u64)).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(7u64));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cross-vm-solidity set_balance_validates_denom`
Expected: FAIL at the `is_err()` assertion (denom is currently ignored, so "BTC" succeeds).

- [ ] **Step 3: Implement validation in `crates/solidity/src/provider/mock.rs`**

```rust
    async fn set_balance(&mut self, addr: &Address, denom: &str, amount: U256) -> Result<(), EvmError> {
        if !denom.eq_ignore_ascii_case(self.info.native_symbol) {
            return Err(EvmError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{}'",
                self.info.native_symbol
            )));
        }
        self.core.set_balance(*addr, amount);
        Ok(())
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p cross-vm-solidity`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/solidity
git commit -m "feat(solidity): validate set_balance denom against native symbol"
```

---

### Task 4: Solana denom validation

**Files:**

- Modify: `crates/solana/src/provider/mock.rs` (`set_balance`)
- Test: `crates/solana/src/tests.rs`

**Interfaces:**

- Consumes: `set_balance(addr, denom, amount)` from Task 1, `SvmError::Balance(String)`.
- Produces: same rule as Task 3 with `SvmError`.

- [ ] **Step 1: Write the failing test**

In `crates/solana/src/tests.rs`, add:

```rust
#[tokio::test]
async fn set_balance_validates_denom() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let bob = chain.new_account("bob").await;

    assert!(chain.set_balance(&bob, "BTC", 1).await.is_err());

    chain.set_balance(&bob, "sol", 7_777).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 7_777);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cross-vm-solana set_balance_validates_denom`
Expected: FAIL at the `is_err()` assertion.

- [ ] **Step 3: Implement validation in `crates/solana/src/provider/mock.rs`**

```rust
    async fn set_balance(&mut self, addr: &Address, denom: &str, amount: u64) -> Result<(), SvmError> {
        if !denom.eq_ignore_ascii_case(self.info.native_symbol) {
            return Err(SvmError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{}'",
                self.info.native_symbol
            )));
        }
        let account = Account {
            lamports: amount,
            data: Vec::new(),
            owner: solana_system_interface::program::ID,
            executable: false,
            rent_epoch: u64::MAX,
        };
        self.svm
            .borrow_mut()
            .set_account(*addr, account)
            .map_err(|e| SvmError::Balance(format!("{e:?}")))
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p cross-vm-solana`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/solana
git commit -m "feat(solana): validate set_balance denom against native symbol"
```

---

### Task 5: Tron denom validation

**Files:**

- Modify: `crates/tron/src/provider/mock.rs` (`set_balance`, plus a test in its in-file test module)

**Interfaces:**

- Consumes: `set_balance(addr, denom, amount)` from Task 1, `TronError::Balance(String)` (defined at `crates/tron/src/error.rs:20`).
- Produces: same rule as Task 3 with `TronError`.

- [ ] **Step 1: Write the failing test**

In the test module at the bottom of `crates/tron/src/provider/mock.rs` (the one with the `provider()` helper), add:

```rust
    #[tokio::test]
    async fn set_balance_validates_denom() {
        let mut c = provider();
        let a = c.new_account("alice").await;

        assert!(c.set_balance(&a, "BTC", 1).await.is_err());

        c.set_balance(&a, "trx", 7 * SUN_PER_TRX).await.unwrap();
        assert_eq!(c.balance(&a).await.unwrap(), 7 * SUN_PER_TRX);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cross-vm-tron set_balance_validates_denom`
Expected: FAIL at the `is_err()` assertion.

- [ ] **Step 3: Implement validation in `crates/tron/src/provider/mock.rs`**

Replace the `set_balance` body (keep the existing forwarding line):

```rust
    async fn set_balance(&mut self, addr: &TronAddress, denom: &str, amount: u64) -> Result<(), TronError> {
        let symbol = self.chain_info().native_symbol;
        if !denom.eq_ignore_ascii_case(symbol) {
            return Err(TronError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{symbol}'"
            )));
        }
        self.core.set_balance(addr.as_evm(), U256::from(amount));
        Ok(())
    }
```

(If the current body has extra lines, for example a comment, keep them; only the validation guard is new.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p cross-vm-tron`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/tron
git commit -m "feat(tron): validate set_balance denom against native symbol"
```

---

### Task 6: Documentation and full workspace verification

**Files:**

- Modify: `SPEC.md` (~line 29, the `ChainProvider` method list)
- Modify: `README.md` (~line 442, the feature table row about `set_balance`)
- Modify: `CHANGELOG.md` (new entry under `## [Unreleased]`)

Reminder: these are doc files, so no dashes as punctuation in the new sentences.

- [ ] **Step 1: Update SPEC.md**

In the sentence listing the `ChainProvider` methods, change `set_balance` to `set_balance(addr, denom, amount)` and append after that sentence:

```
`set_balance` takes a denom argument. CosmWasm mocks mint any bank denom verbatim and preserve the account's other denoms (setting an amount of 0 clears the denom). EVM, Solana, and Tron accept only their native symbol, matched case-insensitively ("ETH", "SOL", "TRX"), and amounts stay in base units (wei, lamports, sun). Every RPC backend keeps returning `Unimplemented` for `set_balance`.
```

- [ ] **Step 2: Update README.md**

In the feature table row for live RPC writes (the cell already ends with "`set_balance` is `Unimplemented` on every RPC backend (a live chain cannot mint)"), leave the RPC note and append to the same cell:

```
On mocks, `set_balance(addr, denom, amount)` mints any bank denom on CosmWasm and accepts only the native symbol on EVM, Solana, and Tron.
```

- [ ] **Step 3: Update CHANGELOG.md**

Add under `## [Unreleased]`, above the existing entries:

```markdown
### Changed (multi native balance: denom aware `set_balance`)

* **Breaking:** `ChainProvider::set_balance` is now `set_balance(addr, denom, amount)`. CosmWasm mocks mint any bank denom verbatim ("uosmo", "uatom", "ibc/...") and merge it into the account's existing coins instead of overwriting them (setting an amount of 0 clears the denom). EVM, Solana, and Tron have a single native token, so `denom` must equal the chain's `native_symbol`, matched case-insensitively ("ETH", "SOL", "TRX"), and amounts stay in base units (wei, lamports, sun); any other denom is a `Balance` error. Every RPC backend keeps returning `Unimplemented` (a live chain cannot mint).
* Fixed: `CwChain::ensure_asset` with a native asset no longer wipes the account's other denoms when it mints (it now routes through the merge aware `set_balance`).
```

- [ ] **Step 4: Full workspace verification**

Run: `cargo fmt --all`
Run: `cargo check --workspace --all-targets`
Expected: clean.

Run: `make compile && cargo test --workspace`
Expected: PASS. (`cargo test --workspace` needs the contract artifacts, hence `make compile` first; it requires the per VM contract toolchains. If those toolchains are unavailable in this environment, fall back to `cargo test -p cross-vm-core -p cross-vm-cosmwasm -p cross-vm-solana -p cross-vm-solidity -p cross-vm-tron -p cross-vm-framework` and say so explicitly in the final report.)

- [ ] **Step 5: Commit**

```bash
git add SPEC.md README.md CHANGELOG.md
git commit -m "docs: document denom-aware set_balance across VMs"
```

---

## Out of scope (deliberately)

- `ChainProvider::balance` keeps reading only the native denom. Reading an arbitrary Cosmos denom is already possible at the bank level (`CwMockProvider::app().wrap().query_balance(addr, denom)`) and via `ensure_asset`; a denom aware read API is a separate decision.
- RPC `set_balance` stays `Unimplemented` on all four VMs, per the requirements.
- No TRC10 support on Tron. The Tron mock models a single native token (sun).
- `cross-vm-revm-common::ChainCore::set_balance` keeps its denomless signature (it is below the denom abstraction).
