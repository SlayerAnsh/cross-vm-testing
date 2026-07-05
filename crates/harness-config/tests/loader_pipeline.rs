//! End-to-end tests for the full five-stage loader pipeline (parse, interpolate, merge, typed
//! deserialize, structural validate), driven by checked-in fixtures under `tests/fixtures/`.
//!
//! These exercise the generic layer only, with the no-op `NoExt` extension: every loader call is
//! `harness_config::from_toml_str::<harness_config::NoExt>(...)`. Chain-specific fixtures and the
//! error variants they triggered live in the cross-vm domain crate, not here.
//!
//! Every test uses a deterministic `vars` closure over a fixed map, never `std::env`, so these
//! tests are hermetic and reproducible in any environment.

use harness_config::ConfigError;

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
    let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("good_full.toml"),
        &no_vars,
    )
    .expect("good_full.toml should load cleanly");
    assert_eq!(cfg.harness.name, "vault");
    assert_eq!(cfg.profiles.len(), 5);
    assert!(cfg.suites.contains_key("nightly"));
    assert!(
        cfg.warnings.is_empty(),
        "expected no defaults-strip warnings, got: {:?}",
        cfg.warnings
    );

    // `[env]` is opaque to the generic layer and round-trips unknown keys unchanged.
    assert_eq!(cfg.env["target"], "mock");
    assert_eq!(cfg.env["chains"][0], "osmosis");
}

#[test]
fn warn_defaults_stripped_loads_with_a_strip_warning() {
    let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("warn_defaults_stripped.toml"),
        &no_vars,
    )
    .expect("a mode-inapplicable default must warn, not hard-error");
    assert_eq!(cfg.warnings.len(), 1);
    assert!(cfg.warnings[0].contains("cases"));
    assert!(cfg.warnings[0].contains("scenario"));
}

#[test]
fn bad_cases_zero_errors() {
    let err = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("bad_cases_zero.toml"),
        &no_vars,
    )
    .unwrap_err();
    assert!(
        matches!(err, ConfigError::InvalidCases { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_empty_steps_errors() {
    let err = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("bad_empty_steps.toml"),
        &no_vars,
    )
    .unwrap_err();
    assert!(
        matches!(err, ConfigError::EmptySteps { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_endurance_missing_bound_errors() {
    let err = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("bad_endurance_missing_bound.toml"),
        &no_vars,
    )
    .unwrap_err();
    assert!(
        matches!(err, ConfigError::EnduranceMissingBound { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn bad_kinds_weights_errors() {
    let err = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("bad_kinds_weights.toml"),
        &no_vars,
    )
    .unwrap_err();
    assert!(
        matches!(err, ConfigError::KindsWeightsConflict { ref profile } if profile == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn defaults_mode_survives_the_strip_and_dispatches_as_fuzz() {
    let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("good_defaults_mode.toml"),
        &no_vars,
    )
    .expect("a [defaults].mode inherited by a mode-less profile should still load cleanly");
    assert!(
        cfg.warnings.is_empty(),
        "mode and its mode-specific defaults must not be stripped, got warnings: {:?}",
        cfg.warnings
    );
    match cfg.profiles.get("p").expect("profile `p` must exist") {
        harness_config::Profile::Fuzz(f) => {
            assert_eq!(f.cases, 1);
            assert_eq!(f.ops, 1);
        }
        other => panic!("expected a Fuzz profile (mode inherited from [defaults]), got {other:?}"),
    }
}

#[test]
fn replay_block_is_tolerated_and_ignored() {
    let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("good_with_replay.toml"),
        &no_vars,
    )
    .expect("a top-level [replay] block must be tolerated, not rejected as an unknown field");
    assert_eq!(cfg.profiles.len(), 1);
}

#[test]
fn bad_suite_missing_profile_errors() {
    let err = harness_config::from_toml_str::<harness_config::NoExt>(
        fixture!("bad_suite_missing_profile.toml"),
        &no_vars,
    )
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

#[test]
fn unknown_top_level_key_is_rejected_by_noext() {
    let err = harness_config::from_toml_str::<harness_config::NoExt>(
        "[harness]\nname = \"h\"\n\n[[chain]]\nlabel = \"eth\"\n",
        &|_| None,
    )
    .expect_err("chain is not a generic key");
    let msg = err.to_string();
    assert!(
        msg.contains("chain"),
        "error names the offending key: {msg}"
    );
}

#[test]
fn env_round_trips_opaquely() {
    let cfg = harness_config::from_toml_str::<harness_config::NoExt>(
        "[harness]\nname = \"h\"\n\n[env]\ntarget = \"mock\"\n[env.params]\nusers = 2\n\n[profile.p]\nmode = \"fuzz\"\ncases = 1\nops = 1\n",
        &|_| None,
    )
    .expect("generic layer does not interpret env");
    assert_eq!(cfg.env["target"], "mock");
    assert_eq!(cfg.env["params"]["users"], 2);
}
