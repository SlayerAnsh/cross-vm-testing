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

use crate::{HarnessError, Verdict};

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
