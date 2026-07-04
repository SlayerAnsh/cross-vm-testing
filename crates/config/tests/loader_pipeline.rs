//! End-to-end tests for the full five-stage loader pipeline (parse, interpolate, merge, typed
//! deserialize, structural validate), driven by checked-in fixtures under `tests/fixtures/`.
//!
//! Every test uses a deterministic `vars` closure over a fixed map, never `std::env`, so these
//! tests are hermetic and reproducible in any environment.

use cross_vm_config::ConfigError;

/// A fixed variable map; `good_full.toml` relies on every `${VAR:-default}` fallback firing,
/// so every lookup here returns `None` on purpose.
fn no_vars(_: &str) -> Option<String> {
    None
}

macro_rules! fixture {
    ($name:expr) => {
        include_str!(concat!("fixtures/", $name))
    };
}

#[test]
fn good_full_loads_successfully_with_no_warnings() {
    let cfg = cross_vm_config::from_toml_str(fixture!("good_full.toml"), &no_vars)
        .expect("good_full.toml should load cleanly");
    assert_eq!(cfg.harness.name, "vault");
    assert_eq!(cfg.ext.chain.len(), 3);
    assert_eq!(cfg.profiles.len(), 5);
    assert!(cfg.suites.contains_key("nightly"));
    assert!(
        cfg.warnings.is_empty(),
        "expected no defaults-strip warnings, got: {:?}",
        cfg.warnings
    );

    // Interpolation with a `:-default` fallback resolved for the eth chain's rpc_url.
    let eth = cfg.ext.chain.iter().find(|c| c.label == "eth").unwrap();
    assert_eq!(eth.rpc_url.as_deref(), Some("https://eth.llamarpc.com"));
}

#[test]
fn warn_defaults_stripped_loads_with_a_strip_warning() {
    let cfg = cross_vm_config::from_toml_str(fixture!("warn_defaults_stripped.toml"), &no_vars)
        .expect("a mode-inapplicable default must warn, not hard-error");
    assert_eq!(cfg.warnings.len(), 1);
    assert!(cfg.warnings[0].contains("cases"));
    assert!(cfg.warnings[0].contains("scenario"));
}

