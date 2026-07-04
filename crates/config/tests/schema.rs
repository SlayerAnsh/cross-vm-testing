//! End-to-end parsing tests for `cross-vm-config`, driven by plain string fixtures.
//!
//! These tests cover only what P1 implements: raw parse plus typed deserialize. Interpolation,
//! `[defaults]` merging, and structural validation are out of scope here (a later task).

use cross_vm_config::{ExpectStr, Profile, SeedSpec};
use std::time::Duration;

/// A closure that never resolves a variable; interpolation is not implemented yet, so its
/// behavior does not matter for these tests, only that the signature is accepted.
fn no_vars(_: &str) -> Option<String> {
    None
}

const FULL_EXAMPLE: &str = r#"
[harness]
name = "vault"                 # registry key, required
setup = "default"              # named setup, optional, defaults to "default"

[[chain]]                      # declare chains as data (section 4.6)
label = "osmosis"
kind = "cosmwasm"
chain_id = "osmosis-1"
name = "Osmosis"
bech32_prefix = "osmo"
native_denom = "uosmo"
native_symbol = "OSMO"
gas_price = 0.025
rpc_url = "https://rpc.osmosis.zone:443"

[[chain]]
label = "eth"
kind = "evm"
chain_id = "1"
name = "Ethereum"
native_symbol = "ETH"
spec_id = "cancun"
rpc_url = "${ETH_RPC:-https://eth.llamarpc.com}"

[[chain]]
label = "solana"
kind = "svm"
chain_id = "devnet"
name = "Solana Devnet"
native_symbol = "SOL"
commitment = "finalized"
rpc_url = "https://api.devnet.solana.com"
ws_url = "wss://api.devnet.solana.com"

[env]                          # request passed to the setup fn
target = "mock"                # "mock" | "rpc", default "mock"
chains = ["osmosis", "eth", "solana"]   # label subset; omitted = all [[chain]]

[env.params]                   # free form table, the harness author defines meaning
users = 2
rpc_label = "${TARGET_CHAIN:-base}"

[defaults]                     # shallow merged under every profile
seed = 42
check_every = 1
stats = true
artifacts_dir = "target/cross-vm"

[profile.smoke]
mode = "fuzz"
cases = 8
ops = 20
kinds = ["Deposit", "Withdraw"]

[profile.deep]
mode = "fuzz"
cases = 200
ops = 50
seed = "random"
weights = { Deposit = 40, Withdraw = 25, Borrow = 20, Repay = 15 }
shrink = true

[profile.invariant-long]
mode = "invariant"
ops = 2000
check_every = 5

[profile.soak]
mode = "endurance"
duration = "8h"
max_ops = 100000               # stop on whichever bound hits first
base_delay = "500ms"
max_delay = "2s"
check_every = 25
advance_blocks = 1
block_jitter = 2
max_consecutive_infra = 5      # RPC flake tolerance, 0 fails on the first Infra
heartbeat = "60s"
env = { target = "rpc" }       # per profile env override, shallow over [env]

[profile.deploy-base]
mode = "scenario"
env = { target = "rpc", chains = ["eth"] }
export_world = "artifacts/deploy-world.json"   # later phase, see section 10

  [[profile.deploy-base.steps]]
  op = { Deposit = { chain = "eth", user = 0, amount = 1000 } }
  expect = "accepted"          # default
  delay = "2s"                 # pacing for live chains, default "0s"

  [[profile.deploy-base.steps]]
  op = { Withdraw = { chain = "eth", user = 0, amount = 2000 } }
  expect = "rejected"          # the model says this must revert
  check = false                # skip the invariant sweep after this step

[suite.nightly]
profiles = ["deep", "invariant-long", "soak"]
stop_on_failure = false
"#;

#[test]
fn full_example_parses_top_level_shape() {
    let cfg = cross_vm_config::from_toml_str(FULL_EXAMPLE, &no_vars).expect("should parse");

    assert_eq!(cfg.harness.name, "vault");
    assert_eq!(cfg.harness.setup, "default");
    assert_eq!(cfg.ext.chain.len(), 3);
    assert_eq!(cfg.ext.chain[0].kind, "cosmwasm");
    assert_eq!(cfg.ext.chain[0].label, "osmosis");

    let smoke = cfg.profiles.get("smoke").expect("smoke profile present");
    match smoke {
        Profile::Fuzz(f) => {
            assert_eq!(f.cases, 8);
            assert_eq!(f.ops, 20);
            assert_eq!(
                f.kinds.as_deref(),
                Some(["Deposit".to_string(), "Withdraw".to_string()].as_slice())
            );
        }
        other => panic!("expected Fuzz, got {other:?}"),
    }

    assert!(cfg.suites.contains_key("nightly"));
    assert_eq!(cfg.profiles.len(), 5);
}

