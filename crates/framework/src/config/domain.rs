//! The cross-vm [`CliDomain`]: adds `--target`/`--target-chain` flags and
//! builds the chain-aware [`SetupRequest`] from the opaque env plus the
//! `[[chain]]` declarations.
//!
//! This module owns the chain-resolution logic that used to live in the
//! framework's own `resolve_profile` (selection filtering, the mock-vs-rpc
//! target precedence funnel via [`cross_vm_config::resolve_chain_target`], and
//! per-kind defaulting) plus the replay-artifact `[[chain]]`/`[env]` section
//! rendering that used to live in the framework's own artifact writer. The
//! generic scalar resolution (`seed`/`shrink`/...) now lives in
//! [`harness_cli::resolve_profile`]; only the cross-vm chain shape is rebuilt
//! here, lazily, inside [`CrossVmDomain::build_setup`].

use std::collections::BTreeMap;

use cross_vm_config::{resolve_chain_target, RunConfig, TargetOverrides, TargetStr};
use cross_vm_core::ChainKind;
use harness_cli::{CliDomain, ResolvedProfile, SetupBuildError};

use super::setup_request::{ChainSpecData, SetupRequest, Target};

/// The gas-adjustment default, applied here (not in `build_chain`) because it is the one chain
/// default that is the same for every [`ChainKind`], so it never needs the VM-crate-gated arms:
/// resolving it once, kind-independently, keeps it out of the `#[cfg]` split entirely. Same reason
/// `default_native_symbol` is resolved here rather than in `build_chain`.
///
/// `1.3` is the value cosmjs and cw-orch converged on, and it is a deliberate cut from the
/// `FEE_BUFFER = 2.0` that CosmWasm's RPC provider hardcodes today: the Cosmos SDK deducts the
/// whole declared fee and refunds nothing, so `2.0` overpays by half again over `1.3` with no
/// stated reason for the extra headroom. The default is a single number for all four VMs rather
/// than a per-kind table. CosmWasm is the only VM where over-provisioning actually costs money,
/// and 1.3 is precisely the number its own ecosystem settled on for exactly that reason; on EVM
/// and Tron the unused headroom is refunded or never charged, and on Solana an over-sized compute
/// budget does not move the per-signature base fee. So a per-kind table would add a knob without
/// buying a difference, and a chain that genuinely needs another value declares one.
const DEFAULT_GAS_ADJUSTMENT: f64 = 1.3;

/// Cross-vm CLI flags added to `run`/`replay`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct TargetArgs {
    /// Blanket mock/rpc target override.
    #[arg(long, value_parser = parse_target)]
    pub target: Option<Target>,
    /// Per-chain target override, `LABEL=mock|rpc`, repeatable.
    #[arg(long = "target-chain", value_parser = parse_target_chain)]
    pub target_chain: Vec<(String, Target)>,
}

/// The cross-vm domain: cross-vm config extension, chain-aware setup
/// requests, target flags, `CROSS_VM_*` env vars.
pub struct CrossVmDomain;

impl CliDomain for CrossVmDomain {
    type Ext = cross_vm_config::CrossVmExt;
    type Setup = SetupRequest;
    type Args = TargetArgs;
    const BIN_NAME: &'static str = "cross-vm";
    const ABOUT: &'static str = "Config-driven cross-VM harness runner";
    const ENV_PREFIX: &'static str = "CROSS_VM";

    fn build_setup(
        cfg: &RunConfig,
        resolved: &ResolvedProfile,
        args: &TargetArgs,
        seed: u64,
    ) -> Result<SetupRequest, SetupBuildError> {
        resolve_chains(cfg, resolved, args).map(|(chain_specs, target, chains, params)| {
            SetupRequest {
                target,
                chains,
                chain_specs,
                params,
                seed,
            }
        })
    }

    fn artifact_sections(
        cfg: &RunConfig,
        resolved: &ResolvedProfile,
        args: &TargetArgs,
    ) -> toml::Table {
        // Best effort: an artifact for a run that got this far resolved its
        // chains once already, so re-resolution cannot fail here in practice;
        // fall back to no sections rather than failing the artifact write.
        match resolve_chains(cfg, resolved, args) {
            Ok((chain_specs, target, chains, _params)) => {
                render_artifact_sections(&chain_specs, target, &chains)
            }
            Err(_) => toml::Table::new(),
        }
    }

