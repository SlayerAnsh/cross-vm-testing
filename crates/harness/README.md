# harness-core

A standalone, mode-typed property-testing runner. You implement one `Harness` trait over a
user-defined `(Ctx, World)` pair, and one implementation drives four run shapes: fuzz, invariant,
endurance, and scenario. The crate has no notion of chains, VMs, or any particular domain; it is
consumed directly here as `harness-core`, and it is the extraction point behind
`cross-vm-framework`'s property-testing harness (see "Flagship consumer" below).

## The `Harness` trait

Two associated types are kept apart on purpose:

* `Ctx`: the live system under test, threaded by `&mut` through every step. For a chain framework
  this is a started multi-chain environment; for a plain function or data structure it can simply
  be `()`.
* `World`: persisted bookkeeping only (a shadow model, flags, identifiers learned so far). It holds
  no live handles, so a handle is rebuilt on demand from `Ctx` plus a stored identifier.

Alongside those, an `Operation` enum, an `Invariant` enum, an `OpKind` enum (the data free
operation kinds), and the functions `apply` (run one operation), `generate_op` (build a random
instance of one kind), and `check` (evaluate one invariant). A provided default `generate` picks a
kind uniformly and calls `generate_op`; override it only to bias the kind mix.

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

struct CounterHarness;

impl Harness for CounterHarness {
    type Ctx = ();
    type World = World;
    type Operation = Op;
    type Invariant = Inv;
    type OpKind = OpKind;

    async fn apply(&self, _ctx: &mut (), world: &mut World, op: &Op) -> Result<Verdict, HarnessError> {
        match op {
            Op::Add(n) => {
                world.sut.add(*n).map_err(HarnessError::infra)?;
                world.model = (world.model + *n as i32).min(u8::MAX as i32);
                Ok(Verdict::Accepted)
            }
            Op::Sub(n) => {
                let expected_ok = world.model >= *n as i32;
                match (world.sut.sub(*n), expected_ok) {
                    (Ok(()), true) => { world.model -= *n as i32; Ok(Verdict::Accepted) }
                    (Ok(()), false) => Err(HarnessError::bug("underflow was accepted")),
                    (Err(reason), false) => Ok(Verdict::Rejected { reason }),
                    (Err(e), true) => Err(HarnessError::bug(format!("valid sub rejected: {e}"))),
                }
            }
        }
    }

    fn op_kinds(&self) -> Vec<OpKind> { vec![OpKind::Add, OpKind::Sub] }

    fn generate_op(&self, rng: &mut Prng, _world: &World, kind: OpKind) -> Op {
        let n = rng.below(300) as u8; // wraps past 255 on purpose
        match kind { OpKind::Add => Op::Add(n), OpKind::Sub => Op::Sub(n) }
    }

    fn invariants(&self) -> Vec<Inv> { vec![Inv::MatchesModel, Inv::NeverExceedsMax] }

    async fn check(&self, _ctx: &mut (), world: &World, inv: &Inv) -> CheckOutcome {
        match inv {
            Inv::MatchesModel if world.sut.value as i32 == world.model => CheckOutcome::Held,
            Inv::MatchesModel => CheckOutcome::violated("sut/model mismatch"),
            Inv::NeverExceedsMax => CheckOutcome::Held,
        }
    }
}

let mut r = Runner::fuzz(CounterHarness, 42);
r.setup((), fresh_world());
let report = r.run(200, None, 1).await;
assert!(report.passed(), "{:?}", report.failure);
```

The full runnable version, including the endurance and scenario modes over the same harness, lives
at `crates/harness/tests/pure_function.rs`, the proof that the crate needs no chain, no VM, and no
`cross-vm-framework` type to be useful.

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
