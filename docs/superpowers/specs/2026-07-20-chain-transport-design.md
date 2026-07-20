# Chain Transport Layer Design

Date: 2026-07-20
Branch: `feat/chain-transport`
Status: planned

## Goal

Add a pluggable transport layer between chain providers and their HTTP clients. Developers can attach any transport they prefer (custom HTTP stacks, websockets later, instrumentation wrappers). Shipped transports:

- Cosmos (`crates/cosmwasm`): `HttpTransport` and `BatchHttpTransport`. The batch transport automatically merges concurrent JSON-RPC calls into a single CometBFT batch request (viem style, debounce window plus max batch size). This is the headline feature.
- EVM (`crates/solidity`): `HttpTransport` only for now.

Mock backends are untouched. Default behavior with no transport specified is identical to today.

## Current state (why a seam is needed)

- `CwRpcProvider` (`crates/cosmwasm/src/provider/rpc.rs`) builds a `cosmrs::rpc::HttpClient` per request inside `client()`. No pooling, no batching, no injection point.
- `EvmRpcProvider` (`crates/solidity/src/provider/rpc.rs`) builds alloy `ProviderBuilder::new()...connect_http(url)` per request in `provider()` and `signing_provider()`.
- No transport trait exists anywhere. `ws_url` already flows through config but nothing consumes it, which marks the intended future hook.
- No JSON-RPC batching exists. `CwBatch` is application level (many cosmos msgs, one tx), not transport level.

## Design

### Cosmos: string envelope transport trait

The tendermint-rpc `Client` trait cannot be the seam. Its `perform<R>` method is generic, therefore not object safe, so no `Rc<dyn Client>`. Generics would infect `CwRpcProvider`, `CwChain`, `AnyChain`, and the framework. Its `#[async_trait]` also demands `Send`, and this repo is Rc based, current thread.

Instead the transport moves raw JSON-RPC envelope strings, while tendermint-rpc stays for serialization and typed parsing only. Verified against tendermint-rpc 0.40.4 (locked): `RequestMessage::into_json()` produces a full JSON-RPC 2.0 envelope with a UUIDv4 id, and `Response::from_string()` parses the envelope and converts JSON-RPC errors into typed errors. Parsing and typing stay identical to today. Only three endpoints are in use: `abci_query`, `status`, and `broadcast::tx_commit`, all `SimpleRequest<LatestDialect>` (v0_38, same as the current `HttpClient` default CompatMode).

New module `crates/cosmwasm/src/transport.rs`:

```rust
/// Boxed non-Send future: matches the repo's Rc/current-thread world, keeps the trait dyn-safe.
pub type TransportFuture<'a> = Pin<Box<dyn Future<Output = Result<String, CwError>> + 'a>>;

/// One CometBFT JSON-RPC call. `request` is a complete JSON-RPC 2.0 envelope
/// ({"jsonrpc","id","method","params"}); returns the raw response envelope for that id.
pub trait CosmosTransport {
    fn call(&self, request: String) -> TransportFuture<'_>;
}

/// pub(crate) seam under BatchHttpTransport: post arbitrary JSON body, return body text.
/// HttpTransport implements it; unit tests inject a fake.
pub(crate) trait JsonRpcPost {
    fn post(&self, body: String) -> TransportFuture<'_>;
}

pub struct HttpTransport { /* url: Option<String>, chain_id: String, http: reqwest::Client */ }

#[derive(Clone, Copy)]
pub struct BatchConfig { pub interval: Duration, pub max_size: usize } // Default: 20ms / 20

pub struct BatchHttpTransport {
    /* poster: Rc<dyn JsonRpcPost>, cfg, queue: RefCell<Vec<Pending>>,
       leader: Cell<bool> */
}
```

`HttpTransport::new(rpc_url, chain_id)` keeps `chain_id` only for the error message, preserving the current "chain '{id}' has no rpc_url; use a chain preset with an endpoint" text. `BatchHttpTransport::with_poster(poster, cfg)` is the `pub(crate)` test constructor.

### Batch algorithm (leader driven, no spawn_local)

1. `call()` parses the envelope, extracts the UUIDv4 id, pushes `Pending { id, envelope, oneshot::Sender }` onto the queue.
2. If the leader flag is clear, this future becomes leader (sets the flag with a drop guard that clears it on cancellation). The leader then runs a tick loop: sleep one `cfg.interval` (the first drain lands a full interval after the leader starts, never immediately), drain at most `max_size` queued calls, and start that chunk's POST without awaiting it, so in flight POSTs overlap later ticks instead of the leader blocking on each. A chunk of one posts the bare envelope, larger chunks post a JSON array. Each POST, once it resolves, parses its response (array or single object, handle both) and routes each element to its pending by id, re-serializing the element to `String` through the oneshot. A missing id or POST failure sends `Err` to the affected pendings. Nothing flushes early: a queue longer than `max_size` simply drains over successive ticks, so a burst settles at a steady one chunk per `interval`. When the queue empties, clear the flag.
3. Caller awaits its own oneshot for the response string. Closed receiver send errors are ignored (caller cancelled).