    fn overrides_json(args: &TargetArgs) -> serde_json::Map<String, serde_json::Value> {
        // Mirrors the old cross-vm CLI's `overrides_json` target-flag key set exactly: the blanket
        // `target` string, and (when any were given) a `target_chain` object of `LABEL -> mock|rpc`.
        // The generic scalar keys (`seed`/`ops`/...) are recorded by `harness_cli::overrides_json`;
        // this only adds the two cross-vm target flags.
        let mut map = serde_json::Map::new();
        if let Some(t) = args.target {
            map.insert("target".into(), target_label(t).into());
        }
        if !args.target_chain.is_empty() {
            let per_chain: serde_json::Map<String, serde_json::Value> = args
                .target_chain
                .iter()
                .map(|(label, target)| {
                    (
                        label.clone(),
                        serde_json::Value::from(target_label(*target)),
                    )
                })
                .collect();
            map.insert("target_chain".into(), serde_json::Value::Object(per_chain));
        }
        map
    }
}

/// Rebuilds the cross-vm chain shape for one resolved profile: the selection-filtered
/// [`ChainSpecData`] set (each with `kind` parsed and `target` resolved through
/// [`resolve_chain_target`]), the profile's own default target (used when no `[[chain]]` is
/// declared), the selected chain labels, and the merged `[env.params]` table.
///
/// This is the chain-resolution block that used to live in the framework's own `resolve_profile`,
/// moved here so the generic [`harness_cli::resolve_profile`] can stay chain-agnostic. Its inputs
/// are adapted: the `[[chain]]` declarations come from `cfg.ext.chain`, and the env comes from
/// re-typing the opaque `resolved.env` value into a [`cross_vm_config::EnvSpec`] via
/// [`cross_vm_config::env_spec`]. Every error is a config/usage mistake ([`SetupBuildError::Usage`]):
/// a malformed env table, a bad `kind`/`target` string, or a chain that resolves to `rpc` with no
/// `rpc_url` (re-asserted here since target resolution happens after the config crate's own
/// load-time validation).
fn resolve_chains(
    cfg: &RunConfig,
    resolved: &ResolvedProfile,
    args: &TargetArgs,
) -> Result<(Vec<ChainSpecData>, Target, Vec<String>, toml::Table), SetupBuildError> {
    let merged_env = cross_vm_config::env_spec(&resolved.env)
        .map_err(|e| SetupBuildError::Usage(e.to_string()))?;

    let overrides = TargetOverrides {
        per_chain: args
            .target_chain
            .iter()
            .map(|(label, target)| (label.clone(), target_to_str(*target)))
            .collect(),
        cli_target: args.target.map(target_to_str),
    };

    // Chain selection: `env.chains` (non-empty) filters the declared chains to that label subset;
    // omitted or empty means every declared chain.
    let selected = cfg
        .ext
        .chain
        .iter()
        .filter(|decl| match &merged_env.chains {
            Some(labels) if !labels.is_empty() => labels.contains(&decl.label),
            _ => true,
        });

    let mut chain_specs = Vec::new();
    for decl in selected {
        let kind: ChainKind = decl
            .kind
            .parse()
            .map_err(|e| SetupBuildError::Usage(format!("chain `{}`: {e}", decl.label)))?;

        let decl_target = decl
            .target
            .as_deref()
            .map(cross_vm_config::parse_target_str)
            .transpose()
            .map_err(|e| SetupBuildError::Usage(format!("chain `{}`: {e}", decl.label)))?;

        let target_str = resolve_chain_target(&decl.label, decl_target, &merged_env, &overrides);
        let target = Target::from(target_str);

        // `spec_id`/`commitment` are carried through as raw NAME strings; they are validated and
        // parsed into their VM-crate enums inside `build_chain`'s per-kind `#[cfg]`-gated arms, so
        // this module never references a VM-specific crate (that keeps `--features cli` composable
        // with any subset of {cw,evm,solana,tron}).
        let spec_id = decl.spec_id.clone();
        let commitment = decl.commitment.clone();

        let name_field = decl.name.clone().unwrap_or_else(|| decl.label.clone());
        let native_symbol = decl
            .native_symbol
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_native_symbol(kind).to_string());

        let rpc_url = decl.rpc_url.clone();
        if target == Target::Rpc && rpc_url.is_none() {
            return Err(SetupBuildError::Usage(format!(
                "chain `{}` resolves to target `rpc` but has no rpc_url",
                decl.label
            )));
        }

        chain_specs.push(ChainSpecData {
            label: decl.label.clone(),
            kind,
            chain_id: decl.chain_id.clone(),
            name: name_field,
            native_symbol,
            rpc_url,
            target,
            gas_adjustment: decl.gas_adjustment.unwrap_or(DEFAULT_GAS_ADJUSTMENT),
            params: decl.params.clone().unwrap_or_default(),
            bech32_prefix: decl.bech32_prefix.clone(),
            native_denom: decl.native_denom.clone(),
            gas_price: decl.gas_price,
            spec_id,
            ws_url: decl.ws_url.clone(),
            commitment,
            transport: decl.transport.clone(),
            batch_wait_ms: decl.batch_wait_ms,
            batch_max_size: decl.batch_max_size,
        });
    }

    // The profile's own default target: same precedence funnel, but with no specific chain label
    // in scope, so `overrides.per_chain` (label-keyed) cannot apply and `decl_target` is `None`;
    // only `overrides.cli_target` / `env.target` / the `Mock` default participate. This mirrors
    // `SetupRequest.target`: the fallback a setup fn uses when `chain_specs` is empty and it hard
    // codes its own chains.
    let default_overrides = TargetOverrides {
        per_chain: BTreeMap::new(),
        cli_target: overrides.cli_target,
    };
    let target = Target::from(resolve_chain_target(
        "",
        None,
        &merged_env,
        &default_overrides,
    ));

    let chains: Vec<String> = chain_specs.iter().map(|c| c.label.clone()).collect();
    let params = merged_env.params.clone().unwrap_or_default();

    Ok((chain_specs, target, chains, params))
}

