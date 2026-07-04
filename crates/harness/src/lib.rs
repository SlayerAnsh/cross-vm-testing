//! Property-style testing over a user-defined `(Ctx, World)` pair.
//!
//! The full trait and runner land in later tasks; this crate currently hosts the
//! deterministic rng and the opt-in per-op stats.

mod rng;
mod stats;

#[cfg(feature = "fuzz")]
pub use rng::sample_arbitrary;
pub use rng::{random_seed, sub_seed, Prng};
pub use stats::{op_label, OpStat, Stats};

#[doc(hidden)]
pub use stats::OpOutcome;