#[test]
fn fuzz_minimal_table_parses() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "fuzz"
cases = 3
ops = 5
"#;
    let cfg = cross_vm_config::from_toml_str(toml, &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Fuzz(f) => {
            assert_eq!(f.cases, 3);
            assert_eq!(f.ops, 5);
            assert_eq!(f.kinds, None);
            assert_eq!(f.weights, None);
            assert_eq!(f.common.seed, SeedSpec::Fixed(0));
            assert_eq!(f.common.check_every, 1);
            assert!(!f.common.stats);
            assert_eq!(f.common.artifacts_dir, "target/cross-vm");
            assert_eq!(f.common.shrink_limit, 256);
        }
        other => panic!("expected Fuzz, got {other:?}"),
    }
}

#[test]
fn invariant_minimal_table_parses() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "invariant"
ops = 10
"#;
    let cfg = cross_vm_config::from_toml_str(toml, &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Invariant(inv) => {
            assert_eq!(inv.ops, 10);
            assert_eq!(inv.kinds, None);
        }
        other => panic!("expected Invariant, got {other:?}"),
    }
}

#[test]
fn endurance_minimal_table_parses() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "endurance"
duration = "1h"
"#;
    let cfg = cross_vm_config::from_toml_str(toml, &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Endurance(e) => {
            assert_eq!(e.duration, Some(Duration::from_secs(3600)));
            assert_eq!(e.max_ops, None);
            assert_eq!(e.base_delay, Duration::ZERO);
            assert_eq!(e.max_delay, Duration::ZERO);
            assert_eq!(e.block_jitter, 0);
            assert_eq!(e.max_consecutive_infra, 0);
            assert_eq!(e.heartbeat, Duration::from_secs(60));
        }
        other => panic!("expected Endurance, got {other:?}"),
    }
}

#[test]
fn scenario_minimal_table_parses() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "scenario"

  [[profile.p.steps]]
  op = "Ping"
"#;
    let cfg = cross_vm_config::from_toml_str(toml, &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Scenario(s) => {
            assert_eq!(s.steps.len(), 1);
            assert_eq!(s.steps[0].expect, ExpectStr::Accepted);
            assert_eq!(s.steps[0].delay, Duration::ZERO);
            assert!(s.steps[0].check);
        }
        other => panic!("expected Scenario, got {other:?}"),
    }
}

#[test]
fn unknown_key_in_profile_is_hard_error() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "fuzz"
cases = 1
ops = 1
bogus = true
"#;
    let err = cross_vm_config::from_toml_str(toml, &no_vars).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("bogus") || message.to_lowercase().contains("unknown field"),
        "expected an unknown-field error, got: {message}"
    );
}

#[test]
fn unknown_mode_is_hard_error() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "bogus_mode"
"#;
    let err = cross_vm_config::from_toml_str(toml, &no_vars).unwrap_err();
    assert!(err.to_string().contains("bogus_mode"));
}

#[test]
fn missing_mode_is_hard_error() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
cases = 1
"#;
    let err = cross_vm_config::from_toml_str(toml, &no_vars).unwrap_err();
    assert!(err.to_string().contains("mode"));
}

fn seeded_fuzz_toml(seed_line: &str) -> String {
    format!(
        r#"
[harness]
name = "h"

[profile.p]
mode = "fuzz"
cases = 1
ops = 1
{seed_line}
"#
    )
}

