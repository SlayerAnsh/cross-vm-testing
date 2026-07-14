# Cross VM gas and cost reporting

Date: 2026-07-14
Status: shipped, all three phases. Phase 1 landed in PR #37; phases 2 and 3 landed together.

This document has been reconciled with what was actually built. Phase 3's Tron design and its list of
limit-taking ops were wrong as originally written and were rejected during implementation; the
sections below describe the shipped API, and each divergence is called out where it applies.

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

`estimate_*` methods on the concrete chains (`CwChain`, `EvmChain`, `SvmChain`, `TronChain`), never
on `AnyChain`. Each returns the same type the executed op reports, not a VM erased `Cost`, so a
forecast and a receipt are directly comparable in the VM's own terms:

| Chain | Methods | Returns |
| --- | --- | --- |
| `CwChain` | `estimate_store_code`, `estimate_instantiate`, `estimate_execute_contract`, `estimate_transfer_funds` | `Option<u64>` gas |
| `EvmChain` | `estimate_deploy_create`, `estimate_call`, `estimate_call_value` | `EvmGas` |
| `TronChain` | `estimate_deploy_create`, `estimate_call`, `estimate_call_value` | `TronResources` |
| `SvmChain` | `estimate_transaction` | `TransactionMetadata` |

Primitives per backend:

- EVM RPC: `alloy Provider::estimate_gas` (`eth_estimateGas`, already in scope, zero new plumbing).
- EVM mock: a revm transact that is not committed.
- CosmWasm RPC: `/cosmos.tx.v1beta1.Service/Simulate`. This is genuinely new. There was no simulate
  call anywhere in the crate.
- CosmWasm mock: `None`. No gas meter to simulate against.
- Solana mock: `LiteSVM::simulate_transaction`, which yields compute units and fee without
  committing. The simulated transaction carries the same `SetComputeUnitLimit` instruction a sent
  one does, so the estimate is for the transaction as it will actually be sent (see "Honest gaps":
  that instruction burns 150 CU of the budget it sets).
- Solana RPC: `Unimplemented`, like every other write path there.
- Tron RPC: `triggerconstantcontract`, which is already called by `static_call`; its `energy_used`
  field was discarded. **Not** `estimateenergy`: that endpoint is off unless the node operator
  enables it, mainnet TronGrid answers "this node does not support estimate energy", and Tron's own
  docs name `triggerconstantcontract` as the fallback for exactly that case.
- Tron mock: an uncommitted revm transact, reporting `TronCompute::Gas`, the unit it meters.

### Phase 3: configurable limits

Limits become a required per call argument on every mutating op. No defaulting fallback, matching
the codebase convention. The types are per VM, because "limit" is not the same quantity on each
chain and a shared `GasLimit::Exact(u64)` would silently mean gas on EVM, sun on Tron, and compute
units on Solana.

```rust
pub enum EvmGasLimit      { Exact(u64), Estimated }  // gas units
pub enum CwGasLimit       { Exact(u64), Estimated }  // gas units
pub enum SvmComputeBudget { Exact(u32), Estimated }  // compute units
pub enum TronLimit        { Fee(u64), Gas(u64), Estimated }  // sun (RPC) or EVM gas (mock)

pub struct TronEnergyPolicy {              // deploy_create only; NOT a per-tx cap
    pub consume_user_resource_percent: u8,
    pub origin_energy_limit: u64,
}
```

`Estimated` runs the phase 2 primitive and multiplies by the chain's `gas_adjustment`, a new chain
config field. It costs one extra round trip. It replaces the hardcoded `FEE_BUFFER = 2.0` in
`crates/cosmwasm/src/provider/rpc.rs`, so live CosmWasm fees are now roughly half what they were:
the buffer was a second factor applied on top of the limit, and the fee is now a pure function of
the resolved limit, with the adjustment reaching it exactly once.

**Divergence: Tron.** The `TronLimits { fee_limit, origin_energy_limit }` struct specced above was
rejected. It conflates two unrelated things:

- `origin_energy_limit` is not a cap on the transaction being sent. It is a field of java-tron's
  `DeployContract` that persists as a property of the *deployed contract*, capping the energy its
  owner will pay for other people's future calls to it. It does not exist on `call` at all, so it
  cannot sit in a type that `call` takes.
- It also cannot ship alone. `consume_user_resource_percent` was a second hardcode in the same
  deploy path, and it is what decides whether `origin_energy_limit` binds at all: at `100` the
  caller pays the whole energy bill, the owner pays none of it, and the limit never binds. Either
  field without the other is meaningless, so they ship as one `TronEnergyPolicy`, on `deploy_create`
  only.
- The per-tx cap that remains is unit tagged, because Tron's two backends meter different
  quantities: `Fee(sun)` is java-tron's `fee_limit` and only the live backend accepts it, `Gas` is
  an EVM gas budget and only the mock (a revm core, which has no energy and no energy price)
  accepts it. Handing a backend the other unit is an error, not a silently ignored cap and not a
  fabricated conversion.

**Divergence: which ops take a limit.** The list above was wrong in three places. The shipped set is
`store_code`, `instantiate`, `execute_contract`, `deploy_create`, `call`, `call_value`,
`send_transaction`, and `transfer_funds` on CosmWasm, EVM, and Solana. Excluded, on purpose:

