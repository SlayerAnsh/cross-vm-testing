//! A small, deterministic, seedable RNG for operation generation.
//!
//! Wraps `rand_chacha::ChaCha8Rng` rather than `thread_rng` so a run is fully reproducible
//! from its seed across platforms and toolchain versions, the property the replay path relies
//! on. The helper methods keep state-aware generation terse (`rng.range(1, balance)`,
//! `rng.index(users.len())`, `rng.weighted(&[40, 25, 20, 15])`).

use rand::{Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// A seeded pseudo-random generator passed to [`Harness::generate_op`](crate::Harness::generate_op),
/// which draws from it to fill an operation's data.
pub struct Prng(ChaCha8Rng);

impl Prng {
    /// Build a generator from a 64-bit seed. Same seed -> same stream.
    pub fn seed_from_u64(seed: u64) -> Self {
        Self(ChaCha8Rng::seed_from_u64(seed))
    }

    /// A value in `0..n`. Returns `0` when `n == 0` (no panic on empty ranges).
    pub fn below(&mut self, n: u128) -> u128 {
        if n == 0 {
            0
        } else {
            self.0.gen_range(0..n)
        }
    }

    /// A value in `lo..hi`. Returns `lo` when `hi <= lo`.
    pub fn range(&mut self, lo: u128, hi: u128) -> u128 {
        if hi <= lo {
            lo
        } else {
            self.0.gen_range(lo..hi)
        }
    }

    /// An index in `0..len`. Returns `0` when `len == 0`.
    pub fn index(&mut self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            self.0.gen_range(0..len)
        }
    }

    /// `true` with probability `p` (clamped to `[0, 1]`).
    pub fn chance(&mut self, p: f64) -> bool {
        let p = p.clamp(0.0, 1.0);
        self.0.gen_bool(p)
    }

    /// Pick a bucket index, weighted by `weights`. A zero-sum or empty slice returns `0`.
    ///
    /// Used to bias operation selection, e.g. `match rng.weighted(&[40, 25, 20, 15])`.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u64 = weights.iter().map(|&w| w as u64).sum();
        if total == 0 {
            return 0;
        }
        let mut pick = self.0.gen_range(0..total);
        for (i, &w) in weights.iter().enumerate() {
            let w = w as u64;
            if pick < w {
                return i;
            }
            pick -= w;
        }
        weights.len() - 1
    }

    /// Fill `buf` with random bytes (used by `sample_arbitrary` under the `fuzz` feature).
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        self.0.fill_bytes(buf);
    }
}

/// A fresh entropy-derived seed, for `#[fuzz_runner(seed = -1)]` and friends (a negative seed in a
/// runner macro means "pick one at random per run"). The chosen value is printed by the generated
/// test so a failure stays reproducible: copy it back as a fixed `seed`. Uses the standard library's
/// `RandomState` (OS-seeded), so it needs no extra dependency.
pub fn random_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// Derive an independent per-case seed from a base seed and a case index. The fuzz fan-out
/// (`#[fuzz_runner]`) seeds case `i`'s runner with `sub_seed(seed, i)`, so re-running a single
/// case (by base seed + index) reproduces it byte for byte.
pub fn sub_seed(seed: u64, case: usize) -> u64 {
    seed ^ (case as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// Generate any [`arbitrary::Arbitrary`] value from the seeded stream: the zero-boilerplate,
/// stateless generation path. Derive `Arbitrary` on an `Operation` and `generate_op` becomes one
/// line. Stateless means invalid operations are produced freely; `apply` must classify them.
#[cfg(feature = "fuzz")]
pub fn sample_arbitrary<T>(rng: &mut Prng) -> T
where
    T: for<'a> arbitrary::Arbitrary<'a>,
{
    let mut buf = [0u8; 256];
    rng.fill_bytes(&mut buf);
    let mut u = arbitrary::Unstructured::new(&buf);
    T::arbitrary(&mut u).unwrap_or_else(|_| {
        // `Unstructured` ran out of bytes for a large type; fall back to the empty-input value,
        // which `Arbitrary` guarantees to produce for every type.
        T::arbitrary(&mut arbitrary::Unstructured::new(&[])).expect("arbitrary from empty input")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        let mut a = Prng::seed_from_u64(42);
        let mut b = Prng::seed_from_u64(42);
        for _ in 0..100 {
            assert_eq!(a.below(1_000_000), b.below(1_000_000));
        }
    }

    #[test]
    fn empty_ranges_do_not_panic() {
        let mut r = Prng::seed_from_u64(1);
        assert_eq!(r.below(0), 0);
        assert_eq!(r.range(5, 5), 5);
        assert_eq!(r.index(0), 0);
    }

    #[test]
    fn weighted_respects_zero_weight() {
        let mut r = Prng::seed_from_u64(7);
        // Only bucket 2 has weight; every draw must land there.
        for _ in 0..200 {
            assert_eq!(r.weighted(&[0, 0, 1, 0]), 2);
        }
    }
}
