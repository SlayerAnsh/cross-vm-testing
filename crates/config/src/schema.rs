//! The cross-vm env wire types: [`TargetStr`] (mock vs rpc) and [`EnvSpec`], the typed shape of
//! the `[env]` table that the generic loader keeps opaque.
//!
//! The generic layer stores `[env]` as a raw [`serde_json::Value`]; this crate re-types it into
//! [`EnvSpec`] on demand through [`crate::env_spec`] (a `serde_json::from_value` call), so a
//! malformed env table surfaces as a domain validation error rather than being silently accepted.

use serde::Deserialize;
use std::collections::BTreeMap;

/// `"mock"` | `"rpc"`, the target a chain or profile resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetStr {
    /// In-process mock VM.
    Mock,
    /// Live RPC endpoint.
    Rpc,
}

/// `[env]` (or a per-profile inline override): the environment request passed to the setup fn.
///
/// Deserialized from the generic layer's opaque env value (a [`serde_json::Value`]) via
/// [`crate::env_spec`]; `deny_unknown_fields` still rejects typos in the env table.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EnvSpec {
    /// Default `"mock"` | `"rpc"` for every declared chain, absent per-chain override.
    pub target: Option<TargetStr>,
    /// Per-label target override map (approved schema extension over the top level `target`).
    pub targets: Option<BTreeMap<String, TargetStr>>,
    /// Label subset of `[[chain]]` to use; omitted means all declared chains.
    pub chains: Option<Vec<String>>,
    /// Free form table passed through to the setup fn as `[env.params]`.
    pub params: Option<toml::Table>,
}
