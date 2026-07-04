//! Stage 3 of the loader pipeline: shallow, key-level merging over the raw
//! [`toml::Value`] document, before typed deserialization.
//!
//! Two independent merges happen per `[profile.<name>]` table:
//!
//! 1. **Defaults merge.** `[defaults]` is shallow-merged into every profile table (profile
//!    keys win), then a per-mode allowlist strips defaulted keys that do not apply to that
//!    profile's `mode` (each strip emits a warning rather than surfacing as a hard
//!    `deny_unknown_fields` error later). Keys the profile itself set (not inherited from
//!    `[defaults]`) are never stripped; a genuine typo in the profile body still hard-errors
//!    at typed deserialize.
//! 2. **Env merge.** The profile's own `env` inline table (if any) shallow-merges over the
//!    top-level `[env]` table. A key present only in the override is inserted; a key that
//!    collides with the top-level table is resolved by [`ConfigExt::merge_env_entry`], whose
//!    default replaces the top-level slot with the override value wholesale. A domain
//!    extension can override the hook to deep-merge selected keys (for example, cross-vm
//!    merges its `targets` map label-wise). The result replaces the profile's `env` key, so by
//!    the time typed deserialization runs, `Profile::common().env` already holds the fully
//!    resolved effective environment for that profile (not just the override delta).
//!
//! `[defaults]` itself is consumed and removed from the document; it has no place in the
//! typed schema; it existing only to seed profiles during this stage.

use crate::ext::ConfigExt;
use crate::value::{Doc, DocMap};
use crate::ConfigError;

/// The profile keys shared by every mode (spec section 4.3); a defaulted key naming one of
/// these always applies, regardless of mode.
///
/// `mode` is included even though section 4.3 doesn't list it as a per-mode config value: it is
/// structural (the dispatcher in `build_run_config` needs it to pick a per-mode struct at all)
/// and must never be stripped, whether a profile set it itself or inherited it from
/// `[defaults]`. Without this, a `[defaults].mode` inherited by a profile that omits its own
/// `mode` would be copied in by the defaults merge below and then immediately stripped back out
/// by the very same allowlist check, since `mode` matched none of the mode-specific key lists
/// either.
const COMMON_KEYS: &[&str] = &[
    "mode",
    "seed",
    "check_every",
    "stats",
    "artifacts_dir",
    "json_report",
    "env",
    "shrink",
    "shrink_limit",
];

/// Returns the mode-specific keys for a known mode name, or `None` for an unrecognized mode
/// (in which case nothing is stripped; typed deserialize surfaces the "unknown mode" error).
fn mode_specific_keys(mode: &str) -> Option<&'static [&'static str]> {
    match mode {
        "fuzz" => Some(&["cases", "ops", "kinds", "weights"]),
        "invariant" => Some(&["ops", "kinds", "weights"]),
        "endurance" => Some(&[
            "duration",
            "max_ops",
            "base_delay",
            "max_delay",
            "advance_blocks",
            "block_jitter",
            "max_consecutive_infra",
            "heartbeat",
            "kinds",
            "weights",
        ]),
        "scenario" => Some(&["steps", "export_world"]),
        _ => None,
    }
}

/// Whether `key` applies to `mode`: always true for [`COMMON_KEYS`], true for an unrecognized
/// mode (so we never strip ahead of the real "unknown mode" error), otherwise a lookup in
/// that mode's specific key list.
fn key_applies_to_mode(key: &str, mode: &str) -> bool {
    if COMMON_KEYS.contains(&key) {
        return true;
    }
    match mode_specific_keys(mode) {
        Some(keys) => keys.contains(&key),
        None => true,
    }
}

