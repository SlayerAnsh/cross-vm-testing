//! Outcomes of applying an operation and of a whole run.
//!
//! Three things must stay distinct:
//! - A legitimate **acceptance** vs a legitimate **rejection** ([`Verdict`]). A withdraw of
//!   more than the balance *should* revert; that is not a bug.
//! - A discovered **bug** vs a test-**infrastructure** failure ([`HarnessError`]). An over-
//!   withdraw that the contract *accepted* is a bug; a failed deploy is infrastructure.
//! - An **invariant** violation ([`Violation`]), surfaced by [`crate::Harness::check`].
//!
//! The classification policy lives inside the developer's `apply`, the only place that knows an
//! operation's semantics. The framework's `classify` helper collapses the common four-way match
//! into one call.

use std::collections::BTreeMap;

/// How the system-under-test responded to an operation the developer judged legitimate to attempt.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// The operation succeeded and the model expected it to.
    Accepted,
    /// The operation was rejected, and that rejection is expected (e.g. withdraw > balance).
    Rejected {
        /// The revert reason, for diagnostics.
        reason: String,
    },
}

/// An error that ends a run as a failure (as opposed to a legitimate [`Verdict::Rejected`]).
#[derive(Debug)]
pub enum HarnessError {
    /// A confirmed bug in the system-under-test.
    Bug(String),
    /// A test-infrastructure failure (deploy/RPC/model desync), reported separately.
    Infra(Box<dyn std::error::Error>),
}

impl HarnessError {
    /// Build an [`Infra`](HarnessError::Infra) from any error or message.
    pub fn infra(e: impl Into<Box<dyn std::error::Error>>) -> Self {
        HarnessError::Infra(e.into())
    }

    /// Build a [`Bug`](HarnessError::Bug) from any displayable detail.
    pub fn bug(detail: impl Into<String>) -> Self {
        HarnessError::Bug(detail.into())
    }
}

impl core::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HarnessError::Bug(m) => write!(f, "bug: {m}"),
            HarnessError::Infra(e) => write!(f, "infra: {e}"),
        }
    }
}

/// Any concrete error lifts to [`Infra`](HarnessError::Infra) via `?` (the anyhow pattern). This is
/// why `HarnessError` itself must not implement [`std::error::Error`]: the blanket impl would
/// collide with the reflexive `From<T> for T`.
impl<E: std::error::Error + 'static> From<E> for HarnessError {
    fn from(e: E) -> Self {
        HarnessError::Infra(Box::new(e))
    }
}

/// A broken invariant, returned by [`crate::Harness::check`].
#[derive(Debug, Clone)]
pub struct Violation {
    /// Human-readable detail of how the invariant was broken.
    pub detail: String,
}

impl Violation {
    /// Build a violation from any displayable detail.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// The result of checking one invariant against the current state.
///
/// Distinct from a plain `Result` so an invariant can declare itself **not yet applicable**
/// (its precondition has not occurred yet, e.g. "no counter has been incremented") instead of
/// being forced to vacuously pass. A [`Skipped`](CheckOutcome::Skipped) check carries a reason
/// for the report and never fails the run.
#[derive(Debug, Clone)]
pub enum CheckOutcome {
    /// The invariant was applicable and held.
    Held,
    /// The invariant is not applicable yet; the string is a human-readable reason.
    Skipped(String),
    /// The invariant was applicable and broke.
    Violated(Violation),
}

impl CheckOutcome {
    /// Convenience: a violation from any displayable detail.
    pub fn violated(detail: impl Into<String>) -> Self {
        CheckOutcome::Violated(Violation::new(detail))
    }

