//! Which execution environment a provider targets.

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
}