/// Merges `[defaults]` into every `[profile.*]` table and each profile's `env` over the
/// top-level `[env]`, operating on the raw parsed document before typed deserialization.
///
/// Per-key env collisions are resolved through the domain extension `X`'s
/// [`ConfigExt::merge_env_entry`] hook; the [`NoExt`](crate::NoExt) default replaces the
/// colliding slot wholesale.
///
/// Returns the warnings emitted by the per-mode defaults allowlist strip (one per stripped
/// key), in profile-then-key order.
pub(crate) fn merge<V: Doc, X: ConfigExt>(root: &mut V) -> Result<Vec<String>, ConfigError> {
    let table = root.as_object_mut().ok_or_else(|| {
        ConfigError::Parse("the root of a config document must be a table".to_string())
    })?;

    let defaults: V::Map = match table.remove("defaults") {
        Some(v) if v.is_object() => v.into_object().expect("checked is_object"),
        Some(_) => return Err(ConfigError::Parse("`defaults` must be a table".to_string())),
        None => V::Map::new(),
    };

    let top_env: V::Map = match table.get("env") {
        Some(v) if v.is_object() => v.as_object().expect("checked is_object").clone(),
        Some(_) => return Err(ConfigError::Parse("`env` must be a table".to_string())),
        None => V::Map::new(),
    };

    let mut warnings = Vec::new();

    if let Some(profiles) = table.get_mut("profile").and_then(Doc::as_object_mut) {
        for (profile_name, profile_value) in profiles.iter_mut() {
            let profile_table = profile_value.as_object_mut().ok_or_else(|| {
                ConfigError::Parse(format!("`profile.{profile_name}` must be a table"))
            })?;

            // 1. Shallow-merge [defaults] into the profile; profile keys win, so only keys
            //    absent from the profile are copied in.
            let mut defaulted_keys: Vec<String> = Vec::new();
            for (key, value) in defaults.iter() {
                if !profile_table.contains_key(key) {
                    profile_table.insert(key.clone(), value.clone());
                    defaulted_keys.push(key.clone());
                }
            }

            // 2. Strip defaulted keys inapplicable to this profile's mode, warning per strip.
            if let Some(mode) = profile_table.get("mode").and_then(|m| m.as_str()) {
                let mode = mode.to_string();
                for key in defaulted_keys {
                    if !key_applies_to_mode(&key, &mode) {
                        profile_table.remove(&key);
                        warnings.push(format!(
                            "profile `{profile_name}`: default key `{key}` does not apply to mode `{mode}` and was stripped"
                        ));
                    }
                }
            }

            // 3. Merge this profile's `env` override (if any) over the top-level `[env]`,
            //    replacing the profile's `env` key with the fully merged result.
            let profile_env_override: Option<V::Map> = match profile_table.get("env") {
                Some(v) if v.is_object() => Some(v.as_object().expect("checked is_object").clone()),
                Some(_) => {
                    return Err(ConfigError::Parse(format!(
                        "`profile.{profile_name}.env` must be a table"
                    )))
                }
                None => None,
            };
            let merged_env = merge_env_tables::<V, X>(&top_env, profile_env_override.as_ref());
            if merged_env.is_empty() {
                profile_table.remove("env");
            } else {
                profile_table.insert("env".to_string(), V::from_object(merged_env));
            }
        }
    }

    Ok(warnings)
}