    /// Convenience: a skip from any displayable reason.
    pub fn skipped(reason: impl Into<String>) -> Self {
        CheckOutcome::Skipped(reason.into())
    }
}

impl From<Result<(), Violation>> for CheckOutcome {
    /// Lift the old `Result<(), Violation>` shape: `Ok` -> `Held`, `Err` -> `Violated`.
    fn from(r: Result<(), Violation>) -> Self {
        match r {
            Ok(()) => CheckOutcome::Held,
            Err(v) => CheckOutcome::Violated(v),
        }
    }
}

/// Why a run failed.
///
/// Externally tagged when serialized (serde's default enum representation): `Bug(String)` ->
/// `{"Bug": "..."}`, `Invariant { name, detail }` -> `{"Invariant": {"name": ..., "detail":
/// ...}}`, `Infra(String)` -> `{"Infra": "..."}`. That tag is also the value stored in
/// `crate::config::ErasedFailure::kind`, so a JSON report reader can `match` on the same key a
/// Rust caller would.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum FailureKind {
    /// `apply` reported a confirmed SUT bug.
    Bug(String),
    /// An invariant was violated after an operation.
    Invariant {
        /// The `Debug` rendering of the violated invariant.
        name: String,
        /// Detail from the [`Violation`].
        detail: String,
    },
    /// A test-infrastructure failure (e.g. `state()` or `advance()` failed).
    Infra(String),
}

/// A single failing step, with everything needed to replay it deterministically.
#[derive(Debug, Clone)]
pub struct Failure<Op> {
    /// 1-based index of the operation that failed, or `0` for a pre-operation failure such as
    /// `state()` itself failing.
    pub step: usize,
    /// The operation that triggered the failure. `None` for a pre-operation failure.
    pub op: Option<Op>,
    /// The full operation history up to and including the failing op, for `Runner::replay`.
    pub history: Vec<Op>,
    /// What went wrong.
    pub kind: FailureKind,
}

/// Per-invariant tally over a run: how many times each invariant held, skipped, or was violated.
///
/// A `held + skipped + violated` total of `0` means the invariant never ran — critical on multi-VM
/// runs, where an invariant can silently never fire on one chain's path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct InvCoverage {
    /// Times the invariant was applicable and held.
    pub held: usize,
    /// Times the invariant was not applicable ([`CheckOutcome::Skipped`]).
    pub skipped: usize,
    /// Times the invariant was violated.
    pub violated: usize,
}

impl InvCoverage {
    /// Total checks recorded for this invariant. `0` means it never ran.
    pub fn total(&self) -> usize {
        self.held + self.skipped + self.violated
    }
}

/// Per-invariant coverage over a whole run, keyed by the invariant's `Debug` name (the same key
/// used for [`FailureKind::Invariant::name`]).
///
/// Seeded with every invariant [`Harness::invariants`](crate::Harness::invariants) reports
/// at run start, so an invariant that is never checked (e.g. `check_every` skipped it, or the run
/// was too short) still appears with an all-zero tally instead of vanishing.
///
/// Serializes transparently as the inner map (`#[serde(transparent)]`): a JSON object keyed by
/// invariant name, e.g. `{"balances_never_negative": {"held": 12, "skipped": 0, "violated": 0}}`,
/// rather than being wrapped in a newtype layer.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Coverage(BTreeMap<String, InvCoverage>);

impl Coverage {
    /// Pre-insert every invariant name at an all-zero tally so never-checked invariants stay visible.
    pub fn seed(names: impl IntoIterator<Item = String>) -> Self {
        Self(
            names
                .into_iter()
                .map(|n| (n, InvCoverage::default()))
                .collect(),
        )
    }

    /// Record that `name` held on one check.
    pub fn record_held(&mut self, name: &str) {
        self.entry(name).held += 1;
    }

    /// Record that `name` was skipped on one check.
    pub fn record_skipped(&mut self, name: &str) {
        self.entry(name).skipped += 1;
    }

    /// Record that `name` was violated on one check.
    pub fn record_violated(&mut self, name: &str) {
        self.entry(name).violated += 1;
    }

