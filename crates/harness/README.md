# harness-core

A standalone, mode-typed property-testing runner. You assemble operations into an `OpSetHarness`
over a user-defined `(Ctx, World)` pair, and that one harness drives four run shapes: fuzz,
invariant, endurance, and scenario. The crate has no notion of chains, VMs, or any particular
domain; it is consumed directly here as `harness-core`, and it is the extraction point behind
`cross-vm-framework`'s property-testing harness (see "Flagship consumer" below).

## Operations and the `OpSetHarness`

Two pieces are kept apart on purpose:

* `Ctx`: the live system under test, threaded by `&mut` through every step. For a chain framework
  this is a started multi-chain environment; for a plain function or data structure it can simply
  be `()`.
* `World`: persisted bookkeeping only (a shadow model, flags, identifiers learned so far). It holds
  no live handles, so a handle is rebuilt on demand from `Ctx` plus a stored identifier.

Each operation is a standalone struct that implements `DynOp<Ctx, World>`: its data fields plus its
own `apply` (run the operation), `kind` (its lowercase registered name), `clone_box`, and `to_data`
(its data as JSON, for reports and replay). You register each op into an `OpSetHarness` with one
`OpDef` (the kind name, a generator fn that builds a random instance, a decoder, and an optional
dynamic weight fn). Invariants follow the same shape via `DynInvariant`. The op struct derives
`Serialize`/`Deserialize`, and registration passes `decode_json_op::<TheOp, _, _>` as the decoder,
so every registered harness works with config, CLI, scenario, and replay. Adding an operation
touches one `OpDef`, and an op is unit-testable without a runner (call its `apply` directly).

`OpSetHarness` implements the internal `Harness` trait, the runner seam every mode drives through;
you rarely name it directly.

## The four modes

One `Harness` implementation drives every mode through a mode-typed `Runner<H, Mode>`:

* **Fuzz**: a short random sequence per case. `#[fuzz_runner]` fans one test out into one case per
  seed derivative, each with its own setup.
* **Invariant**: one long persisted random sequence, invariants checked along the way.
* **Endurance**: random operations at random wall clock delays until a deadline (or an operation
  count bound), with optional block progression, then a final invariant sweep.
* **Scenario**: a fixed, concrete operation or sequence (`run_case` / `run_scenario` / `replay`),
  the entry point for rstest matrices and for turning a recorded failure into a regression test.

## A condensed example

A pure function subject needs no live infrastructure at all, so `Ctx = ()`:

```rust,ignore
struct World {
    sut: SatCounter,
    model: i32,
}

/// Saturating add of `n`: one standalone operation carrying its own `apply`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Add {
    n: u8,
}

impl DynOp<(), World> for Add {
    fn kind(&self) -> &'static str { "add" }

    fn apply<'a>(
        &'a self,
        _ctx: &'a mut (),
        world: &'a mut World,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            world.sut.add(self.n).map_err(HarnessError::infra)?;
            world.model = (world.model + self.n as i32).min(u8::MAX as i32);
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<(), World>> { Box::new(self.clone()) }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

// `Sub` follows the same shape (kind "sub"), producing both an Accepted and a Rejected verdict.

/// Every chain state matches the shadow model: the one invariant of this harness.
#[derive(Debug, Clone)]
struct MatchesModel;

impl DynInvariant<(), World> for MatchesModel {
    fn check<'a>(&'a self, _ctx: &'a mut (), world: &'a World) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            if world.sut.value as i32 == world.model {
                CheckOutcome::Held
            } else {
                CheckOutcome::violated("sut/model mismatch")
            }
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<(), World>> { Box::new(self.clone()) }
}

fn gen_add(rng: &mut Prng, _world: &World) -> Box<dyn DynOp<(), World>> {
    Box::new(Add { n: rng.below(300) as u8 }) // wraps past 255 on purpose
}

fn counter_harness() -> OpSetHarness<(), World> {
    OpSetHarness::new()
        .register(OpDef::new("add", gen_add, decode_json_op::<Add, _, _>))
        // .register(OpDef::new("sub", gen_sub, decode_json_op::<Sub, _, _>))
        .invariant(Box::new(MatchesModel))
}

let mut r = Runner::fuzz(counter_harness(), 42);
r.setup((), fresh_world());
let report = r.run(200, None, 1).await;
assert!(report.passed(), "{:?}", report.failure);
```

The full runnable version, including the endurance and scenario modes over the same harness, lives
at `crates/harness/tests/pure_function.rs`, the proof that the crate needs no chain, no VM, and no
`cross-vm-framework` type to be useful. A second worked example, `crates/harness/tests/math.rs`,
fuzzes a small math library (a checked add, sub, mul, and divide calculator) against an `i64`
shadow model, shows a divide by zero surfacing as a legitimate rejection, and includes a test that
injects a bug and confirms the harness catches it.

## Features

* `fuzz`: enables `sample_arbitrary`, the zero-boilerplate, stateless generation path for any
  `arbitrary::Arbitrary` operation type.
* `serde`: adds `Serialize` to the outcome and stats types (`Coverage`, `InvCoverage`,
  `FailureKind`, `Stats`, `OpStat`), for JSON reports. Never `Deserialize`; reports are write-only.
* `macros`: re-exports the `#[fuzz_runner]` / `#[invariant_runner]` / `#[endurance_runner]`
  attribute macros from `harness-core-macros`, so a standalone consumer needs only this one crate.

## Determinism guarantee

Every run is driven by a `ChaCha8Rng` seeded from a plain `u64`. Same seed, same operation stream,
byte for byte, across platforms and toolchain versions. The base seed is set at `Runner`
construction (`Runner::fuzz(harness, seed)` and friends); `sub_seed(seed, case)` derives an
independent per case seed for the fuzz fan-out, so re-running a single flagged case in isolation
(by base seed plus case index) reproduces it exactly. `random_seed()` picks a fresh, OS-seeded
value for a run that wants a different sequence each time, and prints it so the run stays
reproducible by copying the value back as a fixed seed.

This reproducibility is what a failed `RunReport` hands back: its `seed`, `mode`, and full
operation `history` are enough to replay the exact sequence with `Runner::replay`, or to minimize
it with `Runner::shrink` / `shrink_with`. A change to the crate must never perturb the rng draw
order for an existing code path; the workspace's golden seed tests pin this by asserting a fixed
seed's operation stream is byte-identical across releases.

## Flagship consumer

`cross-vm-framework` is the reference consumer: it pins `Ctx` to a started multi-chain environment
(CosmWasm, EVM, Solana, Tron), adds a `classify` helper for turning a VM response into a `Verdict`,
and re-exports everything else from this crate unchanged. Read `cross-vm-framework`'s
`src/harness/mod.rs` for how a domain-specific `Ctx` is layered on top without touching the runner,
the rng, or the outcome types.