No borrow is held across an await (drain before post). Fully unit testable with a fake `JsonRpcPost` and `#[tokio::test(start_paused = true)]`.

### CwRpcProvider changes

- Field `rpc_url: String` becomes `transport: Rc<dyn CosmosTransport>`. Still `Clone`.
- `new(info, wallets)` keeps its signature and defaults to `HttpTransport` (behavior preserved). Add `new_with_transport(info, wallets, transport)`.
- A typed helper replaces `client()` and the per-method `Client` calls:

```rust
async fn perform<R>(&self, req: R) -> Result<R::Output, CwError>
where R: cosmrs::rpc::SimpleRequest, // default dialect = latest (v0_38)
{
    let resp = self.transport.call(req.into_json()).await?;
    let parsed = <R as Request>::Response::from_string(resp).map_err(|e| CwError::Rpc(e.to_string()))?;
    Ok(parsed.into())
}
```

- `abci_query` calls `perform(endpoint::abci_query::Request::new(...))`, `try_block_height` calls `perform(endpoint::status::Request)`, and `sign_and_broadcast` replaces `raw.broadcast_commit(&client)` with `perform(endpoint::broadcast::tx_commit::Request::new(raw.to_bytes()?))`. Response field access is unchanged.

### EVM: factory trait over alloy's own seam

Alloy already has the transport seam: `RpcClient`. Verified on alloy 2.1.0: `ClientBuilder::default().http(url)` and `ProviderBuilder::connect_client(client)` return a concrete provider, so the inherent `fill` used by `sign_transaction` survives. Our trait is a pluggable factory over that seam, in new module `crates/solidity/src/transport.rs`:

```rust
pub type EvmClientFuture<'a> = Pin<Box<dyn Future<Output = Result<RpcClient, EvmError>> + 'a>>;

pub trait EvmTransport {
    /// Async because future ws transports connect on first use; http resolves immediately.
    fn rpc_client(&self) -> EvmClientFuture<'_>;
}

pub struct HttpTransport { /* url: Option<String>, chain_id: String */ }
```

`EvmRpcProvider` stores `Rc<dyn EvmTransport>` instead of `rpc_url`. `provider()` and `signing_provider()` become async and build through `connect_client(self.transport.rpc_client().await?)`, plus `.wallet(...)` for signing. All call sites are already async. Testable without a node via `RpcClient::mocked(asserter)` behind a test transport.

### Wiring

- Cosmos sugar (`crates/cosmwasm/src/chains/sugar.rs`): keep `rpc()`, add `rpc_with(wallets, transport)` and `rpc_batched(wallets, BatchConfig)`.
- EVM sugar (`crates/solidity/src/chains/sugar.rs`): keep `rpc()`, add `rpc_with(wallets, transport)`.
- Presets are `Copy` and `&'static`, untouched. Transport is chosen at construction, not stored in info.
- TOML config: `ChainDecl` gains `transport: Option<String>` ("http" default, "batch-http" cosmos only), `batch_interval_ms: Option<u64>`, `batch_max_size: Option<usize>` (cosmos only, valid only with `transport = "batch-http"`). `deny_unknown_fields` means the fields thread explicitly through `ChainDecl`, `domain.rs`, `ChainSpecData`, and the `build_chain` arms. Validation lives in `validate.rs` per chain kind, matching the existing rpc url and target checks.
- Mock backends, `CwChain`, `AnyChain`: untouched. Transport hides behind `Rc<dyn>` inside the concrete provider.

## Risks

- URL scheme: tendermint convention allows `tcp://`, reqwest does not. `HttpTransport` maps `tcp://` to `http://`. All presets use https.
- Some public RPC gateways reject JSON-RPC array bodies, so the batch transport fails against them. Default stays `http`, batch is opt in. Document this.
- Batch responses arrive in arbitrary order, so id matching is mandatory (designed in).
- Leader cancellation mid flush: the drop guard clears the flag and queued pendings are picked up by the next caller leader. Must be tested.
- Dialect: pinning `LatestDialect` matches today's `HttpClient` default. Nodes older than CometBFT 0.37 would need a compat mode, the same limitation as today, no regression.
- `into_json` pretty prints, the batch transport re-serializes compact. Servers are indifferent.