    fn entry(&mut self, name: &str) -> &mut InvCoverage {
        // A seeded name is the common case; only an invariant set that grew mid-run allocates.
        self.0.entry(name.to_string()).or_default()
    }

    /// Total skipped checks across all invariants (the aggregate the report also exposes).
    pub fn total_skipped(&self) -> usize {
        self.0.values().map(|c| c.skipped).sum()
    }

    /// Names of invariants that never ran (a zero total): candidates for a coverage gap.
    pub fn uncovered(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .filter(|(_, c)| c.total() == 0)
            .map(|(n, _)| n.as_str())
    }

    /// Iterate every invariant's tally in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &InvCoverage)> {
        self.0.iter().map(|(n, c)| (n.as_str(), c))
    }
}

/// The result of a run: the seed and mode that produced it, the number of steps taken, per-invariant
/// [`coverage`](RunReport::coverage), and the first failure if any. `failure.is_none()` means the
/// run passed.
#[derive(Debug, Clone)]
pub struct RunReport<Op> {
    /// The base seed the run was driven with.
    pub seed: u64,
    /// The mode label (`"fuzz"`, `"invariant"`, `"endurance"`, `"case"`, `"replay"`).
    pub mode: &'static str,
    /// Total operations applied.
    pub steps: usize,
    /// How many invariant checks were skipped (precondition not yet met) over the run. Equals
    /// [`Coverage::total_skipped`] on [`coverage`](RunReport::coverage).
    pub skipped: usize,
    /// Per-invariant tallies (held / skipped / violated), keyed by the invariant's `Debug` name.
    /// An invariant with a zero total never ran on this path.
    pub coverage: Coverage,
    /// The first failure encountered, if any.
    pub failure: Option<Failure<Op>>,
}

impl<Op> RunReport<Op> {
    /// `true` if the run encountered no failure.
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------------------
    // serde (spec section 9): Coverage/InvCoverage/FailureKind shapes. Gated on `serde`, the
    // feature that pulls in the `Serialize` derives; `serde_json` (used here to assert the JSON
    // shape) is a dev-dependency of this crate.
    // -------------------------------------------------------------------------------------

    #[cfg(feature = "serde")]
    mod serde_shapes {
        use super::*;

        #[test]
        fn coverage_serializes_transparently_keyed_by_invariant_name() {
            let mut cov = Coverage::seed(["balances_never_negative".to_string()]);
            cov.record_held("balances_never_negative");
            cov.record_held("balances_never_negative");
            cov.record_skipped("balances_never_negative");

            let value = serde_json::to_value(&cov).unwrap();
            // Transparent: a bare object keyed by invariant name, no wrapper/newtype layer.
            assert_eq!(
                value,
                serde_json::json!({
                    "balances_never_negative": {"held": 2, "skipped": 1, "violated": 0}
                })
            );
        }

        #[test]
        fn empty_coverage_serializes_as_an_empty_object() {
            let cov = Coverage::default();
            assert_eq!(serde_json::to_value(&cov).unwrap(), serde_json::json!({}));
        }

        #[test]
        fn failure_kind_bug_and_infra_serialize_externally_tagged_with_a_string_payload() {
            assert_eq!(
                serde_json::to_value(FailureKind::Bug("over-withdraw accepted".to_string()))
                    .unwrap(),
                serde_json::json!({"Bug": "over-withdraw accepted"})
            );
            assert_eq!(
                serde_json::to_value(FailureKind::Infra("rpc down".to_string())).unwrap(),
                serde_json::json!({"Infra": "rpc down"})
            );
        }

        #[test]
        fn failure_kind_invariant_serializes_with_name_and_detail() {
            let kind = FailureKind::Invariant {
                name: "balances_never_negative".to_string(),
                detail: "alice went negative".to_string(),
            };
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!({
                    "Invariant": {
                        "name": "balances_never_negative",
                        "detail": "alice went negative"
                    }
                })
            );
        }
    }
}
