//! Typed profile schema: the `Profile` enum, per-mode payload structs, and the common keys
//! shared by every mode.
//!
//! `Profile` is dispatched manually rather than through serde's internally tagged enum
//! support: popping the `mode` key and deserializing into a per-mode struct sidesteps the
//! serde limitation where an internally tagged enum conflicts with `deny_unknown_fields`, and
//! gives precise, per-mode error paths. The common keys are repeated directly on each private
//! "wire" struct (`flatten` cannot combine with `deny_unknown_fields` on the same struct) and
//! copied into a `CommonKeys` value on the public per-mode struct, reachable through
//! `Profile::common()`.

use crate::duration::{humantime_duration, humantime_opt};
use crate::seed::SeedSpec;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// `[harness]`: the registry key plus the named setup to use.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRef {
    /// Registry key the framework looks up at run time.
    pub name: String,
    /// Named setup to use; defaults to `"default"`.
    #[serde(default = "default_setup")]
    pub setup: String,
}

fn default_setup() -> String {
    "default".to_string()
}

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

/// Where a suite phase's starting `(Ctx, World)` comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorldSource {
    /// Build a fresh environment via the registered setup fn (today's behavior).
    #[default]
    Fresh,
    /// Start from the live environment and world the donor phase (the single `needs` entry)
    /// finished with. Requires the donor to have passed.
    Inherit,
}

/// One phase of a pipeline suite: a profile to run, the phases that must have passed first, and
/// where its starting world comes from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuitePhase {
    /// A `[profile.*]` name in the same config file.
    pub profile: String,
    /// Names of earlier phases in this suite that must have passed. A failed or skipped
    /// dependency skips this phase.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Starting-state source. `inherit` requires exactly one `needs` entry.
    #[serde(default)]
    pub world: WorldSource,
    /// Optional table handed to the harness's registered world patch fn before this phase
    /// runs (after the starting world is obtained, fresh or inherited). Requires the harness
    /// to be registered with a patch fn; enforced at run time.
    #[serde(default)]
    pub params: Option<toml::Table>,
}

/// `[suite.<name>]`: an ordered pipeline of phases to run together.
///
/// A config may declare phases directly via `[[suite.<name>.phases]]`, or use the legacy
/// `profiles = [..]` sugar. After loading, [`Suite::phases`] is always the source of truth:
/// legacy `profiles` are normalized into fresh, dependency-free phases and `profiles` is cleared.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// Legacy sugar: profile names to run, in order. Normalized into [`Suite::phases`] by the
    /// loader and cleared; read `phases` instead.
    #[serde(default)]
    pub profiles: Vec<String>,
    /// The pipeline phases, in declaration (execution) order. Always populated after loading.
    #[serde(default)]
    pub phases: Vec<SuitePhase>,
    /// Stop the suite at the first failing profile; defaults to `false`.
    #[serde(default)]
    pub stop_on_failure: bool,
}

/// `"accepted"` | `"rejected"` | `"any"`: the verdict a scenario step asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpectStr {
    /// The operation must be accepted.
    #[default]
    Accepted,
    /// The operation must be rejected.
    Rejected,
    /// Either verdict is acceptable.
    Any,
}

/// One `[[profile.<name>.steps]]` entry: a concrete, ordered op with its expected verdict.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioStepRaw {
    /// The externally tagged op, deserialized into `H::Operation` in the framework.
    ///
    /// Held as a format-agnostic [`serde_json::Value`] rather than a [`toml::Value`] so a
    /// JSON-sourced op amount in `(i64::MAX, u64::MAX]` keeps full integer precision (TOML
    /// integers are signed 64-bit). TOML-sourced ops deserialize into this value type losslessly
    /// (TOML integers are `i64`, a subset).
    pub op: serde_json::Value,
    /// Expected verdict; defaults to `Accepted`.
    #[serde(default)]
    pub expect: ExpectStr,
    /// Sleep before this step, live chain pacing; defaults to zero.
    #[serde(default = "zero_duration", with = "humantime_duration")]
    pub delay: Duration,
    /// Run the invariant sweep after this step; defaults to `true`.
    #[serde(default = "default_true")]
    pub check: bool,
}

fn zero_duration() -> Duration {
    Duration::ZERO
}