#[test]
fn seed_random_string_parses_to_random() {
    let cfg =
        cross_vm_config::from_toml_str(&seeded_fuzz_toml(r#"seed = "random""#), &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Fuzz(f) => assert_eq!(f.common.seed, SeedSpec::Random),
        other => panic!("expected Fuzz, got {other:?}"),
    }
}

#[test]
fn seed_negative_one_parses_to_random() {
    let cfg = cross_vm_config::from_toml_str(&seeded_fuzz_toml("seed = -1"), &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Fuzz(f) => assert_eq!(f.common.seed, SeedSpec::Random),
        other => panic!("expected Fuzz, got {other:?}"),
    }
}

#[test]
fn seed_fixed_integer_parses_to_fixed() {
    let cfg = cross_vm_config::from_toml_str(&seeded_fuzz_toml("seed = 42"), &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Fuzz(f) => assert_eq!(f.common.seed, SeedSpec::Fixed(42)),
        other => panic!("expected Fuzz, got {other:?}"),
    }
}

#[test]
fn duration_string_parses() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "endurance"
duration = "8h"
"#;
    let cfg = cross_vm_config::from_toml_str(toml, &no_vars).unwrap();
    match cfg.profiles.get("p").unwrap() {
        Profile::Endurance(e) => assert_eq!(e.duration, Some(Duration::from_secs(8 * 3600))),
        other => panic!("expected Endurance, got {other:?}"),
    }
}

#[test]
fn bare_integer_duration_is_hard_error() {
    let toml = r#"
[harness]
name = "h"

[profile.p]
mode = "endurance"
duration = 8
"#;
    let err = cross_vm_config::from_toml_str(toml, &no_vars).unwrap_err();
    assert!(err.to_string().contains("string"));
}

const PARITY_TOML: &str = r#"
[harness]
name = "vault"
setup = "default"

[[chain]]
label = "osmosis"
kind = "cosmwasm"
chain_id = "osmosis-1"
bech32_prefix = "osmo"
native_denom = "uosmo"
gas_price = 0.025

[env]
target = "mock"
chains = ["osmosis"]

[profile.smoke]
mode = "fuzz"
cases = 8
ops = 20
kinds = ["Deposit", "Withdraw"]
seed = 42

[profile.soak]
mode = "endurance"
duration = "8h"
base_delay = "500ms"

[suite.nightly]
profiles = ["smoke"]
"#;

const PARITY_JSON: &str = r#"
{
  "harness": { "name": "vault", "setup": "default" },
  "chain": [
    {
      "label": "osmosis",
      "kind": "cosmwasm",
      "chain_id": "osmosis-1",
      "bech32_prefix": "osmo",
      "native_denom": "uosmo",
      "gas_price": 0.025
    }
  ],
  "env": { "target": "mock", "chains": ["osmosis"] },
  "profile": {
    "smoke": {
      "mode": "fuzz",
      "cases": 8,
      "ops": 20,
      "kinds": ["Deposit", "Withdraw"],
      "seed": 42
    },
    "soak": {
      "mode": "endurance",
      "duration": "8h",
      "base_delay": "500ms"
    }
  },
  "suite": {
    "nightly": { "profiles": ["smoke"] }
  }
}
"#;

#[test]
fn json_input_parses_to_an_equal_run_config() {
    // `RunConfig` (the generic `harness_config::RunConfig<CrossVmExt>`) is not `PartialEq`, so
    // compare the loaded parts field-wise; TOML and JSON inputs must produce equal contents.
    let from_toml = cross_vm_config::from_toml_str(PARITY_TOML, &no_vars).unwrap();
    let from_json = cross_vm_config::from_json_str(PARITY_JSON, &no_vars).unwrap();
    assert_eq!(from_toml.harness, from_json.harness);
    assert_eq!(from_toml.ext.chain, from_json.ext.chain);
    assert_eq!(from_toml.env, from_json.env);
    assert_eq!(from_toml.profiles, from_json.profiles);
    assert_eq!(from_toml.suites, from_json.suites);
    assert_eq!(from_toml.warnings, from_json.warnings);
}

// --- Phase 3.1: pipeline `phases` and `WorldSource` ---

use cross_vm_config::{ConfigError, WorldSource};

/// Two single-setup `invariant` profiles named `a` and `b`, plus a `[suite.p]` header, ready for
/// a caller to append `[[suite.p.phases]]` blocks. `body` is the phase declarations.
fn suite_with_phases(body: &str) -> String {
    format!(
        r#"
[harness]
name = "vault"
[profile.a]
mode = "invariant"
ops = 10
[profile.b]
mode = "invariant"
ops = 10
[suite.p]
{body}
"#
    )
}

