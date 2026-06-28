# Specification

## Goal

Provide a uniform way to drive three execution environments (CosmWasm, EVM, Solana) from Rust, so the same test code and cross VM scripts work against any of them. Phase 1 covers the chain providers and their in process (mock) backends.

## Design

The three VMs disagree on nearly every concrete type. CosmWasm uses `Addr` and JSON messages, EVM uses a 20 byte `Address` and ABI calldata, Solana uses a 32 byte `Address` (pubkey) and Borsh instructions. A single trait built from associated types is the way to share one method vocabulary while letting each VM keep its idiomatic types. This mirrors cw-orch's `CwEnv` and test-tube's `Runner`.

The core trait is synchronous. All three mock VMs run synchronously, and the future live RPC path will wrap async internally rather than forcing `async` on every caller.

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

`ChainProvider` is the uniform provider surface. Associated types (`Address`, `Code`, `InitMsg`, `ExecMsg`, `QueryMsg`, `ContractRef`, `Response`, `QueryResponse`, `Balance`, `Error`) let each VM specialize. Methods: `chain_info`, `new_account`, `balance`, `set_balance`, `block_height`, `advance_blocks`, `deploy`, `execute`, `query`.

`CrossVmError` is a unified error enum. Each provider's own error converts into it (via the `Error: Into<CrossVmError>` bound), so cross VM scripts can use one `Result` type.

### Per VM mapping

| Concern | CosmWasm (`cw-multi-test`) | EVM (`revm`) | Solana (`litesvm`) |
| --- | --- | --- | --- |
| Backend | `App` with `MockApiBech32` | `MainnetEvm` over `InMemoryDB` | `LiteSVM` |
| Address | `Addr` (bech32, chain prefix) | `Address` (20 bytes) | `Address` (pubkey) |
| Deploy | `store_code` then `instantiate_contract` | create tx with bytecode | `add_program` |
| Execute | `execute_contract` | `transact_commit` (call tx) | signed `Transaction` |
| Query | `wrap().query_wasm_smart` | `transact` (static call) | `get_account` |
| Balance | bank `init_balance` / `query_balance` | `AccountInfo.balance` | `airdrop` / `get_balance` |
| Code/Msg types | `serde_json::Value` | `Bytes` (calldata) | `Vec<Instruction>` |

Notes on specific choices:

* The EVM mock holds the `revm` instance in a `RefCell` so the read only `query` (which `revm` implements through a `&mut` static call) can run behind `&self`. Queries use `transact` (no commit) so they leave no state behind. Nonce checking is disabled and transactions are sent as legacy (no chain id) to keep a test harness free of nonce and EIP-155 bookkeeping.
* The Solana mock keeps the `Keypair` generated for each account and looks it up by pubkey when signing, since Solana has no notion of an address that can send a transaction without its key. Block height is tracked alongside `warp_to_slot`.
* The CosmWasm mock configures `MockApiBech32` with the chain's bech32 prefix, so generated addresses are realistic (for example `osmo1...`).

### Predefined chains

Each VM crate defines its own `ChainInfo` struct (with VM specific fields) implementing `ChainSpec`, plus constants in its `chains` module. The two construction styles are equivalent:

```rust
let chain = OSMOSIS.mock();             // sugar
let chain = CwMockProvider::new(OSMOSIS);
```

RPC providers exist as compiling stubs (`OSMOSIS.rpc()`, `EvmRpcProvider::new`, etc.); every operation returns `Unimplemented` until phase 2.

## Out of scope (phase 2 and later)

Live RPC implementations (cosmrs broadcast, alloy http provider, Solana RpcClient); the cross VM orchestration layer that runs one script across all three; fuzz and invariant harnesses; gas/compute reporting; fork from live.