fn default_true() -> bool {
    true
}

fn default_check_every() -> usize {
    1
}

fn default_artifacts_dir() -> String {
    "target/cross-vm".to_string()
}

fn default_shrink_limit() -> usize {
    256
}

fn default_heartbeat() -> Duration {
    Duration::from_secs(60)
}

/// The profile keys shared by every mode (spec section 4.3).
#[derive(Debug, Clone, PartialEq)]
pub struct CommonKeys {
    /// Run seed; 0 by default.
    pub seed: SeedSpec,
    /// Invariant sweep cadence; 0 means never mid run. Defaults to 1.
    pub check_every: usize,
    /// Enables `Runner::with_stats()`. Defaults to `false`.
    pub stats: bool,
    /// Directory replay artifacts and reports land in. Defaults to `"target/cross-vm"`.
    pub artifacts_dir: String,
    /// Optional path to write the run report as JSON.
    pub json_report: Option<String>,
    /// Per-profile override of the top level `[env]`, shallow merged over it by the loader's
    /// merge stage before typed deserialize (`targets` merges label-wise; `target`, `chains`,
    /// `params` are whole-value overrides). `None` only when neither the profile nor the
    /// top-level `[env]` set anything; otherwise this already holds the fully merged,
    /// effective environment for this profile, not just the override delta.
    pub env: Option<EnvSpec>,
    /// Auto-shrink a failing history before writing the artifact; mode-dependent default
    /// resolved in the framework, so this stays `Option` here.
    pub shrink: Option<bool>,
    /// Shrink replay budget. Defaults to 256.
    pub shrink_limit: usize,
}