#[test]
fn bad_duplicate_label_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_duplicate_label.toml"), &no_vars).unwrap_err();
    assert!(matches!(err, ConfigError::Domain(_)), "unexpected: {err}");
    assert!(
        err.to_string().contains("duplicate chain label `eth`"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_missing_cosmwasm_field_errors() {
    let err = cross_vm_config::from_toml_str(fixture!("bad_missing_cosmwasm_field.toml"), &no_vars)
        .unwrap_err();
    assert!(matches!(err, ConfigError::Domain(_)), "unexpected: {err}");
    let message = err.to_string();
    assert!(
        message.contains("chain `osmosis`") && message.contains("missing required field(s)"),
        "unexpected error: {err}"
    );
    assert!(message.contains("bech32_prefix"), "unexpected error: {err}");
}

#[test]
fn bad_unknown_selection_label_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_unknown_selection_label.toml"), &no_vars)
            .unwrap_err();
    assert!(matches!(err, ConfigError::Domain(_)), "unexpected: {err}");
    assert!(
        err.to_string()
            .contains("env.chains references unknown chain label `osmosis`"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_unknown_target_label_errors() {
    let err = cross_vm_config::from_toml_str(fixture!("bad_unknown_target_label.toml"), &no_vars)
        .unwrap_err();
    assert!(matches!(err, ConfigError::Domain(_)), "unexpected: {err}");
    assert!(
        err.to_string()
            .contains("env.targets references unknown chain label `osmosis`"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_rpc_without_url_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_rpc_without_url.toml"), &no_vars).unwrap_err();
    assert!(matches!(err, ConfigError::Domain(_)), "unexpected: {err}");
    assert!(
        err.to_string()
            .contains("chain `eth` resolves to target `rpc` but has no `rpc_url`"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_cases_zero_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_cases_zero.toml"), &no_vars).unwrap_err();
    assert!(
        matches!(err, ConfigError::InvalidCases { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_empty_steps_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_empty_steps.toml"), &no_vars).unwrap_err();
    assert!(
        matches!(err, ConfigError::EmptySteps { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_endurance_missing_bound_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_endurance_missing_bound.toml"), &no_vars)
            .unwrap_err();
    assert!(
        matches!(err, ConfigError::EnduranceMissingBound { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_kinds_weights_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_kinds_weights.toml"), &no_vars).unwrap_err();
    assert!(
        matches!(err, ConfigError::KindsWeightsConflict { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_empty_chain_kind_errors() {
    let err = cross_vm_config::from_toml_str(fixture!("bad_empty_chain_kind.toml"), &no_vars)
        .unwrap_err();
    assert!(matches!(err, ConfigError::Domain(_)), "unexpected: {err}");
    assert!(
        err.to_string()
            .contains("chain `eth`: `kind` must not be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn defaults_mode_survives_the_strip_and_dispatches_as_fuzz() {
    let cfg = cross_vm_config::from_toml_str(fixture!("good_defaults_mode.toml"), &no_vars)
        .expect("a [defaults].mode inherited by a mode-less profile should still load cleanly");
    assert!(
        cfg.warnings.is_empty(),
        "mode and its mode-specific defaults must not be stripped, got warnings: {:?}",
        cfg.warnings
    );
    match cfg.profiles.get("p").expect("profile `p` must exist") {
        cross_vm_config::Profile::Fuzz(f) => {
            assert_eq!(f.cases, 1);
            assert_eq!(f.ops, 1);
        }
        other => panic!("expected a Fuzz profile (mode inherited from [defaults]), got {other:?}"),
    }
}

#[test]
fn replay_block_is_tolerated_and_ignored() {
    let cfg = cross_vm_config::from_toml_str(fixture!("good_with_replay.toml"), &no_vars)
        .expect("a top-level [replay] block must be tolerated, not rejected as an unknown field");
    assert_eq!(cfg.profiles.len(), 1);
}

#[test]
fn targets_env_key_merges_label_wise_while_other_keys_whole_replace() {
    // The one behavior that distinguishes `CrossVmExt::merge_env_entry` from the generic
    // whole-replace `NoExt` default: the `targets` env key merges label-by-label when a profile's
    // own `env.targets` collides with the top-level `[env].targets`, instead of replacing the
    // whole map. Top-level declares `eth`, profile `p` declares `osmosis`; both must survive.
    let cfg = cross_vm_config::from_toml_str(fixture!("good_targets_label_merge.toml"), &no_vars)
        .expect("good_targets_label_merge.toml should load cleanly");

    let profile = cfg.profiles.get("p").expect("profile `p` must exist");
    let env = profile
        .common()
        .env
        .as_ref()
        .expect("profile `p` carries a merged effective env");
    let spec = cross_vm_config::env_spec(env).expect("merged env must re-type into EnvSpec");
    let targets = spec.targets.expect("merged env must carry a `targets` map");

    // Label-wise merge: BOTH the top-level `eth` and the profile's `osmosis` are present. Under a
    // whole-value replace (`*slot = incoming`), the top-level `eth` label would vanish and this
    // assertion would fail.
    assert!(
        targets.contains_key("eth"),
        "top-level `eth` target label must survive the label-wise merge, got: {targets:?}"
    );
    assert!(
        targets.contains_key("osmosis"),
        "profile `osmosis` target label must be present after merge, got: {targets:?}"
    );
    assert_eq!(targets.get("eth"), Some(&cross_vm_config::TargetStr::Mock));
    assert_eq!(
        targets.get("osmosis"),
        Some(&cross_vm_config::TargetStr::Rpc)
    );

    // Discriminator: the non-`targets` scalar key `target` is whole-replaced by the profile
    // override (mock -> rpc), proving the hook special-cases only `targets` and does not
    // deep-merge every key.
    assert_eq!(
        spec.target,
        Some(cross_vm_config::TargetStr::Rpc),
        "a non-`targets` env key must be whole-replaced by the profile override"
    );
}

#[test]
fn bad_suite_missing_profile_errors() {
    let err = cross_vm_config::from_toml_str(fixture!("bad_suite_missing_profile.toml"), &no_vars)
        .unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::UnknownSuiteProfile { ref suite, ref profile }
            if suite == "nightly" && profile == "missing"
        ),
        "unexpected error: {err}"
    );
}
