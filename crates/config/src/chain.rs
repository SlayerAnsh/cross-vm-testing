//! [`ChainDecl`]: one `[[chain]]` declaration, plus a pure per-kind field-presence helper.
//!
//! This module never touches framework or chain-provider types; `kind` stays a `String` here
//! (the framework resolves it to a `ChainKind` at run time).

use serde::Deserialize;

/// One `[[chain]]` entry: the pool of chains a `RunConfig` may build via the framework's
/// `build_chain` factory. Unknown fields are a hard error (typo safety).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainDecl {
    /// Injection key into `MultiChainEnv`, and the value used in op fields (e.g. `chain = "eth"`).
    pub label: String,
    /// `"cosmwasm"` | `"evm"` | `"svm"` | `"tron"`; stays a raw string in this crate.
    pub kind: String,
    /// Canonical chain id (e.g. `"osmosis-1"`, `"1"`, `"devnet"`).
    pub chain_id: String,
    /// Human readable name; defaults to `label` when absent (resolved by the framework).
    pub name: Option<String>,
    /// Token symbol (e.g. `"OSMO"`, `"ETH"`, `"SOL"`, `"TRX"`); per-kind default resolved
    /// elsewhere.
    pub native_symbol: Option<String>,
    /// RPC endpoint; required when this chain's resolved target is `"rpc"`.
    pub rpc_url: Option<String>,
    /// Per-chain `"mock"` | `"rpc"` override of the top level `[env].target`.
    pub target: Option<String>,
    /// All kinds: multiplier applied to a gas/compute estimate to get the limit an op actually
    /// submits. Unlike `gas_price` (CosmWasm only), this is meaningful on every VM, since every
    /// VM has some notion of an estimate and a limit derived from it. Must be finite and `>= 1.0`
    /// (enforced by this crate's validation stage); the framework applies the `1.3` default when
    /// absent.
    pub gas_adjustment: Option<f64>,
    /// Free form metadata table passed through to `ChainSpecData`.
    pub params: Option<toml::Table>,
    /// CosmWasm only: address prefix (e.g. `"osmo"`).
    pub bech32_prefix: Option<String>,
    /// CosmWasm only: fee denom (e.g. `"uosmo"`).
    pub native_denom: Option<String>,
    /// CosmWasm only: indicative gas price in `native_denom` per gas unit.
    pub gas_price: Option<f64>,
    /// EVM/Tron only: revm hardfork name (e.g. `"cancun"`), parsed to `SpecId` elsewhere.
    pub spec_id: Option<String>,
    /// Solana only: websocket endpoint for subscriptions.
    pub ws_url: Option<String>,
    /// Solana only: commitment level name (e.g. `"finalized"`), parsed elsewhere.
    pub commitment: Option<String>,
    /// RPC transport selector, chosen at construction (not stored in a preset). `"http"` (the
    /// default when absent) on every kind; `"batch-http"` only on CosmWasm, where it merges
    /// concurrent JSON-RPC calls into one CometBFT batch. Validated per kind in `validate.rs`.
    pub transport: Option<String>,
    /// CosmWasm `batch-http` only: debounce window in milliseconds before a batch flushes. Valid
    /// only alongside `transport = "batch-http"`; the transport applies its own default when
    /// absent.
    pub batch_wait_ms: Option<u64>,
    /// CosmWasm `batch-http` only: max calls merged into one batch before an early flush. Valid
    /// only alongside `transport = "batch-http"`; the transport applies its own default when
    /// absent.
    pub batch_max_size: Option<usize>,
}

/// Returns the names of fields required for `decl.kind` that are absent from `decl`.
///
/// Pure, per-kind presence check with no orchestration: it does not know about `RunConfig` and
/// does not run automatically; returning an empty vec does not mean the declaration is
/// otherwise valid (e.g. `kind` could still be empty, or the label could collide with another
/// chain). Structural validation (calling this, plus label uniqueness, plus an empty-`kind`
/// check, plus selection checks) is wired up in the loader's structural-validation stage
/// (`validate::validate`).
pub fn missing_required_fields(decl: &ChainDecl) -> Vec<&'static str> {
    match decl.kind.as_str() {
        "cosmwasm" => {
            let mut missing = Vec::new();
            if decl.bech32_prefix.is_none() {
                missing.push("bech32_prefix");
            }
            if decl.native_denom.is_none() {
                missing.push("native_denom");
            }
            missing
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_decl(kind: &str) -> ChainDecl {
        ChainDecl {
            label: "x".into(),
            kind: kind.into(),
            chain_id: "1".into(),
            name: None,
            native_symbol: None,
            rpc_url: None,
            target: None,
            gas_adjustment: None,
            params: None,
            bech32_prefix: None,
            native_denom: None,
            gas_price: None,
            spec_id: None,
            ws_url: None,
            commitment: None,
            transport: None,
            batch_wait_ms: None,
            batch_max_size: None,
        }
    }

    #[test]
    fn cosmwasm_requires_bech32_and_denom() {
        let decl = base_decl("cosmwasm");
        let missing = missing_required_fields(&decl);
        assert_eq!(missing, vec!["bech32_prefix", "native_denom"]);
    }

    #[test]
    fn evm_requires_nothing_extra() {
        let decl = base_decl("evm");
        assert!(missing_required_fields(&decl).is_empty());
    }

    #[test]
    fn cosmwasm_with_fields_present_is_satisfied() {
        let mut decl = base_decl("cosmwasm");
        decl.bech32_prefix = Some("osmo".into());
        decl.native_denom = Some("uosmo".into());
        assert!(missing_required_fields(&decl).is_empty());
    }
}