/// Per-kind `native_symbol` default (spec section 4.6), applied here when a `[[chain]]`
/// declaration omits (or blanks) the field.
fn default_native_symbol(kind: ChainKind) -> &'static str {
    match kind {
        ChainKind::CosmWasm => "OSMO",
        ChainKind::Evm => "ETH",
        ChainKind::Svm => "SOL",
        ChainKind::Tron => "TRX",
    }
}

/// Converts the framework's [`Target`] back into the config crate's [`TargetStr`], the shape
/// [`resolve_chain_target`] and [`TargetOverrides`] need.
fn target_to_str(t: Target) -> TargetStr {
    match t {
        Target::Mock => TargetStr::Mock,
        Target::Rpc => TargetStr::Rpc,
    }
}

/// Parses a `--target` value: `"mock"` or `"rpc"`.
fn parse_target(s: &str) -> Result<Target, String> {
    match s {
        "mock" => Ok(Target::Mock),
        "rpc" => Ok(Target::Rpc),
        other => Err(format!(
            "invalid target `{other}`, expected \"mock\" or \"rpc\""
        )),
    }
}

/// Parses a `--target-chain` value: `LABEL=mock|rpc`, splitting on the first `=`.
fn parse_target_chain(s: &str) -> Result<(String, Target), String> {
    let (label, value) = s.split_once('=').ok_or_else(|| {
        format!("invalid --target-chain `{s}`: expected LABEL=mock|rpc (e.g. `eth=rpc`)")
    })?;
    let target = parse_target(value)?;
    Ok((label.to_string(), target))
}

/// `"mock"`/`"rpc"`, the JSON-friendly label for a [`Target`] (the inverse of [`parse_target`]).
fn target_label(t: Target) -> &'static str {
    match t {
        Target::Mock => "mock",
        Target::Rpc => "rpc",
    }
}

/// Rebuilds the replay artifact's `[[chain]]` and `[env]` top-level sections for the domain, as a
/// [`toml::Table`] with a `"chain"` array-of-tables key and an `"env"` table key. The generic
/// artifact writer ([`harness_cli::write_replay_artifact`]) merges these into the artifact document
/// before serialization; the byte shape here is exactly what the framework's own artifact writer
/// produced before this refactor (scalar fields first, per-kind `Option`s skipped when `None`,
/// `params` skipped when empty).
fn render_artifact_sections(
    chain_specs: &[ChainSpecData],
    target: Target,
    chains: &[String],
) -> toml::Table {
    let mut table = toml::Table::new();

    let chain_tables: Vec<toml::Value> = chain_specs
        .iter()
        .map(|spec| {
            toml::Value::try_from(artifact_chain(spec))
                .expect("ArtifactChain serializes into a TOML table")
        })
        .collect();
    table.insert("chain".to_string(), toml::Value::Array(chain_tables));

    let env = ArtifactEnv {
        target: target_str(target).to_string(),
        chains: chains.to_vec(),
    };
    table.insert(
        "env".to_string(),
        toml::Value::try_from(env).expect("ArtifactEnv serializes into a TOML table"),
    );

    table
}

