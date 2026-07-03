//! [`ChainSpecData`] and [`SetupRequest`]: the types a config-driven setup fn receives.

use cross_vm_core::ChainKind;

use crate::harness::{Ctx, HarnessError};

/// Mock vs. RPC target for a chain, or the profile default. Framework-side counterpart of
/// [`cross_vm_config::TargetStr`] (that type stays in the pure config crate; this one is what
/// [`crate::config::build_chain`] and the rest of the framework match on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// In-process mock VM.
    Mock,
    /// Live RPC endpoint.
    Rpc,
}

impl From<cross_vm_config::TargetStr> for Target {
    fn from(t: cross_vm_config::TargetStr) -> Self {
        match t {
            cross_vm_config::TargetStr::Mock => Target::Mock,
            cross_vm_config::TargetStr::Rpc => Target::Rpc,
        }
    }
}

/// One resolved `[[chain]]` declaration: owned strings, parsed enums, target and defaults
/// already resolved by [`crate::config::resolve::resolve_profile`]. This is the framework's
/// input to [`crate::config::build_chain::build_chain`].
///
/// Per-kind fields (`bech32_prefix`, `native_denom`, `gas_price`, `spec_id`, `ws_url`,
/// `commitment`) are `Some` only when they apply to `kind`; the rest are `None`. See the module
/// docs on [`build_chain`](crate::config::build_chain) for exactly where each field's default is
/// applied (some in `resolve`, some in `build_chain`).
#[derive(Debug, Clone)]
pub struct ChainSpecData {
    /// Injection key into `MultiChainEnv`, and the value used in op fields (e.g. `chain = "eth"`).
    pub label: String,
    /// Which compiled VM backend this chain uses.
    pub kind: ChainKind,
    /// Canonical chain id (e.g. `"osmosis-1"`, `"1"`, `"devnet"`).
    pub chain_id: String,
    /// Human readable name; defaults to `label` when the declaration omits it.
    pub name: String,
    /// Token symbol (e.g. `"OSMO"`, `"ETH"`, `"SOL"`, `"TRX"`); per-kind default already
    /// resolved when the declaration omits it.
    pub native_symbol: String,
    /// RPC endpoint; required when `target` is [`Target::Rpc`] (re-asserted by `resolve`).
    pub rpc_url: Option<String>,
    /// This chain's resolved mock-vs-rpc target (the output of
    /// [`cross_vm_config::resolve_chain_target`], not a raw declaration field).
    pub target: Target,
    /// Free form metadata table, passed through to the setup fn verbatim.
    pub params: toml::Table,
    /// CosmWasm only: address prefix (e.g. `"osmo"`).
    pub bech32_prefix: Option<String>,
    /// CosmWasm only: fee denom (e.g. `"uosmo"`).
    pub native_denom: Option<String>,
    /// CosmWasm only: indicative gas price in `native_denom` per gas unit. `None` means the
    /// declaration omitted it; [`build_chain`](crate::config::build_chain::build_chain) applies
    /// the `0.025` default.
    pub gas_price: Option<f64>,
    /// EVM/Tron only: the raw hardfork NAME string (e.g. `"cancun"`), carried verbatim from the
    /// declaration. It is validated + parsed into a `revm` `SpecId` inside the EVM/Tron arms of
    /// [`build_chain`](crate::config::build_chain::build_chain) (the only place that names a VM
    /// crate), so this type stays VM-crate-free and `--features cli` composes with any VM subset.
    /// `None` means the declaration omitted `spec_id`; `build_chain` applies the `cancun` default.
    pub spec_id: Option<String>,
    /// Solana only: websocket endpoint for subscriptions.
    pub ws_url: Option<String>,
    /// Solana only: the raw commitment-level NAME string (e.g. `"finalized"`), carried verbatim
    /// from the declaration. It is validated + parsed into a `cross_vm_solana::Commitment` inside
    /// the Solana arm of [`build_chain`](crate::config::build_chain::build_chain). `None` means the
    /// declaration omitted `commitment`; `build_chain` applies the `finalized` default.
    pub commitment: Option<String>,
}

/// The fully assembled input to a config-driven setup fn.
pub struct SetupRequest {
    /// The profile's default target (used when `chain_specs` is empty and the setup fn hard
    /// codes its own chains).
    pub target: Target,
    /// The requested chain label subset; empty means every declared `[[chain]]`.
    pub chains: Vec<String>,
    /// Resolved, selection-filtered chain specs; empty means the config file declared no
    /// `[[chain]]` entries at all, so the setup fn hard codes chains exactly as it does today.
    pub chain_specs: Vec<ChainSpecData>,
    /// `[env.params]` (or the profile's merged override of it), passed through verbatim.
    pub params: toml::Table,
    /// The run seed, already resolved to a concrete value (per-case for fuzz).
    pub seed: u64,
}

/// A boxed, pinned future returning the `(Ctx, World)` pair a config-driven setup fn builds.
/// The lifetime lets the future borrow from the closure that produced it (e.g. a captured
/// `SetupRequest`).
pub type SetupFuture<'a, W> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(Ctx, W), HarnessError>> + 'a>>;
