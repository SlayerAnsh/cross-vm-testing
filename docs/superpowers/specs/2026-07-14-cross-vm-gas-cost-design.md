# Cross VM gas and cost reporting

Date: 2026-07-14
Status: approved, not yet implemented

## Problem

The framework has no working notion of execution cost. `AppResponse::gas_used() -> Option<u128>`
exists, but it returns `Some` for exactly one VM on one backend (Solana on the mock). That is not
because the data is unavailable. It is because the `CosmWasm`, `Evm`, and `Tron` variants of
`RawResponse` have no field to hold it.

Meanwhile every backend already produces a real cost number and throws it away:

| Source | Number available | Where it is dropped |
| --- | --- | --- |
| revm mock (EVM and Tron) | `ExecutionResult::Success.gas.tx_gas_used()` | `crates/revm-common/src/lib.rs:311` (elided by `..`) |
| CosmWasm RPC | `tx_result.gas_used`, `gas_wanted` | `crates/cosmwasm/src/provider/rpc.rs:236` |
| EVM RPC | `receipt.gas_used()`, `effective_gas_price()` | `crates/solidity/src/provider/rpc.rs:160-210` |
| Tron RPC | `receipt.energy_usage_total`, `net_usage`, `fee` | `crates/tron/src/provider/rpc.rs:423+` |
| Solana mock | `TransactionMetadata.fee` | `crates/framework/src/contract/response.rs:105` (only compute units are read) |

Separately, gas limits are hardcoded magic numbers with no way to override them, and they are not
even the same quantity from one VM to the next.

## Non goals

