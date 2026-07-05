//! Dyn-op registry: assemble a [`Harness`](crate::Harness) from standalone operation structs
//! instead of one enum plus match arms.
//!
//! Each operation is a struct implementing [`DynOp`] (its data plus its own `apply`). The
//! `'static` bounds throughout exist because a boxed trait object's type parameters must
//! outlive its (implicit `'static`) lifetime bound; `Ctx`/`World` types are owned in practice,
//! so the bounds cost nothing.

use core::fmt;
use core::future::Future;
use core::pin::Pin;

use crate::{CheckOutcome, HarnessError, Prng, Verdict};

/// Boxed future returned by the object-safe async methods in this module. Object safety
/// forbids `async fn` in the dyn traits, so implementations return `Box::pin(async move { .. })`.
pub type OpFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// One operation instance: its data plus its own apply. The dyn-registry counterpart of one
/// variant of an `Operation` enum plus that variant's match arm.
///
/// Implementors derive `Debug` (the failure dump and per-op stats bucket by the leading
/// `Debug` token, so the struct name becomes the op label) and `Clone`, and write `clone_box`
/// as `Box::new(self.clone())`.
pub trait DynOp<C: 'static, W: 'static>: fmt::Debug {
    /// Apply this operation against the live `ctx`, updating the persisted `world`. Same
    /// contract as [`Harness::apply`](crate::Harness::apply): `Ok` classifies the SUT response,
    /// `Err` is a confirmed bug or an infrastructure failure.
    fn apply<'a>(
        &'a self,
        ctx: &'a mut C,
        world: &'a mut W,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>>;

    /// Clone into a fresh box. Powers `Clone` for `Box<dyn DynOp<C, W>>`, which the runner
    /// needs for replay and shrinking.
    fn clone_box(&self) -> Box<dyn DynOp<C, W>>;
}

impl<C: 'static, W: 'static> Clone for Box<dyn DynOp<C, W>> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// One named property that must always hold: the dyn-registry counterpart of one variant of
/// an `Invariant` enum plus its match arm in `check`.
///
/// Implementors derive `Debug` (coverage buckets by the leading `Debug` token, so the struct
/// name becomes the invariant label) and `Clone`, and write `clone_box` as
/// `Box::new(self.clone())`. Return [`CheckOutcome::skipped`](crate::CheckOutcome::skipped)
/// while a precondition has not happened yet.
pub trait DynInvariant<C: 'static, W: 'static>: fmt::Debug {
    /// Check the invariant against the current (post-operation) state. Same contract as
    /// [`Harness::check`](crate::Harness::check).
    fn check<'a>(&'a self, ctx: &'a mut C, world: &'a W) -> OpFuture<'a, CheckOutcome>;

    /// Clone into a fresh box. Powers `Clone` for `Box<dyn DynInvariant<C, W>>`.
    fn clone_box(&self) -> Box<dyn DynInvariant<C, W>>;
}

impl<C: 'static, W: 'static> Clone for Box<dyn DynInvariant<C, W>> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Generator stored in an [`OpDef`]: build one random op of this kind from `rng`, state-aware
/// via the world (mirrors [`Harness::generate_op`](crate::Harness::generate_op)). A plain fn
/// pointer keeps generation deterministic in `(seed, world)`.
pub type GenerateFn<C, W> = fn(&mut Prng, &W) -> Box<dyn DynOp<C, W>>;

/// Dynamic selection weight stored in an [`OpDef`] (mirrors
/// [`Harness::weight`](crate::Harness::weight)): `0` excludes the kind while the state makes
/// it meaningless. Must be deterministic in `(ctx, world)`; it receives no rng by design.
pub type WeightFn<C, W> = fn(&C, &W) -> u32;

/// One registered operation kind: its name, its generator, and its dynamic weight. The
/// dyn-registry counterpart of one `OpKind` variant plus its `generate_op` and `weight` arms.
pub struct OpDef<C: 'static, W: 'static> {
    name: &'static str,
    generate: GenerateFn<C, W>,
    weight: WeightFn<C, W>,
}

fn weight_one<C, W>(_ctx: &C, _world: &W) -> u32 {
    1
}

impl<C: 'static, W: 'static> OpDef<C, W> {
    /// A new kind descriptor with the default weight of `1` (a uniform mix).
    pub fn new(name: &'static str, generate: GenerateFn<C, W>) -> Self {
        Self {
            name,
            generate,
            weight: weight_one::<C, W>,
        }
    }

    /// Override the dynamic weight (default `1`).
    pub fn with_weight(mut self, weight: WeightFn<C, W>) -> Self {
        self.weight = weight;
        self
    }

    /// The kind name: the `OpKind` value of [`OpSetHarness`] runs, the key config weights
    /// address, and the registry key.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Build one random op of this kind (calls the stored generator).
    pub fn generate(&self, rng: &mut Prng, world: &W) -> Box<dyn DynOp<C, W>> {
        (self.generate)(rng, world)
    }

    /// The kind's dynamic weight for the current state (calls the stored weight fn).
    pub fn weight(&self, ctx: &C, world: &W) -> u32 {
        (self.weight)(ctx, world)
    }
}
