//! [`build_chain`]: materializes one resolved [`ChainSpecData`] into an [`AnyChain`].
//!
//! **Default policy (pick-one-place, documented here):** `resolve_profile` is the *selection and
//! precedence* layer; it fills `name` (defaults to `label`) and `native_symbol` (per-kind
//! default) and resolves `target`, because those depend on the profile's environment and CLI
//! overrides. `build_chain` is the *last mile*, so it owns the three defaults that are pure
//! per-kind constants independent of any profile: `spec_id` (`cancun`), `commitment`
//! (`finalized`), and `gas_price` (`0.025`). A spec whose `resolve_profile` already parsed a
//! `spec_id`/`commitment` string, or set a `gas_price`, keeps that value; `build_chain` only
//! fills the gap when the field is `None`.
//!
//! **String interning.** The per-VM `*ChainInfo` structs store `&'static str` fields (required
//! for their `const` presets), but `ChainSpecData` holds owned `String`s sourced from config.
//! The private `intern` helper bridges the two with a thread-local cache: the first sighting of
//! a given string
//! `Box::leak`s it and caches the `&'static str`; every later call with an equal string returns
//! the cached pointer instead of leaking again. This is bounded, not unbounded, because a run
//! declares a fixed set of chains and fuzz re-invokes setup (and therefore `build_chain`) with
//! the same declared strings on every case/run — the cache saturates at "distinct strings ever
//! declared", not "chains built".

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cross_vm_core::{ChainKind, CrossVmError, WalletFactory};

use crate::any_chain::AnyChain;
use crate::harness::HarnessError;

use super::setup_request::{ChainSpecData, Target};

/// CosmWasm's `0.025` per spec section 4.6, applied here (not in `resolve`) since it is a pure
/// per-kind constant with no dependency on the profile or CLI. Only referenced by the `cw` build
/// arm, so gated behind that feature to stay dead-code-clean in `cli` builds without CosmWasm.
#[cfg(feature = "cw")]
const DEFAULT_GAS_PRICE: f64 = 0.025;

thread_local! {
    /// First-sighting-leaks, then-cached string interner backing the `&'static str` fields on
    /// the per-VM `*ChainInfo` structs. See the module docs for why this is bounded.
    static INTERN_CACHE: RefCell<HashMap<String, &'static str>> = RefCell::new(HashMap::new());
}

/// Interns `s`, leaking it to `&'static str` only the first time this exact string is seen
/// (checked via the thread-local cache); every later call with an equal string returns the
/// cached pointer. See the module docs for why this is a bounded amount of leaking, not
/// unbounded.
fn intern(s: &str) -> &'static str {
    INTERN_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(cached) = cache.get(s) {
            return *cached;
        }
        let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
        cache.insert(s.to_string(), leaked);
        leaked
    })
}

/// Parses one of the 15 revm hardfork short names (spec section 4.6) into the actual
/// `revm::primitives::hardfork::SpecId` constant. Several short names collide on one `SpecId`
/// because revm has no distinct variant for them (documented per-arm below); an unrecognized
/// name errors listing all 15 valid names.
///
/// Shared by the EVM and Tron build arms (the TVM mock also executes against a `revm`
/// hardfork), so the error's `kind` is reported as [`ChainKind::Evm`] regardless of which VM's
/// declaration failed to parse; `spec_id` is inherently an EVM/revm concept even when Tron
/// borrows it. Compiled only when `evm` or `tron` is on, since it is the sole place the framework
/// names `revm`'s `SpecId` (which now feature-gates behind those two VMs).
#[cfg(any(feature = "evm", feature = "tron"))]
pub fn parse_spec_id(s: &str) -> Result<revm::primitives::hardfork::SpecId, HarnessError> {
    use revm::primitives::hardfork::SpecId;
    match s {
        "frontier" => Ok(SpecId::FRONTIER),
        "homestead" => Ok(SpecId::HOMESTEAD),
        "tangerine" => Ok(SpecId::TANGERINE),
        "spurious" => Ok(SpecId::SPURIOUS_DRAGON),
        "byzantium" => Ok(SpecId::BYZANTIUM),
        // Constantinople was overwritten with Petersburg at the same activation block (the
        // EIP-1283 reentrancy issue found before mainnet activation); revm 41 keeps only
        // `PETERSBURG`, so both short names resolve to it.
        "constantinople" => Ok(SpecId::PETERSBURG),
        "petersburg" => Ok(SpecId::PETERSBURG),
        "istanbul" => Ok(SpecId::ISTANBUL),
        // Muir Glacier only delayed the difficulty bomb; no EVM opcode/behavior change, so
        // revm 41 has no distinct variant and `ISTANBUL` is semantically identical.
        "muir" => Ok(SpecId::ISTANBUL),
        "berlin" => Ok(SpecId::BERLIN),
        "london" => Ok(SpecId::LONDON),
        // Paris is The Merge; revm names the variant `MERGE`.
        "paris" => Ok(SpecId::MERGE),
        "shanghai" => Ok(SpecId::SHANGHAI),
        "cancun" => Ok(SpecId::CANCUN),
        "prague" => Ok(SpecId::PRAGUE),
        other => Err(HarnessError::infra(CrossVmError::Other {
            kind: ChainKind::Evm,
            reason: format!(
                "unknown spec_id \"{other}\": expected one of frontier, homestead, tangerine, \
                 spurious, byzantium, constantinople, petersburg, istanbul, muir, berlin, \
                 london, paris, shanghai, cancun, prague"
            ),
        })),
    }
}

