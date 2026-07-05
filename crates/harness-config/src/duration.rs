//! Humantime based serde adapters for [`std::time::Duration`].
//!
//! Durations in the config schema are always strings, parsed with the humantime grammar
//! (`"8h"`, `"500ms"`, `"1h 30m"`). Bare integers are rejected with a hint, so the TOML and
//! JSON input paths behave identically and unit ambiguity never has to be guessed at.

use serde::de::{Error as DeError, Visitor};
use std::fmt;
use std::time::Duration;

/// Parses a duration string with the humantime grammar, rejecting anything that is not a
/// string with a clear hint pointing at the expected shape.
struct DurationVisitor;

impl<'de> Visitor<'de> for DurationVisitor {
    type Value = Duration;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a duration string such as \"500ms\", \"8h\", or \"1h 30m\""
        )
    }

    fn visit_str<E>(self, v: &str) -> Result<Duration, E>
    where
        E: DeError,
    {
        humantime::parse_duration(v).map_err(|e| E::custom(format!("invalid duration `{v}`: {e}")))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Duration, E>
    where
        E: DeError,
    {
        Err(E::custom(format!(
            "duration must be a string (e.g. \"500ms\"), got bare integer `{v}`; write it as a string like \"500ms\""
        )))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Duration, E>
    where
        E: DeError,
    {
        Err(E::custom(format!(
            "duration must be a string (e.g. \"500ms\"), got bare integer `{v}`; write it as a string like \"500ms\""
        )))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Duration, E>
    where
        E: DeError,
    {
        Err(E::custom(format!(
            "duration must be a string (e.g. \"500ms\"), got bare number `{v}`; write it as a string like \"500ms\""
        )))
    }
}

/// Serde adapter for a required `std::time::Duration` field, used as
/// `#[serde(with = "crate::duration::humantime_duration")]`.
pub mod humantime_duration {
    use super::{Duration, DurationVisitor};
    use serde::{Deserializer, Serializer};

    /// Serializes a [`Duration`] as a humantime string (e.g. `"8h"`).
    pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // `humantime::format_duration` never fails; `collect_str` writes through a `Display`.
        serializer.collect_str(&humantime::format_duration(*value))
    }

    /// Deserializes a [`Duration`] from a humantime string, rejecting bare integers.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DurationVisitor)
    }
}

/// Serde adapter for an optional `Option<std::time::Duration>` field, used as
/// `#[serde(with = "crate::duration::humantime_opt")]`.
pub mod humantime_opt {
    use super::{Duration, DurationVisitor};
    use serde::{Deserialize, Deserializer, Serializer};

    /// A single-field newtype so `Option<Duration>` can reuse [`DurationVisitor`] through the
    /// standard `Option` deserialization machinery.
    struct Wrapped(Duration);

    impl<'de> Deserialize<'de> for Wrapped {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(DurationVisitor).map(Wrapped)
        }
    }

    /// Serializes an `Option<Duration>` as a humantime string, or omits it when `None`.
    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(d) => serializer.collect_str(&humantime::format_duration(*d)),
            None => serializer.serialize_none(),
        }
    }

    /// Deserializes an `Option<Duration>` from a humantime string, rejecting bare integers.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<Wrapped>::deserialize(deserializer).map(|opt| opt.map(|w| w.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Req {
        #[serde(with = "humantime_duration")]
        d: Duration,
    }

    #[derive(Deserialize)]
    struct Opt {
        #[serde(with = "humantime_opt", default)]
        d: Option<Duration>,
    }

    #[test]
    fn parses_humantime_string() {
        let parsed: Req = toml::from_str("d = \"8h\"").unwrap();
        assert_eq!(parsed.d, Duration::from_secs(8 * 3600));
    }

    #[test]
    fn rejects_bare_integer() {
        let err = toml::from_str::<Req>("d = 8").unwrap_err();
        assert!(err.to_string().contains("write it as a string"));
    }

    #[test]
    fn optional_duration_round_trips_none() {
        let parsed: Opt = toml::from_str("").unwrap();
        assert_eq!(parsed.d, None);
    }

    #[test]
    fn optional_duration_parses_some() {
        let parsed: Opt = toml::from_str("d = \"1s\"").unwrap();
        assert_eq!(parsed.d, Some(Duration::from_secs(1)));
    }
}