/// `"mock"` / `"rpc"`, the artifact's own target-string spelling.
fn target_str(t: Target) -> &'static str {
    match t {
        Target::Mock => "mock",
        Target::Rpc => "rpc",
    }
}

/// One resolved [`ChainSpecData`] rendered into the artifact's `[[chain]]` shape: owned strings,
/// string-spelled enums (`kind`/`target`/`spec_id`/`commitment`), the resolved `rpc_url` (never a
/// secret), the resolved `gas_adjustment`. Per-kind `Option` fields serialize as absent (not
/// `null`, TOML has none) when `None`, via `skip_serializing_if`.
fn artifact_chain(spec: &ChainSpecData) -> ArtifactChain {
    ArtifactChain {
        label: spec.label.clone(),
        kind: spec.kind.to_string(),
        chain_id: spec.chain_id.clone(),
        name: spec.name.clone(),
        native_symbol: spec.native_symbol.clone(),
        rpc_url: spec.rpc_url.clone(),
        target: target_str(spec.target).to_string(),
        gas_adjustment: spec.gas_adjustment,
        bech32_prefix: spec.bech32_prefix.clone(),
        native_denom: spec.native_denom.clone(),
        gas_price: spec.gas_price,
        spec_id: spec.spec_id.clone(),
        ws_url: spec.ws_url.clone(),
        commitment: spec.commitment.clone(),
        params: spec.params.clone(),
    }
}

/// One `[[chain]]` entry of the replay artifact. Field order matters for the TOML output: scalar
/// and inline-array fields precede any nested-table field (`params`), since a TOML table's plain
/// `key = value` lines cannot follow a `[table]` header for that same table.
#[derive(Debug, serde::Serialize)]
struct ArtifactChain {
    label: String,
    kind: String,
    chain_id: String,
    name: String,
    native_symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc_url: Option<String>,
    target: String,
    /// Written unconditionally, unlike the `Option` fields around it, because by this point it is
    /// the *resolved* adjustment rather than the raw declaration. Pinning it into the artifact is
    /// the point: a replay then reproduces the run's actual limits even if `DEFAULT_GAS_ADJUSTMENT`
    /// later changes. It round-trips, since re-loading the artifact parses it back into
    /// `ChainDecl::gas_adjustment`.
    gas_adjustment: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    bech32_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_denom: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gas_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spec_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ws_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commitment: Option<String>,
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    params: toml::Table,
}

/// The replay artifact's `[env]` section: the resolved default target plus the exact selected chain
/// labels (`env.targets` is deliberately omitted, since every chain's resolved target is already
/// baked into its own `[[chain]].target`).
#[derive(Debug, serde::Serialize)]
struct ArtifactEnv {
    target: String,
    chains: Vec<String>,
}

#[cfg(all(test, feature = "cli"))]
mod tests {
    use super::*;

    fn load(toml: &str) -> RunConfig {
        cross_vm_config::from_toml_str(toml, &|_| None).expect("valid fixture")
    }

    /// Resolves `cfg`'s profile `name` (default CLI overrides), then rebuilds the cross-vm chain
    /// shape via [`resolve_chains`] with the given [`TargetArgs`]. Mirrors what
    /// [`CrossVmDomain::build_setup`] does, so these tests exercise the moved chain-resolution
    /// block directly.
    fn resolve_with(
        cfg: &RunConfig,
        name: &str,
        args: &TargetArgs,
    ) -> Result<(Vec<ChainSpecData>, Target, Vec<String>, toml::Table), SetupBuildError> {
        let resolved = harness_cli::resolve_profile(cfg, name, &harness_cli::RunOptions::default())
            .expect("resolves");
        resolve_chains(cfg, &resolved, args)
    }

    #[test]
    fn no_chain_declarations_yields_empty_chain_specs() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let (chain_specs, _target, chains, _params) =
            resolve_with(&cfg, "smoke", &TargetArgs::default()).unwrap();
        assert!(chain_specs.is_empty());
        assert!(chains.is_empty());
    }

    #[test]
    fn cli_target_chain_beats_env_targets_beats_decl_target() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"
target = "mock"
rpc_url = "http://localhost:8545"