#[test]
fn suite_phases_parse_with_needs_and_world() {
    let toml = r#"
[harness]
name = "vault"
[profile.a]
mode = "invariant"
ops = 10
[profile.b]
mode = "invariant"
ops = 10
[suite.p]
[[suite.p.phases]]
profile = "a"
[[suite.p.phases]]
profile = "b"
needs = ["a"]
world = "inherit"
"#;
    let cfg = cross_vm_config::from_toml_str(toml, &no_vars).expect("loads");
    let suite = &cfg.suites["p"];
    assert_eq!(suite.phases.len(), 2);
    assert_eq!(suite.phases[1].needs, vec!["a".to_string()]);
    assert!(matches!(suite.phases[1].world, WorldSource::Inherit));
    assert!(matches!(suite.phases[0].world, WorldSource::Fresh));
}

#[test]
fn suite_phase_params_parse() {
    let toml = suite_with_phases(
        r#"[[suite.p.phases]]
profile = "a"
params = { pinned_token = "uosmo" }"#,
    );
    let cfg = cross_vm_config::from_toml_str(&toml, &no_vars).expect("loads");
    let suite = &cfg.suites["p"];
    let params = suite.phases[0].params.as_ref().expect("params present");
    assert_eq!(params["pinned_token"].as_str(), Some("uosmo"));
}

#[test]
fn suite_profiles_normalize_into_phases() {
    // Legacy sugar: `profiles = ["a", "b"]` becomes two fresh phases with no needs, and the
    // legacy `profiles` field is cleared so `phases` is the single source of truth.
    let toml = r#"
[harness]
name = "vault"
[profile.a]
mode = "invariant"
ops = 10
[profile.b]
mode = "invariant"
ops = 10
[suite.p]
profiles = ["a", "b"]
"#;
    let cfg = cross_vm_config::from_toml_str(toml, &no_vars).expect("loads");
    let suite = &cfg.suites["p"];
    assert!(suite.profiles.is_empty(), "legacy profiles must be cleared");
    assert_eq!(suite.phases.len(), 2);
    assert_eq!(suite.phases[0].profile, "a");
    assert_eq!(suite.phases[1].profile, "b");
    assert!(suite
        .phases
        .iter()
        .all(|p| matches!(p.world, WorldSource::Fresh)));
    assert!(suite.phases.iter().all(|p| p.needs.is_empty()));
}

#[test]
fn suite_with_both_profiles_and_phases_is_an_error() {
    let toml = suite_with_phases(
        r#"profiles = ["a"]
[[suite.p.phases]]
profile = "b""#,
    );
    let err = cross_vm_config::from_toml_str(&toml, &no_vars).unwrap_err();
    assert!(
        matches!(err, ConfigError::SuiteProfilesAndPhases { ref suite } if suite == "p"),
        "unexpected error: {err}"
    );
}

#[test]
fn phase_needs_must_reference_an_earlier_phase() {
    // `needs = ["b"]` on the FIRST phase is a forward reference and must error.
    let toml = suite_with_phases(
        r#"[[suite.p.phases]]
profile = "a"
needs = ["b"]
[[suite.p.phases]]
profile = "b""#,
    );
    let err = cross_vm_config::from_toml_str(&toml, &no_vars).unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::PhaseNeedsNotEarlier { ref suite, ref phase, ref needed }
                if suite == "p" && phase == "a" && needed == "b"
        ),
        "unexpected error: {err}"
    );
}

#[test]
fn phase_self_reference_is_an_error() {
    let toml = suite_with_phases(
        r#"[[suite.p.phases]]
profile = "a"
needs = ["a"]"#,
    );
    let err = cross_vm_config::from_toml_str(&toml, &no_vars).unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::PhaseNeedsNotEarlier { ref phase, ref needed, .. }
                if phase == "a" && needed == "a"
        ),
        "unexpected error: {err}"
    );
}

#[test]
fn inherit_requires_exactly_one_need() {
    // `world = "inherit"` with no `needs` must error.
    let toml = suite_with_phases(
        r#"[[suite.p.phases]]
profile = "a"
[[suite.p.phases]]
profile = "b"
world = "inherit""#,
    );
    let err = cross_vm_config::from_toml_str(&toml, &no_vars).unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::PhaseInheritArity { ref suite, ref phase, needs } if suite == "p" && phase == "b" && needs == 0
        ),
        "unexpected error: {err}"
    );
}