/// Builds one [`AnyChain`] from a resolved [`ChainSpecData`], dispatching on `spec.kind`.
///
/// Each VM arm constructs the owned `*ChainInfo` from `spec`'s fields (interning the owned
/// `String`s to `&'static str` via `intern`), then calls `.mock(wallets)` or `.rpc(wallets)`
/// per `spec.target`, then `.into()` into an [`AnyChain`]. When `spec.kind`'s cargo feature is
/// off, the arm returns [`HarnessError::Infra`] instead of failing to compile.
pub fn build_chain(
    spec: &ChainSpecData,
    wallets: Rc<WalletFactory>,
) -> Result<AnyChain, HarnessError> {
    match spec.kind {
        ChainKind::CosmWasm => build_cosmwasm(spec, wallets),
        ChainKind::Evm => build_evm(spec, wallets),
        ChainKind::Svm => build_svm(spec, wallets),
        ChainKind::Tron => build_tron(spec, wallets),
    }
}

/// Common "feature not compiled in" error for a `build_chain` arm whose VM feature is off. Only
/// compiled when at least one VM feature is off, so it is never dead code under the default
/// (all four VMs on) build this crate ships and tests with.
#[cfg(not(all(feature = "cw", feature = "evm", feature = "solana", feature = "tron")))]
fn feature_not_compiled(kind: ChainKind) -> HarnessError {
    HarnessError::infra(CrossVmError::Other {
        kind,
        reason: "chain kind not compiled in (enable the feature)".to_string(),
    })
}

#[cfg(feature = "cw")]
fn build_cosmwasm(
    spec: &ChainSpecData,
    wallets: Rc<WalletFactory>,
) -> Result<AnyChain, HarnessError> {
    use cross_vm_cosmwasm::CosmosChainInfo;

    let bech32_prefix = spec.bech32_prefix.as_deref().unwrap_or_default();
    let native_denom = spec.native_denom.as_deref().unwrap_or_default();
    let gas_price = spec.gas_price.unwrap_or(DEFAULT_GAS_PRICE);

    let info = CosmosChainInfo {
        chain_id: intern(&spec.chain_id),
        name: intern(&spec.name),
        bech32_prefix: intern(bech32_prefix),
        native_denom: intern(native_denom),
        native_symbol: intern(&spec.native_symbol),
        gas_price,
        rpc_url: spec.rpc_url.as_deref().map(intern),
    };
    Ok(match spec.target {
        Target::Mock => info.mock(wallets).into(),
        Target::Rpc => info.rpc(wallets).into(),
    })
}

#[cfg(not(feature = "cw"))]
fn build_cosmwasm(
    _spec: &ChainSpecData,
    _wallets: Rc<WalletFactory>,
) -> Result<AnyChain, HarnessError> {
    Err(feature_not_compiled(ChainKind::CosmWasm))
}

#[cfg(feature = "evm")]
fn build_evm(spec: &ChainSpecData, wallets: Rc<WalletFactory>) -> Result<AnyChain, HarnessError> {
    use cross_vm_solidity::EvmChainInfo;
    use revm::primitives::hardfork::SpecId;

    // Validate + parse the raw `spec_id` name here (the VM-crate-gated arm); default to `cancun`.
    let spec_id = match spec.spec_id.as_deref() {
        Some(s) => parse_spec_id(s)?,
        None => SpecId::CANCUN,
    };

    let info = EvmChainInfo {
        chain_id: intern(&spec.chain_id),
        name: intern(&spec.name),
        spec_id,
        native_symbol: intern(&spec.native_symbol),
        rpc_url: spec.rpc_url.as_deref().map(intern),
    };
    Ok(match spec.target {
        Target::Mock => info.mock(wallets).into(),
        Target::Rpc => info.rpc(wallets).into(),
    })
}