[env]
targets = { eth = "mock" }

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let args = TargetArgs {
            target: None,
            target_chain: vec![("eth".to_string(), Target::Rpc)],
        };
        let (chain_specs, _target, _chains, _params) = resolve_with(&cfg, "smoke", &args).unwrap();
        assert_eq!(chain_specs.len(), 1);
        assert_eq!(chain_specs[0].target, Target::Rpc);
    }

    #[test]
    fn rpc_target_without_rpc_url_errors() {
        // No `target`/`rpc_url` on the declaration, so this loads fine (the config crate's own
        // load-time check resolves targets with no CLI overrides, i.e. stays `mock`). Forcing
        // `rpc` via a CLI override at resolve time is exactly the case the framework must
        // re-assert `rpc_url` for, since the config crate never sees CLI flags.
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let args = TargetArgs {
            target: Some(Target::Rpc),
            target_chain: Vec::new(),
        };
        let err = resolve_with(&cfg, "smoke", &args).unwrap_err();
        let SetupBuildError::Usage(msg) = err else {
            panic!("expected a Usage error, got {err:?}");
        };
        assert!(msg.contains("rpc_url"), "message was: {msg}");
    }

    #[test]
    fn chain_selection_filters_by_env_chains() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[[chain]]
label = "osmosis"
kind = "cosmwasm"
chain_id = "osmosis-1"
bech32_prefix = "osmo"
native_denom = "uosmo"

[env]
chains = ["eth"]

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let (chain_specs, _target, chains, _params) =
            resolve_with(&cfg, "smoke", &TargetArgs::default()).unwrap();
        assert_eq!(chain_specs.len(), 1);
        assert_eq!(chain_specs[0].label, "eth");
        assert_eq!(chains, vec!["eth".to_string()]);
    }

    #[test]
    fn native_symbol_defaults_per_kind_when_omitted() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let (chain_specs, _target, _chains, _params) =
            resolve_with(&cfg, "smoke", &TargetArgs::default()).unwrap();
        assert_eq!(chain_specs[0].native_symbol, "ETH");
    }

    #[test]
    fn gas_adjustment_defaults_when_omitted_and_is_honored_when_declared() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[[chain]]
label = "osmosis"
kind = "cosmwasm"
chain_id = "osmosis-1"
bech32_prefix = "osmo"
native_denom = "uosmo"
gas_adjustment = 1.75

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let (chain_specs, _target, _chains, _params) =
            resolve_with(&cfg, "smoke", &TargetArgs::default()).unwrap();

        // Omitted on a non-CosmWasm chain: the default still applies, since `gas_adjustment` is
        // not CosmWasm-scoped the way `gas_price` is.
        assert_eq!(chain_specs[0].label, "eth");
        assert_eq!(chain_specs[0].gas_adjustment, DEFAULT_GAS_ADJUSTMENT);
        // Declared: carried through verbatim, never clamped or re-defaulted.
        assert_eq!(chain_specs[1].label, "osmosis");
        assert_eq!(chain_specs[1].gas_adjustment, 1.75);
    }

    #[test]
    fn artifact_pins_the_resolved_gas_adjustment() {
        // The artifact records the *resolved* adjustment even when the declaration omitted it, so
        // a replay reproduces this run's limits even if the default later changes.
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let (chain_specs, target, chains, _params) =
            resolve_with(&cfg, "smoke", &TargetArgs::default()).unwrap();
        let sections = render_artifact_sections(&chain_specs, target, &chains);
        let chain0 = sections["chain"].as_array().unwrap()[0].as_table().unwrap();
        assert_eq!(
            chain0["gas_adjustment"].as_float(),
            Some(DEFAULT_GAS_ADJUSTMENT)
        );
    }

    #[test]
    fn artifact_sections_render_chain_and_env_tables() {
        let cfg = load(
            r#"
[harness]
name = "vault"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let (chain_specs, target, chains, _params) =
            resolve_with(&cfg, "smoke", &TargetArgs::default()).unwrap();
        let sections = render_artifact_sections(&chain_specs, target, &chains);

        // `[[chain]]` is an array of one table; `[env]` carries the resolved target and labels.
        let chain_arr = sections["chain"].as_array().expect("chain array");
        assert_eq!(chain_arr.len(), 1);
        let chain0 = chain_arr[0].as_table().expect("chain table");
        assert_eq!(chain0["label"].as_str(), Some("eth"));
        assert_eq!(chain0["kind"].as_str(), Some("evm"));
        assert_eq!(chain0["target"].as_str(), Some("mock"));
        // A mock chain has no rpc_url, so the key is skipped entirely (not `null`).
        assert!(!chain0.contains_key("rpc_url"));

        let env = sections["env"].as_table().expect("env table");
        assert_eq!(env["target"].as_str(), Some("mock"));
        assert_eq!(
            env["chains"].as_array().expect("chains array").len(),
            1,
            "one selected chain label"
        );
    }
}
