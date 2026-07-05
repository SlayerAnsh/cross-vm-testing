//! [`SeedSpec`]: the config schema's representation of a fuzz/invariant/endurance run seed.

use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

/// A run seed as written in config: either a fixed non-negative integer, or a request for a
/// fresh random seed (spelled as the string `"random"` or any negative integer, mirroring the
/// existing `#[fuzz_runner(seed = -1)]` macro convention). Resolution to a concrete `u64`
/// happens at run time in the framework, which prints a "set seed = N to reproduce" line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedSpec {
    /// A concrete, reproducible seed.
    Fixed(u64),
    /// Pick a fresh seed per run.
    Random,
}

impl Default for SeedSpec {
    fn default() -> Self {
        SeedSpec::Fixed(0)
    }
}

struct SeedVisitor;

impl<'de> Visitor<'de> for SeedVisitor {
    type Value = SeedSpec;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "an integer seed, a negative integer for a random seed, or the string \"random\""
        )
    }

    fn visit_i64<E>(self, v: i64) -> Result<SeedSpec, E>
    where
        E: DeError,
    {
        if v < 0 {
            Ok(SeedSpec::Random)
        } else {
            Ok(SeedSpec::Fixed(v as u64))
        }
    }

    fn visit_u64<E>(self, v: u64) -> Result<SeedSpec, E>
    where
        E: DeError,
    {
        Ok(SeedSpec::Fixed(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<SeedSpec, E>
    where
        E: DeError,
    {
        if v == "random" {
            Ok(SeedSpec::Random)
        } else {
            Err(E::custom(format!(
                "invalid seed `{v}`, expected an integer or the string \"random\""
            )))
        }
    }
}

impl<'de> Deserialize<'de> for SeedSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SeedVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Wrap {
        #[serde(default)]
        seed: SeedSpec,
    }

    #[test]
    fn fixed_integer_parses() {
        let w: Wrap = toml::from_str("seed = 42").unwrap();
        assert_eq!(w.seed, SeedSpec::Fixed(42));
    }

    #[test]
    fn negative_integer_is_random() {
        let w: Wrap = toml::from_str("seed = -1").unwrap();
        assert_eq!(w.seed, SeedSpec::Random);
    }

    #[test]
    fn random_string_is_random() {
        let w: Wrap = toml::from_str("seed = \"random\"").unwrap();
        assert_eq!(w.seed, SeedSpec::Random);
    }

    #[test]
    fn default_is_fixed_zero() {
        let w: Wrap = toml::from_str("").unwrap();
        assert_eq!(w.seed, SeedSpec::Fixed(0));
    }

    #[test]
    fn invalid_string_errors() {
        let err = toml::from_str::<Wrap>("seed = \"nope\"").unwrap_err();
        assert!(err.to_string().contains("invalid seed"));
    }
}