#[cfg(not(feature = "evm"))]
fn build_evm(_spec: &ChainSpecData, _wallets: Rc<WalletFactory>) -> Result<AnyChain, HarnessError> {
    Err(feature_not_compiled(ChainKind::Evm))
}

#[cfg(feature = "solana")]
fn build_svm(spec: &ChainSpecData, wallets: Rc<WalletFactory>) -> Result<AnyChain, HarnessError> {
    use cross_vm_solana::{Commitment, SolanaChainInfo};

    // Validate + parse the raw `commitment` name here (the VM-crate-gated arm); default to
    // `finalized`. A bad value errors with the valid-names message the `Commitment` parser emits.
    let commitment = match spec.commitment.as_deref() {
        Some(s) => s.parse::<Commitment>().map_err(|e| {
            HarnessError::infra(CrossVmError::Other {
                kind: ChainKind::Svm,
                reason: format!("chain `{}`: {e}", spec.label),
            })
        })?,
        None => Commitment::Finalized,
    };

    let info = SolanaChainInfo {
        chain_id: intern(&spec.chain_id),
        name: intern(&spec.name),
        native_symbol: intern(&spec.native_symbol),
        rpc_url: spec.rpc_url.as_deref().map(intern),
        ws_url: spec.ws_url.as_deref().map(intern),
        commitment,
    };
    Ok(match spec.target {
        Target::Mock => info.mock(wallets).into(),
        Target::Rpc => info.rpc(wallets).into(),
    })
}

#[cfg(not(feature = "solana"))]
fn build_svm(_spec: &ChainSpecData, _wallets: Rc<WalletFactory>) -> Result<AnyChain, HarnessError> {
    Err(feature_not_compiled(ChainKind::Svm))
}

#[cfg(feature = "tron")]
fn build_tron(spec: &ChainSpecData, wallets: Rc<WalletFactory>) -> Result<AnyChain, HarnessError> {
    use cross_vm_tron::TronChainInfo;
    use revm::primitives::hardfork::SpecId;

    // Validate + parse the raw `spec_id` name here (the VM-crate-gated arm); default to `cancun`.
    let spec_id = match spec.spec_id.as_deref() {
        Some(s) => parse_spec_id(s)?,
        None => SpecId::CANCUN,
    };

    let info = TronChainInfo {
        chain_id: intern(&spec.chain_id),
        name: intern(&spec.name),
        spec_id,
        native_symbol: intern(&spec.native_symbol),
        rpc_url: spec.rpc_url.as_deref().map(intern),
    };
    Ok(match spec.target {
        Target::Mock => info.mock(wallets).into(),
        Target::Rpc => info.rpc(wallets).into(),
    })
}

#[cfg(not(feature = "tron"))]
fn build_tron(
    _spec: &ChainSpecData,
    _wallets: Rc<WalletFactory>,
) -> Result<AnyChain, HarnessError> {
    Err(feature_not_compiled(ChainKind::Tron))
}

#[cfg(all(test, feature = "cli"))]
mod tests {
    use super::*;
    use crate::EmptyWallets;

    fn wallets() -> Rc<WalletFactory> {
        Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"))
    }

    fn base_spec(kind: ChainKind) -> ChainSpecData {
        ChainSpecData {
            label: "x".to_string(),
            kind,
            chain_id: "1".to_string(),
            name: "X".to_string(),
            native_symbol: "SYM".to_string(),
            rpc_url: Some("http://localhost:8545".to_string()),
            target: Target::Mock,
            params: toml::Table::new(),
            bech32_prefix: Some("osmo".to_string()),
            native_denom: Some("uosmo".to_string()),
            gas_price: Some(0.025),
            spec_id: Some("cancun".to_string()),
            ws_url: Some("ws://localhost:8900".to_string()),
            commitment: Some("finalized".to_string()),
        }
    }