/// Shallow-merges a profile's `env` override table over the top-level `[env]` table.
///
/// The result starts as a clone of the top-level table. Each override key that is absent from
/// the top-level table is inserted; each key that collides is passed to
/// [`ConfigExt::merge_env_entry`] to resolve. The [`NoExt`](crate::NoExt) default hook replaces
/// the colliding slot with the override value wholesale (so `target`, `chains`, `params`, and
/// any other scalar or table are plain profile-wins overrides), while a domain extension can
/// deep-merge selected keys instead (for example cross-vm's label-wise `targets` merge).
///
/// A profile with no `env` override yields a clone of the top-level table unchanged, so every
/// profile ends up carrying the fully resolved effective environment.
fn merge_env_tables<V: Doc, X: ConfigExt>(
    top: &V::Map,
    profile_override: Option<&V::Map>,
) -> V::Map {
    let mut merged = top.clone();
    let Some(over) = profile_override else {
        return merged;
    };

    for (key, value) in over.iter() {
        match merged.get_mut(key) {
            Some(slot) => X::merge_env_entry(key, slot, value.clone()),
            None => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ext::NoExt;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn defaults_merged_under_profile_profile_wins_on_conflict() {
        let mut doc = parse(
            r#"
            [defaults]
            seed = 1
            check_every = 5

            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            seed = 99
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let profile = doc.get("profile").unwrap().get("p").unwrap();
        // Profile's own `seed` wins over the default.
        assert_eq!(profile.get("seed").unwrap().as_integer(), Some(99));
        // check_every was absent from the profile, so the default flows through.
        assert_eq!(profile.get("check_every").unwrap().as_integer(), Some(5));
    }

    #[test]
    fn defaults_key_inapplicable_to_mode_is_stripped_with_warning() {
        let mut doc = parse(
            r#"
            [defaults]
            seed = 1
            check_every = 5

            [profile.p]
            mode = "scenario"

              [[profile.p.steps]]
              op = "Ping"
            "#,
        );
        let warnings = merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let profile = doc.get("profile").unwrap().get("p").unwrap();
        // `check_every` is a common key, applies everywhere, so it survives.
        assert_eq!(profile.get("check_every").unwrap().as_integer(), Some(5));
        // `seed` is also a common key though; use a mode-specific default key instead to prove
        // stripping. Re-run with a fuzz-only default key against a scenario profile.
        let mut doc2 = parse(
            r#"
            [defaults]
            cases = 8

            [profile.p]
            mode = "scenario"

              [[profile.p.steps]]
              op = "Ping"
            "#,
        );
        let warnings2 = merge::<toml::Value, NoExt>(&mut doc2).unwrap();
        let profile2 = doc2.get("profile").unwrap().get("p").unwrap();
        assert!(profile2.get("cases").is_none(), "cases must be stripped");
        assert!(
            warnings2
                .iter()
                .any(|w| w.contains("cases") && w.contains("scenario")),
            "expected a strip warning, got: {warnings2:?}"
        );
        // The first doc had no mode-inapplicable default keys, so no warnings there.
        assert!(warnings.is_empty());
    }

    #[test]
    fn profile_env_overrides_env_scalars() {
        let mut doc = parse(
            r#"
            [env]
            target = "mock"
            chains = ["osmosis"]

            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            env = { target = "rpc" }
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let env = doc
            .get("profile")
            .unwrap()
            .get("p")
            .unwrap()
            .get("env")
            .unwrap();
        assert_eq!(env.get("target").unwrap().as_str(), Some("rpc"));
        // chains was not overridden, so the top-level value flows through.
        assert_eq!(
            env.get("chains").unwrap().as_array().unwrap()[0].as_str(),
            Some("osmosis")
        );
    }

    #[test]
    fn targets_map_is_whole_value_replaced_under_default_ext() {
        // The generic default hook replaces a colliding env key wholesale: with `NoExt`, a
        // profile `targets` override is NOT label-merged (label-wise merge lives in the
        // domain's own `ConfigExt` impl), so the top-level-only label disappears.
        let mut doc = parse(
            r#"
            [env]
            targets = { eth = "rpc", osmosis = "rpc" }

            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            env = { targets = { osmosis = "mock" } }
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let targets = doc
            .get("profile")
            .unwrap()
            .get("p")
            .unwrap()
            .get("env")
            .unwrap()
            .get("targets")
            .unwrap();
        // Whole-value override: only the override's labels remain.
        assert!(
            targets.get("eth").is_none(),
            "eth is not label-preserved under NoExt"
        );
        assert_eq!(targets.get("osmosis").unwrap().as_str(), Some("mock"));
    }

    #[test]
    fn profile_without_env_override_still_gets_top_level_env() {
        let mut doc = parse(
            r#"
            [env]
            target = "mock"

            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let env = doc
            .get("profile")
            .unwrap()
            .get("p")
            .unwrap()
            .get("env")
            .unwrap();
        assert_eq!(env.get("target").unwrap().as_str(), Some("mock"));
    }

    #[test]
    fn no_top_level_env_and_no_override_leaves_env_absent() {
        let mut doc = parse(
            r#"
            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let profile = doc.get("profile").unwrap().get("p").unwrap();
        assert!(profile.get("env").is_none());
    }

    #[test]
    fn user_set_unknown_key_in_profile_is_not_stripped() {
        // A key the user wrote directly (not from [defaults]) must survive merge and later
        // hard-error at typed deserialize; merge only strips defaulted keys.
        let mut doc = parse(
            r#"
            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            bogus = true
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let profile = doc.get("profile").unwrap().get("p").unwrap();
        assert_eq!(profile.get("bogus").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn defaults_mode_is_never_stripped_by_the_allowlist() {
        // A profile that omits its own `mode` but inherits one from [defaults], plus a
        // mode-specific default key, must retain both after the strip: `mode` is structural
        // and always allowed, and once it survives, the mode-specific key is allowed too.
        let mut doc = parse(
            r#"
            [defaults]
            mode = "fuzz"
            cases = 1
            ops = 1

            [profile.p]
            "#,
        );
        let warnings = merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let profile = doc.get("profile").unwrap().get("p").unwrap();
        assert_eq!(profile.get("mode").unwrap().as_str(), Some("fuzz"));
        assert_eq!(profile.get("cases").unwrap().as_integer(), Some(1));
        assert_eq!(profile.get("ops").unwrap().as_integer(), Some(1));
        assert!(warnings.is_empty());
    }

    #[test]
    fn profiles_own_mode_is_never_stripped_either() {
        // Even when the profile sets `mode` itself (not inherited from [defaults]), a
        // mode-inapplicable default key must be stripped, but `mode` itself must survive.
        let mut doc = parse(
            r#"
            [defaults]
            cases = 8

            [profile.p]
            mode = "scenario"

              [[profile.p.steps]]
              op = "Ping"
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let profile = doc.get("profile").unwrap().get("p").unwrap();
        assert_eq!(profile.get("mode").unwrap().as_str(), Some("scenario"));
        assert!(
            profile.get("cases").is_none(),
            "cases must still be stripped"
        );
    }

    #[test]
    fn malformed_targets_override_passes_through_to_typed_deserialize() {
        // Under the generic default hook, a non-table `targets` override is no longer a hard
        // error in the merge stage (that shape check moves into the domain's `ConfigExt`): it
        // simply overrides the slot, and typed deserialize rejects it downstream.
        let mut doc = parse(
            r#"
            [env]
            targets = { eth = "rpc" }

            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            env = { targets = "not-a-table" }
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        let targets = doc
            .get("profile")
            .unwrap()
            .get("p")
            .unwrap()
            .get("env")
            .unwrap()
            .get("targets")
            .unwrap();
        assert_eq!(targets.as_str(), Some("not-a-table"));
    }

    #[test]
    fn defaults_table_removed_from_document() {
        let mut doc = parse(
            r#"
            [defaults]
            seed = 1

            [profile.p]
            mode = "fuzz"
            cases = 1
            ops = 1
            "#,
        );
        merge::<toml::Value, NoExt>(&mut doc).unwrap();
        assert!(doc.get("defaults").is_none());
    }
}

#[cfg(test)]
mod ext_hook_tests {
    use super::*;
    use crate::ext::{ConfigExt, NoExt};

    /// An ext whose hook deep-merges the `nested` env key label-wise.
    #[derive(Debug, Clone, Default, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DeepNested {}
    impl ConfigExt for DeepNested {
        fn merge_env_entry<V: crate::Doc>(key: &str, slot: &mut V, incoming: V) {
            if key == "nested" {
                if let (Some(base), Some(inc)) =
                    (slot.as_object_mut(), incoming.clone().into_object())
                {
                    let keys: Vec<String> = inc.iter().map(|(k, _)| k.clone()).collect();
                    let mut inc = inc;
                    for k in keys {
                        let v = inc.remove(&k).expect("key came from this map");
                        base.insert(k, v);
                    }
                    return;
                }
            }
            *slot = incoming;
        }
    }

    fn doc(s: &str) -> toml::Value {
        toml::from_str(s).expect("valid toml")
    }

    #[test]
    fn default_hook_replaces_whole_value() {
        let mut root = doc(
            "[env]\n[env.nested]\na = 1\nb = 2\n\n[profile.p]\nmode = \"fuzz\"\ncases = 1\nops = 1\n[profile.p.env]\n[profile.p.env.nested]\na = 9\n",
        );
        merge::<toml::Value, NoExt>(&mut root).expect("merge succeeds");
        let merged = &root["profile"]["p"]["env"]["nested"];
        assert_eq!(merged.get("a").and_then(|v| v.as_integer()), Some(9));
        assert_eq!(
            merged.get("b"),
            None,
            "default hook replaces, not deep-merges"
        );
    }

    #[test]
    fn custom_hook_deep_merges_selected_key() {
        let mut root = doc(
            "[env]\n[env.nested]\na = 1\nb = 2\n\n[profile.p]\nmode = \"fuzz\"\ncases = 1\nops = 1\n[profile.p.env]\n[profile.p.env.nested]\na = 9\n",
        );
        merge::<toml::Value, DeepNested>(&mut root).expect("merge succeeds");
        let merged = &root["profile"]["p"]["env"]["nested"];
        assert_eq!(merged.get("a").and_then(|v| v.as_integer()), Some(9));
        assert_eq!(merged.get("b").and_then(|v| v.as_integer()), Some(2));
    }
}