- `add_program` / `add_program_at` take **no** budget. litesvm writes the program account straight
  into the account store; no transaction runs, so a compute cap constrains nothing.
- `TronChain::transfer_funds` takes **no** limit. A `TransferContract` has no `fee_limit` field: it
  runs no code, burns no energy, and pays only bandwidth for its bytes.
- `AnyChain::transfer_funds` takes **no** limit and always resolves `Estimated`. `Exact(n)` cannot
  survive the VM erasure with its meaning intact (gas on EVM, sun on Tron, compute units on
  Solana), which is the whole reason the limit types are per VM. A caller needing an exact limit
  downcasts to the concrete chain, the same escape hatch every contract op already uses.

This still churns every signature and every call site in `crates/` and `examples/`, including every
method the `CwExecuteFns` derive generates (each gains a trailing `gas: CwGasLimit`, after `funds`
on a `#[payable]` variant).

Constants deleted: CosmWasm's `15_000_000` / `400_000` / `300_000` / `200_000` and its
`FEE_BUFFER = 2.0`, EVM's `TX_GAS_LIMIT = 30_000_000` (`revm-common`), and Tron's
`DEFAULT_FEE_LIMIT`, `DEFAULT_ORIGIN_ENERGY_LIMIT`, and the unnamed `consume_user_resource_percent:
100` literal in its deploy path.

Config placement: `gas_adjustment` goes in chain config (`crates/config`), alongside the existing
CosmWasm only `gas_price`. It is policy, not a secret, so it does not belong in `.env`. It defaults
to `1.3` and is validated finite and `>= 1.0` at load time: a value below `1.0` puts the limit under
the estimate, which always runs out of gas, so it is a config error rather than a foolish but legal
choice. A caller who genuinely wants a limit under the estimate (to exercise the out-of-gas path)
says so with `Exact`. There is no upper bound: over-provisioning is wasteful, never broken.

## Honest gaps

These are load bearing. The API must not paper over them.

**CosmWasm mock returns `None`, and cannot run out of gas.** cw-multi-test does not meter gas at
all; its response is `{events, data}`. There is no number, and none can be synthesized without
simulating a real chain. `crates/cosmwasm/src/chains/info.rs` already says the mock does not charge
gas. The consequence for phase 3 is that a limit on that backend is inert: it is accepted and
ignored, `Exact(1)` executes exactly as `Exact(15_000_000)` does, and an out-of-gas failure is not
reproducible there at all, only against live RPC. The mock still takes the limit so that one script
runs unchanged on either backend.

**A Solana compute budget is charged against itself.** The cap is a `SetComputeUnitLimit`
instruction prepended to the transaction, and it invokes the compute-budget builtin, burning 150 CU
of the very budget it sets. So both what an `Exact` budget must cover and what an estimate reports
are figures for the transaction *as sent*, budget instruction included; estimates simulate it that
way rather than simulating the bare instruction list.

**Tron live writes are now capped much tighter than before.** They previously carried a blanket
1000 TRX `fee_limit`. `TronLimit::Estimated` prices the node's forecast energy into sun with only
the chain's `gas_adjustment` as headroom, so an op whose forecast undershoots what it actually burns
by more than that (a deploy, most plausibly) now fails `OUT_OF_ENERGY` where the blanket cap
absorbed it. This is the intended trade (a runaway op should not silently spend 1000 TRX), and
`TronLimit::Fee` is the escape hatch that states the old behavior directly.

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
be produced there, including `SvmChain::estimate_transaction`. Already tracked in `SPEC.md`.

## Fixed in passing

Phase 2 turned up a pre-existing bug in `TronRpcProvider::static_call`, which the estimator shares
an endpoint with. It read `constant_result[0]` out of the `triggerconstantcontract` response without
checking whether the call had failed, so a reverted or out-of-energy constant call handed the caller
empty `Bytes` as though they were the contract's answer. It is now an error. java-tron signals a
failed constant call two ways (`result.result: false` with a message, and `result.result: true`
*with* a message while still quoting an `energy_used`); both are treated as failures, by
`static_call` and by the estimator, where a reverting call is an error rather than an energy figure.

## Testing

Each phase ships with tests.

- Phase 1: on every VM and backend that can meter, a mutating op reports a nonzero `units`, and the
  reported figure is stable across runs on the mocks. On CosmWasm mock, assert `cost()` is `None`
  rather than zero, so a future change that starts reporting a fake zero fails.
- Phase 2: an estimate for an op is within a sane band of the cost that op actually reports when
  run. This is the test that catches an estimator wired to the wrong unit.
- Phase 3: `Exact` is honored (a limit below the real cost causes the expected out of gas failure);
  `Estimated` produces a limit above the observed cost. The type system rules out passing one VM's
  limit to another, so no test is needed for that. It does *not* rule out Tron's two units, which
  share a type: `TronLimit::Fee` on the mock and `TronLimit::Gas` on the live RPC backend are
  runtime errors, and each is tested. The out-of-gas half of this is untestable on the CosmWasm
  mock, which has no meter to exhaust (see "Honest gaps").

## Risks

Phase 3 was a large mechanical diff across every call site, landing immediately after the tx_hash
refactor touched many of the same signatures. It was sequenced after phases 1 and 2, not
concurrently.

CosmWasm `Simulate` is the only genuinely new network call in the design. Everything else reads a
value that was already computed or already on the wire.
