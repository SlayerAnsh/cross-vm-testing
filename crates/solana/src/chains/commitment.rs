//! Commitment level requested from a (future) RPC endpoint.

use std::str::FromStr;

/// Commitment level requested from a (future) RPC endpoint. Metadata only for the mock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commitment {
    /// Processed by the node, not yet voted on.
    Processed,
    /// Confirmed by a supermajority.
    Confirmed,
    /// Finalized (rooted).
    Finalized,
}

impl core::fmt::Display for Commitment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Commitment::Processed => "processed",
            Commitment::Confirmed => "confirmed",
            Commitment::Finalized => "finalized",
        };
        f.write_str(s)
    }
}

/// Error returned when parsing a string into a [`Commitment`] fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown commitment level \"{input}\": expected one of {valid_values}")]
pub struct ParseCommitmentError {
    /// The input string that failed to parse.
    pub input: String,
    /// A comma-separated list of valid commitment levels.
    pub valid_values: &'static str,
}

impl FromStr for Commitment {
    type Err = ParseCommitmentError;

    /// Parse a string into a [`Commitment`], case-sensitive and lowercase.
    ///
    /// Valid inputs are `"processed"`, `"confirmed"`, and `"finalized"`.
    /// Returns a [`ParseCommitmentError`] if the input is not recognized.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "processed" => Ok(Commitment::Processed),
            "confirmed" => Ok(Commitment::Confirmed),
            "finalized" => Ok(Commitment::Finalized),
            _ => Err(ParseCommitmentError {
                input: s.to_string(),
                valid_values: "processed, confirmed, finalized",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commitment_displays() {
        assert_eq!(Commitment::Processed.to_string(), "processed");
        assert_eq!(Commitment::Confirmed.to_string(), "confirmed");
        assert_eq!(Commitment::Finalized.to_string(), "finalized");
    }

    #[test]
    fn commitment_round_trip() {
        let variants = [
            Commitment::Processed,
            Commitment::Confirmed,
            Commitment::Finalized,
        ];

        for variant in variants {
            let s = variant.to_string();
            let parsed: Commitment = s.parse().expect("should parse");
            assert_eq!(parsed, variant, "round-trip failed for {}", s);
        }
    }

    #[test]
    fn commitment_unknown_string() {
        let result: Result<Commitment, _> = "unknown".parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.input, "unknown");
        assert!(
            err.to_string().contains("processed"),
            "error message should list valid values"
        );
    }
}
