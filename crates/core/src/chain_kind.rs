//! Which execution environment a provider targets.

use std::str::FromStr;

/// Which execution environment a provider targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainKind {
    /// CosmWasm chains driven by `cw-multi-test`.
    CosmWasm,
    /// EVM chains driven by `revm`.
    Evm,
    /// Solana (SVM) chains driven by `litesvm`.
    Svm,
    /// Tron chains driven by a revm-based mock (TVM) or a java-tron RPC backend.
    Tron,
}

impl core::fmt::Display for ChainKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            ChainKind::CosmWasm => "cosmwasm",
            ChainKind::Evm => "evm",
            ChainKind::Svm => "svm",
            ChainKind::Tron => "tron",
        };
        f.write_str(s)
    }
}

/// Error returned when parsing a string into a [`ChainKind`] fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown chain kind \"{input}\": expected one of {valid_values}")]
pub struct ParseChainKindError {
    /// The input string that failed to parse.
    pub input: String,
    /// A comma-separated list of valid chain kind values.
    pub valid_values: &'static str,
}

impl FromStr for ChainKind {
    type Err = ParseChainKindError;

    /// Parse a string into a [`ChainKind`], case-sensitive and lowercase.
    ///
    /// Valid inputs are `"cosmwasm"`, `"evm"`, `"svm"`, and `"tron"`.
    /// Returns a [`ParseChainKindError`] if the input is not recognized.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cosmwasm" => Ok(ChainKind::CosmWasm),
            "evm" => Ok(ChainKind::Evm),
            "svm" => Ok(ChainKind::Svm),
            "tron" => Ok(ChainKind::Tron),
            _ => Err(ParseChainKindError {
                input: s.to_string(),
                valid_values: "cosmwasm, evm, svm, tron",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_kind_displays() {
        assert_eq!(ChainKind::CosmWasm.to_string(), "cosmwasm");
        assert_eq!(ChainKind::Evm.to_string(), "evm");
        assert_eq!(ChainKind::Svm.to_string(), "svm");
        assert_eq!(ChainKind::Tron.to_string(), "tron");
    }

    #[test]
    fn chain_kind_round_trip() {
        let variants = [
            ChainKind::CosmWasm,
            ChainKind::Evm,
            ChainKind::Svm,
            ChainKind::Tron,
        ];

        for variant in variants {
            let s = variant.to_string();
            let parsed: ChainKind = s.parse().expect("should parse");
            assert_eq!(parsed, variant, "round-trip failed for {}", s);
        }
    }

    #[test]
    fn chain_kind_unknown_string() {
        let result: Result<ChainKind, _> = "move".parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.input, "move");
        assert!(
            err.to_string().contains("cosmwasm"),
            "error message should list valid values"
        );
    }
}