#[test]
fn inherit_rejects_multi_case_fuzz_donor_and_consumer() {
    // A multi-case fuzz donor cannot be inherited from (its final world is undefined).
    let donor_multi = r#"
[harness]
name = "vault"
[profile.donor]
mode = "fuzz"
cases = 8
ops = 1
[profile.consumer]
mode = "invariant"
ops = 10
[suite.p]
[[suite.p.phases]]
profile = "donor"
[[suite.p.phases]]
profile = "consumer"
needs = ["donor"]
world = "inherit"
"#;
    let err = cross_vm_config::from_toml_str(donor_multi, &no_vars).unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::PhaseWorldNotSingleSetup { ref suite, ref phase, .. } if suite == "p" && phase == "donor"
        ),
        "donor error: {err}"
    );

    // A multi-case fuzz consumer cannot inherit (it would fan out one world into many cases).
    let consumer_multi = r#"
[harness]
name = "vault"
[profile.donor]
mode = "invariant"
ops = 10
[profile.consumer]
mode = "fuzz"
cases = 8
ops = 1
[suite.p]
[[suite.p.phases]]
profile = "donor"
[[suite.p.phases]]
profile = "consumer"
needs = ["donor"]
world = "inherit"
"#;
    let err = cross_vm_config::from_toml_str(consumer_multi, &no_vars).unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::PhaseWorldNotSingleSetup { ref suite, ref phase, .. } if suite == "p" && phase == "consumer"
        ),
        "consumer error: {err}"
    );

    // A single-case fuzz on both sides is single-setup and loads.
    let both_single = r#"
[harness]
name = "vault"
[profile.donor]
mode = "fuzz"
cases = 1
ops = 1
[profile.consumer]
mode = "fuzz"
cases = 1
ops = 1
[suite.p]
[[suite.p.phases]]
profile = "donor"
[[suite.p.phases]]
profile = "consumer"
needs = ["donor"]
world = "inherit"
"#;
    cross_vm_config::from_toml_str(both_single, &no_vars).expect("single-case fuzz inherit loads");
}

#[test]
fn duplicate_phase_profile_in_one_suite_is_an_error() {
    let toml = suite_with_phases(
        r#"[[suite.p.phases]]
profile = "a"
[[suite.p.phases]]
profile = "a""#,
    );
    let err = cross_vm_config::from_toml_str(&toml, &no_vars).unwrap_err();
    assert!(
        matches!(err, ConfigError::DuplicatePhaseProfile { ref suite, ref profile } if suite == "p" && profile == "a"),
        "unexpected error: {err}"
    );
}

#[test]
fn two_inheritors_of_one_donor_is_an_error() {
    let toml = r#"
[harness]
name = "vault"
[profile.a]
mode = "invariant"
ops = 10
[profile.b]
mode = "invariant"
ops = 10
[profile.c]
mode = "invariant"
ops = 10
[suite.p]
[[suite.p.phases]]
profile = "a"
[[suite.p.phases]]
profile = "b"
needs = ["a"]
world = "inherit"
[[suite.p.phases]]
profile = "c"
needs = ["a"]
world = "inherit"
"#;
    let err = cross_vm_config::from_toml_str(toml, &no_vars).unwrap_err();
    let message = err.to_string();
    assert!(
        matches!(
            err,
            ConfigError::SharedDonor { ref suite, ref donor, ref first, ref second }
                if suite == "p" && donor == "a" && first == "b" && second == "c"
        ),
        "unexpected error: {err}"
    );
    // The message must name the donor and both inheriting phases.
    assert!(message.contains('a') && message.contains('b') && message.contains('c'));
}

#[test]
fn linear_inherit_chain_stays_valid() {
    // a -> b -> c, each phase inheriting from exactly one distinct donor, is legal.
    let toml = r#"
[harness]
name = "vault"
[profile.a]
mode = "invariant"
ops = 10
[profile.b]
mode = "invariant"
ops = 10
[profile.c]
mode = "invariant"
ops = 10
[suite.p]
[[suite.p.phases]]
profile = "a"
[[suite.p.phases]]
profile = "b"
needs = ["a"]
world = "inherit"
[[suite.p.phases]]
profile = "c"
needs = ["b"]
world = "inherit"
"#;
    cross_vm_config::from_toml_str(toml, &no_vars).expect("linear inherit chain loads");
}