## Task list (ordered, dispatchable)

### T1: Cosmos transport module (fable)

- Objective: create `CosmosTransport` trait, `TransportFuture`, `HttpTransport` (reqwest, `tcp://` to `http://`, no-url error with chain id), `JsonRpcPost` seam, `BatchConfig`, `BatchHttpTransport` per the algorithm above, plus unit tests: same-window calls merge into one POST array; out of order response ids route correctly; `max_size = 2` with 5 calls yields 3 POSTs; a single call posts a bare envelope and handles an object response; a JSON-RPC error envelope reaches the right caller; a POST failure errors all chunk pendings; the no-url error text matches the current provider message; the leader-cancel drop guard leaves the queue recoverable.
- Files: `crates/cosmwasm/src/transport.rs` (new), `crates/cosmwasm/src/lib.rs` (mod plus re-exports), `crates/cosmwasm/Cargo.toml` (add `reqwest.workspace`, dev tokio `test-util`).
- Done: `cargo test -p cross-vm-cosmwasm transport` green, no live node, no other file touched.

### T2: CwRpcProvider rides transport (fable, after T1)

- Objective: replace the `rpc_url` field and `client()`/`HttpClient` with `Rc<dyn CosmosTransport>`; add the `perform<R: SimpleRequest>` helper; rewrite `abci_query`, `try_block_height`, `sign_and_broadcast` (tx_commit request, `raw.to_bytes()?`); add `new_with_transport`; `new` defaults to `HttpTransport` with the same lazy no-url error.
- Files: `crates/cosmwasm/src/provider/rpc.rs` only.
- Done: crate builds, existing rpc.rs unit tests pass, `use cosmrs::rpc::{Client, HttpClient}` gone, response field access unchanged. Add one test: provider plus a fake `CosmosTransport` answering `status` makes `try_block_height` return it (proves the perform path without a node).

### T3: Cosmos sugar (sonnet, after T1 and T2)

- Objective: `rpc_with(wallets, transport: Rc<dyn CosmosTransport>)` and `rpc_batched(wallets, cfg: BatchConfig)` on `CosmosChainInfo`.
- Files: `crates/cosmwasm/src/chains/sugar.rs`.
- Done: compiles with doc examples, `rpc()` behavior untouched.

### T4: EVM transport plus provider (opus)

- Objective: `EvmTransport` trait plus `HttpTransport`; `EvmRpcProvider` stores `Rc<dyn EvmTransport>`, `provider()`/`signing_provider()` async via `connect_client`, `sign_transaction` unchanged; `new_with_transport`; sugar `rpc_with`. Test: a custom transport returning `RpcClient::mocked(asserter)` makes `try_block_height` and `balance` return the asserted values.
- Files: `crates/solidity/src/transport.rs` (new), `crates/solidity/src/lib.rs`, `crates/solidity/src/provider/rpc.rs`, `crates/solidity/src/chains/sugar.rs`.
- Done: crate tests green without a node, default `rpc()` behavior and no-url error text preserved.

### T5: Config schema plus validation (opus, parallel with T1 to T4)

- Objective: `ChainDecl` gains `transport`, `batch_interval_ms`, `batch_max_size`; validation: `transport` in {http, batch-http} for cosmos, {http} for evm, absent or http for others; `batch_*` require `transport = "batch-http"`; error messages list valid values (house style).
- Files: `crates/config/src/chain.rs`, `crates/config/src/validate.rs`, `crates/config/tests/schema.rs`.
- Done: config crate tests cover accept and reject cases per chain kind.

### T6: Framework threading plus build_chain (opus, after T2, T4, T5)

- Objective: thread the three fields through `ChainDecl`, `domain.rs`, `ChainSpecData`, and the build arms. Cosmos arm: `"batch-http"` builds `info.rpc_batched(wallets, BatchConfig { interval, max_size })`, else `info.rpc(wallets)`. EVM arm: http only (validated upstream). Tests mirror the `build_threads_gas_adjustment_into_chain_info` pattern: batch selection builds an Rpc chain, absent default is identical to today.
- Files: `crates/framework/src/config/domain.rs`, `crates/framework/src/config/setup_request.rs`, `crates/framework/src/config/build_chain.rs`.
- Done: framework tests green, mock target never sees transport.

### T7: Workspace verify (sonnet)

- Objective: `cargo build --workspace`, `cargo test --workspace`, examples compile with presets, confirm no mock path diffs.
- Files: none (read and run only).
- Done: all green.
