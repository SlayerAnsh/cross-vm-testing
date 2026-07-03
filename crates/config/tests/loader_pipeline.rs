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
    assert_eq!(cfg.chains.len(), 3);
    assert_eq!(cfg.profiles.len(), 5);
    assert!(cfg.suites.contains_key("nightly"));
    assert!(
        cfg.warnings.is_empty(),
        "expected no defaults-strip warnings, got: {:?}",
        cfg.warnings
    );

    // Interpolation with a `:-default` fallback resolved for the eth chain's rpc_url.
    let eth = cfg.chains.iter().find(|c| c.label == "eth").unwrap();
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
    assert!(
        matches!(err, ConfigError::DuplicateChainLabel { ref label } if label == "eth"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_missing_cosmwasm_field_errors() {
    let err = cross_vm_config::from_toml_str(fixture!("bad_missing_cosmwasm_field.toml"), &no_vars)
        .unwrap_err();
    assert!(
        matches!(err, ConfigError::MissingChainFields { ref label, .. } if label == "osmosis"),
        "unexpected error: {err}"
    );
    if let ConfigError::MissingChainFields { fields, .. } = &err {
        assert!(fields.iter().any(|f| f == "bech32_prefix"));
    }
}

#[test]
fn bad_unknown_selection_label_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_unknown_selection_label.toml"), &no_vars)
            .unwrap_err();
    assert!(
        matches!(err, ConfigError::UnknownChainSelection { ref label, .. } if label == "osmosis"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_unknown_target_label_errors() {
    let err = cross_vm_config::from_toml_str(fixture!("bad_unknown_target_label.toml"), &no_vars)
        .unwrap_err();
    assert!(
        matches!(err, ConfigError::UnknownTargetLabel { ref label, .. } if label == "osmosis"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_rpc_without_url_errors() {
    let err =
        cross_vm_config::from_toml_str(fixture!("bad_rpc_without_url.toml"), &no_vars).unwrap_err();
    assert!(
        matches!(err, ConfigError::MissingRpcUrl { ref label, .. } if label == "eth"),
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
    assert!(
        matches!(err, ConfigError::EmptyChainKind { ref label } if label == "eth"),
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