Estimation on `AnyChain`. `AnyChain` exposes no contract execution op, by deliberate design
(`crates/core/src/chain_provider.rs:7-12`: contract ops have irreconcilable per VM signatures, so
they live on each VM's concrete provider). Introducing a VM erased "call intent" type purely to
give `AnyChain` an `estimate` method was considered and rejected. Estimation lives on the concrete
chains, reached through the downcast (`ContractBase::evm()`, `::cosmwasm()`, and so on) that every
contract op already uses. `AnyChain` participates through `AppResponse::cost()`, which is already
VM erased.

Making the CosmWasm mock or the Tron mock report a cost they cannot compute. See "Honest gaps".

## Design

Three phases. Each is independently shippable and phase 1 is a precondition for phase 2.

### Phase 1: make cost reporting real

Replace `gas_used() -> Option<u128>` with a self describing struct. A single scalar is wrong for
half the VMs: Tron bills two independent resources (energy for computation, bandwidth for calldata
bytes, neither derivable from the other), and Solana's fee is not a function of its compute units
(the base fee is per signature, priority fee is a separate opt in).

```rust
pub struct Cost {
    /// Native execution unit consumed.
    pub units: u128,
    /// Which unit `units` is denominated in.
    pub unit: CostUnit,
    /// Bandwidth consumed. Tron only, `None` elsewhere.
    pub bandwidth: Option<u64>,
    /// Fee in the chain's native denom, in base units, where the backend
    /// reports or can derive one.
    pub fee: Option<u128>,
}

pub enum CostUnit {
    Gas,          // EVM, CosmWasm
    ComputeUnits, // Solana
    Energy,       // Tron
}
```

`RawResponse::cost()`, `AppResponse::cost()`, and `HookContext::cost()` return `Option<Cost>`.
`None` means the backend cannot meter, not that the op was free. The `CosmWasm` / `Evm` / `Tron`
variants of `RawResponse` gain a cost field, and every constructor threads it.

Wiring per backend, all of which is reading a value that is already computed or already fetched:

| VM | Backend | Source |
| --- | --- | --- |
| EVM | mock | stop eliding gas in `exec_or_err`; add the field to `Execution` and `Deployment`. Note: in revm 41 the receipt figure is `gas.tx_gas_used()` (already net of the EIP-3529 refund, floored per EIP-7623). There is no `Success.gas_used` field; the `ExecutionResult::gas_used()` accessor is deprecated as ambiguous after the EIP-8037 state gas split |
| EVM | RPC | `receipt.gas_used()`, fee = `gas_used * effective_gas_price` (wei) |
| CosmWasm | mock | `None`. cw-multi-test has no gas meter |
| CosmWasm | RPC | `tx_result.gas_used`; fee = the **declared** fee already computed at `rpc.rs:198`, not `gas_used * gas_price`. The Cosmos SDK's `DeductFeeDecorator` takes the full declared fee up front and does not refund unspent gas, so the declared amount is what the sender actually paid. `gas_used * gas_price` would understate it by the `FEE_BUFFER` factor plus the unspent headroom |
| Solana | mock | `TransactionMetadata.compute_units_consumed` and `.fee` (lamports) |
| Solana | RPC | unreachable, all write paths are `Unimplemented` |
| Tron | mock | revm `gas_used`, reported as `unit: Gas` (not `Energy`), plus bandwidth from the shim. See "Honest gaps" |
| Tron | RPC | `receipt.energy_usage_total`, `receipt.net_usage`, `fee` (sun) |

### Phase 2: estimation

`estimate_*` methods on the concrete chains, returning the same `Cost` type so a forecast and a
receipt are directly comparable. Primitives, all of which already exist:

- EVM RPC: `alloy Provider::estimate_gas` (already in scope, zero new plumbing).
- EVM mock: a revm transact that is not committed.
- CosmWasm RPC: `/cosmos.tx.v1beta1.Service/Simulate`. This is genuinely new. There is currently
  no simulate call anywhere in the crate.
- CosmWasm mock: `None`. No gas meter to simulate against.
- Solana mock: `LiteSVM::simulate_transaction`, which yields compute units and fee without
  committing. Reachable today through `SvmMockProvider::svm()`.
- Tron RPC: `triggerconstantcontract`, which is already called by `static_call`; its `energy_used`
  field is currently discarded.

### Phase 3: configurable limits

Limits become a required per call argument on every mutating op. No defaulting fallback, matching
the codebase convention. The types are per VM, because "limit" is not the same quantity on each
chain and a shared `GasLimit::Exact(u64)` would silently mean gas on EVM, sun on Tron, and compute
units on Solana.

```rust
pub enum EvmGasLimit  { Exact(u64), Estimated }  // gas units
pub enum CwGasLimit   { Exact(u64), Estimated }  // gas units
pub enum SvmComputeBudget { Exact(u32), Estimated }  // compute units

pub struct TronLimits {              // Tron has two independent knobs
    pub fee_limit: TronFeeLimit,     // cap on fee, in sun (NOT on energy)
    pub origin_energy_limit: u64,
}
pub enum TronFeeLimit { Exact(u64), Estimated }
```

`Estimated` runs the phase 2 primitive and multiplies by the chain's `gas_adjustment`, a new chain
config field. It costs one extra round trip. It replaces the hardcoded `FEE_BUFFER = 2.0` at
`crates/cosmwasm/src/provider/rpc.rs:197`.

Ops taking a limit: `store_code`, `instantiate`, `execute_contract`, `deploy_create`, `call`,
`call_value`, `transfer_funds`, `add_program`, `add_program_at`. This churns every signature and
every call site in `crates/` and `examples/`.

Constants replaced: CosmWasm's `15_000_000` / `400_000` / `300_000` / `200_000`
(`cosmwasm/rpc.rs:259,319,352,294`), EVM's `TX_GAS_LIMIT = 30_000_000`
(`revm-common/src/lib.rs:36`), Tron's `DEFAULT_FEE_LIMIT` and `DEFAULT_ORIGIN_ENERGY_LIMIT`
(`tron/rpc.rs:41,43`).

Config placement: `gas_adjustment` goes in chain config (`crates/config`), alongside the existing
CosmWasm only `gas_price`. It is policy, not a secret, so it does not belong in `.env`.

## Honest gaps

These are load bearing. The API must not paper over them.

**CosmWasm mock returns `None`.** cw-multi-test does not meter gas at all; its response is
`{events, data}`. There is no number, and none can be synthesized without simulating a real chain.
`crates/cosmwasm/src/chains/info.rs:22` already says the mock does not charge gas.

**The Tron mock reports gas, not energy.** The Tron mock looks like it has energy
(`ResourceTracker`, `energy()`, `freeze_for_energy`) but energy is never decremented by contract
execution. The shim deliberately sits outside revm's gas loop
(`crates/tron/src/tvm/resources.rs:5-7`), and only bandwidth is charged, by calldata length.
Reporting revm gas as Tron energy would be a lie: Tron energy is not EVM gas.

The `CostUnit` field resolves this without a lie and without a hole. The Tron mock *is* revm, so it
genuinely consumes EVM gas. It therefore reports `unit: Gas`, while Tron RPC reports
`unit: Energy`. Each backend states the quantity it actually metered. A caller comparing a mock
figure against a live one can see from the unit that they are not the same quantity, which is the
truth and is exactly what a self describing unit field is for. The mock also reports the real
`bandwidth` from the shim, and `fee: None`.

`SPEC.md:90` and `README.md:453` already disclaim the shim.

**Solana RPC is unreachable.** Every write path is `Unimplemented`, so no cost and no estimate can
be produced there. Tracked already at `SPEC.md:157`.

## Testing

Each phase ships with tests.

- Phase 1: on every VM and backend that can meter, a mutating op reports a nonzero `units`, and the
  reported figure is stable across runs on the mocks. On CosmWasm mock, assert `cost()` is `None`
  rather than zero, so a future change that starts reporting a fake zero fails.
- Phase 2: an estimate for an op is within a sane band of the cost that op actually reports when
  run. This is the test that catches an estimator wired to the wrong unit.
- Phase 3: `Exact` is honored (a limit below the real cost causes the expected out of gas failure);
  `Estimated` produces a limit above the observed cost. A per VM test that `Exact` cannot be
  constructed with the wrong unit is unnecessary: the type system enforces it.

## Risks

Phase 3 is a large mechanical diff across every call site, landing immediately after the tx_hash
refactor touched many of the same signatures. Sequence it after phases 1 and 2 are merged, not
concurrently.

CosmWasm `Simulate` is the only genuinely new network call in the design. Everything else reads a
value that is already computed or already on the wire.