    #[cfg(feature = "cw")]
    #[test]
    fn build_cosmwasm_mock() {
        let spec = base_spec(ChainKind::CosmWasm);
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::CosmWasm(_)));
    }

    #[cfg(feature = "cw")]
    #[test]
    fn build_cosmwasm_rpc() {
        let mut spec = base_spec(ChainKind::CosmWasm);
        spec.target = Target::Rpc;
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::CosmWasm(_)));
    }

    #[cfg(feature = "evm")]
    #[test]
    fn build_evm_mock() {
        let spec = base_spec(ChainKind::Evm);
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::Evm(_)));
    }

    #[cfg(feature = "evm")]
    #[test]
    fn build_evm_rpc() {
        let mut spec = base_spec(ChainKind::Evm);
        spec.target = Target::Rpc;
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::Evm(_)));
    }

    #[cfg(feature = "solana")]
    #[test]
    fn build_svm_mock() {
        let spec = base_spec(ChainKind::Svm);
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::Svm(_)));
    }

    #[cfg(feature = "solana")]
    #[test]
    fn build_svm_rpc() {
        let mut spec = base_spec(ChainKind::Svm);
        spec.target = Target::Rpc;
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::Svm(_)));
    }

    #[cfg(feature = "tron")]
    #[test]
    fn build_tron_mock() {
        let spec = base_spec(ChainKind::Tron);
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::Tron(_)));
    }

    #[cfg(feature = "tron")]
    #[test]
    fn build_tron_rpc() {
        let mut spec = base_spec(ChainKind::Tron);
        spec.target = Target::Rpc;
        let chain = build_chain(&spec, wallets()).expect("build_chain");
        assert!(matches!(chain, AnyChain::Tron(_)));
    }

    #[cfg(feature = "evm")]
    #[test]
    fn build_evm_rejects_bad_spec_id() {
        let mut spec = base_spec(ChainKind::Evm);
        spec.spec_id = Some("cancn".to_string());
        let Err(err) = build_chain(&spec, wallets()) else {
            panic!("bad spec_id must error");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("cancn"),
            "message should echo the bad input: {msg}"
        );
        // The message must list the valid hardfork names so the user can self-correct.
        assert!(
            msg.contains("cancun"),
            "message should list valid names: {msg}"
        );
        assert!(
            msg.contains("prague"),
            "message should list valid names: {msg}"
        );
    }

    #[cfg(feature = "tron")]
    #[test]
    fn build_tron_rejects_bad_spec_id() {
        let mut spec = base_spec(ChainKind::Tron);
        spec.spec_id = Some("cancn".to_string());
        let Err(err) = build_chain(&spec, wallets()) else {
            panic!("bad spec_id must error");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("cancn"),
            "message should echo the bad input: {msg}"
        );
        assert!(
            msg.contains("cancun"),
            "message should list valid names: {msg}"
        );
        assert!(
            msg.contains("prague"),
            "message should list valid names: {msg}"
        );
    }

    #[cfg(feature = "solana")]
    #[test]
    fn build_svm_rejects_bad_commitment() {
        let mut spec = base_spec(ChainKind::Svm);
        spec.commitment = Some("final".to_string());
        let Err(err) = build_chain(&spec, wallets()) else {
            panic!("bad commitment must error");
        };
        let msg = err.to_string();
        // Quoted form so this does not trivially pass on the "finalized" in the valid list.
        assert!(
            msg.contains("\"final\""),
            "message should echo the bad input: {msg}"
        );
        // The message must list the valid commitment names so the user can self-correct.
        assert!(
            msg.contains("finalized"),
            "message should list valid names: {msg}"
        );
        assert!(
            msg.contains("confirmed"),
            "message should list valid names: {msg}"
        );
        assert!(
            msg.contains("processed"),
            "message should list valid names: {msg}"
        );
    }

    #[cfg(any(feature = "evm", feature = "tron"))]
    #[test]
    fn parse_spec_id_accepts_all_15_names() {
        let names = [
            "frontier",
            "homestead",
            "tangerine",
            "spurious",
            "byzantium",
            "constantinople",
            "petersburg",
            "istanbul",
            "muir",
            "berlin",
            "london",
            "paris",
            "shanghai",
            "cancun",
            "prague",
        ];
        for name in names {
            assert!(parse_spec_id(name).is_ok(), "{name} should parse");
        }
    }

    #[cfg(any(feature = "evm", feature = "tron"))]
    #[test]
    fn parse_spec_id_rejects_unknown() {
        let err = parse_spec_id("bogus").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bogus"));
        assert!(msg.contains("cancun"));
    }

    #[cfg(any(feature = "evm", feature = "tron"))]
    #[test]
    fn parse_spec_id_tricky_mappings() {
        use revm::primitives::hardfork::SpecId;
        assert_eq!(parse_spec_id("tangerine").unwrap(), SpecId::TANGERINE);
        assert_eq!(parse_spec_id("spurious").unwrap(), SpecId::SPURIOUS_DRAGON);
        assert_eq!(parse_spec_id("muir").unwrap(), SpecId::ISTANBUL);
        assert_eq!(parse_spec_id("paris").unwrap(), SpecId::MERGE);
        assert_eq!(parse_spec_id("constantinople").unwrap(), SpecId::PETERSBURG);
    }

    #[test]
    fn intern_caches_repeated_strings_by_pointer() {
        let a = intern("same-string-xyz");
        let b = intern("same-string-xyz");
        assert!(
            std::ptr::eq(a, b),
            "repeated intern calls must return the same &'static str"
        );
    }
}
