//! The [`ConfigExt`] domain-extension seam: how a domain layer plugs extra
//! top-level config sections and a domain validation pass into the otherwise
//! generic loader, plus the no-op [`NoExt`] used when no domain extends it.

use crate::{ConfigError, Doc, RunConfig};

/// Domain extension for the config schema: extra top-level sections plus
/// domain validation. `Self` deserializes from whatever top-level keys remain
/// after the generic loader removes `harness`, `env`, `defaults`, `profile`,
/// `suite`, and `replay`. Use `#[serde(deny_unknown_fields)]` on the
/// implementing struct so unknown top-level keys stay hard errors.
pub trait ConfigExt:
    Sized + Clone + core::fmt::Debug + serde::de::DeserializeOwned + Default + 'static
{
    /// Domain validation pass, runs after generic structural validation.
    fn validate(cfg: &RunConfig<Self>) -> Result<(), ConfigError> {
        let _ = cfg;
        Ok(())
    }
    /// Merge hook for one colliding key when a profile's `env` table is
    /// overlaid on the top-level `[env]`. Default: replace the slot.
    fn merge_env_entry<V: Doc>(key: &str, slot: &mut V, incoming: V) {
        let _ = key;
        *slot = incoming;
    }
}

/// The no-op extension: no extra top-level sections allowed.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoExt {}
impl ConfigExt for NoExt {}
