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
    assert_eq!(cfg.chains.len(), 3);
    assert_eq!(cfg.chains[0].kind, "cosmwasm");
    assert_eq!(cfg.chains[0].label, "osmosis");

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
    let from_toml = cross_vm_config::from_toml_str(PARITY_TOML, &no_vars).unwrap();
    let from_json = cross_vm_config::from_json_str(PARITY_JSON, &no_vars).unwrap();
    assert_eq!(from_toml, from_json);
}