/// Extracts the shared [`CommonKeys`] out of a wire struct's repeated fields.
macro_rules! common_from_wire {
    ($wire:expr) => {
        CommonKeys {
            seed: $wire.seed,
            check_every: $wire.check_every,
            stats: $wire.stats,
            artifacts_dir: $wire.artifacts_dir,
            json_report: $wire.json_report,
            env: $wire.env,
            shrink: $wire.shrink,
            shrink_limit: $wire.shrink_limit,
        }
    };
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FuzzProfileWire {
    #[serde(default)]
    seed: SeedSpec,
    #[serde(default = "default_check_every")]
    check_every: usize,
    #[serde(default)]
    stats: bool,
    #[serde(default = "default_artifacts_dir")]
    artifacts_dir: String,
    #[serde(default)]
    json_report: Option<String>,
    #[serde(default)]
    env: Option<EnvSpec>,
    #[serde(default)]
    shrink: Option<bool>,
    #[serde(default = "default_shrink_limit")]
    shrink_limit: usize,
    cases: usize,
    ops: usize,
    #[serde(default)]
    kinds: Option<Vec<String>>,
    #[serde(default)]
    weights: Option<BTreeMap<String, u32>>,
}

/// `mode = "fuzz"`: fan out `cases` independent runs, `ops` per case.
#[derive(Debug, Clone, PartialEq)]
pub struct FuzzProfile {
    /// Keys shared with every other mode.
    pub common: CommonKeys,
    /// Fan out count; case `i` is seeded `sub_seed(seed, i)`.
    pub cases: usize,
    /// Sequence length per case.
    pub ops: usize,
    /// Restricted uniform draw over these kind names; `None` means all kinds.
    pub kinds: Option<Vec<String>>,
    /// Static weighted draw over kind name to integer weight, multiplied at each draw by the
    /// harness's dynamic `weight(ctx, world, kind)`; mutually exclusive with `kinds`.
    pub weights: Option<BTreeMap<String, u32>>,
}

impl From<FuzzProfileWire> for FuzzProfile {
    fn from(w: FuzzProfileWire) -> Self {
        FuzzProfile {
            cases: w.cases,
            ops: w.ops,
            kinds: w.kinds.clone(),
            weights: w.weights.clone(),
            common: common_from_wire!(w),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvariantProfileWire {
    #[serde(default)]
    seed: SeedSpec,
    #[serde(default = "default_check_every")]
    check_every: usize,
    #[serde(default)]
    stats: bool,
    #[serde(default = "default_artifacts_dir")]
    artifacts_dir: String,
    #[serde(default)]
    json_report: Option<String>,
    #[serde(default)]
    env: Option<EnvSpec>,
    #[serde(default)]
    shrink: Option<bool>,
    #[serde(default = "default_shrink_limit")]
    shrink_limit: usize,
    ops: usize,
    #[serde(default)]
    kinds: Option<Vec<String>>,
    #[serde(default)]
    weights: Option<BTreeMap<String, u32>>,
}

/// `mode = "invariant"`: one long run, no `cases`.
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantProfile {
    /// Keys shared with every other mode.
    pub common: CommonKeys,
    /// Sequence length.
    pub ops: usize,
    /// Restricted uniform draw over these kind names; `None` means all kinds.
    pub kinds: Option<Vec<String>>,
    /// Static weighted draw over kind name to integer weight, multiplied at each draw by the
    /// harness's dynamic `weight(ctx, world, kind)`; mutually exclusive with `kinds`.
    pub weights: Option<BTreeMap<String, u32>>,
}

impl From<InvariantProfileWire> for InvariantProfile {
    fn from(w: InvariantProfileWire) -> Self {
        InvariantProfile {
            ops: w.ops,
            kinds: w.kinds.clone(),
            weights: w.weights.clone(),
            common: common_from_wire!(w),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnduranceProfileWire {
    #[serde(default)]
    seed: SeedSpec,
    #[serde(default = "default_check_every")]
    check_every: usize,
    #[serde(default)]
    stats: bool,
    #[serde(default = "default_artifacts_dir")]
    artifacts_dir: String,
    #[serde(default)]
    json_report: Option<String>,
    #[serde(default)]
    env: Option<EnvSpec>,
    #[serde(default)]
    shrink: Option<bool>,
    #[serde(default = "default_shrink_limit")]
    shrink_limit: usize,
    #[serde(default, with = "humantime_opt")]
    duration: Option<Duration>,
    #[serde(default)]
    max_ops: Option<usize>,
    #[serde(default = "zero_duration", with = "humantime_duration")]
    base_delay: Duration,
    #[serde(default = "zero_duration", with = "humantime_duration")]
    max_delay: Duration,
    #[serde(default)]
    advance_blocks: Option<usize>,
    #[serde(default)]
    block_jitter: usize,
    #[serde(default)]
    max_consecutive_infra: usize,
    #[serde(default = "default_heartbeat", with = "humantime_duration")]
    heartbeat: Duration,
    #[serde(default)]
    kinds: Option<Vec<String>>,
    #[serde(default)]
    weights: Option<BTreeMap<String, u32>>,
}

/// `mode = "endurance"`: a long run bounded by wall clock time and/or op count.
#[derive(Debug, Clone, PartialEq)]
pub struct EnduranceProfile {
    /// Keys shared with every other mode.
    pub common: CommonKeys,
    /// Wall clock bound; required unless `max_ops` is set (enforced by the loader's
    /// structural-validation stage, `validate::validate`).
    pub duration: Option<Duration>,
    /// Op count bound; whichever bound hits first stops the run.
    pub max_ops: Option<usize>,
    /// Floor between ops. Defaults to zero.
    pub base_delay: Duration,
    /// Jitter ceiling on top of `base_delay`. Defaults to zero.
    pub max_delay: Duration,
    /// Blocks advanced per op.
    pub advance_blocks: Option<usize>,
    /// Extra random blocks per advance. Defaults to zero.
    pub block_jitter: usize,
    /// Tolerated consecutive `Infra` failures before the run fails. Defaults to zero (fail on
    /// the first `Infra`).
    pub max_consecutive_infra: usize,
    /// Periodic info log cadence; zero disables it. Defaults to 60 seconds.
    pub heartbeat: Duration,
    /// Restricted uniform draw over these kind names; `None` means all kinds.
    pub kinds: Option<Vec<String>>,
    /// Static weighted draw over kind name to integer weight, multiplied at each draw by the
    /// harness's dynamic `weight(ctx, world, kind)`; mutually exclusive with `kinds`.
    pub weights: Option<BTreeMap<String, u32>>,
}

impl From<EnduranceProfileWire> for EnduranceProfile {
    fn from(w: EnduranceProfileWire) -> Self {
        EnduranceProfile {
            duration: w.duration,
            max_ops: w.max_ops,
            base_delay: w.base_delay,
            max_delay: w.max_delay,
            advance_blocks: w.advance_blocks,
            block_jitter: w.block_jitter,
            max_consecutive_infra: w.max_consecutive_infra,
            heartbeat: w.heartbeat,
            kinds: w.kinds.clone(),
            weights: w.weights.clone(),
            common: common_from_wire!(w),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioProfileWire {
    #[serde(default)]
    seed: SeedSpec,
    #[serde(default = "default_check_every")]
    check_every: usize,
    #[serde(default)]
    stats: bool,
    #[serde(default = "default_artifacts_dir")]
    artifacts_dir: String,
    #[serde(default)]
    json_report: Option<String>,
    #[serde(default)]
    env: Option<EnvSpec>,
    #[serde(default)]
    shrink: Option<bool>,
    #[serde(default = "default_shrink_limit")]
    shrink_limit: usize,
    steps: Vec<ScenarioStepRaw>,
    #[serde(default)]
    export_world: Option<String>,
}

/// `mode = "scenario"`: an ordered, concrete op sequence with per-step expectations.
#[derive(Debug, Clone, PartialEq)]
pub struct ScenarioProfile {
    /// Keys shared with every other mode.
    pub common: CommonKeys,
    /// Ordered concrete steps; required, non-empty (enforced by the loader's
    /// structural-validation stage, `validate::validate`).
    pub steps: Vec<ScenarioStepRaw>,
    /// Optional path to serialize the final `World` as JSON (a later phase).
    pub export_world: Option<String>,
}

impl From<ScenarioProfileWire> for ScenarioProfile {
    fn from(w: ScenarioProfileWire) -> Self {
        ScenarioProfile {
            steps: w.steps.clone(),
            export_world: w.export_world.clone(),
            common: common_from_wire!(w),
        }
    }
}

/// One `[profile.<name>]` block: a runnable configuration for one of the four modes.
///
/// Deserialized manually (see the module docs) rather than through serde's internally tagged
/// enum support: a crate-internal dispatcher pops the `mode` key and deserializes into the
/// matching per-mode struct.
#[derive(Debug, Clone, PartialEq)]
pub enum Profile {
    /// `mode = "fuzz"`.
    Fuzz(FuzzProfile),
    /// `mode = "invariant"`.
    Invariant(InvariantProfile),
    /// `mode = "endurance"`.
    Endurance(EnduranceProfile),
    /// `mode = "scenario"`.
    Scenario(ScenarioProfile),
}

impl Profile {
    /// Returns the keys shared by every mode, regardless of which variant this profile is.
    pub fn common(&self) -> &CommonKeys {
        match self {
            Profile::Fuzz(p) => &p.common,
            Profile::Invariant(p) => &p.common,
            Profile::Endurance(p) => &p.common,
            Profile::Scenario(p) => &p.common,
        }
    }

    /// Dispatches a profile's table value (with its `mode` key already popped) into the matching
    /// per-mode struct, rejecting unknown fields with a precise error path.
    ///
    /// Generic over the document value type ([`toml::Value`] or [`serde_json::Value`]): the
    /// value is deserialized natively into the per-mode wire struct, so a JSON scenario `op`
    /// keeps its precise integer representation instead of being downgraded through
    /// `toml::Value`.
    pub(crate) fn from_mode_table<V: crate::value::Doc>(
        mode: &str,
        value: V,
    ) -> Result<Profile, String> {
        match mode {
            "fuzz" => value
                .deserialize_into::<FuzzProfileWire>()
                .map(|w| Profile::Fuzz(w.into())),
            "invariant" => value
                .deserialize_into::<InvariantProfileWire>()
                .map(|w| Profile::Invariant(w.into())),
            "endurance" => value
                .deserialize_into::<EnduranceProfileWire>()
                .map(|w| Profile::Endurance(w.into())),
            "scenario" => value
                .deserialize_into::<ScenarioProfileWire>()
                .map(|w| Profile::Scenario(w.into())),
            other => Err(format!(
                "unknown mode `{other}`, expected one of \"fuzz\", \"invariant\", \"endurance\", \"scenario\""
            )),
        }
    }
}
